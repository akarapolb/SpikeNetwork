//! MLS state lives on the vault (ADR 0025 section 8): sealed at rest, restored exactly.

mod support;

use spike_core::vault::{KdfParams, Vault};
use spike_mls::group::{Group, Processed};
use spike_mls::policy::Policy;
use spike_mls::state;
use support::{directory, Person};

fn vault() -> Vault {
    Vault::create("pw", KdfParams::insecure_for_tests())
        .unwrap()
        .0
}

#[test]
fn a_device_and_its_groups_survive_a_restart() {
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (da, db, dc) = (alice.mls_device(0), bob.mls_device(0), carol.mls_device(0));
    let mut ga = Group::create(&da, Policy::new(vec![alice.owner_pub()])).unwrap();
    let out = ga
        .add(&da, &dir, &[db.key_package(), dc.key_package()])
        .unwrap();
    ga.confirm(&da).unwrap();
    let mut gb = Group::join(
        &db,
        &dir,
        out.welcome.as_ref().unwrap(),
        out.join_bundle.as_ref().unwrap(),
    )
    .unwrap();
    // A rotation, so the mailbox state is not just what the epoch gives.
    let rm = ga.remove(&da, &[carol.device_bytes(0)]).unwrap();
    ga.confirm(&da).unwrap();
    gb.process(&db, &dir, &rm.commit).unwrap();
    let public = gb.mailbox_material().signing_public();

    let v = vault();
    let sealed = state::save(&v, "mls", &db, &[&gb]).unwrap();
    drop((db, gb));

    let (db, mut groups) = state::load(&v, "mls", &sealed).unwrap();
    let mut gb = groups.pop().unwrap();
    assert_eq!(gb.mailbox_generation(), 1);
    assert_eq!(gb.mailbox_material().signing_public(), public);
    assert_eq!(gb.members().len(), 2);
    assert_eq!(db.device_id(), bob.device_bytes(0));

    // Still a working member: reads, writes, and follows the next commit.
    let wire = ga.encrypt(&da, b"after restart").unwrap();
    assert!(
        matches!(gb.process(&db, &dir, &wire).unwrap(), Processed::Message(m) if m == b"after restart")
    );
    let wire = gb.encrypt(&db, b"and back").unwrap();
    assert!(matches!(
        ga.process(&da, &dir, &wire).unwrap(),
        Processed::Message(_)
    ));
    let su = ga.self_update(&da).unwrap();
    ga.confirm(&da).unwrap();
    assert!(matches!(
        gb.process(&db, &dir, &su.commit).unwrap(),
        Processed::Commit { .. }
    ));
}

#[test]
fn the_sealed_state_is_bound_to_the_vault_and_its_name_and_is_not_readable() {
    let bob = Person::new(1);
    let d = bob.mls_device(0);
    let v = vault();
    let sealed = state::save(&v, "mls", &d, &[]).unwrap();
    assert!(state::load(&v, "mls", &sealed).is_ok());
    assert!(
        state::load(&v, "other", &sealed).is_err(),
        "bound to its name"
    );
    assert!(
        state::load(&vault(), "mls", &sealed).is_err(),
        "bound to its vault"
    );
    let mut bad = sealed.clone();
    let n = bad.len() / 2;
    bad[n] ^= 1;
    assert!(state::load(&v, "mls", &bad).is_err());
    assert!(state::load(&v, "mls", &[]).is_err());
    let text = String::from_utf8_lossy(&sealed).to_string();
    assert!(!text.contains("signer") && !text.contains(&hex::encode(bob.device_bytes(0))));
}

#[test]
fn a_restored_device_can_still_be_welcomed_with_a_key_package_made_before() {
    let (alice, bob) = (Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob]);
    let da = alice.mls_device(0);
    let db = bob.mls_device(0);
    let kp = db.key_package();
    let v = vault();
    let sealed = state::save(&v, "mls", &db, &[]).unwrap();
    drop(db);
    let (db, _) = state::load(&v, "mls", &sealed).unwrap();

    let mut ga = Group::create(&da, Policy::new(vec![alice.owner_pub()])).unwrap();
    let out = ga.add(&da, &dir, &[kp]).unwrap();
    ga.confirm(&da).unwrap();
    assert!(Group::join(
        &db,
        &dir,
        out.welcome.as_ref().unwrap(),
        out.join_bundle.as_ref().unwrap()
    )
    .is_ok());
}
