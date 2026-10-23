//! Large groups (MLS, ADR 0025) from the command-line client, end to end on a real relay.

use spike_cli::large::{self, Event, Status};
use spike_cli::{ops, state::Home, Outcome};
use spike_core::identity::Identity;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase large";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

async fn relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

struct Dev(Home, PathBuf);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.1).ok();
    }
}

/// A person on `url` with pairwise keys and, unless `bare`, a few MLS key packages published.
async fn person(tag: &str, url: &str, bare: bool) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    let dir = std::env::temp_dir().join(format!("spike-large-{tag}-{n}"));
    let d = Dev(
        Home::create(&dir, PASS, KdfParams::insecure_for_tests(), false).unwrap(),
        dir,
    );
    ops::publish_keys(&d.0, url).await.unwrap();
    if !bare {
        assert_eq!(large::publish_key_packages(&d.0, url, 3).await.unwrap(), 3);
    }
    d
}
fn pair(a: &Dev, a_calls_b: &str, b: &Dev, b_calls_a: &str) {
    a.0.add_contact(a_calls_b, &b.0.identity_card().unwrap(), false)
        .unwrap();
    b.0.add_contact(b_calls_a, &a.0.identity_card().unwrap(), false)
        .unwrap();
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}
fn said(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Message { from, body, .. } => Some((from.clone(), body.clone())),
            _ => None,
        })
        .collect()
}

/// Alice created "Org" and added Bob and Carol, who accepted. Bob and Carol are not each other's
/// contacts.
async fn org() -> (String, Dev, Dev, Dev) {
    let url = relay().await;
    let (alice, bob, carol) = (
        person("alice", &url, true).await,
        person("bob", &url, false).await,
        person("carol", &url, false).await,
    );
    pair(&alice, "bob", &bob, "alice");
    pair(&alice, "carol", &carol, "alice");
    let rec = large::create(&alice.0, url.as_str(), "Org").await.unwrap();
    assert_eq!(rec.status, Status::Active);
    for who in ["bob", "carol"] {
        let r = large::add(&alice.0, url.as_str(), "Org", who)
            .await
            .unwrap();
        assert_eq!(r.devices, 1);
    }
    for d in [&bob, &carol] {
        let got = recv(d, &url).await;
        assert!(
            got.iter().any(|o| matches!(o, Outcome::LargeInvite { group, from } if group == "Org" && from == "alice")),
            "{got:?}"
        );
        large::accept(&d.0, url.as_str(), "Org").await.unwrap();
    }
    (url, alice, bob, carol)
}

#[tokio::test]
async fn people_who_are_not_contacts_talk_through_a_large_group() {
    let (url, alice, bob, carol) = org().await;
    // Catch up on the commits that happened before and after joining.
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    large::send(&alice.0, url.as_str(), "Org", "agenda is above")
        .await
        .unwrap();
    for d in [&bob, &carol] {
        let ev = large::sync(&d.0, url.as_str()).await.unwrap();
        assert_eq!(
            said(&ev),
            [("alice".to_string(), "agenda is above".to_string())]
        );
    }
    // Bob writes; Carol has never heard of Bob, but the relay holds his signed device list.
    large::send(&bob.0, url.as_str(), "Org", "thanks, read it")
        .await
        .unwrap();
    let ev = large::sync(&carol.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev).len(), 1, "{ev:?}");
    assert_eq!(said(&ev)[0].1, "thanks, read it");
    assert_ne!(said(&ev)[0].0, "alice");
    let ev = large::sync(&alice.0, url.as_str()).await.unwrap();
    assert_eq!(
        said(&ev),
        [("bob".to_string(), "thanks, read it".to_string())]
    );

    // Stored once, in order, for every member; one's own messages are there too.
    let (_, h) = large::history(&alice.0, "Org", 50).unwrap();
    let bodies: Vec<_> = h.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["agenda is above", "thanks, read it"]);
    // Running sync again changes nothing.
    assert!(said(&large::sync(&alice.0, url.as_str()).await.unwrap()).is_empty());
    assert_eq!(large::history(&alice.0, "Org", 50).unwrap().1.len(), 2);
}

#[tokio::test]
async fn an_invitation_does_nothing_until_it_is_accepted() {
    let url = relay().await;
    let (alice, bob) = (
        person("alice", &url, true).await,
        person("bob", &url, false).await,
    );
    pair(&alice, "bob", &bob, "alice");
    large::create(&alice.0, url.as_str(), "Org").await.unwrap();
    large::add(&alice.0, url.as_str(), "Org", "bob")
        .await
        .unwrap();

    let got = recv(&bob, &url).await;
    assert!(
        got.iter().any(|o| matches!(o, Outcome::LargeInvite { .. })),
        "{got:?}"
    );
    let rec = large::find(&bob.0, "Org").unwrap();
    assert_eq!(rec.status, Status::Invited);
    // Nothing is read or sent for a group not joined.
    assert!(large::sync(&bob.0, url.as_str()).await.unwrap().is_empty());
    assert!(large::send(&bob.0, url.as_str(), "Org", "hi")
        .await
        .is_err());

    large::decline(&bob.0, "Org").unwrap();
    assert!(large::find(&bob.0, "Org").is_err());
    assert!(large::accept(&bob.0, url.as_str(), "Org").await.is_err());
}

#[tokio::test]
async fn removing_someone_moves_the_group_and_she_is_left_behind() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    large::remove(&alice.0, url.as_str(), "Org", "carol")
        .await
        .unwrap();

    // Bob follows the group to its new mailbox and can still talk to Alice.
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert!(
        ev.iter().any(|e| matches!(e, Event::Changed { .. })),
        "{ev:?}"
    );
    large::send(&bob.0, url.as_str(), "Org", "just us")
        .await
        .unwrap();
    let ev = large::sync(&alice.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev), [("bob".to_string(), "just us".to_string())]);
    // And the other way round: Bob, who followed the rotation, reads from the start of the new mailbox.
    large::send(&alice.0, url.as_str(), "Org", "noted")
        .await
        .unwrap();
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev), [("alice".to_string(), "noted".to_string())]);

    // Carol finds out she is out, keeps what she had, and can neither send nor read on.
    let ev = large::sync(&carol.0, url.as_str()).await.unwrap();
    assert!(
        ev.iter().any(|e| matches!(e, Event::Removed { .. })),
        "{ev:?}"
    );
    assert_eq!(
        large::find(&carol.0, "Org").unwrap().status,
        Status::Removed
    );
    assert!(large::send(&carol.0, url.as_str(), "Org", "let me in")
        .await
        .is_err());
    assert!(large::sync(&carol.0, url.as_str())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn adding_someone_with_no_key_packages_says_what_to_do() {
    let url = relay().await;
    let (alice, bob) = (
        person("alice", &url, true).await,
        person("bob", &url, true).await,
    );
    pair(&alice, "bob", &bob, "alice");
    large::create(&alice.0, url.as_str(), "Org").await.unwrap();
    let e = large::add(&alice.0, url.as_str(), "Org", "bob")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("key packages"), "{e:#}");
    // Nothing was committed: the group is still just Alice.
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 1);

    large::publish_key_packages(&bob.0, url.as_str(), 1)
        .await
        .unwrap();
    large::add(&alice.0, url.as_str(), "Org", "bob")
        .await
        .unwrap();
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 2);
}

#[tokio::test]
async fn a_member_who_restarts_keeps_their_place() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    large::send(&alice.0, url.as_str(), "Org", "before")
        .await
        .unwrap();
    assert_eq!(
        said(&large::sync(&bob.0, url.as_str()).await.unwrap()).len(),
        1
    );

    // Unlock the same state again, as a new process would.
    let bob2 = Home::unlock(bob.0.path(), PASS).unwrap();
    large::send(&alice.0, url.as_str(), "Org", "after")
        .await
        .unwrap();
    let ev = large::sync(&bob2, url.as_str()).await.unwrap();
    assert_eq!(said(&ev), [("alice".to_string(), "after".to_string())]);
    large::send(&bob2, url.as_str(), "Org", "from the restarted one")
        .await
        .unwrap();
    let ev = large::sync(&alice.0, url.as_str()).await.unwrap();
    assert_eq!(
        said(&ev),
        [("bob".to_string(), "from the restarted one".to_string())]
    );
}
