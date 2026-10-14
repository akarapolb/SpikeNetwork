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

    /// This person's signed `spike2:` identity card.
    fn identity_card(&self) -> String {
        self.home.identity_card().unwrap()
    }

    /// Conversation key others file this person under (their owner key).
    fn owner(&self) -> String {
        spike_core::owner::IdentityCard::decode(&self.identity_card())
            .unwrap()
            .owner_hex()
    }

    fn add(&self, name: &str, other: &Device, verified: bool) {
        self.add_card(name, &other.identity_card(), verified);
    }

    /// Add using an older single-device `spike1:` card.
    fn add_legacy(&self, name: &str, other: &Device, verified: bool) {
        self.add_card(name, &other.card().encode(), verified);
    }

    fn add_card(&self, name: &str, card: &str, verified: bool) {
        let mut c = self.home.load_contacts().unwrap();
        c.push(Contact {
            name: name.into(),
            card: card.into(),
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
    assert!(r.new_session());
    let r = ops::send(&alice.home, &url, "bob", b"second")
        .await
        .unwrap();
    assert!(!r.new_session(), "later messages reuse the ratchet");

    // Bob returns and gets both, in order.
    let got = bob.recv(&url).await;
    assert_eq!(got.len(), 2);
    assert_eq!(msg(&got[0]), ("alice", &b"first while you were out"[..]));
    assert_eq!(msg(&got[1]), ("alice", &b"second"[..]));

    // Bob replies over the same session (no prekey needed); Alice receives it.
    let r = ops::send(&bob.home, &url, "alice", b"got both")
        .await
        .unwrap();
    assert!(!r.new_session(), "inbound session is reused for replies");
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

#[tokio::test]
async fn nothing_secret_or_identifying_is_stored_in_the_clear() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = pair("alice", "bob");
    // Have a real conversation so message history exists on both sides.
    ops::publish_keys(&bob.home, &url).await.unwrap();
    ops::send(&alice.home, &url, "bob", b"the vault code is 4417")
        .await
        .unwrap();
    bob.recv(&url).await;
    ops::send(&bob.home, &url, "alice", b"got it, see you at noon")
        .await
        .unwrap();
    alice.recv(&url).await;

    let card = bob.card();
    let card_hex = card.encode();
    let bob_key = hex::encode(card.x25519);
    for d in [&alice, &bob] {
        let dir = d.home.path().to_path_buf();
        let mut sealed = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let bytes = std::fs::read(&path).unwrap();
            if name == "vault.json" {
                continue; // salt + KDF parameters only
            }
            if name.ends_with(".enc") {
                assert_eq!(&bytes[..4], b"SPK1", "{name} is not sealed");
                sealed += 1;
            }
            let text = String::from_utf8_lossy(&bytes);
            for needle in [
                "bob",
                "alice",
                &card_hex[7..39],
                &bob_key[..32],
                "ed25519",
                "verified",
                "vault code",
                "see you at noon",
                "direction",
            ] {
                assert!(!text.contains(needle), "{name} leaks {needle}");
            }
        }
        assert_eq!(
            sealed, 5,
            "identity, sessions, contacts, owner key and device list must all be sealed"
        );
        assert!(dir.join("messages.db").exists(), "history must be stored");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("identity.enc")), 0o600);
            assert_eq!(mode(&dir.join("messages.db")), 0o600);
        }
    }
}

#[tokio::test]
async fn history_records_both_directions_on_each_side() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = pair("alice", "bob");
    ops::publish_keys(&bob.home, &url).await.unwrap();
    let s1 = ops::send(&alice.home, &url, "bob", b"one").await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    ops::send(&alice.home, &url, "bob", b"two").await.unwrap();
    bob.recv(&url).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    ops::send(&bob.home, &url, "alice", b"three").await.unwrap();
    alice.recv(&url).await;

    let view = |d: &Device, peer: &Device| {
        let key = peer.owner();
        d.home
            .messages()
            .unwrap()
            .conversation(&key, 100)
            .unwrap()
            .into_iter()
            .map(|m| (m.direction, m.body))
            .collect::<Vec<_>>()
    };
    use spike_cli::messages::Direction::{In, Out};
    assert_eq!(
        view(&alice, &bob),
        vec![
            (Out, "one".into()),
            (Out, "two".into()),
            (In, "three".into())
        ]
    );
    assert_eq!(
        view(&bob, &alice),
        vec![
            (In, "one".into()),
            (In, "two".into()),
            (Out, "three".into())
        ]
    );
    // The same message has the same id on both devices.
    assert!(bob.home.messages().unwrap().contains(&s1.id).unwrap());
}

#[tokio::test]
async fn crash_between_storing_and_saving_ratchet_neither_loses_nor_doubles() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = pair("alice", "bob");
    ops::publish_keys(&bob.home, &url).await.unwrap();
    ops::send(&alice.home, &url, "bob", b"exactly once")
        .await
        .unwrap();

    // Bob fetches the blob without acking, and snapshots his state files first.
    let me = bob.home.load_identity().unwrap();
    let mut relay = Relay::connect(&url, &me).await.unwrap();
    let d = relay
        .next_delivery(Some(Duration::from_millis(300)))
        .await
        .unwrap()
        .unwrap();
    let dir = bob.home.path().to_path_buf();
    let snap = |n: &str| std::fs::read(dir.join(n)).unwrap();
    let (old_id, old_sess) = (snap("identity.enc"), snap("sessions.enc"));

    let (mut me, mut sessions) = (
        bob.home.load_identity().unwrap(),
        bob.home.load_sessions().unwrap(),
    );
    assert!(matches!(
        spike_cli::process(&bob.home, &mut me, &mut sessions, &d.blob).unwrap(),
        Outcome::Message { .. }
    ));

    // "Crash": the message row was written but the ratchet/identity save is lost.
    std::fs::write(dir.join("identity.enc"), old_id).unwrap();
    std::fs::write(dir.join("sessions.enc"), old_sess).unwrap();

    // Redelivery: decrypts again with the old state, and the store ignores the duplicate.
    let (mut me, mut sessions) = (
        bob.home.load_identity().unwrap(),
        bob.home.load_sessions().unwrap(),
    );
    assert_eq!(
        spike_cli::process(&bob.home, &mut me, &mut sessions, &d.blob).unwrap(),
        Outcome::Duplicate
    );
    let key = alice.owner();
    let stored = bob.home.messages().unwrap().conversation(&key, 10).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].body, "exactly once");
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

fn temp_db(tag: &str) -> std::path::PathBuf {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    let dir = std::env::temp_dir().join(format!("spike-relay-{tag}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("relay.db")
}

/// Start a relay with a database and return a handle that can "crash" it.
async fn start_durable(
    db: &std::path::Path,
    cfg: Config,
) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let cfg = Config {
        db_path: Some(db.to_path_buf()),
        ..cfg
    };
    (url, tokio::spawn(serve(l, cfg)))
}

#[tokio::test]
async fn queue_acks_and_prekeys_survive_relay_restart() {
    let db = temp_db("restart");
    let (alice, bob) = pair("alice", "bob");

    let (url, relay) = start_durable(&db, Config::default()).await;
    assert_eq!(ops::publish_keys(&bob.home, &url).await.unwrap(), 30);
    ops::send(&alice.home, &url, "bob", b"before the crash")
        .await
        .unwrap();
    ops::send(&alice.home, &url, "bob", b"also before")
        .await
        .unwrap();
    relay.abort();
    let _ = relay.await;

    // New process, same database: queued messages and the remaining prekeys are intact.
    let (url, relay) = start_durable(&db, Config::default()).await;
    let got = bob.recv(&url).await;
    assert_eq!(got.len(), 2);
    assert_eq!(msg(&got[0]).1, b"before the crash");
    assert_eq!(msg(&got[1]).1, b"also before");
    relay.abort();
    let _ = relay.await;

    // Acks were durable too: nothing is redelivered after another restart.
    let (url, relay) = start_durable(&db, Config::default()).await;
    assert!(bob.recv(&url).await.is_empty());
    // 30 published, one consumed by Alice's session start, and it stayed consumed.
    assert_eq!(ops::publish_keys(&bob.home, &url).await.unwrap(), 29);
    relay.abort();
    std::fs::remove_dir_all(db.parent().unwrap()).ok();
}

#[tokio::test]
async fn expiry_holds_across_restart() {
    let db = temp_db("ttl");
    let cfg = || Config {
        ttl: Duration::from_millis(400),
        ..Config::default()
    };
    let (alice, bob) = pair("alice", "bob");
    let (url, relay) = start_durable(&db, cfg()).await;
    ops::publish_keys(&bob.home, &url).await.unwrap();
    ops::send(&alice.home, &url, "bob", b"too slow")
        .await
        .unwrap();
    relay.abort();
    let _ = relay.await;

    tokio::time::sleep(Duration::from_millis(600)).await;
    let (url, relay) = start_durable(&db, cfg()).await;
    assert!(
        bob.recv(&url).await.is_empty(),
        "expired message must not survive a restart"
    );
    relay.abort();
    std::fs::remove_dir_all(db.parent().unwrap()).ok();
}

#[tokio::test]
async fn relay_disk_holds_no_plaintext_and_is_private() {
    let db = temp_db("disk");
    let (alice, bob) = pair("alice", "bob");
    let (url, relay) = start_durable(&db, Config::default()).await;
    ops::publish_keys(&bob.home, &url).await.unwrap();
    let marker = b"persistent-marker-xyz-123";
    ops::send(&alice.home, &url, "bob", marker).await.unwrap();
    relay.abort();
    let _ = relay.await;

    let dir = db.parent().unwrap();
    let mut total = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        total += bytes.len();
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker),
            "plaintext found in relay storage"
        );
    }
    assert!(total > 0, "the database should hold the queued ciphertext");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

/// Drain every one-time prekey a device has on the relay, the way an abuser would.
async fn drain_prekeys(url: &str, victim: &str) -> usize {
    let mut taken = 0;
    // Each connection may claim only a few, so an attacker has to keep reconnecting.
    for _ in 0..10 {
        let attacker = Identity::generate().unwrap();
        let mut r = Relay::connect(url, &attacker).await.unwrap();
        loop {
            match r.claim_prekey_info(victim).await {
                Ok(Some((_, false))) => taken += 1,
                Ok(_) => return taken, // got the fallback or nothing: one-time keys are gone
                Err(_) => break,       // rate limited on this connection; reconnect
            }
        }
    }
    taken
}

#[tokio::test]
async fn drained_device_stays_reachable_through_the_fallback_prekey() {
    let url = start_relay(Config::default()).await;
    let alice = Device::new("alice");
    let bob = Device::new("bob");
    let carol = Device::new("carol");
    for (d, other, name) in [
        (&alice, &bob, "bob"),
        (&carol, &bob, "bob"),
        (&bob, &alice, "alice"),
        (&bob, &carol, "carol"),
    ] {
        d.add(name, other, true);
    }
    assert_eq!(ops::publish_keys(&bob.home, &url).await.unwrap(), 30);

    // An abuser takes every one-time prekey. Without a fallback, nobody could contact Bob now.
    let bob_id = bob.home.load_identity().unwrap().device_id();
    assert_eq!(drain_prekeys(&url, &bob_id).await, 30);

    // Two different senders can still start sessions, using the same reusable fallback key.
    let r = ops::send(&alice.home, &url, "bob", b"from alice")
        .await
        .unwrap();
    assert!(r.new_session());
    ops::send(&carol.home, &url, "bob", b"from carol")
        .await
        .unwrap();

    let mut bodies: Vec<_> = bob
        .recv(&url)
        .await
        .iter()
        .map(|o| msg(o).1.to_vec())
        .collect();
    bodies.sort();
    assert_eq!(bodies, vec![b"from alice".to_vec(), b"from carol".to_vec()]);
}

#[tokio::test]
async fn fallback_is_only_used_after_one_time_prekeys_run_out() {
    let url = start_relay(Config::default()).await;
    let bob = Device::new("bob");
    ops::publish_keys(&bob.home, &url).await.unwrap();
    let bob_id = bob.home.load_identity().unwrap().device_id();
    let a = Identity::generate().unwrap();
    let mut r = Relay::connect(&url, &a).await.unwrap();
    let (_, fallback) = r.claim_prekey_info(&bob_id).await.unwrap().unwrap();
    assert!(
        !fallback,
        "a one-time key must be preferred while any remain"
    );
}

#[tokio::test]
async fn fallback_rotates_when_old_and_forgeries_are_refused() {
    let url = start_relay(Config::default()).await;
    let bob = Device::new("bob");
    let mut me = bob.home.load_identity().unwrap();
    let bob_id = me.device_id();

    // First connection uploads a fallback; a prompt second connection leaves it alone.
    ops::publish_keys(&bob.home, &url).await.unwrap();
    let spy = Identity::generate().unwrap();
    let mut s = Relay::connect(&url, &spy).await.unwrap();
    // Drain one-time keys so that claims return the fallback.
    drain_prekeys(&url, &bob_id).await;
    let (first, is_fb) = s.claim_prekey_info(&bob_id).await.unwrap().unwrap();
    assert!(is_fb);
    let (again, _) = s.claim_prekey_info(&bob_id).await.unwrap().unwrap();
    assert_eq!(
        first, again,
        "the fallback is reusable and stable until rotated"
    );

    // Reconnecting with a fresh fallback and the default rotation age leaves it alone.
    let mut r = Relay::connect(&url, &me).await.unwrap();
    ops::top_up_prekeys(&mut r, &bob.home, &mut me)
        .await
        .unwrap();
    drain_prekeys(&url, &bob_id).await;
    let (unchanged, is_fb) = s.claim_prekey_info(&bob_id).await.unwrap().unwrap();
    assert!(is_fb);
    assert_eq!(first, unchanged, "a young fallback must not be rotated");

    // Once it is older than the rotation age, reconnecting publishes a different one.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let mut r = Relay::connect(&url, &me).await.unwrap();
    assert!(r.fallback_age_secs.unwrap() >= 1);
    ops::top_up_prekeys_with(&mut r, &bob.home, &mut me, 0)
        .await
        .unwrap();
    drain_prekeys(&url, &bob_id).await;
    let (rotated, is_fb) = s.claim_prekey_info(&bob_id).await.unwrap().unwrap();
    assert!(is_fb, "after draining, claims must return the fallback");
    assert_ne!(first, rotated, "stale fallback must be replaced");

    // A fallback signed by someone else is refused.
    let mut mallory = Identity::generate().unwrap();
    let e = r
        .publish_fallback(&mallory.fresh_fallback_prekey())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("bad_prekey"), "{e}");
}

#[tokio::test]
async fn legacy_single_device_cards_still_interoperate() {
    let url = start_relay(Config::default()).await;
    let (alice, bob) = (Device::new("alice"), Device::new("bob"));
    // Alice only has Bob's old spike1 card; Bob has Alice's new signed card.
    alice.add_legacy("bob", &bob, true);
    bob.add("alice", &alice, true);
    ops::publish_keys(&bob.home, &url).await.unwrap();
    ops::send(&alice.home, &url, "bob", b"old card, new code")
        .await
        .unwrap();
    let got = bob.recv(&url).await;
    assert_eq!(msg(&got[0]), ("alice", &b"old card, new code"[..]));
    ops::send(&bob.home, &url, "alice", b"works both ways")
        .await
        .unwrap();
    let got = alice.recv(&url).await;
    assert_eq!(msg(&got[0]), ("bob", &b"works both ways"[..]));
}

#[test]
fn contact_updates_follow_the_device_list_rules() {
    use spike_cli::state::Added;
    use spike_core::owner::{DeviceEntry, DeviceList};
    let (alice, bob) = (Device::new("alice"), Device::new("bob"));
    let v1 = bob.identity_card();
    assert_eq!(
        alice.home.add_contact("bob", &v1, false).unwrap(),
        Added::New
    );
    assert_eq!(
        alice.home.add_contact("bob", &v1, false).unwrap(),
        Added::AlreadyKnown
    );

    // Mark verified, then Bob adds a tablet: newer signed list keeps verification.
    let mut c = alice.home.load_contacts().unwrap();
    c[0].verified = true;
    alice.home.save_contacts(&c).unwrap();
    let owner = bob.home.load_owner().unwrap();
    let tablet = Identity::generate().unwrap();
    let v2 = bob
        .home
        .load_device_list()
        .unwrap()
        .with_device(
            &owner,
            DeviceEntry {
                card: tablet.card().encode(),
                relay: None,
            },
        )
        .unwrap();
    bob.home.save_device_list(&v2).unwrap();
    let v2_card = bob.identity_card();
    assert_eq!(
        alice.home.add_contact("bob", &v2_card, false).unwrap(),
        Added::Updated { version: 2 }
    );
    let c = alice.home.load_contacts().unwrap();
    assert!(
        c[0].verified,
        "verifying the person covers devices their identity key signs"
    );
    assert_eq!(c[0].identity().unwrap().devices().len(), 2);

    // Rollback to v1 is refused, even with --force.
    let e = alice
        .home
        .add_contact("bob", &v1, true)
        .unwrap_err()
        .to_string();
    assert!(e.contains("rollback"), "{e}");
    // A different list claiming the same version (a fork) is refused.
    let fork = DeviceList::sign(
        &owner,
        2,
        vec![DeviceEntry {
            card: Identity::generate().unwrap().card().encode(),
            relay: None,
        }],
    )
    .unwrap();
    let e = alice
        .home
        .add_contact("bob", &spike_core::owner::IdentityCard::encode(&fork), true)
        .unwrap_err()
        .to_string();
    assert!(e.contains("equivocation"), "{e}");
    // A different person under the same name needs --force and loses verification.
    let mallory = Device::new("mallory");
    let e = alice
        .home
        .add_contact("bob", &mallory.identity_card(), false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("KEY CHANGE"), "{e}");
    assert_eq!(
        alice
            .home
            .add_contact("bob", &mallory.identity_card(), true)
            .unwrap(),
        Added::Replaced
    );
    assert!(!alice.home.load_contacts().unwrap()[0].verified);
}

#[test]
fn legacy_contact_upgrades_to_signed_identity_and_must_reverify() {
    use spike_cli::state::Added;
    let (alice, bob) = (Device::new("alice"), Device::new("bob"));
    alice.add_legacy("bob", &bob, true);
    // Bob's signed card still contains the device Alice already knew: an upgrade, not a key change.
    assert_eq!(
        alice
            .home
            .add_contact("bob", &bob.identity_card(), false)
            .unwrap(),
        Added::Upgraded
    );
    let c = &alice.home.load_contacts().unwrap()[0];
    assert!(
        !c.verified,
        "the new identity key has not been compared yet"
    );
    assert!(!c.identity().unwrap().is_legacy());
}
