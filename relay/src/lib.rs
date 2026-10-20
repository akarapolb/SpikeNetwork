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
use spike_core::owner::DeviceList;
use spike_core::wire::{ClientMsg, ServerMsg, WirePrekey};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use store::{Enqueued, PutChunk, Store};
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
    online: HashMap<String, Vec<(u64, mpsc::UnboundedSender<ServerMsg>)>>,
    next_conn: u64,
}

/// `State` is always locked before the store; keep that order.
struct Shared {
    state: Mutex<State>,
    store: Store,
    metrics: Metrics,
    limits: Arc<IpLimits>,
    started: Instant,
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
    let state = Arc::new(Shared {
        state: Mutex::default(),
        store,
        metrics: Metrics::default(),
        limits: IpLimits::new(cfg.max_conns_per_ip, cfg.conn_rate_per_ip),
        started: Instant::now(),
    });
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
        let (stream, peer) = listener.accept().await?;
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
    tx.send(Message::Text(s)).await.map_err(|_| ())
}

/// Who is on the other end of a connection, once it has signed in.
struct Peer {
    me: String,
    my_ed: [u8; 32],
}

/// What a request costs the connection's token bucket.
fn cost(m: &ClientMsg) -> f64 {
    match m {
        ClientMsg::Ping => 0.0,
        ClientMsg::Ack { .. } => 0.25,
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
    let ws_cfg = WebSocketConfig {
        // Room for the largest message blob or attachment chunk, hex-encoded in JSON.
        max_message_size: Some(frame_limit(&cfg)),
        max_frame_size: Some(frame_limit(&cfg)),
        ..Default::default()
    };
    // Remember the forwarded-for header from the upgrade request: it is only believed if the
    // connection came from a trusted proxy (decided below).
    let forwarded = Arc::new(Mutex::new(None::<String>));
    let seen = forwarded.clone();
    #[allow(clippy::result_large_err)]
    let callback = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        *seen.lock().unwrap() = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        Ok(resp)
    };
    let ws = tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(ws_cfg))
        .await
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
    let verified = (|| {
        let ed: [u8; 32] = hex::decode(ed25519).ok()?.try_into().ok()?;
        let sig: [u8; 64] = hex::decode(sig).ok()?.try_into().ok()?;
        verify_auth(&ed, &nonce, &sig).ok()?;
        Some((device_id_from_ed(&ed), ed))
    })();
    let Some((me, my_ed)) = verified else {
        let _ = send_json(&mut tx, &err("auth_failed", "signature rejected")).await;
        Metrics::inc(&state.metrics.auth_failed);
        state.metrics.error("auth_failed");
        return Err(());
    };
    Metrics::inc(&state.metrics.auth_ok);
    let peer = Arc::new(Peer {
        me: me.clone(),
        my_ed,
    });

    // 2. Register (a device may hold a few live connections; the oldest beyond the cap is closed)
    // and flush the queue. The database work runs on a blocking thread.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let registered = {
        let (state, me) = (state.clone(), me.clone());
        tokio::task::spawn_blocking(move || register(&state, &me, out_tx)).await
    };
    let Ok(Ok((conn_id, backlog, prekeys, fallback_age_secs))) = registered else {
        let _ = send_json(&mut tx, &err("internal", "storage error")).await;
        state.metrics.error("internal");
        return Err(());
    };
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
                    if c > 0.0 && !bucket.allow(c) {
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
    result
}

/// Register a signed-in device and read what is waiting for it. The state lock is held while the
/// backlog is read so a message sent in between is either in the backlog or delivered live, never both.
fn register(
    state: &Shared,
    me: &str,
    out_tx: mpsc::UnboundedSender<ServerMsg>,
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
        ClientMsg::Send { to, blob } => on_send(state, cfg, &to, &blob),
        ClientMsg::Ack { id } => {
            Metrics::inc(&m.acks);
            on_ack(state, &p.me, &id);
            None
        }
        ClientMsg::Ping => Some(ServerMsg::Pong),
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

fn on_send(state: &Shared, cfg: &Config, to: &str, blob_hex: &str) -> Option<ServerMsg> {
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
    for (_, live) in st.online.get(to).into_iter().flatten() {
        if live
            .send(ServerMsg::Deliver {
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

fn on_ack(state: &Shared, me: &str, id: &str) {
    if let Ok(id) = id.parse::<i64>() {
        let _ = state.store.ack(me, id);
    }
}

fn on_publish(state: &Shared, me: &str, my_ed: &[u8; 32], keys: &[WirePrekey]) -> ServerMsg {
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
    cfg.max_blob.max(CHUNK + OVERHEAD) * 2 + 4096
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
