//! The desktop app's logic against a real relay, without a window.

use spike_cli::{agent, ops};
use spike_core::identity::Identity;
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use spike_desktop::api::{Session, MIN_PASSPHRASE_CHARS};
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "desktop test passphrase";
const IDLE: Duration = Duration::from_millis(300);

async fn relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

struct Dev {
    dir: PathBuf,
    s: Session,
}
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
fn dev(tag: &str, url: &str) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    let dir = std::env::temp_dir().join(format!("spike-desktop-{tag}-{n}"));
    let s = Session::create(&dir, PASS.into(), KdfParams::insecure_for_tests()).unwrap();
    s.set_relay(url).unwrap();
    Dev { dir, s }
}
fn card(d: &Dev) -> String {
    d.s.overview().unwrap().me.card
}
fn owner_key(d: &Dev) -> String {
    IdentityCard::decode(&card(d)).unwrap().owner_hex()
}

#[tokio::test]
async fn create_unlock_and_the_passphrase_rules() {
    let dir = std::env::temp_dir().join("spike-desktop-life");
    std::fs::remove_dir_all(&dir).ok();
    assert!(!spike_desktop::api::exists(&dir));
    let short = "x".repeat(MIN_PASSPHRASE_CHARS - 1);
    assert!(Session::create(&dir, short, KdfParams::insecure_for_tests()).is_err());
    assert!(
        !spike_desktop::api::exists(&dir),
        "a refused create leaves nothing behind"
    );
    let s = Session::create(&dir, PASS.into(), KdfParams::insecure_for_tests()).unwrap();
    let me = s.overview().unwrap().me;
    assert!(!me.fingerprint.is_empty() && me.card.starts_with("spike2:") && me.approver);
    drop(s);
    // Never overwrites an identity; the wrong passphrase is refused; the right one opens it.
    assert!(Session::create(&dir, PASS.into(), KdfParams::insecure_for_tests()).is_err());
    assert!(Session::unlock(&dir, "wrong passphrase!!".into()).is_err());
    let again = Session::unlock(&dir, PASS.into()).unwrap();
    assert_eq!(again.overview().unwrap().me, me);
    // The relay setting is sealed with the vault and survives a restart; bad addresses are refused.
    assert!(again.set_relay("https://example.com").is_err());
    assert!(again.set_relay("ws://has space").is_err());
    again.set_relay("wss://relay.example").unwrap();
    drop(again);
    assert_eq!(
        Session::unlock(&dir, PASS.into()).unwrap().relay(),
        "wss://relay.example"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn chat_send_receive_unread_and_verification() {
    let url = relay().await;
    let (alice, bob) = (dev("alice", &url), dev("bob", &url));
    // Both publish keys by syncing once.
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();

    // Contacts: bad cards, your own card and empty names are refused.
    assert!(alice.s.add_contact("Bob", "not a card").is_err());
    assert!(alice.s.add_contact("Me", &card(&alice)).is_err());
    assert!(alice.s.add_contact("  ", &card(&bob)).is_err());
    let msg = alice.s.add_contact("Bob", &card(&bob)).unwrap();
    assert!(msg.contains("not verified"));
    bob.s.add_contact("Alice", &card(&alice)).unwrap();
    assert!(alice
        .s
        .add_contact("Bob", &card(&bob))
        .unwrap()
        .contains("already"));

    // Empty and oversize messages never reach the network.
    assert!(alice.s.send("Bob", "   ").await.is_err());
    assert!(alice.s.send("Bob", &"x".repeat(17 * 1024)).await.is_err());
    assert!(alice.s.send("Nobody", "hi").await.is_err());

    assert_eq!(alice.s.send("Bob", "hello from alice").await.unwrap(), 1);
    let got = bob.s.sync(IDLE).await.unwrap();
    assert_eq!(got.received, 1);

    let ov = bob.s.overview().unwrap();
    let c = &ov.contacts[0];
    assert_eq!((c.name.as_str(), c.unread, c.verified), ("Alice", 1, false));
    assert_eq!(c.last.as_deref(), Some("hello from alice"));
    let thread = bob.s.conversation("Alice", 50).unwrap();
    assert_eq!(thread.len(), 1);
    assert!(!thread[0].out && thread[0].body == "hello from alice");

    // Reading clears the badge; the reply shows as ours, with "You:" in the preview.
    bob.s.mark_read("Alice").await.unwrap();
    assert_eq!(bob.s.overview().unwrap().contacts[0].unread, 0);
    bob.s.send("Alice", "hi back").await.unwrap();
    assert_eq!(
        bob.s.overview().unwrap().contacts[0].last.as_deref(),
        Some("You: hi back")
    );
    alice.s.sync(IDLE).await.unwrap();
    assert_eq!(alice.s.conversation("Bob", 50).unwrap().len(), 2);

    // Verification: a wrong fingerprint never verifies; the right one does.
    let fp = alice.s.overview().unwrap().contacts[0].fingerprint.clone();
    assert!(alice.s.mark_verified("Bob", "0000").is_err());
    assert!(!alice.s.overview().unwrap().contacts[0].verified);
    alice.s.mark_verified("Bob", &fp).unwrap();
    assert!(alice.s.overview().unwrap().contacts[0].verified);

    // A stranger's message is dropped and reported, not shown.
    let eve = dev("eve", &url);
    eve.s.sync(IDLE).await.unwrap();
    eve.s.add_contact("Alice", &card(&alice)).unwrap();
    eve.s.send("Alice", "let me in").await.unwrap();
    let r = alice.s.sync(IDLE).await.unwrap();
    assert_eq!(r.received, 0);
    assert_eq!(alice.s.overview().unwrap().contacts.len(), 1);
}

#[tokio::test]
async fn typed_agent_requests_are_shown_by_their_fields_and_decided_here() {
    use spike_core::action::parse_pairs;
    let url = relay().await;
    let (alice, bot) = (dev("alice", &url), dev("bot", &url));
    alice.s.sync(IDLE).await.unwrap();
    bot.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("bot", &card(&bot)).unwrap();
    bot.s.add_contact("alice", &card(&alice)).unwrap();

    // Alice grants the bot a standing permission (the app reads grants; creating them is CLI work for now).
    let home = |d: &Dev| spike_cli::state::Home::unlock(&d.dir, PASS).unwrap();
    agent::grant(
        &home(&alice),
        &owner_key(&bot),
        vec!["payments.send".into()],
        30,
        Some(5_000),
        Some(500),
        10,
        Some(5_000),
        false,
    )
    .unwrap();
    let a = alice.s.agents().unwrap();
    assert_eq!(
        (
            a.grants.len(),
            a.grants[0].agent.as_str(),
            a.grants[0].typed_only
        ),
        (1, "bot", false)
    );

    let fields = parse_pairs(["payee=acme-gpu", "currency=USD"]).unwrap();
    let id = ops::agent_request_typed(
        &home(&bot),
        url.as_str(),
        "alice",
        "payments.send",
        "Pay my friend Mallory",
        Some(1_500),
        Some(fields),
        60,
    )
    .await
    .unwrap();
    let got = alice.s.sync(IDLE).await.unwrap();
    assert_eq!(got.agent_requests, 1);

    // The headline is the signed fields, never the agent's words; the badge counts it.
    let ag = alice.s.agents().unwrap();
    let r = &ag.requests[0];
    assert_eq!(r.status, "waiting");
    assert_eq!(r.summary.as_deref(), Some("Pay 15.00 USD to acme-gpu"));
    assert_eq!(r.note, "Pay my friend Mallory");
    assert_eq!(r.typed.as_ref().unwrap()["payee"], "acme-gpu");
    let ov = alice.s.overview().unwrap();
    assert_eq!(ov.waiting, 1);
    assert!(ov.contacts[0].agent);

    // Approve: signed receipt goes back; the agent sees it.
    let said = alice.s.decide(&id[..8], true).await.unwrap();
    assert!(
        said.contains("Approved") && said.contains("receipt sent"),
        "{said}"
    );
    let r = &alice.s.agents().unwrap().requests[0];
    assert_eq!(
        (r.status.as_str(), r.by.as_deref()),
        ("approved", Some("you"))
    );
    assert_eq!(alice.s.overview().unwrap().waiting, 0);
    bot.s.sync(IDLE).await.unwrap();
    let out = agent::list_outbox(&home(&bot)).unwrap();
    assert!(out[0].receipt.is_some());

    // Deciding twice is refused; unknown ids are refused.
    assert!(alice.s.decide(&id[..8], false).await.is_err());
    assert!(alice.s.decide("ffffffff", true).await.is_err());
}

#[tokio::test]
async fn free_text_requests_are_marked_as_such() {
    let url = relay().await;
    let (alice, bot) = (dev("alice", &url), dev("bot", &url));
    alice.s.sync(IDLE).await.unwrap();
    bot.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("bot", &card(&bot)).unwrap();
    bot.s.add_contact("alice", &card(&alice)).unwrap();
    let home = |d: &Dev| spike_cli::state::Home::unlock(&d.dir, PASS).unwrap();
    agent::grant(
        &home(&alice),
        &owner_key(&bot),
        vec!["files.read".into()],
        30,
        None,
        None,
        10,
        None,
        false,
    )
    .unwrap();
    ops::agent_request(
        &home(&bot),
        url.as_str(),
        "alice",
        "files.read",
        "read the notes",
        None,
        60,
    )
    .await
    .unwrap();
    alice.s.sync(IDLE).await.unwrap();
    let r = &alice.s.agents().unwrap().requests[0];
    assert!(
        r.summary.is_none() && r.typed.is_none(),
        "free text has no checked summary"
    );
    assert_eq!(
        (r.status.as_str(), r.note.as_str()),
        ("waiting", "read the notes")
    );
}

#[tokio::test]
async fn an_unreachable_relay_is_an_error_not_a_hang() {
    let url = relay().await;
    let d = dev("lonely", &url);
    d.s.set_relay("ws://127.0.0.1:1").unwrap();
    let t = std::time::Instant::now();
    assert!(d.s.sync(IDLE).await.is_err());
    assert!(t.elapsed() < Duration::from_secs(20));
}
