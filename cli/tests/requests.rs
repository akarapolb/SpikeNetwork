//! Message requests (ADR 0021): strangers who introduce themselves wait for your decision.

use spike_cli::client::Relay;
use spike_cli::requests;
use spike_cli::{ops, state::Home, Outcome};
use spike_core::envelope::Envelope;
use spike_core::identity::{DeviceCard, Identity};
use spike_core::session::Sessions;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase requests";
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
fn dev(tag: &str) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    let dir = std::env::temp_dir().join(format!("spike-req-{tag}-{n}"));
    Dev(
        Home::create(&dir, PASS, KdfParams::insecure_for_tests(), false).unwrap(),
        dir,
    )
}
fn card(d: &Dev) -> String {
    d.0.identity_card().unwrap()
}
/// `a` adds `b` as a contact; `b` knows nothing about `a`.
fn one_way(a: &Dev, name: &str, b: &Dev) {
    a.0.add_contact(name, &card(b), false).unwrap();
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}
async fn setup() -> (String, Dev, Dev) {
    let url = relay().await;
    let (alice, bob) = (dev("alice"), dev("bob"));
    ops::publish_keys(&alice.0, &url).await.unwrap();
    ops::publish_keys(&bob.0, &url).await.unwrap();
    one_way(&alice, "bob", &bob);
    (url, alice, bob)
}
fn only_request(got: &[Outcome]) -> (&str, usize, bool) {
    match got {
        [Outcome::MessageRequest { id, held, new, .. }] => (id.as_str(), *held, *new),
        other => panic!("expected one message request, got {other:?}"),
    }
}

/// A message from a stranger with a hand-made introduction (or none), built below the app's own
/// sender so a hostile one can be tried.
async fn forge(url: &str, to: &Dev, intro: Option<String>, text: &str) {
    let target: DeviceCard = to.0.load_identity().unwrap().card();
    let mallory = Identity::generate().unwrap();
    let mut relay = Relay::connect(url, &mallory).await.unwrap();
    let prekey = relay
        .claim_prekey(&target.device_id())
        .await
        .unwrap()
        .expect("the target published prekeys");
    let mut sessions = Sessions::default();
    sessions.start_outbound(&mallory, &target, &prekey).unwrap();
    let mut env = Envelope::text(text).unwrap();
    env.intro = intro;
    let blob = sessions.encrypt(&target, &env.to_bytes()).unwrap();
    relay.send(&target.device_id(), &blob).await.unwrap();
}

#[tokio::test]
async fn an_introduced_stranger_becomes_a_request_not_a_contact() {
    let (url, alice, bob) = setup().await;
    ops::send(
        &alice.0,
        url.as_str(),
        "bob",
        b"hello, we met at the site visit",
    )
    .await
    .unwrap();
    let got = recv(&bob, &url).await;
    let (id, held, new) = only_request(&got);
    assert_eq!((held, new), (1, true));

    // Nothing else changed: no contact, no history, no unread, nothing in the main session store.
    assert!(bob.0.load_contacts().unwrap().is_empty());
    assert!(bob.0.messages().unwrap().all().unwrap().is_empty());
    let list = requests::list(&bob.0).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].key, id);
    // The fingerprint shown is the real one from Alice's validated card.
    let alice_card = spike_core::owner::IdentityCard::decode(&card(&alice)).unwrap();
    assert_eq!(list[0].fingerprint, alice_card.fingerprint());
    assert_eq!(list[0].held[0].body, "hello, we met at the site visit");
    // The stranger is told nothing: Alice sees no receipt or reply of any kind.
    assert!(recv(&alice, &url).await.is_empty());
}

#[tokio::test]
async fn follow_ups_join_the_same_request_up_to_the_cap() {
    let (url, alice, bob) = setup().await;
    for i in 1..=7 {
        ops::send(
            &alice.0,
            url.as_str(),
            "bob",
            format!("message {i}").as_bytes(),
        )
        .await
        .unwrap();
    }
    let got = recv(&bob, &url).await;
    assert_eq!(got.len(), 7);
    assert!(got
        .iter()
        .all(|o| matches!(o, Outcome::MessageRequest { .. })));
    let list = requests::list(&bob.0).unwrap();
    assert_eq!(list.len(), 1, "one request, not seven");
    assert_eq!(
        list[0].held.len(),
        requests::MAX_HELD,
        "only the first {} are kept",
        requests::MAX_HELD
    );
    assert_eq!(list[0].held[0].body, "message 1");
}

#[tokio::test]
async fn accepting_makes_a_contact_adopts_the_session_and_keeps_the_conversation() {
    let (url, alice, bob) = setup().await;
    ops::send(&alice.0, url.as_str(), "bob", b"first")
        .await
        .unwrap();
    ops::send(&alice.0, url.as_str(), "bob", b"second")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    let id = only_request(&got[..1]).0.to_string();

    // Bad names and short ids are refused; nothing changes.
    assert!(requests::accept(&bob.0, &id[..4], "Alice").is_err());
    assert!(requests::accept(&bob.0, &id, "  ").is_err());
    assert_eq!(requests::accept(&bob.0, &id, "Alice").unwrap(), 2);

    // She is a contact, but not a verified one; the held messages are the start of the conversation.
    let contacts = bob.0.load_contacts().unwrap();
    assert_eq!(
        (
            contacts.len(),
            contacts[0].name.as_str(),
            contacts[0].verified
        ),
        (1, "Alice", false)
    );
    let peer = contacts[0].conversation_key().unwrap();
    let hist = bob.0.messages().unwrap().conversation(&peer, 10).unwrap();
    assert_eq!(
        hist.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(requests::list(&bob.0).unwrap().is_empty());

    // The conversation works both ways with the adopted session.
    ops::send(&bob.0, url.as_str(), "Alice", b"nice to meet you")
        .await
        .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Message { body, .. }] if body == b"nice to meet you"),
        "{got:?}"
    );
    ops::send(&alice.0, url.as_str(), "bob", b"likewise")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Message { from, verified: false, body, .. }] if from == "Alice" && body == b"likewise"),
        "{got:?}"
    );

    // Accepting twice, or accepting someone already in your contacts, is refused.
    assert!(requests::accept(&bob.0, &id, "Alice2").is_err());
}

#[tokio::test]
async fn declining_forgets_but_does_not_block() {
    let (url, alice, bob) = setup().await;
    ops::send(&alice.0, url.as_str(), "bob", b"hi")
        .await
        .unwrap();
    let id = only_request(&recv(&bob, &url).await).0.to_string();
    requests::decline(&bob.0, &id).unwrap();
    assert!(requests::list(&bob.0).unwrap().is_empty());
    assert!(bob.0.load_contacts().unwrap().is_empty());
    assert!(requests::decline(&bob.0, &id).is_err(), "already gone");

    // Declining deleted the session keys, so the sender's next message in that same session cannot be
    // read at all: it is dropped as undecryptable. A fresh request would need them to start a new session.
    assert!(
        requests::blocked(&bob.0).unwrap().is_empty(),
        "declined is not blocked"
    );
    ops::send(&alice.0, url.as_str(), "bob", b"hi again")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    assert!(matches!(got.as_slice(), [Outcome::Invalid]), "{got:?}");
    assert!(
        requests::list(&bob.0).unwrap().is_empty() && bob.0.load_contacts().unwrap().is_empty()
    );

    // Someone else, who was never declined, can still write: nothing about this was a block.
    let carol = dev("carol");
    ops::publish_keys(&carol.0, &url).await.unwrap();
    one_way(&carol, "bob", &bob);
    ops::send(&carol.0, url.as_str(), "bob", b"hello from carol")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::MessageRequest { new: true, .. }]),
        "{got:?}"
    );
}

#[tokio::test]
async fn blocking_drops_everything_from_them_before_any_session_exists() {
    let (url, alice, bob) = setup().await;
    ops::send(&alice.0, url.as_str(), "bob", b"hi")
        .await
        .unwrap();
    let id = only_request(&recv(&bob, &url).await).0.to_string();
    requests::block(&bob.0, &id).unwrap();
    assert!(requests::list(&bob.0).unwrap().is_empty());
    let blocked = requests::blocked(&bob.0).unwrap();
    assert_eq!(blocked.len(), 1);

    // Anything more is dropped on arrival: no request, no contact. (The relay still carried it.)
    ops::send(&alice.0, url.as_str(), "bob", b"are you there?")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    assert!(matches!(got.as_slice(), [Outcome::Ignored]), "{got:?}");
    assert!(
        requests::list(&bob.0).unwrap().is_empty() && bob.0.load_contacts().unwrap().is_empty()
    );

    // Unblocking removes the entry, and a second unblock finds nothing. (Her old session is gone for
    // good, so her next message in it still cannot be read; only a new session would make a new request.)
    requests::unblock(&bob.0, &blocked[0].owner).unwrap();
    assert!(requests::blocked(&bob.0).unwrap().is_empty());
    assert!(requests::unblock(&bob.0, &blocked[0].owner).is_err());
    ops::send(&alice.0, url.as_str(), "bob", b"sorry about before")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    assert!(matches!(got.as_slice(), [Outcome::Invalid]), "{got:?}");
}

#[tokio::test]
async fn a_missing_or_stolen_introduction_is_dropped_not_held() {
    let (url, alice, bob) = setup().await;
    // No introduction at all.
    forge(&url, &bob, None, "who am I").await;
    let got = recv(&bob, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::UnknownSender { .. }]),
        "{got:?}"
    );

    // Mallory pastes ALICE's real, validly signed card: it does not list Mallory's device, so it fails.
    forge(&url, &bob, Some(card(&alice)), "I am Alice, really").await;
    let got = recv(&bob, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::UnknownSender { .. }]),
        "{got:?}"
    );

    // Garbage in the introduction field.
    forge(&url, &bob, Some("spike2:not-a-card".into()), "hi").await;
    let got = recv(&bob, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::UnknownSender { .. }]),
        "{got:?}"
    );

    assert!(
        requests::list(&bob.0).unwrap().is_empty(),
        "none of them became a request"
    );
    assert!(bob.0.load_contacts().unwrap().is_empty());
}

#[tokio::test]
async fn files_from_a_stranger_are_never_kept() {
    let (url, alice, bob) = setup().await;
    ops::send_file(
        &alice.0,
        url.as_str(),
        "bob",
        "invoice.pdf.exe",
        b"MZ not really",
        "please open",
    )
    .await
    .unwrap();
    let got = recv(&bob, &url).await;
    let (id, ..) = only_request(&got);
    let req = &requests::list(&bob.0).unwrap()[0];
    assert_eq!(req.held[0].body, "please open");
    assert_eq!(
        req.held[0].file.as_deref(),
        Some("invoice.pdf.exe"),
        "only the name is remembered"
    );
    let id = id.to_string();
    requests::accept(&bob.0, &id, "Alice").unwrap();
    let peer = bob.0.load_contacts().unwrap()[0]
        .conversation_key()
        .unwrap();
    let m = &bob.0.messages().unwrap().conversation(&peer, 5).unwrap()[0];
    assert!(
        m.attachment.is_none(),
        "no file descriptor reaches the history"
    );
    assert!(m.body.contains("not kept"), "{}", m.body);
}

#[tokio::test]
async fn undecided_requests_expire() {
    let (url, alice, bob) = setup().await;
    ops::send(&alice.0, url.as_str(), "bob", b"hi")
        .await
        .unwrap();
    recv(&bob, &url).await;
    let mut all = requests::list(&bob.0).unwrap();
    all[0].received = 1; // a very long time ago
    bob.0.save_json("requests.enc", &all).unwrap();
    assert!(requests::list(&bob.0).unwrap().is_empty());
    assert!(
        bob.0.load_quarantine().unwrap().peers().is_empty(),
        "the quarantined session went with it"
    );
}

#[tokio::test]
async fn people_you_already_know_are_unaffected() {
    let url = relay().await;
    let (alice, bob) = (dev("alice"), dev("bob"));
    ops::publish_keys(&alice.0, &url).await.unwrap();
    ops::publish_keys(&bob.0, &url).await.unwrap();
    one_way(&alice, "bob", &bob);
    one_way(&bob, "alice", &alice);
    ops::send(&alice.0, url.as_str(), "bob", b"hello")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Message { from, .. }] if from == "alice"),
        "{got:?}"
    );
    assert!(requests::list(&bob.0).unwrap().is_empty());
}
