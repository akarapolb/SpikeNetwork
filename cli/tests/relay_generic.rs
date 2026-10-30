//! The relay client over a socket and a clock that are not `tokio-tungstenite` and `tokio` (ADR 0033, step 3):
//! a scripted in-memory relay and a clock the test moves by hand. Keepalive and idle windows run on that clock
//! alone, so none of these tests waits in real time.

use anyhow::Result;
use spike_cli::client::Relay;
use spike_cli::socket::{Connector, Socket};
use spike_cli::transport::Target;
use spike_core::clock::Clock;
use spike_core::identity::Identity;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

/// A clock that only moves when the test says so.
#[derive(Clone, Default)]
struct ManualClock {
    now: Arc<AtomicU64>,
    changed: Arc<Notify>,
}

impl ManualClock {
    fn advance(&self, ms: u64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        1_700_000_000_000 + self.now.load(Ordering::SeqCst)
    }

    fn mono_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    async fn sleep(&self, ms: u64) {
        let target = self.mono_ms() + ms;
        loop {
            let woken = self.changed.notified();
            tokio::pin!(woken);
            // register interest before looking, so an advance in between is not missed
            woken.as_mut().enable();
            if self.mono_ms() >= target {
                return;
            }
            woken.await;
        }
    }
}

/// One end of an in-memory connection; the test holds the other end and plays the relay.
struct MemSocket {
    to_server: mpsc::UnboundedSender<String>,
    from_server: mpsc::UnboundedReceiver<String>,
}

impl Socket for MemSocket {
    async fn send(&mut self, frame: String) -> Result<()> {
        self.to_server.send(frame)?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<String>> {
        Ok(self.from_server.recv().await)
    }
}

struct Server {
    from_client: mpsc::UnboundedReceiver<String>,
    to_client: mpsc::UnboundedSender<String>,
}

impl Server {
    async fn next(&mut self) -> serde_json::Value {
        let t = tokio::time::timeout(Duration::from_secs(5), self.from_client.recv())
            .await
            .expect("the client sent something")
            .expect("the client is still connected");
        serde_json::from_str(&t).unwrap()
    }

    fn say(&self, json: serde_json::Value) {
        self.to_client.send(json.to_string()).unwrap();
    }
}

/// Hands out the one prepared connection.
struct MemConnector(Mutex<Option<MemSocket>>);

impl Connector for MemConnector {
    type Socket = MemSocket;

    async fn connect(&self, target: &Target) -> Result<MemSocket> {
        assert_eq!(target.url, "ws://mem.test");
        Ok(self.0.lock().unwrap().take().expect("connected once"))
    }
}

fn pair() -> (MemConnector, Server) {
    let (to_server, from_client) = mpsc::unbounded_channel();
    let (to_client, from_server) = mpsc::unbounded_channel();
    (
        MemConnector(Mutex::new(Some(MemSocket {
            to_server,
            from_server,
        }))),
        Server {
            from_client,
            to_client,
        },
    )
}

/// Connect a relay client to a scripted server that accepts any sign-in.
async fn connected(clock: ManualClock) -> (Relay<MemSocket, ManualClock>, Server) {
    let (connector, mut server) = pair();
    let me = Identity::generate().unwrap();
    let script = tokio::spawn(async move {
        server.say(serde_json::json!({"type": "challenge", "nonce": "00ff00ff00ff00ff"}));
        assert_eq!(server.next().await["type"], "auth");
        server.say(serde_json::json!({"type": "ready", "device_id": "dev-1", "prekeys": 7}));
        server
    });
    let relay = Relay::connect_with(&connector, clock, "ws://mem.test", &me)
        .await
        .unwrap();
    (relay, script.await.unwrap())
}

#[tokio::test]
async fn a_relay_client_runs_over_a_socket_that_is_not_a_websocket() {
    let (mut relay, mut server) = connected(ManualClock::default()).await;
    assert_eq!(relay.device_id, "dev-1");
    assert_eq!(relay.prekeys, 7);

    // a delivery pushed by the relay comes out, and the ack goes back
    server.say(serde_json::json!({"type": "deliver", "id": "m1", "blob": "abcd"}));
    let d = relay
        .next_delivery(Some(Duration::from_secs(1)))
        .await
        .unwrap()
        .expect("a delivery");
    assert_eq!(
        (d.id.as_str(), d.blob.as_slice()),
        ("m1", &[0xab, 0xcd][..])
    );
    relay.ack("m1").await.unwrap();
    let ack = server.next().await;
    assert_eq!(
        (ack["type"].as_str(), ack["id"].as_str()),
        (Some("ack"), Some("m1"))
    );
}

#[tokio::test]
async fn an_idle_window_ends_on_the_clock_without_real_waiting() {
    let clock = ManualClock::default();
    let (mut relay, _server) = connected(clock.clone()).await;
    let waiting =
        tokio::spawn(async move { relay.next_delivery(Some(Duration::from_secs(30))).await });
    // let the client start waiting, then jump past its window in one step
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished(), "still inside the window");
    clock.advance(30_000);
    let got = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("returns once the window has passed")
        .unwrap()
        .unwrap();
    assert!(got.is_none(), "nothing arrived in the window");
}

#[tokio::test]
async fn a_silent_relay_is_pinged_and_then_given_up_on_by_the_clock() {
    let clock = ManualClock::default();
    let (mut relay, mut server) = connected(clock.clone()).await;
    relay.set_keepalive(Duration::from_secs(10), Duration::from_secs(5));
    let waiting = tokio::spawn(async move { relay.next_delivery(None).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    clock.advance(10_000); // the keepalive interval passes: a ping goes out
    assert_eq!(server.next().await["type"], "ping");
    assert!(!waiting.is_finished());

    clock.advance(5_000); // nobody answered within the wait: the connection counts as lost
    let err = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("gives up")
        .unwrap()
        .expect_err("a lost connection is an error");
    assert!(err.to_string().contains("lost the connection"), "{err}");
}

#[tokio::test]
async fn any_message_from_the_relay_counts_as_an_answer_to_a_ping() {
    let clock = ManualClock::default();
    let (mut relay, mut server) = connected(clock.clone()).await;
    relay.set_keepalive(Duration::from_secs(10), Duration::from_secs(5));
    let waiting = tokio::spawn(async move { relay.next_delivery(None).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    clock.advance(10_000);
    assert_eq!(server.next().await["type"], "ping");
    server.say(serde_json::json!({"type": "pong"}));
    tokio::time::sleep(Duration::from_millis(50)).await;
    clock.advance(5_000); // past the old pong deadline, but the pong arrived
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished(), "the pong kept the connection alive");

    server.say(serde_json::json!({"type": "deliver", "id": "m2", "blob": "00"}));
    let d = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .expect("a delivery");
    assert_eq!(d.id, "m2");
}

#[tokio::test]
async fn timeout_helper_uses_the_given_clock() {
    let clock = ManualClock::default();
    let c2 = clock.clone();
    let task = tokio::spawn(async move {
        spike_cli::socket::timeout(&c2, 1_000, std::future::pending::<u8>()).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    clock.advance(1_000);
    assert_eq!(task.await.unwrap(), None);
    // and a future that finishes first wins
    assert_eq!(
        spike_cli::socket::timeout(&clock, 1_000, async { 7u8 }).await,
        Some(7)
    );
}
