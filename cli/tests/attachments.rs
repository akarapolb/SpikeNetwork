//! Attachments: encrypted client-side, stored on the relay as opaque chunks.

use spike_cli::{
    client::Relay,
    link::{self, History},
    messages::Direction,
    ops,
    state::{Contact, Home},
    Outcome,
};
use spike_core::attachment::{seal_file, CHUNK};
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase attachments";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

async fn relay(cfg: Config) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}
fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "spike-att-{tag}-{}",
        hex::encode(&Identity::generate().unwrap().ed_public()[..6])
    ))
}
struct Dev(Home);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.path()).ok();
    }
}
fn dev(tag: &str) -> Dev {
    Dev(Home::create(tmp(tag), PASS, KdfParams::insecure_for_tests(), false).unwrap())
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
fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        v.extend_from_slice(&Identity::generate().unwrap().ed_public());
    }
    v.truncate(n);
    v
}
async fn pair(url: &str) -> (Dev, Dev) {
    let (a, b) = (dev("alice"), dev("bob"));
    befriend(&a, "bob", &b);
    befriend(&b, "alice", &a);
    ops::publish_keys(&a.0, url).await.unwrap();
    ops::publish_keys(&b.0, url).await.unwrap();
    (a, b)
}

#[tokio::test]
async fn multi_chunk_file_round_trip_and_local_copies() {
    let url = relay(Config::default()).await;
    let (alice, bob) = pair(&url).await;
    let data = random_bytes(CHUNK * 2 + 4321);
    let r = ops::send_file(
        &alice.0,
        &url,
        "bob",
        "../../site-plan.pdf",
        &data,
        "plans attached",
    )
    .await
    .unwrap();
    assert_eq!(r.delivered(), 1);

    let got = recv(&bob, &url).await;
    assert!(matches!(got.as_slice(), [Outcome::Message { body, .. }] if body == b"plans attached"));
    let (att, bytes) = ops::fetch_attachment(&bob.0, &url, &r.id[..8])
        .await
        .unwrap();
    assert_eq!(bytes, data);
    assert_eq!(
        (att.name.as_str(), att.chunks),
        ("site-plan.pdf", 3),
        "path parts stripped from the name"
    );
    // Kept locally, encrypted: a second fetch does not need the relay.
    assert_eq!(bob.0.load_attachment(&r.id).unwrap().unwrap(), data);
    assert_eq!(
        alice.0.load_attachment(&r.id).unwrap().unwrap(),
        data,
        "sender keeps its own copy"
    );
    let stored = bob.0.messages().unwrap().find(&r.id).unwrap();
    assert_eq!(
        (stored.direction, stored.attachment.unwrap().size),
        (Direction::In, data.len() as u64)
    );
}

#[tokio::test]
async fn senders_other_device_gets_a_copy_it_can_open() {
    let url = relay(Config::default()).await;
    let (alice, bob) = pair(&url).await;
    // Give Alice a second device under the same identity.
    let a2 = dev("alice2");
    let owner = alice.0.load_owner().unwrap();
    let list = alice
        .0
        .load_device_list()
        .unwrap()
        .with_device(
            &owner,
            DeviceEntry {
                card: a2.0.load_identity().unwrap().card().encode(),
                relay: None,
            },
        )
        .unwrap();
    alice.0.save_device_list(&list).unwrap();
    a2.0.install_identity(&owner, &list).unwrap();
    befriend(&a2, "bob", &bob);
    ops::publish_keys(&a2.0, &url).await.unwrap();

    let data = b"minutes of the meeting".to_vec();
    let r = ops::send_file(&alice.0, &url, "bob", "minutes.txt", &data, "")
        .await
        .unwrap();
    assert!(
        matches!(recv(&a2, &url).await.as_slice(), [Outcome::SentCopy { to, .. }] if to == "bob")
    );
    assert_eq!(
        ops::fetch_attachment(&a2.0, &url, &r.id).await.unwrap().1,
        data
    );
}

#[tokio::test]
async fn relay_disk_holds_no_file_content_and_tampering_is_caught() {
    let dir = tmp("relaydb");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("relay.db");
    let url = relay(Config {
        db_path: Some(db.clone()),
        ..Config::default()
    })
    .await;
    let (alice, bob) = pair(&url).await;
    let marker = b"TOP-SECRET-ATTACHMENT-MARKER".repeat(1000);
    let r = ops::send_file(&alice.0, &url, "bob", "secret.txt", &marker, "")
        .await
        .unwrap();
    recv(&bob, &url).await;

    for e in std::fs::read_dir(&dir).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        assert!(
            !bytes.windows(28).any(|w| w == &marker[..28]),
            "file content on the relay disk"
        );
        assert!(
            !String::from_utf8_lossy(&bytes).contains("secret.txt"),
            "file name on the relay disk"
        );
    }

    // Flip one byte of the stored chunk: the download must be refused, not returned.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mut data: Vec<u8> = conn
        .query_row("SELECT data FROM blob_chunks", [], |r| r.get(0))
        .unwrap();
    data[100] ^= 1;
    conn.execute("UPDATE blob_chunks SET data = ?1", [data])
        .unwrap();
    let e = ops::fetch_attachment(&bob.0, &url, &r.id)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("integrity"), "{e}");
    assert!(
        bob.0.load_attachment(&r.id).unwrap().is_none(),
        "nothing kept after a failed check"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn expired_attachments_give_a_clear_message() {
    let url = relay(Config {
        blob_ttl: Duration::from_millis(300),
        ..Config::default()
    })
    .await;
    let (alice, bob) = pair(&url).await;
    let r = ops::send_file(&alice.0, &url, "bob", "a.txt", b"short-lived", "")
        .await
        .unwrap();
    recv(&bob, &url).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let e = ops::fetch_attachment(&bob.0, &url, &r.id)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("no longer"), "{e}");
}

#[tokio::test]
async fn relay_blob_rules_ownership_shape_and_quota() {
    let url = relay(Config {
        blob_quota: 300 * 1024,
        ..Config::default()
    })
    .await;
    let (owner, other) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let mut a = Relay::connect(&url, &owner).await.unwrap();
    let mut b = Relay::connect(&url, &other).await.unwrap();
    let (att, chunks) = seal_file("f", &random_bytes(CHUNK + 1)).unwrap();
    a.put_blob_chunk(&att.blob, 0, 2, &chunks[0]).await.unwrap();
    a.put_blob_chunk(&att.blob, 0, 2, &chunks[0]).await.unwrap(); // retry is fine

    let e = b
        .put_blob_chunk(&att.blob, 1, 2, &chunks[1])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("not_owner"), "{e}");
    let e = a
        .put_blob_chunk(&att.blob, 1, 3, &chunks[1])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("chunk count"), "{e}");
    let e = a
        .put_blob_chunk(&att.blob, 2, 2, &chunks[1])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("bad_request"), "{e}");
    let e = a
        .put_blob_chunk(&att.blob, 1, 2, &vec![0u8; CHUNK + 1000])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("too_large"), "{e}");

    // Quota: a second full chunk would exceed 300 KiB for this device.
    let (att2, chunks2) = seal_file("g", &random_bytes(CHUNK)).unwrap();
    let e = a
        .put_blob_chunk(&att2.blob, 0, 1, &chunks2[0])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("quota"), "{e}");
    // Anyone with the random id can read (the id is the capability); unknown ids give nothing.
    assert_eq!(
        b.get_blob_chunk(&att.blob, 0).await.unwrap().unwrap(),
        chunks[0]
    );
    assert!(b.get_blob_chunk(&att2.blob, 0).await.unwrap().is_none());
}

#[tokio::test]
async fn linking_carries_recent_files_and_older_ones_are_fetched_on_demand() {
    let url = relay(Config::default()).await;
    let (alice, bob) = pair(&url).await;
    let recent = ops::send_file(
        &alice.0,
        &url,
        "bob",
        "recent.txt",
        b"this week's notes",
        "",
    )
    .await
    .unwrap();
    // An older attachment that Alice's phone does not hold locally (as if cleared), still on the relay.
    let old_data = b"last quarter's report".to_vec();
    let old = ops::send_file(&alice.0, &url, "bob", "old.txt", &old_data, "")
        .await
        .unwrap();
    let mut old_msg = alice.0.messages().unwrap().find(&old.id).unwrap();
    old_msg.id = "ab".repeat(16);
    old_msg.ts -= 90 * 86_400_000;
    alice.0.messages().unwrap().insert(&old_msg).unwrap();
    drop(bob);

    let a2 = dev("alice-tablet");
    let w = link::offer(&a2.0, &url).unwrap();
    let new_side = async {
        let s = link::await_hello(&a2.0, &w, Duration::from_secs(5))
            .await
            .unwrap();
        link::receive(&a2.0, &w, &s, Duration::from_secs(5))
            .await
            .unwrap()
    };
    let existing = async {
        let p = link::begin(&alice.0, &url, &w.code).await.unwrap();
        link::finish(&alice.0, &url, p, History::All).await.unwrap()
    };
    let (_, linked) = tokio::join!(new_side, existing);
    assert_eq!(
        linked.files, 2,
        "both recent files travel in the bundle; the 90-day-old one does not"
    );
    assert_eq!(
        a2.0.load_attachment(&recent.id).unwrap().unwrap(),
        b"this week's notes"
    );
    assert!(a2.0.load_attachment(&old_msg.id).unwrap().is_none());
    assert_eq!(
        ops::fetch_attachment(&a2.0, &url, &old_msg.id)
            .await
            .unwrap()
            .1,
        old_data,
        "older file fetched on demand"
    );
    assert_eq!(
        IdentityCard::decode(&a2.0.identity_card().unwrap())
            .unwrap()
            .owner_hex(),
        IdentityCard::decode(&alice.0.identity_card().unwrap())
            .unwrap()
            .owner_hex()
    );
}
