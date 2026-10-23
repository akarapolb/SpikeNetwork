//! Hostile input (ADR 0025 section 9): everything that arrives from the relay or a Welcome is
//! attacker-controlled. This is the deterministic stand-in for the fuzz targets in `mls/fuzz`
//! (cargo-fuzz needs nightly): mutate real messages byte by byte and check that nothing panics, that
//! anything accepted was a real message, and that a refusal leaves the group exactly as it was.

mod support;

use spike_mls::credential::MapDirectory;
use spike_mls::group::{Device, Error, Group, Outgoing, Processed};
use spike_mls::policy::Policy;
use support::{directory, Person};

struct World {
    dir: MapDirectory,
    a: (Device, Group),
    b: (Device, Group),
    commit: Outgoing,
    message: Vec<u8>,
    welcome: Outgoing,
    joiner: Device,
}

fn world() -> World {
    let (alice, bob, carol, dave) = (
        Person::new(1),
        Person::new(1),
        Person::new(1),
        Person::new(1),
    );
    let dir = directory(&[&alice, &bob, &carol, &dave]);
    let (da, db, dc, dd) = (
        alice.mls_device(0),
        bob.mls_device(0),
        carol.mls_device(0),
        dave.mls_device(0),
    );
    let mut ga = Group::create(&da, Policy::new(vec![alice.owner_pub()])).unwrap();
    let first = ga
        .add(&da, &dir, &[db.key_package(), dc.key_package()])
        .unwrap();
    ga.confirm(&da).unwrap();
    let gb = Group::join(
        &db,
        &dir,
        first.welcome.as_ref().unwrap(),
        first.join_bundle.as_ref().unwrap(),
    )
    .unwrap();
    let message = ga.encrypt(&da, b"a real message").unwrap();
    let welcome = ga.add(&da, &dir, &[dd.key_package()]).unwrap();
    // Keep this one pending: it is the real next commit.
    World {
        dir,
        a: (da, ga),
        b: (db, gb),
        commit: Outgoing {
            commit: welcome.commit.clone(),
            welcome: None,
            join_bundle: None,
            successor: None,
        },
        message,
        welcome,
        joiner: dd,
    }
}

/// Every single-byte change, plus truncations and an extension.
fn mutations(good: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for i in 0..good.len() {
        let mut m = good.to_vec();
        m[i] ^= 0x01;
        out.push(m);
        if i % 7 == 0 {
            let mut m = good.to_vec();
            m[i] = !m[i];
            out.push(m);
        }
    }
    for n in [0, 1, 2, good.len() / 3, good.len() / 2, good.len() - 1] {
        out.push(good[..n].to_vec());
    }
    let mut longer = good.to_vec();
    longer.extend_from_slice(&[0u8; 17]);
    out.push(longer);
    out
}

#[test]
fn a_mutated_application_message_never_panics_and_is_never_taken_for_the_real_one() {
    let mut w = world();
    for bad in mutations(&w.message) {
        if let Ok(Processed::Message(m)) = w.b.1.process(&w.b.0, &w.dir, &bad) {
            panic!("accepted a mutation: {m:?}");
        }
    }
    // After all of that, the real one is still readable.
    assert!(matches!(
        w.b.1.process(&w.b.0, &w.dir, &w.message).unwrap(),
        Processed::Message(m) if m == b"a real message"
    ));
}

#[test]
fn a_mutated_commit_never_panics_and_a_refusal_changes_nothing() {
    let mut w = world();
    let (epoch, members) = (w.b.1.epoch(), w.b.1.members().len());
    for bad in mutations(&w.commit.commit) {
        if w.b.1.process(&w.b.0, &w.dir, &bad).is_err() {
            assert_eq!(w.b.1.epoch(), epoch);
            assert_eq!(w.b.1.members().len(), members);
        } else {
            panic!("a mutated commit was accepted");
        }
    }
    // The real commit still applies afterwards: refusals did not burn its secrets.
    let r = w.b.1.process(&w.b.0, &w.dir, &w.commit.commit).unwrap();
    assert!(matches!(r, Processed::Commit { .. }), "{r:?}");
    assert_eq!(w.b.1.members().len(), members + 1);
    w.a.1.confirm(&w.a.0).unwrap();
    assert_eq!(w.a.1.epoch(), w.b.1.epoch());
}

#[test]
fn a_mutated_welcome_or_bundle_never_panics_and_never_joins() {
    let w = world();
    let welcome = w.welcome.welcome.as_ref().unwrap();
    let bundle = w.welcome.join_bundle.as_ref().unwrap();
    // Welcomes are large; sample them rather than trying every byte.
    let step = (welcome.len() / 400).max(1);
    for (i, bad) in mutations(welcome).into_iter().enumerate() {
        if i % step != 0 {
            continue;
        }
        assert!(
            Group::join(&w.joiner, &w.dir, &bad, bundle).is_err(),
            "mutation {i}"
        );
    }
    for bad in mutations(bundle) {
        assert!(Group::join(&w.joiner, &w.dir, welcome, &bad).is_err());
    }
    // Both refusals left the key package usable.
    assert!(Group::join(&w.joiner, &w.dir, welcome, bundle).is_ok());
}

#[test]
fn mutated_key_packages_are_refused_by_the_committer() {
    let mut w = world();
    let kp = w.joiner.key_package();
    let step = (kp.len() / 300).max(1);
    // Drop the commit left pending by `world()`, so a refusal below is about the key package.
    w.a.1.discard(&w.a.0).unwrap();
    for (i, bad) in mutations(&kp).into_iter().enumerate() {
        if i % step != 0 {
            continue;
        }
        match w.a.1.add(&w.a.0, &w.dir, &[bad]) {
            Err(Error::PendingCommit) => panic!("refused for the wrong reason"),
            Err(_) => {}
            Ok(_) => panic!("mutation {i} was accepted"),
        }
    }
    assert!(!w.a.1.has_pending());
}
