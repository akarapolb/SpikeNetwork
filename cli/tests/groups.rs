//! Small groups over pairwise sessions (ADR 0024), end to end on a real relay.

use spike_cli::groups::{self, Status};
use spike_cli::{ops, requests, state::Home, Outcome};
use spike_core::envelope::Envelope;
use spike_core::group::AddBy;
use spike_core::identity::Identity;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase groups";
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
async fn dev(tag: &str, url: &str) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    let dir = std::env::temp_dir().join(format!("spike-grp-{tag}-{n}"));
    let d = Dev(
        Home::create(&dir, PASS, KdfParams::insecure_for_tests(), false).unwrap(),
        dir,
    );
    ops::publish_keys(&d.0, url).await.unwrap();
    d
}
fn owner(d: &Dev) -> String {
    d.0.load_device_list().unwrap().owner
}
/// `a` and `b` add each other as contacts.
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
fn texts(got: &[Outcome]) -> Vec<(String, String)> {
    got.iter()
        .filter_map(|o| match o {
            Outcome::GroupMessage { from, body, .. } => {
                Some((from.clone(), String::from_utf8_lossy(body).into_owned()))
            }
            _ => None,
        })
        .collect()
}
fn current_hash(d: &Dev, group: &str) -> String {
    groups::find(&d.0, group).unwrap().current().unwrap().hash()
}

/// Alice knows Bob and Carol; Bob and Carol do not know each other.
async fn pilot() -> (String, Dev, Dev, Dev) {
    let url = relay().await;
    let (alice, bob, carol) = (
        dev("alice", &url).await,
        dev("bob", &url).await,
        dev("carol", &url).await,
    );
    pair(&alice, "bob", &bob, "alice");
    pair(&alice, "carol", &carol, "alice");
    let (s, r) = groups::create(
        &alice.0,
        url.as_str(),
        "Pilot",
        &["bob".into(), "carol".into()],
        AddBy::Admins,
    )
    .await
    .unwrap();
    assert_eq!((s.members.len(), r.people, r.reached), (3, 2, 2));
    for d in [&bob, &carol] {
        let got = recv(d, &url).await;
        assert!(
            matches!(got.as_slice(), [Outcome::GroupJoined { group, by }] if group == "Pilot" && by == "alice"),
            "{got:?}"
        );
    }
    (url, alice, bob, carol)
}

#[tokio::test]
async fn members_who_are_not_contacts_talk_through_the_group() {
    let (url, alice, bob, carol) = pilot().await;
    let r = groups::send(&alice.0, url.as_str(), "Pilot", "agenda is above")
        .await
        .unwrap();
    assert_eq!((r.reached, r.delivered_devices()), (2, 2));
    assert_eq!(
        texts(&recv(&bob, &url).await),
        [("alice".into(), "agenda is above".into())]
    );
    assert_eq!(
        texts(&recv(&carol, &url).await),
        [("alice".into(), "agenda is above".into())]
    );

    // Bob writes to Carol, whom he has never met: her devices are fetched from the relay.
    let r = groups::send(&bob.0, url.as_str(), "Pilot", "hi carol, bob here")
        .await
        .unwrap();
    assert_eq!(r.reached, 2);
    assert!(!r.all_verified);
    let got = recv(&carol, &url).await;
    let t = texts(&got);
    assert_eq!(t.len(), 1, "{got:?}");
    assert!(
        t[0].0.starts_with('~'),
        "a member who is not a contact: {:?}",
        t[0]
    );
    assert_eq!(t[0].1, "hi carol, bob here");
    assert_eq!(texts(&recv(&alice, &url).await)[0].0, "bob");

    // Carol answers; nothing went to message requests and nobody became a contact.
    groups::send(&carol.0, url.as_str(), "Pilot", "hello bob")
        .await
        .unwrap();
    assert_eq!(texts(&recv(&bob, &url).await)[0].1, "hello bob");
    for d in [&bob, &carol] {
        assert!(requests::list(&d.0).unwrap().is_empty());
        assert_eq!(d.0.load_contacts().unwrap().len(), 1);
    }
    // Everyone stored the same three messages, each with its author.
    recv(&alice, &url).await;
    let (_, a) = groups::history(&alice.0, "Pilot", 50).unwrap();
    let (_, c) = groups::history(&carol.0, "Pilot", 50).unwrap();
    let ids = |v: &[spike_cli::messages::StoredMessage]| {
        let mut x: Vec<(String, Option<String>)> =
            v.iter().map(|m| (m.id.clone(), m.author.clone())).collect();
        x.sort();
        x
    };
    assert_eq!(a.len(), 3);
    assert_eq!(ids(&a), ids(&c));
    assert!(a
        .iter()
        .any(|m| m.author.as_deref() == Some(owner(&bob).as_str())));

    // A private message from a member who is not a contact is not accepted as one.
    let carol_card = carol.0.identity_card().unwrap();
    bob.0.add_contact("carol", &carol_card, false).unwrap();
    ops::send(&bob.0, url.as_str(), "carol", b"psst")
        .await
        .unwrap();
    let got = recv(&carol, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { .. }]),
        "{got:?}"
    );
}

#[tokio::test]
async fn removed_members_get_nothing_new_and_cannot_post() {
    let (url, alice, bob, carol) = pilot().await;
    // Carol keeps a copy of the state she was a member of, to try later.
    let old = current_hash(&carol, "Pilot");
    let (s, _) = groups::remove(&alice.0, url.as_str(), "Pilot", "carol")
        .await
        .unwrap();
    assert_eq!(s.members.len(), 2);
    let got = recv(&carol, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::GroupRemoved { by, .. }] if by == "alice"),
        "{got:?}"
    );
    assert_eq!(
        groups::find(&carol.0, "Pilot").unwrap().status,
        Status::Left
    );
    assert!(matches!(
        recv(&bob, &url).await.as_slice(),
        [Outcome::GroupChanged { version: 2, .. }]
    ));

    let r = groups::send(&alice.0, url.as_str(), "Pilot", "carol cannot read this")
        .await
        .unwrap();
    assert_eq!(r.people, 1);
    assert!(texts(&recv(&carol, &url).await).is_empty());
    assert!(
        groups::send(&carol.0, url.as_str(), "Pilot", "still here?")
            .await
            .is_err(),
        "her app refuses"
    );

    // A hand-made message naming the old member list is refused by the others.
    let mut env = Envelope::text("let me back in").unwrap();
    env.group = Some(s.group.clone());
    env.gstate = Some(old);
    ops::deliver_envelope(&carol.0, url.as_str(), "alice", env)
        .await
        .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { .. }]),
        "{got:?}"
    );
    let (_, msgs) = groups::history(&alice.0, "Pilot", 50).unwrap();
    assert!(msgs.iter().all(|m| m.body != "let me back in"));
}

#[tokio::test]
async fn concurrent_admin_changes_converge_everywhere() {
    let (url, alice, bob, carol) = pilot().await;
    groups::make_admin(&alice.0, url.as_str(), "Pilot", "bob")
        .await
        .unwrap();
    recv(&bob, &url).await;
    recv(&carol, &url).await;
    // Both admins change version 2 before hearing from each other.
    let id = groups::find(&alice.0, "Pilot").unwrap().id().to_string();
    groups::rename(&alice.0, url.as_str(), &id, "Pilot team")
        .await
        .unwrap();
    groups::set_add_by(&bob.0, url.as_str(), &id, AddBy::Any)
        .await
        .unwrap();
    for _ in 0..2 {
        for d in [&alice, &bob, &carol] {
            recv(d, &url).await;
        }
    }
    let h = current_hash(&alice, &id);
    assert_eq!(current_hash(&bob, &id), h);
    assert_eq!(current_hash(&carol, &id), h);
    let s = groups::find(&carol.0, &id).unwrap().current().unwrap();
    assert_eq!(s.version, 3, "one of the two changes won");

    // The loser's next message names a state that lost: it is still shown, marked, and the
    // sender gets the winning state back.
    let winner_is_alice = s.name == "Pilot team";
    let loser = if winner_is_alice { &bob } else { &alice };
    // Put the loser back on its own branch, as if it had not heard yet.
    let mut rec = groups::find(&loser.0, &id).unwrap();
    let mut chain = rec.chain().unwrap();
    let theirs = chain
        .after(2)
        .into_iter()
        .find(|x| x.hash() != s.hash())
        .unwrap();
    rec.states = chain
        .states()
        .into_iter()
        .filter(|x| x.hash() != s.hash())
        .collect();
    chain = spike_core::group::Chain::from_states(rec.states.clone()).unwrap();
    assert_eq!(chain.current(), &theirs);
    loser.0.save_json("groups.enc", &vec![rec]).unwrap();
    groups::send(&loser.0, url.as_str(), &id, "sent from the losing side")
        .await
        .unwrap();
    let got = recv(&carol, &url).await;
    assert!(
        got.iter().any(|o| matches!(
            o,
            Outcome::GroupMessage {
                out_of_date: true,
                ..
            }
        )),
        "{got:?}"
    );
    recv(loser, &url).await;
    assert_eq!(current_hash(loser, &id), h, "the loser caught up");
}

#[tokio::test]
async fn an_add_by_someone_who_is_not_a_contact_is_an_invite() {
    let (url, alice, bob, _carol) = pilot().await;
    // Erin is Alice's contact, but Erin has never heard of Alice.
    let erin = dev("erin", &url).await;
    alice
        .0
        .add_contact("erin", &erin.0.identity_card().unwrap(), false)
        .unwrap();
    groups::add(&alice.0, url.as_str(), "Pilot", &["erin".into()])
        .await
        .unwrap();
    let got = recv(&erin, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::GroupInvite { group, .. }] if group == "Pilot"),
        "{got:?}"
    );
    assert!(
        requests::list(&erin.0).unwrap().is_empty(),
        "not a message request"
    );
    assert_eq!(
        groups::find(&erin.0, "Pilot").unwrap().status,
        Status::Invited
    );
    assert!(groups::send(&erin.0, url.as_str(), "Pilot", "hi")
        .await
        .is_err());

    // Nothing from the group is shown before she accepts.
    recv(&bob, &url).await;
    groups::send(&alice.0, url.as_str(), "Pilot", "before you accepted")
        .await
        .unwrap();
    assert!(texts(&recv(&erin, &url).await).is_empty());

    groups::accept(&erin.0, "Pilot").unwrap();
    groups::send(&alice.0, url.as_str(), "Pilot", "welcome erin")
        .await
        .unwrap();
    groups::send(&bob.0, url.as_str(), "Pilot", "hi erin, bob")
        .await
        .unwrap();
    let got = texts(&recv(&erin, &url).await);
    let bodies: Vec<&str> = got.iter().map(|(_, b)| b.as_str()).collect();
    assert_eq!(bodies, ["welcome erin", "hi erin, bob"], "{got:?}");
    assert!(got.iter().all(|(from, _)| from.starts_with('~')));
    groups::send(&erin.0, url.as_str(), "Pilot", "thanks")
        .await
        .unwrap();
    let got = texts(&recv(&alice, &url).await);
    assert_eq!(got.last().unwrap().1, "thanks", "{got:?}");
}

#[tokio::test]
async fn declining_an_invite_keeps_you_out_and_leaving_needs_a_new_invite() {
    let (url, alice, bob, carol) = pilot().await;
    // Carol leaves; Alice adds her back: that is an invite now, even from a contact.
    groups::leave(&carol.0, url.as_str(), "Pilot")
        .await
        .unwrap();
    assert!(matches!(
        recv(&alice, &url).await.as_slice(),
        [Outcome::GroupChanged { .. }]
    ));
    recv(&bob, &url).await;
    groups::add(&alice.0, url.as_str(), "Pilot", &["carol".into()])
        .await
        .unwrap();
    let got = recv(&carol, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::GroupInvite { .. }]),
        "{got:?}"
    );
    groups::decline(&carol.0, "Pilot").unwrap();
    groups::send(&alice.0, url.as_str(), "Pilot", "are you there?")
        .await
        .unwrap();
    assert!(texts(&recv(&carol, &url).await).is_empty());
    assert_eq!(
        groups::find(&carol.0, "Pilot").unwrap().status,
        Status::Left
    );
}

#[tokio::test]
async fn only_the_author_can_edit_or_delete_a_group_message() {
    let (url, alice, bob, carol) = pilot().await;
    let r = groups::send(&bob.0, url.as_str(), "Pilot", "draft")
        .await
        .unwrap();
    recv(&alice, &url).await;
    recv(&carol, &url).await;
    ops::edit(&bob.0, url.as_str(), &r.id, "final")
        .await
        .unwrap();
    let got = recv(&carol, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Edited { who, .. }] if who.starts_with('~')),
        "{got:?}"
    );
    let m = carol.0.messages().unwrap().get(&r.id).unwrap().unwrap();
    assert_eq!((m.body.as_str(), m.edited), ("final", true));

    // Carol tries to delete Bob's message by sending a delete to the group.
    let theirs = carol.0.messages().unwrap().get(&r.id).unwrap().unwrap();
    groups::send_change(
        &carol.0,
        url.as_str().into(),
        &theirs,
        Envelope::delete(&r.id).unwrap(),
    )
    .await
    .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        matches!(
            got.as_slice(),
            [Outcome::Edited { .. }, Outcome::Rejected { .. }]
        ),
        "{got:?}"
    );
    assert!(
        !alice
            .0
            .messages()
            .unwrap()
            .get(&r.id)
            .unwrap()
            .unwrap()
            .deleted
    );

    ops::delete(&bob.0, url.as_str(), &r.id).await.unwrap();
    recv(&alice, &url).await;
    assert!(
        alice
            .0
            .messages()
            .unwrap()
            .get(&r.id)
            .unwrap()
            .unwrap()
            .deleted
    );
}

#[tokio::test]
async fn a_message_naming_a_state_we_missed_waits_for_it() {
    let (url, alice, bob, carol) = pilot().await;
    groups::rename(&alice.0, url.as_str(), "Pilot", "Pilot 2")
        .await
        .unwrap();
    recv(&carol, &url).await;
    recv(&bob, &url).await;
    // Bob loses version 2 (as if it never reached him).
    let mut rec = groups::find(&bob.0, "Pilot 2").unwrap();
    rec.states.retain(|s| s.version == 1);
    bob.0.save_json("groups.enc", &vec![rec]).unwrap();

    groups::send(&carol.0, url.as_str(), "Pilot 2", "named with version 2")
        .await
        .unwrap();
    // Bob holds it and asks Carol for the state.
    assert!(texts(&recv(&bob, &url).await).is_empty());
    // Carol answers when she next receives; then Bob shows the waiting message.
    recv(&carol, &url).await;
    let got = recv(&bob, &url).await;
    assert!(
        got.iter().any(|o| matches!(
            o,
            Outcome::GroupChanged {
                version: 2,
                released: 1,
                ..
            }
        )),
        "{got:?}"
    );
    let (_, msgs) = groups::history(&bob.0, "Pilot 2", 50).unwrap();
    assert!(msgs.iter().any(|m| m.body == "named with version 2"));
}

#[tokio::test]
async fn a_delete_for_a_message_you_never_had_leaves_no_visible_trace() {
    let (url, alice, bob, _carol) = pilot().await;
    let old = groups::send(&alice.0, url.as_str(), "Pilot", "sent before erin joined")
        .await
        .unwrap();
    recv(&bob, &url).await;
    let erin = dev("erin", &url).await;
    pair(&alice, "erin", &erin, "alice");
    groups::add(&alice.0, url.as_str(), "Pilot", &["erin".into()])
        .await
        .unwrap();
    assert!(matches!(
        recv(&erin, &url).await.as_slice(),
        [Outcome::GroupJoined { .. }]
    ));

    // Alice deletes it: Erin never had it, so her app keeps only a marker that stops a late copy.
    ops::delete(&alice.0, url.as_str(), &old.id).await.unwrap();
    let got = recv(&erin, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Deleted { .. }]),
        "{got:?}"
    );
    let marker = erin.0.messages().unwrap().get(&old.id).unwrap().unwrap();
    assert!(marker.deleted && groups::is_ghost(&marker));
    assert!(groups::history(&erin.0, "Pilot", 50).unwrap().1.is_empty());

    // Real messages after it show normally, and a deleted message she did have is still listed.
    let keep = groups::send(&alice.0, url.as_str(), "Pilot", "welcome erin")
        .await
        .unwrap();
    assert_eq!(texts(&recv(&erin, &url).await).len(), 1);
    ops::delete(&alice.0, url.as_str(), &keep.id).await.unwrap();
    recv(&erin, &url).await;
    let (_, shown) = groups::history(&erin.0, "Pilot", 50).unwrap();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].deleted && !groups::is_ghost(&shown[0]),
        "a known message shows as deleted"
    );
}

#[tokio::test]
async fn a_device_linked_later_gets_your_groups_and_can_use_them() {
    use spike_cli::link::{self, History};
    let (url, alice, bob, carol) = pilot().await;
    groups::send(&bob.0, url.as_str(), "Pilot", "before the laptop existed")
        .await
        .unwrap();
    recv(&alice, &url).await;
    recv(&carol, &url).await; // Carol keeps Bob as a member who is not a contact

    // Alice links a new laptop, bringing all history.
    let laptop = dev("alice-laptop", &url).await;
    let t = Duration::from_secs(5);
    let w = link::offer(&laptop.0, url.as_str()).unwrap();
    let new_side = async {
        let s = link::await_hello(&laptop.0, &w, t).await.unwrap();
        link::receive(&laptop.0, &w, &s, t).await.unwrap()
    };
    let old_side = async {
        let p = link::begin(&alice.0, url.as_str(), &w.code).await.unwrap();
        link::finish(&alice.0, url.as_str(), p, History::All)
            .await
            .unwrap()
    };
    let (installed, _linked) = tokio::join!(new_side, old_side);
    assert_eq!(
        installed.messages, 1,
        "the group message came with the history"
    );

    // The laptop knows the group, its history with authors, and can write to it as Alice.
    let rec = groups::find(&laptop.0, "Pilot").unwrap();
    assert_eq!(rec.status, Status::Joined);
    assert_eq!(rec.current().unwrap().members.len(), 3);
    let (_, hist) = groups::history(&laptop.0, "Pilot", 50).unwrap();
    assert!(hist
        .iter()
        .any(|m| m.body == "before the laptop existed" && m.author.is_some()));
    groups::send(
        &laptop.0,
        url.as_str(),
        "Pilot",
        "hello from the new laptop",
    )
    .await
    .unwrap();
    assert_eq!(texts(&recv(&bob, &url).await).last().unwrap().0, "alice");
    // Bob answers; the laptop (which learned Bob's card from the bundle) can read it.
    groups::send(&bob.0, url.as_str(), "Pilot", "welcome, laptop")
        .await
        .unwrap();
    let got = texts(&recv(&laptop, &url).await);
    assert_eq!(got.last().unwrap().1, "welcome, laptop", "{got:?}");

    // An invite that waits for you stays on the device that holds its session.
    let erin = dev("erin", &url).await;
    alice
        .0
        .add_contact("erin", &erin.0.identity_card().unwrap(), false)
        .unwrap();
    groups::add(&alice.0, url.as_str(), "Pilot", &["erin".into()])
        .await
        .unwrap();
    recv(&erin, &url).await;
    let (recs, _) = groups::export_for_link(&erin.0).unwrap();
    assert!(recs.is_empty(), "an unaccepted invite is not exported");
}

/// A person with a laptop and a phone under one owner key.
async fn two_devices(tag: &str, url: &str) -> (Dev, Dev) {
    let (laptop, phone) = (
        dev(&format!("{tag}-laptop"), url).await,
        dev(&format!("{tag}-phone"), url).await,
    );
    let owner = laptop.0.load_owner().unwrap();
    let entries = [&laptop, &phone]
        .iter()
        .map(|d| spike_core::owner::DeviceEntry {
            card: d.0.load_identity().unwrap().card().encode(),
            relay: None,
        })
        .collect();
    let list = spike_core::owner::DeviceList::sign(&owner, 2, entries).unwrap();
    for d in [&laptop, &phone] {
        d.0.install_identity(&owner, &list).unwrap();
        ops::publish_keys(&d.0, url).await.unwrap();
    }
    (laptop, phone)
}

#[tokio::test]
async fn catching_up_from_your_other_device_accepts_group_messages() {
    let url = relay().await;
    let (laptop, phone) = two_devices("alice", &url).await;
    let bob = dev("bob", &url).await;
    for d in [&laptop, &phone] {
        pair(d, "bob", &bob, "alice");
    }
    groups::create(
        &laptop.0,
        url.as_str(),
        "Family",
        &["bob".into()],
        AddBy::Admins,
    )
    .await
    .unwrap();
    recv(&bob, &url).await;
    groups::send(&bob.0, url.as_str(), "Family", "dinner at 7?")
        .await
        .unwrap();
    // Alongside an ordinary chat message, so one batch carries both kinds of record.
    ops::send(&laptop.0, url.as_str(), "bob", b"and a private word")
        .await
        .unwrap();
    recv(&laptop, &url).await;

    // The phone asks the laptop for what it missed; the laptop answers when it next receives.
    ops::request_catch_up(&phone.0, url.as_str()).await.unwrap();
    recv(&laptop, &url).await;
    let got = recv(&phone, &url).await;
    assert!(
        got.iter().any(|o| matches!(o, Outcome::CaughtUp { .. })),
        "the batch was accepted: {got:?}"
    );
    assert!(
        !got.iter().any(|o| matches!(o, Outcome::Invalid)),
        "{got:?}"
    );
    let (_, hist) = groups::history(&phone.0, "Family", 50).unwrap();
    assert!(hist.iter().any(|m| m.body == "dinner at 7?"));
}

#[tokio::test]
async fn a_group_follows_you_to_your_other_devices() {
    let url = relay().await;
    let (laptop, phone) = two_devices("alice", &url).await;
    let bob = dev("bob", &url).await;
    for d in [&laptop, &phone] {
        pair(d, "bob", &bob, "alice");
    }
    groups::create(
        &laptop.0,
        url.as_str(),
        "Family",
        &["bob".into()],
        AddBy::Admins,
    )
    .await
    .unwrap();
    let got = recv(&phone, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::GroupJoined { by, .. }] if by == "you"),
        "{got:?}"
    );
    recv(&bob, &url).await;

    groups::send(&bob.0, url.as_str(), "Family", "dinner at 7?")
        .await
        .unwrap();
    for d in [&laptop, &phone] {
        assert_eq!(
            texts(&recv(d, &url).await),
            [("bob".into(), "dinner at 7?".into())]
        );
    }
    // Reading it on the laptop clears it on the phone too.
    let unread = |d: &Dev| groups::unread_counts(&d.0).unwrap()[0].1;
    assert_eq!((unread(&laptop), unread(&phone)), (1, 1));
    groups::mark_read(&laptop.0, url.as_str(), "Family")
        .await
        .unwrap();
    assert_eq!(unread(&laptop), 0);
    let got = recv(&phone, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::ReadSynced { .. }]),
        "{got:?}"
    );
    assert_eq!(unread(&phone), 0);

    let r = groups::send(&laptop.0, url.as_str(), "Family", "yes")
        .await
        .unwrap();
    assert_eq!(
        r.devices.iter().filter(|d| d.own).count(),
        1,
        "a sent copy for the phone"
    );
    let got = recv(&phone, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::SentCopy { to, .. }] if to == "Family"),
        "{got:?}"
    );
    assert_eq!(
        texts(&recv(&bob, &url).await),
        [("alice".into(), "yes".into())]
    );
    // The phone can write to the group too, as the same person.
    groups::send(&phone.0, url.as_str(), "Family", "from my phone")
        .await
        .unwrap();
    assert_eq!(
        texts(&recv(&bob, &url).await),
        [("alice".into(), "from my phone".into())]
    );
}
