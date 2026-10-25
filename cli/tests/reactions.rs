//! Replies and reactions reach every device of both people and settle the same way everywhere.

use spike_cli::{
    messages::StoredMessage,
    ops,
    state::{Contact, Home},
    Outcome,
};
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase react";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

async fn relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
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
        std::env::temp_dir().join(format!("spike-react-{tag}-{n}")),
        PASS,
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap()
}
/// A person with `n` devices under one owner key, all on the default relay.
fn person(tag: &str, n: usize) -> (Vec<Dev>, String) {
    let devs: Vec<Dev> = (0..n)
        .map(|i| Dev(new_home(&format!("{tag}{i}"))))
        .collect();
    let owner = devs[0].0.load_owner().unwrap();
    let entries = devs
        .iter()
        .map(|d| DeviceEntry {
            card: d.0.load_identity().unwrap().card().encode(),
            relay: None,
        })
        .collect();
    let list = DeviceList::sign(&owner, 2, entries).unwrap();
    for d in &devs {
        d.0.install_identity(&owner, &list).unwrap();
    }
    let card = IdentityCard::encode(&list);
    (devs, card)
}
fn befriend(devs: &[Dev], name: &str, card: &str) {
    for d in devs {
        let mut c = d.0.load_contacts().unwrap();
        c.push(Contact {
            name: name.into(),
            card: card.into(),
            verified: true,
        });
        d.0.save_contacts(&c).unwrap();
    }
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}
fn owner_of(d: &Dev) -> String {
    IdentityCard::decode(&d.0.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}
fn get(d: &Dev, id: &str) -> StoredMessage {
    d.0.messages()
        .unwrap()
        .get(id)
        .unwrap()
        .unwrap_or_else(|| panic!("message {id} missing"))
}
/// Alice (2 devices) and Bob (2 devices), everyone connected, plus one message from Alice's phone
/// received everywhere.
async fn world() -> (String, Vec<Dev>, Vec<Dev>, String) {
    let url = relay().await;
    let (alice, ac) = person("alice", 2);
    let (bob, bc) = person("bob", 2);
    befriend(&alice, "bob", &bc);
    befriend(&bob, "alice", &ac);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    let id = ops::send(&alice[0].0, &url, "bob", b"meet at 3pm")
        .await
        .unwrap()
        .id;
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
    }
    (url, alice, bob, id)
}

fn emoji_of(d: &Dev, id: &str, who: &str) -> Option<String> {
    get(d, id).reactions.get(who).map(|r| r.emoji.clone())
}

#[tokio::test]
async fn a_reply_reaches_every_device_with_what_it_replies_to() {
    let (url, alice, bob, id) = world().await;
    let reply = ops::send_reply(&bob[1].0, &url, "alice", &id, "3pm works")
        .await
        .unwrap()
        .id;
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
        let m = get(d, &reply);
        assert_eq!(
            (m.body.as_str(), m.reply_to.as_deref()),
            ("3pm works", Some(id.as_str())),
            "device {}",
            d.0.path().display()
        );
    }
}

#[tokio::test]
async fn reactions_reach_everyone_change_and_can_be_taken_back() {
    let (url, alice, bob, id) = world().await;
    let b = owner_of(&bob[0]);
    // Bob reacts from his phone: all four devices see it, under Bob.
    ops::react(&bob[0].0, &url, &id, "👍").await.unwrap();
    let mut saw = false;
    for d in alice.iter().chain(bob.iter()) {
        for o in recv(d, &url).await {
            saw |= matches!(o, Outcome::Reacted { ref emoji, .. } if emoji == "👍");
        }
        assert_eq!(
            emoji_of(d, &id, &b).as_deref(),
            Some("👍"),
            "device {}",
            d.0.path().display()
        );
    }
    assert!(saw, "someone was told about the reaction");
    // He changes it from his laptop: one reaction per person, the newest wins everywhere.
    ops::react(&bob[1].0, &url, &id, "🎉").await.unwrap();
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
        let m = get(d, &id);
        assert_eq!(m.reactions.len(), 1, "one reaction per person");
        assert_eq!(emoji_of(d, &id, &b).as_deref(), Some("🎉"));
    }
    // Alice reacts to her own message too; then Bob takes his back.
    ops::react(&alice[1].0, &url, &id, "❤️").await.unwrap();
    ops::react(&bob[0].0, &url, &id, "").await.unwrap();
    let a = owner_of(&alice[0]);
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
        recv(d, &url).await;
        assert_eq!(emoji_of(d, &id, &a).as_deref(), Some("❤️"));
        assert_eq!(
            emoji_of(d, &id, &b).as_deref(),
            Some(""),
            "taken back, but remembered"
        );
    }
}

#[tokio::test]
async fn reactions_to_deleted_or_unknown_messages_change_nothing() {
    let (url, alice, bob, id) = world().await;
    ops::delete(&alice[0].0, &url, &id).await.unwrap();
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
    }
    assert!(
        ops::react(&bob[0].0, &url, &id, "👍").await.is_err(),
        "no reacting to a deleted message"
    );
    // A hand-made reaction to a message nobody has is valid but changes nothing.
    let env = spike_core::envelope::Envelope::react(&"ab".repeat(16), "👍").unwrap();
    ops::deliver_envelope(&bob[0].0, &url, "alice", env)
        .await
        .unwrap();
    for o in recv(&alice[0], &url).await {
        assert!(!matches!(o, Outcome::Reacted { .. }), "{o:?}");
    }
}
