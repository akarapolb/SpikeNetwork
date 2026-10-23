//! The outer layer and mailbox key material (ADR 0025 sections 4 and 5): everything a relay can
//! see is wrapped so it learns neither MLS headers nor which kind of message it holds, and the
//! relay's per-generation signing key and outer key come from one secret.

use spike_core::owner::verify_raw;
use spike_mls::outer::{open, seal, Kind, Material};

const GID: &[u8] = b"group-1";

#[test]
fn material_is_deterministic_and_separated() {
    let a = Material::derive(&[1; 32]);
    let b = Material::derive(&[1; 32]);
    assert_eq!(a.signing_public(), b.signing_public());
    assert_eq!(a.outer_key(), b.outer_key());
    assert_ne!(
        Material::derive(&[2; 32]).signing_public(),
        a.signing_public()
    );
    // The key that signs for the relay is not the key that encrypts.
    assert_ne!(&a.signing_public()[..], &a.outer_key()[..]);
    // And the relay signature really is made by the public key we would hand the relay.
    let sig = a.sign(b"hello");
    assert!(verify_raw(&a.signing_public(), b"hello", &sig));
    assert!(!verify_raw(&a.signing_public(), b"hellp", &sig));
}

#[test]
fn sealed_messages_open_only_with_the_same_key_context_and_kind() {
    let m = Material::derive(&[1; 32]);
    let sealed = seal(&m, GID, 3, Kind::Commit, b"mls bytes");
    assert_eq!(
        open(&m, GID, 3, Kind::Commit, &sealed).unwrap(),
        b"mls bytes"
    );

    assert!(
        open(&Material::derive(&[2; 32]), GID, 3, Kind::Commit, &sealed).is_err(),
        "other key"
    );
    assert!(
        open(&m, b"group-2", 3, Kind::Commit, &sealed).is_err(),
        "other group"
    );
    assert!(
        open(&m, GID, 4, Kind::Commit, &sealed).is_err(),
        "other generation"
    );
    assert!(
        open(&m, GID, 3, Kind::Message, &sealed).is_err(),
        "other kind"
    );
}

#[test]
fn tampering_and_garbage_are_refused() {
    let m = Material::derive(&[1; 32]);
    let sealed = seal(&m, GID, 0, Kind::Message, b"x");
    for i in 0..sealed.len() {
        let mut bad = sealed.clone();
        bad[i] ^= 1;
        assert!(open(&m, GID, 0, Kind::Message, &bad).is_err(), "byte {i}");
    }
    for len in [0, 1, 23, 24, 25, sealed.len() - 1] {
        assert!(
            open(&m, GID, 0, Kind::Message, &sealed[..len]).is_err(),
            "length {len}"
        );
    }
    assert!(open(&m, GID, 0, Kind::Message, &[]).is_err());
}

#[test]
fn two_seals_of_the_same_message_differ() {
    let m = Material::derive(&[1; 32]);
    assert_ne!(
        seal(&m, GID, 0, Kind::Message, b"same"),
        seal(&m, GID, 0, Kind::Message, b"same")
    );
}
