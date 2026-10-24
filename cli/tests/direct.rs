//! Direct device-to-device connections (ADR 0027): a message reaches the other device with no relay
//! running at all, and every way it can go wrong ends in the relay or in a refusal.

use spike_cli::direct;
use spike_cli::{
    ops,
    state::{Contact, Home},
    transport::Target,
    Outcome,
};
use spike_core::direct as handshake;
use spike_core::identity::Identity;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PASS: &str = "test passphrase for direct";
/// Nothing listens here: a relay that is down.
const DEAD_RELAY: &str = "ws://127.0.0.1:1";

async fn start_relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

struct Dev {
    home: Arc<Home>,
    got: Arc<Mutex<Vec<Outcome>>>,
    net: Arc<tokio::sync::Mutex<()>>,
    listener: Option<direct::Listener>,
}

impl Dev {
    fn new(tag: &str) -> Self {
        let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
        let dir = std::env::temp_dir().join(format!("spike-direct-{tag}-{n}"));
        Self {
            home: Arc::new(
                Home::create(dir, PASS, KdfParams::insecure_for_tests(), false).unwrap(),
            ),
            got: Arc::default(),
            net: Arc::default(),
            listener: None,
        }
    }

    fn card(&self) -> String {
        self.home.identity_card().unwrap()
    }

    fn owner(&self) -> String {
        spike_core::owner::IdentityCard::decode(&self.card())
            .unwrap()
            .owner_hex()
    }

    fn befriend(&self, name: &str, other: &Dev) {
        let mut c = self.home.load_contacts().unwrap();
        c.push(Contact {
            name: name.into(),
            card: other.card(),
            verified: true,
        });
        self.home.save_contacts(&c).unwrap();
    }

    async fn listen(&mut self, relay: &str) -> u16 {
        direct::set_enabled(&self.home, true).unwrap();
        let got = self.got.clone();
        let url = relay.to_string();
        let l = direct::listen(
            self.home.clone(),
            self.net.clone(),
            Arc::new(move || Target::from(url.as_str())),
            Arc::new(move |o| got.lock().unwrap().push(o)),
        )
        .await
        .unwrap();
        let port = l.port;
        self.listener = Some(l);
        port
    }

    async fn recv(&self, relay: &str) {
        ops::recv(&self.home, relay, Some(Duration::from_millis(300)), |_| {})
            .await
            .unwrap();
    }

    fn texts(&self) -> Vec<String> {
        self.got
            .lock()
            .unwrap()
            .iter()
            .filter_map(|o| match o {
                Outcome::Message { body, .. } => Some(String::from_utf8_lossy(body).into_owned()),
                _ => None,
            })
            .collect()
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.home.path()).ok();
    }
}

/// Two friends who already have a session (the first message went through a relay, as it must).
async fn pair() -> (Dev, Dev, String) {
    let relay = start_relay().await;
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    a.befriend("bob", &b);
    b.befriend("alice", &a);
    b.recv(&relay).await; // publishes prekeys
    ops::send(&a.home, relay.as_str(), "bob", b"first, by relay")
        .await
        .unwrap();
    b.recv(&relay).await;
    a.recv(&relay).await;
    assert_eq!(b.texts().len(), 0, "nothing went direct yet");
    let _ = (&mut a, &mut b);
    (a, b, relay)
}

#[tokio::test]
async fn a_message_arrives_with_no_relay_running() {
    let (mut a, mut b, relay) = pair().await;
    let port_a = a.listen(&relay).await;
    let port_b = b.listen(&relay).await;
    assert_ne!(port_a, 0);
    direct::set_typed_address(&a.home, &b.owner(), Some(&format!("127.0.0.1:{port_b}"))).unwrap();

    // The relay is gone. Alice writes to Bob.
    let report = ops::send(&a.home, DEAD_RELAY, "bob", b"hello, directly")
        .await
        .unwrap();
    assert_eq!(report.delivered(), 1);
    assert_eq!(b.texts(), vec!["hello, directly".to_string()]);

    // Bob never typed an address: he learned Alice's from her call, and answers the same way.
    let st = direct::load(&b.home).unwrap();
    assert_eq!(st.learned.len(), 1, "{st:?}");
    ops::send(&b.home, DEAD_RELAY, "alice", b"and back")
        .await
        .unwrap();
    assert_eq!(a.texts(), vec!["and back".to_string()]);

    // It is stored like any other message.
    let theirs = b
        .home
        .messages()
        .unwrap()
        .conversation(&a.owner(), 10)
        .unwrap();
    assert!(theirs.iter().any(|m| m.body == "hello, directly"));
    let _ = port_a;
}

#[tokio::test]
async fn an_unreachable_peer_falls_back_to_the_relay() {
    let (mut a, b, relay) = pair().await;
    a.listen(&relay).await;
    // Bob is not listening anywhere Alice knows.
    direct::set_typed_address(&a.home, &b.owner(), Some("127.0.0.1:1")).unwrap();
    ops::send(&a.home, relay.as_str(), "bob", b"by relay after all")
        .await
        .unwrap();
    let mut seen = vec![];
    ops::recv(
        &b.home,
        relay.as_str(),
        Some(Duration::from_millis(300)),
        |o| seen.push(o),
    )
    .await
    .unwrap();
    assert!(seen
        .iter()
        .any(|o| matches!(o, Outcome::Message { body, .. } if body == b"by relay after all")));
}

#[tokio::test]
async fn with_direct_off_nothing_is_dialled_and_a_dead_relay_still_fails() {
    let (a, b, relay) = pair().await;
    let _ = relay;
    direct::set_typed_address(&a.home, &b.owner(), Some("127.0.0.1:9")).unwrap();
    assert!(!direct::enabled(&a.home));
    let err = ops::send(&a.home, DEAD_RELAY, "bob", b"x")
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").to_lowercase().contains("connect"),
        "{err:#}"
    );
}

#[tokio::test]
async fn a_blob_the_device_cannot_read_is_refused_not_acknowledged() {
    let (mut a, mut b, relay) = pair().await;
    a.listen(&relay).await;
    let port_b = b.listen(&relay).await;
    direct::set_typed_address(&a.home, &b.owner(), Some(&format!("127.0.0.1:{port_b}"))).unwrap();
    let me = a.home.load_identity().unwrap();
    let bob = b.home.load_identity().unwrap().card();
    let err = direct::try_send(&a.home, &me, &bob, b"not a session message")
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("did not take it"), "{err:#}");
    assert!(b.got.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_stranger_on_the_network_gets_nothing() {
    let (_a, mut b, relay) = pair().await;
    let port_b = b.listen(&relay).await;
    let stranger = Identity::generate().unwrap();
    let bob_id = b.home.load_identity().unwrap().device_id();

    let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port_b))
        .await
        .unwrap();
    let (init, m1) = handshake::initiate();
    frame(&mut tcp, &m1).await;
    let m2 = read(&mut tcp).await;
    let (m3, mut ch) = init.finish(&stranger, 4000, &bob_id, &m2).unwrap();
    frame(&mut tcp, &m3).await;
    // Even a well-formed delivery is not answered: the connection is closed.
    let mut blob = vec![1u8];
    blob.extend_from_slice(b"hello");
    frame(&mut tcp, &ch.seal(&blob).unwrap()).await;
    let mut buf = [0u8; 8];
    let n = tokio::time::timeout(Duration::from_secs(3), tcp.read(&mut buf))
        .await
        .expect("closed promptly")
        .unwrap_or(0);
    assert_eq!(n, 0, "no reply to a stranger");
    assert!(b.got.lock().unwrap().is_empty());
    assert!(
        direct::load(&b.home).unwrap().learned.is_empty(),
        "a stranger's address is not remembered"
    );

    // And garbage in place of a handshake is simply dropped.
    let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port_b))
        .await
        .unwrap();
    frame(&mut tcp, &[0xff; 40]).await;
    let n = tokio::time::timeout(Duration::from_secs(3), tcp.read(&mut buf))
        .await
        .expect("closed promptly")
        .unwrap_or(0);
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_peer_with_the_wrong_key_at_the_address_is_not_trusted() {
    let (mut a, b, relay) = pair().await;
    a.listen(&relay).await;
    // Something else (a different device of a stranger) listens where Bob is said to be.
    let mut imposter = Dev::new("imposter");
    let port = imposter.listen(&relay).await;
    direct::set_typed_address(&a.home, &b.owner(), Some(&format!("127.0.0.1:{port}"))).unwrap();
    let me = a.home.load_identity().unwrap();
    let bob = b.home.load_identity().unwrap().card();
    assert!(direct::try_send(&a.home, &me, &bob, b"secret")
        .await
        .is_err());
    assert!(imposter.got.lock().unwrap().is_empty());
}

async fn frame(tcp: &mut tokio::net::TcpStream, data: &[u8]) {
    tcp.write_all(&(data.len() as u32).to_be_bytes())
        .await
        .unwrap();
    tcp.write_all(data).await.unwrap();
}

async fn read(tcp: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut len = [0u8; 4];
    tcp.read_exact(&mut len).await.unwrap();
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    tcp.read_exact(&mut buf).await.unwrap();
    buf
}
