//! Wake-up pushes (UnifiedPush, ADR 0028): a device that is offline can leave a push endpoint with the relay; a
//! message queued for it then makes the relay send one content-free POST there. The relay refuses endpoints that
//! would point it at its own network.

use spike_cli::client::Relay;
use spike_core::identity::Identity;
use spike_relay::{serve, Config};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn relay_with(cfg: Config) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}

fn test_cfg() -> Config {
    Config {
        push_allow_insecure: true,
        push_min_interval: Duration::from_millis(1200),
        ..Config::default()
    }
}

type Seen = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A push endpoint that records every request (path and body) and answers with `status`.
async fn endpoint(status: u16) -> (String, Seen) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).to_string();
                        let len: usize = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        while buf.len() < end + 4 + len {
                            let n = s.read(&mut tmp).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        let path = head
                            .lines()
                            .next()
                            .unwrap_or("")
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("")
                            .to_string();
                        log.lock().unwrap().push((path, buf[end + 4..].to_vec()));
                        let _ = s.write_all(format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
                        break;
                    }
                }
            });
        }
    });
    (url, seen)
}

#[tokio::test]
async fn a_message_for_an_offline_device_sends_one_content_free_wake_up() {
    let relay = relay_with(test_cfg()).await;
    let (ep, seen) = endpoint(201).await;
    let (bob, alice) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    {
        let mut r = Relay::connect(relay.as_str(), &bob).await.unwrap();
        r.register_push(&format!("{ep}/upBOB")).await.unwrap();
    } // bob disconnects
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut a = Relay::connect(relay.as_str(), &alice).await.unwrap();
    a.send(
        &bob.device_id(),
        b"a sealed blob that must never reach the endpoint",
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    {
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "/upBOB");
        assert_eq!(
            got[0].1, b"spike",
            "the push carries nothing but a fixed word"
        );
    }
    // several messages in a burst: still no more than one push per interval
    for _ in 0..4 {
        a.send(&bob.device_id(), b"x").await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "coalesced within the interval"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    a.send(&bob.device_id(), b"later").await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(seen.lock().unwrap().len(), 2, "one more after the interval");
}

#[tokio::test]
async fn a_connected_device_is_not_pushed_and_an_unregistered_one_neither() {
    let relay = relay_with(test_cfg()).await;
    let (ep, seen) = endpoint(200).await;
    let (bob, alice) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let mut b = Relay::connect(relay.as_str(), &bob).await.unwrap();
    b.register_push(&format!("{ep}/up")).await.unwrap();
    let mut a = Relay::connect(relay.as_str(), &alice).await.unwrap();
    a.send(&bob.device_id(), b"while bob is connected")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        seen.lock().unwrap().is_empty(),
        "bob is online: the message is delivered, no push"
    );
    b.unregister_push().await.unwrap();
    drop(b);
    tokio::time::sleep(Duration::from_millis(200)).await;
    a.send(&bob.device_id(), b"after unregistering")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        seen.lock().unwrap().is_empty(),
        "no endpoint registered: no push"
    );
}

#[tokio::test]
async fn an_endpoint_that_says_it_is_gone_is_forgotten() {
    let relay = relay_with(test_cfg()).await;
    let (ep, seen) = endpoint(410).await;
    let (bob, alice) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    drop({
        let mut r = Relay::connect(relay.as_str(), &bob).await.unwrap();
        r.register_push(&format!("{ep}/gone")).await.unwrap();
        r
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut a = Relay::connect(relay.as_str(), &alice).await.unwrap();
    a.send(&bob.device_id(), b"one").await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(1300)).await;
    a.send(&bob.device_id(), b"two").await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the 410 removed the registration"
    );
}

#[tokio::test]
async fn by_default_the_relay_refuses_endpoints_that_would_point_it_at_its_own_network() {
    let relay = relay_with(Config::default()).await;
    let mut r = Relay::connect(relay.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    for bad in [
        "http://push.example.org/t",
        "https://127.0.0.1/t",
        "https://10.0.0.8/t",
        "https://169.254.169.254/latest/meta-data/",
        "https://localhost/t",
        "https://user:pw@push.example.org/t",
        "not a url",
    ] {
        let e = r.register_push(bad).await.unwrap_err().to_string();
        assert!(e.contains("bad_request"), "{bad}: {e}");
    }
    r.register_push("https://ntfy.example.org/upSomeTopic")
        .await
        .unwrap();
    // the connection is still fine afterwards
    r.ping().await.unwrap();
}

#[tokio::test]
async fn a_relay_with_pushes_off_says_so() {
    let relay = relay_with(Config {
        push_enabled: false,
        ..Config::default()
    })
    .await;
    let mut r = Relay::connect(relay.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    let e = r
        .register_push("https://ntfy.example.org/t")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("no_push"), "{e}");
}
