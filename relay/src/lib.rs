//! Store-and-forward relay. Holds opaque blobs per recipient device until the
//! recipient acknowledges them or they expire. It authenticates devices by
//! Ed25519 challenge-response, enforces size/queue/TTL limits, and logs no
//! message content. Queues and prekeys are stored in SQLite (see [`store`]);
//! with no database path they live in memory and are lost on restart.
//!
//! Abuse limits (per-address connection caps, per-connection request rates) live in
//! [`limits`], and content-free counters in [`metrics`]. Every request is processed on a
//! blocking thread so a slow disk cannot stall the connections of everyone else (ADR 0022).

pub mod limits;
pub mod metrics;
pub mod push;
pub mod store;
pub mod tls;

use futures_util::{SinkExt, StreamExt};
use limits::{client_ip, is_trusted, Admission, Cidr, IpLimits, Refused, TokenBucket};
use metrics::Metrics;
use rand::{rngs::OsRng, RngCore};
use spike_core::attachment::{CHUNK, MAX_FILE, OVERHEAD};
use spike_core::identity::{
    device_id_from_ed, is_valid_device_id, verify_auth, verify_prekey, SignedPrekey, MAX_PREKEYS,
};
use spike_core::mailbox::{self, is_valid_mailbox_id};
use spike_core::owner::DeviceList;
use spike_core::wire::{ClientMsg, GroupKind, ServerMsg, WireEntry, WirePrekey, WireSeal};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use store::{Enqueued, GroupAppended, GroupCreated, GroupLimits, PutChunk, Store};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};

#[derive(Clone, Debug)]
pub struct Config {
    /// Largest accepted blob, in bytes.
    pub max_blob: usize,
    /// Most undelivered blobs held per recipient.
    pub queue_cap: usize,
    /// How long an unacknowledged blob is kept.
    pub ttl: Duration,
    /// Time allowed to complete authentication after connecting.
    pub auth_timeout: Duration,
    /// A connection that has sent nothing for this long is closed. Waiting clients send a
    /// `Ping` every 25 seconds, so only dead or abandoned connections reach it.
    pub idle_timeout: Duration,
    /// Prekeys one connection may claim from others before being refused.
    pub max_claims_per_conn: usize,
    /// How long attachment chunks are kept.
    pub blob_ttl: Duration,
    /// Attachment bytes one device may have stored at once (all its unexpired chunks).
    pub blob_quota: usize,
    /// Connections one client address may hold open at once (0 = unlimited).
    pub max_conns_per_ip: usize,
    /// New connections one client address may open per minute (0 = unlimited).
    pub conn_rate_per_ip: u32,
    /// Requests per second one connection may sustain (0 = unlimited). Pings are free and
    /// acknowledgements cost a quarter of a request.
    pub req_rate: f64,
    /// Requests one connection may make in a burst before the sustained rate applies.
    pub req_burst: f64,
    /// Reverse proxies whose `X-Forwarded-For` header is believed. Empty means no proxy: the
    /// header is ignored and the connection's own address is used.
    pub trusted_proxies: Vec<Cidr>,
    /// If set, `/metrics` needs `Authorization: Bearer <token>`.
    pub metrics_token: Option<String>,
    /// The name clients sign into their sign-in answer (`host[:port]`, as they dial it). Unset means
    /// the `Host` header of the upgrade request is used, which is right unless a proxy rewrites it.
    pub auth_audience: Option<String>,
    /// Largest entry in a group mailbox, in bytes (ADR 0025).
    pub group_max_entry: usize,
    /// How long a group message is kept. Short: a member who is away longer reads it from a device
    /// that kept it, not from the relay.
    pub group_message_ttl: Duration,
    /// How long a commit is kept, and a mailbox after its last append. Long: a device that misses
    /// one commit cannot read anything afterwards, so commits outlive any ordinary absence.
    pub group_handshake_ttl: Duration,
    /// Bytes one mailbox may hold. Past it the oldest messages (never commits) are dropped.
    pub group_max_bytes: usize,
    /// Group mailboxes one device may have created (and not yet seen expire).
    pub group_max_mailboxes_per_device: usize,
    /// Mailboxes one connection may be subscribed to.
    pub max_group_subs_per_conn: usize,
    /// TURN server URLs the relay's operator runs (`turn:host:3478?transport=udp`, `turns:host:5349`, ...). With
    /// `turn_secret`, signed-in devices may ask for short-lived credentials (ADR 0028).
    pub turn_urls: Vec<String>,
    /// The secret shared with the TURN server (coturn `static-auth-secret`).
    pub turn_secret: Option<String>,
    /// How long an issued TURN credential is valid.
    pub turn_ttl: Duration,
    /// Send wake-up pushes to endpoints devices register (UnifiedPush, ADR 0028).
    pub push_enabled: bool,
    /// Also allow `http://` and private addresses for push endpoints (tests and local setups only).
    pub push_allow_insecure: bool,
    /// At most one wake-up per device this often, however many messages arrive.
    pub push_min_interval: Duration,
    /// SQLite file for queues and prekeys; `None` keeps them in memory only.
    pub db_path: Option<PathBuf>,
    /// Terminate TLS in the relay. Without it the relay speaks plain WebSocket
    /// and must sit behind a TLS-terminating proxy or on loopback.
    pub tls: Option<tls::TlsFiles>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_blob: 64 * 1024,
            queue_cap: 1000,
            ttl: Duration::from_secs(7 * 24 * 3600),
            auth_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(120),
            max_claims_per_conn: 20,
            blob_ttl: Duration::from_secs(30 * 24 * 3600),
            blob_quota: 1024 * 1024 * 1024,
            max_conns_per_ip: 64,
            conn_rate_per_ip: 300,
            req_rate: 50.0,
            req_burst: 200.0,
            trusted_proxies: Vec::new(),
            metrics_token: None,
            auth_audience: None,
            group_max_entry: 256 * 1024,
            group_message_ttl: Duration::from_secs(30 * 24 * 3600),
            group_handshake_ttl: Duration::from_secs(180 * 24 * 3600),
            group_max_bytes: 64 * 1024 * 1024,
            group_max_mailboxes_per_device: 200,
            max_group_subs_per_conn: 256,
            turn_urls: Vec::new(),
            turn_secret: None,
            turn_ttl: Duration::from_secs(3600),
            push_enabled: true,
            push_allow_insecure: false,
            push_min_interval: Duration::from_secs(5),
            db_path: None,
            tls: None,
        }
    }
}

/// Live connections one device may hold at once. A client may keep one open to be told the moment
/// something arrives while another does its work (receiving sends receipts over a second
/// connection). Past this, the oldest is closed so a buggy or hostile client cannot pile them up.
const MAX_LIVE_PER_DEVICE: usize = 4;

/// After this many consecutive refused requests the connection is closed: it is not going to slow down.
const MAX_RATE_LIMIT_STREAK: u32 = 200;

#[derive(Default)]
struct State {
    /// Device id -> its live connections, oldest first. A new message is pushed to all of them.
    online: HashMap<String, Vec<(u64, mpsc::Sender<ServerMsg>)>>,
    /// Mailbox id -> the connections subscribed to it: one stored copy, pushed to each. The
    /// senders are *weak*: only `online` keeps a connection's channel open, so evicting a
    /// connection (dropping its sender there) still ends it, subscribed or not.
    groups: HashMap<String, Vec<(u64, mpsc::WeakSender<ServerMsg>)>>,
    next_conn: u64,
}

/// `State` is always locked before the store; keep that order.
struct Shared {
    state: Mutex<State>,
    store: Store,
    metrics: Metrics,
    limits: Arc<IpLimits>,
    started: Instant,
    /// Devices to wake (their ids); `None` when pushes are off. A worker task reads it (see `push_worker`).
    push_tx: Option<mpsc::Sender<String>>,
}

pub async fn serve(listener: TcpListener, cfg: Config) -> std::io::Result<()> {
    serve_with_metrics(listener, None, cfg).await
}

/// [`serve`], plus an HTTP listener for `/healthz` and `/metrics` if one is given. Keep that
/// listener on loopback or behind a firewall: it holds no content, but it is for the operator.
pub async fn serve_with_metrics(
    listener: TcpListener,
    metrics_listener: Option<TcpListener>,
    cfg: Config,
) -> std::io::Result<()> {
    let store = Store::open(cfg.db_path.as_deref()).map_err(std::io::Error::other)?;
    let (push_tx, push_rx) = if cfg.push_enabled {
        let (tx, rx) = mpsc::channel(256);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let state = Arc::new(Shared {
        state: Mutex::default(),
        store,
        metrics: Metrics::default(),
        limits: IpLimits::new(cfg.max_conns_per_ip, cfg.conn_rate_per_ip),
        started: Instant::now(),
        push_tx,
    });
    if let Some(rx) = push_rx {
        tokio::spawn(push_worker(
            state.clone(),
            rx,
            cfg.push_allow_insecure,
            cfg.push_min_interval,
        ));
    }
    let acceptor = cfg.tls.as_ref().map(tls::acceptor).transpose()?;
    let cfg = Arc::new(cfg);
    {
        // Expired messages are also dropped lazily; this bounds disk use for idle recipients.
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let s = state.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = s.store.purge_expired();
                    s.limits.purge();
                })
                .await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }
    if let Some(ml) = metrics_listener {
        tokio::spawn(serve_metrics(ml, state.clone(), cfg.metrics_token.clone()));
    }
    loop {
        // An accept error (out of file descriptors, a client that reset while queued) is routine
        // under load: back off briefly and keep serving instead of ending the relay.
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let (state, cfg, acceptor) = (state.clone(), cfg.clone(), acceptor.clone());
        tokio::spawn(async move {
            let peer_ip = peer.ip();
            // A connection from a trusted proxy is admitted after the handshake, once its real
            // address is known. Everyone else is admitted (or turned away) before any work is done.
            let proxied = is_trusted(peer_ip, &cfg.trusted_proxies);
            let admission = if proxied {
                None
            } else {
                match state.limits.admit(peer_ip) {
                    Ok(a) => Some(a),
                    Err(r) => {
                        count_refusal(&state, r);
                        return;
                    }
                }
            };
            // Connection errors are routine (peers disconnect); never log content.
            match acceptor {
                Some(a) => {
                    // A stalled TLS handshake must not hold a connection slot open.
                    let Ok(Ok(stream)) =
                        tokio::time::timeout(cfg.auth_timeout, a.accept(stream)).await
                    else {
                        return;
                    };
                    let _ = handle(stream, peer_ip, admission, state, cfg).await;
                }
                None => {
                    let _ = handle(stream, peer_ip, admission, state, cfg).await;
                }
            }
        });
    }
}

fn count_refusal(state: &Shared, r: Refused) {
    match r {
        Refused::TooManyConnections => Metrics::inc(&state.metrics.refused_too_many_connections),
        Refused::TooFast => Metrics::inc(&state.metrics.refused_too_fast),
    }
}

fn err(code: &str, message: &str) -> ServerMsg {
    ServerMsg::Error {
        code: code.into(),
        message: message.into(),
    }
}

async fn send_json<S>(tx: &mut S, m: &ServerMsg) -> Result<(), ()>
where
    S: futures_util::Sink<Message> + Unpin,
{
    let s = serde_json::to_string(m).map_err(|_| ())?;
    // A peer that stops reading must not hold a connection (and what is queued for it) forever.
    tokio::time::timeout(WRITE_TIMEOUT, tx.send(Message::Text(s.into())))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

/// Messages that may wait to be written to one connection before it counts as too slow for live pushes.
const OUT_QUEUE: usize = 32;
/// Longest one write to a client may take.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Who is on the other end of a connection, once it has signed in.
struct Peer {
    me: String,
    my_ed: [u8; 32],
    /// This connection, and a weak handle to push to it (see `State::groups`).
    conn_id: u64,
    out: mpsc::WeakSender<ServerMsg>,
    /// The mailboxes this connection is subscribed to.
    subs: Mutex<HashSet<String>>,
}

/// What a request costs the connection's token bucket.
fn cost(m: &ClientMsg) -> f64 {
    match m {
        // Pings have their own, small bucket (see `handle`): free of the request budget so keepalive
        // never starves, but not unlimited.
        ClientMsg::Ping => 0.0,
        ClientMsg::Ack { .. } | ClientMsg::GroupUnsubscribe { .. } => 0.25,
        ClientMsg::TurnCredentials => 5.0,
        ClientMsg::RegisterPush { .. } | ClientMsg::UnregisterPush => 3.0,
        _ => 1.0,
    }
}

/// Keeps `connections_current` right however a connection ends.
struct OpenGauge(Arc<Shared>);
impl Drop for OpenGauge {
    fn drop(&mut self) {
        self.0
            .metrics
            .connections_current
            .fetch_sub(1, Ordering::Relaxed);
    }
}

async fn handle<S>(
    stream: S,
    peer_ip: IpAddr,
    admission: Option<Admission>,
    state: Arc<Shared>,
    cfg: Arc<Config>,
) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    Metrics::inc(&state.metrics.connections_total);
    state
        .metrics
        .connections_current
        .fetch_add(1, Ordering::Relaxed);
    let _open = OpenGauge(state.clone());
    // Room for the largest message blob or attachment chunk, hex-encoded in JSON.
    let ws_cfg = WebSocketConfig::default()
        .max_message_size(Some(frame_limit(&cfg)))
        .max_frame_size(Some(frame_limit(&cfg)));
    // Remember the forwarded-for header from the upgrade request: it is only believed if the
    // connection came from a trusted proxy (decided below).
    let forwarded = Arc::new(Mutex::new(None::<String>));
    let host_seen = Arc::new(Mutex::new(None::<String>));
    let seen = forwarded.clone();
    let host_cell = host_seen.clone();
    #[allow(clippy::result_large_err)]
    let callback = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        *host_cell.lock().unwrap() = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        *seen.lock().unwrap() = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        Ok(resp)
    };
    // A client that opens a connection and says nothing must not hold it (or a slot) open.
    let ws = tokio::time::timeout(
        cfg.auth_timeout,
        tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(ws_cfg)),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let _admission = match admission {
        Some(a) => a,
        None => {
            let header = forwarded.lock().unwrap().clone();
            let ip = client_ip(peer_ip, header.as_deref(), &cfg.trusted_proxies);
            match state.limits.admit(ip) {
                Ok(a) => a,
                Err(r) => {
                    count_refusal(&state, r);
                    return Err(());
                }
            }
        }
    };
    let (mut tx, mut rx) = ws.split();

    // 1. Challenge / response.
    let mut nonce = [0u8; 32];
    OsRng.fill_bytes(&mut nonce);
    send_json(
        &mut tx,
        &ServerMsg::Challenge {
            nonce: hex::encode(nonce),
        },
    )
    .await?;
    let first = tokio::time::timeout(cfg.auth_timeout, rx.next())
        .await
        .map_err(|_| ())?;
    let Some(Ok(Message::Text(text))) = first else {
        return Err(());
    };
    let Ok(ClientMsg::Auth { ed25519, sig }) = serde_json::from_str::<ClientMsg>(&text) else {
        let _ = send_json(&mut tx, &err("bad_request", "expected auth")).await;
        state.metrics.error("bad_request");
        return Err(());
    };
    // The answer must be signed for this relay's name, so a relay that passes our challenge to a
    // client cannot use the client's signature to sign in here as that client.
    let audience = cfg
        .auth_audience
        .clone()
        .or_else(|| host_seen.lock().unwrap().clone())
        .unwrap_or_default();
    let verified = (|| {
        let ed: [u8; 32] = hex::decode(ed25519).ok()?.try_into().ok()?;
        let sig: [u8; 64] = hex::decode(sig).ok()?.try_into().ok()?;
        verify_auth(&ed, &nonce, &audience, &sig).ok()?;
        Some((device_id_from_ed(&ed), ed))
    })();
    let Some((me, my_ed)) = verified else {
        let _ = send_json(&mut tx, &err("auth_failed", "signature rejected")).await;
        Metrics::inc(&state.metrics.auth_failed);
        state.metrics.error("auth_failed");
        return Err(());
    };
    Metrics::inc(&state.metrics.auth_ok);

    // 2. Register (a device may hold a few live connections; the oldest beyond the cap is closed)
    // and flush the queue. The database work runs on a blocking thread.
    let (out_tx, mut out_rx) = mpsc::channel(OUT_QUEUE);
    let out_for_peer = out_tx.downgrade();
    let registered = {
        let (state, me) = (state.clone(), me.clone());
        tokio::task::spawn_blocking(move || register(&state, &me, out_tx)).await
    };
    let Ok(Ok((conn_id, backlog, prekeys, fallback_age_secs))) = registered else {
        let _ = send_json(&mut tx, &err("internal", "storage error")).await;
        state.metrics.error("internal");
        return Err(());
    };
    let peer = Arc::new(Peer {
        me: me.clone(),
        my_ed,
        conn_id,
        out: out_for_peer,
        subs: Mutex::new(HashSet::new()),
    });
    let run = async {
        send_json(
            &mut tx,
            &ServerMsg::Ready {
                device_id: me.clone(),
                prekeys,
                fallback_age_secs,
            },
        )
        .await?;
        Metrics::add(&state.metrics.delivered_backlog, backlog.len() as u64);
        for m in backlog {
            send_json(&mut tx, &m).await?;
        }

        // 3. Serve.
        let mut claims = 0usize;
        let mut bucket = TokenBucket::new(cfg.req_rate, cfg.req_burst);
        // Clients ping every 25 s; ten at once and two a second is far more than any honest one needs.
        let mut ping_bucket = TokenBucket::new(if cfg.req_rate > 0.0 { 2.0 } else { 0.0 }, 10.0);
        let mut limited_streak = 0u32;
        let idle = tokio::time::sleep(cfg.idle_timeout);
        tokio::pin!(idle);
        loop {
            tokio::select! {
                // Nothing from the client for too long: it is gone or abandoned this connection.
                () = &mut idle => {
                    Metrics::inc(&state.metrics.connections_idle_closed);
                    return Ok(());
                }
                m = out_rx.recv() => match m {
                    Some(m) => send_json(&mut tx, &m).await?,
                    // Evicted because the device opened more connections than allowed. Close this one so its
                    // client notices; left open it would wait forever for messages that will never come.
                    None => return Ok(()),
                },
                frame = rx.next() => {
                    let Some(Ok(frame)) = frame else { return Ok::<(), ()>(()) };
                    idle.as_mut().reset(tokio::time::Instant::now() + cfg.idle_timeout);
                    let Message::Text(text) = frame else {
                        if matches!(frame, Message::Close(_)) { return Ok(()) }
                        continue;
                    };
                    let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
                        state.metrics.error("bad_request");
                        send_json(&mut tx, &err("bad_request", "unexpected message")).await?;
                        continue;
                    };
                    // Slow down a connection that asks too fast, and hang up on one that never does.
                    let c = cost(&msg);
                    let over = if matches!(msg, ClientMsg::Ping) {
                        !ping_bucket.allow(1.0)
                    } else {
                        c > 0.0 && !bucket.allow(c)
                    };
                    if over {
                        Metrics::inc(&state.metrics.rate_limited_requests);
                        state.metrics.error("rate_limited");
                        limited_streak += 1;
                        send_json(&mut tx, &err("rate_limited", "too many requests: slow down")).await?;
                        if limited_streak > MAX_RATE_LIMIT_STREAK { return Ok(()) }
                        continue;
                    }
                    limited_streak = 0;
                    // The database is blocking: run the request on a blocking thread so a slow
                    // disk delays this connection only, not everyone's.
                    let (st, cf, pr) = (state.clone(), cfg.clone(), peer.clone());
                    let (reply, c2) = tokio::task::spawn_blocking(move || {
                        let mut claims = claims;
                        let r = dispatch(&st, &cf, &pr, &mut claims, msg);
                        (r, claims)
                    })
                    .await
                    .map_err(|_| ())?;
                    claims = c2;
                    if let Some(r) = reply {
                        if let ServerMsg::Error { code, .. } = &r { state.metrics.error(code); }
                        send_json(&mut tx, &r).await?;
                    }
                }
            }
        }
    };
    let result = run.await;

    let mut st = state.state.lock().unwrap();
    if let Some(conns) = st.online.get_mut(&me) {
        conns.retain(|(c, _)| *c != conn_id);
        if conns.is_empty() {
            st.online.remove(&me);
        }
    }
    // Forget this connection's group subscriptions.
    let subscribed: Vec<String> = peer.subs.lock().unwrap().drain().collect();
    for mailbox in subscribed {
        if let Some(conns) = st.groups.get_mut(&mailbox) {
            conns.retain(|(c, _)| *c != conn_id);
            if conns.is_empty() {
                st.groups.remove(&mailbox);
            }
        }
    }
    result
}

/// Register a signed-in device and read what is waiting for it. The state lock is held while the
/// backlog is read so a message sent in between is either in the backlog or delivered live, never both.
fn register(
    state: &Shared,
    me: &str,
    out_tx: mpsc::Sender<ServerMsg>,
) -> rusqlite::Result<(u64, Vec<ServerMsg>, usize, Option<u64>)> {
    let mut st = state.state.lock().unwrap();
    st.next_conn += 1;
    let conn_id = st.next_conn;
    let conns = st.online.entry(me.to_string()).or_default();
    conns.push((conn_id, out_tx));
    while conns.len() > MAX_LIVE_PER_DEVICE {
        // Dropping the sender makes that connection's serve loop end (see `handle`).
        conns.remove(0);
        Metrics::inc(&state.metrics.connections_evicted);
    }
    let backlog: Vec<ServerMsg> = state
        .store
        .backlog(me)?
        .into_iter()
        .map(|m| ServerMsg::Deliver {
            id: m.id.to_string(),
            blob: hex::encode(&m.blob),
        })
        .collect();
    let prekeys = state.store.prekey_count(me)?;
    let fallback_age = state.store.fallback_age_secs(me)?;
    Ok((conn_id, backlog, prekeys, fallback_age))
}

/// Handle one request from a signed-in device. Runs on a blocking thread.
fn dispatch(
    state: &Shared,
    cfg: &Config,
    p: &Peer,
    claims: &mut usize,
    msg: ClientMsg,
) -> Option<ServerMsg> {
    let m = &state.metrics;
    match msg {
        ClientMsg::Send { to, blob, live } => on_send(state, cfg, &to, &blob, live),
        ClientMsg::Ack { id } => {
            Metrics::inc(&m.acks);
            on_ack(state, &p.me, &id);
            None
        }
        ClientMsg::Ping => Some(ServerMsg::Pong),
        ClientMsg::RegisterPush { url } => Some(if state.push_tx.is_none() {
            err("no_push", "this relay does not send wake-up pushes")
        } else {
            match push::parse(&url, cfg.push_allow_insecure) {
                Err(why) => err("bad_request", why),
                Ok(_) => match state.store.set_push(&p.me, &url) {
                    Ok(()) => ServerMsg::PushRegistered,
                    Err(_) => internal(),
                },
            }
        }),
        ClientMsg::UnregisterPush => Some(match state.store.clear_push(&p.me) {
            Ok(()) => ServerMsg::PushCleared,
            Err(_) => internal(),
        }),
        ClientMsg::TurnCredentials => Some(match (&cfg.turn_secret, cfg.turn_urls.is_empty()) {
            (Some(secret), false) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let ttl = cfg.turn_ttl.as_secs();
                let (username, credential) =
                    spike_core::turn::credentials(secret.as_bytes(), &p.me, now, ttl);
                ServerMsg::TurnCredentials {
                    urls: cfg.turn_urls.clone(),
                    username,
                    credential,
                    ttl_secs: ttl,
                }
            }
            _ => ServerMsg::Error {
                code: "no_turn".into(),
                message: "this relay has no TURN server configured".into(),
            },
        }),
        ClientMsg::PublishPrekeys { keys } => {
            let r = on_publish(state, &p.me, &p.my_ed, &keys);
            if matches!(r, ServerMsg::Published { .. }) {
                Metrics::add(&m.prekeys_published, keys.len() as u64);
            }
            Some(r)
        }
        ClientMsg::PublishFallback { key } => Some(on_fallback(state, &p.me, &p.my_ed, &key)),
        ClientMsg::PublishDeviceList { list } => {
            let r = on_publish_list(state, cfg, &p.me, &list);
            if matches!(r, ServerMsg::DeviceListStored { .. }) {
                Metrics::inc(&m.device_lists_published);
            }
            Some(r)
        }
        ClientMsg::FetchDeviceList { owner } => Some(on_fetch_list(state, &owner)),
        ClientMsg::PutBlobChunk {
            blob,
            index,
            total,
            data,
        } => {
            let r = on_put_chunk(state, cfg, &p.me, &blob, index, total, &data);
            if matches!(r, ServerMsg::BlobChunkStored { .. }) {
                Metrics::inc(&m.chunks_stored);
            }
            Some(r)
        }
        ClientMsg::GetBlobChunk { blob, index } => {
            let r = on_get_chunk(state, &blob, index);
            if matches!(r, ServerMsg::BlobChunk { data: Some(_), .. }) {
                Metrics::inc(&m.chunks_served);
            }
            Some(r)
        }
        ClientMsg::PublishKeyPackages { packages } => {
            Some(on_publish_key_packages(state, &p.me, &packages))
        }
        ClientMsg::PublishLastResortKeyPackage { package } => {
            Some(on_last_resort_key_package(state, &p.me, &package))
        }
        ClientMsg::ClaimKeyPackage { device } => {
            // Shares the per-connection claim limit with prekeys: a stranger cannot drain a device.
            *claims += 1;
            if *claims > cfg.max_claims_per_conn {
                Some(err("rate_limited", "too many claims on this connection"))
            } else {
                Some(on_claim_key_package(state, &device))
            }
        }
        ClientMsg::GroupCreate {
            mailbox,
            pubkey,
            sig,
        } => Some(on_group_create(state, cfg, p, &mailbox, &pubkey, &sig)),
        ClientMsg::GroupAppend {
            mailbox,
            kind,
            expected_seq,
            blob,
            seal,
            sig,
        } => Some(on_group_append(
            state,
            cfg,
            p,
            &mailbox,
            kind,
            expected_seq,
            &blob,
            seal.as_ref(),
            &sig,
        )),
        ClientMsg::GroupRead {
            mailbox,
            after_seq,
            limit,
            sig,
        } => Some(on_group_read(state, p, &mailbox, after_seq, limit, &sig)),
        ClientMsg::GroupSubscribe { mailbox, sig } => {
            Some(on_group_subscribe(state, cfg, p, &mailbox, &sig))
        }
        ClientMsg::GroupUnsubscribe { mailbox } => {
            on_group_unsubscribe(state, p, &mailbox);
            None
        }
        ClientMsg::ClaimPrekey { device } => {
            *claims += 1;
            if *claims > cfg.max_claims_per_conn {
                Some(err(
                    "rate_limited",
                    "too many prekey claims on this connection",
                ))
            } else {
                Metrics::inc(&m.prekey_claims);
                Some(on_claim(state, &device))
            }
        }
        _ => Some(err("bad_request", "unexpected message")),
    }
}

fn internal() -> ServerMsg {
    err("internal", "storage error")
}

fn on_send(
    state: &Shared,
    cfg: &Config,
    to: &str,
    blob_hex: &str,
    live: bool,
) -> Option<ServerMsg> {
    if !is_valid_device_id(to) {
        return Some(err("bad_request", "invalid recipient id"));
    }
    if blob_hex.len() > cfg.max_blob * 2 {
        return Some(err("too_large", "blob exceeds relay limit"));
    }
    let Ok(blob) = hex::decode(blob_hex) else {
        return Some(err("bad_request", "blob is not hex"));
    };
    let st = state.state.lock().unwrap();
    if live {
        // Never stored and never a wake-up push: connected devices get it now, others never.
        // The id is not a number, so the receiver's acknowledgement of it is ignored.
        let id = "live".to_string();
        for (_, conn) in st.online.get(to).into_iter().flatten() {
            if conn
                .try_send(ServerMsg::Deliver {
                    id: id.clone(),
                    blob: blob_hex.to_string(),
                })
                .is_ok()
            {
                Metrics::inc(&state.metrics.delivered_live);
            }
        }
        return Some(ServerMsg::Accepted { id });
    }
    let id = match state
        .store
        .enqueue(to, &blob, cfg.ttl.as_millis() as i64, cfg.queue_cap)
    {
        Ok(Enqueued::Id(id)) => id.to_string(),
        Ok(Enqueued::Full) => return Some(err("queue_full", "recipient queue is full")),
        Err(_) => return Some(internal()),
    };
    Metrics::inc(&state.metrics.messages_accepted);
    Metrics::add(&state.metrics.bytes_accepted, blob.len() as u64);
    // Nobody connected for this device: if it left a push endpoint, ask the worker to wake it.
    if st.online.get(to).is_none_or(|v| v.is_empty()) {
        if let Some(tx) = &state.push_tx {
            let _ = tx.try_send(to.to_string());
        }
    }
    for (_, live) in st.online.get(to).into_iter().flatten() {
        // A connection that is not keeping up (its queue is full) misses the live copy; the message
        // is in the database and arrives with the backlog when it reconnects.
        if live
            .try_send(ServerMsg::Deliver {
                id: id.clone(),
                blob: blob_hex.to_string(),
            })
            .is_ok()
        {
            Metrics::inc(&state.metrics.delivered_live);
        }
    }
    Some(ServerMsg::Accepted { id })
}

/// Sends the wake-up pushes. One at a time per device at most every `min_interval`, a few devices in parallel, and an
/// endpoint that says it is gone is forgotten. A failure is silent: the message is queued, and the device reads it the
/// next time it connects, as it always did.
async fn push_worker(
    state: Arc<Shared>,
    mut rx: mpsc::Receiver<String>,
    allow_insecure: bool,
    min_interval: Duration,
) {
    let throttle = Arc::new(push::Throttle::new(min_interval));
    let gate = Arc::new(tokio::sync::Semaphore::new(16));
    while let Some(device) = rx.recv().await {
        if !throttle.due(&device) {
            continue;
        }
        let Ok(permit) = gate.clone().try_acquire_owned() else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let dev = device.clone();
            let s2 = state.clone();
            let url = tokio::task::spawn_blocking(move || s2.store.push_url(&dev))
                .await
                .ok()
                .and_then(Result::ok)
                .flatten();
            let Some(url) = url else { return };
            let Ok(ep) = push::parse(&url, allow_insecure) else {
                return;
            };
            Metrics::inc(&state.metrics.pushes_sent);
            if push::send(&ep, allow_insecure).await == push::Outcome::Gone {
                let s3 = state.clone();
                let _ = tokio::task::spawn_blocking(move || s3.store.clear_push(&device)).await;
            }
        });
    }
}

fn on_ack(state: &Shared, me: &str, id: &str) {
    if let Ok(id) = id.parse::<i64>() {
        let _ = state.store.ack(me, id);
    }
}

fn on_publish(state: &Shared, me: &str, my_ed: &[u8; 32], keys: &[WirePrekey]) -> ServerMsg {
    // Count first: checking signatures is the expensive part, and a frame can hold thousands of keys.
    if keys.len() > MAX_PREKEYS {
        return err("too_large", "too many prekeys in one request");
    }
    // Reject the whole batch if any key is not signed by this device: a relay
    // that stores forgeries would hand them to senders.
    for w in keys {
        match SignedPrekey::from_wire(w) {
            Ok(p) if verify_prekey(my_ed, &p).is_ok() => {}
            _ => {
                return err(
                    "bad_prekey",
                    "prekey is malformed or not signed by this device",
                )
            }
        }
    }
    match state.store.publish_prekeys(me, keys, MAX_PREKEYS) {
        Ok(n) => ServerMsg::Published { prekeys: n },
        Err(_) => internal(),
    }
}

fn on_fallback(state: &Shared, me: &str, my_ed: &[u8; 32], key: &WirePrekey) -> ServerMsg {
    match SignedPrekey::from_wire(key) {
        Ok(p) if verify_prekey(my_ed, &p).is_ok() => match state.store.publish_fallback(me, key) {
            Ok(()) => ServerMsg::FallbackPublished,
            Err(_) => internal(),
        },
        _ => err(
            "bad_prekey",
            "prekey is malformed or not signed by this device",
        ),
    }
}

/// Accept a signed device list only if: it verifies; the publishing device is
/// in it (so a stranger cannot overwrite someone's list); and its version is
/// higher than the stored one, or identical content at the same version. The
/// relay applies the same rollback and equivocation rules as clients (ADR 0008).
fn on_publish_list(state: &Shared, cfg: &Config, me: &str, json: &str) -> ServerMsg {
    if json.len() > cfg.max_blob {
        return err("too_large", "device list exceeds relay limit");
    }
    let Ok(list) = serde_json::from_str::<DeviceList>(json) else {
        return err("bad_list", "not a device list");
    };
    let Ok(cards) = list.verify() else {
        return err("bad_list", "device list signature or contents invalid");
    };
    if !cards.iter().any(|c| c.device_id() == me) {
        return err("not_member", "only a device in the list may publish it");
    }
    match state.store.device_list(&list.owner) {
        Err(_) => return internal(),
        Ok(Some((v, _))) if v > list.version => {
            return err("rollback", &format!("relay already holds version {v}"))
        }
        Ok(Some((v, stored))) if v == list.version => {
            let same = serde_json::from_str::<DeviceList>(&stored)
                .map(|s| s == list)
                .unwrap_or(false);
            return if same {
                ServerMsg::DeviceListStored { version: v }
            } else {
                err(
                    "equivocation",
                    "a different list already holds this version",
                )
            };
        }
        _ => {}
    }
    match state.store.put_device_list(&list.owner, list.version, json) {
        Ok(()) => ServerMsg::DeviceListStored {
            version: list.version,
        },
        Err(_) => internal(),
    }
}

fn frame_limit(cfg: &Config) -> usize {
    cfg.max_blob.max(CHUNK + OVERHEAD).max(cfg.group_max_entry) * 2 + 4096
}

// ---------------------------------------------------------------- group mailboxes (ADR 0025)

fn group_limits(cfg: &Config) -> GroupLimits {
    GroupLimits {
        message_ttl_ms: cfg.group_message_ttl.as_millis() as i64,
        handshake_ttl_ms: cfg.group_handshake_ttl.as_millis() as i64,
        max_bytes: cfg.group_max_bytes,
        max_per_device: cfg.group_max_mailboxes_per_device,
    }
}

fn is_pubkey(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Entries one read may return.
const GROUP_PAGE: u32 = 200;

/// One-time key packages one device may have stored, and the largest one. An MLS key package is
/// about 400 bytes; the limit leaves room for extensions and nothing more.
const MAX_KEY_PACKAGES: usize = 200;
const MAX_KEY_PACKAGE_BYTES: usize = 16 * 1024;

fn decode_package(hex_pkg: &str) -> Result<Vec<u8>, ServerMsg> {
    if hex_pkg.len() > MAX_KEY_PACKAGE_BYTES * 2 {
        return Err(err("too_large", "key package exceeds the relay's limit"));
    }
    match hex::decode(hex_pkg) {
        Ok(b) if !b.is_empty() => Ok(b),
        _ => Err(err("bad_request", "a key package is empty or not hex")),
    }
}

/// The relay cannot read a key package, so it cannot tell a good one from rubbish: whoever claims
/// one checks it (and the credential in it) before use. It does bound what a device may store.
fn on_publish_key_packages(state: &Shared, me: &str, packages: &[String]) -> ServerMsg {
    if packages.is_empty() {
        return err("bad_request", "no key packages given");
    }
    let mut decoded = Vec::with_capacity(packages.len());
    for p in packages {
        match decode_package(p) {
            Ok(b) => decoded.push(b),
            Err(e) => return e,
        }
    }
    match state
        .store
        .publish_key_packages(me, &decoded, MAX_KEY_PACKAGES)
    {
        Ok(count) => {
            Metrics::add(&state.metrics.key_packages_published, decoded.len() as u64);
            ServerMsg::KeyPackagesPublished { count }
        }
        Err(_) => internal(),
    }
}

fn on_last_resort_key_package(state: &Shared, me: &str, package: &str) -> ServerMsg {
    let bytes = match decode_package(package) {
        Ok(b) => b,
        Err(e) => return e,
    };
    match state.store.publish_last_resort_key_package(me, &bytes) {
        Ok(()) => ServerMsg::LastResortKeyPackagePublished,
        Err(_) => internal(),
    }
}

fn on_claim_key_package(state: &Shared, device: &str) -> ServerMsg {
    if !is_valid_device_id(device) {
        return err("bad_request", "invalid device id");
    }
    match state.store.claim_key_package(device) {
        Ok(found) => {
            Metrics::inc(&state.metrics.key_package_claims);
            ServerMsg::KeyPackage {
                device: device.to_string(),
                last_resort: found.as_ref().is_some_and(|(_, last)| *last),
                package: found.map(|(p, _)| hex::encode(p)),
            }
        }
        Err(_) => internal(),
    }
}

fn on_group_create(
    state: &Shared,
    cfg: &Config,
    p: &Peer,
    mailbox: &str,
    pubkey: &str,
    sig: &str,
) -> ServerMsg {
    if !is_valid_mailbox_id(mailbox) || !is_pubkey(pubkey) {
        return err("bad_request", "invalid mailbox id or public key");
    }
    // The new key signs the request: whoever registers a mailbox holds its private key.
    if mailbox::verify(pubkey, &mailbox::create_bytes(&p.me, mailbox, pubkey), sig).is_err() {
        return err("bad_signature", "the new key did not sign this request");
    }
    match state
        .store
        .create_group_mailbox(mailbox, pubkey, &p.me, &group_limits(cfg))
    {
        Ok(GroupCreated::Created) => {
            Metrics::inc(&state.metrics.group_mailboxes_created);
            ServerMsg::GroupCreated {
                mailbox: mailbox.into(),
            }
        }
        Ok(GroupCreated::Exists) => err("exists", "that mailbox id is taken"),
        Ok(GroupCreated::OverQuota) => err(
            "quota_exceeded",
            "this device has created as many group mailboxes as allowed",
        ),
        Err(_) => internal(),
    }
}

/// Look up a mailbox's key and check a request signed with it. `Err` is the reply to send.
fn group_auth(state: &Shared, mailbox: &str, signed: &[u8], sig: &str) -> Result<(), ServerMsg> {
    let pubkey = match state.store.group_pubkey(mailbox) {
        Ok(Some(k)) => k,
        Ok(None) => return Err(err("unknown_mailbox", "no such mailbox")),
        Err(_) => return Err(internal()),
    };
    mailbox::verify(&pubkey, signed, sig).map_err(|_| err("bad_signature", "signature rejected"))
}

#[allow(clippy::too_many_arguments)]
fn on_group_append(
    state: &Shared,
    cfg: &Config,
    p: &Peer,
    mailbox: &str,
    kind: GroupKind,
    expected_seq: Option<u64>,
    blob_hex: &str,
    seal: Option<&WireSeal>,
    sig: &str,
) -> ServerMsg {
    if !is_valid_mailbox_id(mailbox) {
        return err("bad_request", "invalid mailbox id");
    }
    if blob_hex.len() > cfg.group_max_entry * 2 {
        return err("too_large", "entry exceeds the relay's limit");
    }
    let Ok(blob) = hex::decode(blob_hex) else {
        return err("bad_request", "entry is not hex");
    };
    // A commit changes the group, so it must say where it expects to land; only a commit may seal.
    if kind == GroupKind::Commit && expected_seq.is_none() {
        return err("bad_request", "a commit must say which position it expects");
    }
    if kind == GroupKind::Message && seal.is_some() {
        return err("bad_request", "only a commit can seal a mailbox");
    }
    if let Some(s) = seal {
        if !is_valid_mailbox_id(&s.mailbox) || !is_pubkey(&s.pubkey) {
            return err("bad_request", "invalid successor mailbox id or key");
        }
    }
    let signed = mailbox::append_bytes(
        &p.me,
        mailbox,
        kind,
        expected_seq,
        &blob,
        seal.map(|s| (s.mailbox.as_str(), s.pubkey.as_str())),
    );
    if let Err(e) = group_auth(state, mailbox, &signed, sig) {
        return e;
    }
    // The successor's key proves it exists, as when a mailbox is registered.
    if let Some(s) = seal {
        let proof = mailbox::create_bytes(&p.me, &s.mailbox, &s.pubkey);
        if mailbox::verify(&s.pubkey, &proof, &s.sig).is_err() {
            return err(
                "bad_signature",
                "the successor key did not sign this request",
            );
        }
    }
    // State before store, as everywhere: a subscriber registered in between gets this entry
    // pushed, one registered after reads it.
    let mut st = state.state.lock().unwrap();
    let outcome = state.store.append_group(
        mailbox,
        kind,
        expected_seq,
        &blob,
        seal.map(|s| (s.mailbox.as_str(), s.pubkey.as_str())),
        &p.me,
        &group_limits(cfg),
    );
    match outcome {
        Ok(GroupAppended::Appended { seq, sealed }) => {
            let m = &state.metrics;
            Metrics::inc(&m.group_appends);
            Metrics::add(&m.group_bytes_appended, blob.len() as u64);
            if let Some(subs) = st.groups.get_mut(mailbox) {
                let push = ServerMsg::GroupEntry {
                    mailbox: mailbox.into(),
                    entry: WireEntry {
                        seq,
                        kind,
                        blob: blob_hex.to_string(),
                    },
                    sealed,
                };
                // A connection that went away fails the send: forget it. One that is merely slow (its
                // queue is full) skips this push and reads the entry from the mailbox by sequence number.
                subs.retain(
                    |(_, tx)| match tx.upgrade().map(|tx| tx.try_send(push.clone())) {
                        Some(Ok(())) => {
                            Metrics::inc(&m.group_pushes);
                            true
                        }
                        Some(Err(mpsc::error::TrySendError::Full(_))) => true,
                        _ => false,
                    },
                );
            }
            ServerMsg::GroupAppended {
                mailbox: mailbox.into(),
                seq,
            }
        }
        Ok(GroupAppended::Conflict(next_seq)) => ServerMsg::GroupConflict {
            mailbox: mailbox.into(),
            next_seq,
        },
        Ok(GroupAppended::Unknown) => err("unknown_mailbox", "no such mailbox"),
        Ok(GroupAppended::Sealed) => err("sealed", "this mailbox was closed by a commit"),
        Ok(GroupAppended::Full) => err(
            "mailbox_full",
            "this mailbox is full of entries that cannot be dropped",
        ),
        Ok(GroupAppended::OverQuota) => err(
            "quota_exceeded",
            "this device has created as many group mailboxes as allowed",
        ),
        Ok(GroupAppended::TargetExists) => err("exists", "the successor mailbox id is taken"),
        Err(_) => internal(),
    }
}

fn on_group_read(
    state: &Shared,
    p: &Peer,
    mailbox: &str,
    after_seq: u64,
    limit: u32,
    sig: &str,
) -> ServerMsg {
    if !is_valid_mailbox_id(mailbox) {
        return err("bad_request", "invalid mailbox id");
    }
    if let Err(e) = group_auth(
        state,
        mailbox,
        &mailbox::read_bytes(&p.me, mailbox, after_seq, limit),
        sig,
    ) {
        return e;
    }
    match state
        .store
        .read_group(mailbox, after_seq, limit.clamp(1, GROUP_PAGE))
    {
        Ok(Some(page)) => {
            Metrics::inc(&state.metrics.group_reads);
            ServerMsg::GroupEntries {
                mailbox: mailbox.into(),
                entries: page
                    .entries
                    .into_iter()
                    .map(|e| WireEntry {
                        seq: e.seq,
                        kind: e.kind,
                        blob: hex::encode(e.blob),
                    })
                    .collect(),
                next_seq: page.next_seq,
                sealed: page.sealed,
            }
        }
        Ok(None) => err("unknown_mailbox", "no such mailbox"),
        Err(_) => internal(),
    }
}

fn on_group_subscribe(
    state: &Shared,
    cfg: &Config,
    p: &Peer,
    mailbox: &str,
    sig: &str,
) -> ServerMsg {
    if !is_valid_mailbox_id(mailbox) {
        return err("bad_request", "invalid mailbox id");
    }
    if let Err(e) = group_auth(
        state,
        mailbox,
        &mailbox::subscribe_bytes(&p.me, mailbox),
        sig,
    ) {
        return e;
    }
    let mut subs = p.subs.lock().unwrap();
    if !subs.contains(mailbox) && subs.len() >= cfg.max_group_subs_per_conn {
        return err(
            "quota_exceeded",
            "this connection is subscribed to as many mailboxes as allowed",
        );
    }
    let mut st = state.state.lock().unwrap();
    let status = match state.store.group_status(mailbox) {
        Ok(Some(s)) => s,
        Ok(None) => return err("unknown_mailbox", "no such mailbox"),
        Err(_) => return internal(),
    };
    if subs.insert(mailbox.to_string()) {
        st.groups
            .entry(mailbox.to_string())
            .or_default()
            .push((p.conn_id, p.out.clone()));
    }
    ServerMsg::GroupSubscribed {
        mailbox: mailbox.into(),
        next_seq: status.0,
        sealed: status.1,
    }
}

fn on_group_unsubscribe(state: &Shared, p: &Peer, mailbox: &str) {
    if p.subs.lock().unwrap().remove(mailbox) {
        let mut st = state.state.lock().unwrap();
        if let Some(conns) = st.groups.get_mut(mailbox) {
            conns.retain(|(c, _)| *c != p.conn_id);
            if conns.is_empty() {
                st.groups.remove(mailbox);
            }
        }
    }
}

fn is_blob_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Store an attachment chunk. Opaque to the relay; checked only for shape,
/// ownership (the first uploader of a blob owns it), and quota.
fn on_put_chunk(
    state: &Shared,
    cfg: &Config,
    me: &str,
    blob: &str,
    index: u32,
    total: u32,
    hex_data: &str,
) -> ServerMsg {
    let max_chunks = (MAX_FILE.div_ceil(CHUNK as u64)) as u32;
    if !is_blob_id(blob) || total == 0 || total > max_chunks || index >= total {
        return err("bad_request", "invalid blob id, index or chunk count");
    }
    if hex_data.len() > (CHUNK + OVERHEAD) * 2 {
        return err("too_large", "chunk exceeds relay limit");
    }
    let Ok(data) = hex::decode(hex_data) else {
        return err("bad_request", "chunk is not hex");
    };
    if data.len() < OVERHEAD {
        return err("bad_request", "chunk too short");
    }
    match state.store.put_chunk(
        blob,
        index,
        total,
        me,
        &data,
        cfg.blob_ttl.as_millis() as i64,
        cfg.blob_quota,
    ) {
        Ok(PutChunk::Stored) => ServerMsg::BlobChunkStored {
            blob: blob.into(),
            index,
        },
        Ok(PutChunk::NotOwner) => err("not_owner", "another device created this blob"),
        Ok(PutChunk::TotalMismatch) => err(
            "bad_request",
            "chunk count differs from the blob's first chunk",
        ),
        Ok(PutChunk::OverQuota) => {
            err("quota_exceeded", "this device's attachment storage is full")
        }
        Err(_) => internal(),
    }
}

fn on_get_chunk(state: &Shared, blob: &str, index: u32) -> ServerMsg {
    if !is_blob_id(blob) {
        return err("bad_request", "invalid blob id");
    }
    match state.store.get_chunk(blob, index) {
        Ok(data) => ServerMsg::BlobChunk {
            blob: blob.into(),
            index,
            data: data.map(hex::encode),
        },
        Err(_) => internal(),
    }
}

fn on_fetch_list(state: &Shared, owner: &str) -> ServerMsg {
    match state.store.device_list(owner) {
        Ok(found) => ServerMsg::DeviceList {
            owner: owner.to_string(),
            list: found.map(|(_, l)| l),
        },
        Err(_) => internal(),
    }
}

fn on_claim(state: &Shared, device: &str) -> ServerMsg {
    if !is_valid_device_id(device) {
        return err("bad_request", "invalid device id");
    }
    match state.store.claim_prekey(device) {
        Ok(found) => {
            let fallback = found.as_ref().is_some_and(|(_, f)| *f);
            ServerMsg::Prekey {
                device: device.to_string(),
                prekey: found.map(|(k, _)| k),
                fallback,
            }
        }
        Err(_) => internal(),
    }
}

// ---------------------------------------------------------------- operator HTTP

/// Compare two strings without stopping at the first difference, so response time does not leak
/// how much of a token was right.
fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

async fn serve_metrics(listener: TcpListener, state: Arc<Shared>, token: Option<String>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let (state, token) = (state.clone(), token.clone());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                http_once(&mut sock, &state, token.as_deref()),
            )
            .await;
        });
    }
}

/// Answer one HTTP/1.1 request on a socket, then close it. Only `GET /healthz` and
/// `GET /metrics`; there is nothing else to serve.
async fn http_once(sock: &mut tokio::net::TcpStream, state: &Arc<Shared>, token: Option<&str>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or("");
    let mut parts = request.split(' ');
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let bearer = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, v)| v.trim().strip_prefix("Bearer "))
        .map(str::trim);
    let (status, body) = if method != "GET" {
        ("405 Method Not Allowed", "method not allowed\n".to_string())
    } else if path == "/healthz" {
        let s = state.clone();
        match tokio::task::spawn_blocking(move || s.store.ping()).await {
            Ok(Ok(())) => ("200 OK", "ok\n".to_string()),
            _ => (
                "503 Service Unavailable",
                "database unavailable\n".to_string(),
            ),
        }
    } else if path == "/metrics" {
        let allowed = token.is_none_or(|t| bearer.is_some_and(|b| same_secret(b, t)));
        if !allowed {
            (
                "401 Unauthorized",
                "a bearer token is required\n".to_string(),
            )
        } else {
            let s = state.clone();
            let stats = tokio::task::spawn_blocking(move || s.store.stats().ok())
                .await
                .ok()
                .flatten();
            let live = state.state.lock().unwrap().online.len();
            (
                "200 OK",
                state
                    .metrics
                    .render(state.started.elapsed().as_secs(), live, stats.as_ref()),
            )
        }
    } else {
        ("404 Not Found", "not found\n".to_string())
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(reply.as_bytes()).await;
    let _ = sock.shutdown().await;
}
