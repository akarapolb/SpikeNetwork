//! Small groups over pairwise sessions (ADR 0024).
//!
//! A group is a random id plus a chain of signed [`GroupState`]s. Each state
//! names the members (by owner key), the admins and the settings, points to
//! the previous state by hash, and is signed by the owner key of the member
//! who made the change. Messages themselves still travel over the one-to-one
//! sessions; this module only decides who is in the group.
//!
//! Every member's app keeps the whole chain in a [`Chain`] and applies the
//! same rules, so apps that have seen the same states agree on the current
//! one: the valid state with the highest version, ties broken by the lowest
//! hash. Concurrent changes by two admins therefore converge; the losing
//! change is simply not applied.

use crate::owner::OwnerKey;
use crate::{Error, Result};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use vodozemac::{Ed25519PublicKey, Ed25519Signature};

const STATE_CTX: &[u8] = b"spike-group-state-v1";
const HASH_CTX: &[u8] = b"spike-group-hash-v1";
/// Small groups only: every message is encrypted once per member device.
pub const MAX_MEMBERS: usize = 20;
/// Longest group name, in characters.
pub const MAX_NAME: usize = 100;
/// Longest relay hint, in bytes.
pub const MAX_RELAY: usize = 512;
/// States kept per group. Changes are rare; this only bounds a hostile admin.
pub const MAX_CHAIN: usize = 5_000;

/// One member: their owner key and where their device list can be fetched.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AddBy {
    Admins,
    Any,
}

/// What a new member gets to see.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum History {
    /// Messages from when they join.
    Join,
    /// The last 7 days, sent by the device that adds them.
    Week,
}

/// The editable part of a state: what a change may touch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    /// In joining order: new members are appended, so the first is the longest-standing.
    pub members: Vec<Member>,
    pub admins: Vec<String>,
    pub add_by: AddBy,
    pub history: History,
}

/// The part that is signed. Field order is the signing order; do not reorder.
#[derive(Serialize)]
struct Signed<'a> {
    group: &'a str,
    version: u64,
    prev: &'a Option<String>,
    name: &'a str,
    members: &'a [Member],
    admins: &'a [String],
    add_by: AddBy,
    history: History,
    author: &'a str,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GroupState {
    /// 128-bit random id, hex.
    pub group: String,
    /// 1 for a new group, previous + 1 for every change.
    pub version: u64,
    /// Hash of the previous state ([`GroupState::hash`]); none for version 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    pub name: String,
    pub members: Vec<Member>,
    pub admins: Vec<String>,
    pub add_by: AddBy,
    pub history: History,
    /// Owner key (hex) of the member who made this change.
    pub author: String,
    /// The author's owner signature over the fields above, hex.
    pub sig: String,
}

impl GroupState {
    /// A new group with `author` as its first member and only admin.
    pub fn create(
        author: &OwnerKey,
        author_relay: Option<String>,
        name: &str,
        others: Vec<Member>,
        add_by: AddBy,
        history: History,
    ) -> Result<Self> {
        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let me = hex::encode(author.public());
        let mut members = vec![Member {
            owner: me.clone(),
            relay: author_relay,
        }];
        members.extend(others);
        let draft = Draft {
            name: name.trim().to_string(),
            members,
            admins: vec![me],
            add_by,
            history,
        };
        let state = Self::sign(author, hex::encode(id), 1, None, draft)?;
        state.verify_genesis()?;
        Ok(state)
    }

    /// The next state, made by `author` from this one. `change` edits a copy
    /// of the current contents; the result is checked against the same rules
    /// receivers apply, so an app can never sign a change others would refuse.
    pub fn successor(&self, author: &OwnerKey, change: impl FnOnce(&mut Draft)) -> Result<Self> {
        let mut draft = self.draft();
        change(&mut draft);
        let next = Self::sign(
            author,
            self.group.clone(),
            self.version + 1,
            Some(self.hash()),
            draft,
        )?;
        next.verify_transition(self)?;
        Ok(next)
    }

    /// Leave the group. The last admin hands admin rights to the longest-standing member.
    pub fn leave(&self, me: &OwnerKey) -> Result<Self> {
        let mine = hex::encode(me.public());
        self.successor(me, |d| {
            d.members.retain(|m| m.owner != mine);
            d.admins.retain(|a| *a != mine);
            if d.admins.is_empty() {
                if let Some(first) = d.members.first() {
                    d.admins.push(first.owner.clone());
                }
            }
        })
    }

    pub fn draft(&self) -> Draft {
        Draft {
            name: self.name.clone(),
            members: self.members.clone(),
            admins: self.admins.clone(),
            add_by: self.add_by,
            history: self.history,
        }
    }

    pub fn is_member(&self, owner: &str) -> bool {
        self.members.iter().any(|m| m.owner == owner)
    }

    pub fn is_admin(&self, owner: &str) -> bool {
        self.admins.iter().any(|a| a == owner)
    }

    /// Names this exact state: what messages carry as `gstate` and what the next state's `prev` holds.
    pub fn hash(&self) -> String {
        let body = serde_json::to_vec(self).expect("serializes");
        hex::encode(
            Sha256::new()
                .chain_update(HASH_CTX)
                .chain_update(body)
                .finalize(),
        )
    }

    fn sign(
        author: &OwnerKey,
        group: String,
        version: u64,
        prev: Option<String>,
        d: Draft,
    ) -> Result<Self> {
        let mut state = Self {
            group,
            version,
            prev,
            name: d.name,
            members: d.members,
            admins: d.admins,
            add_by: d.add_by,
            history: d.history,
            author: hex::encode(author.public()),
            sig: String::new(),
        };
        state.check_shape()?;
        state.sig = hex::encode(author.sign_raw(&state.signing_bytes()));
        Ok(state)
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let body = serde_json::to_vec(&Signed {
            group: &self.group,
            version: self.version,
            prev: &self.prev,
            name: &self.name,
            members: &self.members,
            admins: &self.admins,
            add_by: self.add_by,
            history: self.history,
            author: &self.author,
        })
        .expect("serializes");
        [STATE_CTX, &body].concat()
    }

    fn check_shape(&self) -> Result<()> {
        let bad = |why| Err(Error::BadGroup(why));
        let id_ok = self.group.len() == 32 && hex::decode(&self.group).is_ok();
        if !id_ok {
            return bad("group id must be 128 bits of hex");
        }
        if self.version == 0 || (self.version == 1) != self.prev.is_none() {
            return bad("version and previous state do not match");
        }
        let name_len = self.name.chars().count();
        if name_len == 0 || name_len > MAX_NAME || self.name.trim() != self.name {
            return bad("name must be 1 to 100 characters without outer spaces");
        }
        if self.members.is_empty() || self.members.len() > MAX_MEMBERS {
            return bad("needs between 1 and 20 members");
        }
        for m in &self.members {
            if !is_key_hex(&m.owner) {
                return bad("member is not an owner key");
            }
            if m.relay
                .as_ref()
                .is_some_and(|r| r.is_empty() || r.len() > MAX_RELAY)
            {
                return bad("relay hint is empty or too long");
            }
        }
        let mut owners: Vec<&str> = self.members.iter().map(|m| m.owner.as_str()).collect();
        owners.sort_unstable();
        owners.dedup();
        if owners.len() != self.members.len() {
            return bad("duplicate member");
        }
        if self.admins.is_empty() {
            return bad("a group needs at least one admin");
        }
        let mut admins = self.admins.clone();
        admins.sort_unstable();
        admins.dedup();
        if admins.len() != self.admins.len() || !self.admins.iter().all(|a| self.is_member(a)) {
            return bad("admins must be distinct members");
        }
        if !is_key_hex(&self.author) {
            return bad("author is not an owner key");
        }
        Ok(())
    }

    /// Signature and shape only. Whether the change was allowed needs the previous state.
    pub fn verify_signature(&self) -> Result<()> {
        self.check_shape()?;
        let owner: [u8; 32] = hex::decode(&self.author)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let sig: [u8; 64] = hex::decode(&self.sig)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let pk = Ed25519PublicKey::from_slice(&owner).map_err(|_| Error::BadSignature)?;
        let s = Ed25519Signature::from_slice(&sig).map_err(|_| Error::BadSignature)?;
        pk.verify(&self.signing_bytes(), &s)
            .map_err(|_| Error::BadSignature)
    }

    /// A valid version 1: signed by its first member, who is its only admin.
    pub fn verify_genesis(&self) -> Result<()> {
        self.verify_signature()?;
        if self.version != 1 {
            return Err(Error::BadGroup("not the first state of a group"));
        }
        if self.members[0].owner != self.author || self.admins != [self.author.clone()] {
            return Err(Error::BadGroup("a new group is made by its only admin"));
        }
        Ok(())
    }

    /// A valid change from `prev`, made by someone allowed to make it.
    pub fn verify_transition(&self, prev: &GroupState) -> Result<()> {
        self.verify_signature()?;
        if self.group != prev.group
            || self.version != prev.version + 1
            || self.prev.as_deref() != Some(prev.hash().as_str())
        {
            return Err(Error::BadGroup("does not follow the previous state"));
        }
        if !prev.is_member(&self.author) {
            return Err(Error::BadGroup("only members can change a group"));
        }
        if prev.is_admin(&self.author) {
            return Ok(());
        }
        // Members who are not admins may leave, or add people if the group allows it.
        let old = prev.draft();
        let new = self.draft();
        let mut left = old.clone();
        left.members.retain(|m| m.owner != self.author);
        if new == left {
            return Ok(());
        }
        let appended = new.members.len() > old.members.len()
            && new.members[..old.members.len()] == old.members[..];
        let mut added = old.clone();
        added.members = new.members.clone();
        if prev.add_by == AddBy::Any && appended && new == added {
            return Ok(());
        }
        Err(Error::BadGroup("only admins can make this change"))
    }
}

fn is_key_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What happened when a state was offered to a [`Chain`].
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    /// Already held; nothing changed.
    Known,
    /// Valid and stored. `current_changed` says whether it is now the current state.
    New { current_changed: bool },
    /// Its previous state is not held yet: ask the sender for states after `have`.
    MissingPrev { have: u64 },
}

/// How a group message relates to the receiver's view of the group (ADR 0024, section 4).
#[derive(Debug, PartialEq, Eq)]
pub enum MessageCheck {
    /// Sent with our current state.
    Current,
    /// Sent with an earlier state on our chain; fine (sent before the latest change reached them).
    Earlier,
    /// Sent with a state that lost to ours: show it with a note and send our state back.
    OutOfDate,
    /// We do not hold that state: ask for it and hold the message.
    Unknown,
    /// The sender is not a member (of our current state, or of the state they named): drop it.
    NotMember,
}

/// Every valid state of one group that this device holds.
#[derive(Clone, Debug)]
pub struct Chain {
    states: HashMap<String, GroupState>,
    current: String,
}

impl Chain {
    /// Start from a group's first state.
    pub fn new(genesis: GroupState) -> Result<Self> {
        genesis.verify_genesis()?;
        let h = genesis.hash();
        Ok(Self {
            states: HashMap::from([(h.clone(), genesis)]),
            current: h,
        })
    }

    /// Rebuild from stored states (any order), checking every one again.
    pub fn from_states(mut states: Vec<GroupState>) -> Result<Self> {
        states.sort_by_key(|s| s.version);
        let mut it = states.into_iter();
        let mut chain = Self::new(it.next().ok_or(Error::BadGroup("no states"))?)?;
        for s in it {
            if let Added::MissingPrev { .. } = chain.add(s)? {
                return Err(Error::BadGroup("stored chain has a gap"));
            }
        }
        Ok(chain)
    }

    pub fn group(&self) -> &str {
        &self.current().group
    }

    pub fn current(&self) -> &GroupState {
        &self.states[&self.current]
    }

    pub fn get(&self, hash: &str) -> Option<&GroupState> {
        self.states.get(hash)
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Offer a state received from anyone. Invalid states are refused with an error.
    pub fn add(&mut self, state: GroupState) -> Result<Added> {
        if state.group != self.group() {
            return Err(Error::BadGroup("belongs to another group"));
        }
        let h = state.hash();
        if self.states.contains_key(&h) {
            return Ok(Added::Known);
        }
        let Some(prev) = state.prev.as_ref().and_then(|p| self.states.get(p)) else {
            if state.version == 1 {
                return Err(Error::BadGroup("a second first state for this group"));
            }
            state.verify_signature()?;
            return Ok(Added::MissingPrev {
                have: self.highest_contiguous(),
            });
        };
        state.verify_transition(prev)?;
        if self.states.len() >= MAX_CHAIN {
            return Err(Error::BadGroup("too many changes to this group"));
        }
        let wins = beats(&state, &h, self.current(), &self.current);
        self.states.insert(h.clone(), state);
        if wins {
            self.current = h;
        }
        Ok(Added::New {
            current_changed: wins,
        })
    }

    /// States to send to someone who asked for those after `version`, oldest first.
    pub fn after(&self, version: u64) -> Vec<GroupState> {
        let mut v: Vec<GroupState> = self
            .states
            .values()
            .filter(|s| s.version > version)
            .cloned()
            .collect();
        v.sort_by_key(|a| (a.version, a.hash()));
        v
    }

    /// Every stored state, for saving.
    pub fn states(&self) -> Vec<GroupState> {
        self.after(0)
    }

    /// Is `hash` the current state or one of its ancestors?
    pub fn on_current_chain(&self, hash: &str) -> bool {
        let mut at = Some(self.current.as_str());
        while let Some(h) = at {
            if h == hash {
                return true;
            }
            at = self.states.get(h).and_then(|s| s.prev.as_deref());
        }
        false
    }

    /// Decide what to do with a group message from `sender` (owner key) that names state `gstate`.
    pub fn check_message(&self, gstate: &str, sender: &str) -> MessageCheck {
        if !self.current().is_member(sender) {
            return MessageCheck::NotMember;
        }
        let Some(named) = self.states.get(gstate) else {
            return MessageCheck::Unknown;
        };
        if !named.is_member(sender) {
            return MessageCheck::NotMember;
        }
        if gstate == self.current {
            MessageCheck::Current
        } else if self.on_current_chain(gstate) {
            MessageCheck::Earlier
        } else {
            MessageCheck::OutOfDate
        }
    }

    /// The highest version up to which every version has at least one state held.
    fn highest_contiguous(&self) -> u64 {
        let mut v = 0;
        while self.states.values().any(|s| s.version == v + 1) {
            v += 1;
        }
        v
    }
}

/// The winner rule: higher version, then lower hash.
fn beats(a: &GroupState, a_hash: &str, b: &GroupState, b_hash: &str) -> bool {
    (a.version, std::cmp::Reverse(a_hash)) > (b.version, std::cmp::Reverse(b_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_hex(k: &OwnerKey) -> String {
        hex::encode(k.public())
    }

    fn member(k: &OwnerKey) -> Member {
        Member {
            owner: key_hex(k),
            relay: Some("wss://relay.example".into()),
        }
    }

    struct People {
        alice: OwnerKey,
        bob: OwnerKey,
        carol: OwnerKey,
        dana: OwnerKey,
    }

    fn people() -> People {
        People {
            alice: OwnerKey::generate(),
            bob: OwnerKey::generate(),
            carol: OwnerKey::generate(),
            dana: OwnerKey::generate(),
        }
    }

    /// Alice creates "Pilot" with Bob and Carol.
    fn pilot(p: &People, add_by: AddBy) -> GroupState {
        GroupState::create(
            &p.alice,
            None,
            "Pilot",
            vec![member(&p.bob), member(&p.carol)],
            add_by,
            History::Join,
        )
        .unwrap()
    }

    #[test]
    fn a_new_group_is_signed_by_its_only_admin() {
        let p = people();
        let g = pilot(&p, AddBy::Admins);
        assert_eq!(g.version, 1);
        assert_eq!(g.admins, vec![key_hex(&p.alice)]);
        assert_eq!(g.members.len(), 3);
        g.verify_genesis().unwrap();
        // Survives a JSON round trip with the same hash.
        let back: GroupState = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back.hash(), g.hash());
        back.verify_genesis().unwrap();
    }

    #[test]
    fn tampering_breaks_the_signature() {
        let p = people();
        let g = pilot(&p, AddBy::Admins);
        let mut renamed = g.clone();
        renamed.name = "Other".into();
        let mut added = g.clone();
        added.members.push(member(&p.dana));
        let mut promoted = g.clone();
        promoted.admins.push(key_hex(&p.bob));
        let mut reauthored = g.clone();
        reauthored.author = key_hex(&p.bob);
        for bad in [renamed, added, promoted, reauthored] {
            assert!(bad.verify_signature().is_err());
        }
    }

    #[test]
    fn shape_rules() {
        let p = people();
        let make = |name: &str, others: Vec<Member>| {
            GroupState::create(&p.alice, None, name, others, AddBy::Admins, History::Join)
        };
        assert!(make("", vec![]).is_err(), "empty name");
        assert!(make(&"x".repeat(MAX_NAME + 1), vec![]).is_err());
        assert!(
            make(&"ก".repeat(MAX_NAME), vec![]).is_ok(),
            "counts characters, not bytes"
        );
        assert!(
            make("Pilot", vec![member(&p.alice)]).is_err(),
            "duplicate member"
        );
        let crowd: Vec<Member> = (0..MAX_MEMBERS)
            .map(|_| member(&OwnerKey::generate()))
            .collect();
        assert!(make("Big", crowd).is_err(), "21 people");
        let bad_key = Member {
            owner: "AB".repeat(32),
            relay: None,
        };
        assert!(
            make("Pilot", vec![bad_key]).is_err(),
            "keys are lowercase hex"
        );
        let g = pilot(&p, AddBy::Admins);
        assert!(
            g.successor(&p.alice, |d| d.admins.clear()).is_err(),
            "a group always has an admin"
        );
        assert!(
            g.successor(&p.alice, |d| d.admins.push(key_hex(&p.dana)))
                .is_err(),
            "admins must be members"
        );
    }

    #[test]
    fn admins_can_change_anything() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let v2 = v1
            .successor(&p.alice, |d| {
                d.members.push(member(&p.dana));
                d.admins.push(key_hex(&p.carol));
                d.name = "Pilot team".into();
            })
            .unwrap();
        v2.verify_transition(&v1).unwrap();
        // Carol, now an admin, removes Alice (admins are equal).
        let v3 = v2
            .successor(&p.carol, |d| {
                d.members.retain(|m| m.owner != key_hex(&p.alice));
                d.admins.retain(|a| *a != key_hex(&p.alice));
                d.history = History::Week;
            })
            .unwrap();
        v3.verify_transition(&v2).unwrap();
        assert!(!v3.is_member(&key_hex(&p.alice)));
    }

    #[test]
    fn members_may_only_leave_or_add_when_allowed() {
        let p = people();
        let closed = pilot(&p, AddBy::Admins);
        let add_dana = |d: &mut Draft| d.members.push(member(&p.dana));
        assert!(closed.successor(&p.bob, add_dana).is_err(), "admins only");
        assert!(closed
            .successor(&p.bob, |d| d.name = "Mine".into())
            .is_err());
        assert!(closed
            .successor(&p.bob, |d| d.admins.push(key_hex(&p.bob)))
            .is_err());
        assert!(
            closed
                .successor(&p.bob, |d| d
                    .members
                    .retain(|m| m.owner != key_hex(&p.carol)))
                .is_err(),
            "members cannot remove others"
        );
        assert!(
            closed.successor(&p.dana, |d| d.name = "x".into()).is_err(),
            "not a member"
        );
        let left = closed.leave(&p.bob).unwrap();
        assert!(!left.is_member(&key_hex(&p.bob)));

        let open = pilot(&p, AddBy::Any);
        open.successor(&p.bob, add_dana).unwrap();
        assert!(
            open.successor(&p.bob, |d| {
                d.members.push(member(&p.dana));
                d.name = "Sneaky".into();
            })
            .is_err(),
            "adding does not allow other changes"
        );
        assert!(
            open.successor(&p.bob, |d| d.members.insert(0, member(&p.dana)))
                .is_err(),
            "new members are appended"
        );
    }

    #[test]
    fn the_last_admin_hands_over_when_leaving() {
        let p = people();
        let g = pilot(&p, AddBy::Admins);
        let left = g.leave(&p.alice).unwrap();
        assert_eq!(
            left.admins,
            vec![key_hex(&p.bob)],
            "longest-standing member"
        );
        left.verify_transition(&g).unwrap();
        let solo =
            GroupState::create(&p.alice, None, "Me", vec![], AddBy::Admins, History::Join).unwrap();
        assert!(
            solo.leave(&p.alice).is_err(),
            "the last member just deletes the group"
        );
    }

    #[test]
    fn transitions_must_follow_their_previous_state() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let v2 = v1.successor(&p.alice, |d| d.name = "Two".into()).unwrap();
        let other = pilot(&p, AddBy::Admins);
        assert!(v2.verify_transition(&other).is_err(), "other group");
        let v3 = v2.successor(&p.alice, |d| d.name = "Three".into()).unwrap();
        assert!(v3.verify_transition(&v1).is_err(), "skips a version");
    }

    #[test]
    fn chain_follows_changes_and_reloads() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let v2 = v1
            .successor(&p.alice, |d| d.members.push(member(&p.dana)))
            .unwrap();
        let v3 = v2.leave(&p.bob).unwrap();
        let mut c = Chain::new(v1.clone()).unwrap();
        assert_eq!(
            c.add(v2.clone()).unwrap(),
            Added::New {
                current_changed: true
            }
        );
        assert_eq!(c.add(v2.clone()).unwrap(), Added::Known);
        assert_eq!(
            c.add(v3.clone()).unwrap(),
            Added::New {
                current_changed: true
            }
        );
        assert_eq!(c.current(), &v3);
        let reloaded = Chain::from_states(vec![v3.clone(), v1, v2]).unwrap();
        assert_eq!(reloaded.current(), &v3);
        assert_eq!(reloaded.len(), 3);
    }

    #[test]
    fn concurrent_admin_changes_converge_in_any_order() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let v2 = v1
            .successor(&p.alice, |d| d.admins.push(key_hex(&p.carol)))
            .unwrap();
        // Alice and Carol both change version 2 at the same time.
        let a = v2
            .successor(&p.alice, |d| d.name = "Alice's name".into())
            .unwrap();
        let c = v2
            .successor(&p.carol, |d| d.members.push(member(&p.dana)))
            .unwrap();
        let winner = if a.hash() < c.hash() { &a } else { &c };
        for order in [[&a, &c], [&c, &a]] {
            let mut chain = Chain::new(v1.clone()).unwrap();
            chain.add(v2.clone()).unwrap();
            for s in order {
                chain.add(s.clone()).unwrap();
            }
            assert_eq!(chain.current(), winner, "same winner whatever the order");
        }
        // A change built on the losing side is still valid, and a higher version wins.
        let loser = if winner == &a { &c } else { &a };
        let mut chain = Chain::new(v1).unwrap();
        for s in [&v2, winner, loser] {
            chain.add(s.clone()).unwrap();
        }
        let next = loser
            .successor(&p.alice, |d| d.history = History::Week)
            .unwrap();
        assert_eq!(
            chain.add(next.clone()).unwrap(),
            Added::New {
                current_changed: true
            }
        );
        assert!(chain.on_current_chain(&loser.hash()));
        assert!(!chain.on_current_chain(&winner.hash()));
    }

    #[test]
    fn a_missing_state_is_asked_for_not_guessed() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let v2 = v1.successor(&p.alice, |d| d.name = "Two".into()).unwrap();
        let v3 = v2.successor(&p.alice, |d| d.name = "Three".into()).unwrap();
        let mut c = Chain::new(v1.clone()).unwrap();
        assert_eq!(c.add(v3.clone()).unwrap(), Added::MissingPrev { have: 1 });
        assert_eq!(c.current(), &v1, "nothing applied");
        assert!(
            Chain::from_states(vec![v1.clone(), v3.clone()]).is_err(),
            "gap"
        );
        c.add(v2).unwrap();
        c.add(v3.clone()).unwrap();
        assert_eq!(c.current(), &v3);
        // A forged state is refused even when its previous state is missing.
        let mut forged = v3.successor(&p.alice, |d| d.name = "Four".into()).unwrap();
        forged.version = 9;
        assert!(Chain::new(v1).unwrap().add(forged).is_err());
    }

    #[test]
    fn invalid_states_never_enter_the_chain() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let mut c = Chain::new(v1.clone()).unwrap();
        // Bob (not an admin) signs a change that only an admin could make.
        let mut d = v1.draft();
        d.admins.push(key_hex(&p.bob));
        let grab = GroupState::sign(&p.bob, v1.group.clone(), 2, Some(v1.hash()), d).unwrap();
        assert!(c.add(grab).is_err());
        // Another group's state, and a second genesis.
        assert!(c.add(pilot(&p, AddBy::Admins)).is_err());
        let mut second = pilot(&p, AddBy::Admins);
        second.group = v1.group.clone();
        assert!(c.add(second).is_err());
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn message_checks() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let v2 = v1
            .successor(&p.alice, |d| d.admins.push(key_hex(&p.carol)))
            .unwrap();
        let a = v2
            .successor(&p.alice, |d| d.members.push(member(&p.dana)))
            .unwrap();
        let c = v2
            .successor(&p.carol, |d| d.name = "Carol's".into())
            .unwrap();
        let (win, lose) = if a.hash() < c.hash() {
            (&a, &c)
        } else {
            (&c, &a)
        };
        let mut chain = Chain::new(v1.clone()).unwrap();
        for s in [&v2, win, lose] {
            chain.add(s.clone()).unwrap();
        }
        let bob = key_hex(&p.bob);
        assert_eq!(
            chain.check_message(&win.hash(), &bob),
            MessageCheck::Current
        );
        assert_eq!(chain.check_message(&v1.hash(), &bob), MessageCheck::Earlier);
        assert_eq!(
            chain.check_message(&lose.hash(), &bob),
            MessageCheck::OutOfDate
        );
        assert_eq!(
            chain.check_message(&"0".repeat(64), &bob),
            MessageCheck::Unknown
        );
        let stranger = key_hex(&OwnerKey::generate());
        assert_eq!(
            chain.check_message(&win.hash(), &stranger),
            MessageCheck::NotMember
        );

        // Removed members cannot keep posting by naming an old state.
        let gone = win
            .successor(&p.alice, |d| d.members.retain(|m| m.owner != bob))
            .unwrap();
        chain.add(gone).unwrap();
        assert_eq!(
            chain.check_message(&v1.hash(), &bob),
            MessageCheck::NotMember
        );
    }

    #[test]
    fn answering_a_request_sends_states_in_order() {
        let p = people();
        let v1 = pilot(&p, AddBy::Admins);
        let v2 = v1.successor(&p.alice, |d| d.name = "Two".into()).unwrap();
        let v3 = v2.successor(&p.alice, |d| d.name = "Three".into()).unwrap();
        let chain = Chain::from_states(vec![v1.clone(), v2.clone(), v3.clone()]).unwrap();
        assert_eq!(chain.after(1), vec![v2, v3]);
        // The asker applies them in order and catches up.
        let mut behind = Chain::new(v1).unwrap();
        for s in chain.after(1) {
            behind.add(s).unwrap();
        }
        assert_eq!(behind.current(), chain.current());
    }

    #[test]
    fn a_full_group_fits_one_envelope() {
        let p = people();
        let others: Vec<Member> = (1..MAX_MEMBERS)
            .map(|_| member(&OwnerKey::generate()))
            .collect();
        let g = GroupState::create(
            &p.alice,
            None,
            &"n".repeat(MAX_NAME),
            others,
            AddBy::Any,
            History::Week,
        )
        .unwrap();
        let size = serde_json::to_vec(&g).unwrap().len();
        assert!(size < crate::session::MAX_BODY / 4, "{size} bytes");
    }
}
