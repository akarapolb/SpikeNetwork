//! The credential that binds an MLS leaf to a person (ADR 0025, section 3): an owner-key
//! signature over the leaf's key, the device and the device-list version, checked against the
//! owner's signed device list.

mod support;

use openmls::prelude::{BasicCredential, Credential};
use spike_core::owner::OwnerKey;
use spike_mls::credential::{
    certify, verify_leaf, CredentialError, Directory, MapDirectory, IDENTITY_LEN,
};
use support::{directory, Person};

fn credential(identity: Vec<u8>) -> Credential {
    BasicCredential::new(identity).into()
}

const LEAF: [u8; 32] = [7; 32];

#[test]
fn a_certified_device_verifies_against_its_owners_list() {
    let p = Person::new(2);
    let dir = directory(&[&p]);
    let ident = certify(&p.owner, &LEAF, p.device_bytes(1), p.list.version);
    assert_eq!(ident.len(), IDENTITY_LEN);
    let got = verify_leaf(&dir, &credential(ident), &LEAF).unwrap();
    assert_eq!(got.owner, p.owner_pub());
    assert_eq!(got.device, p.device_bytes(1));
    assert_eq!(got.list_version, 1);
}

#[test]
fn the_certificate_binds_the_leaf_key_the_device_and_the_list_version() {
    let p = Person::new(2);
    let dir = directory(&[&p]);
    let ident = certify(&p.owner, &LEAF, p.device_bytes(0), 1);

    // Another leaf key: a credential cannot be lifted onto a different key.
    let r = verify_leaf(&dir, &credential(ident.clone()), &[8; 32]);
    assert!(matches!(r, Err(CredentialError::BadSignature)), "{r:?}");

    // Every field of the identity is covered by the signature.
    for (name, range) in [
        ("owner", 1..33),
        ("device", 33..49),
        ("list version", 49..57),
        ("signature", 57..IDENTITY_LEN),
    ] {
        let mut bad = ident.clone();
        bad[range.start] ^= 1;
        let r = verify_leaf(&dir, &credential(bad), &LEAF);
        assert!(r.is_err(), "changing the {name} must not verify");
    }
}

#[test]
fn an_owner_signing_for_another_persons_device_does_not_make_it_theirs() {
    // Mallory certifies Alice's device id under Mallory's own owner key: the directory has
    // Mallory's list (which does not name that device), so it is refused, and Alice's list is
    // never consulted for Mallory's key.
    let (alice, mallory) = (Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &mallory]);
    let ident = certify(&mallory.owner, &LEAF, alice.device_bytes(0), 1);
    let r = verify_leaf(&dir, &credential(ident), &LEAF);
    assert!(matches!(r, Err(CredentialError::Revoked)), "{r:?}");
}

#[test]
fn an_owner_the_directory_does_not_know_is_refused() {
    let (known, stranger) = (Person::new(1), Person::new(1));
    let dir = directory(&[&known]);
    let ident = certify(&stranger.owner, &LEAF, stranger.device_bytes(0), 1);
    let r = verify_leaf(&dir, &credential(ident), &LEAF);
    assert!(matches!(r, Err(CredentialError::UnknownOwner)), "{r:?}");
}

#[test]
fn a_revoked_device_is_refused_once_the_directory_has_caught_up() {
    let mut p = Person::new(2);
    let ident = certify(&p.owner, &LEAF, p.device_bytes(1), p.list.version);
    assert!(verify_leaf(&directory(&[&p]), &credential(ident.clone()), &LEAF).is_ok());
    p.revoke(1);
    assert_eq!(p.list.version, 2);
    let r = verify_leaf(&directory(&[&p]), &credential(ident), &LEAF);
    assert!(matches!(r, Err(CredentialError::Revoked)), "{r:?}");
}

#[test]
fn a_device_from_a_newer_list_than_we_know_is_stale_not_valid() {
    let mut p = Person::new(1);
    let stale_dir = directory(&[&p]); // knows version 1 only
    let new = p.add_device(); // version 2 names the new device
    let ident = certify(&p.owner, &LEAF, p.device_bytes(new), p.list.version);

    // We cannot tell a device added since from a forgery, so: ask to refresh, do not accept.
    let r = verify_leaf(&stale_dir, &credential(ident.clone()), &LEAF);
    match r {
        Err(CredentialError::StaleDirectory { have, need }) => assert_eq!((have, need), (1, 2)),
        other => panic!("{other:?}"),
    }
    // After the directory is refreshed it verifies.
    assert!(verify_leaf(&directory(&[&p]), &credential(ident), &LEAF).is_ok());
}

#[test]
fn a_list_that_the_owner_did_not_sign_is_not_trusted() {
    // A directory that hands back someone else's list under the owner's name must not be believed.
    struct Liar(spike_core::owner::DeviceList);
    impl Directory for Liar {
        fn device_list(&self, _owner: &[u8; 32]) -> Option<spike_core::owner::DeviceList> {
            Some(self.0.clone())
        }
    }
    let (alice, mallory) = (Person::new(1), Person::new(1));
    let ident = certify(&alice.owner, &LEAF, alice.device_bytes(0), 1);
    let r = verify_leaf(&Liar(mallory.list.clone()), &credential(ident), &LEAF);
    assert!(matches!(r, Err(CredentialError::UnknownOwner)), "{r:?}");

    // And a list whose signature was damaged is not a list at all.
    let mut forged = alice.list.clone();
    forged.version = 99;
    let mut d = MapDirectory::default();
    d.insert(forged);
    let ident = certify(&alice.owner, &LEAF, alice.device_bytes(0), 1);
    let r = verify_leaf(&d, &credential(ident), &LEAF);
    assert!(matches!(r, Err(CredentialError::UnknownOwner)), "{r:?}");
}

#[test]
fn malformed_identities_are_refused_without_panicking() {
    let p = Person::new(1);
    let dir = directory(&[&p]);
    let good = certify(&p.owner, &LEAF, p.device_bytes(0), 1);
    for len in [0, 1, 56, IDENTITY_LEN - 1, IDENTITY_LEN + 1, 4096] {
        let mut v = good.clone();
        v.resize(len, 0);
        let r = verify_leaf(&dir, &credential(v), &LEAF);
        assert!(
            matches!(
                r,
                Err(CredentialError::Malformed | CredentialError::BadSignature)
            ),
            "length {len}: {r:?}"
        );
    }
    // An unknown format version.
    let mut v = good.clone();
    v[0] = 9;
    assert!(matches!(
        verify_leaf(&dir, &credential(v), &LEAF),
        Err(CredentialError::Malformed)
    ));
    // Not a basic credential at all.
    let x509 = Credential::new(openmls::prelude::CredentialType::X509, good.clone());
    assert!(matches!(
        verify_leaf(&dir, &x509, &LEAF),
        Err(CredentialError::Malformed)
    ));
    // A zero owner key is not a key.
    let mut v = good;
    v[1..33].fill(0);
    assert!(verify_leaf(&dir, &credential(v), &LEAF).is_err());
    let _ = OwnerKey::generate();
}
