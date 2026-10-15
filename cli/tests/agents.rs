//! Phase 2: agent requests with human approval, over a real relay.

use spike_cli::{
    agent::{self, Status},
    ops,
    state::{Contact, Home},
    Outcome,
};
use spike_core::envelope::{Envelope, Kind};
use spike_core::identity::Identity;
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase agents";
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
fn dev(tag: &str) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    Dev(Home::create(
        std::env::temp_dir().join(format!("spike-agent-{tag}-{n}")),
        PASS,
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap())
}
fn befriend(a: &Dev, name: &str, b: &Dev) {
    let mut c = a.0.load_contacts().unwrap();
    c.push(Contact {
        name: name.into(),
        card: b.0.identity_card().unwrap(),
        verified: true,
    });
    a.0.save_contacts(&c).unwrap();
}
fn key(d: &Dev) -> String {
    IdentityCard::decode(&d.0.identity_card().unwrap())
        .unwrap()
        .owner_hex()
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}

/// Human "alice" and her agent "bot".
async fn pair(url: &str) -> (Dev, Dev) {
    let (alice, bot) = (dev("alice"), dev("bot"));
    befriend(&alice, "bot", &bot);
    befriend(&bot, "alice", &alice);
    ops::publish_keys(&alice.0, url).await.unwrap();
    ops::publish_keys(&bot.0, url).await.unwrap();
    (alice, bot)
}
fn grant(alice: &Dev, bot: &Dev) {
    agent::grant(
        &alice.0,
        &key(bot),
        vec!["payments.send".into(), "files.read".into()],
        30,
        Some(5_000),
        Some(500),
        5,
    )
    .unwrap();
}

#[tokio::test]
async fn ask_approve_receipt_and_audit_trail() {
    let url = relay().await;
    let (alice, bot) = pair(&url).await;
    grant(&alice, &bot);

    // Over the auto-approve limit: waits for the human.
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
    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::AgentRequest { verdict, amount_cents: Some(1500), .. }] if verdict == "pending"),
        "{got:?}"
    );
    assert!(recv(&bot, &url).await.is_empty(), "nothing is decided yet");
    assert!(agent::list_inbox(&alice.0).unwrap()[0].status == Status::Pending);

    assert!(
        ops::agent_decide(&alice.0, &url, &id[..8], true, "ok, once")
            .await
            .unwrap()
    );
    let got = recv(&bot, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::AgentReceipt { approved: true, by_policy: false, note, .. }] if note == "ok, once"),
        "{got:?}"
    );
    let rec = &agent::list_outbox(&bot.0).unwrap()[0];
    assert!(rec
        .receipt
        .as_ref()
        .unwrap()
        .verify_for(&rec.signed.req)
        .is_ok());

    // Deciding twice is refused.
    assert!(agent::decide(&alice.0, &id[..8], false, "").is_err());

    // Small request: approved by policy, no human involved.
    ops::agent_request(
        &bot.0,
        &url,
        "alice",
        "payments.send",
        "Top up 4 dollars",
        Some(400),
        60,
    )
    .await
    .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::AgentRequest { verdict, .. }] if verdict == "approved by policy"),
        "{got:?}"
    );
    let got = recv(&bot, &url).await;
    assert!(
        matches!(
            got.as_slice(),
            [Outcome::AgentReceipt {
                approved: true,
                by_policy: true,
                ..
            }]
        ),
        "{got:?}"
    );

    // Out of scope: denied by policy, the agent is told why.
    ops::agent_request(
        &bot.0,
        &url,
        "alice",
        "admin.delete",
        "Delete everything",
        None,
        60,
    )
    .await
    .unwrap();
    recv(&alice, &url).await;
    let got = recv(&bot, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::AgentReceipt { approved: false, by_policy: true, note, .. }] if note.contains("not granted")),
        "{got:?}"
    );

    // The audit log verifies, and has no content, only scope and amount.
    let log = agent::verified_audit(&alice.0).unwrap();
    let events: Vec<&str> = log.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(
        events,
        ["grant", "request", "approved", "request", "approved", "request", "denied"]
    );
    assert!(log
        .iter()
        .all(|e| !e.detail.contains("GPU") && !e.detail.contains("Delete")));
}

#[tokio::test]
async fn tampering_with_the_audit_log_on_disk_is_caught() {
    let url = relay().await;
    let (alice, bot) = pair(&url).await;
    grant(&alice, &bot);
    ops::agent_request(&bot.0, &url, "alice", "files.read", "read notes", None, 60)
        .await
        .unwrap();
    recv(&alice, &url).await;
    assert_eq!(agent::verified_audit(&alice.0).unwrap().len(), 3);

    // Someone with the vault key rewrites an entry: the chain no longer verifies.
    let mut log: Vec<spike_core::agent::AuditEntry> = alice.0.load_json("agent-audit.enc").unwrap();
    log[1].event = "denied".into();
    alice.0.save_json("agent-audit.enc", &log).unwrap();
    let e = agent::verified_audit(&alice.0).unwrap_err().to_string();
    assert!(e.contains("tampered"), "{e}");
}

#[tokio::test]
async fn revoked_agents_cannot_be_approved_and_unknown_ones_are_denied() {
    let url = relay().await;
    let (alice, bot) = pair(&url).await;
    grant(&alice, &bot);
    let id = ops::agent_request(&bot.0, &url, "alice", "payments.send", "pay", Some(900), 60)
        .await
        .unwrap();
    recv(&alice, &url).await;
    assert!(agent::revoke(&alice.0, &key(&bot)).unwrap());
    let e = agent::decide(&alice.0, &id[..8], true, "")
        .unwrap_err()
        .to_string();
    assert!(e.contains("cannot approve"), "{e}");
    // Denying is still possible.
    assert!(
        ops::agent_decide(&alice.0, &url, &id[..8], false, "revoked")
            .await
            .unwrap()
    );
    let got = recv(&bot, &url).await;
    assert!(
        matches!(
            got.as_slice(),
            [Outcome::AgentReceipt {
                approved: false,
                ..
            }]
        ),
        "{got:?}"
    );

    // With no grant at all, a new request is denied by policy.
    ops::agent_request(&bot.0, &url, "alice", "files.read", "read", None, 60)
        .await
        .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::AgentRequest { verdict, .. }] if verdict.contains("no grant")),
        "{got:?}"
    );
}

#[tokio::test]
async fn forgeries_replays_and_rate_limits_are_refused() {
    let url = relay().await;
    let (alice, bot) = pair(&url).await;
    grant(&alice, &bot);
    let alice_key = key(&alice);

    // 1. A contact forwards someone else's signed request as its own.
    let other = dev("other");
    let forged = spike_core::agent::SignedRequest::sign(
        &other.0.load_owner().unwrap(),
        spike_core::agent::Request {
            id: "11".repeat(16),
            agent: key(&other),
            principal: alice_key.clone(),
            scope: "files.read".into(),
            action: "read".into(),
            amount_cents: None,
            created: now(),
            expires: now() + 60_000,
        },
    )
    .unwrap();
    ops::deliver_envelope(
        &bot.0,
        &url,
        "alice",
        Envelope::agent(Kind::AgentRequest, &forged.to_json()).unwrap(),
    )
    .await
    .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { reason }] if reason.contains("other than the sender")),
        "{got:?}"
    );

    // 2. A tampered request body (amount changed after signing).
    let mut tampered = spike_core::agent::SignedRequest::from_json(
        &spike_core::agent::SignedRequest::sign(
            &bot.0.load_owner().unwrap(),
            spike_core::agent::Request {
                id: "22".repeat(16),
                agent: key(&bot),
                principal: alice_key.clone(),
                scope: "payments.send".into(),
                action: "pay".into(),
                amount_cents: Some(1),
                created: now(),
                expires: now() + 60_000,
            },
        )
        .unwrap()
        .to_json(),
    )
    .unwrap();
    tampered.req.amount_cents = Some(4_999);
    ops::deliver_envelope(
        &bot.0,
        &url,
        "alice",
        Envelope::agent(Kind::AgentRequest, &tampered.to_json()).unwrap(),
    )
    .await
    .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { reason }] if reason.contains("bad signature")),
        "{got:?}"
    );

    // 3. The same signed request delivered twice is handled once.
    let id = ops::agent_request(&bot.0, &url, "alice", "payments.send", "pay", Some(900), 60)
        .await
        .unwrap();
    recv(&alice, &url).await;
    let again = agent::list_outbox(&bot.0)
        .unwrap()
        .last()
        .unwrap()
        .signed
        .to_json();
    ops::deliver_envelope(
        &bot.0,
        &url,
        "alice",
        Envelope::agent(Kind::AgentRequest, &again).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(recv(&alice, &url).await, vec![Outcome::Duplicate]);
    assert_eq!(agent::list_inbox(&alice.0).unwrap().len(), 1);
    let _ = id;

    // 4. The agent forges its own approval: the receipt is not signed by the principal.
    let req = agent::list_outbox(&bot.0).unwrap()[0].signed.req.clone();
    let mut body = spike_core::agent::ReceiptBody {
        request_id: req.id.clone(),
        request_hash: req.hash(),
        agent: req.agent.clone(),
        principal: req.principal.clone(),
        decision: spike_core::agent::Decision::Approved,
        by: spike_core::agent::DecidedBy::Human,
        decided: now(),
        note: String::new(),
        audit_seq: 99,
        audit_hash: "00".repeat(32),
    };
    assert!(spike_core::agent::Receipt::sign(&bot.0.load_owner().unwrap(), body.clone()).is_err());
    body.principal = key(&bot);
    // Even a receipt "signed by the agent" naming itself as principal is refused: wrong principal.
    let fake = spike_core::agent::Receipt::sign(&bot.0.load_owner().unwrap(), body).unwrap();
    let out = agent::on_receipt(&bot.0, "alice", &alice_key, &fake.to_json()).unwrap();
    assert!(matches!(out, Outcome::Rejected { .. }), "{out:?}");
    assert!(agent::list_outbox(&bot.0).unwrap()[0].receipt.is_none());

    // 5. Rate limit: the grant allows 5 per hour; the 6th is denied (1 above + 4 more here).
    for i in 0..5 {
        ops::agent_request(
            &bot.0,
            &url,
            "alice",
            "files.read",
            &format!("read {i}"),
            None,
            60,
        )
        .await
        .unwrap();
    }
    let got = recv(&alice, &url).await;
    let denied = got
        .iter()
        .filter(|o| matches!(o, Outcome::AgentRequest { verdict, .. } if verdict.contains("requests in an hour")))
        .count();
    assert_eq!(denied, 1, "{got:?}");
}

#[tokio::test]
async fn an_agent_that_sees_the_audit_log_go_backwards_is_warned() {
    let url = relay().await;
    let (alice, bot) = pair(&url).await;
    grant(&alice, &bot);
    for n in 0..2 {
        ops::agent_request(
            &bot.0,
            &url,
            "alice",
            "files.read",
            &format!("read {n}"),
            None,
            60,
        )
        .await
        .unwrap();
        recv(&alice, &url).await;
        recv(&bot, &url).await;
    }
    // Alice's log is rolled back to before the second decision and she decides again.
    let mut log: Vec<spike_core::agent::AuditEntry> = alice.0.load_json("agent-audit.enc").unwrap();
    log.truncate(2);
    alice.0.save_json("agent-audit.enc", &log).unwrap();
    ops::agent_request(&bot.0, &url, "alice", "files.read", "read 3", None, 60)
        .await
        .unwrap();
    recv(&alice, &url).await;
    let got = recv(&bot, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { reason }] if reason.contains("went backwards or forked")),
        "{got:?}"
    );
}

#[tokio::test]
async fn an_approval_is_honoured_by_a_service_exactly_once() {
    use spike_core::verifier::{redeem, Action, Config, Refusal};
    let url = relay().await;
    let (alice, bot) = pair(&url).await;
    grant(&alice, &bot);
    let id = ops::agent_request(
        &bot.0,
        &url,
        "alice",
        "payments.send",
        "Buy GPU",
        Some(1_500),
        60,
    )
    .await
    .unwrap();
    recv(&alice, &url).await;
    // Not answered yet: nothing to present.
    assert!(spike_cli::verifier::present(&bot.0, &id[..8], "pay.example").is_err());
    ops::agent_decide(&alice.0, &url, &id[..8], true, "")
        .await
        .unwrap();
    recv(&bot, &url).await;

    let p = spike_cli::verifier::present(&bot.0, &id[..8], "pay.example").unwrap();
    let db = std::env::temp_dir().join(format!("spike-spent-e2e-{}.db", &id[..8]));
    let cfg = Config {
        audience: "pay.example".into(),
        principals: vec![key(&alice)],
    };
    let pay = |cents| Action {
        scope: "payments.send",
        amount_cents: Some(cents),
    };
    let mut store = spike_cli::verifier::SqliteSpent::open(&db).unwrap();
    let t = now();
    // Too much, then a different service, are refused without using it up.
    assert!(matches!(
        redeem(&cfg, &mut store, &p, &pay(1_501), t),
        Err(Refusal::AmountTooHigh { .. })
    ));
    let other = Config {
        audience: "evil.example".into(),
        ..cfg.clone()
    };
    assert_eq!(
        redeem(&other, &mut store, &p, &pay(1_500), t),
        Err(Refusal::WrongAudience)
    );
    // Someone who is not alice cannot approve for this service.
    let not_alice = Config {
        principals: vec![key(&bot)],
        ..cfg.clone()
    };
    assert_eq!(
        redeem(&not_alice, &mut store, &p, &pay(1_500), t),
        Err(Refusal::UntrustedPrincipal)
    );
    let ok = redeem(&cfg, &mut store, &p, &pay(1_500), t).unwrap();
    assert_eq!(ok.principal, key(&alice));
    assert_eq!(ok.approved_cents, Some(1_500));
    // Used up, even after the service restarts.
    drop(store);
    let mut store = spike_cli::verifier::SqliteSpent::open(&db).unwrap();
    assert_eq!(
        redeem(&cfg, &mut store, &p, &pay(1_500), t),
        Err(Refusal::AlreadyUsed)
    );
    std::fs::remove_file(&db).ok();
    for ext in ["-wal", "-shm"] {
        std::fs::remove_file(format!("{}{ext}", db.display())).ok();
    }

    // A denied request cannot be presented at all.
    let id2 = ops::agent_request(&bot.0, &url, "alice", "admin.delete", "Wipe", None, 60)
        .await
        .unwrap();
    recv(&alice, &url).await;
    recv(&bot, &url).await;
    assert!(spike_cli::verifier::present(&bot.0, &id2[..8], "pay.example").is_err());
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
