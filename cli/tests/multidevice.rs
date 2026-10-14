//! Milestone M3: every message reaches every device, over each device's own relay.

use spike_cli::{
    client::Relay,
    messages::Direction,
    ops,
    state::{Contact, Home},
    Outcome,
};
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase multidevice";
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
        std::env::temp_dir().join(format!("spike-md-{tag}-{n}")),
        PASS,
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap()
}

/// A person with several devices under one owner key. `relays[i]` is device i's
/// home relay (None = the default relay). Returns the devices and the person's
/// *first* card (v1, only device 0), which contacts may still hold.
fn person(tag: &str, relays: &[Option<&str>]) -> (Vec<Dev>, String) {
    let devs: Vec<Dev> = (0..relays.len())
        .map(|i| Dev(new_home(&format!("{tag}{i}"))))
        .collect();
    let first_card = devs[0].0.identity_card().unwrap();
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
    (devs, first_card)
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

fn texts(o: &[Outcome]) -> Vec<String> {
    o.iter()
        .map(|o| match o {
            Outcome::Message { from, body, .. } => {
                format!("from {from}: {}", String::from_utf8_lossy(body))
            }
            Outcome::SentCopy { to, body, .. } => {
                format!("copy to {to}: {}", String::from_utf8_lossy(body))
            }
            other => format!("{other:?}"),
        })
        .collect()
}

fn owner_of(d: &Dev) -> String {
    IdentityCard::decode(&d.0.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}

#[tokio::test]
async fn message_reaches_every_recipient_device_and_senders_other_devices() {
    let url = relay().await;
    let (alice, alice_v1) = person("alice", &[None, None]);
    let (bob, bob_v1) = person("bob", &[None, None]);
    // Contacts hold each other's *old* single-device cards: the relay directory must update them.
    befriend(&alice, "bob", &bob_v1);
    befriend(&bob, "alice", &alice_v1);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }

    let r = ops::send(&alice[0].0, &url, "bob", b"hello all your devices")
        .await
        .unwrap();
    assert_eq!(r.delivered(), 2, "{r:?}");
    assert_eq!(
        r.devices
            .iter()
            .filter(|d| d.own && d.error.is_none())
            .count(),
        1,
        "sent copy to Alice's laptop"
    );
    assert!(
        r.notes.iter().any(|n| n.contains("bob's devices changed")),
        "{:?}",
        r.notes
    );

    for b in &bob {
        assert_eq!(
            texts(&recv(b, &url).await),
            ["from alice: hello all your devices"]
        );
    }
    assert_eq!(
        texts(&recv(&alice[1], &url).await),
        ["copy to bob: hello all your devices"]
    );

    // Same message, same id, filed in the same conversation everywhere.
    let in_conv = |d: &Dev, peer: &Dev| {
        d.0.messages()
            .unwrap()
            .conversation(&owner_of(peer), 10)
            .unwrap()
    };
    let a2 = in_conv(&alice[1], &bob[0]);
    assert_eq!(
        (a2.len(), a2[0].direction, a2[0].id.as_str()),
        (1, Direction::Out, r.id.as_str())
    );
    assert_eq!(in_conv(&bob[1], &alice[0])[0].id, r.id);
}

#[tokio::test]
async fn reply_from_a_device_the_contact_has_not_seen_is_accepted_after_refresh() {
    let url = relay().await;
    let (alice, alice_v1) = person("alice", &[None, None]);
    let (bob, bob_v1) = person("bob", &[None]);
    befriend(&alice, "bob", &bob_v1);
    befriend(&bob, "alice", &alice_v1); // Bob only knows Alice's phone.
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    // Alice writes from her laptop, a device Bob has never seen.
    ops::send(&alice[1].0, &url, "bob", b"from my laptop")
        .await
        .unwrap();
    assert_eq!(
        texts(&recv(&bob[0], &url).await),
        ["from alice: from my laptop"]
    );
    let c = &bob[0].0.load_contacts().unwrap()[0];
    assert_eq!(
        c.identity().unwrap().devices().len(),
        2,
        "Bob learned Alice's laptop from the signed list"
    );
    assert!(
        c.verified,
        "verification of Alice carries over to her signed devices"
    );
}

#[tokio::test]
async fn removed_device_stops_receiving_and_cannot_send() {
    let url = relay().await;
    let (alice, alice_v1) = person("alice", &[None]);
    let (bob, bob_v1) = person("bob", &[None, None]);
    befriend(&alice, "bob", &bob_v1);
    befriend(&bob, "alice", &alice_v1);
    for d in alice.iter().chain(bob.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    // Bob removes his laptop from his phone and publishes the new list.
    let owner = bob[0].0.load_owner().unwrap();
    let laptop = bob[1].0.load_identity().unwrap().device_id();
    let v3 = bob[0]
        .0
        .load_device_list()
        .unwrap()
        .without_device(&owner, &laptop)
        .unwrap();
    bob[0].0.save_device_list(&v3).unwrap();
    ops::publish_keys(&bob[0].0, &url).await.unwrap();

    let r = ops::send(&alice[0].0, &url, "bob", b"phone only now")
        .await
        .unwrap();
    assert_eq!((r.delivered(), r.devices.len()), (1, 1), "{r:?}");
    assert!(
        recv(&bob[1], &url).await.is_empty(),
        "removed laptop receives nothing new"
    );
    let e = ops::send(&bob[1].0, &url, "alice", b"still here?")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("removed"), "{e}");
}

#[tokio::test]
async fn relay_directory_enforces_the_list_rules() {
    let url = relay().await;
    let (bob, _) = person("bob", &[None, None]);
    let owner = bob[0].0.load_owner().unwrap();
    let v2 = bob[0].0.load_device_list().unwrap();
    let me = bob[0].0.load_identity().unwrap();
    let mut r = Relay::connect(&url, &me).await.unwrap();
    assert!(r.fetch_device_list(&v2.owner).await.unwrap().is_none());
    assert_eq!(r.publish_device_list(&v2).await.unwrap(), 2);
    assert_eq!(
        r.publish_device_list(&v2).await.unwrap(),
        2,
        "republishing the same list is fine"
    );

    let older = DeviceList::sign(&owner, 1, v2.devices[..1].to_vec()).unwrap();
    assert!(r
        .publish_device_list(&older)
        .await
        .unwrap_err()
        .to_string()
        .contains("rollback"));
    let fork = DeviceList::sign(&owner, 2, v2.devices[..1].to_vec()).unwrap();
    assert!(r
        .publish_device_list(&fork)
        .await
        .unwrap_err()
        .to_string()
        .contains("equivocation"));

    // A device that is not in the list cannot publish it.
    let stranger = Identity::generate().unwrap();
    let mut s = Relay::connect(&url, &stranger).await.unwrap();
    let v3 = v2
        .with_device(
            &owner,
            DeviceEntry {
                card: Identity::generate().unwrap().card().encode(),
                relay: None,
            },
        )
        .unwrap();
    assert!(s
        .publish_device_list(&v3)
        .await
        .unwrap_err()
        .to_string()
        .contains("not_member"));
    // A forged signature is refused.
    let mut forged = v3.clone();
    forged.version = 7;
    assert!(r
        .publish_device_list(&forged)
        .await
        .unwrap_err()
        .to_string()
        .contains("bad_list"));

    assert_eq!(s.fetch_device_list(&v2.owner).await.unwrap().unwrap(), v2);
}

#[tokio::test]
async fn each_device_is_reached_on_its_own_home_relay() {
    let (r1, r2) = (relay().await, relay().await);
    let (alice, alice_v1) = person("alice", &[None]);
    let (bob, bob_v1) = person("bob", &[None, Some(&r2)]);
    befriend(&alice, "bob", &bob_v1);
    befriend(&bob, "alice", &alice_v1);
    ops::publish_keys(&alice[0].0, &r1).await.unwrap();
    ops::publish_keys(&bob[0].0, &r1).await.unwrap(); // publishes Bob's list (with B2 -> r2) on r1
    ops::publish_keys(&bob[1].0, &r2).await.unwrap(); // B2's prekeys live on its home relay

    let r = ops::send(&alice[0].0, &r1, "bob", b"two relays")
        .await
        .unwrap();
    assert_eq!(r.delivered(), 2, "{r:?}");
    assert!(
        recv(&bob[1], &r1).await.is_empty(),
        "nothing for B2 on the wrong relay"
    );
    assert_eq!(texts(&recv(&bob[1], &r2).await), ["from alice: two relays"]);
    assert_eq!(texts(&recv(&bob[0], &r1).await), ["from alice: two relays"]);
}

#[tokio::test]
async fn partial_delivery_is_reported_and_still_stored() {
    let url = relay().await;
    let (alice, alice_v1) = person("alice", &[None]);
    let (bob, bob_v1) = person("bob", &[None, None]);
    befriend(&alice, "bob", &bob_v1);
    befriend(&bob, "alice", &alice_v1);
    ops::publish_keys(&alice[0].0, &url).await.unwrap();
    ops::publish_keys(&bob[0].0, &url).await.unwrap(); // Bob's laptop never connected: no prekeys

    let r = ops::send(&alice[0].0, &url, "bob", b"some of you")
        .await
        .unwrap();
    assert_eq!(r.delivered(), 1);
    let failed: Vec<_> = r.devices.iter().filter(|d| d.error.is_some()).collect();
    assert_eq!(failed.len(), 1);
    assert!(failed[0].error.as_ref().unwrap().contains("no prekeys"));
    assert!(alice[0].0.messages().unwrap().contains(&r.id).unwrap());
}
