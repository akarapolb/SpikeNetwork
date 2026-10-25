//! TURN credentials from the relay (ADR 0028, phase 3): a signed-in device gets a short-lived login for the
//! operator's TURN server; a relay without one says so.

use spike_cli::client::Relay;
use spike_core::identity::Identity;
use spike_relay::{serve, Config};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

async fn relay_with(cfg: Config) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn a_device_gets_a_login_the_turn_server_accepts_and_it_expires() {
    let url = relay_with(Config {
        turn_urls: vec![
            "turn:turn.example:3478?transport=udp".into(),
            "turns:turn.example:5349".into(),
        ],
        turn_secret: Some("operator secret".into()),
        turn_ttl: Duration::from_secs(600),
        ..Config::default()
    })
    .await;
    let me = Identity::generate().unwrap();
    let mut r = Relay::connect(url.as_str(), &me).await.unwrap();
    let t = r
        .turn_credentials()
        .await
        .unwrap()
        .expect("this relay has a TURN server");
    assert_eq!(
        t.urls,
        [
            "turn:turn.example:3478?transport=udp",
            "turns:turn.example:5349"
        ]
    );
    assert_eq!(t.ttl_secs, 600);
    // What the TURN server does with it, knowing only the shared secret.
    assert!(spike_core::turn::verify(
        b"operator secret",
        &t.username,
        &t.credential,
        now()
    ));
    assert!(!spike_core::turn::verify(
        b"operator secret",
        &t.username,
        &t.credential,
        now() + 601
    ));
    assert!(!spike_core::turn::verify(
        b"someone else's secret",
        &t.username,
        &t.credential,
        now()
    ));
    // The username names the device only by a short label, never its full id.
    assert!(!t.username.contains(&me.device_id()), "{}", t.username);
    assert!(t.username.len() < 30, "{}", t.username);
}

#[tokio::test]
async fn a_relay_without_turn_says_so_and_keeps_working() {
    let url = relay_with(Config::default()).await;
    let mut r = Relay::connect(url.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    assert_eq!(r.turn_credentials().await.unwrap(), None);
    // the same connection still answers
    r.ping().await.unwrap();
}

#[tokio::test]
async fn a_relay_with_urls_but_no_secret_does_not_hand_out_anything() {
    let url = relay_with(Config {
        turn_urls: vec!["turn:x:3478".into()],
        ..Config::default()
    })
    .await;
    let mut r = Relay::connect(url.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    assert_eq!(r.turn_credentials().await.unwrap(), None);
}
