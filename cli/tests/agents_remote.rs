//! Phase 2: deciding an agent request from a device other than the approver.

use spike_cli::{
    agent, ops,
    state::{Contact, Home},
    Outcome,
};
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase remote approval";
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
                std::env::temp_dir().join(format!("spike-ar-{tag}{i}-{id}")),
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
fn owner_key(d: &Dev) -> String {
    IdentityCard::decode(&d.0.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}

/// Alice has a desktop (first in her list: the approver) and a phone; `bot` is her agent.
async fn world() -> (String, Dev, Dev, Dev) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    let (mut alice, ac) = person("alice", 2);
    let (mut bot, bc) = person("bot", 1);
    befriend(&alice, "bot", &bc);
    befriend(&bot, "alice", &ac);
    for d in alice.iter().chain(bot.iter()) {
        ops::publish_keys(&d.0, &url).await.unwrap();
    }
    let phone = alice.pop().unwrap();
    let desktop = alice.pop().unwrap();
    (url, desktop, phone, bot.pop().unwrap())
}
fn grant(desktop: &Dev, bot: &Dev) {
    agent::grant(
        &desktop.0,
        &owner_key(bot),
        vec!["payments.send".into()],
        30,
        Some(5_000),
        Some(500),
        10,
        None,
    )
    .unwrap();
}

#[tokio::test]
async fn a_request_can_be_approved_from_the_phone() {
    let (url, desktop, phone, bot) = world().await;
    grant(&desktop, &bot);
    let id = ops::agent_request(
        &bot.0,
        &url,
        "alice",
        "payments.send",
        "Buy 3 GPU hours",
        Some(1_500),
        60,
    )
    .await
    .unwrap();

    // The desktop (approver) holds it and tells the phone; the phone does not decide alone.
    let got = recv(&desktop, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::AgentRequest { verdict, .. }] if verdict == "pending"),
        "{got:?}"
    );
    let got = recv(&phone, &url).await;
    assert!(
        got.iter().any(|o| matches!(o, Outcome::AgentRemotePending { id: i, amount_cents: Some(1500), .. } if *i == id)),
        "{got:?}"
    );
    assert!(agent::list_inbox(&phone.0).unwrap().is_empty());
    assert_eq!(agent::list_remote(&phone.0).unwrap().len(), 1);
    assert!(recv(&bot, &url).await.is_empty(), "nothing is decided yet");

    // Approve on the phone: forwarded, applied and logged by the desktop.
    assert!(
        ops::agent_decide(&phone.0, &url, &id[..8], true, "from my phone")
            .await
            .unwrap()
    );
    assert!(agent::list_remote(&phone.0).unwrap().is_empty());
    assert!(
        ops::agent_decide(&phone.0, &url, &id[..8], true, "")
            .await
            .is_err(),
        "a forwarded request is decided once"
    );
    assert!(
        recv(&bot, &url).await.is_empty(),
        "the phone sends no receipt itself"
    );
    let got = recv(&desktop, &url).await;
    assert!(
        matches!(
            got.as_slice(),
            [Outcome::AgentRemoteDecision { approved: true, .. }]
        ),
        "{got:?}"
    );
    let got = recv(&bot, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::AgentReceipt { approved: true, by_policy: false, note, .. }] if note == "from my phone"),
        "{got:?}"
    );

    // One audit chain, on the desktop only, and it verifies.
    let events: Vec<String> = agent::verified_audit(&desktop.0)
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events, ["grant", "request", "approved"]);
    assert!(agent::verified_audit(&phone.0).unwrap().is_empty());
}

#[tokio::test]
async fn the_desktop_still_enforces_the_grant_on_a_forwarded_decision() {
    let (url, desktop, phone, bot) = world().await;
    grant(&desktop, &bot);
    let id = ops::agent_request(&bot.0, &url, "alice", "payments.send", "Pay", Some(900), 60)
        .await
        .unwrap();
    recv(&desktop, &url).await;
    recv(&phone, &url).await;
    // Alice revokes at the desktop, then approves on the phone before it knows.
    assert!(agent::revoke(&desktop.0, &owner_key(&bot)).unwrap());
    assert!(ops::agent_decide(&phone.0, &url, &id[..8], true, "")
        .await
        .unwrap());
    let got = recv(&desktop, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { reason }] if reason.contains("cannot approve")),
        "{got:?}"
    );
    assert!(
        recv(&bot, &url).await.is_empty(),
        "no receipt for a refused approval"
    );
    assert!(agent::list_inbox(&desktop.0).unwrap()[0].status == agent::Status::Pending);
    // Denying still works from the desktop.
    assert!(agent::decide(&desktop.0, &id[..8], false, "revoked").is_ok());
}

#[tokio::test]
async fn forwarded_requests_are_only_trusted_from_the_approver() {
    let (url, desktop, phone, bot) = world().await;
    grant(&desktop, &bot);
    ops::agent_request(&bot.0, &url, "alice", "payments.send", "Pay", Some(900), 60)
        .await
        .unwrap();
    recv(&desktop, &url).await;
    let pending = agent::list_inbox(&desktop.0).unwrap().remove(0);

    // The phone (not the approver) tries to plant a request on the desktop: ignored.
    let msg = spike_cli::sync::SyncMsg::AgentPending {
        request: pending.signed.clone(),
        from: "bot".into(),
    };
    let my_id = desktop.0.load_identity().unwrap().device_id();
    let phone_id = phone.0.load_identity().unwrap().device_id();
    assert_eq!(
        agent::on_pending(&desktop.0, &my_id, &phone_id, "bot", pending.signed.clone()).unwrap(),
        Outcome::Ignored,
        "the approver never takes forwarded requests"
    );
    // A forwarded request from a non-approver device is refused on the phone.
    assert!(matches!(
        agent::on_pending(
            &phone.0,
            &phone_id,
            &phone_id,
            "bot",
            pending.signed.clone()
        )
        .unwrap(),
        Outcome::Rejected { .. }
    ));
    // A decision arriving at a non-approver device is ignored; it does not log anything.
    assert_eq!(
        agent::on_remote_decision(&phone.0, &phone_id, &pending.signed.req.id, true, "").unwrap(),
        Outcome::Ignored
    );
    assert!(agent::verified_audit(&phone.0).unwrap().is_empty());
    // Unknown request ids on the approver are refused, not logged as decisions.
    let unknown = "ab".repeat(16);
    assert!(matches!(
        agent::on_remote_decision(&desktop.0, &my_id, &unknown, true, "").unwrap(),
        Outcome::Rejected { .. }
    ));
    // The wire form validates the agent's signature.
    let mut forged = pending.signed.clone();
    forged.req.amount_cents = Some(1);
    let bad = spike_cli::sync::SyncMsg::AgentPending {
        request: forged,
        from: "bot".into(),
    };
    assert!(spike_cli::sync::SyncMsg::from_envelope(&bad.to_envelope().unwrap()).is_err());
    assert!(spike_cli::sync::SyncMsg::from_envelope(&msg.to_envelope().unwrap()).is_ok());
}
