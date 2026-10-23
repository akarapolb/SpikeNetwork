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

#[tokio::test]
async fn grants_are_created_validated_and_revoked_from_the_app() {
    use spike_desktop::api::GrantRequest;
    let url = relay().await;
    let (alice, bot) = (dev("alice", &url), dev("bot", &url));
    alice.s.add_contact("bot", &card(&bot)).unwrap();
    let req = |scopes: &[&str]| GrantRequest {
        agent: "bot".into(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        days: 30,
        max_cents: Some(5_000),
        auto_below_cents: Some(500),
        per_hour: 10,
        daily_cents: Some(5_000),
        typed_only: false,
    };
    // The form's choices come from the registry.
    let types = spike_desktop::api::action_types();
    assert!(types
        .iter()
        .any(|t| t.name == "payments.send" && t.money && t.fields == ["payee", "currency"]));

    alice
        .s
        .grant(req(&["payments.send", "files.read"]))
        .unwrap();
    let g = &alice.s.agents().unwrap().grants[0];
    assert_eq!(
        (g.agent.as_str(), g.scopes.len(), g.max_cents, g.typed_only),
        ("bot", 2, Some(5_000), false)
    );
    // A second grant replaces the first.
    let mut tighter = req(&["files.read"]);
    tighter.typed_only = true;
    alice.s.grant(tighter).unwrap();
    let grants = alice.s.agents().unwrap().grants;
    assert_eq!(
        (
            grants.len(),
            grants[0].scopes.as_slice(),
            grants[0].typed_only
        ),
        (1, &["files.read".to_string()][..], true)
    );

    // Bad input is refused before anything is stored.
    assert!(alice.s.grant(req(&[])).is_err());
    assert!(alice.s.grant(req(&["Bad Scope"])).is_err());
    assert!(
        alice.s.grant(req(&["admin.wipe"])).is_ok(),
        "free-text scopes are allowed unless typed-only"
    );
    let mut typed_unknown = req(&["admin.wipe"]);
    typed_unknown.typed_only = true;
    assert!(
        alice.s.grant(typed_unknown).is_err(),
        "typed-only needs known action types"
    );
    let mut forever = req(&["files.read"]);
    forever.days = 0;
    assert!(alice.s.grant(forever).is_err());
    let mut daily_below_max = req(&["files.read"]);
    daily_below_max.daily_cents = Some(100);
    assert!(alice.s.grant(daily_below_max).is_err());
    let mut unknown_agent = req(&["files.read"]);
    unknown_agent.agent = "nobody".into();
    assert!(alice.s.grant(unknown_agent).is_err());

    assert!(alice.s.revoke("bot").unwrap());
    assert!(alice.s.agents().unwrap().grants.is_empty());
    assert!(!alice.s.revoke("bot").unwrap());
}

#[tokio::test]
async fn files_are_sent_encrypted_and_saved_by_the_receiver() {
    let url = relay().await;
    let (alice, bob) = (dev("alice", &url), dev("bob", &url));
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Bob", &card(&bob)).unwrap();
    bob.s.add_contact("Alice", &card(&alice)).unwrap();

    // Two chunks' worth of bytes with a recognisable pattern.
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    assert!(
        alice
            .s
            .send_file("Bob", "report.bin", &[], "")
            .await
            .is_err(),
        "empty files are refused"
    );
    assert_eq!(
        alice
            .s
            .send_file("Bob", "..\\evil/report.bin", &data, "here you go")
            .await
            .unwrap(),
        1
    );
    assert_eq!(bob.s.sync(IDLE).await.unwrap().received, 1);

    let m = bob.s.conversation("Alice", 10).unwrap().pop().unwrap();
    assert_eq!(m.body, "here you go");
    let f = m.file.as_ref().unwrap();
    assert_eq!(
        (f.name.as_str(), f.size),
        ("report.bin", data.len() as u64),
        "path parts are stripped from the name"
    );
    assert!(bob.s.overview().unwrap().contacts[0]
        .last
        .as_deref()
        .unwrap()
        .contains("(file)"));

    // The receiver downloads and checks it; the sender already has its own copy.
    let (name, bytes) = bob.s.attachment(&m.id).await.unwrap();
    assert_eq!((name.as_str(), bytes == data), ("report.bin", true));
    let mine = alice.s.conversation("Bob", 10).unwrap().pop().unwrap();
    assert_eq!(alice.s.attachment(&mine.id).await.unwrap().1, data);
    assert!(bob.s.attachment("deadbeef").await.is_err());
}

#[tokio::test]
async fn a_new_device_links_to_your_identity_and_receives_history() {
    let url = relay().await;
    let (alice, bob, phone) = (dev("alice", &url), dev("bob", &url), dev("phone", &url));
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Bob", &card(&bob)).unwrap();
    bob.s.add_contact("Alice", &card(&alice)).unwrap();
    alice
        .s
        .send("Bob", "history before the phone")
        .await
        .unwrap();
    bob.s.sync(IDLE).await.unwrap();

    let before = phone.s.overview().unwrap();
    assert_eq!((before.me.devices, before.contacts.len()), (1, 0));

    // The phone shows a code; Alice's computer enters it; both show the same safety code.
    let code = phone.s.link_offer().unwrap();
    let t = Duration::from_secs(20);
    let (phone_safety, alice_safety) = tokio::join!(phone.s.link_wait(t), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        alice.s.link_begin(&code).await
    });
    let (phone_safety, alice_safety) = (phone_safety.unwrap(), alice_safety.unwrap());
    assert_eq!(
        phone_safety, alice_safety,
        "both screens must show the same code"
    );

    // Receiving before the existing device shares, or finishing without a link, is refused cleanly.
    assert!(alice.s.link_finish("sideways", 30).await.is_err());
    let (done, got) = tokio::join!(alice.s.link_finish("all", 30), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        phone.s.link_receive(t).await
    });
    let (done, got) = (done.unwrap(), got.unwrap());
    assert_eq!((done.contacts, done.messages), (1, 1));
    assert_eq!((got.contacts, got.messages), (1, 1));

    // The phone is now one of Alice's devices, with her contacts and history.
    let ov = phone.s.overview().unwrap();
    assert_eq!(
        ov.me.fingerprint,
        alice.s.overview().unwrap().me.fingerprint,
        "one identity"
    );
    assert_eq!(
        (ov.me.devices, ov.me.device_list.len(), ov.me.approver),
        (2, 2, false)
    );
    assert_eq!(ov.me.device_list.iter().filter(|d| d.this).count(), 1);
    assert_eq!(
        phone.s.conversation("Bob", 10).unwrap()[0].body,
        "history before the phone"
    );
    let a = alice.s.overview().unwrap().me;
    assert_eq!(
        (a.devices, a.approver, a.list_version),
        (2, true, ov.me.list_version)
    );

    // Only the main device links new devices and holds grants.
    assert!(phone.s.link_begin("whatever").await.is_err());
    assert!(phone
        .s
        .grant(spike_desktop::api::GrantRequest {
            agent: "Bob".into(),
            scopes: vec!["files.read".into()],
            days: 1,
            max_cents: None,
            auto_below_cents: None,
            per_hour: 1,
            daily_cents: None,
            typed_only: false,
        })
        .is_err());

    // Bob's next message reaches both of Alice's devices.
    bob.s.sync(IDLE).await.unwrap();
    bob.s
        .send("Alice", "to both of your devices")
        .await
        .unwrap();
    assert_eq!(alice.s.sync(IDLE).await.unwrap().received, 1);
    assert_eq!(phone.s.sync(IDLE).await.unwrap().received, 1);
}

// ------------------------------------------------------------ relay trust

async fn start_tls_relay(cert: &std::path::Path, key: &std::path::Path) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("wss://127.0.0.1:{}", l.local_addr().unwrap().port());
    let cfg = Config {
        tls: Some(spike_relay::tls::TlsFiles {
            cert: cert.into(),
            key: key.into(),
        }),
        ..Config::default()
    };
    tokio::spawn(serve(l, cfg));
    url
}

#[tokio::test]
async fn a_self_signed_relay_is_refused_until_its_certificate_is_trusted() {
    let tmp = std::env::temp_dir().join(format!("spike-desktop-tls-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let ck =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let (cert, key) = (tmp.join("r.crt"), tmp.join("r.key"));
    std::fs::write(&cert, ck.cert.pem()).unwrap();
    std::fs::write(&key, ck.key_pair.serialize_pem()).unwrap();
    let url = start_tls_relay(&cert, &key).await;

    let d = dev("tls", &url);
    // Not trusted: nothing connects, and the app says why.
    assert!(d.s.sync(IDLE).await.is_err());
    let why = format!("{:#}", d.s.test_relay().await.unwrap_err());
    assert!(
        why.to_lowercase().contains("certificate") || why.to_lowercase().contains("tls"),
        "{why}"
    );

    // Bad input is refused and changes nothing.
    assert!(d.s.set_relay_ca(Some("not a certificate".into())).is_err());
    assert!(
        d.s.set_relay_ca(Some(ck.key_pair.serialize_pem())).is_err(),
        "a private key is not a certificate"
    );
    assert_eq!(d.s.overview().unwrap().relay_trust.ca_certs, 0);

    // Trust that one certificate: it works, and says the certificate was verified.
    assert_eq!(d.s.set_relay_ca(Some(ck.cert.pem())).unwrap(), 1);
    assert_eq!(d.s.overview().unwrap().relay_trust.ca_certs, 1);
    d.s.sync(IDLE).await.unwrap();
    assert!(d
        .s
        .test_relay()
        .await
        .unwrap()
        .contains("certificate verified"));

    // The two settings are independent: toggling plain ws never drops the trusted certificate, and vice versa.
    d.s.set_allow_insecure(true).unwrap();
    assert_eq!(d.s.overview().unwrap().relay_trust.ca_certs, 1);
    d.s.set_allow_insecure(false).unwrap();
    assert_eq!(d.s.overview().unwrap().relay_trust.ca_certs, 1);

    // The choice is sealed in the home and survives a restart.
    let again = Session::unlock(&d.dir, PASS.into()).unwrap();
    assert_eq!(again.overview().unwrap().relay_trust.ca_certs, 1);
    again.sync(IDLE).await.unwrap();

    // Removing it goes back to refusing the self-signed relay.
    again.set_relay_ca(None).unwrap();
    assert!(again.sync(IDLE).await.is_err());
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn plain_ws_off_loopback_needs_an_explicit_opt_in() {
    let url = relay().await;
    let d = dev("lan", &url);
    d.s.set_relay("ws://192.0.2.10:8787").unwrap();
    let why = format!("{:#}", d.s.sync(IDLE).await.unwrap_err());
    assert!(why.contains("refusing plain ws://"), "{why}");
    assert!(!d.s.overview().unwrap().relay_trust.allow_insecure);
    d.s.set_allow_insecure(true).unwrap();
    assert!(d.s.overview().unwrap().relay_trust.allow_insecure);
    d.s.set_allow_insecure(false).unwrap();
    assert!(!d.s.overview().unwrap().relay_trust.allow_insecure);
}

// ------------------------------------------------------ edit, delete, devices

#[tokio::test]
async fn edits_and_deletes_reach_the_other_person() {
    let url = relay().await;
    let (alice, bob) = (dev("alice", &url), dev("bob", &url));
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Bob", &card(&bob)).unwrap();
    bob.s.add_contact("Alice", &card(&alice)).unwrap();
    alice.s.send("Bob", "see you at 3").await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    let id = alice.s.conversation("Bob", 5).unwrap()[0].id.clone();

    assert!(
        alice.s.edit(&id, "   ").await.is_err(),
        "empty edits are refused"
    );
    alice.s.edit(&id, "see you at 3:30").await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    let m = &bob.s.conversation("Alice", 5).unwrap()[0];
    assert_eq!((m.body.as_str(), m.edited), ("see you at 3:30", true));
    assert_eq!(
        alice.s.conversation("Bob", 5).unwrap()[0].body,
        "see you at 3:30"
    );

    // Bob cannot edit or delete Alice's message.
    assert!(bob.s.edit(&m.id, "gotcha").await.is_err());
    assert!(bob.s.delete(&m.id).await.is_err());

    alice.s.delete(&id).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    let m = &bob.s.conversation("Alice", 5).unwrap()[0];
    assert!(
        m.deleted && m.body.is_empty(),
        "the text is gone, a tombstone stays"
    );
    assert!(
        alice.s.edit(&id, "too late").await.is_err(),
        "a deleted message cannot be edited"
    );
}

#[tokio::test]
async fn a_removed_device_is_told_and_stops_receiving() {
    let url = relay().await;
    let (alice, bob, phone) = (dev("alice", &url), dev("bob", &url), dev("phone", &url));
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Bob", &card(&bob)).unwrap();
    bob.s.add_contact("Alice", &card(&alice)).unwrap();
    // Link the phone.
    let code = phone.s.link_offer().unwrap();
    let t = Duration::from_secs(20);
    let (a, b) = tokio::join!(phone.s.link_wait(t), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        alice.s.link_begin(&code).await
    });
    assert_eq!(a.unwrap(), b.unwrap());
    let (x, y) = tokio::join!(alice.s.link_finish("all", 30), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        phone.s.link_receive(t).await
    });
    x.unwrap();
    y.unwrap();
    assert!(!phone.s.overview().unwrap().me.removed);

    let phone_id = alice
        .s
        .overview()
        .unwrap()
        .me
        .device_list
        .iter()
        .find(|d| !d.this)
        .unwrap()
        .id
        .clone();
    // Refusals: too short, your own device, a stranger's id, and from a device that is not the main one.
    assert!(alice.s.remove_device(&phone_id[..4]).await.is_err());
    let my_id = alice
        .s
        .overview()
        .unwrap()
        .me
        .device_list
        .iter()
        .find(|d| d.this)
        .unwrap()
        .id
        .clone();
    assert!(alice.s.remove_device(&my_id).await.is_err());
    assert!(alice.s.remove_device("00000000").await.is_err());
    assert!(phone.s.remove_device(&my_id).await.is_err());

    alice.s.remove_device(&phone_id).await.unwrap();
    assert_eq!(alice.s.overview().unwrap().me.devices, 1);
    // The phone learns on its next sync and says so.
    phone.s.sync(IDLE).await.unwrap();
    assert!(
        phone.s.overview().unwrap().me.removed,
        "the removed device is told"
    );

    // Bob's next message goes to Alice's remaining device only.
    bob.s.sync(IDLE).await.unwrap();
    bob.s.send("Alice", "only the laptop now").await.unwrap();
    assert_eq!(alice.s.sync(IDLE).await.unwrap().received, 1);
    assert_eq!(phone.s.sync(IDLE).await.unwrap().received, 0);
}

// --------------------------------------------------------- live connection

#[tokio::test]
async fn the_live_connection_wakes_on_a_message_without_consuming_it() {
    let url = relay().await;
    let (alice, bob) = (dev("alice", &url), dev("bob", &url));
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Bob", &card(&bob)).unwrap();
    bob.s.add_contact("Alice", &card(&alice)).unwrap();

    // Bob's listener waits. Nothing yet: the heartbeat window passes quietly.
    let mut live = bob.s.listener_connect().await.unwrap();
    assert!(live
        .next_delivery(Some(Duration::from_millis(300)))
        .await
        .unwrap()
        .is_none());

    // Alice sends. Bob's connection is told straight away...
    alice.s.send("Bob", "ping").await.unwrap();
    let woke = tokio::time::timeout(
        Duration::from_secs(5),
        live.next_delivery(Some(Duration::from_secs(5))),
    )
    .await;
    assert!(matches!(woke, Ok(Ok(Some(_)))), "the relay pushed it");
    drop(live);

    // ...but it was never acknowledged, so a normal sync still receives it, once.
    assert_eq!(bob.s.sync(IDLE).await.unwrap().received, 1);
    assert_eq!(bob.s.sync(IDLE).await.unwrap().received, 0);
    assert_eq!(bob.s.conversation("Alice", 5).unwrap()[0].body, "ping");

    // Doing something else (which opens its own connection) must not deafen the waiting one: it stays
    // live and is still pushed to. (Before the relay allowed several connections per device, the
    // second connection silently took over and the first waited forever without ever being told.)
    let mut live = bob.s.listener_connect().await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    assert!(
        matches!(
            live.next_delivery(Some(Duration::from_millis(300))).await,
            Ok(None)
        ),
        "still connected and quiet"
    );
    alice.s.send("Bob", "second ping").await.unwrap();
    let woke = tokio::time::timeout(
        Duration::from_secs(5),
        live.next_delivery(Some(Duration::from_secs(5))),
    )
    .await;
    assert!(
        matches!(woke, Ok(Ok(Some(_)))),
        "still woken after an operation ran"
    );
    drop(live);
}

#[tokio::test]
async fn a_device_that_piles_up_connections_loses_the_oldest() {
    let url = relay().await;
    let bob = dev("bob", &url);
    bob.s.sync(IDLE).await.unwrap();
    let mut first = bob.s.listener_connect().await.unwrap();
    let mut others = vec![];
    for _ in 0..4 {
        others.push(bob.s.listener_connect().await.unwrap());
    }
    // Five connections from one device: the relay keeps four and closes the oldest, which finds out at once.
    let ended = tokio::time::timeout(
        Duration::from_secs(5),
        first.next_delivery(Some(Duration::from_secs(30))),
    )
    .await;
    assert!(
        matches!(ended, Ok(Err(_))),
        "the oldest connection must be closed"
    );
    for o in &mut others {
        assert!(
            matches!(
                o.next_delivery(Some(Duration::from_millis(200))).await,
                Ok(None)
            ),
            "the newer ones stay open"
        );
    }
}

#[tokio::test]
async fn linking_pauses_the_listener_until_it_finishes_or_is_cancelled() {
    let url = relay().await;
    let phone = dev("phone", &url);
    assert!(!phone.s.paused());
    phone.s.link_offer().unwrap();
    assert!(
        phone.s.paused(),
        "a sync would take the link frames off the relay"
    );
    phone.s.link_cancel();
    assert!(!phone.s.paused());
}

// ------------------------------------------------------- message requests

#[tokio::test]
async fn strangers_wait_as_message_requests_until_you_decide() {
    let url = relay().await;
    let (alice, bob) = (dev("alice", &url), dev("bob", &url));
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    // Alice has Bob's card; Bob has never heard of Alice.
    alice.s.add_contact("Bob", &card(&bob)).unwrap();
    alice
        .s
        .send("Bob", "hello, we met at the site visit")
        .await
        .unwrap();

    let got = bob.s.sync(IDLE).await.unwrap();
    assert_eq!(
        (got.received, got.message_requests),
        (0, 1),
        "a request, not a message"
    );
    let ov = bob.s.overview().unwrap();
    assert_eq!(
        (ov.requests, ov.contacts.len()),
        (1, 0),
        "no contact, no conversation"
    );
    assert_eq!(bob.s.attention().unwrap(), 1, "it asks for your attention");

    let reqs = bob.s.message_requests().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].fingerprint,
        alice.s.overview().unwrap().me.fingerprint,
        "their real, validated card"
    );
    assert_eq!(reqs[0].held[0].body, "hello, we met at the site visit");

    // Accepting needs a sensible name; then she is a contact, not verified, and the chat has her message.
    assert!(bob.s.accept_request(&reqs[0].id, "   ").is_err());
    assert_eq!(bob.s.accept_request(&reqs[0].id, "Alice").unwrap(), 1);
    let ov = bob.s.overview().unwrap();
    assert_eq!(
        (
            ov.requests,
            ov.contacts[0].name.as_str(),
            ov.contacts[0].verified
        ),
        (0, "Alice", false)
    );
    assert_eq!(
        bob.s.conversation("Alice", 5).unwrap()[0].body,
        "hello, we met at the site visit"
    );

    // The conversation continues in both directions.
    bob.s.send("Alice", "nice to meet you").await.unwrap();
    assert_eq!(alice.s.sync(IDLE).await.unwrap().received, 1);
    alice.s.send("Bob", "likewise").await.unwrap();
    assert_eq!(bob.s.sync(IDLE).await.unwrap().received, 1);
}

#[tokio::test]
async fn blocking_a_stranger_drops_them_and_can_be_undone() {
    let url = relay().await;
    let (carol, bob) = (dev("carol", &url), dev("bob", &url));
    carol.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    carol.s.add_contact("Bob", &card(&bob)).unwrap();
    carol.s.send("Bob", "buy my thing").await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    let id = bob.s.message_requests().unwrap()[0].id.clone();

    bob.s.block_request(&id).unwrap();
    assert!(bob.s.message_requests().unwrap().is_empty());
    let blocked = bob.s.blocked().unwrap();
    assert_eq!(blocked.len(), 1);
    assert_eq!(
        blocked[0].fingerprint,
        carol.s.overview().unwrap().me.fingerprint
    );

    // More from them is dropped quietly: nothing new for Bob to look at.
    carol.s.send("Bob", "hello?").await.unwrap();
    let got = bob.s.sync(IDLE).await.unwrap();
    assert_eq!((got.received, got.message_requests), (0, 0));
    assert_eq!(bob.s.overview().unwrap().requests, 0);

    bob.s.unblock(&blocked[0].owner).unwrap();
    assert!(bob.s.blocked().unwrap().is_empty());
    assert!(bob.s.unblock(&blocked[0].owner).is_err());

    // Declining another stranger is not blocking.
    let dave = dev("dave", &url);
    dave.s.sync(IDLE).await.unwrap();
    dave.s.add_contact("Bob", &card(&bob)).unwrap();
    dave.s.send("Bob", "hi").await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    let id = bob.s.message_requests().unwrap()[0].id.clone();
    bob.s.decline_request(&id).unwrap();
    assert!(bob.s.blocked().unwrap().is_empty());
    assert!(bob.s.decline_request(&id).is_err());
}

#[test]
fn a_link_code_fits_in_a_scannable_qr() {
    // The code a new device shows is short enough for a QR code of moderate density.
    let dir = std::env::temp_dir().join(format!("spike-qr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let id = spike_core::identity::Identity::generate().unwrap();
    let n = spike_core::link::NewDevice::new(&id.card(), Some("wss://relay.example.org".into()));
    let code = n.offer.encode();
    assert!(code.len() < 400, "{}", code.len());
    let svg = spike_desktop::api::qr_svg(&code).unwrap();
    assert!(
        svg.starts_with("<?xml") || svg.contains("<svg"),
        "{}",
        &svg[..60]
    );
    assert!(svg.contains("#ffffff") && svg.contains("#000000"));
    assert!(spike_desktop::api::qr_svg(&"x".repeat(2000)).is_err());
}

#[tokio::test]
async fn the_screen_is_told_as_soon_as_a_message_is_stored_not_after_the_idle_wait() {
    let url = relay().await;
    let (alice, bob) = (dev("alice", &url), dev("bob", &url));
    alice.s.sync(IDLE).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Bob", &card(&bob)).unwrap();
    bob.s.add_contact("Alice", &card(&alice)).unwrap();
    bob.s.send("Alice", "hello").await.unwrap();

    // A long idle wait: the sync ends two seconds after the last message...
    let started = std::time::Instant::now();
    let mut first = None;
    let mut unread_then = 0;
    let r = alice
        .s
        .sync_with(Duration::from_secs(2), |totals, attention| {
            if first.is_none() {
                first = Some((started.elapsed(), totals.received));
                unread_then = attention;
            }
        })
        .await
        .unwrap();
    let total = started.elapsed();
    // ...but the progress call came as soon as the message was stored, long before that.
    let (at, received) = first.expect("progress was reported");
    assert_eq!((r.received, received), (1, 1));
    assert_eq!(unread_then, 1, "the unread count already includes it");
    assert!(
        total - at >= Duration::from_millis(1500),
        "told after {at:?} of a {total:?} sync"
    );
}

// ------------------------------------------------------------------ groups (ADR 0024)

/// Alice (admin) with Bob and Carol, who know Alice but not each other.
async fn pilot() -> (String, Dev, Dev, Dev, String) {
    let url = relay().await;
    let (alice, bob, carol) = (dev("alice", &url), dev("bob", &url), dev("carol", &url));
    for d in [&alice, &bob, &carol] {
        d.s.sync(IDLE).await.unwrap();
    }
    for (name, d) in [("Bob", &bob), ("Carol", &carol)] {
        alice.s.add_contact(name, &card(d)).unwrap();
        d.s.add_contact("Alice", &card(&alice)).unwrap();
    }
    let made = alice
        .s
        .group_create("Pilot", &["Bob".into(), "Carol".into()], false)
        .await
        .unwrap();
    assert_eq!((made.reached, made.people), (2, 2));
    for d in [&bob, &carol] {
        assert_eq!(d.s.sync(IDLE).await.unwrap().groups, 1);
    }
    (url, alice, bob, carol, made.group)
}

#[tokio::test]
async fn a_group_is_created_listed_and_written_in_from_the_app() {
    let (_url, alice, bob, carol, gid) = pilot().await;

    // It appears in the list for everyone, with no unread yet.
    for d in [&alice, &bob, &carol] {
        let g = &d.s.overview().unwrap().groups;
        assert_eq!(g.len(), 1);
        assert_eq!(
            (g[0].name.as_str(), g[0].members, g[0].unread),
            ("Pilot", 3, 0)
        );
        assert_eq!(g[0].status, "joined");
    }
    // Alice knows two unverified people; Bob knows Alice (a contact) and Carol (a stranger).
    assert_eq!(alice.s.overview().unwrap().groups[0].unverified, 2);

    // A message arrives with its author's name, unread, and in the preview.
    assert_eq!(
        alice
            .s
            .group_send(&gid, "agenda is above")
            .await
            .unwrap()
            .reached,
        2
    );
    assert_eq!(bob.s.sync(IDLE).await.unwrap().received, 1);
    let row = &bob.s.overview().unwrap().groups[0];
    assert_eq!(
        (row.unread, row.last.as_deref()),
        (1, Some("Alice: agenda is above"))
    );
    assert_eq!(bob.s.attention().unwrap(), 1, "the window title counts it");
    let thread = bob.s.group_conversation(&gid, 50).unwrap();
    assert_eq!(thread.len(), 1);
    assert_eq!(
        (
            thread[0].from.as_deref(),
            thread[0].from_verified,
            thread[0].out
        ),
        (Some("Alice"), false, false)
    );
    bob.s.group_mark_read(&gid).await.unwrap();
    assert_eq!(bob.s.overview().unwrap().groups[0].unread, 0);
    assert_eq!(bob.s.attention().unwrap(), 0);

    // Bob writes to Carol, whom he has never met: she sees a name she can recognise by its code.
    bob.s.group_send(&gid, "hi carol, bob here").await.unwrap();
    carol.s.sync(IDLE).await.unwrap();
    let msgs = carol.s.group_conversation(&gid, 50).unwrap();
    let last = msgs.last().unwrap();
    assert!(last.from.as_deref().unwrap().starts_with('~'), "{last:?}");
    let detail = carol.s.group(&gid).unwrap();
    let bobm = detail
        .members
        .iter()
        .find(|m| !m.you && !m.contact)
        .unwrap();
    assert!(bobm.fingerprint.is_some() && !bobm.verified);
    assert_eq!(
        (detail.unverified, detail.me_admin, detail.can_add),
        (2, false, false)
    );

    // She can keep him as a contact; his messages then carry the name she gave.
    carol.s.add_member_as_contact(&bobm.owner, "Bob").unwrap();
    assert_eq!(
        carol
            .s
            .group(&gid)
            .unwrap()
            .members
            .iter()
            .filter(|m| m.contact)
            .count(),
        2
    );
    assert_eq!(
        carol
            .s
            .group_conversation(&gid, 50)
            .unwrap()
            .last()
            .unwrap()
            .from
            .as_deref(),
        Some("Bob")
    );
    assert!(carol
        .s
        .add_member_as_contact(&"0".repeat(64), "Nobody")
        .is_err());

    // Empty and oversize messages never reach the network.
    assert!(alice.s.group_send(&gid, "  ").await.is_err());
    assert!(alice
        .s
        .group_send(&gid, &"x".repeat(17 * 1024))
        .await
        .is_err());
    assert!(alice.s.group_send("nope", "hi").await.is_err());

    // Editing and deleting your own group message reaches everyone; others' messages are not yours.
    let mine = alice.s.group_conversation(&gid, 50).unwrap();
    let id = mine.iter().find(|m| m.out).unwrap().id.clone();
    alice.s.edit(&id, "agenda v2").await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    let seen = bob.s.group_conversation(&gid, 50).unwrap();
    let m = seen.iter().find(|m| m.id == id).unwrap();
    assert_eq!((m.body.as_str(), m.edited), ("agenda v2", true));
    assert!(carol.s.edit(&id, "mine now").await.is_err());
    alice.s.delete(&id).await.unwrap();
    bob.s.sync(IDLE).await.unwrap();
    assert!(bob
        .s
        .group_conversation(&gid, 50)
        .unwrap()
        .iter()
        .any(|m| m.id == id && m.deleted));
}

#[tokio::test]
async fn members_and_settings_change_only_as_the_rules_allow() {
    let (_url, alice, bob, carol, gid) = pilot().await;
    let bob_owner = owner_key(&bob);
    let carol_owner = owner_key(&carol);

    let a = alice.s.group(&gid).unwrap();
    assert_eq!(
        (a.me_admin, a.can_add, a.add_by.as_str(), a.version, a.max),
        (true, true, "admins", 1, 20)
    );
    // A member who is not an admin can change nothing, and cannot add while it is admins-only.
    assert!(bob.s.group_rename(&gid, "Mine").await.is_err());
    assert!(bob.s.group_remove(&gid, &carol_owner).await.is_err());
    assert!(bob.s.group_make_admin(&gid, &bob_owner).await.is_err());
    assert!(bob.s.group_add(&gid, &["Alice".into()]).await.is_err());
    assert!(!bob.s.group(&gid).unwrap().can_add);

    // Alice opens adding to everyone, renames, and makes Bob an admin; they hear about it.
    alice.s.group_set_add_by(&gid, true).await.unwrap();
    alice.s.group_rename(&gid, "Pilot team").await.unwrap();
    alice.s.group_make_admin(&gid, &bob_owner).await.unwrap();
    let r = bob.s.sync(IDLE).await.unwrap();
    assert!(r.groups >= 1);
    let b = bob.s.group(&gid).unwrap();
    assert_eq!(
        (b.name.as_str(), b.add_by.as_str(), b.me_admin, b.can_add),
        ("Pilot team", "any", true, true)
    );
    assert_eq!(b.members.iter().filter(|m| m.admin).count(), 2);
    assert!(
        alice.s.group_rename(&gid, "").await.is_err(),
        "a group needs a name"
    );

    // Alice removes Carol: she learns it, keeps the history as read-only, and cannot write.
    alice.s.group_send(&gid, "before").await.unwrap();
    carol.s.sync(IDLE).await.unwrap();
    alice.s.group_remove(&gid, &carol_owner).await.unwrap();
    let r = carol.s.sync(IDLE).await.unwrap();
    assert!(
        r.notes.iter().any(|n| n.contains("removed")),
        "{:?}",
        r.notes
    );
    let row = &carol.s.overview().unwrap().groups[0];
    assert_eq!((row.status.as_str(), row.members), ("left", 2));
    assert!(carol
        .s
        .group_conversation(&gid, 50)
        .unwrap()
        .iter()
        .any(|m| m.body == "before"));
    assert!(carol.s.group_send(&gid, "still here?").await.is_err());
    alice.s.group_send(&gid, "after").await.unwrap();
    carol.s.sync(IDLE).await.unwrap();
    assert!(!carol
        .s
        .group_conversation(&gid, 50)
        .unwrap()
        .iter()
        .any(|m| m.body == "after"));

    // Bob leaves, after hearing about Carol's removal (otherwise the two changes would be
    // concurrent and the one with the lower hash would win).
    bob.s.sync(IDLE).await.unwrap();
    bob.s.group_leave(&gid).await.unwrap();
    assert_eq!(bob.s.group(&gid).unwrap().status, "left");
    let row = &bob.s.overview().unwrap().groups[0];
    assert_eq!(row.status, "left", "listed because it has a history");
    assert!(bob.s.group_send(&gid, "still here?").await.is_err());
    alice.s.sync(IDLE).await.unwrap();
    assert_eq!(alice.s.group(&gid).unwrap().members.len(), 1);
}

#[tokio::test]
async fn an_invite_from_someone_you_do_not_know_waits_for_you() {
    let (url, alice, _bob, _carol, gid) = pilot().await;
    // Eve is Alice's contact, but Eve has never heard of Alice.
    let eve = dev("eve", &url);
    eve.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Eve", &card(&eve)).unwrap();
    alice.s.group_add(&gid, &["Eve".into()]).await.unwrap();

    let r = eve.s.sync(IDLE).await.unwrap();
    assert_eq!(r.message_requests, 1, "counted so the screen refreshes");
    let ov = eve.s.overview().unwrap();
    assert_eq!(ov.requests, 0, "not in the message-request list");
    assert_eq!(
        (ov.groups[0].status.as_str(), ov.groups[0].name.as_str()),
        ("invited", "Pilot")
    );
    assert!(ov.groups[0].invited_by.as_deref().unwrap().starts_with('~'));
    assert_eq!(eve.s.attention().unwrap(), 1, "an invite asks for you");
    assert!(eve.s.group_send(&gid, "hi").await.is_err());

    alice
        .s
        .group_send(&gid, "before you accepted")
        .await
        .unwrap();
    eve.s.sync(IDLE).await.unwrap();
    assert!(eve.s.group_conversation(&gid, 50).unwrap().is_empty());

    eve.s.group_accept(&gid).unwrap();
    assert_eq!(eve.s.overview().unwrap().groups[0].status, "joined");
    assert_eq!(eve.s.attention().unwrap(), 0);
    alice.s.group_send(&gid, "welcome eve").await.unwrap();
    eve.s.sync(IDLE).await.unwrap();
    assert_eq!(eve.s.group_conversation(&gid, 50).unwrap().len(), 1);
    assert!(eve.s.group_send(&gid, "thanks").await.is_ok());

    // Declining keeps you out, and a second invite after leaving is an invite again.
    let zed = dev("zed", &url);
    zed.s.sync(IDLE).await.unwrap();
    alice.s.add_contact("Zed", &card(&zed)).unwrap();
    alice.s.group_add(&gid, &["Zed".into()]).await.unwrap();
    zed.s.sync(IDLE).await.unwrap();
    zed.s.group_decline(&gid).unwrap();
    assert!(
        zed.s.overview().unwrap().groups.is_empty(),
        "nothing to show: no history"
    );
    assert!(zed.s.group_accept(&gid).is_err());
}
