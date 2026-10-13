//! wss:// end to end with real certificates, and the refusals that matter.

use spike_cli::{
    client::Relay,
    ops,
    state::{Contact, Home},
    transport::{Target, TransportOpts},
    Outcome,
};
use spike_core::{identity::Identity, vault::KdfParams};
use spike_relay::{serve, tls::TlsFiles, Config};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PASS: &str = "test passphrase for tls";

struct TestDir(PathBuf);
impl TestDir {
    fn new(tag: &str) -> Self {
        let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
        let p = std::env::temp_dir().join(format!("spike-tls-{tag}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Self-signed certificate for the given names; returns (cert.pem path, key.pem path).
fn make_cert(dir: &Path, name: &str, sans: &[&str]) -> (PathBuf, PathBuf) {
    let ck =
        rcgen::generate_simple_self_signed(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap();
    let (c, k) = (
        dir.join(format!("{name}.crt")),
        dir.join(format!("{name}.key")),
    );
    std::fs::write(&c, ck.cert.pem()).unwrap();
    std::fs::write(&k, ck.key_pair.serialize_pem()).unwrap();
    (c, k)
}

async fn start_tls_relay(cert: &Path, key: &Path) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let cfg = Config {
        tls: Some(TlsFiles {
            cert: cert.into(),
            key: key.into(),
        }),
        ..Config::default()
    };
    tokio::spawn(serve(l, cfg));
    port
}

fn target(url: String, ca: Option<&Path>) -> Target {
    Target {
        url,
        opts: TransportOpts {
            ca_file: ca.map(Path::to_path_buf),
            allow_insecure: false,
        },
    }
}

fn device(dir: &Path, name: &str) -> Home {
    Home::create(dir.join(name), PASS, KdfParams::insecure_for_tests(), false).unwrap()
}

#[tokio::test]
async fn full_conversation_over_wss_with_trusted_ca() {
    let d = TestDir::new("ok");
    let (cert, key) = make_cert(d.path(), "relay", &["localhost", "127.0.0.1"]);
    let port = start_tls_relay(&cert, &key).await;
    let url = format!("wss://127.0.0.1:{port}");

    let (alice, bob) = (device(d.path(), "alice"), device(d.path(), "bob"));
    let add = |h: &Home, name: &str, other: &Home| {
        h.save_contacts(&[Contact {
            name: name.into(),
            card: other.load_identity().unwrap().card().encode(),
            verified: true,
        }])
        .unwrap();
    };
    add(&alice, "bob", &bob);
    add(&bob, "alice", &alice);

    ops::publish_keys(&bob, target(url.clone(), Some(&cert)))
        .await
        .unwrap();
    ops::send(&alice, target(url.clone(), Some(&cert)), "bob", b"over tls")
        .await
        .unwrap();
    let mut got = vec![];
    ops::recv(
        &bob,
        target(url, Some(&cert)),
        Some(Duration::from_millis(300)),
        |o| got.push(o),
    )
    .await
    .unwrap();
    assert!(
        matches!(got.as_slice(), [Outcome::Message { body, .. }] if body == b"over tls"),
        "{got:?}"
    );
}

#[tokio::test]
async fn untrusted_wrong_or_mismatched_certificates_are_refused() {
    let d = TestDir::new("bad");
    let (cert, key) = make_cert(d.path(), "relay", &["localhost", "127.0.0.1"]);
    let port = start_tls_relay(&cert, &key).await;
    let id = Identity::generate().unwrap();
    let url = format!("wss://127.0.0.1:{port}");

    // No CA configured: a self-signed relay is not trusted.
    let e = Relay::connect(target(url.clone(), None), &id)
        .await
        .err()
        .unwrap();
    assert!(format!("{e:#}").contains("TLS handshake"), "{e:#}");

    // A different CA: still refused.
    let (other, _) = make_cert(d.path(), "other", &["localhost", "127.0.0.1"]);
    assert!(Relay::connect(target(url, Some(&other)), &id)
        .await
        .is_err());

    // Right CA but the certificate does not name the host we connect to.
    let (name_only, name_key) = make_cert(d.path(), "named", &["relay.example"]);
    let port2 = start_tls_relay(&name_only, &name_key).await;
    let e = Relay::connect(
        target(format!("wss://127.0.0.1:{port2}"), Some(&name_only)),
        &id,
    )
    .await
    .err()
    .unwrap();
    assert!(
        format!("{e:#}").contains("TLS handshake"),
        "hostname mismatch accepted: {e:#}"
    );
}

#[tokio::test]
async fn plain_ws_to_a_tls_relay_fails_and_does_not_hang() {
    let d = TestDir::new("plain");
    let (cert, key) = make_cert(d.path(), "relay", &["127.0.0.1"]);
    let port = start_tls_relay(&cert, &key).await;
    let id = Identity::generate().unwrap();
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        Relay::connect(format!("ws://127.0.0.1:{port}"), &id),
    )
    .await;
    assert!(
        matches!(r, Ok(Err(_))),
        "plain client must fail promptly against a TLS relay"
    );
}

#[tokio::test]
async fn plain_ws_off_loopback_is_refused_before_any_connection() {
    let id = Identity::generate().unwrap();
    let e = Relay::connect("ws://relay.example.com:8787", &id)
        .await
        .err()
        .unwrap();
    assert!(e.to_string().contains("refusing plain ws://"), "{e}");
    let e = Relay::connect("http://127.0.0.1:1", &id)
        .await
        .err()
        .unwrap();
    assert!(e.to_string().contains("must start with wss://"), "{e}");
}
