//! Abuse limits, content-free metrics and the operator endpoints (ADR 0022).

use futures_util::StreamExt;
use spike_cli::client::Relay;
use spike_core::identity::Identity;
use spike_relay::limits::Cidr;
use spike_relay::{serve, serve_with_metrics, Config};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

async fn relay_with(cfg: Config) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}

async fn connect(url: &str) -> anyhow::Result<Relay> {
    Relay::connect(url, &Identity::generate().unwrap()).await
}

#[tokio::test]
async fn one_address_cannot_hold_more_connections_than_allowed() {
    let url = relay_with(Config {
        max_conns_per_ip: 2,
        ..Config::default()
    })
    .await;
    let a = connect(&url).await.unwrap();
    let _b = connect(&url).await.unwrap();
    assert!(connect(&url).await.is_err(), "the third is turned away");
    // Closing one frees its slot.
    drop(a);
    let mut ok = false;
    for _ in 0..40 {
        if connect(&url).await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ok, "a slot is free again after a connection closes");
}

#[tokio::test]
async fn new_connections_are_rate_limited_per_address() {
    let url = relay_with(Config {
        conn_rate_per_ip: 3,
        ..Config::default()
    })
    .await;
    for _ in 0..3 {
        drop(connect(&url).await.unwrap());
    }
    assert!(
        connect(&url).await.is_err(),
        "the fourth within a minute is refused"
    );
}

#[tokio::test]
async fn a_connection_that_asks_too_fast_is_slowed_down_not_broken() {
    let url = relay_with(Config {
        req_rate: 2.0,
        req_burst: 3.0,
        ..Config::default()
    })
    .await;
    let mut r = connect(&url).await.unwrap();
    let owner = Identity::generate().unwrap().device_id();
    let mut refused = 0;
    for _ in 0..8 {
        if let Err(e) = r.fetch_device_list(&owner).await {
            assert!(e.to_string().contains("rate_limited"), "{e}");
            refused += 1;
        }
    }
    assert!(refused >= 4, "burst of 3 then refusals, got {refused}");
    // Pings are free even when the bucket is empty, and the connection works again after a pause.
    r.ping().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    r.fetch_device_list(&owner).await.unwrap();
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// Open a WebSocket carrying an `X-Forwarded-For` header and report whether the relay let it in
/// (it sends a challenge) or hung up.
async fn ws_with_xff(url: &str, xff: &str) -> Option<Ws> {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("x-forwarded-for", xff.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.ok()?;
    match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
        Ok(Some(Ok(Message::Text(t)))) if t.contains("challenge") => Some(ws),
        _ => None,
    }
}

#[tokio::test]
async fn the_forwarded_header_counts_only_from_a_trusted_proxy() {
    // Not trusted: the header is a client's say-so, so both connections are the same address.
    let url = relay_with(Config {
        max_conns_per_ip: 1,
        ..Config::default()
    })
    .await;
    let first = ws_with_xff(&url, "9.9.9.9").await;
    assert!(first.is_some());
    assert!(
        ws_with_xff(&url, "8.8.8.8").await.is_none(),
        "lying in the header does not buy a new address"
    );
    drop(first);

    // Trusted (the test connects from 127.0.0.1): each forwarded address has its own budget.
    let url = relay_with(Config {
        max_conns_per_ip: 1,
        trusted_proxies: vec![Cidr::parse("127.0.0.1").unwrap()],
        ..Config::default()
    })
    .await;
    let a = ws_with_xff(&url, "9.9.9.9").await;
    let b = ws_with_xff(&url, "8.8.8.8").await;
    assert!(
        a.is_some() && b.is_some(),
        "different clients behind the proxy"
    );
    assert!(
        ws_with_xff(&url, "9.9.9.9").await.is_none(),
        "the same forwarded client is limited"
    );
    // A forged left-hand entry does not help: the rightmost entry (the one the proxy added) counts.
    assert!(ws_with_xff(&url, "7.7.7.7, 9.9.9.9").await.is_none());
}

async fn http_get(addr: &str, path: &str, bearer: Option<&str>) -> (String, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let auth = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n{auth}\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

#[tokio::test]
async fn health_and_metrics_report_activity_without_content() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let m = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let maddr = m.local_addr().unwrap().to_string();
    tokio::spawn(serve_with_metrics(l, Some(m), Config::default()));

    let (status, body) = http_get(&maddr, "/healthz", None).await;
    assert!(
        status.contains("200") && body.trim() == "ok",
        "{status} {body}"
    );

    let alice = Identity::generate().unwrap();
    let bob = Identity::generate().unwrap();
    let mut a = Relay::connect(url.as_str(), &alice).await.unwrap();
    a.send(&bob.device_id(), b"secret ciphertext")
        .await
        .unwrap();

    let (status, text) = http_get(&maddr, "/metrics", None).await;
    assert!(status.contains("200"), "{status}");
    for line in [
        "spike_relay_messages_accepted_total 1",
        "spike_relay_bytes_accepted_total 17",
        "spike_relay_queued_messages 1",
        "spike_relay_connections_current 1",
        "spike_relay_db_up 1",
    ] {
        assert!(text.lines().any(|l| l == line), "missing {line}\n{text}");
    }
    // Nothing identifying or secret appears anywhere in the output.
    for needle in [
        alice.device_id(),
        bob.device_id(),
        "secret".to_string(),
        hex::encode(b"secret"),
    ] {
        assert!(!text.contains(&needle), "metrics leaked {needle}");
    }
    let (status, _) = http_get(&maddr, "/nope", None).await;
    assert!(status.contains("404"));
}

#[tokio::test]
async fn metrics_can_require_a_token() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let m = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let maddr = m.local_addr().unwrap().to_string();
    tokio::spawn(serve_with_metrics(
        l,
        Some(m),
        Config {
            metrics_token: Some("s3cret".into()),
            ..Config::default()
        },
    ));
    assert!(http_get(&maddr, "/metrics", None).await.0.contains("401"));
    assert!(http_get(&maddr, "/metrics", Some("wrong"))
        .await
        .0
        .contains("401"));
    assert!(http_get(&maddr, "/metrics", Some("s3cret"))
        .await
        .0
        .contains("200"));
    // The health check stays open so a load balancer can use it.
    assert!(http_get(&maddr, "/healthz", None).await.0.contains("200"));
}

#[tokio::test]
async fn many_busy_connections_do_not_stall_each_other() {
    // With the database on blocking threads, a pile of simultaneous requests all finish promptly.
    let path = std::env::temp_dir().join(format!("spike-hardening-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let url = relay_with(Config {
        db_path: Some(path),
        req_burst: 1000.0,
        req_rate: 1000.0,
        max_conns_per_ip: 0,
        conn_rate_per_ip: 0,
        ..Config::default()
    })
    .await;
    let bob = Identity::generate().unwrap().device_id();
    let started = std::time::Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let (url, bob) = (url.clone(), bob.clone());
        tasks.push(tokio::spawn(async move {
            let mut r = connect(&url).await.unwrap();
            for _ in 0..10 {
                r.send(&bob, b"x").await.unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

// ---- Audit fixes (docs/review/audit-findings.md) -------------------------------------------------

/// A client that connects and says nothing must be dropped, not held open (slowloris).
#[tokio::test]
async fn a_silent_connection_is_dropped_before_the_handshake_finishes() {
    let url = relay_with(Config {
        auth_timeout: Duration::from_millis(300),
        ..Config::default()
    })
    .await;
    let mut s = TcpStream::connect(url.trim_start_matches("ws://"))
        .await
        .unwrap();
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
        .await
        .expect("the relay closed the silent connection")
        .unwrap_or(0);
    assert_eq!(n, 0, "closed without sending anything");
}

/// Pings are cheap but metered: a flood runs into the rate limit.
#[tokio::test]
async fn a_ping_flood_is_rate_limited() {
    use futures_util::SinkExt;
    let url = relay_with(Config {
        req_rate: 1.0,
        req_burst: 5.0,
        ..Config::default()
    })
    .await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let Some(Ok(Message::Text(t))) = ws.next().await else {
        panic!("no challenge")
    };
    let spike_core::wire::ServerMsg::Challenge { nonce } = serde_json::from_str(&t).unwrap() else {
        panic!("not a challenge")
    };
    let id = Identity::generate().unwrap();
    let sig = id.sign_auth(
        &hex::decode(nonce).unwrap(),
        url.trim_start_matches("ws://"),
    );
    let auth = spike_core::wire::ClientMsg::Auth {
        ed25519: hex::encode(id.ed_public()),
        sig: hex::encode(sig),
    };
    ws.send(Message::Text(serde_json::to_string(&auth).unwrap()))
        .await
        .unwrap();
    let ping = serde_json::to_string(&spike_core::wire::ClientMsg::Ping).unwrap();
    for _ in 0..40 {
        if ws.send(Message::Text(ping.clone())).await.is_err() {
            break;
        }
    }
    let mut limited = false;
    let mut seen: Vec<String> = Vec::new();
    while let Ok(Some(Ok(Message::Text(t)))) =
        tokio::time::timeout(Duration::from_secs(2), ws.next()).await
    {
        seen.push(t.chars().take(60).collect::<String>());
        if t.contains("rate_limited") {
            limited = true;
            break;
        }
    }
    assert!(limited, "a flood of pings gets rate_limited; saw {seen:?}");
}

/// A contact can put any number in an envelope's clock. With overflow checks on, taking it at face value made
/// every later send panic.
#[test]
fn a_hostile_clock_value_does_not_wedge_sending() {
    use spike_cli::state::Home;
    use spike_core::vault::KdfParams;
    let dir = std::env::temp_dir().join(format!(
        "spike-test-lamport-{}",
        hex::encode(&Identity::generate().unwrap().ed_public()[..6])
    ));
    let home = Home::create(
        dir,
        "a long enough passphrase",
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap();
    home.observe_lamport(u64::MAX).unwrap();
    let a = home.next_lamport().unwrap();
    let b = home.next_lamport().unwrap();
    assert!(b > a, "the clock still moves forward");
    assert!(b < u64::MAX / 2, "and stays far from the end of the range");
}
