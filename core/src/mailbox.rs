//! Group mailboxes (ADR 0025, slice 0): the key a group's members share for one mailbox and the
//! exact bytes they sign when they make a request to a relay.
//!
//! A relay holds one Ed25519 *public* key per mailbox generation and checks every request
//! against it. It learns nothing else about the group: not who the members are, not what is
//! inside an entry. Every signature also names the **device** making the request (the relay
//! knows which device authenticated the connection), so a signature captured on one connection
//! cannot be replayed by another device, and each kind of request has its own context string,
//! so a signature for one cannot be used for another. Fields are length-prefixed, so no two
//! different requests produce the same bytes.

use crate::wire::GroupKind;
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use vodozemac::{Ed25519PublicKey, Ed25519SecretKey, Ed25519Signature};

const CREATE_CTX: &[u8] = b"spike-mailbox-create-v1";
const APPEND_CTX: &[u8] = b"spike-mailbox-append-v1";
const READ_CTX: &[u8] = b"spike-mailbox-read-v1";
const SUBSCRIBE_CTX: &[u8] = b"spike-mailbox-subscribe-v1";

/// 32 hex characters (128 random bits): a mailbox's address on a relay.
pub fn is_valid_mailbox_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The key every member of one mailbox generation holds. Whoever holds it can read and append;
/// a new key replaces it when a member is removed.
pub struct MailboxKey(Ed25519SecretKey);

impl MailboxKey {
    pub fn generate() -> Self {
        Self(Ed25519SecretKey::new())
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let b: [u8; 32] = hex::decode(s.trim())?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        Ok(Self(Ed25519SecretKey::from_slice(&b)))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(*self.0.to_bytes())
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.0.public_key().as_bytes())
    }

    /// Sign request bytes made by one of this module's `*_bytes` functions.
    pub fn sign(&self, msg: &[u8]) -> String {
        hex::encode(self.0.sign(msg).to_bytes())
    }
}

/// A random mailbox id.
pub fn new_mailbox_id() -> String {
    use rand::RngCore;
    let mut id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut id);
    hex::encode(id)
}

fn frame(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(&(p.len() as u32).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// What the new mailbox's key signs to register it: proof that whoever asks holds the private key.
pub fn create_bytes(device: &str, mailbox: &str, pubkey_hex: &str) -> Vec<u8> {
    frame(&[
        CREATE_CTX,
        device.as_bytes(),
        mailbox.as_bytes(),
        pubkey_hex.as_bytes(),
    ])
}

/// What the mailbox key signs for an append. `seal` is the (new mailbox id, its public key) a
/// commit hands over to when it closes this mailbox.
pub fn append_bytes(
    device: &str,
    mailbox: &str,
    kind: GroupKind,
    expected_seq: Option<u64>,
    blob: &[u8],
    seal: Option<(&str, &str)>,
) -> Vec<u8> {
    let digest = Sha256::digest(blob);
    let expected = match expected_seq {
        Some(n) => [&[1u8][..], &n.to_be_bytes()].concat(),
        None => vec![0u8],
    };
    let seal = match seal {
        Some((mailbox, pubkey)) => frame(&[b"seal", mailbox.as_bytes(), pubkey.as_bytes()]),
        None => b"noseal".to_vec(),
    };
    frame(&[
        APPEND_CTX,
        device.as_bytes(),
        mailbox.as_bytes(),
        &[kind.byte()],
        &expected,
        &digest,
        &seal,
    ])
}

pub fn read_bytes(device: &str, mailbox: &str, after_seq: u64, limit: u32) -> Vec<u8> {
    frame(&[
        READ_CTX,
        device.as_bytes(),
        mailbox.as_bytes(),
        &after_seq.to_be_bytes(),
        &limit.to_be_bytes(),
    ])
}

pub fn subscribe_bytes(device: &str, mailbox: &str) -> Vec<u8> {
    frame(&[SUBSCRIBE_CTX, device.as_bytes(), mailbox.as_bytes()])
}

/// Check a hex signature over `msg` against a hex public key.
pub fn verify(pubkey_hex: &str, msg: &[u8], sig_hex: &str) -> Result<()> {
    let pk: [u8; 32] = hex::decode(pubkey_hex)?
        .try_into()
        .map_err(|_| Error::BadSignature)?;
    let sig: [u8; 64] = hex::decode(sig_hex)?
        .try_into()
        .map_err(|_| Error::BadSignature)?;
    let pk = Ed25519PublicKey::from_slice(&pk).map_err(|_| Error::BadSignature)?;
    let sig = Ed25519Signature::from_slice(&sig).map_err(|_| Error::BadSignature)?;
    pk.verify(msg, &sig).map_err(|_| Error::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "aa";
    const M: &str = "bb";

    #[test]
    fn a_signature_is_good_only_for_the_exact_request() {
        let k = MailboxKey::generate();
        let pk = k.public_hex();
        let msg = append_bytes(D, M, GroupKind::Message, None, b"hello", None);
        let sig = k.sign(&msg);
        assert!(verify(&pk, &msg, &sig).is_ok());
        // Another device, mailbox, kind, blob, order number or seal is a different request.
        for other in [
            append_bytes("ab", M, GroupKind::Message, None, b"hello", None),
            append_bytes(D, "bc", GroupKind::Message, None, b"hello", None),
            append_bytes(D, M, GroupKind::Commit, None, b"hello", None),
            append_bytes(D, M, GroupKind::Message, None, b"hellp", None),
            append_bytes(D, M, GroupKind::Message, Some(0), b"hello", None),
            append_bytes(D, M, GroupKind::Message, None, b"hello", Some(("cc", "dd"))),
        ] {
            assert!(verify(&pk, &other, &sig).is_err());
        }
        // Someone else's key does not verify it.
        assert!(verify(&MailboxKey::generate().public_hex(), &msg, &sig).is_err());
    }

    #[test]
    fn each_kind_of_request_has_its_own_signature_space() {
        let k = MailboxKey::generate();
        let pk = k.public_hex();
        let read = read_bytes(D, M, 0, 10);
        let sub = subscribe_bytes(D, M);
        let create = create_bytes(D, M, &pk);
        let append = append_bytes(D, M, GroupKind::Message, None, b"", None);
        let all = [&read, &sub, &create, &append];
        for (i, a) in all.iter().enumerate() {
            let sig = k.sign(a);
            for (j, b) in all.iter().enumerate() {
                assert_eq!(verify(&pk, b, &sig).is_ok(), i == j, "{i} against {j}");
            }
        }
    }

    #[test]
    fn fields_cannot_be_shifted_between_each_other() {
        // Without length prefixes these two would both be "...ab" + "c" + ...
        assert_ne!(
            create_bytes("a", "bc", "d"),
            create_bytes("ab", "c", "d"),
            "moving a character across a field boundary changes the bytes"
        );
        assert_ne!(read_bytes("a", "b", 1, 2), read_bytes("a", "b", 2, 1));
    }

    #[test]
    fn keys_survive_hex_and_ids_are_checked() {
        let k = MailboxKey::generate();
        let back = MailboxKey::from_hex(&k.to_hex()).unwrap();
        assert_eq!(k.public_hex(), back.public_hex());
        assert!(MailboxKey::from_hex("nope").is_err());
        assert!(is_valid_mailbox_id(&new_mailbox_id()));
        assert!(!is_valid_mailbox_id("AB".repeat(16).as_str()));
        assert!(!is_valid_mailbox_id("abc"));
    }
}
