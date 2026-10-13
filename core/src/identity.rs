//! Device identity: an Ed25519 signing key (relay auth, card binding) and an
//! X25519 static key (Noise sessions). A [`DeviceCard`] is the shareable public
//! half, with the X25519 key signed by the Ed25519 key so the two cannot be
//! split apart or swapped.

use crate::{Error, Result, NOISE_PARAMS};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CARD_PREFIX: &str = "spike1:";
const CARD_CTX: &[u8] = b"spike-card-v1";
const AUTH_CTX: &[u8] = b"spike-relay-auth-v1";
const FP_CTX: &[u8] = b"spike-fp-v1";

pub struct Identity {
    signing: SigningKey,
    x_secret: [u8; 32],
    x_public: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    ed25519_secret: String,
    x25519_secret: String,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let signing = SigningKey::generate(&mut OsRng);
        let kp = snow::Builder::new(NOISE_PARAMS.parse()?).generate_keypair()?;
        Self::from_parts(signing, &kp.private)
    }

    fn from_parts(signing: SigningKey, x_secret: &[u8]) -> Result<Self> {
        let x_secret: [u8; 32] = x_secret.try_into().map_err(|_| Error::Malformed)?;
        // Derive the public key from the secret so the two can never disagree.
        let x_public = x25519_public(&x_secret)?;
        Ok(Self {
            signing,
            x_secret,
            x_public,
        })
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&IdentityFile {
            ed25519_secret: hex::encode(self.signing.to_bytes()),
            x25519_secret: hex::encode(self.x_secret),
        })?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        let f: IdentityFile = serde_json::from_str(s)?;
        let ed: [u8; 32] = hex::decode(f.ed25519_secret)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        Self::from_parts(SigningKey::from_bytes(&ed), &hex::decode(f.x25519_secret)?)
    }

    pub fn card(&self) -> DeviceCard {
        let ed25519 = self.signing.verifying_key().to_bytes();
        let sig = self.signing.sign(&card_msg(&self.x_public)).to_bytes();
        DeviceCard {
            ed25519,
            x25519: self.x_public,
            sig,
        }
    }

    pub fn device_id(&self) -> String {
        device_id_from_ed(&self.signing.verifying_key().to_bytes())
    }

    pub(crate) fn x_secret(&self) -> &[u8; 32] {
        &self.x_secret
    }

    pub fn x_public(&self) -> [u8; 32] {
        self.x_public
    }

    pub fn ed_public(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Signature a device presents to a relay to prove it holds its key.
    pub fn sign_auth(&self, nonce: &[u8]) -> [u8; 64] {
        self.signing.sign(&auth_msg(nonce)).to_bytes()
    }
}

/// Verify a relay-auth signature made by [`Identity::sign_auth`].
pub fn verify_auth(ed25519: &[u8; 32], nonce: &[u8], sig: &[u8; 64]) -> Result<()> {
    let vk = VerifyingKey::from_bytes(ed25519).map_err(|_| Error::BadSignature)?;
    vk.verify_strict(&auth_msg(nonce), &Signature::from_bytes(sig))
        .map_err(|_| Error::BadSignature)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCard {
    pub ed25519: [u8; 32],
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
        let vk =
            VerifyingKey::from_bytes(&ed25519).map_err(|_| Error::BadCard("bad signing key"))?;
        vk.verify_strict(&card_msg(&x25519), &Signature::from_bytes(&sig))
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

fn card_msg(x25519: &[u8; 32]) -> Vec<u8> {
    [CARD_CTX, x25519].concat()
}

fn auth_msg(nonce: &[u8]) -> Vec<u8> {
    [AUTH_CTX, nonce].concat()
}

fn x25519_public(secret: &[u8; 32]) -> Result<[u8; 32]> {
    // Reuse snow's DH implementation rather than adding a second X25519 crate.
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    let params: snow::params::NoiseParams = NOISE_PARAMS.parse()?;
    let mut dh = DefaultResolver
        .resolve_dh(&params.dh)
        .ok_or(Error::Malformed)?;
    dh.set(secret);
    dh.pubkey().try_into().map_err(|_| Error::Malformed)
}
