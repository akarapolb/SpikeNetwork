//! The desktop's local API (ADR 0030): lets a program on this computer use app channels as the
//! person, after the person allows it once.
//!
//! - Plain HTTP on `127.0.0.1` only, one request per connection, JSON in and out.
//! - A program asks with `POST /v1/connect {"app", "name"}` and polls `GET /v1/connect/<id>`. The
//!   person sees a prompt; once they allow it the program gets a token, shown once. Only the token's
//!   SHA-256 is kept (sealed, with the app's registration). Removing the app revokes it.
//! - Everything else needs `Authorization: Bearer <token>` and acts only for that app: send, take
//!   events, ask for and send state, list who it can send to. It cannot read chats.
//! - Web pages are kept out: a request carrying `Origin` or `Sec-Fetch-Site` (what browsers add to
//!   what a page sends) is refused, and the `Host` must be this address, so DNS rebinding does not
//!   help either. A page cannot read the answers anyway (no CORS headers), so it never sees a token.
//!
//! Routes: see [`LocalApi::handle`] and the ADR.

use crate::apps::ToDto;
use crate::Session;
use anyhow::{anyhow, Result};
use base64::Engine;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spike_core::app::Mode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The port programs look for first.
pub const DEFAULT_PORT: u16 = 8797;
/// Largest request headers.
const MAX_HEAD: usize = 16 * 1024;
/// Largest request body: a snapshot of up to 16 MiB, in base64, with room for the JSON around it.
const MAX_BODY: usize = 22 * 1024 * 1024;
/// Largest snapshot a program can send through this API.
pub const MAX_SNAPSHOT: usize = 16 * 1024 * 1024;
/// Connection requests waiting for the person, at most.
const MAX_WAITING: usize = 4;
/// A connection request the person did not answer is forgotten after this.
const ASK_TTL_MS: u64 = 10 * 60 * 1000;
/// Longest a program may wait for events in one request.
const MAX_WAIT: Duration = Duration::from_secs(25);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Waiting,
    Allowed(String),
    Denied,
}

#[derive(Clone, Debug)]
struct Ask {
    id: String,
    app: String,
    name: String,
    at: u64,
    answer: Answer,
}

/// A program waiting for the person to allow it.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ConnectDto {
    pub id: String,
    pub app: String,
    pub name: String,
}

type Provider = dyn Fn() -> Option<Arc<Session>> + Send + Sync;

struct Inner {
    session: Box<Provider>,
    asks: Mutex<Vec<Ask>>,
    port: u16,
    /// Poked when a program asks to connect, so the screen can show the prompt at once.
    pub asked: tokio::sync::Notify,
}

/// A running local API. Dropping it does not stop it; call [`LocalApi::stop`].
#[derive(Clone)]
pub struct LocalApi {
    inner: Arc<Inner>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: String,
    token: Option<String>,
    body: Vec<u8>,
}

struct Reply(u16, Value);

fn err(code: u16, msg: impl Into<String>) -> Reply {
    Reply(code, json!({ "error": msg.into() }))
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        423 => "Locked",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    }
}

#[derive(Deserialize)]
struct ConnectBody {
    app: String,
    name: String,
}

#[derive(Deserialize)]
struct SendBody {
    to: ToDto,
    channel: String,
    #[serde(default = "stored")]
    mode: Mode,
    data: String,
}

fn stored() -> Mode {
    Mode::Stored
}

#[derive(Deserialize)]
struct StateRequestBody {
    to: ToDto,
    channel: String,
}

fn b64(s: &str) -> Result<Vec<u8>, Reply> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| err(400, "data must be base64"))
}

fn query_u64(q: &str, key: &str) -> Option<u64> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
}

impl LocalApi {
    /// Listen on `127.0.0.1:port` (0 picks a free port). `session` gives the unlocked session, if any.
    pub async fn start(
        port: u16,
        session: impl Fn() -> Option<Arc<Session>> + Send + Sync + 'static,
    ) -> Result<LocalApi> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
            anyhow!("could not open 127.0.0.1:{port} ({e}); is something else using it?")
        })?;
        let port = listener.local_addr()?.port();
        let api = LocalApi {
            inner: Arc::new(Inner {
                session: Box::new(session),
                asks: Mutex::new(vec![]),
                port,
                asked: tokio::sync::Notify::new(),
            }),
            task: Arc::new(Mutex::new(None)),
        };
        let me = api.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, peer)) = listener.accept().await else {
                    continue;
                };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let me = me.clone();
                tokio::spawn(async move {
                    let _ = me.serve(stream).await;
                });
            }
        });
        *api.task.lock().expect("api lock") = Some(task);
        Ok(api)
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    pub fn stop(&self) {
        if let Some(t) = self.task.lock().expect("api lock").take() {
            t.abort();
        }
    }

    /// Waits until a program asks to connect.
    pub async fn asked(&self) {
        self.inner.asked.notified().await
    }

    /// Programs waiting for the person to allow them.
    pub fn waiting(&self) -> Vec<ConnectDto> {
        let now = now_ms();
        let mut asks = self.inner.asks.lock().expect("api lock");
        asks.retain(|a| now < a.at + ASK_TTL_MS);
        asks.iter()
            .filter(|a| a.answer == Answer::Waiting)
            .map(|a| ConnectDto {
                id: a.id.clone(),
                app: a.app.clone(),
                name: a.name.clone(),
            })
            .collect()
    }

    /// The person's answer to a connection request. Allowing sets the app up on this device and
    /// makes a token the program collects once.
    pub fn decide(&self, id: &str, allow: bool) -> Result<()> {
        let mut asks = self.inner.asks.lock().expect("api lock");
        let a = asks
            .iter_mut()
            .find(|a| a.id == id && a.answer == Answer::Waiting)
            .ok_or_else(|| anyhow!("that request is no longer waiting"))?;
        if !allow {
            a.answer = Answer::Denied;
            return Ok(());
        }
        let s = (self.inner.session)().ok_or_else(|| anyhow!("locked"))?;
        s.app_register(&a.app, &a.name)?;
        let mut t = [0u8; 32];
        OsRng.fill_bytes(&mut t);
        let token = hex::encode(t);
        s.app_set_token(&a.app, &hash(&token))?;
        a.answer = Answer::Allowed(token);
        Ok(())
    }

    async fn serve(&self, mut stream: TcpStream) -> Result<()> {
        let reply = match tokio::time::timeout(
            Duration::from_secs(10),
            read_request(&mut stream, self.inner.port),
        )
        .await
        {
            Ok(Ok(req)) => self.handle(req).await,
            Ok(Err(r)) => r,
            Err(_) => err(400, "too slow"),
        };
        let body = reply.1.to_string();
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            reply.0,
            reason(reply.0),
            body.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
        stream.shutdown().await?;
        Ok(())
    }

    async fn handle(&self, r: Request) -> Reply {
        let Some(s) = (self.inner.session)() else {
            return err(423, "Spike is locked; unlock it and try again");
        };
        match s.app_api() {
            Ok(true) => {}
            _ => return err(403, "the local API is turned off in Spike's Settings"),
        }
        let parts: Vec<&str> = r.path.trim_matches('/').split('/').collect();
        match (r.method.as_str(), parts.as_slice()) {
            ("POST", ["v1", "connect"]) => self.connect(&r.body),
            ("GET", ["v1", "connect", id]) => self.collect(id),
            _ => match self.authed(&s, &r) {
                Ok(app) => match self.route(&s, &app, &r, &parts).await {
                    Ok(v) => Reply(200, v),
                    Err(e) => e,
                },
                Err(e) => e,
            },
        }
    }

    fn connect(&self, body: &[u8]) -> Reply {
        let Ok(c) = serde_json::from_slice::<ConnectBody>(body) else {
            return err(
                400,
                "send {\"app\": \"work.center\", \"name\": \"Center Work\"}",
            );
        };
        if !spike_core::app::valid_app_id(&c.app) {
            return err(
                400,
                "app must be a lowercase reverse-DNS id, like work.center",
            );
        }
        let name = c.name.trim().to_string();
        if name.is_empty()
            || name.len() > spike_cli::apps::MAX_APP_NAME
            || name.chars().any(char::is_control)
        {
            return err(400, "name must be 1 to 64 characters");
        }
        let now = now_ms();
        let mut asks = self.inner.asks.lock().expect("api lock");
        asks.retain(|a| now < a.at + ASK_TTL_MS);
        if let Some(a) = asks
            .iter()
            .find(|a| a.app == c.app && a.answer == Answer::Waiting)
        {
            return Reply(200, json!({ "id": a.id }));
        }
        if asks.iter().filter(|a| a.answer == Answer::Waiting).count() >= MAX_WAITING {
            return err(429, "too many programs are waiting for an answer");
        }
        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let id = hex::encode(id);
        asks.push(Ask {
            id: id.clone(),
            app: c.app,
            name,
            at: now,
            answer: Answer::Waiting,
        });
        drop(asks);
        self.inner.asked.notify_waiters();
        Reply(200, json!({ "id": id }))
    }

    fn collect(&self, id: &str) -> Reply {
        let mut asks = self.inner.asks.lock().expect("api lock");
        let Some(i) = asks.iter().position(|a| a.id == id) else {
            return err(404, "no such request (it may have expired)");
        };
        match asks[i].answer.clone() {
            Answer::Waiting => Reply(200, json!({ "status": "waiting" })),
            Answer::Denied => {
                asks.remove(i);
                Reply(200, json!({ "status": "denied" }))
            }
            Answer::Allowed(token) => {
                let app = asks.remove(i).app;
                Reply(
                    200,
                    json!({ "status": "allowed", "app": app, "token": token }),
                )
            }
        }
    }

    fn authed(&self, s: &Session, r: &Request) -> Result<String, Reply> {
        let token = r
            .token
            .as_deref()
            .ok_or_else(|| err(401, "missing Authorization: Bearer <token>"))?;
        match s.app_for_token(&hash(token)) {
            Ok(Some(app)) => Ok(app),
            Ok(None) => Err(err(401, "unknown token (the app may have been removed)")),
            Err(e) => Err(err(500, format!("{e:#}"))),
        }
    }

    async fn route(
        &self,
        s: &Session,
        app: &str,
        r: &Request,
        parts: &[&str],
    ) -> Result<Value, Reply> {
        let fail = |e: anyhow::Error| err(400, format!("{e:#}"));
        let json_body = |r: &Request| -> Result<Value, Reply> {
            serde_json::from_slice(&r.body).map_err(|_| err(400, "the body is not JSON"))
        };
        match (r.method.as_str(), parts) {
            ("GET", ["v1", "me"]) => Ok(json!({ "app": app })),
            ("GET", ["v1", "targets"]) => serde_json::to_value(s.app_targets().map_err(fail)?)
                .map_err(|e| err(500, e.to_string())),
            ("POST", ["v1", "send"]) => {
                let b: SendBody = serde_json::from_value(json_body(r)?)
                    .map_err(|_| err(400, "send {\"to\", \"channel\", \"mode\", \"data\"}"))?;
                let bytes = b64(&b.data)?;
                let to = b.to.to().map_err(fail)?;
                let id = s
                    .app_send(&to, app, &b.channel, b.mode, &bytes)
                    .await
                    .map_err(fail)?;
                Ok(json!({ "id": id }))
            }
            ("GET", ["v1", "events"]) => {
                let after = query_u64(&r.query, "after").unwrap_or(0);
                let wait =
                    Duration::from_secs(query_u64(&r.query, "wait").unwrap_or(0)).min(MAX_WAIT);
                let events = s.app_events(app, after, wait).await.map_err(fail)?;
                Ok(json!({ "events": events }))
            }
            ("POST", ["v1", "state", "request"]) => {
                let b: StateRequestBody = serde_json::from_value(json_body(r)?)
                    .map_err(|_| err(400, "send {\"to\", \"channel\"}"))?;
                let to = b.to.to().map_err(fail)?;
                let id = s
                    .app_request_state(&to, app, &b.channel)
                    .await
                    .map_err(fail)?;
                Ok(json!({ "id": id }))
            }
            ("POST", ["v1", "state", "send"]) => {
                let b: SendBody = serde_json::from_value(json_body(r)?)
                    .map_err(|_| err(400, "send {\"to\", \"channel\", \"data\"}"))?;
                let bytes = b64(&b.data)?;
                if bytes.len() > MAX_SNAPSHOT {
                    return Err(err(
                        413,
                        "state sent through the local API is limited to 16 MiB",
                    ));
                }
                let to = b.to.to().map_err(fail)?;
                let id = s
                    .app_send_state(&to, app, &b.channel, &bytes)
                    .await
                    .map_err(fail)?;
                Ok(json!({ "id": id }))
            }
            ("GET", ["v1", "state", seq]) => {
                let seq: u64 = seq.parse().map_err(|_| err(404, "no such state"))?;
                let bytes = s
                    .app_snapshot(app, seq)
                    .await
                    .map_err(|e| err(404, format!("{e:#}")))?;
                Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(bytes) }))
            }
            _ => Err(err(404, "no such route")),
        }
    }
}

/// Read one request, refusing anything a browser would send and any other host name.
async fn read_request(stream: &mut TcpStream, port: u16) -> Result<Request, Reply> {
    let mut buf = Vec::with_capacity(4096);
    let head_len = loop {
        let mut chunk = [0u8; 4096];
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|_| err(400, "read failed"))?;
        if n == 0 {
            return Err(err(400, "incomplete request"));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return Err(err(413, "headers too large"));
        }
    };
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    match req.parse(&buf[..head_len]) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(err(400, "not an HTTP request")),
    }
    let method = req.method.unwrap_or_default().to_string();
    let target = req.path.unwrap_or_default().to_string();
    let mut host = None;
    let mut token = None;
    let mut length = 0usize;
    for h in req.headers.iter() {
        let name = h.name.to_ascii_lowercase();
        let value = std::str::from_utf8(h.value).unwrap_or_default().trim();
        if name == "origin" || name == "sec-fetch-site" {
            return Err(err(403, "web pages cannot use this API"));
        }
        match name.as_str() {
            "host" => host = Some(value.to_ascii_lowercase()),
            "authorization" => token = value.strip_prefix("Bearer ").map(|t| t.trim().to_string()),
            "content-length" => {
                length = value.parse().map_err(|_| err(400, "bad Content-Length"))?
            }
            "transfer-encoding" => return Err(err(400, "send a Content-Length instead")),
            _ => {}
        }
    }
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if !host.as_ref().is_some_and(|h| allowed.contains(h)) {
        return Err(err(403, "wrong Host"));
    }
    if length > MAX_BODY {
        return Err(err(413, "request too large"));
    }
    let mut body = buf[head_len..].to_vec();
    if body.len() > length {
        return Err(err(400, "body longer than Content-Length"));
    }
    while body.len() < length {
        let mut chunk = vec![0u8; (length - body.len()).min(64 * 1024)];
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|_| err(400, "read failed"))?;
        if n == 0 {
            return Err(err(400, "incomplete body"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    Ok(Request {
        method,
        path,
        query,
        token,
        body,
    })
}
