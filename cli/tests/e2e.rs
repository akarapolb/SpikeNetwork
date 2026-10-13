//! End to end: real relay on a loopback port, real clients, real crypto.

use futures_util::{SinkExt, StreamExt};
use spike_cli::{
    client::Relay,
    process,
    state::{Contact, Home},
    Outcome,
};
use spike_core::{
    identity::Identity,
    session,
    wire::{ClientMsg, ServerMsg},
};
use spike_relay::{serve, Config};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

async fn start_relay(cfg: Config) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}

fn temp_home(tag: &str) -> Home {
    let p = std::env::temp_dir().join(format!("spike-test-{tag}-{}", hex::encode(rand_bytes())));
    Home::open(p)
}

fn rand_bytes() -> [u8; 8] {
    Identity::generate().unwrap().ed_public()[..8]
        .try_into()
        .unwrap()
}

const IDLE: Option<Duration> = Some(Duration::from_millis(300));

#[tokio::test]
async fn offline_recipient_gets_message_later_and_ack_clears_queue() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());

    // Alice sends while Bob is offline, then disconnects.
    let sealed = session::seal(&alice, &bob.card(), b"hello while you were out").unwrap();
    {
        let mut a = Relay::connect(&url, &alice).await.unwrap();
        a.send(&bob.card().device_id(), &sealed).await.unwrap();
    }

    // Bob comes online later and receives it.
    let mut b = Relay::connect(&url, &bob).await.unwrap();
    let d = b
        .next_delivery(IDLE)
        .await
        .unwrap()
        .expect("queued message delivered");
    assert_eq!(
        d.blob, sealed,
        "relay returns exactly the ciphertext it was given"
    );
    let opened = session::open(&bob, &d.blob).unwrap();
    assert_eq!(opened.body, b"hello while you were out");
    assert_eq!(opened.sender_x25519, alice.x_public());

    // Without an ack the relay redelivers on reconnect.
    drop(b);
    let mut b = Relay::connect(&url, &bob).await.unwrap();
    let again = b
        .next_delivery(IDLE)
        .await
        .unwrap()
        .expect("unacked message redelivered");
    b.ack(&again.id).await.unwrap();
    // Ack is fire-and-forget; reconnect and confirm the queue is empty.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(b);
    let mut b = Relay::connect(&url, &bob).await.unwrap();
    assert!(
        b.next_delivery(IDLE).await.unwrap().is_none(),
        "acked message must not return"
    );
}

#[tokio::test]
async fn online_recipient_gets_live_delivery() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let mut b = Relay::connect(&url, &bob).await.unwrap();
    let mut a = Relay::connect(&url, &alice).await.unwrap();
    let sealed = session::seal(&alice, &bob.card(), b"live").unwrap();
    a.send(&bob.card().device_id(), &sealed).await.unwrap();
    let d = b
        .next_delivery(Some(Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session::open(&bob, &d.blob).unwrap().body, b"live");
}

#[tokio::test]
async fn wrong_key_cannot_authenticate_or_read_someone_elses_queue() {
    let url = start_relay(Config::default()).await;
    let (alice, bob, eve) = (
        Identity::generate().unwrap(),
        Identity::generate().unwrap(),
        Identity::generate().unwrap(),
    );
    let mut a = Relay::connect(&url, &alice).await.unwrap();
    a.send(
        &bob.card().device_id(),
        &session::seal(&alice, &bob.card(), b"secret").unwrap(),
    )
    .await
    .unwrap();

    // Eve claims Bob's key but signs with her own.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let Message::Text(t) = ws.next().await.unwrap().unwrap() else {
        panic!()
    };
    let ServerMsg::Challenge { nonce } = serde_json::from_str(&t).unwrap() else {
        panic!()
    };
    let sig = eve.sign_auth(&hex::decode(nonce).unwrap());
    let auth = ClientMsg::Auth {
        ed25519: hex::encode(bob.ed_public()),
        sig: hex::encode(sig),
    };
    ws.send(Message::Text(serde_json::to_string(&auth).unwrap()))
        .await
        .unwrap();
    let Message::Text(t) = ws.next().await.unwrap().unwrap() else {
        panic!()
    };
    assert!(matches!(
        serde_json::from_str::<ServerMsg>(&t).unwrap(),
        ServerMsg::Error { .. }
    ));

    // Eve authenticating as herself sees nothing and cannot decrypt Bob's mail anyway.
    let mut e = Relay::connect(&url, &eve).await.unwrap();
    assert!(e.next_delivery(IDLE).await.unwrap().is_none());
}

#[tokio::test]
async fn relay_enforces_size_ttl_and_queue_limits() {
    let cfg = Config {
        max_blob: 1024,
        queue_cap: 2,
        ttl: Duration::from_millis(400),
        ..Config::default()
    };
    let url = start_relay(cfg).await;
    let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let to = bob.card().device_id();
    let mut a = Relay::connect(&url, &alice).await.unwrap();

    // Slightly over the limit: refused with an error, connection stays usable.
    let e = a.send(&to, &vec![0u8; 1100]).await.unwrap_err().to_string();
    assert!(e.contains("too_large"), "{e}");

    a.send(&to, b"one").await.unwrap();
    a.send(&to, b"two").await.unwrap();
    let e = a.send(&to, b"three").await.unwrap_err().to_string();
    assert!(e.contains("queue_full"), "{e}");

    // Far over the limit: the frame cap drops the connection outright.
    let mut huge = Relay::connect(&url, &alice).await.unwrap();
    assert!(huge.send(&to, &vec![0u8; 100_000]).await.is_err());

    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut b = Relay::connect(&url, &bob).await.unwrap();
    assert!(
        b.next_delivery(IDLE).await.unwrap().is_none(),
        "expired messages must not be delivered"
    );
}

#[test]
fn process_handles_replay_unknown_sender_and_garbage() {
    let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let home = temp_home("process");
    let sealed = session::seal(&alice, &bob.card(), b"hi").unwrap();

    // Unknown sender: authentic but not a contact -> dropped, not recorded as seen.
    assert!(matches!(
        process(&home, &bob, &sealed).unwrap(),
        Outcome::UnknownSender { .. }
    ));

    home.save_contacts(&[Contact {
        name: "alice".into(),
        card: alice.card().encode(),
        verified: false,
    }])
    .unwrap();
    match process(&home, &bob, &sealed).unwrap() {
        Outcome::Message {
            from,
            verified,
            body,
            ..
        } => {
            assert_eq!(
                (from.as_str(), verified, body.as_slice()),
                ("alice", false, &b"hi"[..])
            );
        }
        o => panic!("{o:?}"),
    }
    // The same ciphertext replayed by a malicious relay is ignored.
    assert_eq!(process(&home, &bob, &sealed).unwrap(), Outcome::Duplicate);
    // Garbage.
    assert_eq!(
        process(&home, &bob, b"not a noise message").unwrap(),
        Outcome::Invalid
    );
    std::fs::remove_dir_all(home.path()).ok();
}
