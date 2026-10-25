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

    large::decline(&bob.0, url.as_str(), "Org").await.unwrap();
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

fn owner_of(d: &Dev) -> String {
    d.0.load_device_list().unwrap().owner
}

#[tokio::test]
async fn an_admin_renames_the_group_and_everyone_sees_the_new_name() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    // Only an admin may.
    assert!(large::rename(&bob.0, url.as_str(), "Org", "Mine now")
        .await
        .is_err());
    large::rename(&alice.0, url.as_str(), "Org", "Board")
        .await
        .unwrap();
    assert_eq!(large::find(&alice.0, "Board").unwrap().name, "Board");
    for d in [&bob, &carol] {
        let ev = large::sync(&d.0, url.as_str()).await.unwrap();
        assert!(
            ev.iter().any(|e| matches!(e, Event::Changed { .. })),
            "{ev:?}"
        );
        assert_eq!(large::find(&d.0, "Board").unwrap().name, "Board");
        assert!(large::find(&d.0, "Org").is_err(), "the old name is gone");
    }
    // The name is part of the group's signed policy, so a person who joins later gets it too.
    assert!(large::rename(&alice.0, url.as_str(), "Board", "  ")
        .await
        .is_err());
    assert!(
        large::rename(&alice.0, url.as_str(), "Board", &"x".repeat(101))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_second_admin_can_add_people() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let dave = person("dave", &url, false).await;
    pair(&bob, "dave", &dave, "bob");
    // Bob is not an admin: he cannot add Dave.
    assert!(large::add(&bob.0, url.as_str(), "Org", "dave")
        .await
        .is_err());

    large::make_admin(&alice.0, url.as_str(), "Org", &owner_of(&bob))
        .await
        .unwrap();
    // Bob learns it by syncing, and then may add.
    large::sync(&bob.0, url.as_str()).await.unwrap();
    let members = large::members(&bob.0, "Org").unwrap();
    assert!(members.iter().filter(|m| m.admin).count() == 2);
    large::add(&bob.0, url.as_str(), "Org", "dave")
        .await
        .unwrap();
    let got = recv(&dave, &url).await;
    assert!(
        got.iter()
            .any(|o| matches!(o, Outcome::LargeInvite { from, .. } if from == "bob")),
        "{got:?}"
    );

    // Making someone who is not in the group an admin is refused.
    assert!(
        large::make_admin(&alice.0, url.as_str(), "Org", &"ab".repeat(32))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_group_can_let_any_member_add_people() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let dave = person("dave", &url, false).await;
    pair(&bob, "dave", &dave, "bob");
    assert!(large::add(&bob.0, url.as_str(), "Org", "dave")
        .await
        .is_err());
    large::set_members_can_add(&alice.0, url.as_str(), "Org", true)
        .await
        .unwrap();
    large::sync(&bob.0, url.as_str()).await.unwrap();
    large::add(&bob.0, url.as_str(), "Org", "dave")
        .await
        .unwrap();
    // Not anyone may remove, though.
    large::sync(&carol.0, url.as_str()).await.unwrap();
    assert!(large::remove(&carol.0, url.as_str(), "Org", "alice")
        .await
        .is_err());
}

#[tokio::test]
async fn a_commit_every_member_refuses_is_skipped_recorded_and_the_group_goes_on() {
    use spike_cli::large_testing;
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    // Bob is not an admin; he puts a commit adding Dave in the mailbox anyway.
    let dave = person("dave", &url, false).await;
    let kp = large_testing::key_package(&dave.0).unwrap();
    let seq = large_testing::append_unchecked_add(&bob.0, url.as_str(), "Org", &kp)
        .await
        .unwrap();

    for d in [&alice, &carol] {
        let ev = large::sync(&d.0, url.as_str()).await.unwrap();
        let refused: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                Event::Refused { seq, why, .. } => Some((*seq, why.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(refused.len(), 1, "{ev:?}");
        assert_eq!(refused[0].0, seq);
        // Nobody announced Dave to the group, so the commit is refused before its authority is
        // even looked at (the authority rules have their own tests in spike-mls).
        assert!(
            refused[0].1.contains("no valid device list for this owner"),
            "{refused:?}"
        );
        // It is on the record, so the app can say so later, and nobody was added.
        let rec = large::find(&d.0, "Org").unwrap();
        assert_eq!(
            (rec.refused_total, rec.refused.len(), rec.members),
            (1, 1, 3)
        );
    }
    // Skipped, not stuck: the group carries on, and syncing again does not report it again.
    large::send(&alice.0, url.as_str(), "Org", "still here")
        .await
        .unwrap();
    let ev = large::sync(&carol.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev), [("alice".to_string(), "still here".to_string())]);
    assert!(!ev.iter().any(|e| matches!(e, Event::Refused { .. })));
    assert_eq!(large::find(&carol.0, "Org").unwrap().refused_total, 1);
    // And a real change after it still works, from the same state.
    let erin = person("erin", &url, false).await;
    pair(&alice, "erin", &erin, "alice");
    large::add(&alice.0, url.as_str(), "Org", "erin")
        .await
        .unwrap();
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 4);
}

#[tokio::test]
async fn a_member_leaves_and_an_admin_removes_them_on_the_next_sync() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    large::send(&carol.0, url.as_str(), "Org", "before I go")
        .await
        .unwrap();
    large::leave(&carol.0, url.as_str(), "Org").await.unwrap();
    // She is out here at once, keeps her history, and nothing more is read or sent for her.
    assert_eq!(large::find(&carol.0, "Org").unwrap().status, Status::Left);
    assert!(large::send(&carol.0, url.as_str(), "Org", "again")
        .await
        .is_err());
    assert!(large::sync(&carol.0, url.as_str())
        .await
        .unwrap()
        .is_empty());
    assert_eq!(large::history(&carol.0, "Org", 50).unwrap().1.len(), 1);

    // An admin who reads the notice removes her: the group moves to a mailbox she cannot reach.
    let ev = large::sync(&alice.0, url.as_str()).await.unwrap();
    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::Left { who, .. } if who == "carol")),
        "{ev:?}"
    );
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 2);
    assert!(
        large::find(&alice.0, "Org")
            .unwrap()
            .pending_leavers
            .is_empty(),
        "done, so no longer carried"
    );
    // The notice is not a message: nobody's history shows it.
    assert!(!large::history(&alice.0, "Org", 50)
        .unwrap()
        .1
        .iter()
        .any(|m| m.body == "{}"));
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::Changed { rotated: true, .. })),
        "{ev:?}"
    );
    large::send(&bob.0, url.as_str(), "Org", "two of us")
        .await
        .unwrap();
    let ev = large::sync(&alice.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev), [("bob".to_string(), "two of us".to_string())]);
}

#[tokio::test]
async fn the_only_admin_cannot_leave_until_there_is_another() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let e = large::leave(&alice.0, url.as_str(), "Org")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("only admin"), "{e:#}");
    assert_eq!(large::find(&alice.0, "Org").unwrap().status, Status::Active);

    large::make_admin(&alice.0, url.as_str(), "Org", &owner_of(&bob))
        .await
        .unwrap();
    large::leave(&alice.0, url.as_str(), "Org").await.unwrap();
    // Bob, now an admin, removes her when he next reads.
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::Left { who, .. } if who == "alice")),
        "{ev:?}"
    );
    assert_eq!(large::find(&bob.0, "Org").unwrap().members, 2);
}

#[tokio::test]
async fn someone_whose_key_packages_ran_out_can_still_be_added_with_the_reusable_one() {
    let url = relay().await;
    let (alice, bob) = (
        person("alice", &url, true).await,
        person("bob", &url, true).await,
    );
    pair(&alice, "bob", &bob, "alice");
    // Bob published only his reusable fallback: no one-time packages at all.
    assert_eq!(
        large::publish_key_packages(&bob.0, url.as_str(), 0)
            .await
            .unwrap(),
        0
    );

    // Used in two different groups, and noted in the report as the weaker path.
    for name in ["One", "Two"] {
        large::create(&alice.0, url.as_str(), name).await.unwrap();
        let r = large::add(&alice.0, url.as_str(), name, "bob")
            .await
            .unwrap();
        assert!(r.used_last_resort);
        let got = recv(&bob, &url).await;
        assert!(
            got.iter()
                .any(|o| matches!(o, Outcome::LargeInvite { group, .. } if group == name)),
            "{got:?}"
        );
        large::accept(&bob.0, url.as_str(), name).await.unwrap();
    }
    large::send(&alice.0, url.as_str(), "Two", "second group works")
        .await
        .unwrap();
    large::send(&alice.0, url.as_str(), "One", "and the first")
        .await
        .unwrap();
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev).len(), 2, "{ev:?}");

    // With a one-time package published, that is used first and the report says so.
    large::publish_key_packages(&bob.0, url.as_str(), 2)
        .await
        .unwrap();
    large::create(&alice.0, url.as_str(), "Three")
        .await
        .unwrap();
    let r = large::add(&alice.0, url.as_str(), "Three", "bob")
        .await
        .unwrap();
    assert!(!r.used_last_resort);
}

// --- a person with more than one device --------------------------------------------------------

use spike_cli::client::Relay;
use spike_cli::link::{self, History};

const T: Duration = Duration::from_secs(5);

/// Link `new` to `existing`: both screens show the same code and a person confirms it.
async fn link_devices(existing: &Dev, new: &Dev, url: &str) {
    let w = link::offer(&new.0, url).unwrap();
    let new_side = async {
        let s = link::await_hello(&new.0, &w, T).await.unwrap();
        let code = s.safety_code.clone();
        link::receive(&new.0, &w, &s, T).await.unwrap();
        code
    };
    let existing_side = async {
        let p = link::begin(&existing.0, url, &w.code).await.unwrap();
        let code = p.safety_code.clone();
        link::finish(&existing.0, url, p, History::None)
            .await
            .unwrap();
        code
    };
    let (a, b) = tokio::join!(new_side, existing_side);
    assert_eq!(a, b);
}

/// A new device of `of`'s, ready to be added to groups: keys published, packages published.
async fn second_device(of: &Dev, tag: &str, url: &str) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    let dir = std::env::temp_dir().join(format!("spike-large-{tag}-{n}"));
    let d = Dev(
        Home::create(&dir, PASS, KdfParams::insecure_for_tests(), false).unwrap(),
        dir,
    );
    link_devices(of, &d, url).await;
    ops::publish_keys(&d.0, url).await.unwrap();
    large::publish_key_packages(&d.0, url, 3).await.unwrap();
    d
}

#[tokio::test]
async fn a_device_linked_later_joins_the_groups_its_person_is_already_in() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let laptop = second_device(&alice, "laptop", &url).await;
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 3);

    // Alice's phone notices the new device in her signed list and adds it; nobody has to ask.
    let ev = large::sync(&alice.0, url.as_str()).await.unwrap();
    assert!(ev.iter().any(|e| matches!(e, Event::OwnDevices)), "{ev:?}");
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 4);
    // The laptop is told, joins without asking (same person), and is a full member.
    recv(&laptop, &url).await;
    assert_eq!(
        large::find(&laptop.0, "Org").unwrap().status,
        Status::Invited
    );
    large::sync(&laptop.0, url.as_str()).await.unwrap();
    assert_eq!(
        large::find(&laptop.0, "Org").unwrap().status,
        Status::Active
    );

    large::send(&laptop.0, url.as_str(), "Org", "from my laptop")
        .await
        .unwrap();
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert_eq!(
        said(&ev),
        [("alice".to_string(), "from my laptop".to_string())]
    );
    large::send(&bob.0, url.as_str(), "Org", "got it")
        .await
        .unwrap();
    let ev = large::sync(&laptop.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev), [("bob".to_string(), "got it".to_string())]);
    // Others see four devices but still three people.
    large::sync(&carol.0, url.as_str()).await.unwrap();
    let people = large::members(&carol.0, "Org").unwrap();
    assert_eq!(
        (
            people.len(),
            people.iter().map(|m| m.devices).sum::<usize>()
        ),
        (3, 4)
    );
}

#[tokio::test]
async fn a_revoked_device_is_taken_out_of_the_groups() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let laptop = second_device(&alice, "laptop", &url).await;
    large::sync(&alice.0, url.as_str()).await.unwrap();
    recv(&laptop, &url).await;
    large::sync(&laptop.0, url.as_str()).await.unwrap();
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 4);

    // The laptop is lost: Alice revokes it from her phone.
    let laptop_id = laptop.0.load_identity().unwrap().device_id();
    let owner = alice.0.load_owner().unwrap();
    let revoked = alice
        .0
        .load_device_list()
        .unwrap()
        .without_device(&owner, &laptop_id)
        .unwrap();
    alice.0.save_device_list(&revoked).unwrap();
    let me = alice.0.load_identity().unwrap();
    Relay::connect(url.as_str(), &me)
        .await
        .unwrap()
        .publish_device_list(&revoked)
        .await
        .unwrap();

    // Her phone takes it out of every group on its next sync; the group moves to a new mailbox.
    let ev = large::sync(&alice.0, url.as_str()).await.unwrap();
    assert!(ev.iter().any(|e| matches!(e, Event::OwnDevices)), "{ev:?}");
    assert_eq!(large::find(&alice.0, "Org").unwrap().members, 3);
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::Changed { rotated: true, .. })),
        "{ev:?}"
    );
    // The lost laptop learns it is out and reads nothing more.
    let ev = large::sync(&laptop.0, url.as_str()).await.unwrap();
    assert!(
        ev.iter().any(|e| matches!(e, Event::Removed { .. })),
        "{ev:?}"
    );
    large::send(&alice.0, url.as_str(), "Org", "after revoking")
        .await
        .unwrap();
    assert!(said(&large::sync(&laptop.0, url.as_str()).await.unwrap()).is_empty());
}

#[tokio::test]
async fn an_invitation_accepted_on_one_device_is_settled_on_the_other() {
    let url = relay().await;
    let (alice, bob) = (
        person("alice", &url, true).await,
        person("bob", &url, false).await,
    );
    pair(&alice, "bob", &bob, "alice");
    let phone2 = second_device(&bob, "bob2", &url).await;
    // Alice learns of Bob's second device (his signed list is on the relay).
    large::create(&alice.0, url.as_str(), "Org").await.unwrap();
    ops::refresh_contacts(
        &mut Relay::connect(url.as_str(), &alice.0.load_identity().unwrap())
            .await
            .unwrap(),
        &alice.0,
        None,
    )
    .await
    .unwrap();
    let r = large::add(&alice.0, url.as_str(), "Org", "bob")
        .await
        .unwrap();
    assert_eq!(r.devices, 2, "both of Bob's devices are added");

    for d in [&bob, &phone2] {
        let got = recv(d, &url).await;
        assert!(
            got.iter().any(|o| matches!(o, Outcome::LargeInvite { .. })),
            "{got:?}"
        );
    }
    // He accepts on one device; the other follows without asking.
    large::accept(&bob.0, url.as_str(), "Org").await.unwrap();
    recv(&phone2, &url).await;
    large::sync(&phone2.0, url.as_str()).await.unwrap();
    assert_eq!(
        large::find(&phone2.0, "Org").unwrap().status,
        Status::Active
    );

    // And the same for a refusal.
    large::create(&alice.0, url.as_str(), "Two").await.unwrap();
    large::add(&alice.0, url.as_str(), "Two", "bob")
        .await
        .unwrap();
    for d in [&bob, &phone2] {
        recv(d, &url).await;
    }
    large::decline(&bob.0, url.as_str(), "Two").await.unwrap();
    recv(&phone2, &url).await;
    large::sync(&phone2.0, url.as_str()).await.unwrap();
    assert!(large::find(&phone2.0, "Two").is_err());
}

#[tokio::test]
async fn the_relay_pushes_a_new_entry_to_a_member_who_is_listening() {
    use spike_cli::client::Incoming;
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let subs = large::subscriptions(&bob.0, &url).unwrap();
    assert_eq!(subs.len(), 1);
    assert!(
        large::subscriptions(&bob.0, "ws://somewhere.else")
            .unwrap()
            .is_empty(),
        "only groups on the relay we are listening to"
    );
    let me = bob.0.load_identity().unwrap();
    let mut r = Relay::connect(url.as_str(), &me).await.unwrap();
    for (id, key) in &subs {
        r.group_subscribe(key, id).await.unwrap();
    }
    assert!(r
        .next_incoming(Some(Duration::from_millis(300)))
        .await
        .unwrap()
        .is_none());

    large::send(&alice.0, url.as_str(), "Org", "push me")
        .await
        .unwrap();
    let got = r.next_incoming(Some(Duration::from_secs(3))).await.unwrap();
    assert!(matches!(got, Some(Incoming::Group(_))), "{got:?}");
    drop(r);
    // The push is only a nudge; reading it is a sync.
    let ev = large::sync(&bob.0, url.as_str()).await.unwrap();
    assert_eq!(said(&ev), [("alice".to_string(), "push me".to_string())]);
}

#[tokio::test]
async fn large_group_replies_and_reactions_reach_everyone() {
    let (url, alice, bob, carol) = org().await;
    for d in [&alice, &bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let first = large::send(&alice.0, url.as_str(), "Org", "agenda is above")
        .await
        .unwrap();
    for d in [&bob, &carol] {
        large::sync(&d.0, url.as_str()).await.unwrap();
    }
    let reply = large::send_reply(&bob.0, url.as_str(), "Org", &first, "read it")
        .await
        .unwrap();
    ops::react(&carol.0, url.as_str(), &first, "🙌")
        .await
        .unwrap();
    for d in [&alice, &bob, &carol] {
        let ev = large::sync(&d.0, url.as_str()).await.unwrap();
        assert!(
            !ev.iter().any(|e| matches!(e, Event::Refused { .. })),
            "nothing refused: {ev:?}"
        );
        let (_, h) = large::history(&d.0, "Org", 50).unwrap();
        // A reaction is not a message: only the two texts are in the history.
        assert_eq!(h.len(), 2, "{h:?}");
        let r = h.iter().find(|m| m.id == reply).unwrap();
        assert_eq!(r.reply_to.as_deref(), Some(first.as_str()));
        let f = h.iter().find(|m| m.id == first).unwrap();
        assert_eq!(
            f.reactions
                .values()
                .map(|x| x.emoji.as_str())
                .collect::<Vec<_>>(),
            ["🙌"]
        );
    }
}
