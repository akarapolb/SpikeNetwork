//! End to end: real relay on a loopback port, real clients, real crypto.

use futures_util::{SinkExt, StreamExt};
use spike_cli::{
    client::Relay,
    ops,
    state::{Contact, Home},
    Outcome,
};
use spike_core::vault::KdfParams;
use spike_core::{
    identity::Identity,
    session::Sessions,
    wire::{ClientMsg, ServerMsg},
};
use spike_relay::{serve, Config};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

const PASS: &str = "test passphrase for e2e";

async fn start_relay(cfg: Config) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}

/// A fresh device with its own state directory, paired (verified) with `peers`.
struct Device {
    home: Home,
}

impl Device {
    fn new(tag: &str) -> Self {
        let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
        let dir = std::env::temp_dir().join(format!("spike-test-{tag}-{n}"));
        let home = Home::create(dir, PASS, KdfParams::insecure_for_tests(), false).unwrap();
        Self { home }
    }

    fn card(&self) -> spike_core::identity::DeviceCard {
        self.home.load_identity().unwrap().card()
    }

    fn add(&self, name: &str, other: &Device, verified: bool) {
        let mut c = self.home.load_contacts().unwrap();
        c.push(Contact {
            name: name.into(),
            card: other.card().encode(),
            verified,
        });
        self.home.save_contacts(&c).unwrap();
    }

    async fn recv(&self, url: &str) -> Vec<Outcome> {
        let mut out = vec![];
        ops::recv(&self.home, url, Some(Duration::from_millis(300)), |o| {
            out.push(o)
        })
        .await
        .unwrap();
        out
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.home.path()).ok();
    }
}

fn pair(a_name: &str, b_name: &str) -> (Device, Device) {
    let (a, b) = (Device::new(a_name), Device::new(b_name));
    a.add("bob", &b, true);
    b.add("alice", &a, true);
    (a, b)
}

fn msg(o: &Outcome) -> (&str, &[u8]) {
    match o {
        Outcome::Message { from, body, .. } => (from.as_str(), body.as_slice()),
        o => panic!("expected a message, got {o:?}"),
    }
}

#[tokio::test]
async fn conversation_survives_offline_recipient_and_ratchets_both_ways() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = pair("alice", "bob");

    // Bob registers prekeys with the relay once, then goes offline.
    assert_eq!(ops::publish_keys(&bob.home, &url).await.unwrap(), 30);

    // Alice sends while Bob is offline; the first message starts a session.
    let r = ops::send(&alice.home, &url, "bob", b"first while you were out")
        .await
        .unwrap();
    assert!(r.new_session);
    let r = ops::send(&alice.home, &url, "bob", b"second")
        .await
        .unwrap();
    assert!(!r.new_session, "later messages reuse the ratchet");

    // Bob returns and gets both, in order.
    let got = bob.recv(&url).await;
    assert_eq!(got.len(), 2);
    assert_eq!(msg(&got[0]), ("alice", &b"first while you were out"[..]));
    assert_eq!(msg(&got[1]), ("alice", &b"second"[..]));

    // Bob replies over the same session (no prekey needed); Alice receives it.
    let r = ops::send(&bob.home, &url, "alice", b"got both")
        .await
        .unwrap();
    assert!(!r.new_session, "inbound session is reused for replies");
    let got = alice.recv(&url).await;
    assert_eq!((got.len(), msg(&got[0])), (1, ("bob", &b"got both"[..])));

    // Acked messages do not return.
    assert!(bob.recv(&url).await.is_empty());
    assert!(alice.recv(&url).await.is_empty());
}

#[tokio::test]
async fn cannot_start_session_without_published_prekeys() {
    let url = start_relay(Config::default()).await;
    let (alice, _bob) = pair("alice", "bob");
    let e = ops::send(&alice.home, &url, "bob", b"hi")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("no prekeys"), "{e}");
}

#[tokio::test]
async fn unknown_sender_is_dropped() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = (Device::new("alice"), Device::new("bob"));
    alice.add("bob", &bob, true);
    ops::publish_keys(&bob.home, &url).await.unwrap();
    ops::send(&alice.home, &url, "bob", b"who am i")
        .await
        .unwrap();

    let got = bob.recv(&url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::UnknownSender { .. }]),
        "{got:?}"
    );
}

#[tokio::test]
async fn replayed_ciphertext_from_relay_is_ignored() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = pair("alice", "bob");
    ops::publish_keys(&bob.home, &url).await.unwrap();
    ops::send(&alice.home, &url, "bob", b"once").await.unwrap();

    // Bob receives and processes it; then a malicious relay re-sends the same blob.
    let me = bob.home.load_identity().unwrap();
    let mut relay = Relay::connect(&url, &me).await.unwrap();
    let d = relay
        .next_delivery(Some(Duration::from_millis(300)))
        .await
        .unwrap()
        .unwrap();
    let (mut me, mut sessions) = (
        bob.home.load_identity().unwrap(),
        bob.home.load_sessions().unwrap(),
    );
    let first = spike_cli::process(&bob.home, &mut me, &mut sessions, &d.blob).unwrap();
    assert_eq!(msg(&first), ("alice", &b"once"[..]));
    let replay = spike_cli::process(&bob.home, &mut me, &mut sessions, &d.blob).unwrap();
    assert_eq!(replay, Outcome::Invalid);
    let _: Sessions = bob.home.load_sessions().unwrap();
}

#[tokio::test]
async fn relay_serves_each_prekey_once_and_rejects_forged_ones() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let mut b = Relay::connect(&url, &bob).await.unwrap();
    let mut bob_id = bob;
    b.publish_prekeys(&bob_id.fresh_prekeys(2)).await.unwrap();

    let mut a = Relay::connect(&url, &alice).await.unwrap();
    let id = bob_id.device_id();
    let k1 = a.claim_prekey(&id).await.unwrap().unwrap();
    let k2 = a.claim_prekey(&id).await.unwrap().unwrap();
    assert_ne!(k1, k2);
    assert!(
        a.claim_prekey(&id).await.unwrap().is_none(),
        "prekeys are single-use"
    );

    // A device cannot publish prekeys signed by someone else.
    let mut mallory = Identity::generate().unwrap();
    let forged = mallory.fresh_prekeys(1);
    let e = b.publish_prekeys(&forged).await.unwrap_err().to_string();
    assert!(e.contains("bad_prekey"), "{e}");
}

#[tokio::test]
async fn wrong_key_cannot_authenticate() {
    let url = start_relay(Config::default()).await;
    let (bob, eve) = (Identity::generate().unwrap(), Identity::generate().unwrap());

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
}

#[tokio::test]
async fn relay_enforces_size_ttl_queue_and_claim_limits() {
    let cfg = Config {
        max_blob: 1024,
        queue_cap: 2,
        ttl: Duration::from_millis(400),
        max_claims_per_conn: 2,
        ..Config::default()
    };
    let url = start_relay(cfg).await;
    let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let to = bob.device_id();
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

    // Prekey claims are capped per connection (limits draining someone's prekeys).
    let mut c = Relay::connect(&url, &alice).await.unwrap();
    c.claim_prekey(&to).await.unwrap();
    c.claim_prekey(&to).await.unwrap();
    let e = c.claim_prekey(&to).await.unwrap_err().to_string();
    assert!(e.contains("rate_limited"), "{e}");

    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut b = Relay::connect(&url, &bob).await.unwrap();
    assert!(
        b.next_delivery(Some(Duration::from_millis(300)))
            .await
            .unwrap()
            .is_none(),
        "expired messages must not be delivered"
    );
}

#[test]
fn nothing_secret_or_identifying_is_stored_in_the_clear() {
    let (alice, bob) = pair("alice", "bob");
    let card = bob.card().encode();
    let dir = alice.home.path().to_path_buf();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let bytes = std::fs::read(&path).unwrap();
        if name == "vault.json" {
            continue; // salt + KDF parameters only
        }
        assert_eq!(&bytes[..4], b"SPK1", "{name} is not sealed");
        let text = String::from_utf8_lossy(&bytes);
        for needle in ["bob", "alice", &card[7..39], "ed25519", "verified"] {
            assert!(!text.contains(needle), "{name} leaks {needle}");
        }
        checked += 1;
    }
    assert_eq!(
        checked, 3,
        "identity, sessions and contacts must all be sealed"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("identity.enc")), 0o600);
    }
}

#[test]
fn wrong_passphrase_and_swapped_files_are_refused() {
    let (alice, bob) = pair("alice", "bob");
    let a = alice.home.path().to_path_buf();
    assert!(Home::unlock(&a, "not the passphrase")
        .err()
        .unwrap()
        .to_string()
        .contains("wrong passphrase"));
    assert!(Home::unlock(&a, PASS).is_ok());

    // Replace Alice's contacts with Bob's (sealed under a different key and name): refused.
    let b = bob.home.path().to_path_buf();
    std::fs::copy(b.join("contacts.enc"), a.join("contacts.enc")).unwrap();
    assert!(alice.home.load_contacts().is_err());
    // Even a file copied from another vault under the *same* name and passphrase fails: different salt.
    // And truncation is caught.
    let sessions = std::fs::read(a.join("sessions.enc")).unwrap();
    std::fs::write(a.join("sessions.enc"), &sessions[..sessions.len() - 1]).unwrap();
    assert!(alice.home.load_sessions().is_err());
}

#[test]
fn init_refuses_to_overwrite_existing_state() {
    let (alice, _bob) = pair("alice", "bob");
    let a = alice.home.path().to_path_buf();
    assert!(Home::create(&a, PASS, KdfParams::insecure_for_tests(), false).is_err());
    let before = alice.card();
    let fresh = Home::create(&a, PASS, KdfParams::insecure_for_tests(), true)
        .ok()
        .unwrap();
    assert_ne!(
        fresh.load_identity().unwrap().card(),
        before,
        "--force makes a new identity"
    );
}
