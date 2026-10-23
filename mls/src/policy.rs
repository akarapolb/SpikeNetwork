//! The group's policy and the commit-authority rules (ADR 0025 sections 3 and 7).
//!
//! MLS lets any member send any commit. Who may add, remove or change the policy is therefore ours
//! to enforce: every member runs [`authorize`] on a staged commit before merging it, and a commit
//! that fails is dropped on every device alike.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Hard ceilings, whatever a group's own policy says (ADR 0025, decision on size).
pub const MAX_PEOPLE: u32 = 500;
pub const MAX_DEVICES: u32 = 1000;
pub const POLICY_VERSION: u8 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Who {
    Admins,
    Members,
}

/// Stored in a GroupContext extension, so every member and every joiner holds the same copy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub v: u8,
    /// Owner keys (hex) of the people who administer the group.
    pub admins: Vec<String>,
    pub add: Who,
    pub remove: Who,
    pub max_people: u32,
    pub max_devices: u32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("malformed policy")]
    Malformed,
    #[error("not allowed to {0}")]
    NotAllowed(&'static str),
    #[error("a device is in the group twice")]
    DuplicateDevice,
    #[error("too many {0}")]
    TooMany(&'static str),
    #[error("a leaf may not change who it is")]
    IdentityChanged,
    #[error("not a member")]
    NotAMember,
    #[error("{0} commits are not allowed")]
    Forbidden(&'static str),
    #[error("the new policy is not valid")]
    InvalidPolicy,
}

impl Policy {
    pub fn new(admins: Vec<[u8; 32]>) -> Self {
        Self {
            v: POLICY_VERSION,
            admins: admins.iter().map(hex::encode).collect(),
            add: Who::Admins,
            remove: Who::Admins,
            max_people: MAX_PEOPLE,
            max_devices: MAX_DEVICES,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serializes")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PolicyError> {
        let p: Policy = serde_json::from_slice(bytes).map_err(|_| PolicyError::Malformed)?;
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<(), PolicyError> {
        let admins_ok = !self.admins.is_empty()
            && self.admins.len() <= 64
            && self
                .admins
                .iter()
                .all(|a| hex::decode(a).map(|b| b.len() == 32).unwrap_or(false));
        if self.v != POLICY_VERSION
            || !admins_ok
            || self.max_people == 0
            || self.max_people > MAX_PEOPLE
            || self.max_devices == 0
            || self.max_devices > MAX_DEVICES
        {
            return Err(PolicyError::Malformed);
        }
        Ok(())
    }

    pub fn is_admin(&self, owner: &[u8; 32]) -> bool {
        let h = hex::encode(owner);
        self.admins.contains(&h)
    }
}

/// The people and devices in a group right now.
#[derive(Default, Clone, Debug)]
pub struct Roster(BTreeMap<[u8; 32], BTreeSet<[u8; 16]>>);

impl Roster {
    pub fn insert(&mut self, owner: [u8; 32], device: [u8; 16]) {
        self.0.entry(owner).or_default().insert(device);
    }
    fn remove(&mut self, owner: &[u8; 32], device: &[u8; 16]) -> bool {
        let Some(set) = self.0.get_mut(owner) else {
            return false;
        };
        let had = set.remove(device);
        if set.is_empty() {
            self.0.remove(owner);
        }
        had
    }
    pub fn has_device(&self, device: &[u8; 16]) -> bool {
        self.0.values().any(|s| s.contains(device))
    }
    pub fn people(&self) -> usize {
        self.0.len()
    }
    pub fn devices(&self) -> usize {
        self.0.values().map(BTreeSet::len).sum()
    }
}

/// One thing a commit does to the group.
#[derive(Clone, Debug)]
pub enum Change {
    Add {
        owner: [u8; 32],
        device: [u8; 16],
    },
    Remove {
        owner: [u8; 32],
        device: [u8; 16],
    },
    Update {
        old: ([u8; 32], [u8; 16]),
        new: ([u8; 32], [u8; 16]),
    },
    SetPolicy(Policy),
    /// A proposal type this protocol does not use (external, reinit, psk).
    Forbidden(&'static str),
}

/// May `committer` make these changes to a group with this policy and roster?
pub fn authorize(
    policy: &Policy,
    committer: &[u8; 32],
    roster: &Roster,
    changes: &[Change],
) -> Result<(), PolicyError> {
    let admin = policy.is_admin(committer);
    let mut after = roster.clone();

    for c in changes {
        if let Change::Forbidden(what) = c {
            return Err(PolicyError::Forbidden(what));
        }
    }
    for c in changes {
        match c {
            Change::Update { old, new } if old != new => return Err(PolicyError::IdentityChanged),
            Change::SetPolicy(p) => {
                if !admin {
                    return Err(PolicyError::NotAllowed("policy"));
                }
                p.validate().map_err(|_| PolicyError::InvalidPolicy)?;
            }
            _ => {}
        }
    }
    // MLS applies removals before additions within one commit.
    for c in changes {
        if let Change::Remove { owner, device } = c {
            if owner != committer && !admin && policy.remove != Who::Members {
                return Err(PolicyError::NotAllowed("remove"));
            }
            if !after.remove(owner, device) {
                return Err(PolicyError::NotAMember);
            }
        }
    }
    for c in changes {
        if let Change::Add { owner, device } = c {
            if owner != committer && !admin && policy.add != Who::Members {
                return Err(PolicyError::NotAllowed("add"));
            }
            if after.has_device(device) {
                return Err(PolicyError::DuplicateDevice);
            }
            after.insert(*owner, *device);
        }
    }
    let new_policy = changes.iter().find_map(|c| match c {
        Change::SetPolicy(p) => Some(p),
        _ => None,
    });
    let p = new_policy.unwrap_or(policy);
    if after.people() > p.max_people.min(MAX_PEOPLE) as usize {
        return Err(PolicyError::TooMany("people"));
    }
    if after.devices() > p.max_devices.min(MAX_DEVICES) as usize {
        return Err(PolicyError::TooMany("devices"));
    }
    Ok(())
}
