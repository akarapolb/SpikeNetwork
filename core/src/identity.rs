//! Device identity: an Olm account (Ed25519 signing key + Curve25519 identity
//! key + one-time prekeys). A [`DeviceCard`] is the shareable public half,
//! with the Curve25519 key signed by the Ed25519 key so the two cannot be
//! split apart or swapped. Prekeys are signed by the same Ed25519 key so a
//! relay cannot invent them.

use crate::wire::WirePrekey;
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use vodozemac::olm::{Account, AccountPickle};
use vodozemac::{Ed25519PublicKey, Ed25519Signature};

const CARD_PREFIX: &str = "spike1:";
const CARD_CTX: &[u8] = b"spike-card-v1";
const AUTH_CTX: &[u8] = b"spike-relay-auth-v1";
const PREKEY_CTX: &[u8] = b"spike-prekey-v1";
const FP_CTX: &[u8] = b"spike-fp-v1";

/// Most prekeys a relay stores per device, and the most an account keeps.
pub const MAX_PREKEYS: usize = 50;

pub struct Identity {
    account: Account,
}

/// A one-time prekey with the owner's signature over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedPrekey {
    pub key: [u8; 32],
    pub sig: [u8; 64],
}

impl SignedPrekey {
    pub fn to_wire(&self) -> WirePrekey {
        WirePrekey {
            key: hex::encode(self.key),
            sig: hex::encode(self.sig),
        }
    }

    pub fn from_wire(w: &WirePrekey) -> Result<Self> {
        Ok(Self {
            key: hex::decode(&w.key)?
                .try_into()
                .map_err(|_| Error::Malformed)?,
            sig: hex::decode(&w.sig)?
                .try_into()
                .map_err(|_| Error::Malformed)?,
        })
    }
}

/// Check that `p` was signed by the device whose Ed25519 key is `ed25519`.
pub fn verify_prekey(ed25519: &[u8; 32], p: &SignedPrekey) -> Result<()> {
    verify(ed25519, &[PREKEY_CTX, &p.key].concat(), &p.sig)
}

impl Identity {
    pub fn generate() -> Result<Self> {
        Ok(Self {
            account: Account::new(),
        })
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.account.pickle())?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        let p: AccountPickle = serde_json::from_str(s)?;
        Ok(Self {
            account: Account::from_pickle(p),
        })
    }

    pub fn card(&self) -> DeviceCard {
        let k = self.account.identity_keys();
        let x25519 = k.curve25519.to_bytes();
        let sig = self.account.sign([CARD_CTX, &x25519].concat()).to_bytes();
        DeviceCard {
            ed25519: *k.ed25519.as_bytes(),
            x25519,
            sig,
        }
    }

    pub fn device_id(&self) -> String {
        device_id_from_ed(&self.ed_public())
    }

    pub fn ed_public(&self) -> [u8; 32] {
        *self.account.identity_keys().ed25519.as_bytes()
    }

    pub fn x_public(&self) -> [u8; 32] {
        self.account.identity_keys().curve25519.to_bytes()
    }

    pub(crate) fn account(&self) -> &Account {
        &self.account
    }

    pub(crate) fn account_mut(&mut self) -> &mut Account {
        &mut self.account
    }

    /// Signature a device presents to a relay to prove it holds its key.
    pub fn sign_auth(&self, nonce: &[u8]) -> [u8; 64] {
        self.account.sign([AUTH_CTX, nonce].concat()).to_bytes()
    }

    /// Generate a new *fallback* prekey (a last-resort key a relay hands out
    /// when all one-time prekeys are used up), sign it, and mark it published.
    /// The previous fallback private key is kept until the next rotation so
    /// messages already in flight still open; the one before that is dropped.
    /// Persist the identity afterwards and upload the result.
    pub fn fresh_fallback_prekey(&mut self) -> SignedPrekey {
        self.account.generate_fallback_key();
        let key = self
            .account
            .fallback_key()
            .values()
            .next()
            .map(|k| k.to_bytes())
            .expect("fallback key was just generated");
        self.account.mark_keys_as_published();
        let sig = self.account.sign([PREKEY_CTX, &key].concat()).to_bytes();
        SignedPrekey { key, sig }
    }

    /// Generate `n` new one-time prekeys (capped by the account's limit),
    /// sign them, and mark them published. The caller must persist the
    /// identity afterwards and upload the result to a relay.
    pub fn fresh_prekeys(&mut self, n: usize) -> Vec<SignedPrekey> {
        self.account.generate_one_time_keys(n.min(MAX_PREKEYS));
        let keys: Vec<[u8; 32]> = self
            .account
            .one_time_keys()
            .values()
            .map(|k| k.to_bytes())
            .collect();
        self.account.mark_keys_as_published();
        keys.into_iter()
            .map(|key| {
                let sig = self.account.sign([PREKEY_CTX, &key].concat()).to_bytes();
                SignedPrekey { key, sig }
            })
            .collect()
    }
}

/// Verify a relay-auth signature made by [`Identity::sign_auth`].
pub fn verify_auth(ed25519: &[u8; 32], nonce: &[u8], sig: &[u8; 64]) -> Result<()> {
    verify(ed25519, &[AUTH_CTX, nonce].concat(), sig)
}

fn verify(ed25519: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> Result<()> {
    let pk = Ed25519PublicKey::from_slice(ed25519).map_err(|_| Error::BadSignature)?;
    let sig = Ed25519Signature::from_slice(sig).map_err(|_| Error::BadSignature)?;
    pk.verify(msg, &sig).map_err(|_| Error::BadSignature)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCard {
    pub ed25519: [u8; 32],
    /// Curve25519 identity key (named x25519 for the DH it is used for).
    pub x25519: [u8; 32],
    sig: [u8; 64],
}

impl DeviceCard {
    pub fn encode(&self) -> String {
        let mut b = Vec::with_capacity(128);
        b.extend_from_slice(&self.ed25519);
        b.extend_from_slice(&self.x25519);
        b.extend_from_slice(&self.sig);
        format!("{CARD_PREFIX}{}", hex::encode(b))
    }

    pub fn decode(s: &str) -> Result<Self> {
        let body = s
            .trim()
            .strip_prefix(CARD_PREFIX)
            .ok_or(Error::BadCard("missing spike1: prefix"))?;
        let b = hex::decode(body)?;
        if b.len() != 128 {
            return Err(Error::BadCard("wrong length"));
        }
        let ed25519: [u8; 32] = b[..32].try_into().unwrap();
        let x25519: [u8; 32] = b[32..64].try_into().unwrap();
        let sig: [u8; 64] = b[64..].try_into().unwrap();
        verify(&ed25519, &[CARD_CTX, &x25519].concat(), &sig)
            .map_err(|_| Error::BadCard("key binding signature invalid"))?;
        Ok(Self {
            ed25519,
            x25519,
            sig,
        })
    }

    pub fn device_id(&self) -> String {
        device_id_from_ed(&self.ed25519)
    }

    /// Short string for out-of-band comparison, e.g. read aloud or compared on
    /// a second channel. Covers both keys.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(FP_CTX);
        h.update(self.ed25519);
        h.update(self.x25519);
        let d = h.finalize();
        let s = hex::encode(&d[..15]);
        s.as_bytes()
            .chunks(5)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    }
}

/// 32 hex characters; this is the address relays route on.
pub fn device_id_from_ed(ed25519: &[u8; 32]) -> String {
    hex::encode(&Sha256::digest(ed25519)[..16])
}

pub fn is_valid_device_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
