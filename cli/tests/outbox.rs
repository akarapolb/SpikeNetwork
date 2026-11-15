//! ADR 0040: a copy whose relay cannot be reached waits in the outbox and is sent later, once.

use spike_cli::{
    ops,
    outbox::{self, State},
    state::{Contact, Home},
    Outcome,
};
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase outbox";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

/// A relay that can be stopped and started again on the same address with the same database.
struct Relay {
    addr: SocketAddr,
    db: PathBuf,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Relay {
    async fn new(tag: &str) -> Self {
        let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
        let dir = std::env::temp_dir().join(format!("spike-outbox-relay-{tag}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let mut r = Self {
            addr,
            db: dir.join("relay.db"),
            task: None,
        };
        r.serve(l);
        r
    }
    fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }
    fn serve(&mut self, l: tokio::net::TcpListener) {
        let cfg = Config {
            db_path: Some(self.db.clone()),
            ..Config::default()
        };
        self.task = Some(tokio::spawn(serve(l, cfg)));
    }
    async fn stop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
            let _ = t.await;
        }
    }
    async fn start(&mut self) {
        let mut tries = 0;
        let l = loop {
            match tokio::net::TcpListener::bind(self.addr).await {
                Ok(l) => break l,
                Err(e) if tries < 50 => {
                    tries += 1;
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => panic!("could not listen on {} again: {e}", self.addr),
            }
        };
        self.serve(l);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
        std::fs::remove_dir_all(self.db.parent().unwrap()).ok();
    }
}

struct Dev(Home);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.path()).ok();
    }
}

fn new_home(tag: &str) -> Home {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    Home::create(
        std::env::temp_dir().join(format!("spike-outbox-{tag}-{n}")),
        PASS,
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap()
}

/// A person with one device per entry of `relays` (each device's home relay; None = the default).
/// Returns the devices and the person's current card (with every device and relay).
fn person(tag: &str, relays: &[Option<&str>]) -> (Vec<Dev>, String) {
    let devs: Vec<Dev> = (0..relays.len())
        .map(|i| Dev(new_home(&format!("{tag}{i}"))))
        .collect();
    let owner = devs[0].0.load_owner().unwrap();
    let entries = devs
        .iter()
        .zip(relays)
        .map(|(d, r)| DeviceEntry {
            card: d.0.load_identity().unwrap().card().encode(),
            relay: r.map(String::from),
        })
        .collect();
    let list = DeviceList::sign(&owner, 2, entries).unwrap();
    for d in &devs {
        d.0.install_identity(&owner, &list).unwrap();
    }
    let card = devs[0].0.identity_card().unwrap();
    (devs, card)
}

fn befriend(devs: &[Dev], name: &str, card: &str) {
    for d in devs {
        let mut c = d.0.load_contacts().unwrap();
        c.push(Contact {
            name: name.into(),
            card: card.into(),
            verified: true,
            key_changed: None,
            verified_at: None,
            devices_changed: None,
        });
        d.0.save_contacts(&c).unwrap();
    }
}

async fn recv(d: &Dev, url: &str) -> Vec<String> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out.iter()
        .filter_map(|o| match o {
            Outcome::Message { from, body, .. } => {
                Some(format!("from {from}: {}", String::from_utf8_lossy(body)))
            }
            Outcome::SentCopy { to, body, .. } => {
                Some(format!("copy to {to}: {}", String::from_utf8_lossy(body)))
            }
            _ => None,
        })
        .collect()
}

fn owner_of(d: &Dev) -> String {
    IdentityCard::decode(&d.0.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}

fn state(d: &Dev, id: &str) -> State {
    outbox::states(&d.0).unwrap().of(id)
}

fn now() -> u64 {
    spike_core::clock::now_ms()
}

#[tokio::test]
async fn a_message_sent_while_the_relay_is_down_arrives_once_when_it_is_back() {
    let mut relay = Relay::new("same").await;
    let url = relay.url();
    let (alice, alice_card) = person("alice", &[None, None]);
    let (bob, bob_card) = person("bob", &[None]);
    befriend(&alice, "bob", &bob_card);
    befriend(&bob, "alice", &alice_card);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    ops::send(&alice[0].0, &url, "bob", b"hello").await.unwrap();
    assert_eq!(recv(&bob[0], &url).await, ["from alice: hello"]);
    assert_eq!(recv(&alice[1], &url).await, ["copy to bob: hello"]);

    relay.stop().await;
    // The relay is gone: the send still succeeds, kept for later and shown as waiting.
    let r = ops::send(&alice[0].0, &url, "bob", b"while you were down")
        .await
        .unwrap();
    assert_eq!(r.delivered(), 0, "{r:?}");
    assert!(r.waiting() && r.relay_unreachable(), "{r:?}");
    assert!(r.devices.iter().all(|d| d.queued), "{r:?}");
    assert!(alice[0].0.messages().unwrap().contains(&r.id).unwrap());
    assert_eq!(state(&alice[0], &r.id), State::Waiting);
    assert_eq!(
        outbox::entries(&alice[0].0).unwrap().len(),
        2,
        "Bob's and Alice's laptop's copies"
    );

    // A sync while it is still down keeps it.
    assert!(ops::recv(&alice[0].0, &url, IDLE, |_| {}).await.is_err());
    assert_eq!(state(&alice[0], &r.id), State::Waiting);

    relay.start().await;
    // Back: the next sync sends both copies (the backoff from the failed retry has passed).
    let f = outbox::flush_at(&alice[0].0, &url, now() + 10 * 60_000)
        .await
        .unwrap();
    assert_eq!(f.delivered, 2, "{f:?}");
    assert!(outbox::entries(&alice[0].0).unwrap().is_empty());
    assert_eq!(state(&alice[0], &r.id), State::Sent);

    assert_eq!(
        recv(&bob[0], &url).await,
        ["from alice: while you were down"]
    );
    assert_eq!(
        recv(&alice[1], &url).await,
        ["copy to bob: while you were down"]
    );
    // Exactly once: nothing more is sent or received, and Bob holds each message once.
    assert_eq!(outbox::flush(&alice[0].0, &url).await.unwrap().delivered, 0);
    assert!(recv(&bob[0], &url).await.is_empty());
    let conv = bob[0]
        .0
        .messages()
        .unwrap()
        .conversation(&owner_of(&alice[0]), 10)
        .unwrap();
    assert_eq!(conv.len(), 2);
    assert_eq!(conv.iter().filter(|m| m.id == r.id).count(), 1);
}

#[tokio::test]
async fn a_sync_sends_what_waits_once_the_relay_is_back() {
    let mut relay = Relay::new("sync").await;
    let url = relay.url();
    let (alice, alice_card) = person("alice", &[None]);
    let (bob, bob_card) = person("bob", &[None]);
    befriend(&alice, "bob", &bob_card);
    befriend(&bob, "alice", &alice_card);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    relay.stop().await;
    // The very first message: the session cannot even be started (no prekey without the relay).
    let r = ops::send(&alice[0].0, &url, "bob", b"first ever")
        .await
        .unwrap();
    assert!(r.waiting(), "{r:?}");
    relay.start().await;
    // An ordinary sync, with nothing tried in between, sends it.
    assert!(recv(&alice[0], &url).await.is_empty());
    assert_eq!(state(&alice[0], &r.id), State::Sent);
    assert_eq!(recv(&bob[0], &url).await, ["from alice: first ever"]);
}

#[tokio::test]
async fn a_contact_on_another_relay_that_is_down_gets_it_later_in_order() {
    let home_relay = Relay::new("home").await;
    let mut theirs = Relay::new("theirs").await;
    let (a, b) = (home_relay.url(), theirs.url());
    let (alice, alice_card) = person("alice", &[None]);
    let (bob, bob_card) = person("bob", &[Some(&b)]);
    befriend(&alice, "bob", &bob_card);
    befriend(&bob, "alice", &alice_card);
    ops::publish_keys(&alice[0].0, &a).await.unwrap();
    ops::publish_keys(&bob[0].0, &b).await.unwrap();
    ops::send(&alice[0].0, &a, "bob", b"one").await.unwrap();
    assert_eq!(recv(&bob[0], &b).await, ["from alice: one"]);

    theirs.stop().await;
    let r = ops::send(&alice[0].0, &a, "bob", b"two").await.unwrap();
    assert!(r.waiting() && r.relay_unreachable(), "{r:?}");
    // Alice's own relay is fine; her sync tries Bob's relay and keeps the copy.
    assert!(recv(&alice[0], &a).await.is_empty());
    assert_eq!(state(&alice[0], &r.id), State::Waiting);
    let e = outbox::entries(&alice[0].0).unwrap();
    assert_eq!((e.len(), e[0].attempts), (1, 2), "{e:?}");

    theirs.start().await;
    // The next message to Bob sends the waiting one first, whatever its backoff.
    let r3 = ops::send(&alice[0].0, &a, "bob", b"three").await.unwrap();
    assert_eq!(r3.delivered(), 1, "{r3:?}");
    assert_eq!(state(&alice[0], &r.id), State::Sent);
    assert_eq!(
        recv(&bob[0], &b).await,
        ["from alice: two", "from alice: three"]
    );
    assert!(recv(&bob[0], &b).await.is_empty());
}

#[tokio::test]
async fn a_copy_is_given_up_after_the_expiry_and_recorded() {
    let mut relay = Relay::new("expiry").await;
    let url = relay.url();
    let (alice, alice_card) = person("alice", &[None]);
    let (bob, bob_card) = person("bob", &[None]);
    befriend(&alice, "bob", &bob_card);
    befriend(&bob, "alice", &alice_card);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    ops::send(&alice[0].0, &url, "bob", b"hello").await.unwrap();
    relay.stop().await;
    let r = ops::send(&alice[0].0, &url, "bob", b"too late")
        .await
        .unwrap();
    assert_eq!(state(&alice[0], &r.id), State::Waiting);

    relay.start().await;
    let f = outbox::flush_at(&alice[0].0, &url, now() + outbox::EXPIRY_MS + 1)
        .await
        .unwrap();
    assert_eq!((f.delivered, f.dropped), (0, 1), "{f:?}");
    assert!(outbox::entries(&alice[0].0).unwrap().is_empty());
    assert!(outbox::given_up(&alice[0].0).unwrap().contains_key(&r.id));
    assert_eq!(state(&alice[0], &r.id), State::NotSent);
    // It stays in Alice's history, and never reaches Bob.
    assert!(alice[0].0.messages().unwrap().contains(&r.id).unwrap());
    assert_eq!(recv(&bob[0], &url).await, ["from alice: hello"]);
}

#[tokio::test]
async fn a_note_to_your_own_devices_waits_too() {
    let mut relay = Relay::new("own").await;
    let url = relay.url();
    let (alice, _) = person("alice", &[None, None]);
    for d in &alice {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    relay.stop().await;
    let (id, ok) = ops::send_to_own(&alice[0].0, &url, "note while down", None)
        .await
        .unwrap();
    assert_eq!(ok, 0);
    assert_eq!(state(&alice[0], &id), State::Waiting);
    relay.start().await;
    assert!(recv(&alice[0], &url).await.is_empty());
    assert_eq!(state(&alice[0], &id), State::Sent);
    recv(&alice[1], &url).await;
    assert!(alice[1].0.messages().unwrap().contains(&id).unwrap());
}
