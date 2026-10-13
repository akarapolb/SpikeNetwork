//! Store-and-forward relay. Holds opaque blobs per recipient device until the
//! recipient acknowledges them or they expire. It authenticates devices by
//! Ed25519 challenge-response, enforces size/queue/TTL limits, and logs no
//! message content. Queues and prekeys are stored in SQLite (see [`store`]);
//! with no database path they live in memory and are lost on restart.

pub mod store;

use futures_util::{SinkExt, StreamExt};
use rand::{rngs::OsRng, RngCore};
use spike_core::identity::{
    device_id_from_ed, is_valid_device_id, verify_auth, verify_prekey, SignedPrekey, MAX_PREKEYS,
};
use spike_core::wire::{ClientMsg, ServerMsg, WirePrekey};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::{Enqueued, Store};
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
    /// Prekeys one connection may claim from others before being refused.
    pub max_claims_per_conn: usize,
    /// SQLite file for queues and prekeys; `None` keeps them in memory only.
    pub db_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_blob: 64 * 1024,
            queue_cap: 1000,
            ttl: Duration::from_secs(7 * 24 * 3600),
            auth_timeout: Duration::from_secs(10),
            max_claims_per_conn: 20,
            db_path: None,
        }
    }
}

#[derive(Default)]
struct State {
    online: HashMap<String, (u64, mpsc::UnboundedSender<ServerMsg>)>,
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

async fn handle(stream: TcpStream, state: Arc<Shared>, cfg: Arc<Config>) -> Result<(), ()> {
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
        Some((device_id_from_ed(&ed), ed))
    })();
    let Some((me, my_ed)) = verified else {
        let _ = send_json(&mut tx, &err("auth_failed", "signature rejected")).await;
        return Err(());
    };

    // 2. Register (replacing any older connection for this device) and flush the queue.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let registered = (|| {
        let mut st = state.state.lock().unwrap();
        st.next_conn += 1;
        let conn_id = st.next_conn;
        st.online.insert(me.clone(), (conn_id, out_tx));
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
        Ok::<_, rusqlite::Error>((conn_id, backlog, prekeys))
    })();
    let Ok((conn_id, backlog, prekeys)) = registered else {
        let _ = send_json(&mut tx, &err("internal", "storage error")).await;
        return Err(());
    };
    let run = async {
        send_json(
            &mut tx,
            &ServerMsg::Ready {
                device_id: me.clone(),
                prekeys,
            },
        )
        .await?;
        for m in backlog {
            send_json(&mut tx, &m).await?;
        }

        // 3. Serve.
        let mut claims = 0usize;
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
                        Ok(ClientMsg::PublishPrekeys { keys }) => Some(on_publish(&state, &me, &my_ed, &keys)),
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
    if st.online.get(&me).map(|(c, _)| *c) == Some(conn_id) {
        st.online.remove(&me);
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
    if let Some((_, live)) = st.online.get(to) {
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

fn on_claim(state: &Shared, device: &str) -> ServerMsg {
    if !is_valid_device_id(device) {
        return err("bad_request", "invalid device id");
    }
    match state.store.claim_prekey(device) {
        Ok(prekey) => ServerMsg::Prekey {
            device: device.to_string(),
            prekey,
        },
        Err(_) => internal(),
    }
}
