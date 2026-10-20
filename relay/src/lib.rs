//! Store-and-forward relay. Holds opaque blobs per recipient device until the
//! recipient acknowledges them or they expire. It authenticates devices by
//! Ed25519 challenge-response, enforces size/queue/TTL limits, and logs no
//! message content. Queues and prekeys are stored in SQLite (see [`store`]);
//! with no database path they live in memory and are lost on restart.

pub mod store;
pub mod tls;

use futures_util::{SinkExt, StreamExt};
use rand::{rngs::OsRng, RngCore};
use spike_core::attachment::{CHUNK, MAX_FILE, OVERHEAD};
use spike_core::identity::{
    device_id_from_ed, is_valid_device_id, verify_auth, verify_prekey, SignedPrekey, MAX_PREKEYS,
};
use spike_core::owner::DeviceList;
use spike_core::wire::{ClientMsg, ServerMsg, WirePrekey};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::{Enqueued, PutChunk, Store};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
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
            db_path: None,
            tls: None,
        }
    }
}

/// Live connections one device may hold at once. A client may keep one open to be told the moment
/// something arrives while another does its work (receiving sends receipts over a second
/// connection). Past this, the oldest is closed so a buggy or hostile client cannot pile them up.
const MAX_LIVE_PER_DEVICE: usize = 4;

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
}

pub async fn serve(listener: TcpListener, cfg: Config) -> std::io::Result<()> {
    let store = Store::open(cfg.db_path.as_deref()).map_err(std::io::Error::other)?;
    let state = Arc::new(Shared {
        state: Mutex::default(),
        store,
    });
    let acceptor = cfg.tls.as_ref().map(tls::acceptor).transpose()?;
    let cfg = Arc::new(cfg);
    {
        // Expired messages are also dropped lazily; this bounds disk use for idle recipients.
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let _ = state.store.purge_expired();
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }
    loop {
        let (stream, _) = listener.accept().await?;
        let (state, cfg, acceptor) = (state.clone(), cfg.clone(), acceptor.clone());
        tokio::spawn(async move {
            // Connection errors are routine (peers disconnect); never log content.
            match acceptor {
                Some(a) => {
                    // A stalled TLS handshake must not hold a connection slot open.
                    let Ok(Ok(stream)) =
                        tokio::time::timeout(cfg.auth_timeout, a.accept(stream)).await
                    else {
                        return;
                    };
                    let _ = handle(stream, state, cfg).await;
                }
                None => {
                    let _ = handle(stream, state, cfg).await;
                }
            }
        });
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

async fn handle<S>(stream: S, state: Arc<Shared>, cfg: Arc<Config>) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let ws_cfg = WebSocketConfig {
        // Room for the largest message blob or attachment chunk, hex-encoded in JSON.
        max_message_size: Some(frame_limit(&cfg)),
        max_frame_size: Some(frame_limit(&cfg)),
        ..Default::default()
    };
    let ws = tokio_tungstenite::accept_async_with_config(stream, Some(ws_cfg))
        .await
        .map_err(|_| ())?;
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
        return Err(());
    };

    // 2. Register (a device may hold a few live connections; the oldest beyond the cap is closed) and flush the queue.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let registered = (|| {
        let mut st = state.state.lock().unwrap();
        st.next_conn += 1;
        let conn_id = st.next_conn;
        let conns = st.online.entry(me.clone()).or_default();
        conns.push((conn_id, out_tx));
        while conns.len() > MAX_LIVE_PER_DEVICE {
            // Dropping the sender makes that connection's serve loop end (see below).
            conns.remove(0);
        }
        // Hold the state lock while reading the backlog so a message sent in
        // between is either in the backlog or delivered live, never both.
        let backlog: Vec<ServerMsg> = state
            .store
            .backlog(&me)?
            .into_iter()
            .map(|m| ServerMsg::Deliver {
                id: m.id.to_string(),
                blob: hex::encode(&m.blob),
            })
            .collect();
        let prekeys = state.store.prekey_count(&me)?;
        let fallback_age = state.store.fallback_age_secs(&me)?;
        Ok::<_, rusqlite::Error>((conn_id, backlog, prekeys, fallback_age))
    })();
    let Ok((conn_id, backlog, prekeys, fallback_age_secs)) = registered else {
        let _ = send_json(&mut tx, &err("internal", "storage error")).await;
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
        for m in backlog {
            send_json(&mut tx, &m).await?;
        }

        // 3. Serve.
        let mut claims = 0usize;
        let idle = tokio::time::sleep(cfg.idle_timeout);
        tokio::pin!(idle);
        loop {
            tokio::select! {
                // Nothing from the client for too long: it is gone or abandoned this connection.
                () = &mut idle => return Ok(()),
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
                    let reply = match serde_json::from_str::<ClientMsg>(&text) {
                        Ok(ClientMsg::Send { to, blob }) => on_send(&state, &cfg, &to, &blob),
                        Ok(ClientMsg::Ack { id }) => { on_ack(&state, &me, &id); None }
                        Ok(ClientMsg::Ping) => Some(ServerMsg::Pong),
                        Ok(ClientMsg::PublishPrekeys { keys }) => Some(on_publish(&state, &me, &my_ed, &keys)),
                        Ok(ClientMsg::PublishFallback { key }) => Some(on_fallback(&state, &me, &my_ed, &key)),
                        Ok(ClientMsg::PublishDeviceList { list }) => Some(on_publish_list(&state, &cfg, &me, &list)),
                        Ok(ClientMsg::FetchDeviceList { owner }) => Some(on_fetch_list(&state, &owner)),
                        Ok(ClientMsg::PutBlobChunk { blob, index, total, data }) => Some(on_put_chunk(&state, &cfg, &me, &blob, index, total, &data)),
                        Ok(ClientMsg::GetBlobChunk { blob, index }) => Some(on_get_chunk(&state, &blob, index)),
                        Ok(ClientMsg::ClaimPrekey { device }) => {
                            claims += 1;
                            if claims > cfg.max_claims_per_conn {
                                Some(err("rate_limited", "too many prekey claims on this connection"))
                            } else {
                                Some(on_claim(&state, &device))
                            }
                        }
                        _ => Some(err("bad_request", "unexpected message")),
                    };
                    if let Some(r) = reply { send_json(&mut tx, &r).await?; }
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
    for (_, live) in st.online.get(to).into_iter().flatten() {
        let _ = live.send(ServerMsg::Deliver {
            id: id.clone(),
            blob: blob_hex.to_string(),
        });
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
