//! The desktop's local API for other programs (ADR 0030), over real HTTP and a real relay: a
//! program asks, the person allows it, and the program then sends and receives its own data as
//! the person, and nothing else.

use serde_json::{json, Value};
use spike_app::local_api::LocalApi;
use spike_app::Session;
use spike_cli::apps::To;
use spike_core::app::Mode;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const APP: &str = "work.center";

async fn relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

struct Person {
    s: Arc<Session>,
    dir: PathBuf,
}
impl Drop for Person {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

async fn person(tag: &str, url: &str) -> Person {
    // Tests run in parallel and each makes an "alice": a counter keeps their folders apart.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spike-localapi-{tag}-{}-{n}", std::process::id()));
    let s = Session::create(
        &dir,
        "local api test passphrase".into(),
        KdfParams::insecure_for_tests(),
    )
    .unwrap();
    s.set_relay(url).unwrap();
    s.sync(Duration::from_millis(100)).await.unwrap();
    Person {
        s: Arc::new(s),
        dir,
    }
}

/// One HTTP request; returns (status, JSON body).
async fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Value) {
    let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    c.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).await.unwrap();
    let status: u16 = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("null");
    (status, serde_json::from_str(json).unwrap_or(Value::Null))
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Alice and Bob are contacts; Alice's local API runs. Returns the API and a token for work.center.
async fn setup() -> (String, Person, Person, LocalApi, String) {
    let url = relay().await;
    let alice = person("alice", &url).await;
    let bob = person("bob", &url).await;
    alice
        .s
        .add_contact("bob", &bob.s.overview().unwrap().me.card)
        .unwrap();
    bob.s
        .add_contact("alice", &alice.s.overview().unwrap().me.card)
        .unwrap();
    let held = alice.s.clone();
    let api = LocalApi::start(0, move || Some(held.clone()))
        .await
        .unwrap();
    let port = api.port();

    // Off until the person turns it on.
    let (code, _) = http(
        port,
        "POST",
        "/v1/connect",
        &[],
        Some(json!({"app": APP, "name": "Center Work"})),
    )
    .await;
    assert_eq!(code, 403);
    alice.s.app_set_api(true).unwrap();

    let (code, r) = http(
        port,
        "POST",
        "/v1/connect",
        &[],
        Some(json!({"app": APP, "name": "Center Work"})),
    )
    .await;
    assert_eq!(code, 200, "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    let (_, r) = http(port, "GET", &format!("/v1/connect/{id}"), &[], None).await;
    assert_eq!(r["status"], "waiting");
    let waiting = api.waiting();
    assert_eq!(
        (waiting.len(), waiting[0].name.as_str()),
        (1, "Center Work")
    );
    api.decide(&id, true).unwrap();
    let (_, r) = http(port, "GET", &format!("/v1/connect/{id}"), &[], None).await;
    assert_eq!(
        (r["status"].as_str(), r["app"].as_str()),
        (Some("allowed"), Some(APP))
    );
    let token = r["token"].as_str().unwrap().to_string();
    let (code, _) = http(port, "GET", &format!("/v1/connect/{id}"), &[], None).await;
    assert_eq!(code, 404, "the token is handed over once");
    assert!(alice.s.apps().unwrap().apps[0].connected);
    (url, alice, bob, api, token)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allowed_program_sends_and_receives_its_own_data() {
    let (_url, alice, bob, api, token) = setup().await;
    let port = api.port();
    let auth = format!("Bearer {token}");
    let h = [("Authorization", auth.as_str())];

    let (code, r) = http(port, "GET", "/v1/targets", &h, None).await;
    assert_eq!(code, 200);
    assert_eq!(r["contacts"][0]["name"], "bob");

    // The program sends to Bob, whose own app linked the library.
    bob.s.app_register(APP, "Center Work").unwrap();
    let (code, r) = http(
        port,
        "POST",
        "/v1/send",
        &h,
        Some(json!({"to": {"contact": "bob"}, "channel": "sheet-1", "mode": "stored", "data": b64(b"A1=1")})),
    )
    .await;
    assert_eq!(code, 200, "{r}");
    bob.s.sync(Duration::from_millis(300)).await.unwrap();
    let got = bob.s.app_events(APP, 0, Duration::ZERO).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].from.as_str(), got[0].channel.as_str()),
        ("alice", "sheet-1")
    );

    // A long wait wakes as soon as Bob's answer is received, live or stored.
    let auth2 = auth.clone();
    let waiter = tokio::spawn(async move {
        let started = Instant::now();
        let r = http(
            port,
            "GET",
            "/v1/events?after=0&wait=20",
            &[("Authorization", auth2.as_str())],
            None,
        )
        .await;
        (r, started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    bob.s
        .app_send(
            &To::Contact("alice".into()),
            APP,
            "sheet-1",
            Mode::Live,
            b"cursor A2",
        )
        .await
        .unwrap();
    alice.s.sync(Duration::from_millis(300)).await.unwrap();
    let ((code, r), took) = waiter.await.unwrap();
    assert_eq!(code, 200, "{r}");
    assert!(took < Duration::from_secs(10), "woke after {took:?}");
    let events = r["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{r}");
    assert_eq!(
        (events[0]["mode"].as_str(), events[0]["seq"].as_u64()),
        (Some("live"), Some(0))
    );
    assert_eq!(events[0]["data"], b64(b"cursor A2"));
    assert!(events[0].get("snapshot").is_none());

    // Removing the app in Settings revokes the token.
    alice.s.app_remove(APP).unwrap();
    let (code, _) = http(port, "GET", "/v1/targets", &h, None).await;
    assert_eq!(code, 401);
    api.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_travel_through_the_api() {
    let (_url, alice, bob, api, token) = setup().await;
    let port = api.port();
    let auth = format!("Bearer {token}");
    let h = [("Authorization", auth.as_str())];
    bob.s.app_register(APP, "Center Work").unwrap();

    // The program asks Bob for a channel's state; Bob's app answers it.
    let (code, r) = http(
        port,
        "POST",
        "/v1/state/request",
        &h,
        Some(json!({"to": {"contact": "bob"}, "channel": "sheet-1"})),
    )
    .await;
    assert_eq!(code, 200, "{r}");
    bob.s.sync(Duration::from_millis(300)).await.unwrap();
    let ask = bob.s.app_events(APP, 0, Duration::ZERO).await.unwrap();
    assert_eq!(ask.len(), 1);
    bob.s
        .app_send_state(&To::Contact("alice".into()), APP, "sheet-1", b"whole sheet")
        .await
        .unwrap();
    alice.s.sync(Duration::from_millis(300)).await.unwrap();
    let (_, r) = http(port, "GET", "/v1/events?after=0", &h, None).await;
    let e = &r["events"][0];
    assert_eq!(e["kind"], "state", "{r}");
    let seq = e["seq"].as_u64().unwrap();
    let (code, r) = http(port, "GET", &format!("/v1/state/{seq}"), &h, None).await;
    assert_eq!(code, 200, "{r}");
    assert_eq!(r["data"], b64(b"whole sheet"));
    api.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn web_pages_strangers_and_bad_tokens_are_kept_out() {
    let (_url, alice, _bob, api, token) = setup().await;
    let port = api.port();
    let auth = format!("Bearer {token}");
    // A browser adds Origin and Sec-Fetch-Site to what a page sends.
    for h in [
        ("Origin", "https://evil.example"),
        ("Sec-Fetch-Site", "cross-site"),
    ] {
        let (code, _) = http(
            port,
            "GET",
            "/v1/targets",
            &[("Authorization", auth.as_str()), h],
            None,
        )
        .await;
        assert_eq!(code, 403);
    }
    // DNS rebinding: another host name pointing at 127.0.0.1.
    let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    c.write_all(format!("GET /v1/targets HTTP/1.1\r\nHost: evil.example:{port}\r\nAuthorization: {auth}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 403"), "{out}");

    // Node's fetch sends Sec-Fetch-Mode: a program, not a page, so that is fine.
    let (code, _) = http(
        port,
        "GET",
        "/v1/targets",
        &[("Authorization", auth.as_str()), ("Sec-Fetch-Mode", "cors")],
        None,
    )
    .await;
    assert_eq!(code, 200);
    let (code, _) = http(port, "GET", "/v1/targets", &[], None).await;
    assert_eq!(code, 401);
    let (code, _) = http(
        port,
        "GET",
        "/v1/targets",
        &[("Authorization", "Bearer 00")],
        None,
    )
    .await;
    assert_eq!(code, 401);

    // A refused program gets nothing.
    let (_, r) = http(
        port,
        "POST",
        "/v1/connect",
        &[],
        Some(json!({"app": "com.other.app", "name": "Other"})),
    )
    .await;
    let id = r["id"].as_str().unwrap().to_string();
    api.decide(&id, false).unwrap();
    let (_, r) = http(port, "GET", &format!("/v1/connect/{id}"), &[], None).await;
    assert_eq!(r["status"], "denied");
    assert_eq!(alice.s.apps().unwrap().apps.len(), 1, "only work.center");
    let (code, _) = http(
        port,
        "POST",
        "/v1/connect",
        &[],
        Some(json!({"app": "Not An Id", "name": "x"})),
    )
    .await;
    assert_eq!(code, 400);
    api.stop();
}
