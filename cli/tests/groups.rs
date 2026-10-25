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

// ----- invite decisions follow you to your other devices; block an inviter -----

/// Alice, and Eve with a laptop and a phone who have never heard of Alice. Alice knows Eve.
async fn alice_and_eve_with_two_devices() -> (String, Dev, Dev, Dev) {
    let url = relay().await;
    let alice = dev("alice", &url).await;
    let (laptop, phone) = two_devices("eve", &url).await;
    alice
        .0
        .add_contact("eve", &laptop.0.identity_card().unwrap(), false)
        .unwrap();
    (url, alice, laptop, phone)
}

fn invites(got: &[Outcome]) -> usize {
    got.iter()
        .filter(|o| matches!(o, Outcome::GroupInvite { .. }))
        .count()
}

fn settled(got: &[Outcome], word: &str) -> bool {
    got.iter()
        .any(|o| matches!(o, Outcome::GroupInviteSettled { decision, .. } if decision == word))
}

#[tokio::test]
async fn an_invite_decision_follows_you_to_your_other_devices() {
    use groups::InviteDecision::{Accept, Decline};
    let (url, alice, laptop, phone) = alice_and_eve_with_two_devices().await;
    let pilot = groups::create(
        &alice.0,
        url.as_str(),
        "Pilot",
        &["eve".into()],
        AddBy::Admins,
    )
    .await
    .unwrap()
    .0
    .group;
    let club = groups::create(
        &alice.0,
        url.as_str(),
        "Club",
        &["eve".into()],
        AddBy::Admins,
    )
    .await
    .unwrap()
    .0
    .group;
    for d in [&laptop, &phone] {
        let got = recv(d, &url).await;
        assert_eq!(invites(&got), 2, "both devices were invited twice: {got:?}");
    }

    // Accepting on the laptop tells the phone, which joins too and can read what follows.
    let told = groups::decide_invite(&laptop.0, url.as_str(), &pilot, Accept)
        .await
        .unwrap();
    assert_eq!(told, 1, "one other device was told");
    assert_eq!(
        groups::find(&laptop.0, &pilot).unwrap().status,
        Status::Joined
    );
    let got = recv(&phone, &url).await;
    assert!(settled(&got, "accepted"), "{got:?}");
    assert_eq!(
        groups::find(&phone.0, &pilot).unwrap().status,
        Status::Joined
    );
    groups::send(&alice.0, url.as_str(), "Pilot", "welcome, both")
        .await
        .unwrap();
    for d in [&laptop, &phone] {
        let got = recv(d, &url).await;
        assert_eq!(texts(&got).len(), 1, "{got:?}");
    }

    // Declining on the phone leaves the laptop out of the other group.
    groups::decide_invite(&phone.0, url.as_str(), &club, Decline)
        .await
        .unwrap();
    let got = recv(&laptop, &url).await;
    assert!(settled(&got, "declined"), "{got:?}");
    for d in [&laptop, &phone] {
        assert_eq!(groups::find(&d.0, &club).unwrap().status, Status::Left);
    }
    // Deciding twice is refused: the invite is gone.
    assert!(
        groups::decide_invite(&laptop.0, url.as_str(), &club, Accept)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_decision_that_arrives_before_its_invite_is_remembered() {
    use groups::InviteDecision::Accept;
    let (url, alice, laptop, phone) = alice_and_eve_with_two_devices().await;
    let gid = groups::create(
        &alice.0,
        url.as_str(),
        "Pilot",
        &["eve".into()],
        AddBy::Admins,
    )
    .await
    .unwrap()
    .0
    .group;
    assert_eq!(invites(&recv(&laptop, &url).await), 1);

    // The phone hears the decision before its own copy of the invite (say, over a faster relay).
    let o = groups::apply_invite_decision(&phone.0, &gid, Accept, None).unwrap();
    assert_eq!(o, Outcome::Ignored);
    assert!(groups::find(&phone.0, &gid).is_err(), "nothing to show yet");
    let got = recv(&phone, &url).await;
    assert!(
        settled(&got, "accepted"),
        "the invite settled on arrival: {got:?}"
    );
    assert_eq!(invites(&got), 0, "it never shows as an open invite");
    assert_eq!(groups::find(&phone.0, &gid).unwrap().status, Status::Joined);
}

#[tokio::test]
async fn blocking_an_inviter_blocks_them_on_every_device_and_drops_later_invites() {
    use groups::InviteDecision::Block;
    let (url, alice, laptop, phone) = alice_and_eve_with_two_devices().await;
    let pilot = groups::create(
        &alice.0,
        url.as_str(),
        "Pilot",
        &["eve".into()],
        AddBy::Admins,
    )
    .await
    .unwrap()
    .0
    .group;
    for d in [&laptop, &phone] {
        assert_eq!(invites(&recv(d, &url).await), 1);
    }

    let told = groups::decide_invite(&laptop.0, url.as_str(), &pilot, Block)
        .await
        .unwrap();
    assert_eq!(told, 1);
    assert_eq!(
        groups::find(&laptop.0, &pilot).unwrap().status,
        Status::Left
    );
    let got = recv(&phone, &url).await;
    assert!(settled(&got, "blocked"), "{got:?}");
    for d in [&laptop, &phone] {
        let blocked = requests::blocked(&d.0).unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].owner, owner(&alice));
        assert_eq!(groups::find(&d.0, &pilot).unwrap().status, Status::Left);
    }

    // A new group from the blocked person never shows up on either device.
    let club = groups::create(
        &alice.0,
        url.as_str(),
        "Club",
        &["eve".into()],
        AddBy::Admins,
    )
    .await
    .unwrap()
    .0
    .group;
    for d in [&laptop, &phone] {
        let got = recv(d, &url).await;
        assert_eq!(invites(&got), 0, "{got:?}");
        assert!(groups::find(&d.0, &club).is_err());
    }
}

#[tokio::test]
async fn only_a_stranger_can_be_blocked_from_an_invite() {
    use groups::InviteDecision::Block;
    let (url, alice, _bob, carol) = pilot().await;
    // Carol leaves and Alice (a contact) adds her back: an invite, but not from a stranger.
    groups::leave(&carol.0, url.as_str(), "Pilot")
        .await
        .unwrap();
    recv(&alice, &url).await;
    groups::add(&alice.0, url.as_str(), "Pilot", &["carol".into()])
        .await
        .unwrap();
    let got = recv(&carol, &url).await;
    assert_eq!(invites(&got), 1, "{got:?}");
    let e = groups::decide_invite(&carol.0, url.as_str(), "Pilot", Block)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not in your contacts"), "{e}");
    assert_eq!(
        groups::find(&carol.0, "Pilot").unwrap().status,
        Status::Invited
    );
    assert!(requests::blocked(&carol.0).unwrap().is_empty());
}

#[tokio::test]
async fn a_former_member_stays_known_but_cannot_write_privately_until_added() {
    let (url, _alice, bob, carol) = pilot().await;
    // Carol knows Bob only from the group; they have a session from his group messages.
    groups::send(&bob.0, url.as_str(), "Pilot", "hello from bob")
        .await
        .unwrap();
    assert_eq!(texts(&recv(&carol, &url).await).len(), 1);
    bob.0
        .add_contact("carol", &carol.0.identity_card().unwrap(), false)
        .unwrap();

    // Private messages from him are refused with a hint, in the group and after she leaves it;
    // they are not lost to a request she could not read (the old session lives in her main store).
    for round in 0..2 {
        if round == 1 {
            groups::leave(&carol.0, url.as_str(), "Pilot")
                .await
                .unwrap();
        }
        ops::send(&bob.0, url.as_str(), "carol", b"psst")
            .await
            .unwrap();
        let got = recv(&carol, &url).await;
        assert!(
            matches!(got.as_slice(), [Outcome::Rejected { .. }]),
            "round {round}: {got:?}"
        );
    }
    assert!(requests::list(&carol.0).unwrap().is_empty());

    // Adding him as a contact is what lets him write to her one to one.
    carol
        .0
        .add_contact("bob", &bob.0.identity_card().unwrap(), false)
        .unwrap();
    ops::send(&bob.0, url.as_str(), "carol", b"now it works")
        .await
        .unwrap();
    let got = recv(&carol, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Message { from, .. }] if from == "bob"),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_blocked_inviter_cannot_invite_you_again_even_if_you_share_a_group() {
    use groups::InviteDecision::{Accept, Block};
    let (url, alice, laptop, phone) = alice_and_eve_with_two_devices().await;
    let ids: Vec<String> = {
        let mut v = vec![];
        for name in ["Pilot", "Club"] {
            v.push(
                groups::create(&alice.0, url.as_str(), name, &["eve".into()], AddBy::Admins)
                    .await
                    .unwrap()
                    .0
                    .group,
            );
        }
        v
    };
    for d in [&laptop, &phone] {
        assert_eq!(invites(&recv(d, &url).await), 2);
    }
    // Eve joins Pilot (so Alice is a known member on both devices), then blocks Alice over Club.
    groups::decide_invite(&laptop.0, url.as_str(), &ids[0], Accept)
        .await
        .unwrap();
    groups::decide_invite(&laptop.0, url.as_str(), &ids[1], Block)
        .await
        .unwrap();
    let got = recv(&phone, &url).await;
    assert!(
        settled(&got, "accepted") && settled(&got, "blocked"),
        "{got:?}"
    );

    // A third group from Alice is dropped on both devices; Pilot, which Eve chose, still works.
    let third = groups::create(
        &alice.0,
        url.as_str(),
        "Third",
        &["eve".into()],
        AddBy::Admins,
    )
    .await
    .unwrap()
    .0
    .group;
    groups::send(&alice.0, url.as_str(), "Pilot", "still here")
        .await
        .unwrap();
    for d in [&laptop, &phone] {
        let got = recv(d, &url).await;
        assert_eq!(invites(&got), 0, "{got:?}");
        assert!(
            groups::find(&d.0, &third).is_err(),
            "no trace of the blocked invite"
        );
        assert_eq!(
            texts(&got).len(),
            1,
            "the group she joined keeps working: {got:?}"
        );
    }
}

#[tokio::test]
async fn blocking_an_inviter_also_clears_their_other_waiting_invites() {
    use groups::InviteDecision::Block;
    let (url, alice, laptop, phone) = alice_and_eve_with_two_devices().await;
    let mut ids = vec![];
    for name in ["One", "Two", "Three"] {
        ids.push(
            groups::create(&alice.0, url.as_str(), name, &["eve".into()], AddBy::Admins)
                .await
                .unwrap()
                .0
                .group,
        );
    }
    for d in [&laptop, &phone] {
        assert_eq!(invites(&recv(d, &url).await), 3);
    }
    groups::decide_invite(&laptop.0, url.as_str(), &ids[0], Block)
        .await
        .unwrap();
    recv(&phone, &url).await;
    for d in [&laptop, &phone] {
        for id in &ids {
            assert_eq!(
                groups::find(&d.0, id).unwrap().status,
                Status::Left,
                "every waiting invite from the blocked person is gone"
            );
        }
    }
}

// ----- files in groups, and the "last 7 days" option for new members -----

fn history_bodies(d: &Dev, group: &str) -> Vec<String> {
    groups::history(&d.0, group, 100)
        .unwrap()
        .1
        .into_iter()
        .map(|m| m.body)
        .collect()
}

fn group_history_outcome(got: &[Outcome]) -> Option<usize> {
    got.iter().find_map(|o| match o {
        Outcome::GroupHistory { added, .. } => Some(*added),
        _ => None,
    })
}

#[tokio::test]
async fn a_file_sent_to_a_group_reaches_every_member_and_is_checked() {
    let (url, alice, bob, carol) = pilot().await;
    // More than one 256 KiB chunk, so reassembly and the integrity check are exercised.
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    assert!(
        groups::send_file(&alice.0, url.as_str(), "Pilot", "empty.bin", &[], "")
            .await
            .is_err()
    );
    let sent = groups::send_file(
        &alice.0,
        url.as_str(),
        "Pilot",
        "plan.bin",
        &bytes,
        "the plan",
    )
    .await
    .unwrap();
    assert_eq!(sent.reached, 2, "one upload, one message per member device");
    assert_eq!(
        alice.0.load_attachment(&sent.id).unwrap().unwrap(),
        bytes,
        "sender keeps a copy"
    );

    for d in [&bob, &carol] {
        let got = recv(d, &url).await;
        let [Outcome::GroupMessage { id, body, .. }] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!(
            (id.as_str(), body.as_slice()),
            (sent.id.as_str(), &b"the plan"[..])
        );
        let m = d.0.messages().unwrap().get(id).unwrap().unwrap();
        assert_eq!(m.attachment.as_ref().unwrap().name, "plan.bin");
        let (att, got_bytes) = ops::fetch_attachment(&d.0, url.as_str(), id).await.unwrap();
        assert_eq!((att.name.as_str(), got_bytes), ("plan.bin", bytes.clone()));
    }

    // Bob sends one too; Carol, who does not know Bob, can fetch it.
    let from_bob = groups::send_file(
        &bob.0,
        url.as_str(),
        "Pilot",
        "notes.txt",
        b"bob's notes",
        "",
    )
    .await
    .unwrap();
    let got = recv(&carol, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::GroupMessage { .. }]),
        "{got:?}"
    );
    let (_, b) = ops::fetch_attachment(&carol.0, url.as_str(), &from_bob.id)
        .await
        .unwrap();
    assert_eq!(b, b"bob's notes");

    // Deleting the message removes the file on every device that kept it.
    ops::delete(&alice.0, url.as_str(), &sent.id).await.unwrap();
    recv(&bob, &url).await;
    assert!(bob.0.load_attachment(&sent.id).unwrap().is_none());
    assert!(alice.0.load_attachment(&sent.id).unwrap().is_none());
}

#[tokio::test]
async fn new_members_see_the_last_seven_days_when_the_group_says_so() {
    use spike_cli::messages::{Direction, StoredMessage};
    use spike_core::group::History;
    let url = relay().await;
    let (alice, bob, carol) = (
        dev("alice", &url).await,
        dev("bob", &url).await,
        dev("carol", &url).await,
    );
    pair(&alice, "bob", &bob, "alice");
    pair(&alice, "carol", &carol, "alice");
    let gid = groups::create_with(
        &alice.0,
        url.as_str(),
        "Pilot",
        &["bob".into()],
        AddBy::Admins,
        History::Week,
    )
    .await
    .unwrap()
    .0
    .group;
    recv(&bob, &url).await;

    groups::send(&alice.0, url.as_str(), &gid, "monday plan")
        .await
        .unwrap();
    recv(&bob, &url).await;
    groups::send(&bob.0, url.as_str(), &gid, "bob's reply")
        .await
        .unwrap();
    recv(&alice, &url).await;
    let gone = groups::send(&alice.0, url.as_str(), &gid, "to be deleted")
        .await
        .unwrap();
    recv(&bob, &url).await;
    ops::delete(&alice.0, url.as_str(), &gone.id).await.unwrap();
    recv(&bob, &url).await;
    let file = groups::send_file(
        &alice.0,
        url.as_str(),
        &gid,
        "old.txt",
        b"from last week",
        "",
    )
    .await
    .unwrap();
    recv(&bob, &url).await;
    // Something from nine days ago is outside the window.
    alice
        .0
        .messages()
        .unwrap()
        .insert(&StoredMessage {
            id: "ab".repeat(16),
            peer: gid.clone(),
            direction: Direction::Out,
            ts: 1,
            body: "ancient".into(),
            attachment: None,
            lamport: 1,
            stamp_device: "x".into(),
            edited: false,
            deleted: false,
            changed_at: 0,
            author: Some(owner(&alice)),
            shared_by: None,
            reply_to: None,
            reactions: Default::default(),
        })
        .unwrap();

    // Alice adds Carol. Carol asks Alice when she joins; Alice answers; Carol reads it.
    groups::add(&alice.0, url.as_str(), &gid, &["carol".into()])
        .await
        .unwrap();
    let got = recv(&carol, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::GroupJoined { .. }]),
        "{got:?}"
    );
    recv(&alice, &url).await;
    let got = recv(&carol, &url).await;
    assert_eq!(group_history_outcome(&got), Some(3), "{got:?}");

    let (_, shown) = groups::history(&carol.0, &gid, 100).unwrap();
    let bodies: Vec<&str> = shown.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(
        bodies,
        ["monday plan", "bob's reply", ""],
        "no deleted, no ancient: {bodies:?}"
    );
    assert!(shown
        .iter()
        .all(|m| m.shared_by.as_deref() == Some(owner(&alice).as_str())));
    assert_eq!(shown[0].author.as_deref(), Some(owner(&alice).as_str()));
    assert_eq!(
        shown[1].author.as_deref(),
        Some(owner(&bob).as_str()),
        "as claimed by the sharer"
    );
    // The file comes with a fresh copy on the relay, not the original upload.
    let (_, bytes) = ops::fetch_attachment(&carol.0, url.as_str(), &shown[2].id)
        .await
        .unwrap();
    assert_eq!(bytes, b"from last week");
    assert_eq!(shown[2].id, file.id);

    // She cannot ask again for a while, and nobody can push history she did not ask for.
    let mut again =
        Envelope::group_control(spike_core::envelope::Kind::GroupHistoryRequest, &gid, "{}")
            .unwrap();
    again.lamport = 1;
    ops::deliver_envelope(&carol.0, url.as_str(), "alice", again)
        .await
        .unwrap();
    recv(&alice, &url).await;
    let got = recv(&carol, &url).await;
    assert_eq!(
        group_history_outcome(&got),
        None,
        "answered once per day: {got:?}"
    );
}

#[tokio::test]
async fn history_is_not_shared_unless_the_group_says_so_and_never_unasked() {
    use spike_core::envelope::Kind;
    use spike_core::group::History;
    let (url, alice, bob, carol) = pilot().await; // History::Join
    groups::send(&alice.0, url.as_str(), "Pilot", "before dana")
        .await
        .unwrap();
    recv(&bob, &url).await;
    recv(&carol, &url).await;
    let dana = dev("dana", &url).await;
    pair(&alice, "dana", &dana, "alice");
    groups::add(&alice.0, url.as_str(), "Pilot", &["dana".into()])
        .await
        .unwrap();
    recv(&dana, &url).await;
    recv(&alice, &url).await;
    let got = recv(&dana, &url).await;
    assert_eq!(group_history_outcome(&got), None);
    assert!(
        history_bodies(&dana, "Pilot").is_empty(),
        "joined from now on"
    );

    // Alice (a member Dana trusts) sends a history Dana never asked for: dropped.
    let gid = groups::find(&dana.0, "Pilot").unwrap().id().to_string();
    let forged = serde_json::json!({ "messages": [{
        "id": "cd".repeat(16), "ts": 4_000_000_000_000u64, "author": owner(&bob),
        "body": "forged", "edited": false
    }]})
    .to_string();
    let env = Envelope::group_control(Kind::GroupHistory, &gid, &forged).unwrap();
    ops::deliver_envelope(&alice.0, url.as_str(), "dana", env)
        .await
        .unwrap();
    let got = recv(&dana, &url).await;
    assert_eq!(group_history_outcome(&got), None, "{got:?}");
    assert!(history_bodies(&dana, "Pilot").is_empty());

    // Switching the setting to the last 7 days is a group change every member sees.
    groups::set_history(&alice.0, url.as_str(), "Pilot", History::Week)
        .await
        .unwrap();
    recv(&bob, &url).await;
    assert_eq!(
        groups::find(&bob.0, "Pilot")
            .unwrap()
            .current()
            .unwrap()
            .history,
        History::Week
    );
    // Even when the group now offers it, Dana never asked (she joined before it did), so a history
    // pushed at her is refused: asking is what opens the door, and only for the person asked.
    recv(&dana, &url).await;
    assert_eq!(
        groups::find(&dana.0, "Pilot")
            .unwrap()
            .current()
            .unwrap()
            .history,
        History::Week
    );
    let env = Envelope::group_control(Kind::GroupHistory, &gid, &forged).unwrap();
    ops::deliver_envelope(&alice.0, url.as_str(), "dana", env)
        .await
        .unwrap();
    let got = recv(&dana, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { reason }] if reason.contains("did not ask")),
        "{got:?}"
    );
    assert!(history_bodies(&dana, "Pilot").is_empty());

    groups::set_history(&alice.0, url.as_str(), "Pilot", History::Join)
        .await
        .unwrap();
    assert!(
        groups::set_history(&bob.0, url.as_str(), "Pilot", History::Week)
            .await
            .is_err(),
        "admins only"
    );
}

#[tokio::test]
async fn accepting_an_invite_asks_for_the_history_the_group_offers() {
    use spike_core::group::History;
    let url = relay().await;
    let (alice, bob, eve) = (
        dev("alice", &url).await,
        dev("bob", &url).await,
        dev("eve", &url).await,
    );
    pair(&alice, "bob", &bob, "alice");
    alice
        .0
        .add_contact("eve", &eve.0.identity_card().unwrap(), false)
        .unwrap();
    let gid = groups::create_with(
        &alice.0,
        url.as_str(),
        "Pilot",
        &["bob".into()],
        AddBy::Admins,
        History::Week,
    )
    .await
    .unwrap()
    .0
    .group;
    recv(&bob, &url).await;
    groups::send(&alice.0, url.as_str(), &gid, "before eve")
        .await
        .unwrap();

    // Eve does not know Alice: an invite, nothing shown, nothing asked yet.
    groups::add(&alice.0, url.as_str(), &gid, &["eve".into()])
        .await
        .unwrap();
    assert_eq!(invites(&recv(&eve, &url).await), 1);
    recv(&alice, &url).await;
    assert!(group_history_outcome(&recv(&eve, &url).await).is_none());
    assert!(history_bodies(&eve, &gid).is_empty());

    // Accepting is what asks; the answer arrives; the earlier message shows, marked as shared.
    groups::decide_invite(&eve.0, url.as_str(), &gid, groups::InviteDecision::Accept)
        .await
        .unwrap();
    recv(&alice, &url).await;
    let got = recv(&eve, &url).await;
    assert_eq!(group_history_outcome(&got), Some(1), "{got:?}");
    assert_eq!(history_bodies(&eve, &gid), ["before eve"]);
}

#[tokio::test]
async fn group_members_reply_and_react_and_everyone_sees_it() {
    let (url, alice, bob, carol) = pilot().await;
    let r = groups::send(&bob.0, url.as_str(), "Pilot", "draft is up")
        .await
        .unwrap();
    recv(&alice, &url).await;
    recv(&carol, &url).await;
    // Carol (not Bob's contact) replies and reacts; anyone in the group may react to anything.
    let reply = groups::send_reply(&carol.0, url.as_str(), "Pilot", &r.id, "reading it now")
        .await
        .unwrap();
    ops::react(&carol.0, url.as_str(), &r.id, "👀")
        .await
        .unwrap();
    ops::react(&alice.0, url.as_str(), &r.id, "👍")
        .await
        .unwrap();
    for d in [&alice, &bob, &carol] {
        recv(d, &url).await;
        recv(d, &url).await;
        let m = d.0.messages().unwrap().get(&reply.id).unwrap().unwrap();
        assert_eq!(m.reply_to.as_deref(), Some(r.id.as_str()));
        let target = d.0.messages().unwrap().get(&r.id).unwrap().unwrap();
        let mut emojis: Vec<&str> = target
            .reactions
            .values()
            .map(|x| x.emoji.as_str())
            .collect();
        emojis.sort();
        assert_eq!(emojis, ["👀", "👍"]);
    }
}
