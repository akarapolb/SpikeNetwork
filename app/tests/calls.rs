//! Call setup between two phones through the bridge and a real relay (ADR 0028): no media, the
//! "screens" here pass fake SDP and candidates. What it proves is the whole signalling path:
//! sealed envelopes, the state machines on both sides, and what each screen is told to do.

use serde_json::{json, Value};
use spike_app::bridge::Engine;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;

fn relay() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(format!("ws://{}", l.local_addr().unwrap())).unwrap();
            serve(l, Config::default()).await.ok();
        });
    });
    rx.recv().unwrap()
}

fn rand_id() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() ^ std::process::id()
}

struct Phone {
    e: Engine,
    dir: PathBuf,
}
impl Drop for Phone {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

impl Phone {
    fn new(tag: &str, relay: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("spike-calls-{tag}-{}", rand_id()));
        let e = Engine::with_kdf(&dir, KdfParams::insecure_for_tests()).unwrap();
        let p = Self { e, dir };
        p.ok("create_identity", json!({ "passphrase": "bridge test passphrase" }));
        p.ok("set_relay", json!({ "url": relay }));
        p
    }
    fn ok(&self, cmd: &str, args: Value) -> Value {
        let r: Value = serde_json::from_str(&self.e.call(cmd, &args.to_string())).unwrap();
        assert!(r.get("err").is_none(), "{cmd}: {r}");
        r["ok"].clone()
    }
    fn card(&self) -> String {
        self.ok("overview", json!({}))["me"]["card"].as_str().unwrap().to_string()
    }
    /// Fetch from the relay, then take what the call machine wants the screen to do.
    fn events(&self) -> Vec<Value> {
        self.raw_events().into_iter().filter(|e| e["type"] != "stop_ringing").collect()
    }
    fn raw_events(&self) -> Vec<Value> {
        self.ok("sync", json!({}));
        self.wait()
    }
    /// What the screen is asked to do next, without fetching.
    fn wait(&self) -> Vec<Value> {
        self.ok("call_wait", json!({ "ms": 200 })).as_array().unwrap().clone()
    }
    fn next(&self) -> Vec<Value> {
        self.wait().into_iter().filter(|e| e["type"] != "stop_ringing").collect()
    }
}

fn pair() -> (Phone, Phone) {
    let url = relay();
    let (alice, bob) = (Phone::new("alice", &url), Phone::new("bob", &url));
    alice.ok("sync", json!({}));
    bob.ok("sync", json!({}));
    alice.ok("add_contact", json!({ "name": "Bob", "card": bob.card() }));
    bob.ok("add_contact", json!({ "name": "Alice", "card": alice.card() }));
    (alice, bob)
}

fn kinds(ev: &[Value]) -> Vec<&str> {
    ev.iter().map(|e| e["type"].as_str().unwrap()).collect()
}

#[test]
fn a_call_rings_connects_exchanges_candidates_and_ends() {
    let (alice, bob) = pair();
    let call = alice.ok("call_start", json!({ "name": "Bob", "video": false, "sdp": "v=0 offer" }));
    let call = call.as_str().unwrap().to_string();

    let ev = bob.events();
    assert_eq!(kinds(&ev), ["ring"], "{ev:?}");
    assert_eq!(ev[0]["name"], "Alice");
    assert_eq!(ev[0]["sdp"], "v=0 offer");
    assert_eq!(ev[0]["call"], call.as_str());
    assert_eq!(ev[0]["video"], false);

    // Alice's candidates are held back until someone answers.
    alice.ok("call_ice", json!({ "candidates": ["candidate:1 host"] }));
    bob.ok("call_accept", json!({ "sdp": "v=0 answer" }));
    let ev = alice.events();
    assert_eq!(kinds(&ev), ["connect"], "{ev:?}");
    assert_eq!(ev[0]["sdp"], "v=0 answer");

    // After the answer, Alice's held candidate reaches Bob, and Bob's reach Alice.
    bob.ok("call_ice", json!({ "candidates": ["candidate:2 host"] }));
    let ev = bob.events();
    assert_eq!(kinds(&ev), ["remote_ice"], "{ev:?}");
    assert_eq!(ev[0]["candidates"][0], "candidate:1 host");
    let ev = alice.events();
    assert_eq!(kinds(&ev), ["remote_ice"], "{ev:?}");
    assert_eq!(ev[0]["candidates"][0], "candidate:2 host");

    alice.ok("call_hangup", json!({}));
    let ev = alice.next();
    assert_eq!(ev[0]["type"], "ended");
    assert_eq!(ev[0]["result"], "completed");
    let ev = bob.events();
    assert_eq!(kinds(&ev), ["ended"], "{ev:?}");
    assert_eq!(ev[0]["result"], "completed");
    assert_eq!(ev[0]["name"], "Alice");
    assert_eq!(ev[0]["incoming"], true);

    // Both conversations now have a line for the call.
    for (p, who) in [(&alice, "Bob"), (&bob, "Alice")] {
        let thread = p.ok("conversation", json!({ "name": who }));
        let last = thread.as_array().unwrap().last().unwrap();
        assert!(last["body"].as_str().unwrap().starts_with("Voice call, 0:"), "{last}");
    }
}

#[test]
fn a_declined_call_tells_the_caller() {
    let (alice, bob) = pair();
    alice.ok("call_start", json!({ "name": "Bob", "video": true, "sdp": "offer" }));
    let ev = bob.events();
    assert_eq!(ev[0]["video"], true);
    bob.ok("call_decline", json!({}));
    let ev = bob.next();
    assert_eq!(ev[0]["result"], "declined");
    let ev = alice.events();
    assert_eq!(kinds(&ev), ["ended"], "{ev:?}");
    assert_eq!(ev[0]["result"], "declined");
    assert_eq!(ev[0]["incoming"], false);
    let thread = alice.ok("conversation", json!({ "name": "Bob" }));
    assert_eq!(thread.as_array().unwrap().last().unwrap()["body"], "Declined video call");
    // and Alice can call again
    alice.ok("call_start", json!({ "name": "Bob", "video": false, "sdp": "offer2" }));
}

#[test]
fn a_caller_who_gives_up_stops_the_ringing_and_a_busy_phone_says_so() {
    let (alice, bob) = pair();
    alice.ok("call_start", json!({ "name": "Bob", "video": false, "sdp": "offer" }));
    assert_eq!(kinds(&bob.events()), ["ring"]);
    alice.ok("call_hangup", json!({}));
    let ev = bob.raw_events();
    assert_eq!(kinds(&ev), ["stop_ringing", "ended"], "{ev:?}");
    assert_eq!(ev[1]["result"], "missed");

    // Bob is busy with a call of his own: Alice is told.
    let (alice, bob) = pair();
    bob.ok("call_start", json!({ "name": "Alice", "video": false, "sdp": "bob offer" }));
    // Alice does not see Bob's invite yet; she calls Bob at the same moment (glare), so exactly one call survives.
    alice.ok("call_start", json!({ "name": "Bob", "video": false, "sdp": "alice offer" }));
    let (a, b) = (alice.events(), bob.events());
    let rings = a.iter().chain(b.iter()).filter(|e| e["type"] == "ring").count();
    assert_eq!(rings, 1, "alice {a:?} bob {b:?}");
}

#[test]
fn media_failure_ends_the_call_for_both() {
    let (alice, bob) = pair();
    alice.ok("call_start", json!({ "name": "Bob", "video": false, "sdp": "offer" }));
    bob.events();
    bob.ok("call_accept", json!({ "sdp": "answer" }));
    alice.events();
    alice.ok("call_fail", json!({}));
    let ev = bob.events();
    assert_eq!(kinds(&ev), ["ended"], "{ev:?}");
    assert_eq!(ev[0]["result"], "failed");
}
