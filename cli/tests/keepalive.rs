//! Keepalive: dead connections are noticed, and quiet ones are cleaned up.

use spike_cli::client::Relay;
use spike_core::identity::Identity;
use spike_relay::{serve, Config};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn relay_with(cfg: Config) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}

/// A TCP proxy in front of the relay. When `blackhole` is set it keeps both sockets open but
/// silently throws everything away, like a router that has dropped a connection without telling anyone.
async fn proxy_to(relay_url: &str, blackhole: Arc<AtomicBool>) -> String {
    let relay_addr = relay_url.trim_start_matches("ws://").to_string();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = l.accept().await else {
                return;
            };
            let Ok(server) = TcpStream::connect(&relay_addr).await else {
                return;
            };
            let (mut cr, mut cw) = client.into_split();
            let (mut sr, mut sw) = server.into_split();
            let (b1, b2) = (blackhole.clone(), blackhole.clone());
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = cr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if !b1.load(Ordering::SeqCst) && sw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = sr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if !b2.load(Ordering::SeqCst) && cw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    url
}

#[tokio::test]
async fn a_ping_gets_an_answer() {
    let url = relay_with(Config::default()).await;
    let mut r = Relay::connect(url.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    let rtt = r.ping().await.unwrap();
    assert!(rtt < Duration::from_secs(2), "{rtt:?}");
    // And again on the same connection.
    r.ping().await.unwrap();
}

#[tokio::test]
async fn the_relay_closes_a_connection_that_goes_quiet() {
    let url = relay_with(Config {
        idle_timeout: Duration::from_millis(400),
        ..Config::default()
    })
    .await;
    let mut r = Relay::connect(url.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    // This client does not ping (long interval), so the relay hangs up on it.
    r.set_keepalive(Duration::from_secs(60), Duration::from_secs(30));
    let t = Instant::now();
    let ended = r.next_delivery(Some(Duration::from_secs(5))).await;
    assert!(ended.is_err(), "the relay should have closed it");
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "and quickly: {:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn a_client_that_pings_is_not_closed() {
    let url = relay_with(Config {
        idle_timeout: Duration::from_millis(600),
        ..Config::default()
    })
    .await;
    let mut r = Relay::connect(url.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    r.set_keepalive(Duration::from_millis(150), Duration::from_millis(500));
    // Two seconds is more than three idle timeouts: only the pings keep it open.
    let got = r.next_delivery(Some(Duration::from_secs(2))).await;
    assert!(matches!(got, Ok(None)), "still connected and quiet");
}

#[tokio::test]
async fn a_silently_dead_connection_is_noticed_quickly() {
    let relay = relay_with(Config::default()).await;
    let blackhole = Arc::new(AtomicBool::new(false));
    let via = proxy_to(&relay, blackhole.clone()).await;
    let mut r = Relay::connect(via.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    r.set_keepalive(Duration::from_millis(200), Duration::from_millis(300));

    // Healthy: the pings are answered, so a short wait just reports "nothing arrived".
    assert!(matches!(
        r.next_delivery(Some(Duration::from_millis(700))).await,
        Ok(None)
    ));

    // The path dies without any close or reset. Without a keepalive this would wait for the whole
    // (very long) idle window; with one it is reported as soon as a ping goes unanswered.
    blackhole.store(true, Ordering::SeqCst);
    let t = Instant::now();
    let err = r
        .next_delivery(Some(Duration::from_secs(60)))
        .await
        .expect_err("a dead connection must be an error");
    assert!(err.to_string().contains("lost the connection"), "{err}");
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "noticed in {:?}",
        t.elapsed()
    );
}
