//! Mailbox key material and the outer layer around MLS messages (ADR 0025 sections 4 and 5).
//!
//! One 32-byte secret per mailbox generation gives two independent keys: an Ed25519 key whose
//! public half the relay holds (it checks signatures on appends) and an XChaCha20-Poly1305 key that
//! wraps every MLS message before it is stored, so the relay sees neither MLS framing nor which kind
//! of message it holds.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use spike_core::mailbox::MailboxKey;
use spike_core::owner::OwnerKey;
use zeroize::Zeroize;

const NONCE: usize = 24;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("cannot open this message")]
pub struct OpenError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Commit = 1,
    Message = 2,
}

/// The keys of one mailbox generation.
pub struct Material {
    signing: OwnerKey,
    seed: [u8; 32],
    id: [u8; 16],
    outer: [u8; 32],
}

impl Material {
    pub fn derive(secret: &[u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(b"spike-mls-mailbox-v1"), secret);
        let mut seed = [0u8; 32];
        let mut outer = [0u8; 32];
        let mut id = [0u8; 16];
        hk.expand(b"relay signing key", &mut seed)
            .expect("32 bytes");
        hk.expand(b"mailbox id", &mut id).expect("16 bytes");
        hk.expand(b"outer aead key", &mut outer).expect("32 bytes");
        let signing = OwnerKey::from_hex(&hex::encode(seed)).expect("32 bytes");
        Self {
            signing,
            seed,
            id,
            outer,
        }
    }

    pub fn signing_public(&self) -> [u8; 32] {
        self.signing.public()
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign_raw(msg)
    }

    /// The key the relay knows this mailbox generation by.
    pub fn mailbox_key(&self) -> MailboxKey {
        MailboxKey::from_hex(&hex::encode(self.seed)).expect("32 bytes")
    }

    /// The mailbox id of this generation: every member derives the same one.
    pub fn mailbox_id(&self) -> String {
        hex::encode(self.id)
    }

    pub fn outer_key(&self) -> [u8; 32] {
        self.outer
    }
}

impl Drop for Material {
    fn drop(&mut self) {
        self.outer.zeroize();
        self.seed.zeroize();
    }
}

fn aad(group: &[u8], generation: u64, kind: Kind) -> Vec<u8> {
    let mut v = Vec::with_capacity(group.len() + 10 + 20);
    v.extend_from_slice(b"spike-mls-outer-v1");
    v.extend_from_slice(&(group.len() as u32).to_be_bytes());
    v.extend_from_slice(group);
    v.extend_from_slice(&generation.to_be_bytes());
    v.push(kind as u8);
    v
}

/// XChaCha20-Poly1305 with a random nonce in front.
pub(crate) fn seal_raw(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("encryption does not fail");
    [&nonce[..], &ct].concat()
}

pub(crate) fn open_raw(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, OpenError> {
    if sealed.len() < NONCE + 16 {
        return Err(OpenError);
    }
    let (nonce, ct) = sealed.split_at(NONCE);
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| OpenError)
}

pub fn seal(m: &Material, group: &[u8], generation: u64, kind: Kind, plaintext: &[u8]) -> Vec<u8> {
    seal_raw(&m.outer, &aad(group, generation, kind), plaintext)
}

pub fn open(
    m: &Material,
    group: &[u8],
    generation: u64,
    kind: Kind,
    sealed: &[u8],
) -> Result<Vec<u8>, OpenError> {
    open_raw(&m.outer, &aad(group, generation, kind), sealed)
}
