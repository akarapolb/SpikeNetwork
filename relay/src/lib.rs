//! Store-and-forward relay. Holds opaque blobs per recipient device until the
//! recipient acknowledges them or they expire. It authenticates devices by
//! Ed25519 challenge-response, enforces size/queue/TTL limits, and logs no
//! message content. Queues are in memory in Phase 0 (lost on restart).

use futures_util::{SinkExt, StreamExt};
use rand::{rngs::OsRng, RngCore};
use spike_core::identity::{device_id_from_ed, is_valid_device_id, verify_auth};
use spike_core::wire::{ClientMsg, ServerMsg};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_blob: 64 * 1024,
            queue_cap: 1000,
            ttl: Duration::from_secs(7 * 24 * 3600),
            auth_timeout: Duration::from_secs(10),
        }
    }
}

struct Queued {
    id: String,
    blob: Vec<u8>,
    expires: Instant,
}

#[derive(Default)]
struct State {
    queues: HashMap<String, VecDeque<Queued>>,
    online: HashMap<String, (u64, mpsc::UnboundedSender<ServerMsg>)>,
    next_id: u64,
    next_conn: u64,
}

type Shared = Arc<Mutex<State>>;

pub async fn serve(listener: TcpListener, cfg: Config) -> std::io::Result<()> {
    let state: Shared = Arc::default();
    let cfg = Arc::new(cfg);
    loop {
        let (stream, _) = listener.accept().await?;
        let (state, cfg) = (state.clone(), cfg.clone());
        tokio::spawn(async move {
            // Connection errors are routine (peers disconnect); never log content.
            let _ = handle(stream, state, cfg).await;
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

async fn handle(stream: TcpStream, state: Shared, cfg: Arc<Config>) -> Result<(), ()> {
    let ws_cfg = WebSocketConfig {
        max_message_size: Some(cfg.max_blob * 2 + 1024),
        max_frame_size: Some(cfg.max_blob * 2 + 1024),
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
        Some(device_id_from_ed(&ed))
    })();
    let Some(me) = verified else {
        let _ = send_json(&mut tx, &err("auth_failed", "signature rejected")).await;
        return Err(());
    };

    // 2. Register (replacing any older connection for this device) and flush the queue.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let (conn_id, backlog) = {
        let mut st = state.lock().unwrap();
        st.next_conn += 1;
        let conn_id = st.next_conn;
        st.online.insert(me.clone(), (conn_id, out_tx));
        let now = Instant::now();
        let q = st.queues.entry(me.clone()).or_default();
        q.retain(|m| m.expires > now);
        let backlog: Vec<ServerMsg> = q
            .iter()
            .map(|m| ServerMsg::Deliver {
                id: m.id.clone(),
                blob: hex::encode(&m.blob),
            })
            .collect();
        (conn_id, backlog)
    };
    let run = async {
        send_json(
            &mut tx,
            &ServerMsg::Ready {
                device_id: me.clone(),
            },
        )
        .await?;
        for m in backlog {
            send_json(&mut tx, &m).await?;
        }

        // 3. Serve.
        loop {
            tokio::select! {
                Some(m) = out_rx.recv() => send_json(&mut tx, &m).await?,
                frame = rx.next() => {
                    let Some(Ok(frame)) = frame else { return Ok::<(), ()>(()) };
                    let Message::Text(text) = frame else {
                        if matches!(frame, Message::Close(_)) { return Ok(()) }
                        continue;
                    };
                    let reply = match serde_json::from_str::<ClientMsg>(&text) {
                        Ok(ClientMsg::Send { to, blob }) => on_send(&state, &cfg, &to, &blob),
                        Ok(ClientMsg::Ack { id }) => { on_ack(&state, &me, &id); None }
                        _ => Some(err("bad_request", "unexpected message")),
                    };
                    if let Some(r) = reply { send_json(&mut tx, &r).await?; }
                }
            }
        }
    };
    let result = run.await;

    let mut st = state.lock().unwrap();
    if st.online.get(&me).map(|(c, _)| *c) == Some(conn_id) {
        st.online.remove(&me);
    }
    result
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
    let mut st = state.lock().unwrap();
    let now = Instant::now();
    st.next_id += 1;
    let id = st.next_id.to_string();
    let q = st.queues.entry(to.to_string()).or_default();
    q.retain(|m| m.expires > now);
    if q.len() >= cfg.queue_cap {
        return Some(err("queue_full", "recipient queue is full"));
    }
    q.push_back(Queued {
        id: id.clone(),
        blob,
        expires: now + cfg.ttl,
    });
    if let Some((_, live)) = st.online.get(to) {
        let _ = live.send(ServerMsg::Deliver {
            id: id.clone(),
            blob: blob_hex.to_string(),
        });
    }
    Some(ServerMsg::Accepted { id })
}

fn on_ack(state: &Shared, me: &str, id: &str) {
    if let Some(q) = state.lock().unwrap().queues.get_mut(me) {
        q.retain(|m| m.id != id);
    }
}
