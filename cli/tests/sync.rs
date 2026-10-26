//! Milestone M5 (first slice): edits and deletes merge the same way on every device.

use spike_cli::{
    apply_change,
    link::{self, History},
    messages::{Direction, StoredMessage},
    ops,
    state::{Contact, Home},
    MergeAction, Outcome,
};
use spike_core::envelope::Envelope;
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase sync";
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
        std::env::temp_dir().join(format!("spike-sync-{tag}-{n}")),
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

#[tokio::test]
async fn edit_from_another_device_reaches_everyone_then_delete_removes_it_everywhere() {
    let (url, alice, bob, id) = world().await;
    // Alice edits from her laptop (a device that only has a sent copy).
    ops::edit(&alice[1].0, &url, &id[..8], "meet at 4pm")
        .await
        .unwrap();
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
        let m = get(d, &id);
        assert_eq!(
            (m.body.as_str(), m.edited, m.deleted),
            ("meet at 4pm", true, false),
            "device {}",
            d.0.path().display()
        );
    }
    // Then deletes from her phone.
    ops::delete(&alice[0].0, &url, &id).await.unwrap();
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
        let m = get(d, &id);
        assert!(
            m.deleted && m.body.is_empty() && m.attachment.is_none(),
            "device {}",
            d.0.path().display()
        );
    }
    // A deleted message cannot be edited again.
    assert!(ops::edit(&alice[0].0, &url, &id, "zombie")
        .await
        .unwrap_err()
        .to_string()
        .contains("deleted"));
}

#[tokio::test]
async fn concurrent_edits_from_two_devices_converge_everywhere() {
    let (url, alice, bob, id) = world().await;
    // Both of Alice's devices edit before seeing each other's edit.
    ops::edit(&alice[0].0, &url, &id, "version from phone")
        .await
        .unwrap();
    ops::edit(&alice[1].0, &url, &id, "version from laptop")
        .await
        .unwrap();
    for _ in 0..2 {
        for d in alice.iter().chain(bob.iter()) {
            recv(d, &url).await;
        }
    }
    let texts: Vec<String> = alice
        .iter()
        .chain(bob.iter())
        .map(|d| get(d, &id).body)
        .collect();
    assert!(
        texts.iter().all(|t| *t == texts[0]),
        "all four devices must agree: {texts:?}"
    );
    assert!(texts[0].starts_with("version from"), "{texts:?}");
    // The winner is the edit with the larger (clock, device id) stamp.
    let stamps: Vec<_> = alice
        .iter()
        .chain(bob.iter())
        .map(|d| {
            let m = get(d, &id);
            (m.lamport, m.stamp_device)
        })
        .collect();
    assert!(stamps.iter().all(|s| *s == stamps[0]), "{stamps:?}");
}

#[tokio::test]
async fn delete_beats_a_concurrent_edit_whichever_arrives_first() {
    for delete_first in [false, true] {
        let (url, alice, bob, id) = world().await;
        if delete_first {
            ops::delete(&alice[1].0, &url, &id).await.unwrap();
            ops::edit(&alice[0].0, &url, &id, "late edit")
                .await
                .unwrap();
        } else {
            ops::edit(&alice[0].0, &url, &id, "early edit")
                .await
                .unwrap();
            ops::delete(&alice[1].0, &url, &id).await.unwrap();
        }
        for _ in 0..2 {
            for d in alice.iter().chain(bob.iter()) {
                recv(d, &url).await;
            }
        }
        for d in alice.iter().chain(bob.iter()) {
            assert!(get(d, &id).deleted, "delete_first={delete_first}");
        }
    }
}

#[tokio::test]
async fn only_the_author_can_change_a_message() {
    let (url, alice, bob, id) = world().await;
    // Bob cannot edit or delete Alice's message through the app...
    let e = ops::edit(&bob[0].0, &url, &id, "forged")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("only edit or delete messages you sent"), "{e}");
    assert!(ops::delete(&bob[0].0, &url, &id).await.is_err());
    // ...and a crafted change from Bob (or from Alice's side) is refused by the merge rules.
    let alice_key = owner_of(&alice[0]);
    let mut forged = Envelope::edit(&id, "forged").unwrap();
    forged.lamport = 1_000;
    assert_eq!(
        apply_change(&bob[0].0, &forged, true, &alice_key, "evil").unwrap(),
        MergeAction::NotAuthor,
        "I (Bob) did not write it"
    );
    assert_eq!(
        apply_change(&bob[0].0, &forged, false, &owner_of(&bob[1]), "evil").unwrap(),
        MergeAction::NotAuthor,
        "wrong conversation"
    );
    let del = Envelope::delete(&id).unwrap();
    assert_eq!(
        apply_change(&alice[0].0, &del, false, &owner_of(&bob[0]), "bob-phone").unwrap(),
        MergeAction::NotAuthor,
        "Bob may not delete Alice's message"
    );
    assert_eq!(get(&bob[0], &id).body, "meet at 3pm");
    assert_eq!(get(&alice[0], &id).body, "meet at 3pm");
}

#[tokio::test]
async fn stale_edits_and_replays_do_not_change_anything() {
    let (url, alice, bob, id) = world().await;
    ops::edit(&alice[0].0, &url, &id, "second").await.unwrap();
    recv(&bob[0], &url).await;
    let current = get(&bob[0], &id);
    let peer = owner_of(&alice[0]);
    let mut older = Envelope::edit(&id, "older text").unwrap();
    older.lamport = current.lamport - 1;
    assert_eq!(
        apply_change(&bob[0].0, &older, false, &peer, &current.stamp_device).unwrap(),
        MergeAction::Ignore
    );
    let mut same = Envelope::edit(&id, "replayed").unwrap();
    same.lamport = current.lamport;
    assert_eq!(
        apply_change(&bob[0].0, &same, false, &peer, &current.stamp_device).unwrap(),
        MergeAction::Ignore
    );
    assert_eq!(get(&bob[0], &id).body, "second");
    // An edit for a message we never had is dropped, not invented.
    let unknown = Envelope::edit(&"cd".repeat(16), "who?").unwrap();
    assert_eq!(
        apply_change(&bob[0].0, &unknown, false, &peer, "x").unwrap(),
        MergeAction::DropUnknown
    );
}

#[tokio::test]
async fn a_delete_that_arrives_first_leaves_a_tombstone_so_the_message_never_appears() {
    let (alice, ac) = person("alice", 1);
    let (bob, bc) = person("bob", 1);
    befriend(&alice, "bob", &bc);
    befriend(&bob, "alice", &ac);
    let peer = owner_of(&alice[0]);
    let id = "ef".repeat(16);
    let del = Envelope::delete(&id).unwrap();
    assert_eq!(
        apply_change(&bob[0].0, &del, false, &peer, "a").unwrap(),
        MergeAction::Tombstone
    );
    // Now the original message arrives late: the store ignores it.
    let late = StoredMessage {
        id: id.clone(),
        peer,
        direction: Direction::In,
        ts: 1,
        body: "should stay hidden".into(),
        attachment: None,
        lamport: 1,
        stamp_device: "a".into(),
        edited: false,
        deleted: false,
        changed_at: 0,
        author: None,
        shared_by: None,
        reply_to: None,
        reactions: Default::default(),
    };
    assert!(!bob[0].0.messages().unwrap().insert(&late).unwrap());
    assert!(get(&bob[0], &id).deleted);
}

#[tokio::test]
async fn a_new_device_receives_edits_and_tombstones_through_linking() {
    let (url, alice, bob, id) = world().await;
    let keep = ops::send(&alice[0].0, &url, "bob", b"keep me")
        .await
        .unwrap()
        .id;
    ops::edit(&alice[0].0, &url, &keep, "keep me (fixed)")
        .await
        .unwrap();
    ops::delete(&alice[0].0, &url, &id).await.unwrap();
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
    }
    let tablet = Dev(new_home("alice-tablet"));
    let w = link::offer(&tablet.0, &url).unwrap();
    let new_side = async {
        let s = link::await_hello(&tablet.0, &w, Duration::from_secs(5))
            .await
            .unwrap();
        link::receive(&tablet.0, &w, &s, Duration::from_secs(5))
            .await
            .unwrap();
    };
    let old_side = async {
        let p = link::begin(&alice[0].0, &url, &w.code).await.unwrap();
        link::finish(&alice[0].0, &url, p, History::All)
            .await
            .unwrap();
    };
    tokio::join!(new_side, old_side);
    let (k, d) = (get(&tablet, &keep), get(&tablet, &id));
    assert_eq!((k.body.as_str(), k.edited), ("keep me (fixed)", true));
    assert!(
        d.deleted && d.body.is_empty(),
        "the deleted message arrives as a tombstone, not as text"
    );
    // The tablet then follows later changes like any device.
    ops::edit(&alice[0].0, &url, &keep, "keep me (final)")
        .await
        .unwrap();
    recv(&tablet, &url).await;
    assert_eq!(get(&tablet, &keep).body, "keep me (final)");
}

#[tokio::test]
async fn deleting_removes_the_stored_attachment_file_too() {
    let (url, alice, bob, _) = world().await;
    let id = ops::send_file(&alice[0].0, &url, "bob", "plan.txt", b"private plan", "")
        .await
        .unwrap()
        .id;
    for d in alice.iter().chain(bob.iter()) {
        recv(d, &url).await;
    }
    ops::fetch_attachment(&bob[0].0, &url, &id).await.unwrap();
    assert!(bob[0].0.load_attachment(&id).unwrap().is_some());
    assert!(alice[0].0.load_attachment(&id).unwrap().is_some());
    ops::delete(&alice[0].0, &url, &id).await.unwrap();
    recv(&bob[0], &url).await;
    assert!(
        bob[0].0.load_attachment(&id).unwrap().is_none(),
        "recipient's copy removed"
    );
    assert!(
        alice[0].0.load_attachment(&id).unwrap().is_none(),
        "sender's own copy removed"
    );
    assert!(get(&bob[0], &id).attachment.is_none());
}
