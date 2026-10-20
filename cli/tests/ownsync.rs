//! Milestone M5 (second slice): the own-device sync channel: read state,
//! catch-up after a long absence, and device-to-device file requests.

use spike_cli::{
    ops,
    state::{Contact, Home},
    sync::SyncMsg,
    Outcome,
};
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase ownsync";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

async fn relay(cfg: Config) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, cfg));
    url
}
struct Dev(Home);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.path()).ok();
    }
}
fn person(tag: &str, n: usize) -> (Vec<Dev>, String) {
    let devs: Vec<Dev> = (0..n)
        .map(|i| {
            let id = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
            Dev(Home::create(
                std::env::temp_dir().join(format!("spike-os-{tag}{i}-{id}")),
                PASS,
                KdfParams::insecure_for_tests(),
                false,
            )
            .unwrap())
        })
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
    (devs, IdentityCard::encode(&list))
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
async fn world(cfg: Config) -> (String, Vec<Dev>, Vec<Dev>) {
    let url = relay(cfg).await;
    let (alice, ac) = person("alice", 2);
    let (bob, bc) = person("bob", 1);
    befriend(&alice, "bob", &bc);
    befriend(&bob, "alice", &ac);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    (url, alice, bob)
}
fn unread(d: &Dev) -> usize {
    ops::unread_counts(&d.0)
        .unwrap()
        .iter()
        .find(|(n, _)| n == "bob")
        .unwrap()
        .1
}
fn history(d: &Dev, peer: &Dev) -> Vec<(String, bool, bool)> {
    let key = IdentityCard::decode(&peer.0.identity_card().unwrap())
        .unwrap()
        .owner_hex();
    d.0.messages()
        .unwrap()
        .conversation(&key, 1000)
        .unwrap()
        .into_iter()
        .map(|m| (m.body, m.edited, m.deleted))
        .collect()
}

#[tokio::test]
async fn read_state_syncs_between_own_devices_and_only_moves_forward() {
    let (url, alice, bob) = world(Config::default()).await;
    for t in ["one", "two", "three"] {
        ops::send(&bob[0].0, &url, "alice", t.as_bytes())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    for d in &alice {
        recv(d, &url).await;
    }
    assert_eq!((unread(&alice[0]), unread(&alice[1])), (3, 3));

    // Reading on the phone clears it on the laptop too.
    assert_eq!(ops::mark_read(&alice[0].0, &url, "bob").await.unwrap(), 1);
    assert_eq!(unread(&alice[0]), 0);
    let got = recv(&alice[1], &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::ReadSynced { peer }] if peer == "bob"),
        "{got:?}"
    );
    assert_eq!(unread(&alice[1]), 0);

    // New mail is unread on both; the marker never moves backwards.
    ops::send(&bob[0].0, &url, "alice", b"four").await.unwrap();
    for d in &alice {
        recv(d, &url).await;
    }
    assert_eq!((unread(&alice[0]), unread(&alice[1])), (1, 1));
    let key = IdentityCard::decode(&bob[0].0.identity_card().unwrap())
        .unwrap()
        .owner_hex();
    let marker = alice[1].0.read_marker(&key).unwrap();
    assert!(!alice[1].0.advance_read_marker(&key, marker - 1).unwrap());
    assert_eq!(alice[1].0.read_marker(&key).unwrap(), marker);
}

#[tokio::test]
async fn a_contact_cannot_use_the_own_device_channel() {
    let (url, alice, bob) = world(Config::default()).await;
    // Bob hand-crafts every kind of sync message and sends it to Alice as if it were a message.
    let attacks = [
        SyncMsg::Read {
            peer: IdentityCard::decode(&bob[0].0.identity_card().unwrap())
                .unwrap()
                .owner_hex(),
            upto: u64::MAX,
        },
        SyncMsg::CatchUpRequest { since: 0 },
        SyncMsg::FileRequest {
            msg_id: "ab".repeat(16),
        },
    ];
    for a in attacks {
        ops::deliver_envelope(&bob[0].0, &url, "alice", a.to_envelope().unwrap())
            .await
            .unwrap();
    }
    for d in &alice {
        let got = recv(d, &url).await;
        assert!(!got.is_empty());
        for o in &got {
            if matches!(o, Outcome::SentCopy { .. }) {
                continue;
            }
            assert!(
                matches!(o, Outcome::Rejected { .. }),
                "a contact's sync message must be refused, got {o:?}"
            );
        }
    }
    // Nothing happened: no answer was triggered and no read marker moved.
    assert_eq!(
        alice[0]
            .0
            .read_marker(
                &IdentityCard::decode(&bob[0].0.identity_card().unwrap())
                    .unwrap()
                    .owner_hex()
            )
            .unwrap(),
        0
    );
    assert!(
        recv(&bob[0], &url).await.is_empty(),
        "nothing was sent back to the contact"
    );
}

#[tokio::test]
async fn a_device_that_missed_messages_edits_and_deletes_catches_up_from_another() {
    // Messages expire from the relay after 2 s, so the laptop misses everything while "away".
    let (url, alice, bob) = world(Config {
        ttl: Duration::from_millis(2000),
        ..Config::default()
    })
    .await;
    let (phone, laptop) = (&alice[0], &alice[1]);
    // Both collect once so the laptop has a real "last synced" time.
    recv(laptop, &url).await;

    let a = ops::send(&bob[0].0, &url, "alice", b"first")
        .await
        .unwrap()
        .id;
    let b = ops::send(&bob[0].0, &url, "alice", b"second (will be edited)")
        .await
        .unwrap()
        .id;
    let c = ops::send(&bob[0].0, &url, "alice", b"third (will be deleted)")
        .await
        .unwrap()
        .id;
    recv(phone, &url).await; // the phone is online and gets them
    ops::edit(&bob[0].0, &url, &b, "second (fixed)")
        .await
        .unwrap();
    ops::delete(&bob[0].0, &url, &c).await.unwrap();
    recv(phone, &url).await;
    assert_eq!(history(phone, &bob[0]).len(), 3);

    tokio::time::sleep(Duration::from_millis(2400)).await; // the laptop's copies expire at the relay
    assert!(
        recv(laptop, &url).await.is_empty(),
        "nothing left in the laptop's queue"
    );
    assert!(history(laptop, &bob[0]).is_empty());

    // Laptop asks; phone answers on its next receive; laptop collects the answer.
    assert_eq!(ops::request_catch_up(&laptop.0, &url).await.unwrap(), 1);
    let answered = recv(phone, &url).await;
    assert!(
        matches!(answered.as_slice(), [Outcome::SyncRequest { .. }]),
        "{answered:?}"
    );
    let got = recv(laptop, &url).await;
    assert!(
        got.iter()
            .any(|o| matches!(o, Outcome::CaughtUp { last: true, .. })),
        "{got:?}"
    );

    let want = vec![
        ("first".to_string(), false, false),
        ("second (fixed)".into(), true, false),
        ("".into(), false, true),
    ];
    assert_eq!(history(phone, &bob[0]), want);
    assert_eq!(
        history(laptop, &bob[0]),
        want,
        "the laptop converges to the phone, edits and tombstones included"
    );
    assert!(laptop.0.messages().unwrap().contains(&a).unwrap());

    // Asking again changes nothing (idempotent), and a deleted message stays deleted.
    ops::request_catch_up(&laptop.0, &url).await.unwrap();
    recv(phone, &url).await;
    let again = recv(laptop, &url).await;
    assert!(
        again
            .iter()
            .all(|o| !matches!(o, Outcome::CaughtUp { changed, .. } if *changed > 0)),
        "{again:?}"
    );
    assert_eq!(history(laptop, &bob[0]), want);
}

#[tokio::test]
async fn expired_attachment_is_re_supplied_by_another_own_device() {
    let (url, alice, bob) = world(Config {
        blob_ttl: Duration::from_secs(4),
        ..Config::default()
    })
    .await;
    let (phone, laptop) = (&alice[0], &alice[1]);
    let data = b"the signed contract".repeat(500);
    let id = ops::send_file(&bob[0].0, &url, "alice", "contract.pdf", &data, "")
        .await
        .unwrap()
        .id;
    recv(phone, &url).await;
    recv(laptop, &url).await;
    ops::fetch_attachment(&phone.0, &url, &id).await.unwrap(); // only the phone downloads it

    tokio::time::sleep(Duration::from_millis(4400)).await; // the relay expires the blob
    let e = ops::fetch_attachment(&laptop.0, &url, &id)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("no longer"), "{e}");

    ops::request_file(&laptop.0, &url, &id).await.unwrap();
    recv(phone, &url).await; // answers: re-encrypts and uploads a fresh copy
    let got = recv(laptop, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::FileReady { .. }]),
        "{got:?}"
    );
    let (att, bytes) = ops::fetch_attachment(&laptop.0, &url, &id).await.unwrap();
    assert_eq!(bytes, data);
    assert_eq!(att.name, "contract.pdf");
}

#[tokio::test]
async fn file_requests_are_not_answered_when_the_device_does_not_hold_the_file() {
    let (url, alice, bob) = world(Config::default()).await;
    let (phone, laptop) = (&alice[0], &alice[1]);
    let id = ops::send_file(&bob[0].0, &url, "alice", "x.bin", b"data", "")
        .await
        .unwrap()
        .id;
    recv(phone, &url).await;
    recv(laptop, &url).await;
    // Neither device ever downloaded it, so neither can answer.
    ops::request_file(&laptop.0, &url, &id).await.unwrap();
    recv(phone, &url).await;
    assert!(
        recv(laptop, &url).await.is_empty(),
        "no offer comes back from a device without the bytes"
    );
    // And a deleted message's file is never offered.
    ops::fetch_attachment(&phone.0, &url, &id).await.unwrap();
    ops::delete(&bob[0].0, &url, &id).await.unwrap();
    recv(phone, &url).await;
    ops::request_file(&laptop.0, &url, &id).await.ok();
    recv(phone, &url).await;
    assert!(!recv(laptop, &url)
        .await
        .iter()
        .any(|o| matches!(o, Outcome::FileReady { .. })));
}

#[tokio::test]
async fn a_file_offer_that_does_not_match_the_message_is_rejected() {
    let (url, alice, bob) = world(Config::default()).await;
    let (phone, laptop) = (&alice[0], &alice[1]);
    let id = ops::send_file(&bob[0].0, &url, "alice", "plan.pdf", b"the real plan", "")
        .await
        .unwrap()
        .id;
    recv(phone, &url).await;
    recv(laptop, &url).await;
    let before = laptop
        .0
        .messages()
        .unwrap()
        .get(&id)
        .unwrap()
        .unwrap()
        .attachment
        .unwrap();

    // A (compromised or buggy) own device offers a different file for the same message.
    let (fake, _chunks) =
        spike_core::attachment::seal_file("plan.pdf", b"a different file").unwrap();
    let offer = SyncMsg::FileOffer {
        msg_id: id.clone(),
        attachment: fake,
    };
    ops::deliver_envelope(&phone.0, &url, "bob", offer.to_envelope().unwrap())
        .await
        .unwrap();
    let got = recv(laptop, &url).await;
    assert!(
        got.iter()
            .any(|o| matches!(o, Outcome::Rejected { reason } if reason.contains("did not match"))),
        "{got:?}"
    );
    let after = laptop
        .0
        .messages()
        .unwrap()
        .get(&id)
        .unwrap()
        .unwrap()
        .attachment
        .unwrap();
    assert_eq!(before, after, "the message still points at the real file");
}
