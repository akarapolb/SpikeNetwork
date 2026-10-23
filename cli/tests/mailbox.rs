//! Group mailboxes on the relay (ADR 0025, slice 0), with fake payloads: the relay never looks
//! inside an entry, so these tests prove ordering, access, fan-out, rotation, retention and limits
//! without any MLS in sight.

use spike_cli::client::{Appended, Relay};
use spike_core::identity::Identity;
use spike_core::mailbox::{append_bytes, new_mailbox_id, MailboxKey};
use spike_core::wire::{ClientMsg, GroupKind, ServerMsg};
use spike_relay::{serve, serve_with_metrics, Config};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use GroupKind::{Commit, Message};

async fn relay(cfg: Config) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}

fn dev() -> Identity {
    Identity::generate().unwrap()
}

async fn conn(url: &str, who: &Identity) -> Relay {
    Relay::connect(url, who).await.unwrap()
}

/// A mailbox that exists on the relay, with the key that opens it.
async fn mailbox(r: &mut Relay) -> (String, MailboxKey) {
    let (id, key) = (new_mailbox_id(), MailboxKey::generate());
    r.group_create(&key, &id).await.unwrap();
    (id, key)
}

fn seqs(p: &spike_cli::client::GroupPage) -> Vec<u64> {
    p.entries.iter().map(|e| e.seq).collect()
}

fn blobs(p: &spike_cli::client::GroupPage) -> Vec<String> {
    p.entries
        .iter()
        .map(|e| String::from_utf8_lossy(&e.blob).into_owned())
        .collect()
}

fn refused(e: anyhow::Error, code: &str) {
    assert!(e.to_string().contains(code), "expected {code}, got: {e}");
}

#[tokio::test]
async fn a_mailbox_is_created_appended_to_and_read_in_order() {
    let url = relay(Config::default()).await;
    let (alice, bob) = (dev(), dev());
    let mut a = conn(&url, &alice).await;
    let (id, key) = mailbox(&mut a).await;
    for i in 1..=3u64 {
        let r = a
            .group_append(&key, &id, Message, None, format!("m{i}").as_bytes(), None)
            .await
            .unwrap();
        assert_eq!(r, Appended::Seq(i));
    }
    // Bob holds the same key (it reached him in a Welcome) and sees everything, in order.
    let mut b = conn(&url, &bob).await;
    let page = b.group_read(&key, &id, 0, 100).await.unwrap();
    assert_eq!(seqs(&page), [1, 2, 3]);
    assert_eq!(blobs(&page), ["m1", "m2", "m3"]);
    assert_eq!((page.next_seq, page.sealed), (4, false));
    // A cursor and a limit.
    assert_eq!(seqs(&b.group_read(&key, &id, 2, 100).await.unwrap()), [3]);
    let two = b.group_read(&key, &id, 0, 2).await.unwrap();
    assert_eq!(seqs(&two), [1, 2]);
    assert_eq!(
        two.next_seq, 4,
        "the next position, not the end of this page"
    );
    assert!(
        b.group_read(&key, &id, 3, 100)
            .await
            .unwrap()
            .entries
            .is_empty(),
        "nothing after the last entry"
    );
    assert!(b
        .group_read(&key, &id, 9, 100)
        .await
        .unwrap()
        .entries
        .is_empty());
    // An id can be registered once.
    refused(
        a.group_create(&MailboxKey::generate(), &id)
            .await
            .unwrap_err(),
        "exists",
    );
}

#[tokio::test]
async fn only_holders_of_the_key_can_use_a_mailbox() {
    let url = relay(Config::default()).await;
    let (alice, mallory) = (dev(), dev());
    let mut a = conn(&url, &alice).await;
    let (id, key) = mailbox(&mut a).await;
    a.group_append(&key, &id, Message, None, b"secret", None)
        .await
        .unwrap();

    // A different key opens nothing.
    let mut m = conn(&url, &mallory).await;
    let wrong = MailboxKey::generate();
    refused(
        m.group_append(&wrong, &id, Message, None, b"x", None)
            .await
            .unwrap_err(),
        "bad_signature",
    );
    refused(
        m.group_read(&wrong, &id, 0, 10).await.unwrap_err(),
        "bad_signature",
    );
    refused(
        m.group_subscribe(&wrong, &id).await.unwrap_err(),
        "bad_signature",
    );

    // A mailbox nobody registered.
    refused(
        m.group_read(&key, &new_mailbox_id(), 0, 10)
            .await
            .unwrap_err(),
        "unknown_mailbox",
    );

    // A request Alice signed correctly, repeated by Mallory on her own connection, is refused:
    // every signature names the device that made the request.
    let sig = key.sign(&append_bytes(
        &alice.device_id(),
        &id,
        Message,
        None,
        b"replay",
        None,
    ));
    let replayed = ClientMsg::GroupAppend {
        mailbox: id.clone(),
        kind: Message,
        expected_seq: None,
        blob: hex::encode(b"replay"),
        seal: None,
        sig,
    };
    let reply = m.request(replayed).await.unwrap();
    assert!(
        matches!(&reply, ServerMsg::Error { code, .. } if code == "bad_signature"),
        "{reply:?}"
    );

    // Registering needs proof of the private key: a public key signed by someone else is refused,
    // and nothing is created.
    let (fresh_id, theirs, mine) = (
        new_mailbox_id(),
        MailboxKey::generate(),
        MailboxKey::generate(),
    );
    let bad = ClientMsg::GroupCreate {
        mailbox: fresh_id.clone(),
        pubkey: theirs.public_hex(),
        sig: mine.sign(&spike_core::mailbox::create_bytes(
            &mallory.device_id(),
            &fresh_id,
            &theirs.public_hex(),
        )),
    };
    let reply = m.request(bad).await.unwrap();
    assert!(
        matches!(&reply, ServerMsg::Error { code, .. } if code == "bad_signature"),
        "{reply:?}"
    );
    refused(
        m.group_read(&theirs, &fresh_id, 0, 1).await.unwrap_err(),
        "unknown_mailbox",
    );

    // Malformed ids and hex are plain bad requests.
    let junk = ClientMsg::GroupRead {
        mailbox: "not-an-id".into(),
        after_seq: 0,
        limit: 1,
        sig: "zz".into(),
    };
    let reply = m.request(junk).await.unwrap();
    assert!(
        matches!(&reply, ServerMsg::Error { code, .. } if code == "bad_request"),
        "{reply:?}"
    );

    // None of that changed the mailbox.
    let page = a.group_read(&key, &id, 0, 10).await.unwrap();
    assert_eq!(blobs(&page), ["secret"]);
}

#[tokio::test]
async fn commits_are_totally_ordered_by_compare_and_append() {
    let url = relay(Config::default()).await;
    let (alice, bob) = (dev(), dev());
    let (mut a, mut b) = (conn(&url, &alice).await, conn(&url, &bob).await);
    let (id, key) = mailbox(&mut a).await;

    // A commit has to say where it expects to land.
    refused(
        a.group_append(&key, &id, Commit, None, b"c", None)
            .await
            .unwrap_err(),
        "bad_request",
    );

    // Two members commit from the same position at once, five times over: exactly one wins each
    // round, the other is told where the log is now and retries from there.
    for round in 0..5u64 {
        let next = round + 1; // each round the log has grown by exactly one commit
        let (pa, pb) = (format!("a{round}"), format!("b{round}"));
        let (ra, rb) = tokio::join!(
            a.group_append(&key, &id, Commit, Some(next), pa.as_bytes(), None),
            b.group_append(&key, &id, Commit, Some(next), pb.as_bytes(), None),
        );
        let (ra, rb) = (ra.unwrap(), rb.unwrap());
        let winners = [&ra, &rb]
            .iter()
            .filter(|r| **r == &Appended::Seq(next))
            .count();
        assert_eq!(winners, 1, "round {round}: {ra:?} {rb:?}");
        let loser = if ra == Appended::Seq(next) { rb } else { ra };
        assert_eq!(loser, Appended::Conflict { next_seq: next + 1 });
    }
    let log = a.group_read(&key, &id, 0, 100).await.unwrap();
    assert_eq!(seqs(&log), [1, 2, 3, 4, 5], "no gaps, no duplicates");

    // Messages carry no such constraint: two at once both land, in some order, with their own numbers.
    let (ra, rb) = tokio::join!(
        a.group_append(&key, &id, Message, None, b"x", None),
        b.group_append(&key, &id, Message, None, b"y", None),
    );
    let mut got = [ra.unwrap(), rb.unwrap()];
    got.sort_by_key(|r| match r {
        Appended::Seq(n) => *n,
        _ => 0,
    });
    assert_eq!(got, [Appended::Seq(6), Appended::Seq(7)]);
}

async fn http_get(addr: &str, path: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.split_once("\r\n\r\n").unwrap().1.to_string()
}

#[tokio::test]
async fn one_append_is_stored_once_and_pushed_to_every_subscriber() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let m = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let maddr = m.local_addr().unwrap().to_string();
    tokio::spawn(serve_with_metrics(l, Some(m), Config::default()));

    let devices = [dev(), dev(), dev(), dev()];
    let mut writer = conn(&url, &devices[0]).await;
    let (id, key) = mailbox(&mut writer).await;
    let mut readers = vec![];
    for d in &devices[1..] {
        let mut r = conn(&url, d).await;
        let (next, sealed) = r.group_subscribe(&key, &id).await.unwrap();
        assert_eq!((next, sealed), (1, false));
        readers.push(r);
    }
    // Someone without the key cannot listen in.
    let stranger = dev();
    let mut s = conn(&url, &stranger).await;
    refused(
        s.group_subscribe(&MailboxKey::generate(), &id)
            .await
            .unwrap_err(),
        "bad_signature",
    );

    let secret = b"top secret group traffic";
    let seq = writer
        .group_append(&key, &id, Message, None, secret, None)
        .await
        .unwrap();
    assert_eq!(seq, Appended::Seq(1));
    for r in &mut readers {
        let (mb, entry, sealed) = r
            .next_group_entry(Duration::from_secs(2))
            .await
            .unwrap()
            .expect("pushed");
        assert_eq!((mb.as_str(), entry.seq, sealed), (id.as_str(), 1, false));
        assert_eq!(entry.blob, secret);
    }
    // The stranger got nothing.
    assert!(s
        .next_group_entry(Duration::from_millis(300))
        .await
        .unwrap()
        .is_none());

    // One copy stored, three pushed, and the numbers say nothing identifying.
    let text = http_get(&maddr, "/metrics").await;
    for line in [
        "spike_relay_group_entries 1",
        "spike_relay_group_mailboxes 1",
        "spike_relay_group_pushes_total 3",
        "spike_relay_group_appends_total 1",
    ] {
        assert!(text.lines().any(|l| l == line), "missing {line}\n{text}");
    }
    for needle in [
        id.clone(),
        hex::encode(secret),
        devices[0].device_id(),
        key.public_hex(),
    ] {
        assert!(!text.contains(&needle), "metrics leaked {needle}");
    }

    // Unsubscribing stops the pushes.
    readers[0].group_unsubscribe(&id).await.unwrap();
    readers[0].ping().await.unwrap();
    writer
        .group_append(&key, &id, Message, None, b"again", None)
        .await
        .unwrap();
    assert!(readers[0]
        .next_group_entry(Duration::from_millis(300))
        .await
        .unwrap()
        .is_none());
    let (_, e, _) = readers[1]
        .next_group_entry(Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e.seq, 2);
}

#[tokio::test]
async fn a_reader_who_joins_late_catches_up_from_its_cursor_without_gaps() {
    let url = relay(Config::default()).await;
    let (alice, bob) = (dev(), dev());
    let mut a = conn(&url, &alice).await;
    let (id, key) = mailbox(&mut a).await;
    for i in 1..=5 {
        a.group_append(&key, &id, Message, None, format!("m{i}").as_bytes(), None)
            .await
            .unwrap();
    }
    let mut b = conn(&url, &bob).await;
    // Subscribe first, then read from the last position Bob processed (2).
    let (next, _) = b.group_subscribe(&key, &id).await.unwrap();
    assert_eq!(next, 6);
    let page = b.group_read(&key, &id, 2, 100).await.unwrap();
    assert_eq!(blobs(&page), ["m3", "m4", "m5"]);
    // Something appended meanwhile is pushed with its own position, so Bob can tell it apart.
    a.group_append(&key, &id, Message, None, b"m6", None)
        .await
        .unwrap();
    let (_, pushed, _) = b
        .next_group_entry(Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((pushed.seq, pushed.blob.as_slice()), (6, &b"m6"[..]));
}

/// The new key and mailbox a removal commit hands the group to, as a fake payload: a real client
/// puts this inside the encrypted commit.
fn handover(id: &str, key: &MailboxKey) -> Vec<u8> {
    format!("next:{id}:{}", key.to_hex()).into_bytes()
}

fn parse_handover(blob: &[u8]) -> (String, MailboxKey) {
    let s = String::from_utf8_lossy(blob).into_owned();
    let mut p = s.splitn(3, ':');
    assert_eq!(p.next(), Some("next"));
    (
        p.next().unwrap().to_string(),
        MailboxKey::from_hex(p.next().unwrap()).unwrap(),
    )
}

#[tokio::test]
async fn a_removal_seals_the_mailbox_and_the_group_moves_on_without_the_removed_member() {
    let url = relay(Config::default()).await;
    let (admin, member, lagging, removed) = (dev(), dev(), dev(), dev());
    let mut a = conn(&url, &admin).await;
    let (old_id, old_key) = mailbox(&mut a).await;
    a.group_append(&old_key, &old_id, Message, None, b"hello", None)
        .await
        .unwrap();

    let (new_id, new_key) = (new_mailbox_id(), MailboxKey::generate());
    let seal = Some((new_id.as_str(), &new_key));

    // A seal that does not prove it holds the new key changes nothing.
    let proof_by_someone_else = ClientMsg::GroupAppend {
        mailbox: old_id.clone(),
        kind: Commit,
        expected_seq: Some(2),
        blob: hex::encode(handover(&new_id, &new_key)),
        seal: Some(spike_core::wire::WireSeal {
            mailbox: new_id.clone(),
            pubkey: new_key.public_hex(),
            sig: MailboxKey::generate().sign(&spike_core::mailbox::create_bytes(
                &admin.device_id(),
                &new_id,
                &new_key.public_hex(),
            )),
        }),
        sig: old_key.sign(&append_bytes(
            &admin.device_id(),
            &old_id,
            Commit,
            Some(2),
            &handover(&new_id, &new_key),
            Some((&new_id, &new_key.public_hex())),
        )),
    };
    let reply = a.request(proof_by_someone_else).await.unwrap();
    assert!(
        matches!(&reply, ServerMsg::Error { code, .. } if code == "bad_signature"),
        "{reply:?}"
    );
    let still = a.group_read(&old_key, &old_id, 0, 10).await.unwrap();
    assert_eq!(
        (still.next_seq, still.sealed),
        (2, false),
        "nothing was appended or sealed"
    );

    // A stale position with a seal: told to retry, nothing sealed.
    let r = a
        .group_append(
            &old_key,
            &old_id,
            Commit,
            Some(1),
            &handover(&new_id, &new_key),
            seal,
        )
        .await
        .unwrap();
    assert_eq!(r, Appended::Conflict { next_seq: 2 });
    assert!(!a.group_read(&old_key, &old_id, 0, 1).await.unwrap().sealed);

    // A message cannot seal; a seal cannot land on an id that exists.
    refused(
        a.group_append(&old_key, &old_id, Message, None, b"x", seal)
            .await
            .unwrap_err(),
        "bad_request",
    );
    let (taken_id, taken_key) = mailbox(&mut a).await;
    refused(
        a.group_append(
            &old_key,
            &old_id,
            Commit,
            Some(2),
            b"x",
            Some((&taken_id, &taken_key)),
        )
        .await
        .unwrap_err(),
        "exists",
    );
    assert!(!a.group_read(&old_key, &old_id, 0, 1).await.unwrap().sealed);

    // The real thing: the removal commit seals the old mailbox and opens the new one at once.
    let r = a
        .group_append(
            &old_key,
            &old_id,
            Commit,
            Some(2),
            &handover(&new_id, &new_key),
            seal,
        )
        .await
        .unwrap();
    assert_eq!(r, Appended::Seq(2));
    refused(
        a.group_append(&old_key, &old_id, Message, None, b"late", None)
            .await
            .unwrap_err(),
        "sealed",
    );
    refused(
        a.group_append(&old_key, &old_id, Commit, Some(3), b"again", seal)
            .await
            .unwrap_err(),
        "sealed",
    );
    let old = a.group_read(&old_key, &old_id, 0, 10).await.unwrap();
    assert!(old.sealed);
    assert_eq!(
        seqs(&old),
        [1, 2],
        "the log, including the sealing commit, can still be replayed"
    );

    // The new mailbox works for the new key and only the new key.
    let mut m = conn(&url, &member).await;
    assert_eq!(
        m.group_append(&new_key, &new_id, Message, None, b"after", None)
            .await
            .unwrap(),
        Appended::Seq(1)
    );

    // A member who was merely offline replays the old log, finds the handover in the commit,
    // and reads on from the new mailbox.
    let mut l = conn(&url, &lagging).await;
    let replay = l.group_read(&old_key, &old_id, 0, 10).await.unwrap();
    let commit = replay.entries.iter().find(|e| e.kind == Commit).unwrap();
    let (found_id, found_key) = parse_handover(&commit.blob);
    let page = l.group_read(&found_key, &found_id, 0, 10).await.unwrap();
    assert_eq!(blobs(&page), ["after"]);

    // The removed member holds only the old key: she can read what was already there, but she
    // cannot append to the old mailbox and cannot touch the new one.
    let mut x = conn(&url, &removed).await;
    assert_eq!(
        seqs(&x.group_read(&old_key, &old_id, 0, 10).await.unwrap()),
        [1, 2]
    );
    refused(
        x.group_append(&old_key, &old_id, Message, None, b"noise", None)
            .await
            .unwrap_err(),
        "sealed",
    );
    refused(
        x.group_read(&old_key, &new_id, 0, 10).await.unwrap_err(),
        "bad_signature",
    );
    refused(
        x.group_append(&old_key, &new_id, Message, None, b"noise", None)
            .await
            .unwrap_err(),
        "bad_signature",
    );
}

#[tokio::test]
async fn handshake_entries_outlive_ordinary_messages() {
    let url = relay(Config {
        group_message_ttl: Duration::from_millis(500),
        ..Config::default()
    })
    .await;
    let alice = dev();
    let mut a = conn(&url, &alice).await;
    let (id, key) = mailbox(&mut a).await;
    a.group_append(&key, &id, Message, None, b"chat", None)
        .await
        .unwrap();
    a.group_append(&key, &id, Commit, Some(2), b"commit", None)
        .await
        .unwrap();
    assert_eq!(seqs(&a.group_read(&key, &id, 0, 10).await.unwrap()), [1, 2]);
    tokio::time::sleep(Duration::from_millis(900)).await;
    let page = a.group_read(&key, &id, 0, 10).await.unwrap();
    assert_eq!(
        blobs(&page),
        ["commit"],
        "the chat expired, the commit did not"
    );
    assert_eq!(page.next_seq, 3, "positions are never reused");
}

#[tokio::test]
async fn entries_are_size_limited() {
    let url = relay(Config {
        group_max_entry: 1000,
        ..Config::default()
    })
    .await;
    let mut a = conn(&url, &dev()).await;
    let (id, key) = mailbox(&mut a).await;
    refused(
        a.group_append(&key, &id, Message, None, &vec![0u8; 1001], None)
            .await
            .unwrap_err(),
        "too_large",
    );
    a.group_append(&key, &id, Message, None, &vec![0u8; 1000], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn one_device_cannot_create_unlimited_mailboxes() {
    let url = relay(Config {
        group_max_mailboxes_per_device: 2,
        ..Config::default()
    })
    .await;
    let mut a = conn(&url, &dev()).await;
    let (id, key) = mailbox(&mut a).await;
    mailbox(&mut a).await;
    refused(
        a.group_create(&MailboxKey::generate(), &new_mailbox_id())
            .await
            .unwrap_err(),
        "quota_exceeded",
    );
    // Another device is not affected, and a seal opens a mailbox too, so it counts for whoever seals.
    let mut b = conn(&url, &dev()).await;
    mailbox(&mut b).await;
    let (next_id, next_key) = (new_mailbox_id(), MailboxKey::generate());
    refused(
        a.group_append(
            &key,
            &id,
            Commit,
            Some(1),
            b"c",
            Some((&next_id, &next_key)),
        )
        .await
        .unwrap_err(),
        "quota_exceeded",
    );
    assert!(
        !a.group_read(&key, &id, 0, 1).await.unwrap().sealed,
        "and nothing was sealed"
    );
}

#[tokio::test]
async fn a_full_mailbox_drops_old_messages_but_never_commits() {
    let url = relay(Config {
        group_max_bytes: 2500,
        ..Config::default()
    })
    .await;
    let mut a = conn(&url, &dev()).await;
    let (id, key) = mailbox(&mut a).await;
    let kb = vec![7u8; 1000];
    a.group_append(&key, &id, Commit, Some(1), &vec![1u8; 500], None)
        .await
        .unwrap();
    a.group_append(&key, &id, Message, None, &kb, None)
        .await
        .unwrap(); // 1500
    a.group_append(&key, &id, Message, None, &kb, None)
        .await
        .unwrap(); // 2500: fits exactly
                   // One more does not fit: the oldest message makes room.
    a.group_append(&key, &id, Message, None, &kb, None)
        .await
        .unwrap();
    let page = a.group_read(&key, &id, 0, 10).await.unwrap();
    assert_eq!(
        seqs(&page),
        [1, 3, 4],
        "the commit stays, the oldest message went"
    );

    // A mailbox full of commits has nothing to give up: it says so and appends nothing.
    let (cid, ckey) = mailbox(&mut a).await;
    a.group_append(&ckey, &cid, Commit, Some(1), &kb, None)
        .await
        .unwrap();
    a.group_append(&ckey, &cid, Commit, Some(2), &kb, None)
        .await
        .unwrap();
    refused(
        a.group_append(&ckey, &cid, Commit, Some(3), &kb, None)
            .await
            .unwrap_err(),
        "mailbox_full",
    );
    assert_eq!(a.group_read(&ckey, &cid, 0, 10).await.unwrap().next_seq, 3);
}

#[tokio::test]
async fn subscriptions_are_capped_per_connection_and_dropped_connections_do_not_matter() {
    let url = relay(Config {
        max_group_subs_per_conn: 2,
        ..Config::default()
    })
    .await;
    let mut a = conn(&url, &dev()).await;
    let mut boxes = vec![];
    for _ in 0..3 {
        boxes.push(mailbox(&mut a).await);
    }
    let mut s = conn(&url, &dev()).await;
    s.group_subscribe(&boxes[0].1, &boxes[0].0).await.unwrap();
    s.group_subscribe(&boxes[1].1, &boxes[1].0).await.unwrap();
    // Subscribing again to one already held does not use another slot.
    s.group_subscribe(&boxes[0].1, &boxes[0].0).await.unwrap();
    refused(
        s.group_subscribe(&boxes[2].1, &boxes[2].0)
            .await
            .unwrap_err(),
        "quota_exceeded",
    );
    s.group_unsubscribe(&boxes[1].0).await.unwrap();
    s.ping().await.unwrap();
    s.group_subscribe(&boxes[2].1, &boxes[2].0).await.unwrap();

    // Subscribers that vanish are cleaned up; the live one still gets its push and appends work.
    let (id, key) = &boxes[0];
    let mut gone = conn(&url, &dev()).await;
    gone.group_subscribe(key, id).await.unwrap();
    drop(gone);
    tokio::time::sleep(Duration::from_millis(200)).await;
    a.group_append(key, id, Message, None, b"still fine", None)
        .await
        .unwrap();
    let (_, e, _) = s
        .next_group_entry(Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e.blob, b"still fine");
}

#[tokio::test]
async fn mailboxes_survive_a_relay_restart() {
    let dir = std::env::temp_dir().join(format!("spike-mailbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = || Config {
        db_path: Some(dir.join("relay.db")),
        ..Config::default()
    };
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let first = tokio::spawn(serve(l, cfg()));
    let alice = dev();
    let mut a = conn(&url, &alice).await;
    let (id, key) = mailbox(&mut a).await;
    a.group_append(&key, &id, Message, None, b"kept", None)
        .await
        .unwrap();
    let (nid, nkey) = (new_mailbox_id(), MailboxKey::generate());
    a.group_append(&key, &id, Commit, Some(2), b"seal", Some((&nid, &nkey)))
        .await
        .unwrap();
    drop(a);
    first.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg()));
    let mut a = conn(&url, &alice).await;
    let page = a.group_read(&key, &id, 0, 10).await.unwrap();
    assert_eq!(blobs(&page), ["kept", "seal"]);
    assert!(page.sealed, "the seal survived too");
    assert_eq!(
        a.group_append(&nkey, &nid, Message, None, b"new", None)
            .await
            .unwrap(),
        Appended::Seq(1),
        "and so did the successor"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_evicted_connection_is_closed_even_if_it_was_subscribed() {
    let url = relay(Config::default()).await;
    let who = dev();
    let mut a = conn(&url, &dev()).await;
    let (id, key) = mailbox(&mut a).await;
    // The same device opens connections until the relay keeps only four (the oldest is closed).
    let mut first = conn(&url, &who).await;
    first.group_subscribe(&key, &id).await.unwrap();
    let mut others = vec![];
    for _ in 0..4 {
        others.push(conn(&url, &who).await);
    }
    let ended = tokio::time::timeout(
        Duration::from_secs(5),
        first.next_group_entry(Duration::from_secs(30)),
    )
    .await;
    assert!(
        matches!(ended, Ok(Err(_))),
        "the oldest connection must be closed, subscribed or not: {ended:?}"
    );
    // The relay forgot its subscription: an append reaches nobody and breaks nothing.
    a.group_append(&key, &id, Message, None, b"x", None)
        .await
        .unwrap();
}

// ----- key packages: opaque blobs a device publishes so others can add it to a group -----

fn pkg(tag: u8, len: usize) -> Vec<u8> {
    vec![tag; len]
}

#[tokio::test]
async fn key_packages_are_handed_out_once_each_then_the_last_resort_repeats() {
    let url = relay(Config::default()).await;
    let (owner, other) = (dev(), dev());
    let mut o = conn(&url, &owner).await;
    let mut c = conn(&url, &other).await;

    // Nothing published: nothing to claim.
    assert!(c
        .claim_key_package(&owner.device_id())
        .await
        .unwrap()
        .is_none());

    assert_eq!(
        o.publish_key_packages(&[pkg(1, 385), pkg(2, 385), pkg(3, 385)])
            .await
            .unwrap(),
        3
    );
    // Publishing the same one twice does not double it.
    assert_eq!(o.publish_key_packages(&[pkg(3, 385)]).await.unwrap(), 3);
    o.publish_last_resort(&pkg(9, 385)).await.unwrap();

    // One-time packages go out in order, each once, and never to two claimers.
    let got: Vec<_> = {
        let mut v = vec![];
        for _ in 0..3 {
            v.push(
                c.claim_key_package(&owner.device_id())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        v
    };
    assert_eq!(
        got,
        [
            (pkg(1, 385), false),
            (pkg(2, 385), false),
            (pkg(3, 385), false)
        ]
    );
    // Then the reusable last-resort package, every time and flagged as such.
    for _ in 0..2 {
        assert_eq!(
            c.claim_key_package(&owner.device_id()).await.unwrap(),
            Some((pkg(9, 385), true))
        );
    }
    // A new last-resort package replaces the old one.
    o.publish_last_resort(&pkg(8, 385)).await.unwrap();
    assert_eq!(
        c.claim_key_package(&owner.device_id()).await.unwrap(),
        Some((pkg(8, 385), true))
    );
    // One device's packages are not another's.
    assert!(o
        .claim_key_package(&other.device_id())
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn key_package_publishing_is_bounded() {
    let url = relay(Config {
        max_claims_per_conn: 3,
        ..Config::default()
    })
    .await;
    let owner = dev();
    let mut o = conn(&url, &owner).await;
    refused(
        o.publish_key_packages(&[pkg(1, 17 * 1024)])
            .await
            .unwrap_err(),
        "too_large",
    );
    refused(
        o.publish_key_packages(&[]).await.unwrap_err(),
        "bad_request",
    );
    // A device holds a bounded number; the rest are not stored.
    let many: Vec<Vec<u8>> = (0..300u32).map(|i| i.to_be_bytes().repeat(100)).collect();
    let stored = o.publish_key_packages(&many).await.unwrap();
    assert_eq!(stored, 200, "the cap");
    // One connection may claim only so many (the same limit as prekeys), so a stranger cannot drain a device.
    let mut c = conn(&url, &dev()).await;
    for _ in 0..3 {
        c.claim_key_package(&owner.device_id()).await.unwrap();
    }
    refused(
        c.claim_key_package(&owner.device_id()).await.unwrap_err(),
        "rate_limited",
    );
}

#[tokio::test]
async fn key_packages_survive_a_restart_and_show_only_as_a_count() {
    let dir = std::env::temp_dir().join(format!("spike-kp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = || Config {
        db_path: Some(dir.join("relay.db")),
        ..Config::default()
    };
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let first = tokio::spawn(serve(l, cfg()));
    let owner = dev();
    let mut o = conn(&url, &owner).await;
    o.publish_key_packages(&[pkg(1, 385), pkg(2, 385)])
        .await
        .unwrap();
    o.publish_last_resort(&pkg(9, 385)).await.unwrap();
    drop(o);
    first.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let m = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let maddr = m.local_addr().unwrap().to_string();
    tokio::spawn(serve_with_metrics(l, Some(m), cfg()));
    let mut c = conn(&url, &dev()).await;
    assert_eq!(
        c.claim_key_package(&owner.device_id()).await.unwrap(),
        Some((pkg(1, 385), false))
    );
    let text = http_get(&maddr, "/metrics").await;
    assert!(
        text.lines().any(|l| l == "spike_relay_key_packages 1"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l == "spike_relay_key_package_claims_total 1"),
        "{text}"
    );
    assert!(!text.contains(&owner.device_id()));
    let _ = std::fs::remove_dir_all(&dir);
}
