//! The group lifecycle (ADR 0025 sections 3-7): growing, shrinking, rotating the mailbox, merge
//! only after the relay accepts a commit, and refusing commits that break the rules.

mod support;

use spike_mls::credential::{CredentialError, MapDirectory};
use spike_mls::group::{Device, Error, Group, Outgoing, Processed};
use spike_mls::policy::{Policy, PolicyError, Who};
use support::{directory, Person};

/// A person's device with the group it is in.
struct Member {
    dev: Device,
    group: Option<Group>,
}

impl Member {
    fn new(p: &Person, i: usize) -> Self {
        Self {
            dev: p.mls_device(i),
            group: None,
        }
    }
    fn g(&mut self) -> &mut Group {
        self.group.as_mut().unwrap()
    }
    fn kp(&self) -> Vec<u8> {
        self.dev.key_package()
    }
    fn process(&mut self, dir: &MapDirectory, wire: &[u8]) -> Result<Processed, Error> {
        self.group.as_mut().unwrap().process(&self.dev, dir, wire)
    }
    fn encrypt(&mut self, msg: &[u8]) -> Result<Vec<u8>, Error> {
        self.group.as_mut().unwrap().encrypt(&self.dev, msg)
    }
    fn add(&mut self, dir: &MapDirectory, kps: &[Vec<u8>]) -> Result<Outgoing, Error> {
        self.group.as_mut().unwrap().add(&self.dev, dir, kps)
    }
    fn remove(&mut self, devices: &[[u8; 16]]) -> Result<Outgoing, Error> {
        self.group.as_mut().unwrap().remove(&self.dev, devices)
    }
    fn set_policy(&mut self, p: Policy) -> Result<Outgoing, Error> {
        self.group.as_mut().unwrap().set_policy(&self.dev, p)
    }
    fn confirm(&mut self) {
        self.group.as_mut().unwrap().confirm(&self.dev).unwrap()
    }
    fn discard(&mut self) {
        self.group.as_mut().unwrap().discard(&self.dev).unwrap()
    }
    fn clear_pending(&mut self) {
        spike_mls::testing::clear_pending(&self.dev, self.group.as_mut().unwrap())
    }
    fn unchecked_remove(&mut self, device: &[u8; 16]) -> Vec<u8> {
        spike_mls::testing::unchecked_remove(&self.dev, self.group.as_mut().unwrap(), device)
    }
    fn unchecked_policy(&mut self, p: &Policy) -> Vec<u8> {
        spike_mls::testing::unchecked_policy(&self.dev, self.group.as_mut().unwrap(), p)
    }
}

/// Alice (admin) creates a group and adds each of `others` in one commit. Everyone ends up joined.
fn grow(alice: &mut Member, others: &mut [&mut Member], dir: &MapDirectory, policy: Policy) {
    alice.group = Some(Group::create(&alice.dev, policy).unwrap());
    if others.is_empty() {
        return;
    }
    let kps: Vec<Vec<u8>> = others.iter().map(|m| m.kp()).collect();
    let out = alice.add(dir, &kps).unwrap();
    alice.confirm();
    for m in others.iter_mut() {
        m.group = Some(
            Group::join(
                &m.dev,
                dir,
                out.welcome.as_ref().unwrap(),
                out.join_bundle.as_ref().unwrap(),
            )
            .unwrap(),
        );
    }
}

fn policy_for(admin: &Person) -> Policy {
    Policy::new(vec![admin.owner_pub()])
}

#[test]
fn a_group_grows_and_everyone_reads_what_is_sent() {
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut b, mut c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    grow(&mut a, &mut [&mut b, &mut c], &dir, policy_for(&alice));

    assert_eq!(a.g().epoch(), b.g().epoch());
    assert_eq!(a.g().members().len(), 3);
    assert_eq!(a.g().mailbox_generation(), 0);
    assert_eq!(
        a.g().mailbox_material().signing_public(),
        c.g().mailbox_material().signing_public(),
        "everyone derives the same mailbox key"
    );

    let wire = a.encrypt(b"hello group").unwrap();
    for m in [&mut b, &mut c] {
        match m.process(&dir, &wire).unwrap() {
            Processed::Message(bytes, _) => assert_eq!(bytes, b"hello group"),
            other => panic!("{other:?}"),
        }
    }
    let wire = c.encrypt(b"hi back").unwrap();
    assert!(matches!(a.process(&dir, &wire).unwrap(), Processed::Message(m, _) if m == b"hi back"));
}

#[test]
fn a_commit_changes_nothing_until_it_is_confirmed() {
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut b, c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));
    let epoch = a.g().epoch();

    let out = a.add(&dir, &[c.kp()]).unwrap();
    assert!(a.g().has_pending());
    assert_eq!(a.g().epoch(), epoch, "not applied yet");
    assert_eq!(a.g().members().len(), 2);
    assert!(out.successor.is_none(), "an add keeps the mailbox");

    // The relay said no: drop it and carry on from the same state.
    a.discard();
    assert!(!a.g().has_pending());
    assert_eq!(a.g().epoch(), epoch);
    let wire = a.encrypt(b"still here").unwrap();
    assert!(matches!(
        b.process(&dir, &wire).unwrap(),
        Processed::Message(..)
    ));

    // The next attempt is accepted.
    a.add(&dir, &[c.kp()]).unwrap();
    a.confirm();
    assert_eq!(a.g().epoch(), epoch + 1);
    assert_eq!(a.g().members().len(), 3);
}

#[test]
fn a_commit_that_won_at_the_relay_replaces_our_pending_one() {
    let (alice, bob, carol, dave) = (
        Person::new(1),
        Person::new(1),
        Person::new(1),
        Person::new(1),
    );
    let dir = directory(&[&alice, &bob, &carol, &dave]);
    let (mut a, mut b) = (Member::new(&alice, 0), Member::new(&bob, 0));
    let (c, d) = (Member::new(&carol, 0), Member::new(&dave, 0));
    let mut policy = policy_for(&alice);
    policy.admins.push(hex::encode(bob.owner_pub()));
    grow(&mut a, &mut [&mut b], &dir, policy);

    // Both build a commit on the same epoch; Bob's is accepted first.
    let _mine = a.add(&dir, &[c.kp()]).unwrap();
    let theirs = b.add(&dir, &[d.kp()]).unwrap();
    b.confirm();

    let r = a.process(&dir, &theirs.commit).unwrap();
    assert!(matches!(r, Processed::Commit { .. }), "{r:?}");
    assert!(!a.g().has_pending(), "ours lost the race and is gone");
    assert_eq!(a.g().epoch(), b.g().epoch());
    assert_eq!(a.g().members().len(), 3);
    // Redo on the winner.
    a.add(&dir, &[c.kp()]).unwrap();
    a.confirm();
    assert_eq!(a.g().members().len(), 4);
}

#[test]
fn a_commit_that_does_not_validate_leaves_our_pending_commit_alone() {
    // A bad commit from someone else must not cost us the commit we are waiting on.
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut b, c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));

    a.add(&dir, &[c.kp()]).unwrap();
    // Bob is not an admin: his own add is built unchecked and must be refused by Alice.
    let bad = unchecked_add(&mut b, &c.kp());
    let r = a.process(&dir, &bad);
    assert!(
        matches!(r, Err(Error::Policy(PolicyError::NotAllowed("add")))),
        "{r:?}"
    );
    assert!(a.g().has_pending(), "our pending commit survives");
    a.confirm();
    assert_eq!(a.g().members().len(), 3);
}

#[test]
fn our_own_commit_coming_back_from_the_relay_is_recognized() {
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut b, c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));
    let out = a.add(&dir, &[c.kp()]).unwrap();
    assert!(matches!(
        a.process(&dir, &out.commit).unwrap(),
        Processed::Own
    ));
    a.confirm();
    let epoch = a.g().epoch();
    assert!(matches!(
        a.process(&dir, &out.commit).unwrap(),
        Processed::Own
    ));
    assert_eq!(a.g().epoch(), epoch);
}

#[test]
fn removing_someone_rotates_the_mailbox_and_she_cannot_follow() {
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut b, mut c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    grow(&mut a, &mut [&mut b, &mut c], &dir, policy_for(&alice));
    let old_public = a.g().mailbox_material().signing_public();

    let out = a.remove(&[carol.device_bytes(0)]).unwrap();
    let (generation, secret) = out.successor.expect("a removal rotates the mailbox");
    assert_eq!(generation, 1);
    a.confirm();
    assert_eq!(a.g().mailbox_generation(), 1);
    let new_public = a.g().mailbox_material().signing_public();
    assert_ne!(new_public, old_public);
    assert_eq!(
        new_public,
        spike_mls::outer::Material::derive(&secret).signing_public()
    );

    // Bob gets the same successor from the commit alone.
    let r = b.process(&dir, &out.commit).unwrap();
    assert!(
        matches!(
            r,
            Processed::Commit {
                rotated: true,
                removed_me: false,
                ..
            }
        ),
        "{r:?}"
    );
    assert_eq!(b.g().mailbox_material().signing_public(), new_public);

    // Carol learns she is out, and has nothing that leads to the new key.
    let r = c.process(&dir, &out.commit).unwrap();
    assert!(
        matches!(
            r,
            Processed::Commit {
                removed_me: true,
                ..
            }
        ),
        "{r:?}"
    );
    assert_eq!(
        c.g().mailbox_generation(),
        0,
        "she is stuck on the old generation"
    );
    assert_eq!(c.g().mailbox_material().signing_public(), old_public);
    assert!(c.encrypt(b"let me back in").is_err());

    // A message after the removal: Bob reads it, Carol cannot.
    let wire = a.encrypt(b"after").unwrap();
    assert!(b.process(&dir, &wire).is_ok());
    assert!(c.process(&dir, &wire).is_err());
}

#[test]
fn a_new_member_is_handed_the_current_mailbox_generation() {
    let (alice, bob, carol, dave) = (
        Person::new(1),
        Person::new(1),
        Person::new(1),
        Person::new(1),
    );
    let dir = directory(&[&alice, &bob, &carol, &dave]);
    let (mut a, mut b, mut c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    let mut d = Member::new(&dave, 0);
    grow(&mut a, &mut [&mut b, &mut c], &dir, policy_for(&alice));
    let out = a.remove(&[carol.device_bytes(0)]).unwrap();
    a.confirm();
    b.process(&dir, &out.commit).unwrap();
    assert_eq!(a.g().mailbox_generation(), 1);

    let add = a.add(&dir, &[d.kp()]).unwrap();
    assert!(add.successor.is_none());
    a.confirm();
    d.group = Some(
        Group::join(
            &d.dev,
            &dir,
            add.welcome.as_ref().unwrap(),
            add.join_bundle.as_ref().unwrap(),
        )
        .unwrap(),
    );
    assert_eq!(d.g().mailbox_generation(), 1);
    assert_eq!(
        d.g().mailbox_material().signing_public(),
        a.g().mailbox_material().signing_public(),
        "an old member's epoch-derived key would differ; the bundle carries the real one"
    );
}

#[test]
fn a_join_bundle_that_is_wrong_or_from_somewhere_else_is_refused() {
    let (alice, bob, carol, dave) = (
        Person::new(1),
        Person::new(1),
        Person::new(1),
        Person::new(1),
    );
    let dir = directory(&[&alice, &bob, &carol, &dave]);
    let (mut a, mut b) = (Member::new(&alice, 0), Member::new(&bob, 0));
    let (c, d) = (Member::new(&carol, 0), Member::new(&dave, 0));
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));
    let for_carol = a.add(&dir, &[c.kp()]).unwrap();
    a.confirm();
    let for_dave = a.add(&dir, &[d.kp()]).unwrap();
    a.confirm();

    let welcome = for_dave.welcome.as_ref().unwrap();
    let bundle = for_dave.join_bundle.clone().unwrap();
    // A bundle from an earlier epoch.
    assert!(Group::join(
        &d.dev,
        &dir,
        welcome,
        for_carol.join_bundle.as_ref().unwrap()
    )
    .is_err());
    // A damaged one.
    for i in 0..bundle.len() {
        let mut bad = bundle.clone();
        bad[i] ^= 1;
        assert!(
            Group::join(&d.dev, &dir, welcome, &bad).is_err(),
            "byte {i}"
        );
    }
    assert!(Group::join(&d.dev, &dir, welcome, &[]).is_err());
    assert!(Group::join(&d.dev, &dir, welcome, &bundle).is_ok());
}

#[test]
fn a_message_sent_just_before_a_commit_still_arrives_after_it() {
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut b, c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));

    let late = b.encrypt(b"sent in the old epoch").unwrap();
    let out = a.add(&dir, &[c.kp()]).unwrap();
    a.confirm();
    assert!(matches!(
        a.process(&dir, &late).unwrap(),
        Processed::Message(..)
    ));
    let _ = out;
}

// --- commits that break the rules ---------------------------------------------------------------

/// Commit an add the way a rule-breaking member would: straight through openmls, no checks.
fn unchecked_add(m: &mut Member, key_package: &[u8]) -> Vec<u8> {
    spike_mls::testing::unchecked_add(&m.dev, m.group.as_mut().unwrap(), key_package)
}

/// What every receiver must do with a commit that breaks a rule: refuse it, change nothing, and
/// still be able to carry on with the commits that follow.
fn assert_refused(m: &mut Member, dir: &MapDirectory, wire: &[u8]) -> Error {
    let (epoch, members) = (m.g().epoch(), m.g().members().len());
    let err = m.process(dir, wire).expect_err("must be refused");
    assert_eq!(m.g().epoch(), epoch, "the epoch did not move");
    assert_eq!(
        m.g().members().len(),
        members,
        "nobody was added or removed"
    );
    err
}

#[test]
fn receivers_refuse_a_commit_adding_someone_the_directory_does_not_know() {
    let (alice, bob, mallory) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob]); // Mallory is not known
    let (mut a, mut b) = (Member::new(&alice, 0), Member::new(&bob, 0));
    let m = Member::new(&mallory, 0);
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));

    // The honest path stops it at the committer...
    let r = a.add(&dir, &[m.kp()]);
    assert!(
        matches!(r, Err(Error::Credential(CredentialError::UnknownOwner))),
        "{:?}",
        r.err()
    );
    assert!(!a.g().has_pending());
    // ...and a committer who skips the check is stopped by the receiver.
    let bad = unchecked_add(&mut a, &m.kp());
    let err = assert_refused(&mut b, &dir, &bad);
    assert!(
        matches!(err, Error::Credential(CredentialError::UnknownOwner)),
        "{err:?}"
    );
}

#[test]
fn receivers_refuse_a_revoked_device_and_ask_for_a_refresh_when_behind() {
    let (alice, bob, mut carol) = (Person::new(1), Person::new(1), Person::new(2));
    let (mut a, mut b) = (Member::new(&alice, 0), Member::new(&bob, 0));
    let dir = directory(&[&alice, &bob, &carol]);
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));

    // Carol's second device was revoked: both Alice's and Bob's directories know.
    let stolen = Member::new(&carol, 1);
    carol.revoke(1);
    let dir_now = directory(&[&alice, &bob, &carol]);
    let bad = unchecked_add(&mut a, &stolen.kp());
    let err = assert_refused(&mut b, &dir_now, &bad);
    assert!(
        matches!(err, Error::Credential(CredentialError::Revoked)),
        "{err:?}"
    );

    // A brand-new device of Carol's, from a list newer than Bob's directory.
    let new = carol.add_device();
    let newer = Member::new(&carol, new);
    let bad = unchecked_add(&mut a, &newer.kp());
    let err = assert_refused(&mut b, &dir_now, &bad);
    assert!(
        matches!(
            err,
            Error::Credential(CredentialError::StaleDirectory { .. })
        ),
        "{err:?}"
    );
    // The commit was not lost: with a fresh directory the very same bytes are accepted.
    let fresh = directory(&[&alice, &bob, &carol]);
    // (Alice's unchecked commit is pending on her side; she needs a directory that knows Carol
    // to confirm it.)
    let r = b.process(&fresh, &bad).unwrap();
    assert!(matches!(r, Processed::Commit { .. }), "{r:?}");
    assert_eq!(b.g().members().len(), 3);
}

#[test]
fn receivers_refuse_commits_from_people_without_the_authority() {
    let (alice, bob, carol, dave) = (
        Person::new(1),
        Person::new(1),
        Person::new(1),
        Person::new(1),
    );
    let dir = directory(&[&alice, &bob, &carol, &dave]);
    let (mut a, mut b, mut c) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&carol, 0),
    );
    let d = Member::new(&dave, 0);
    grow(&mut a, &mut [&mut b, &mut c], &dir, policy_for(&alice));

    // Bob (not an admin) adds Dave behind everyone's back.
    let bad = unchecked_add(&mut b, &d.kp());
    let err = assert_refused(&mut a, &dir, &bad);
    assert!(
        matches!(err, Error::Policy(PolicyError::NotAllowed("add"))),
        "{err:?}"
    );
    let err = assert_refused(&mut c, &dir, &bad);
    assert!(
        matches!(err, Error::Policy(PolicyError::NotAllowed("add"))),
        "{err:?}"
    );
    // Bob removes Carol.
    b.clear_pending();
    let bad = b.unchecked_remove(&carol.device_bytes(0));
    let err = assert_refused(&mut a, &dir, &bad);
    assert!(
        matches!(err, Error::Policy(PolicyError::NotAllowed("remove"))),
        "{err:?}"
    );
    // Bob rewrites the policy to make himself an admin.
    b.clear_pending();
    let mut mine = policy_for(&alice);
    mine.admins.push(hex::encode(bob.owner_pub()));
    let bad = b.unchecked_policy(&mine);
    let err = assert_refused(&mut a, &dir, &bad);
    assert!(
        matches!(err, Error::Policy(PolicyError::NotAllowed("policy"))),
        "{err:?}"
    );

    // None of it stopped the group: Alice's own commits still go through.
    b.clear_pending();
    let out = a.add(&dir, &[d.kp()]).unwrap();
    a.confirm();
    assert!(matches!(
        b.process(&dir, &out.commit).unwrap(),
        Processed::Commit { .. }
    ));
}

#[test]
fn the_size_caps_and_one_leaf_per_device_are_enforced_on_receipt() {
    let (alice, bob, carol, dave) = (
        Person::new(1),
        Person::new(1),
        Person::new(1),
        Person::new(1),
    );
    let dir = directory(&[&alice, &bob, &carol, &dave]);
    let (mut a, mut b) = (Member::new(&alice, 0), Member::new(&bob, 0));
    let (c, d) = (Member::new(&carol, 0), Member::new(&dave, 0));
    let mut policy = policy_for(&alice);
    policy.max_people = 3;
    grow(&mut a, &mut [&mut b], &dir, policy);

    // Carol fits, Dave does not.
    let out = a.add(&dir, &[c.kp()]).unwrap();
    a.confirm();
    assert!(b.process(&dir, &out.commit).is_ok());
    let r = a.add(&dir, &[d.kp()]);
    assert!(
        matches!(r, Err(Error::Policy(PolicyError::TooMany("people")))),
        "{:?}",
        r.err()
    );
    let bad = unchecked_add(&mut a, &d.kp());
    let err = assert_refused(&mut b, &dir, &bad);
    assert!(
        matches!(err, Error::Policy(PolicyError::TooMany("people"))),
        "{err:?}"
    );
}

#[test]
fn the_policy_travels_with_the_group_and_admins_can_change_it() {
    let (alice, bob) = (Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob]);
    let (mut a, mut b) = (Member::new(&alice, 0), Member::new(&bob, 0));
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));
    assert_eq!(b.g().policy(), a.g().policy());
    assert_eq!(b.g().policy().add, Who::Admins);

    let mut next = a.g().policy().clone();
    next.add = Who::Members;
    let out = a.set_policy(next.clone()).unwrap();
    a.confirm();
    b.process(&dir, &out.commit).unwrap();
    assert_eq!(b.g().policy().add, Who::Members);
    assert_eq!(a.g().policy(), &next);
    // Bob is not an admin and may not do it himself.
    let r = b.set_policy(next);
    assert!(
        matches!(r, Err(Error::Policy(PolicyError::NotAllowed("policy")))),
        "{:?}",
        r.err()
    );
}

#[test]
fn garbage_is_an_error_not_a_panic_and_changes_nothing() {
    let (alice, bob) = (Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob]);
    let (mut a, mut b) = (Member::new(&alice, 0), Member::new(&bob, 0));
    grow(&mut a, &mut [&mut b], &dir, policy_for(&alice));
    let good = a.encrypt(b"x").unwrap();
    for bad in [
        vec![],
        vec![0],
        vec![0xff; 64],
        good[..good.len() / 2].to_vec(),
    ] {
        assert!(b.process(&dir, &bad).is_err());
    }
    // And the real one still works afterwards.
    assert!(matches!(
        b.process(&dir, &good).unwrap(),
        Processed::Message(..)
    ));
}

#[test]
fn a_welcome_with_a_member_nobody_vouches_for_is_refused_and_can_be_retried() {
    // Alice adds Bob and, in the same welcome, Mallory (not in anyone's directory).
    let (alice, bob, mallory) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob]);
    let (mut a, b, m) = (
        Member::new(&alice, 0),
        Member::new(&bob, 0),
        Member::new(&mallory, 0),
    );
    grow(&mut a, &mut [], &dir, policy_for(&alice));
    let (_commit, welcome) = spike_mls::testing::unchecked_add_many(
        &a.dev,
        a.group.as_mut().unwrap(),
        &[b.kp(), m.kp()],
    );
    // The bundle is junk, on purpose: the member check must refuse first, for the right reason.
    let r = Group::join(&b.dev, &dir, &welcome, &[0u8; 80]);
    assert!(
        matches!(r, Err(Error::Credential(CredentialError::UnknownOwner))),
        "{:?}",
        r.err()
    );
    // Once the directory knows Mallory the welcome gets past the member check (and then fails on
    // the junk bundle), which also shows the refusal above did not use up Bob's key package.
    let dir2 = directory(&[&alice, &bob, &mallory]);
    let r = Group::join(&b.dev, &dir2, &welcome, &[0u8; 80]);
    assert!(matches!(r, Err(Error::Bundle)), "{:?}", r.err());
}

#[test]
fn a_commit_from_a_revoked_device_is_refused_even_when_it_only_adds_people() {
    // Alice's second device is lost and she revokes it, but it is still in the group's tree until
    // someone removes it, and it still holds her admin rights through its credential.
    let (mut alice, bob, carol) = (Person::new(2), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a0, mut a1, mut b) = (
        Member::new(&alice, 0),
        Member::new(&alice, 1),
        Member::new(&bob, 0),
    );
    let c = Member::new(&carol, 0);
    grow(&mut a0, &mut [&mut a1, &mut b], &dir, policy_for(&alice));
    alice.revoke(1);
    let dir_now = directory(&[&alice, &bob, &carol]);

    // The thief adds Carol. A commit that only adds has no new leaf of the sender's to check, so
    // the sender's own credential has to be checked too.
    let bad = unchecked_add(&mut a1, &c.kp());
    let err = assert_refused(&mut b, &dir_now, &bad);
    assert!(
        matches!(err, Error::Credential(CredentialError::Revoked)),
        "{err:?}"
    );
    assert!(matches!(
        assert_refused(&mut a0, &dir_now, &bad),
        Error::Credential(CredentialError::Revoked)
    ));
    // The same with no update path at all (legal when a commit only adds): nothing in the commit
    // belongs to the thief except the sender's place in the tree, which is what gets checked.
    let bad = spike_mls::testing::unchecked_add_without_path(
        &a1.dev,
        a1.group.as_mut().unwrap(),
        &c.kp(),
    );
    let err = assert_refused(&mut b, &dir_now, &bad);
    assert!(
        matches!(err, Error::Credential(CredentialError::Revoked)),
        "{err:?}"
    );
    // Alice removes the lost device; the group carries on without it.
    let out = a0.remove(&[alice.device_bytes(1)]).unwrap();
    a0.confirm();
    assert!(matches!(
        b.process(&dir_now, &out.commit).unwrap(),
        Processed::Commit { .. }
    ));
    assert_eq!(b.g().members().len(), 2);
}

#[test]
fn a_last_resort_key_package_can_be_used_again_and_a_one_time_one_cannot() {
    let (alice, bob) = (Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob]);
    let b = Member::new(&bob, 0);
    let (mut a1, mut a2) = (Member::new(&alice, 0), Member::new(&alice, 0));
    grow(&mut a1, &mut [], &dir, policy_for(&alice));
    grow(&mut a2, &mut [], &dir, policy_for(&alice));

    // The same last-resort package welcomes Bob into two different groups.
    let kp = b.dev.last_resort_key_package();
    for a in [&mut a1, &mut a2] {
        let out = a.add(&dir, std::slice::from_ref(&kp)).unwrap();
        a.confirm();
        let g = Group::join(
            &b.dev,
            &dir,
            out.welcome.as_ref().unwrap(),
            out.join_bundle.as_ref().unwrap(),
        )
        .expect("a last-resort package is not used up");
        assert_eq!(g.members().len(), 2);
    }

    // A one-time package is used up by the first Welcome.
    let (mut a3, mut a4) = (Member::new(&alice, 0), Member::new(&alice, 0));
    grow(&mut a3, &mut [], &dir, policy_for(&alice));
    grow(&mut a4, &mut [], &dir, policy_for(&alice));
    let once = b.kp();
    let first = a3.add(&dir, std::slice::from_ref(&once)).unwrap();
    a3.confirm();
    Group::join(
        &b.dev,
        &dir,
        first.welcome.as_ref().unwrap(),
        first.join_bundle.as_ref().unwrap(),
    )
    .unwrap();
    let second = a4.add(&dir, &[once]).unwrap();
    assert!(Group::join(
        &b.dev,
        &dir,
        second.welcome.as_ref().unwrap(),
        second.join_bundle.as_ref().unwrap()
    )
    .is_err());
}
