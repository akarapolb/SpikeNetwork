//! The group's name lives in the signed policy (slice 5): validated like the rest of it, and
//! changeable only by an admin.

use spike_mls::policy::{authorize, Change, Policy, PolicyError, Roster};

fn o(n: u8) -> [u8; 32] {
    [n; 32]
}
fn d(n: u8) -> [u8; 16] {
    [n; 16]
}
fn policy() -> Policy {
    Policy::new(vec![o(1)])
}
fn roster() -> Roster {
    let mut r = Roster::default();
    r.insert(o(1), d(1));
    r.insert(o(2), d(2));
    r
}

#[test]
fn the_group_name_is_part_of_the_policy_and_must_be_sane() {
    let mut p = policy();
    p.name = "Board of directors".into();
    assert_eq!(Policy::decode(&p.encode()).unwrap(), p);
    p.name = "x".repeat(101);
    assert!(Policy::decode(&p.encode()).is_err(), "too long");
    p.name = "line\nbreak".into();
    assert!(Policy::decode(&p.encode()).is_err(), "control characters");
    // A policy written before names existed still reads, with no name.
    let mut v: serde_json::Value = serde_json::to_value(policy()).unwrap();
    v.as_object_mut().unwrap().remove("name");
    assert_eq!(
        Policy::decode(&serde_json::to_vec(&v).unwrap())
            .unwrap()
            .name,
        ""
    );
}

#[test]
fn a_change_of_name_is_a_change_of_policy_so_admins_only() {
    let mut renamed = policy();
    renamed.name = "New".into();
    assert_eq!(
        authorize(
            &policy(),
            &o(2),
            &roster(),
            &[Change::SetPolicy(renamed.clone())]
        ),
        Err(PolicyError::NotAllowed("policy"))
    );
    assert_eq!(
        authorize(&policy(), &o(1), &roster(), &[Change::SetPolicy(renamed)]),
        Ok(())
    );
}
