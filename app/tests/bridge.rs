//! The JSON bridge the Android app uses, against a real relay: the same commands the desktop app
//! has, as strings in and out.

use serde_json::{json, Value};
use spike_app::bridge::Engine;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::sync::Arc;

/// A relay on its own thread and runtime (the engine runs its own, so tests are plain threads).
fn relay() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(format!("ws://{}", l.local_addr().unwrap()))
                .unwrap();
            serve(l, Config::default()).await.ok();
        });
    });
    rx.recv().unwrap()
}

fn rand_id() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
        ^ std::process::id()
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
        let dir = std::env::temp_dir().join(format!("spike-bridge-{tag}-{}", rand_id()));
        let e = Engine::with_kdf(&dir, KdfParams::insecure_for_tests()).unwrap();
        let p = Self { e, dir };
        p.ok(
            "create_identity",
            json!({ "passphrase": "bridge test passphrase" }),
        );
        p.ok("set_relay", json!({ "url": relay }));
        p
    }
    fn call(&self, cmd: &str, args: Value) -> Value {
        serde_json::from_str(&self.e.call(cmd, &args.to_string())).unwrap()
    }
    fn ok(&self, cmd: &str, args: Value) -> Value {
        let r = self.call(cmd, args);
        assert!(r.get("err").is_none(), "{cmd}: {r}");
        r["ok"].clone()
    }
    fn card(&self) -> String {
        self.ok("overview", json!({}))["me"]["card"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

#[test]
fn a_locked_or_confused_engine_answers_with_errors_never_panics() {
    let dir = std::env::temp_dir().join(format!("spike-bridge-locked-{}", rand_id()));
    let e = Engine::with_kdf(&dir, KdfParams::insecure_for_tests()).unwrap();
    let r: Value = serde_json::from_str(&e.call("app_status", "")).unwrap();
    assert_eq!(r["ok"], json!({ "exists": false, "unlocked": false }));
    for (cmd, args) in [
        ("overview", "{}"),
        ("send", "{\"name\":\"x\",\"text\":\"y\"}"),
        ("no_such_command", "{}"),
        ("unlock", "not json"),
        ("unlock", "{}"),
        ("create_identity", "{\"passphrase\":\"short\"}"),
    ] {
        let r: Value = serde_json::from_str(&e.call(cmd, args)).unwrap();
        assert!(r["err"].is_string(), "{cmd}: {r}");
    }
    assert!(!dir.exists(), "a refused create leaves nothing behind");
}

#[test]
fn two_phones_chat_through_the_bridge_and_a_locked_one_is_refused() {
    let url = relay();
    let (alice, bob) = (Phone::new("alice", &url), Phone::new("bob", &url));
    alice.ok("sync", json!({}));
    bob.ok("sync", json!({}));
    alice.ok("add_contact", json!({ "name": "Bob", "card": bob.card() }));
    bob.ok(
        "add_contact",
        json!({ "name": "Alice", "card": alice.card() }),
    );

    alice.ok(
        "send",
        json!({ "name": "Bob", "text": "hello from the bridge" }),
    );
    let got = bob.ok("sync", json!({}));
    assert_eq!(got["received"], 1);
    let thread = bob.ok("conversation", json!({ "name": "Alice" }));
    assert_eq!(thread[0]["body"], "hello from the bridge");
    assert_eq!(bob.ok("overview", json!({}))["contacts"][0]["unread"], 1);
    bob.ok("mark_read", json!({ "name": "Alice" }));
    assert_eq!(bob.ok("overview", json!({}))["contacts"][0]["unread"], 0);

    // Locking forgets the keys; the right passphrase brings it back, the wrong one does not.
    bob.ok("lock", json!({}));
    assert!(bob.call("overview", json!({}))["err"].is_string());
    assert!(bob.call("unlock", json!({ "passphrase": "wrong passphrase!!" }))["err"].is_string());
    bob.ok("unlock", json!({ "passphrase": "bridge test passphrase" }));
    assert_eq!(
        bob.ok("conversation", json!({ "name": "Alice" }))
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_large_group_works_through_the_bridge_and_the_relay_wakes_a_waiting_phone() {
    let url = relay();
    let (alice, bob) = (Phone::new("alice", &url), Phone::new("bob", &url));
    alice.ok("sync", json!({}));
    bob.ok("sync", json!({}));
    alice.ok("add_contact", json!({ "name": "Bob", "card": bob.card() }));
    bob.ok(
        "add_contact",
        json!({ "name": "Alice", "card": alice.card() }),
    );

    let made = alice.ok(
        "group_create_large",
        json!({ "name": "Org", "members": ["Bob"] }),
    );
    let gid = made["group"].as_str().unwrap().to_string();
    bob.ok("sync", json!({}));
    let row = bob.ok("overview", json!({}))["groups"][0].clone();
    assert_eq!(
        (row["status"].as_str(), row["large"].as_bool()),
        (Some("invited"), Some(true))
    );
    bob.ok("group_accept", json!({ "id": gid }));

    // Bob waits for the relay; Alice writes; Bob is woken, syncs, and reads it.
    let bob = Arc::new(bob);
    let waiting = {
        let b = bob.clone();
        std::thread::spawn(move || b.ok("wait_for_push", json!({ "ms": 15000 })))
    };
    std::thread::sleep(std::time::Duration::from_millis(1500));
    alice.ok("group_send", json!({ "id": gid, "text": "push me" }));
    assert_eq!(waiting.join().unwrap(), json!(true), "woken by the relay");
    bob.ok("sync", json!({}));
    let thread = bob.ok("group_conversation", json!({ "id": gid }));
    assert_eq!(thread[0]["body"], "push me");
    assert_eq!(bob.ok("group", json!({ "id": gid }))["large"], true);

    // Nothing to wake for: the wait just ends.
    assert_eq!(bob.ok("wait_for_push", json!({ "ms": 400 })), json!(false));
}

#[test]
fn the_phone_screens_get_what_they_draw_from_the_bridge() {
    let url = relay();
    let p = Phone::new("screens", &url);
    // A QR code as rows of 0 and 1, square, for drawing natively.
    let qr = p.ok("qr_matrix", json!({ "text": p.card() }));
    let size = qr["size"].as_u64().unwrap() as usize;
    let rows = qr["rows"].as_array().unwrap();
    assert!(size > 20 && rows.len() == size);
    assert!(rows.iter().all(|r| r.as_str().unwrap().len() == size
        && r.as_str().unwrap().chars().all(|c| c == '0' || c == '1')));
    assert!(p.call("qr_matrix", json!({ "text": "x".repeat(10_000) }))["err"].is_string());
    // The devices and agents screens.
    let me = p.ok("overview", json!({}))["me"].clone();
    assert_eq!(me["device_list"].as_array().unwrap().len(), 1);
    assert_eq!(me["device_list"][0]["this"], true);
    let agents = p.ok("agents", json!({}));
    assert!(agents["requests"].as_array().unwrap().is_empty());
    assert!(p
        .ok("message_requests", json!({}))
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn a_file_goes_by_path_and_is_saved_into_a_folder() {
    let url = relay();
    let alice = Phone::new("file-a", &url);
    let bob = Phone::new("file-b", &url);
    alice.ok("add_contact", json!({ "name": "Bob", "card": bob.card() }));
    bob.ok(
        "add_contact",
        json!({ "name": "Alice", "card": alice.card() }),
    );
    bob.ok("sync", json!({})); // publishes Bob's prekeys

    let src = alice.dir.join("hello.txt");
    std::fs::create_dir_all(&alice.dir).unwrap();
    std::fs::write(&src, b"a file over the bridge").unwrap();
    alice.ok(
        "send_file",
        json!({ "name": "Bob", "file_name": "hello.txt", "path": src.display().to_string(), "caption": "here" }),
    );
    bob.ok("sync", json!({}));
    let thread = bob.ok("conversation", json!({ "name": "Alice" }));
    assert_eq!(thread[0]["file"]["name"], "hello.txt");
    let id = thread[0]["id"].as_str().unwrap();
    let out = bob.dir.join("saved");
    let path = bob.ok(
        "save_attachment",
        json!({ "id": id, "dir": out.display().to_string() }),
    );
    assert_eq!(
        std::fs::read(path.as_str().unwrap()).unwrap(),
        b"a file over the bridge"
    );
    // A second save never overwrites the first.
    let again = bob.ok(
        "save_attachment",
        json!({ "id": id, "dir": out.display().to_string() }),
    );
    assert_ne!(again, path);
    // A path that is not there is an error, not a crash.
    assert!(alice.call(
        "send_file",
        json!({ "name": "Bob", "file_name": "x", "path": "/no/such/file" })
    )["err"]
        .is_string());
}

#[test]
fn a_second_phone_is_linked_through_the_bridge() {
    let url = relay();
    let old = Arc::new(Phone::new("link-old", &url));
    let new = Arc::new(Phone::new("link-new", &url));
    let friend = Phone::new("link-friend", &url);
    old.ok(
        "add_contact",
        json!({ "name": "Friend", "card": friend.card() }),
    );

    let code = new.ok("link_offer", json!({}));
    let waiter = {
        let n = new.clone();
        std::thread::spawn(move || n.ok("link_wait", json!({})))
    };
    std::thread::sleep(std::time::Duration::from_millis(800));
    let seen_old = old.ok("link_begin", json!({ "code": code }));
    let seen_new = waiter.join().unwrap();
    assert_eq!(seen_old, seen_new, "both screens show the same safety code");

    let receiver = {
        let n = new.clone();
        std::thread::spawn(move || n.ok("link_receive", json!({})))
    };
    let linked = old.ok("link_finish", json!({ "history": "all" }));
    assert_eq!(linked["contacts"], 1);
    receiver.join().unwrap();

    let devices = new.ok("overview", json!({}))["me"]["device_list"].clone();
    assert_eq!(devices.as_array().unwrap().len(), 2);
    assert!(new.ok("overview", json!({}))["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["name"] == "Friend"));
    // Giving up with nothing started is harmless.
    old.ok("link_cancel", json!({}));
}
