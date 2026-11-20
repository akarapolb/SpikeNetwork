//! Organisations end to end (ADR 0023, ADR 0035): an admin key, a tenant the operator created, a relay in
//! enrolled mode, single-use tokens, rules that only move forward, revoking, leaving, and messages from
//! a contact on another relay reaching a member on the organisation's relay.

use spike_cli::client::Relay;
use spike_cli::org::{self, AdminRelay};
use spike_cli::state::{Contact, Home};
use spike_cli::{ops, Outcome};
use spike_core::identity::Identity;
use spike_core::owner::OwnerKey;
use spike_core::tenant::{AgentRule, BackupRule, EnrolToken, Rules, SignedRules};
use spike_core::vault::KdfParams;
use spike_relay::store::Store;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase organisations";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

struct Dev(Home);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.path()).ok();
    }
}

fn tag() -> String {
    hex::encode(&Identity::generate().unwrap().ed_public()[..6])
}

fn dev(name: &str) -> Dev {
    Dev(Home::create(
        std::env::temp_dir().join(format!("spike-org-{name}-{}", tag())),
        PASS,
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap())
}

async fn open_relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

/// The organisation's relay: a database file with the tenant the operator created, in enrolled mode
/// unless `open`.
async fn org_relay(admin_pub: &str, max_devices: u64, open: bool) -> (String, PathBuf) {
    let db = std::env::temp_dir().join(format!("spike-org-relay-{}.db", tag()));
    Store::open(Some(&db))
        .unwrap()
        .tenant_create("northwind", "Northwind", admin_pub, max_devices, 0)
        .unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(
        l,
        Config {
            db_path: Some(db.clone()),
            enrolled: !open,
            ..Config::default()
        },
    ));
    (url, db)
}

/// An admin console on its own computer, holding the key, for the relay at `url`.
fn console(url: &str) -> Dev {
    let d = dev("admin");
    org::create_admin(&d.0, "northwind", "Northwind", url).unwrap();
    d
}

fn admin_pub(c: &Dev) -> String {
    org::admin(&c.0).unwrap().unwrap().admin_pub().unwrap()
}

async fn admin_conn(c: &Dev, url: &str) -> AdminRelay {
    AdminRelay::connect(url.into(), &org::admin(&c.0).unwrap().unwrap())
        .await
        .unwrap()
}

fn err_text<T>(r: anyhow::Result<T>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => format!("{e:#}"),
    }
}

/// The admin key is created before the relay knows it, so tests make the console first with a
/// placeholder relay address, then create the tenant and point the console at the real relay.
async fn setup(max_devices: u64, open: bool) -> (Dev, String) {
    let c = dev("admin");
    let a = org::create_admin(&c.0, "northwind", "Northwind", "ws://placeholder").unwrap();
    let (url, _) = org_relay(&a.admin_pub().unwrap(), max_devices, open).await;
    let mut a = org::admin(&c.0).unwrap().unwrap();
    a.relay = url.clone();
    org::save_admin(&c.0, Some(&a)).unwrap();
    (c, url)
}

#[tokio::test]
async fn an_unenrolled_device_is_refused_and_a_token_admits_exactly_one_device() {
    let (c, url) = setup(50, false).await;
    let phone = dev("phone");
    let me = phone.0.load_identity().unwrap();
    // Not enrolled: signed in as a visitor, which may deliver to members but keep nothing here.
    let mut visitor = Relay::connect(url.as_str(), &me).await.unwrap();
    assert!(visitor.visitor);
    let other = Identity::generate().unwrap().device_id();
    let e = err_text(visitor.send(&other, b"ciphertext").await);
    assert!(
        e.contains("not_enrolled"),
        "no mailbox for a non-member: {e}"
    );
    let refused = err_text(ops::publish_keys(&phone.0, url.as_str()).await);
    assert!(refused.contains("not_enrolled"), "{refused}");

    let (token, issued) = org::issue_token(&c.0, "Alice's phone", 24 * 3600).unwrap();
    assert_eq!(issued.label, "Alice's phone");
    let shown = org::inspect(&token).unwrap();
    assert_eq!(
        (shown.org_name.as_str(), shown.relay.as_str()),
        ("Northwind", url.as_str())
    );

    let m = org::join(&phone.0, &token, url.as_str().into(), None)
        .await
        .unwrap();
    assert_eq!(
        (m.tenant.as_str(), m.removed, m.admin_pub.as_str()),
        ("northwind", false, admin_pub(&c).as_str())
    );
    // Signed in normally from now on, and the device list names the organisation's relay.
    Relay::connect(url.as_str(), &me).await.unwrap();
    let list = phone.0.load_device_list().unwrap();
    assert_eq!(list.devices[0].relay.as_deref(), Some(url.as_str()));

    // The same token on another device: refused in words.
    let laptop = dev("laptop");
    let used = err_text(org::join(&laptop.0, &token, url.as_str().into(), None).await);
    assert!(used.contains("already used by another device"), "{used}");

    // The console sees one device id, active, and no names or traffic.
    let mut admin = admin_conn(&c, &url).await;
    let devices = admin.devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(
        (devices[0].device.as_str(), devices[0].activity.as_str()),
        (me.device_id().as_str(), "active")
    );
    let wire = serde_json::to_string(&devices).unwrap();
    assert!(
        !wire.contains("Alice"),
        "the token label never reaches the relay"
    );
    let usage = admin.usage().await.unwrap();
    assert_eq!(
        (usage.devices, usage.device_limit, usage.enrolled_mode),
        (1, 50, true)
    );
}

#[tokio::test]
async fn a_full_organisation_and_a_withdrawn_or_foreign_token_are_refused() {
    let (c, url) = setup(1, false).await;
    let (t1, _) = org::issue_token(&c.0, "", 3600).unwrap();
    let (t2, i2) = org::issue_token(&c.0, "", 3600).unwrap();
    let (t3, _) = org::issue_token(&c.0, "", 3600).unwrap();
    let a = dev("a");
    org::join(&a.0, &t1, url.as_str().into(), None)
        .await
        .unwrap();
    let full = err_text(org::join(&dev("b").0, &t2, url.as_str().into(), None).await);
    assert!(full.contains("no device places left"), "{full}");

    let mut admin = admin_conn(&c, &url).await;
    org::withdraw_token(&c.0, &mut admin, &i2.token_id)
        .await
        .unwrap();
    // Room again, but the withdrawn token stays refused.
    admin
        .revoke(&a.0.load_identity().unwrap().device_id())
        .await
        .unwrap();
    let withdrawn = err_text(org::join(&dev("b").0, &t2, url.as_str().into(), None).await);
    assert!(withdrawn.contains("withdrew this token"), "{withdrawn}");
    org::join(&dev("c").0, &t3, url.as_str().into(), None)
        .await
        .unwrap();

    // A token signed by some other key for the same tenant id.
    let (forged, _) = EnrolToken::issue(
        &OwnerKey::generate(),
        "northwind",
        &url,
        "Northwind",
        3600,
        1,
    )
    .unwrap();
    assert!(org::inspect(&forged.encode()).is_err(), "expired long ago");
    let (forged, _) = EnrolToken::issue(
        &OwnerKey::generate(),
        "northwind",
        &url,
        "Northwind",
        3600,
        spike_core::clock::now_ms() / 1000,
    )
    .unwrap();
    let e = err_text(org::join(&dev("d").0, &forged.encode(), url.as_str().into(), None).await);
    assert!(e.contains("does not recognise the organisation"), "{e}");
}

#[tokio::test]
async fn rules_reach_members_signed_and_only_forward() {
    let (c, url) = setup(50, false).await;
    let phone = dev("phone");
    let (token, _) = org::issue_token(&c.0, "", 3600).unwrap();
    org::join(&phone.0, &token, url.as_str().into(), None)
        .await
        .unwrap();
    assert!(org::rules_in_force(&phone.0).unwrap().is_none());

    let mut admin = admin_conn(&c, &url).await;
    let rules = Rules {
        lock_idle_minutes: Some(5),
        keep_messages_days: Some(90),
        backups: Some(BackupRule::KeyRequired),
        agents: Some(AgentRule::VerifiedOnly),
        ..Default::default()
    };
    assert_eq!(
        org::publish_rules(&c.0, &mut admin, rules.clone())
            .await
            .unwrap(),
        1
    );

    let me = phone.0.load_identity().unwrap();
    let mut r = Relay::connect(url.as_str(), &me).await.unwrap();
    let up = org::refresh(&phone.0, &mut r).await.unwrap();
    assert!(up.changed && !up.removed);
    let held = org::rules_in_force(&phone.0).unwrap().unwrap();
    assert_eq!(
        (held.version, held.keep_messages_days, held.agents),
        (1, Some(90), Some(AgentRule::VerifiedOnly))
    );
    // Nothing new: no change.
    assert!(!org::refresh(&phone.0, &mut r).await.unwrap().changed);

    // The relay refuses an older version and rules signed by another key.
    let a = org::admin(&c.0).unwrap().unwrap();
    let old = SignedRules::sign(
        &a.key().unwrap(),
        &Rules {
            v: 1,
            tenant: "northwind".into(),
            version: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(err_text(admin.publish_rules(&old).await).contains("stale_rules"));
    let forged = SignedRules::sign(
        &OwnerKey::generate(),
        &Rules {
            v: 1,
            tenant: "northwind".into(),
            version: 9,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(err_text(admin.publish_rules(&forged).await).contains("bad_rules"));

    // A second publish moves to version 2, and the console sees which version the phone fetched.
    org::publish_rules(
        &c.0,
        &mut admin,
        Rules {
            lock_idle_minutes: Some(10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    org::refresh(&phone.0, &mut r).await.unwrap();
    assert_eq!(
        org::rules_in_force(&phone.0)
            .unwrap()
            .unwrap()
            .lock_idle_minutes,
        Some(10)
    );
    assert_eq!(admin.devices().await.unwrap()[0].rules_version, 2);
}

#[tokio::test]
async fn the_retention_rule_deletes_older_messages_on_the_device() {
    let (c, url) = setup(50, false).await;
    let phone = dev("phone");
    let (token, _) = org::issue_token(&c.0, "", 3600).unwrap();
    org::join(&phone.0, &token, url.as_str().into(), None)
        .await
        .unwrap();
    let store = phone.0.messages().unwrap();
    let now = spike_core::clock::now_ms();
    for (id, age_days) in [("old", 40u64), ("new", 1)] {
        store
            .insert(
                &serde_json::from_value::<spike_cli::messages::StoredMessage>(serde_json::json!({
                    "id": id,
                    "peer": "someone",
                    "direction": spike_cli::messages::Direction::In,
                    "ts": now - age_days * 86_400_000,
                    "body": "text",
                }))
                .unwrap(),
            )
            .unwrap();
    }
    let mut admin = admin_conn(&c, &url).await;
    org::publish_rules(
        &c.0,
        &mut admin,
        Rules {
            keep_messages_days: Some(30),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut r = Relay::connect(url.as_str(), &phone.0.load_identity().unwrap())
        .await
        .unwrap();
    assert_eq!(org::refresh(&phone.0, &mut r).await.unwrap().pruned, 1);
    let left: Vec<String> = phone
        .0
        .messages()
        .unwrap()
        .all()
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(left, ["new"]);
}

#[tokio::test]
async fn a_revoked_device_is_refused_and_learns_it_was_removed() {
    // Open mode: a removed device may still sign in as an ordinary device and is told it left.
    let (c, url) = setup(50, true).await;
    let phone = dev("phone");
    let (token, _) = org::issue_token(&c.0, "", 3600).unwrap();
    org::join(&phone.0, &token, url.as_str().into(), None)
        .await
        .unwrap();
    let me = phone.0.load_identity().unwrap();
    let mut admin = admin_conn(&c, &url).await;
    admin.revoke(&me.device_id()).await.unwrap();
    assert!(admin.devices().await.unwrap().is_empty());
    let mut r = Relay::connect(url.as_str(), &me).await.unwrap();
    assert!(org::refresh(&phone.0, &mut r).await.unwrap().removed);
    assert!(org::rules_in_force(&phone.0).unwrap().is_none());

    // Enrolled mode.
    let (c, url) = setup(50, false).await;
    let phone = dev("phone2");
    let (token, _) = org::issue_token(&c.0, "", 3600).unwrap();
    org::join(&phone.0, &token, url.as_str().into(), None)
        .await
        .unwrap();
    let me = phone.0.load_identity().unwrap();
    admin_conn(&c, &url)
        .await
        .revoke(&me.device_id())
        .await
        .unwrap();
    // Enrolled mode: only a visitor from now on (no mailbox, nothing delivered), and told why.
    let mut r = Relay::connect(url.as_str(), &me).await.unwrap();
    assert!(r.visitor);
    assert!(org::refresh(&phone.0, &mut r).await.unwrap().removed);
    let e = err_text(ops::publish_keys(&phone.0, url.as_str()).await);
    assert!(e.contains("not_enrolled"), "{e}");
}

#[tokio::test]
async fn a_contact_on_another_relay_reaches_a_member_and_leaving_moves_back() {
    let home_relay = open_relay().await;
    let (c, org_url) = setup(50, false).await;
    let alice = dev("alice");
    let bob = dev("bob");
    for d in [&alice, &bob] {
        ops::publish_keys(&d.0, home_relay.as_str()).await.unwrap();
    }
    let befriend = |d: &Dev, name: &str, other: &Dev| {
        let mut cs = d.0.load_contacts().unwrap();
        cs.push(Contact {
            name: name.into(),
            card: other.0.identity_card().unwrap(),
            verified: true,
            key_changed: None,
            verified_at: None,
            devices_changed: None,
        });
        d.0.save_contacts(&cs).unwrap();
    };
    befriend(&alice, "bob", &bob);
    befriend(&bob, "alice", &alice);

    // Bob's phone joins Northwind's relay; his list on his old relay now points there.
    let (token, _) = org::issue_token(&c.0, "", 3600).unwrap();
    org::join(
        &bob.0,
        &token,
        org_url.as_str().into(),
        Some(home_relay.as_str().into()),
    )
    .await
    .unwrap();

    let r = ops::send(&alice.0, home_relay.as_str(), "bob", b"hello at work")
        .await
        .unwrap();
    assert_eq!(r.delivered(), 1, "{r:?}");
    let mut got = vec![];
    ops::recv(&bob.0, org_url.as_str(), IDLE, |o| got.push(o))
        .await
        .unwrap();
    assert!(
        got.iter()
            .any(|o| matches!(o, Outcome::Message { body, .. } if body == b"hello at work")),
        "{got:?}"
    );

    // Leaving: the organisation forgets the device and the list points back at the old relay.
    org::leave(
        &bob.0,
        Some(org_url.as_str().into()),
        home_relay.as_str().into(),
    )
    .await
    .unwrap();
    assert!(org::membership(&bob.0).unwrap().is_none());
    assert!(admin_conn(&c, &org_url)
        .await
        .devices()
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        bob.0.load_device_list().unwrap().devices[0]
            .relay
            .as_deref(),
        Some(home_relay.as_str())
    );
}

#[tokio::test]
async fn the_admin_interface_needs_the_admin_key_and_devices_cannot_use_it() {
    let (c, url) = setup(50, false).await;
    // Another key claiming the tenant.
    let impostor = console(&url);
    let e = err_text(
        AdminRelay::connect(
            url.as_str().into(),
            &org::admin(&impostor.0).unwrap().unwrap(),
        )
        .await,
    );
    assert!(e.contains("does not know this admin key"), "{e}");
    // A member asking for the device list on its own connection is refused.
    let phone = dev("phone");
    let (token, _) = org::issue_token(&c.0, "", 3600).unwrap();
    org::join(&phone.0, &token, url.as_str().into(), None)
        .await
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    use futures_util::{SinkExt, StreamExt};
    use spike_core::wire::{ClientMsg, ServerMsg};
    use tokio_tungstenite::tungstenite::Message;
    let Some(Ok(Message::Text(t))) = ws.next().await else {
        panic!()
    };
    let ServerMsg::Challenge { nonce } = serde_json::from_str(&t).unwrap() else {
        panic!()
    };
    let me = phone.0.load_identity().unwrap();
    let audience = url.trim_start_matches("ws://");
    let auth = ClientMsg::Auth {
        ed25519: hex::encode(me.ed_public()),
        sig: hex::encode(me.sign_auth(&hex::decode(nonce).unwrap(), audience)),
        enrol: None,
    };
    ws.send(Message::Text(serde_json::to_string(&auth).unwrap().into()))
        .await
        .unwrap();
    let _ready = ws.next().await;
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::ListDevices)
            .unwrap()
            .into(),
    ))
    .await
    .unwrap();
    let Some(Ok(Message::Text(t))) = ws.next().await else {
        panic!()
    };
    assert!(
        matches!(serde_json::from_str(&t).unwrap(), ServerMsg::Error { code, .. } if code == "forbidden")
    );
}
