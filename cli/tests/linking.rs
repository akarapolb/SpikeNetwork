//! Milestone M4: linking a new device to an existing identity.

use spike_cli::{
    client::Relay,
    link::{self, History},
    messages::{Direction, StoredMessage},
    ops,
    state::{Contact, Home},
    Outcome,
};
use spike_core::identity::Identity;
use spike_core::link::LinkSession;
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase linking";
const T: Duration = Duration::from_secs(5);
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

async fn relay_with(db: Option<PathBuf>) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(
        l,
        Config {
            db_path: db,
            ..Config::default()
        },
    ));
    url
}

struct Dev(Home);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.path()).ok();
    }
}
fn dev(tag: &str) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    Dev(Home::create(
        std::env::temp_dir().join(format!("spike-link-{tag}-{n}")),
        PASS,
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap())
}
fn owner(d: &Dev) -> String {
    IdentityCard::decode(&d.0.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}
fn befriend(a: &Dev, name: &str, b: &Dev) {
    let mut c = a.0.load_contacts().unwrap();
    c.push(Contact {
        name: name.into(),
        card: b.0.identity_card().unwrap(),
        verified: true,
    });
    a.0.save_contacts(&c).unwrap();
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}

/// Run both sides of a link, as two people at two screens who confirm matching codes.
async fn link(
    existing: &Dev,
    new: &Dev,
    url: &str,
    mode: History,
) -> (link::Linked, link::Installed) {
    let w = link::offer(&new.0, url).unwrap();
    let new_side = async {
        let s = link::await_hello(&new.0, &w, T).await.unwrap();
        let code = s.safety_code.clone();
        (code, link::receive(&new.0, &w, &s, T).await.unwrap())
    };
    let existing_side = async {
        let p = link::begin(&existing.0, url, &w.code).await.unwrap();
        let code = p.safety_code.clone();
        // A person checks the code on both screens before confirming; asserted below.
        (code, link::finish(&existing.0, url, p, mode).await.unwrap())
    };
    let ((new_code, installed), (old_code, linked)) = tokio::join!(new_side, existing_side);
    assert_eq!(new_code, old_code, "both screens must show the same code");
    (linked, installed)
}

/// Alice (one device) and Bob, with some history between them.
async fn alice_and_bob(url: &str) -> (Dev, Dev) {
    let (a, b) = (dev("alice"), dev("bob"));
    befriend(&a, "bob", &b);
    befriend(&b, "alice", &a);
    ops::publish_keys(&a.0, url).await.unwrap();
    ops::publish_keys(&b.0, url).await.unwrap();
    ops::send(&a.0, url, "bob", b"before the laptop existed")
        .await
        .unwrap();
    recv(&b, url).await;
    ops::send(&b.0, url, "alice", b"reply from bob")
        .await
        .unwrap();
    recv(&a, url).await;
    (a, b)
}

#[tokio::test]
async fn linked_device_joins_the_identity_with_contacts_and_history() {
    let url = relay_with(None).await;
    let (a1, bob) = alice_and_bob(&url).await;
    let a2 = dev("alice-laptop");
    let (linked, installed) = link(&a1, &a2, &url, History::All).await;
    assert_eq!((linked.version, installed.version), (2, 2));
    assert_eq!((installed.contacts, installed.messages), (1, 2));

    // Same person now: same owner key and fingerprint, two devices, contact still verified.
    assert_eq!(owner(&a1), owner(&a2));
    assert_eq!(
        IdentityCard::decode(&a2.0.identity_card().unwrap())
            .unwrap()
            .devices()
            .len(),
        2
    );
    let c = &a2.0.load_contacts().unwrap()[0];
    assert!(c.name == "bob" && c.verified);
    let hist =
        a2.0.messages()
            .unwrap()
            .conversation(&owner(&bob), 10)
            .unwrap();
    assert_eq!(
        hist.iter()
            .map(|m| (m.direction, m.body.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (Direction::Out, "before the laptop existed"),
            (Direction::In, "reply from bob")
        ]
    );

    // And it works as a device: Bob reaches both, and the laptop can write.
    ops::send(&bob.0, &url, "alice", b"to all of alice")
        .await
        .unwrap();
    for d in [&a1, &a2] {
        assert!(
            matches!(recv(d, &url).await.as_slice(), [Outcome::Message { body, .. }] if body == b"to all of alice")
        );
    }
    ops::send(&a2.0, &url, "bob", b"hi from the laptop")
        .await
        .unwrap();
    assert!(
        matches!(recv(&bob, &url).await.as_slice(), [Outcome::Message { from, .. }] if from == "alice")
    );
    assert!(
        matches!(recv(&a1, &url).await.as_slice(), [Outcome::SentCopy { to, .. }] if to == "bob")
    );
}

#[tokio::test]
async fn history_choice_is_honoured() {
    let url = relay_with(None).await;
    let (a1, bob) = alice_and_bob(&url).await;
    // Plant an old message (90 days ago) in Alice's store.
    let old_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        - 90 * 86_400_000;
    a1.0.messages()
        .unwrap()
        .insert(&StoredMessage {
            attachment: None,
            lamport: 0,
            stamp_device: String::new(),
            edited: false,
            deleted: false,
            changed_at: 0,
            author: None,
            id: "aa".repeat(16),
            peer: owner(&bob),
            direction: Direction::In,
            ts: old_ts,
            body: "ancient".into(),
        })
        .unwrap();

    for (mode, want) in [
        (History::All, 3),
        (History::Recent { days: 30 }, 2),
        (History::None, 0),
    ] {
        let d = dev("laptop");
        let (_, installed) = link(&a1, &d, &url, mode).await;
        assert_eq!(installed.messages, want, "{mode:?}");
        let has_ancient =
            d.0.messages()
                .unwrap()
                .all()
                .unwrap()
                .iter()
                .any(|m| m.body == "ancient");
        assert_eq!(has_ancient, mode == History::All, "{mode:?}");
        assert_eq!(installed.contacts, 1, "contacts always come along");
    }
}

#[tokio::test]
async fn substituted_hello_shows_a_different_code_and_the_bundle_does_not_open() {
    let url = relay_with(None).await;
    let (a1, _bob) = alice_and_bob(&url).await;
    let a2 = dev("alice-laptop");
    let w = link::offer(&a2.0, &url).unwrap();

    // An attacker who somehow saw the code races a hello of its own to the new device.
    let attacker = Identity::generate().unwrap();
    let offer = spike_core::link::LinkOffer::decode(&w.code).unwrap();
    let (_evil_session, evil_hello) = LinkSession::approve(&offer).unwrap();
    let mut r = Relay::connect(&url, &attacker).await.unwrap();
    r.send(&offer.device_card().unwrap().device_id(), &evil_hello)
        .await
        .unwrap();

    let new_session = link::await_hello(&a2.0, &w, T).await.unwrap();
    let p = link::begin(&a1.0, &url, &w.code).await.unwrap();
    assert_ne!(
        new_session.safety_code, p.safety_code,
        "the person sees different codes and stops"
    );

    // Even if the person ignored the mismatch, Alice's real bundle cannot be opened.
    link::finish(&a1.0, &url, p, History::All).await.unwrap();
    let e = link::receive(&a2.0, &w, &new_session, Duration::from_millis(800))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("timed out"), "{e}");
    assert_ne!(owner(&a2), owner(&a1), "nothing was installed");
}

#[tokio::test]
async fn an_already_linked_device_cannot_be_linked_again() {
    let url = relay_with(None).await;
    let (a1, _bob) = alice_and_bob(&url).await;
    let a2 = dev("alice-laptop");
    link(&a1, &a2, &url, History::None).await;
    let w = link::offer(&a2.0, &url).unwrap();
    let e = link::begin(&a1.0, &url, &w.code)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("already linked"), "{e}");
}

#[tokio::test]
async fn relay_never_sees_the_identity_key_contacts_or_history() {
    let dir = std::env::temp_dir().join(format!(
        "spike-link-relaydb-{}",
        hex::encode(&Identity::generate().unwrap().ed_public()[..6])
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let url = relay_with(Some(dir.join("relay.db"))).await;
    let (a1, _bob) = alice_and_bob(&url).await;
    let secret_owner = a1.0.load_owner().unwrap().to_hex();
    let a2 = dev("alice-laptop");
    let w = link::offer(&a2.0, &url).unwrap();
    // Bundle queued at the relay but not yet collected: inspect the relay's disk.
    // Run the new side's hello wait concurrently so the hello is consumed;
    // the bundle frames then stay queued at the relay for inspection.
    let (hello, p) = tokio::join!(
        link::await_hello(&a2.0, &w, T),
        link::begin(&a1.0, &url, &w.code)
    );
    hello.unwrap();
    let p = p.unwrap();
    link::finish(&a1.0, &url, p, History::All).await.unwrap();
    for e in std::fs::read_dir(&dir).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for needle in [
            secret_owner.as_str(),
            "before the laptop existed",
            "reply from bob",
            "\"bob\"",
        ] {
            assert!(!text.contains(needle), "relay storage leaks {needle}");
        }
    }
    std::fs::remove_dir_all(dir).ok();
}
