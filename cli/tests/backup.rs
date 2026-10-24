//! Backup and recovery of an identity (ADR 0029): lose the only device, restore from the file, and
//! the person is back, with their contacts, and without a single piece of the lost device's secrets.

use spike_cli::{
    backup::{self, Opened},
    link,
    messages::Direction,
    ops,
    state::{Contact, Home},
    transport::Target,
    Outcome,
};
use spike_core::identity::Identity;
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PASS: &str = "device passphrase one";
const NEW_PASS: &str = "device passphrase two";
const BACKUP_PASS: &str = "a separate backup passphrase";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

fn kdf() -> KdfParams {
    KdfParams::insecure_for_tests()
}

async fn start_relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

fn tmp(tag: &str) -> PathBuf {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    std::env::temp_dir().join(format!("spike-backup-{tag}-{n}"))
}

struct Dev(Home);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.path()).ok();
    }
}
fn dev(tag: &str) -> Dev {
    Dev(Home::create(tmp(tag), PASS, kdf(), false).unwrap())
}
fn owner_of(h: &Home) -> String {
    IdentityCard::decode(&h.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}
fn befriend(a: &Home, name: &str, b: &Home) {
    let mut c = a.load_contacts().unwrap();
    c.push(Contact {
        name: name.into(),
        card: b.identity_card().unwrap(),
        verified: true,
    });
    a.save_contacts(&c).unwrap();
}
async fn recv(h: &Home, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(h, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}

fn none() -> std::collections::BTreeMap<String, String> {
    Default::default()
}

/// Alice and Bob who have talked, plus Alice's backup file (with history unless `history` is false).
async fn alice_bob_and_backup(url: &str, history: bool) -> (Dev, Dev, Vec<u8>) {
    let (a, b) = (dev("alice"), dev("bob"));
    befriend(&a.0, "bob", &b.0);
    befriend(&b.0, "alice", &a.0);
    ops::publish_keys(&a.0, url).await.unwrap();
    ops::publish_keys(&b.0, url).await.unwrap();
    ops::send(&a.0, url, "bob", b"before the loss")
        .await
        .unwrap();
    recv(&b.0, url).await;
    ops::send(&b.0, url, "alice", b"bob answers").await.unwrap();
    recv(&a.0, url).await;
    let file = backup::export(&a.0, BACKUP_PASS, kdf(), history, none()).unwrap();
    (a, b, file)
}

async fn restore_into(
    dir: &Path,
    file: &[u8],
    pass: &str,
    url: &str,
) -> anyhow::Result<backup::Restored> {
    let url = url.to_string();
    Opened::open(file, pass)?
        .restore(dir, NEW_PASS, kdf(), move |_, _| {
            Ok(Target::from(url.as_str()))
        })
        .await
}

#[tokio::test]
async fn restore_after_total_loss() {
    let url = start_relay().await;
    let (alice, bob, file) = alice_bob_and_backup(&url, true).await;
    let (owner, old_device, old_list) = (
        owner_of(&alice.0),
        alice.0.load_identity().unwrap().device_id(),
        alice.0.load_device_list().unwrap().version,
    );
    // The phone is gone: its state is deleted, only the file survives.
    drop(alice);

    let dir = tmp("alice-new");
    let r = restore_into(&dir, &file, BACKUP_PASS, &url).await.unwrap();
    let home = Home::unlock(&dir, NEW_PASS).unwrap();
    let _cleanup = Dev(Home::unlock(&dir, NEW_PASS).unwrap());
    assert_eq!((r.contacts, r.messages), (1, 2));
    assert_eq!(r.version, old_list + 1);

    // The same person, on a brand-new device, and only that device.
    assert_eq!(owner_of(&home), owner);
    let list = home.load_device_list().unwrap();
    let ids: Vec<_> = list
        .verify()
        .unwrap()
        .iter()
        .map(|c| c.device_id())
        .collect();
    assert_eq!(ids, vec![home.load_identity().unwrap().device_id()]);
    assert!(
        !ids.contains(&old_device),
        "the lost device must be unlisted"
    );

    // Contacts and history are there, and Bob is still verified.
    let c = &home.load_contacts().unwrap()[0];
    assert!(c.name == "bob" && c.verified);
    let hist = home
        .messages()
        .unwrap()
        .conversation(&owner_of(&bob.0), 10)
        .unwrap();
    assert_eq!(
        hist.iter()
            .map(|m| (m.direction, m.body.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (Direction::Out, "before the loss"),
            (Direction::In, "bob answers")
        ]
    );

    // Bob reaches the new device (his send refreshes Alice's list), and Alice reaches Bob with no
    // key-change alarm: same owner key, newer list.
    ops::send(&bob.0, &url, "alice", b"are you back?")
        .await
        .unwrap();
    assert!(
        matches!(recv(&home, &url).await.as_slice(), [Outcome::Message { body, .. }] if body == b"are you back?")
    );
    ops::send(&home, &url, "bob", b"I am back").await.unwrap();
    assert!(
        matches!(recv(&bob.0, &url).await.as_slice(), [Outcome::Message { from, body, .. }] if from == "alice" && body == b"I am back")
    );
    assert!(bob.0.load_contacts().unwrap()[0].verified);
}

#[tokio::test]
async fn restoring_removes_a_lost_device_that_turns_up_again() {
    let url = start_relay().await;
    let (alice, _bob, file) = alice_bob_and_backup(&url, false).await;
    let dir = tmp("alice-new");
    restore_into(&dir, &file, BACKUP_PASS, &url).await.unwrap();
    let _cleanup = Dev(Home::unlock(&dir, NEW_PASS).unwrap());
    // The "lost" phone is found and goes online: it learns it was removed.
    assert!(alice.0.this_device_is_listed().unwrap());
    recv(&alice.0, &url).await;
    assert!(!alice.0.this_device_is_listed().unwrap());
}

#[tokio::test]
async fn the_version_is_above_what_the_relay_already_holds() {
    let url = start_relay().await;
    let (alice, _bob, file) = alice_bob_and_backup(&url, false).await;
    // After the backup, Alice links a laptop (list v2) and removes it again (v3). The file says v1.
    let laptop = dev("laptop");
    let w = link::offer(&laptop.0, url.as_str()).unwrap();
    let t = Duration::from_secs(5);
    let new_side = async {
        let s = link::await_hello(&laptop.0, &w, t).await.unwrap();
        link::receive(&laptop.0, &w, &s, t).await.unwrap();
    };
    let old_side = async {
        let p = link::begin(&alice.0, url.as_str(), &w.code).await.unwrap();
        link::finish(&alice.0, url.as_str(), p, link::History::None)
            .await
            .unwrap();
    };
    tokio::join!(new_side, old_side);
    let laptop_id = laptop.0.load_identity().unwrap().device_id();
    let owner = alice.0.load_owner().unwrap();
    let v3 = alice
        .0
        .load_device_list()
        .unwrap()
        .without_device(&owner, &laptop_id)
        .unwrap();
    alice.0.save_device_list(&v3).unwrap();
    ops::publish_keys(&alice.0, url.as_str()).await.unwrap();
    assert_eq!(v3.version, 3);

    let dir = tmp("alice-new");
    let r = restore_into(&dir, &file, BACKUP_PASS, &url).await.unwrap();
    let _cleanup = Dev(Home::unlock(&dir, NEW_PASS).unwrap());
    assert_eq!(
        r.version, 4,
        "must be newer than the relay's list, not the file's"
    );
}

#[tokio::test]
async fn a_backup_without_history_has_none() {
    let url = start_relay().await;
    let (_alice, _bob, file) = alice_bob_and_backup(&url, false).await;
    let opened = Opened::open(&file, BACKUP_PASS).unwrap();
    assert_eq!((opened.contacts(), opened.messages()), (1, 0));
    let dir = tmp("alice-new");
    let r = restore_into(&dir, &file, BACKUP_PASS, &url).await.unwrap();
    let _cleanup = Dev(Home::unlock(&dir, NEW_PASS).unwrap());
    assert_eq!((r.contacts, r.messages), (1, 0));
}

#[tokio::test]
async fn failures_change_nothing() {
    let url = start_relay().await;
    let (alice, _bob, file) = alice_bob_and_backup(&url, true).await;

    // Wrong backup passphrase.
    assert!(Opened::open(&file, "not the passphrase").is_err());
    // A file that is not a backup, or is damaged.
    assert!(Opened::open(b"hello", BACKUP_PASS).is_err());
    let mut bad = file.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(Opened::open(&bad, BACKUP_PASS).is_err());

    // The relay is down: nothing is left behind, and the file can be tried again.
    let dir = tmp("alice-new");
    assert!(restore_into(&dir, &file, BACKUP_PASS, "ws://127.0.0.1:1")
        .await
        .is_err());
    assert!(!Home::exists(&dir));
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());

    // An identity is already here: refuse, and leave it alone.
    let r = restore_into(alice.0.path(), &file, BACKUP_PASS, &url).await;
    assert!(r.is_err());
    assert!(Home::unlock(alice.0.path(), PASS).is_ok());

    // And then it works, once the relay is back.
    restore_into(&dir, &file, BACKUP_PASS, &url).await.unwrap();
    let _cleanup = Dev(Home::unlock(&dir, NEW_PASS).unwrap());
}

#[tokio::test]
async fn the_file_holds_no_device_secrets_and_the_status_tracks_it() {
    let a = dev("alice");
    let b = dev("bob");
    befriend(&a.0, "bob", &b.0);
    assert!(backup::status(&a.0).unwrap().last_ms.is_none());
    let file = backup::export(&a.0, BACKUP_PASS, kdf(), false, none()).unwrap();
    let s = backup::status(&a.0).unwrap();
    assert_eq!(s.last_ms, Some(backup::created_at(&file).unwrap()));
    assert!(!s.stale, "a backup just made is current");
    // A new contact makes it out of date.
    let c = dev("carol");
    befriend(&a.0, "carol", &c.0);
    let s = backup::status(&a.0).unwrap();
    assert!(s.stale && s.reason.is_some());

    // The payload is the owner key and cards, not the device's account.
    let payload = Opened::open(&file, BACKUP_PASS)
        .unwrap()
        .raw_payload()
        .to_vec();
    let account = a.0.load_identity().unwrap().to_json().unwrap();
    let pickled: serde_json::Value = serde_json::from_str(&account).unwrap();
    let payload_text = String::from_utf8(payload).unwrap();
    assert!(payload_text.contains(&a.0.load_owner().unwrap().to_hex()));
    // No fragment of the Olm account pickle is in the payload.
    let secret = pickled.to_string();
    for chunk in secret.as_bytes().chunks(24).filter(|c| c.len() == 24) {
        assert!(!payload_text.contains(std::str::from_utf8(chunk).unwrap()));
    }
}
