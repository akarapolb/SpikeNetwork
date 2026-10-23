//! How fast does the relay take group appends? Not a pass/fail test: run it by hand.
//!
//!     cargo test -p spike-cli --release --test mailbox_bench -- --ignored --nocapture
//!
//! Everything goes through a real database file with the relay's usual `synchronous=FULL` (one
//! disk flush per append), over loopback, with a 177-byte payload (an MLS application message).

use spike_cli::client::Relay;
use spike_core::identity::Identity;
use spike_core::mailbox::{new_mailbox_id, MailboxKey};
use spike_core::wire::GroupKind;
use spike_relay::{serve, Config};
use std::time::Instant;
use tokio::net::TcpListener;

/// `senders` devices append `per_sender` messages each, to one mailbox or to one each.
async fn run(senders: usize, per_sender: usize, shared: bool) -> f64 {
    let dir = std::env::temp_dir().join(format!(
        "spike-bench-{}-{senders}-{shared}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(
        l,
        Config {
            db_path: Some(dir.join("relay.db")),
            // The benchmark is about storage, not the abuse limits.
            req_rate: 0.0,
            max_conns_per_ip: 0,
            conn_rate_per_ip: 0,
            ..Config::default()
        },
    ));
    let key = std::sync::Arc::new(MailboxKey::generate());
    let first = new_mailbox_id();
    let mut owner = Relay::connect(url.as_str(), &Identity::generate().unwrap())
        .await
        .unwrap();
    owner.group_create(&key, &first).await.unwrap();
    let mut tasks = vec![];
    let start = Instant::now();
    for i in 0..senders {
        let (url, key, first) = (url.clone(), key.clone(), first.clone());
        tasks.push(tokio::spawn(async move {
            let mut r = Relay::connect(url.as_str(), &Identity::generate().unwrap())
                .await
                .unwrap();
            let (mailbox, k) = if shared {
                (first, key)
            } else {
                let (id, k) = (
                    new_mailbox_id(),
                    std::sync::Arc::new(MailboxKey::generate()),
                );
                r.group_create(&k, &id).await.unwrap();
                (id, k)
            };
            let blob = vec![i as u8; 177];
            for _ in 0..per_sender {
                r.group_append(&k, &mailbox, GroupKind::Message, None, &blob, None)
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let rate = (senders * per_sender) as f64 / start.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&dir);
    rate
}

#[tokio::test]
#[ignore = "a measurement, not a test: run it by hand"]
async fn append_throughput() {
    for (senders, per, shared) in [
        (1, 300, true),
        (8, 100, true),
        (32, 40, true),
        (32, 40, false),
    ] {
        let rate = run(senders, per, shared).await;
        println!(
            "{senders:>2} senders x {per:>3} appends, {}: {rate:>7.0} appends/s",
            if shared {
                "one mailbox   "
            } else {
                "own mailboxes "
            }
        );
    }
}
