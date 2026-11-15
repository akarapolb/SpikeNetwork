//! Tenants and enrollment tokens (ADR 0023), end to end: a real relay on loopback with its
//! operator listener, real clients, real signatures.

use spike_cli::client::Relay;
use spike_cli::state::Home;
use spike_cli::transport::{Enrollments, Target, TransportOpts};
use spike_core::enroll::{AdminOp, AdminRequest, EnrollToken, SigningKey, TenantLimits};
use spike_core::identity::Identity;
use spike_core::vault::KdfParams;
use spike_core::wire::{ClientMsg, ServerMsg};
use spike_relay::{admin_client, serve_with_metrics, Config};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct TestRelay {
    url: String,
    admin_addr: String,
    operator: SigningKey,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn start(tweak: impl FnOnce(&mut Config)) -> TestRelay {
    let operator = SigningKey::generate();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let m = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let admin_addr = m.local_addr().unwrap().to_string();
    let mut cfg = Config {
        require_enrollment: true,
        operator_key: Some(operator.public_hex()),
        ..Config::default()
    };
    tweak(&mut cfg);
    tokio::spawn(serve_with_metrics(l, Some(m), cfg));
    TestRelay {
        url,
        admin_addr,
        operator,
    }
}

impl TestRelay {
    async fn admin(&self, op: AdminOp) -> admin_client::Answer {
        admin_client::send(&self.admin_addr, &self.operator, &op)
            .await
            .unwrap()
    }

    /// Register a tenant and return its admin key.
    async fn tenant(&self, id: &str, limits: TenantLimits) -> SigningKey {
        let admin = SigningKey::generate();
        let a = self
            .admin(AdminOp::CreateTenant {
                tenant: id.into(),
                admin_pub: admin.public_hex(),
                limits,
            })
            .await;
        assert!(a.ok, "{:?}", a.body);
        admin
    }

    fn target(&self, token: Option<&str>) -> Target {
        let mut enroll = Enrollments::default();
        if let Some(t) = token {
            enroll.insert(&self.url, t);
        }
        Target {
            url: self.url.clone(),
            opts: TransportOpts {
                enroll,
                ..TransportOpts::default()
            },
        }
    }

    async fn sign_in(&self, who: &Identity, token: Option<&str>) -> anyhow::Result<Relay> {
        Relay::connect(self.target(token), who).await
    }

    /// Enroll a fresh device in `tenant` and return it with its open connection.
    async fn member(&self, admin: &SigningKey, tenant: &str) -> (Identity, Relay) {
        let id = Identity::generate().unwrap();
        let tok = admin
            .issue(tenant, &id.device_id(), now() + 3600)
            .unwrap()
            .encode();
        let r = self.sign_in(&id, Some(&tok)).await.unwrap();
        assert_eq!(r.tenant.as_deref(), Some(tenant));
        (id, r)
    }
}

fn refused(r: anyhow::Result<Relay>, code: &str) {
    match r {
        Ok(_) => panic!("expected a refusal with {code}, was admitted"),
        Err(e) => assert!(e.to_string().contains(code), "expected {code}: {e}"),
    }
}

fn no_visitors(c: &mut Config) {
    c.enroll_visitors = false;
}

#[tokio::test]
async fn an_enrolled_device_is_admitted_and_stays_enrolled_without_the_token() {
    let relay = start(no_visitors).await;
    let admin = relay.tenant("t-acme", TenantLimits::default()).await;
    let (id, mut r) = relay.member(&admin, "t-acme").await;
    r.ping().await.unwrap();
    drop(r);
    // The enrollment lasts: no token needed the next time, and presenting it again is harmless.
    let r = relay.sign_in(&id, None).await.unwrap();
    assert_eq!(r.tenant.as_deref(), Some("t-acme"));
    let tok = admin.issue("t-acme", &id.device_id(), now() + 60).unwrap();
    let r = relay.sign_in(&id, Some(&tok.encode())).await.unwrap();
    assert_eq!(r.tenant.as_deref(), Some("t-acme"));
}

#[tokio::test]
async fn an_unenrolled_device_is_refused_with_words() {
    let relay = start(no_visitors).await;
    relay.tenant("t-acme", TenantLimits::default()).await;
    let e = relay
        .sign_in(&Identity::generate().unwrap(), None)
        .await
        .err()
        .expect("refused")
        .to_string();
    assert!(e.contains("not_enrolled"), "{e}");
    assert!(e.contains("spike join"), "the refusal says what to do: {e}");
}

#[tokio::test]
async fn expired_tokens_are_refused() {
    let relay = start(no_visitors).await;
    let admin = relay.tenant("t-acme", TenantLimits::default()).await;
    let id = Identity::generate().unwrap();
    let tok = admin.issue("t-acme", &id.device_id(), now() - 1).unwrap();
    refused(
        relay.sign_in(&id, Some(&tok.encode())).await,
        "enroll_expired",
    );
    // A token valid for longer than the relay allows to redeem is refused too.
    let tok = admin
        .issue("t-acme", &id.device_id(), now() + 30 * 24 * 3600)
        .unwrap();
    refused(
        relay.sign_in(&id, Some(&tok.encode())).await,
        "enroll_invalid",
    );
}

#[tokio::test]
async fn wrong_tenant_tokens_are_refused() {
    let relay = start(no_visitors).await;
    let acme = relay.tenant("t-acme", TenantLimits::default()).await;
    let globex = relay.tenant("t-globex", TenantLimits::default()).await;
    let id = Identity::generate().unwrap();
    // Globex's admin cannot sign a token for Acme.
    let forged = globex.issue("t-acme", &id.device_id(), now() + 60).unwrap();
    refused(
        relay.sign_in(&id, Some(&forged.encode())).await,
        "enroll_invalid",
    );
    // A tenant that does not exist.
    let ghost = acme.issue("t-ghost", &id.device_id(), now() + 60).unwrap();
    refused(
        relay.sign_in(&id, Some(&ghost.encode())).await,
        "enroll_invalid",
    );
    // A device enrolled in Acme cannot also join Globex.
    let (member, _r) = relay.member(&acme, "t-acme").await;
    let other = globex
        .issue("t-globex", &member.device_id(), now() + 60)
        .unwrap();
    refused(
        relay.sign_in(&member, Some(&other.encode())).await,
        "enroll_conflict",
    );
}

#[tokio::test]
async fn tampered_and_borrowed_tokens_are_refused() {
    let relay = start(no_visitors).await;
    let admin = relay.tenant("t-acme", TenantLimits::default()).await;
    let alice = Identity::generate().unwrap();
    let mallory = Identity::generate().unwrap();
    let tok = admin
        .issue("t-acme", &alice.device_id(), now() + 60)
        .unwrap();
    // Mallory presents Alice's token.
    refused(
        relay.sign_in(&mallory, Some(&tok.encode())).await,
        "enroll_invalid",
    );
    // Mallory rewrites it for herself, or stretches its expiry: the signature no longer matches.
    let mut changed = tok.clone();
    changed.device_id = mallory.device_id();
    refused(
        relay.sign_in(&mallory, Some(&changed.encode())).await,
        "enroll_invalid",
    );
    let mut later = tok.clone();
    later.not_after += 3600;
    refused(
        relay.sign_in(&alice, Some(&later.encode())).await,
        "enroll_invalid",
    );
    // Garbage.
    refused(
        relay.sign_in(&alice, Some("spike-enroll1:AAAA")).await,
        "enroll_invalid",
    );
    // The untouched token still works for Alice.
    relay.sign_in(&alice, Some(&tok.encode())).await.unwrap();
}

#[tokio::test]
async fn the_device_limit_is_enforced() {
    let relay = start(no_visitors).await;
    let admin = relay
        .tenant(
            "t-small",
            TenantLimits {
                devices: 2,
                ..TenantLimits::default()
            },
        )
        .await;
    relay.member(&admin, "t-small").await;
    relay.member(&admin, "t-small").await;
    let third = Identity::generate().unwrap();
    let tok = admin
        .issue("t-small", &third.device_id(), now() + 60)
        .unwrap();
    let e = relay
        .sign_in(&third, Some(&tok.encode()))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("tenant_over_limit"), "{e}");
    assert!(e.contains("no room for another device"), "{e}");
}

#[tokio::test]
async fn the_queue_depth_limit_is_enforced_per_recipient() {
    let relay = start(no_visitors).await;
    let admin = relay
        .tenant(
            "t-q",
            TenantLimits {
                queue_depth: 2,
                ..TenantLimits::default()
            },
        )
        .await;
    let (bob, bob_conn) = relay.member(&admin, "t-q").await;
    drop(bob_conn);
    let (_alice, mut a) = relay.member(&admin, "t-q").await;
    a.send(&bob.device_id(), b"one").await.unwrap();
    a.send(&bob.device_id(), b"two").await.unwrap();
    let e = a.send(&bob.device_id(), b"three").await.unwrap_err();
    assert!(
        e.to_string().contains("tenant_over_limit: queue_depth"),
        "{e}"
    );
}

#[tokio::test]
async fn the_attachment_bytes_limit_covers_the_whole_tenant() {
    let relay = start(no_visitors).await;
    let admin = relay
        .tenant(
            "t-att",
            TenantLimits {
                attachment_bytes: 300,
                ..TenantLimits::default()
            },
        )
        .await;
    let (_a, mut a) = relay.member(&admin, "t-att").await;
    let (_b, mut b) = relay.member(&admin, "t-att").await;
    a.put_blob_chunk(&"a".repeat(32), 0, 1, &[1u8; 200])
        .await
        .unwrap();
    // Another device of the same tenant shares the limit.
    let e = b
        .put_blob_chunk(&"b".repeat(32), 0, 1, &[2u8; 200])
        .await
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("tenant_over_limit: attachment_bytes"),
        "{e}"
    );
    b.put_blob_chunk(&"b".repeat(32), 0, 1, &[2u8; 90])
        .await
        .unwrap();
}

#[tokio::test]
async fn the_connection_limit_is_enforced_and_released() {
    let relay = start(no_visitors).await;
    let admin = relay
        .tenant(
            "t-c",
            TenantLimits {
                connections: 1,
                ..TenantLimits::default()
            },
        )
        .await;
    let (id, first) = relay.member(&admin, "t-c").await;
    refused(relay.sign_in(&id, None).await, "tenant_over_limit");
    drop(first);
    // The place is freed when the connection ends.
    let mut ok = false;
    for _ in 0..50 {
        if relay.sign_in(&id, None).await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ok, "the connection place came back");
}

#[tokio::test]
async fn the_request_rate_limit_is_shared_by_the_tenant() {
    let relay = start(no_visitors).await;
    let admin = relay
        .tenant(
            "t-r",
            TenantLimits {
                req_rate: 1.0,
                ..TenantLimits::default()
            },
        )
        .await;
    let (_a, mut a) = relay.member(&admin, "t-r").await;
    let (_b, mut b) = relay.member(&admin, "t-r").await;
    let fetch = || ClientMsg::FetchDeviceList {
        owner: "ab".repeat(32),
    };
    // The burst is 50 requests for the whole tenant: two devices together run out.
    let mut limited = false;
    for i in 0..60 {
        let c = if i % 2 == 0 { &mut a } else { &mut b };
        if let ServerMsg::Error { code, message } = c.request(fetch()).await.unwrap() {
            assert_eq!(code, "tenant_over_limit");
            assert!(message.starts_with("req_rate"), "{message}");
            limited = true;
            break;
        }
    }
    assert!(limited, "the tenant's shared budget ran out");
    // Pings are never refused: keepalive keeps working.
    a.ping().await.unwrap();
}

#[tokio::test]
async fn suspend_refuses_and_disconnects_and_resume_restores() {
    let relay = start(no_visitors).await;
    let admin = relay.tenant("t-s", TenantLimits::default()).await;
    let (bob, bob_conn) = relay.member(&admin, "t-s").await;
    drop(bob_conn);
    let (alice, mut a) = relay.member(&admin, "t-s").await;
    a.send(&bob.device_id(), b"kept while suspended")
        .await
        .unwrap();
    assert!(
        relay
            .admin(AdminOp::Suspend {
                tenant: "t-s".into()
            })
            .await
            .ok
    );
    // The live connection is closed.
    let mut closed = false;
    for _ in 0..20 {
        if a.ping().await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "a suspended tenant's connections are closed");
    refused(relay.sign_in(&alice, None).await, "tenant_suspended");
    assert!(
        relay
            .admin(AdminOp::Resume {
                tenant: "t-s".into()
            })
            .await
            .ok
    );
    // Nothing was deleted.
    let mut b = relay.sign_in(&bob, None).await.unwrap();
    let d = b
        .next_delivery(Some(Duration::from_secs(2)))
        .await
        .unwrap()
        .expect("the queued message survived");
    assert_eq!(d.blob, b"kept while suspended");
}

#[tokio::test]
async fn delete_removes_the_tenant_and_its_queued_ciphertext() {
    let relay = start(no_visitors).await;
    let admin = relay.tenant("t-d", TenantLimits::default()).await;
    let (bob, bob_conn) = relay.member(&admin, "t-d").await;
    drop(bob_conn);
    let (alice, mut a) = relay.member(&admin, "t-d").await;
    a.send(&bob.device_id(), b"to be deleted").await.unwrap();
    a.put_blob_chunk(&"c".repeat(32), 0, 1, &[3u8; 100])
        .await
        .unwrap();
    let usage = relay
        .admin(AdminOp::Usage {
            tenant: Some("t-d".into()),
        })
        .await;
    assert!(usage.ok);
    assert_eq!(usage.body[0]["queued_messages"], 1);
    assert_eq!(usage.body[0]["attachment_bytes"], 100);
    assert_eq!(usage.body[0]["devices"], 2);

    let gone = relay
        .admin(AdminOp::Delete {
            tenant: "t-d".into(),
        })
        .await;
    assert!(gone.ok, "{:?}", gone.body);
    assert_eq!(gone.body["devices_removed"], 2);
    refused(relay.sign_in(&alice, None).await, "not_enrolled");
    let usage = relay.admin(AdminOp::Usage { tenant: None }).await;
    assert_eq!(usage.body, serde_json::json!([]));

    // Re-created with the same id, the old devices start from nothing: no queue, no chunks.
    let admin = relay.tenant("t-d", TenantLimits::default()).await;
    let tok = admin.issue("t-d", &bob.device_id(), now() + 60).unwrap();
    let mut b = relay.sign_in(&bob, Some(&tok.encode())).await.unwrap();
    assert!(b
        .next_delivery(Some(Duration::from_millis(500)))
        .await
        .unwrap()
        .is_none());
    assert!(b
        .get_blob_chunk(&"c".repeat(32), 0)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn revocation_by_the_admin_key_removes_a_device() {
    let relay = start(no_visitors).await;
    let admin = relay.tenant("t-v", TenantLimits::default()).await;
    let bob = Identity::generate().unwrap();
    let bob_token = admin
        .issue("t-v", &bob.device_id(), now() + 3600)
        .unwrap()
        .encode();
    let mut b = relay.sign_in(&bob, Some(&bob_token)).await.unwrap();
    let (_alice, mut a) = relay.member(&admin, "t-v").await;

    // Someone without the admin key cannot revoke.
    let forged = SigningKey::generate()
        .revoke("t-v", &bob.device_id())
        .unwrap();
    let e = a.revoke(&forged.encode()).await.unwrap_err();
    assert!(e.to_string().contains("bad_signature"), "{e}");

    let rev = admin.revoke("t-v", &bob.device_id()).unwrap();
    a.revoke(&rev.encode()).await.unwrap();
    let mut closed = false;
    for _ in 0..20 {
        if b.ping().await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "the revoked device's connection is closed");
    refused(relay.sign_in(&bob, None).await, "not_enrolled");
    // The token that admitted it cannot admit it again.
    refused(
        relay.sign_in(&bob, Some(&bob_token)).await,
        "enroll_revoked",
    );
}

#[tokio::test]
async fn visitors_can_reach_members_but_store_nothing_of_their_own() {
    let relay = start(|_| {}).await;
    let admin = relay.tenant("t-x", TenantLimits::default()).await;
    let (member, mut m) = relay.member(&admin, "t-x").await;
    let outsider = Identity::generate().unwrap();
    let mut v = relay.sign_in(&outsider, None).await.unwrap();
    assert_eq!(v.tenant, None, "signed in as a visitor");

    v.send(&member.device_id(), b"hello from elsewhere")
        .await
        .unwrap();
    let d = m
        .next_delivery(Some(Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.blob, b"hello from elsewhere");

    // A visitor cannot use the relay as storage for itself or for strangers.
    let e = v
        .send(&Identity::generate().unwrap().device_id(), b"x")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not_enrolled"), "{e}");
    let e = v
        .publish_prekeys(&Identity::generate().unwrap().fresh_prekeys(1))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not_enrolled"), "{e}");
    let e = v
        .put_blob_chunk(&"d".repeat(32), 0, 1, &[0u8; 64])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not_enrolled"), "{e}");
}

#[tokio::test]
async fn every_storage_action_is_refused_to_a_visitor_and_reaching_actions_are_not() {
    use spike_core::wire::{GroupKind, WirePrekey};
    let relay = start(|c| {
        c.turn_urls = vec!["turn:turn.example:3478".into()];
        c.turn_secret = Some("s".into());
    })
    .await;
    let admin = relay.tenant("t-b", TenantLimits::default()).await;
    let (member, _m) = relay.member(&admin, "t-b").await;
    let mut v = relay
        .sign_in(&Identity::generate().unwrap(), None)
        .await
        .unwrap();
    let key = || WirePrekey {
        key: "00".repeat(32),
        sig: "00".repeat(64),
    };
    let refused_to_visitors = [
        ClientMsg::PublishPrekeys { keys: vec![key()] },
        ClientMsg::PublishFallback { key: key() },
        ClientMsg::PublishDeviceList { list: "{}".into() },
        ClientMsg::PublishKeyPackages {
            packages: vec!["01".into()],
        },
        ClientMsg::PublishLastResortKeyPackage {
            package: "01".into(),
        },
        ClientMsg::PutBlobChunk {
            blob: "e".repeat(32),
            index: 0,
            total: 1,
            data: "00".repeat(64),
        },
        ClientMsg::GroupCreate {
            mailbox: "f".repeat(32),
            pubkey: "00".repeat(32),
            sig: "00".repeat(64),
        },
        ClientMsg::RegisterPush {
            url: "https://push.example/x".into(),
        },
        ClientMsg::UnregisterPush,
        ClientMsg::TurnCredentials,
    ];
    for msg in refused_to_visitors {
        let shown = format!("{msg:?}");
        match v.request(msg).await.unwrap() {
            ServerMsg::Error { code, .. } => assert_eq!(code, "not_enrolled", "{shown}"),
            other => panic!("a visitor was allowed {shown}: {other:?}"),
        }
    }
    // What reaches members is not refused as such (it may fail for its own reasons).
    let reaching = [
        ClientMsg::ClaimPrekey {
            device: member.device_id(),
        },
        ClientMsg::ClaimKeyPackage {
            device: member.device_id(),
        },
        ClientMsg::FetchDeviceList {
            owner: "ab".repeat(32),
        },
        ClientMsg::GetBlobChunk {
            blob: "e".repeat(32),
            index: 0,
        },
        ClientMsg::GroupRead {
            mailbox: "f".repeat(32),
            after_seq: 0,
            limit: 10,
            sig: "00".repeat(64),
        },
        ClientMsg::GroupAppend {
            mailbox: "f".repeat(32),
            kind: GroupKind::Message,
            expected_seq: None,
            blob: "00".into(),
            seal: None,
            sig: "00".repeat(64),
        },
        ClientMsg::GroupSubscribe {
            mailbox: "f".repeat(32),
            sig: "00".repeat(64),
        },
    ];
    for msg in reaching {
        let shown = format!("{msg:?}");
        if let ServerMsg::Error { code, .. } = v.request(msg).await.unwrap() {
            assert_ne!(code, "not_enrolled", "{shown}");
        }
    }
    v.ping().await.unwrap();
}

#[tokio::test]
async fn visitor_sends_count_against_the_recipient_tenants_queue_depth() {
    let relay = start(|_| {}).await;
    let admin = relay
        .tenant(
            "t-vq",
            TenantLimits {
                queue_depth: 2,
                ..TenantLimits::default()
            },
        )
        .await;
    let (member, conn) = relay.member(&admin, "t-vq").await;
    drop(conn);
    let mut v = relay
        .sign_in(&Identity::generate().unwrap(), None)
        .await
        .unwrap();
    v.send(&member.device_id(), b"1").await.unwrap();
    v.send(&member.device_id(), b"2").await.unwrap();
    let e = v.send(&member.device_id(), b"3").await.unwrap_err();
    assert!(
        e.to_string().contains("tenant_over_limit: queue_depth"),
        "{e}"
    );
    // The refusal is counted for the recipient's tenant.
    let usage = relay
        .admin(AdminOp::Usage {
            tenant: Some("t-vq".into()),
        })
        .await;
    assert_eq!(usage.body[0]["refused_over_limit"], 1);
    assert_eq!(usage.body[0]["queued_messages"], 2);
}

#[tokio::test]
async fn visitors_can_be_switched_off() {
    let relay = start(no_visitors).await;
    relay.tenant("t-closed", TenantLimits::default()).await;
    refused(
        relay.sign_in(&Identity::generate().unwrap(), None).await,
        "not_enrolled",
    );
}

#[tokio::test]
async fn open_mode_is_unchanged_and_ignores_tokens() {
    let relay = start(|c| c.require_enrollment = false).await;
    let admin = relay.tenant("t-o", TenantLimits::default()).await;
    let stranger = Identity::generate().unwrap();
    let mut r = relay.sign_in(&stranger, None).await.unwrap();
    assert_eq!(r.tenant, None);
    let mut me = Identity::generate().unwrap();
    let keys = me.fresh_prekeys(2);
    let mut mine = relay.sign_in(&me, None).await.unwrap();
    assert_eq!(mine.publish_prekeys(&keys).await.unwrap(), 2);
    // A token (even a bad one) changes nothing in open mode.
    let tok = admin.issue("t-o", &me.device_id(), now() - 10).unwrap();
    let again = relay.sign_in(&me, Some(&tok.encode())).await.unwrap();
    assert_eq!(again.tenant, None);
    r.send(&me.device_id(), b"open as ever").await.unwrap();
}

/// POST a hand-made body to the operator listener; returns the HTTP status line.
async fn raw_admin(addr: &str, body: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST /admin HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.lines().next().unwrap_or("").to_string()
}

#[tokio::test]
async fn operator_requests_need_the_operator_key_and_cannot_be_replayed() {
    let relay = start(no_visitors).await;
    let op = AdminOp::CreateTenant {
        tenant: "t-op".into(),
        admin_pub: SigningKey::generate().public_hex(),
        limits: TenantLimits::default(),
    };
    // Signed by someone else.
    let stranger = SigningKey::generate();
    let a = admin_client::send(&relay.admin_addr, &stranger, &op)
        .await
        .unwrap();
    assert!(!a.ok);
    // Signed properly, then sent a second time.
    let req: AdminRequest = relay.operator.sign_admin(&op, now()).unwrap();
    let body = serde_json::to_string(&req).unwrap();
    assert!(raw_admin(&relay.admin_addr, &body).await.contains("200"));
    assert!(raw_admin(&relay.admin_addr, &body).await.contains("401"));
    // Too old.
    let old = relay.operator.sign_admin(&op, now() - 3600).unwrap();
    let body = serde_json::to_string(&old).unwrap();
    assert!(raw_admin(&relay.admin_addr, &body).await.contains("401"));
}

#[tokio::test]
async fn the_join_step_stores_the_token_sealed_and_shows_it_only_to_its_relay() {
    let dir = std::env::temp_dir().join(format!(
        "spike-test-join-{}",
        hex::encode(&Identity::generate().unwrap().ed_public()[..6])
    ));
    let home = Home::create(
        &dir,
        "join test passphrase",
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap();
    let relay = start(no_visitors).await;
    let admin = relay.tenant("t-j", TenantLimits::default()).await;
    let me = home.load_identity().unwrap();
    let tok = admin
        .issue("t-j", &me.device_id(), now() + 600)
        .unwrap()
        .encode();
    let mut e = home.load_enrollments().unwrap();
    e.insert(&relay.url, &tok);
    home.save_enrollments(&e).unwrap();

    // Sealed: the token is not in any file in plain text.
    for f in std::fs::read_dir(&dir).unwrap() {
        let bytes = std::fs::read(f.unwrap().path()).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("spike-enroll1:"),
            "a token was written in plain text"
        );
    }
    let back = Home::unlock(&dir, "join test passphrase")
        .unwrap()
        .load_enrollments()
        .unwrap();
    assert_eq!(back.for_url(&relay.url), Some(tok.as_str()));
    // Only this relay gets it: another relay's address has no token.
    assert_eq!(back.for_url("wss://relay.elsewhere.example"), None);
    assert!(!format!("{back:?}").contains("spike-enroll1:"));

    let r = Relay::connect(
        Target {
            url: relay.url.clone(),
            opts: TransportOpts {
                enroll: back,
                ..TransportOpts::default()
            },
        },
        &me,
    )
    .await
    .unwrap();
    assert_eq!(r.tenant.as_deref(), Some("t-j"));
    assert!(EnrollToken::decode(&tok).is_ok());
}
