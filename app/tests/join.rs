//! Joining an organisation's relay from the app (ADR 0023): the desktop's "Join" step, against a
//! real relay that requires enrollment.

use spike_app::Session;
use spike_core::enroll::{AdminOp, SigningKey, TenantLimits};
use spike_core::vault::KdfParams;
use spike_relay::{admin_client, serve, serve_with_metrics, Config};
use std::path::PathBuf;

struct Person {
    s: Session,
    dir: PathBuf,
}
impl Drop for Person {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn person(tag: &str) -> Person {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spike-join-{tag}-{}-{n}", std::process::id()));
    let s = Session::create(
        &dir,
        "join test passphrase".into(),
        KdfParams::insecure_for_tests(),
    )
    .unwrap();
    Person { s, dir }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// An enrolled relay with tenant `t-acme`; returns its address and the tenant's admin key.
async fn org_relay() -> (String, SigningKey) {
    let operator = SigningKey::generate();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let m = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let admin_addr = m.local_addr().unwrap().to_string();
    tokio::spawn(serve_with_metrics(
        l,
        Some(m),
        Config {
            require_enrollment: true,
            enroll_visitors: false,
            operator_key: Some(operator.public_hex()),
            ..Config::default()
        },
    ));
    let admin = SigningKey::generate();
    let a = admin_client::send(
        &admin_addr,
        &operator,
        &AdminOp::CreateTenant {
            tenant: "t-acme".into(),
            admin_pub: admin.public_hex(),
            limits: TenantLimits::default(),
        },
    )
    .await
    .unwrap();
    assert!(a.ok, "{:?}", a.body);
    (url, admin)
}

#[tokio::test]
async fn joining_enrolls_this_device_and_makes_the_relay_home() {
    let (url, admin) = org_relay().await;
    let p = person("ok");
    let me = p.s.overview().unwrap().me.device_id;
    let tok = admin.issue("t-acme", &me, now() + 3600).unwrap().encode();

    let preview = p.s.preview_join(&tok, &url).unwrap();
    assert_eq!(preview.tenant, "t-acme");
    assert_eq!(preview.relay, url);
    assert!(preview.relay_host.starts_with("127.0.0.1:"));

    assert_eq!(p.s.join_relay(&tok, &url).await.unwrap(), "t-acme");
    let ov = p.s.overview().unwrap();
    assert_eq!(ov.relay, url, "the organisation's relay is now home");
    assert_eq!(ov.organisation.unwrap().tenant, "t-acme");
    // The app now signs in there without being asked again.
    p.s.test_relay().await.unwrap();

    // Leaving forgets the token; the relay still knows the device (until the admin revokes it).
    p.s.leave_relay().unwrap();
    assert!(p.s.overview().unwrap().organisation.is_none());
    p.s.test_relay().await.unwrap();
}

#[tokio::test]
async fn a_refused_or_wrong_token_changes_nothing() {
    let (url, admin) = org_relay().await;
    let p = person("bad");
    let home_before = p.s.overview().unwrap().relay;

    // Another device's token, an expired one, and something that is not a token at all.
    let other = admin
        .issue(
            "t-acme",
            &spike_core::identity::Identity::generate()
                .unwrap()
                .device_id(),
            now() + 60,
        )
        .unwrap()
        .encode();
    let e = p.s.preview_join(&other, &url).unwrap_err().to_string();
    assert!(e.contains("another device"), "{e}");
    let me = p.s.overview().unwrap().me.device_id;
    let old = admin.issue("t-acme", &me, now() - 5).unwrap().encode();
    let e = p.s.preview_join(&old, &url).unwrap_err().to_string();
    assert!(e.contains("expired"), "{e}");
    assert!(p.s.preview_join("hello", &url).is_err());

    // Signed by someone who is not the organisation's admin: the relay refuses it.
    let forged = SigningKey::generate()
        .issue("t-acme", &me, now() + 60)
        .unwrap()
        .encode();
    let e = format!("{:#}", p.s.join_relay(&forged, &url).await.unwrap_err());
    assert!(
        e.contains("did not admit this device") && e.contains("not valid here"),
        "{e}"
    );
    assert!(
        !e.contains("enroll_invalid"),
        "said in words, not as a code: {e}"
    );
    let ov = p.s.overview().unwrap();
    assert_eq!(ov.relay, home_before, "the home relay did not change");
    assert!(ov.organisation.is_none());
}

#[tokio::test]
async fn a_token_is_not_needed_on_an_open_relay() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    let p = person("open");
    let me = p.s.overview().unwrap().me.device_id;
    let tok = SigningKey::generate()
        .issue("t-acme", &me, now() + 60)
        .unwrap()
        .encode();
    let e = p.s.join_relay(&tok, &url).await.unwrap_err().to_string();
    assert!(e.contains("does not require enrollment"), "{e}");
    assert!(p.s.overview().unwrap().organisation.is_none());
}
