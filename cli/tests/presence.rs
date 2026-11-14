//! ADR 0037: read receipts, online now and typing, between two people over a real relay.

use spike_cli::{ops, presence, state::Contact, state::Home};
use spike_core::envelope::{Envelope, Status};
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase presence";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

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
                std::env::temp_dir().join(format!("spike-pr-{tag}{i}-{id}")),
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
fn befriend(devs: &[Dev], name: &str, card: &str, verified: bool) {
    for d in devs {
        let mut c = d.0.load_contacts().unwrap();
        c.push(Contact {
            name: name.into(),
            card: card.into(),
            verified,
            key_changed: None,
            verified_at: None,
            devices_changed: None,
        });
        d.0.save_contacts(&c).unwrap();
    }
}
async fn recv(d: &Dev, url: &str) {
    ops::recv(&d.0, url, IDLE, |_| {}).await.unwrap();
}
fn key(d: &Dev) -> String {
    IdentityCard::decode(&d.0.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}
fn now() -> u64 {
    spike_core::clock::now_ms()
}
fn all_on() -> presence::Share {
    presence::Share {
        receipts: true,
        online: true,
        typing: true,
    }
}

#[tokio::test]
async fn statuses_follow_both_switches_and_stay_between_two_people() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    let (alice, ac) = person("alice", 2);
    let (bob, bc) = person("bob", 1);
    befriend(&alice, "bob", &bc, true);
    befriend(&bob, "alice", &ac, false);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    let (a, b) = (&alice[0], &bob[0]);

    // Off by default: nothing is sent, nothing is shown.
    assert_eq!(presence::share(&a.0).unwrap(), presence::Share::default());
    assert!(!presence::send(&a.0, url.as_str().into(), "bob", Status::Read { upto: 5 }).await.unwrap());
    assert!(!presence::send(&a.0, url.as_str().into(), "bob", Status::Typing { on: true }).await.unwrap());

    // Alice shares everything; Bob shares nothing yet. Bob keeps none of it (reciprocal).
    presence::set_share(&a.0, all_on()).unwrap();
    assert!(presence::send(&a.0, url.as_str().into(), "bob", Status::Read { upto: now() }).await.unwrap());
    assert!(presence::send(&a.0, url.as_str().into(), "bob", Status::Typing { on: true }).await.unwrap());
    recv(b, &url).await;
    assert_eq!(presence::shown(&b.0, &key(a), now()).unwrap(), presence::Shown::default());

    // Both share: Bob sees Alice's receipt and typing, and Alice sees Bob's.
    presence::set_share(&b.0, all_on()).unwrap();
    let t1 = now();
    assert!(presence::send(&a.0, url.as_str().into(), "bob", Status::Read { upto: t1 }).await.unwrap());
    assert!(presence::send(&a.0, url.as_str().into(), "bob", Status::Typing { on: true }).await.unwrap());
    // Online goes to a verified contact only: Alice verified Bob, Bob did not verify Alice.
    assert!(presence::send(&a.0, url.as_str().into(), "bob", Status::Online).await.unwrap());
    assert!(!presence::send(&b.0, url.as_str().into(), "alice", Status::Online).await.unwrap());
    recv(b, &url).await;
    let seen = presence::shown(&b.0, &key(a), now()).unwrap();
    assert_eq!(seen.read_upto, Some(t1));
    assert!(seen.typing && seen.online);

    // The same read point is not told twice, and a read point never moves back.
    assert!(!presence::send(&a.0, url.as_str().into(), "bob", Status::Read { upto: t1 }).await.unwrap());
    presence::on_status(&b.0, &key(a), Status::Read { upto: 1 }, now(), now()).unwrap();
    assert_eq!(presence::shown(&b.0, &key(a), now()).unwrap().read_upto, Some(t1));

    // Typing goes stale by itself, and a message ends it.
    assert!(!presence::shown(&b.0, &key(a), now() + presence::TYPING_MS + 1).unwrap().typing);
    ops::send(&a.0, &url, "bob", b"done typing").await.unwrap();
    recv(b, &url).await;
    assert!(!presence::shown(&b.0, &key(a), now()).unwrap().typing);

    // An old online or typing signal (an offline phone collecting its queue) is not believed.
    let old = now() - 10 * 60_000;
    assert!(!presence::on_status(&b.0, &key(a), Status::Online, old, now()).unwrap());

    // Turning the switch off forgets what was kept.
    presence::set_share(&b.0, presence::Share { receipts: false, ..all_on() }).unwrap();
    assert_eq!(presence::shown(&b.0, &key(a), now()).unwrap().read_upto, None);

    // Alice's other device receives nothing of Bob's or Alice's statuses: they go to contacts only.
    presence::set_share(&alice[1].0, all_on()).unwrap();
    recv(&alice[1], &url).await;
    assert_eq!(presence::shown(&alice[1].0, &key(b), now()).unwrap(), presence::Shown::default());

    // A status never travels in a group, and a malformed one is refused by the envelope check.
    let mut e = Envelope::status(Status::Online).unwrap();
    e.group = Some("ab".repeat(16));
    assert!(Envelope::from_bytes(&e.to_bytes()).is_err());
    let mut e = Envelope::status(Status::Online).unwrap();
    e.body = "{\"read\":{\"upto\":\"soon\"}}".into();
    assert!(Envelope::from_bytes(&e.to_bytes()).is_err());
}
