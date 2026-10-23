//! Who may change a large group, and by how much (ADR 0025 sections 3 and 7). These rules are
//! pure: they look at the group's policy, the committer, the current roster and the changes a
//! commit makes, and say yes or no.

use spike_mls::policy::{
    authorize, Change, Policy, PolicyError, Roster, Who, MAX_DEVICES, MAX_PEOPLE,
};

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
    r.insert(o(1), d(1)); // admin
    r.insert(o(2), d(2)); // member
    r.insert(o(2), d(3)); // member's second device
    r
}

#[test]
fn policy_round_trips_and_rejects_anything_unexpected() {
    let p = policy();
    assert_eq!(Policy::decode(&p.encode()).unwrap(), p);
    assert!(Policy::decode(b"").is_err());
    assert!(Policy::decode(b"{}").is_err());
    let mut v: serde_json::Value = serde_json::from_slice(&p.encode()).unwrap();
    v["surprise"] = true.into();
    assert!(Policy::decode(&serde_json::to_vec(&v).unwrap()).is_err());
    let mut v: serde_json::Value = serde_json::from_slice(&p.encode()).unwrap();
    v["v"] = 2.into();
    assert!(Policy::decode(&serde_json::to_vec(&v).unwrap()).is_err());
    let mut v: serde_json::Value = serde_json::from_slice(&p.encode()).unwrap();
    v["admins"] = serde_json::json!([]);
    assert!(
        Policy::decode(&serde_json::to_vec(&v).unwrap()).is_err(),
        "a group needs an admin"
    );
    let mut v: serde_json::Value = serde_json::from_slice(&p.encode()).unwrap();
    v["max_people"] = (MAX_PEOPLE + 1).into();
    assert!(
        Policy::decode(&serde_json::to_vec(&v).unwrap()).is_err(),
        "above the hard cap"
    );
}

#[test]
fn an_admin_may_add_and_remove_anyone() {
    let r = roster();
    let p = policy();
    let add = [Change::Add {
        owner: o(3),
        device: d(4),
    }];
    assert_eq!(authorize(&p, &o(1), &r, &add), Ok(()));
    let rm = [Change::Remove {
        owner: o(2),
        device: d(2),
    }];
    assert_eq!(authorize(&p, &o(1), &r, &rm), Ok(()));
}

#[test]
fn a_plain_member_cannot_add_or_remove_other_people_under_the_default_policy() {
    let r = roster();
    let p = policy();
    assert_eq!(
        authorize(
            &p,
            &o(2),
            &r,
            &[Change::Add {
                owner: o(3),
                device: d(4)
            }]
        ),
        Err(PolicyError::NotAllowed("add"))
    );
    assert_eq!(
        authorize(
            &p,
            &o(2),
            &r,
            &[Change::Remove {
                owner: o(1),
                device: d(1)
            }]
        ),
        Err(PolicyError::NotAllowed("remove"))
    );
}

#[test]
fn a_member_manages_their_own_devices() {
    let r = roster();
    let p = policy();
    assert_eq!(
        authorize(
            &p,
            &o(2),
            &r,
            &[Change::Add {
                owner: o(2),
                device: d(9)
            }]
        ),
        Ok(()),
        "linking their own new device"
    );
    assert_eq!(
        authorize(
            &p,
            &o(2),
            &r,
            &[Change::Remove {
                owner: o(2),
                device: d(3)
            }]
        ),
        Ok(()),
        "removing their own other device"
    );
    // Not as a cover for adding someone else in the same commit.
    assert!(authorize(
        &p,
        &o(2),
        &r,
        &[
            Change::Add {
                owner: o(2),
                device: d(9)
            },
            Change::Add {
                owner: o(3),
                device: d(4)
            }
        ]
    )
    .is_err());
}

#[test]
fn the_policy_can_let_members_add() {
    let mut p = policy();
    p.add = Who::Members;
    assert_eq!(
        authorize(
            &p,
            &o(2),
            &roster(),
            &[Change::Add {
                owner: o(3),
                device: d(4)
            }]
        ),
        Ok(())
    );
    assert!(authorize(
        &p,
        &o(2),
        &roster(),
        &[Change::Remove {
            owner: o(1),
            device: d(1)
        }]
    )
    .is_err());
}

#[test]
fn one_leaf_per_device() {
    assert_eq!(
        authorize(
            &policy(),
            &o(1),
            &roster(),
            &[Change::Add {
                owner: o(2),
                device: d(2)
            }]
        ),
        Err(PolicyError::DuplicateDevice)
    );
    // Two adds of the same device in one commit.
    assert_eq!(
        authorize(
            &policy(),
            &o(1),
            &roster(),
            &[
                Change::Add {
                    owner: o(3),
                    device: d(4)
                },
                Change::Add {
                    owner: o(3),
                    device: d(4)
                }
            ]
        ),
        Err(PolicyError::DuplicateDevice)
    );
    // A device id cannot move to another person.
    assert_eq!(
        authorize(
            &policy(),
            &o(1),
            &roster(),
            &[Change::Add {
                owner: o(3),
                device: d(2)
            }]
        ),
        Err(PolicyError::DuplicateDevice)
    );
}

#[test]
fn the_size_caps_hold() {
    let mut p = policy();
    p.max_people = 3;
    p.max_devices = 4;
    let r = roster(); // 2 people, 3 devices
    assert_eq!(
        authorize(
            &p,
            &o(1),
            &r,
            &[Change::Add {
                owner: o(3),
                device: d(4)
            }]
        ),
        Ok(())
    );
    assert_eq!(
        authorize(
            &p,
            &o(1),
            &r,
            &[
                Change::Add {
                    owner: o(3),
                    device: d(4)
                },
                Change::Add {
                    owner: o(4),
                    device: d(5)
                }
            ]
        ),
        Err(PolicyError::TooMany("people"))
    );
    assert_eq!(
        authorize(
            &p,
            &o(1),
            &r,
            &[
                Change::Add {
                    owner: o(2),
                    device: d(5)
                },
                Change::Add {
                    owner: o(2),
                    device: d(6)
                }
            ]
        ),
        Err(PolicyError::TooMany("devices"))
    );
    // Removing in the same commit makes room.
    assert_eq!(
        authorize(
            &p,
            &o(1),
            &r,
            &[
                Change::Remove {
                    owner: o(2),
                    device: d(3)
                },
                Change::Add {
                    owner: o(2),
                    device: d(5)
                },
                Change::Add {
                    owner: o(2),
                    device: d(6)
                }
            ]
        ),
        Ok(())
    );
    assert_eq!((MAX_PEOPLE, MAX_DEVICES), (500, 1000));
}

#[test]
fn an_update_may_not_change_who_a_leaf_is() {
    let r = roster();
    let same = Change::Update {
        old: (o(2), d(2)),
        new: (o(2), d(2)),
    };
    assert_eq!(authorize(&policy(), &o(2), &r, &[same]), Ok(()));
    let other_device = Change::Update {
        old: (o(2), d(2)),
        new: (o(2), d(3)),
    };
    assert_eq!(
        authorize(&policy(), &o(2), &r, &[other_device]),
        Err(PolicyError::IdentityChanged)
    );
    let other_person = Change::Update {
        old: (o(2), d(2)),
        new: (o(1), d(2)),
    };
    assert_eq!(
        authorize(&policy(), &o(2), &r, &[other_person]),
        Err(PolicyError::IdentityChanged)
    );
}

#[test]
fn only_an_admin_changes_the_policy_and_it_must_stay_valid() {
    let r = roster();
    let mut next = policy();
    next.add = Who::Members;
    assert_eq!(
        authorize(&policy(), &o(1), &r, &[Change::SetPolicy(next.clone())]),
        Ok(())
    );
    assert_eq!(
        authorize(&policy(), &o(2), &r, &[Change::SetPolicy(next.clone())]),
        Err(PolicyError::NotAllowed("policy"))
    );
    // An admin cannot be made out of thin air by a member, nor the group left without one.
    let mut none = policy();
    none.admins = vec![];
    assert!(authorize(&policy(), &o(1), &r, &[Change::SetPolicy(none)]).is_err());
}

#[test]
fn proposals_that_change_the_protocol_are_refused() {
    for what in ["external", "reinit", "psk"] {
        assert_eq!(
            authorize(&policy(), &o(1), &roster(), &[Change::Forbidden(what)]),
            Err(PolicyError::Forbidden(what))
        );
    }
}
