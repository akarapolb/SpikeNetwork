//! Q2 + Q4: size/time benchmark. `cargo run --release -- bench 20 100 500 1000`
//! or `-- hello` (tiny program for the binary-size measurement).
use openmls_spike::*;
use std::time::{Duration, Instant};
use tls_codec::Serialize as _;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn trust_all(_: &[u8; 32]) -> bool {
    true
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hello") => {
            // Minimal use of the crate set, for binary-size accounting.
            let o = Owner::random();
            let mut c = Client::new(&o, [0; 16]);
            c.create_group();
            let m = c.send(b"x");
            println!("{}", m.len());
        }
        Some("olm") => olm(),
        Some("bench") => {
            println!("size,state,kp_B,add_commit_B,welcome_B,tree_B,remove_commit_B,app_msg_B,create_ms,add_ms,remove_ms,member_process_add_ms,member_process_remove_ms,join_ms,keypkg_gen_ms,bulk_commit_B,bulk_welcome_B");
            for n in args[2..].iter().map(|s| s.parse::<usize>().unwrap()) {
                bench(n, args[1] == "populated");
            }
        }
        _ => eprintln!("usage: hello | olm | bench <fresh|populated> <sizes...>"),
    }
}

/// Group of `n` devices. `populated`: every member self-updates once (a mature group, the
/// ratchet tree has no blank nodes). Otherwise: creator adds everybody in a single commit.
fn bench(n: usize, populated: bool) {
    let owners: Vec<Owner> = (0..n).map(|_| Owner::random()).collect();
    let mut cl: Vec<Client> =
        owners.iter().enumerate().map(|(i, o)| Client::new(o, [i as u8; 16])).collect();

    let t = Instant::now();
    let kps: Vec<_> = cl[1..].iter().map(|c| c.key_package()).collect();
    let keypkg_gen_ms = ms(t.elapsed()) / (n - 1).max(1) as f64;
    let kp_b = kps[0].tls_serialize_detached().unwrap().len();

    let t = Instant::now();
    cl[0].create_group();
    let create_ms = ms(t.elapsed());

    // Bulk add (one commit, one Welcome). Time reported as part of setup.
    let (commit, welcome) = cl[0].add(&kps);
    let tree = cl[0].tree();
    let mut join_ms = 0.0;
    for (i, c) in cl.iter_mut().enumerate().skip(1) {
        let t = Instant::now();
        c.join(&welcome, tree.clone(), &trust_all).unwrap();
        if i == 1 {
            join_ms = ms(t.elapsed()); // includes credential verification of all n leaves
        }
    }
    let (bulk_commit_b, bulk_welcome_b) = (commit.len(), welcome.len());

    if populated {
        for i in 0..n {
            let upd = cl[i].self_update();
            for (j, c) in cl.iter_mut().enumerate() {
                if j != i {
                    c.process(&upd, &trust_all).unwrap();
                }
            }
        }
    }

    // --- add one member (new device) ---
    let newbie = Client::new(&Owner::random(), [255; 16]);
    let nkp = newbie.key_package();
    let t = Instant::now();
    let (add_commit, add_welcome) = cl[0].add(&[nkp]);
    let add_ms = ms(t.elapsed());
    let tree_b = cl[0].tree().tls_serialize_detached().unwrap().len();
    let t = Instant::now();
    cl[1].process(&add_commit, &trust_all).unwrap();
    let member_process_add_ms = ms(t.elapsed());
    for c in cl.iter_mut().skip(2) {
        // keep the others in sync is O(n); only needed for the remove test below (cl[0], cl[1])
        let _ = c;
    }

    // --- application message ---
    let app = cl[0].send(&[0u8; 32]);
    let t = Instant::now();
    match cl[1].process(&app, &trust_all).unwrap() {
        Processed::App(_) => {}
        _ => panic!(),
    }
    let _app_process = ms(t.elapsed());

    // --- remove one member (leaf 2) ---
    let victim = openmls::prelude::LeafNodeIndex::new(2);
    let t = Instant::now();
    let rm_commit = cl[0].remove(&[victim]);
    let remove_ms = ms(t.elapsed());
    let t = Instant::now();
    cl[1].process(&rm_commit, &trust_all).unwrap();
    let member_process_remove_ms = ms(t.elapsed());

    println!(
        "{n},{},{kp_b},{},{},{tree_b},{},{},{create_ms:.1},{add_ms:.1},{remove_ms:.1},{member_process_add_ms:.1},{member_process_remove_ms:.1},{join_ms:.1},{keypkg_gen_ms:.2},{bulk_commit_b},{bulk_welcome_b}",
        if populated { "populated" } else { "fresh" },
        add_commit.len(),
        add_welcome.len(),
        rm_commit.len(),
        app.len(),
    );
}

/// Baseline: size of one Olm (vodozemac) message, as the pairwise groups send per member device.
fn olm() {
    use vodozemac::olm::{Account, OlmMessage, SessionConfig};
    let alice = Account::new();
    let mut bob = Account::new();
    bob.generate_one_time_keys(1);
    let otk = *bob.one_time_keys().values().next().unwrap();
    let mut a = alice.create_outbound_session(SessionConfig::default(), bob.curve25519_key(), otk).unwrap();
    let body = [0u8; 32];
    let first = a.encrypt(&body).unwrap();
    let (k1, b1) = first.clone().to_parts();
    println!("olm first message (prekey) with 32-byte body: {} bytes (+1 kind byte in core/session.rs)", b1.len());
    let OlmMessage::PreKey(pk) = first else { panic!() };
    let r = bob.create_inbound_session(SessionConfig::default(), alice.curve25519_key(), &pk).unwrap();
    let mut b = r.session;
    let reply = b.encrypt(&body).unwrap();
    a.decrypt(&reply).unwrap();
    let (k2, n) = a.encrypt(&body).unwrap().to_parts();
    println!("olm normal message with 32-byte body: {} bytes (kinds {k1}/{k2})", n.len());
    let (_, n2) = a.encrypt(&[0u8; 1000]).unwrap().to_parts();
    println!("olm normal message with 1000-byte body: {} bytes", n2.len());
}
