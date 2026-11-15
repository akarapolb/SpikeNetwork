//! One-time invite links (ADR 0039): the link format and the sealed payload the relay holds.
//!
//! The link carries a random invite id (what the relay stores the blob under), a random link key
//! (never sent to the relay) and the relay's address. The blob is the inviter's card, the name
//! they offer, a one-time token and an expiry, sealed with XChaCha20-Poly1305 under the link key.

use crate::{Error, Result};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "spikeinvite1:";
const AD: &[u8] = b"spike-invite-v1";
/// How long an invite is valid.
pub const TTL_MS: u64 = 7 * 24 * 3600 * 1000;

/// What the link says: where the sealed invite is and the key that opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteLink {
    pub id: [u8; 16],
    pub key: [u8; 32],
    pub relay: Option<String>,
}

impl InviteLink {
    pub fn generate(relay: Option<String>) -> Self {
        let mut id = [0u8; 16];
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut id);
        OsRng.fill_bytes(&mut key);
        Self { id, key, relay }
    }

    /// The relay's name for this invite: 32 lowercase hex characters.
    pub fn id_hex(&self) -> String {
        hex::encode(self.id)
    }

    /// `spikeinvite1:<base64url(id ‖ key)>[@relay]`.
    pub fn encode(&self) -> String {
        let mut b = self.id.to_vec();
        b.extend_from_slice(&self.key);
        let mut s = format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
        );
        if let Some(r) = &self.relay {
            s.push('@');
            s.push_str(r);
        }
        s
    }

    /// Read a link, forgiving the spaces and line breaks a chat app adds.
    pub fn decode(s: &str) -> Result<Self> {
        let s: String = s.split_whitespace().collect();
        let rest = s.strip_prefix(PREFIX).ok_or(Error::Malformed)?;
        let (code, relay) = match rest.split_once('@') {
            Some((c, r)) => (c, Some(r)),
            None => (rest, None),
        };
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(code)
            .map_err(|_| Error::Malformed)?;
        if b.len() != 48 {
            return Err(Error::Malformed);
        }
        let relay = match relay {
            Some(r) if r.starts_with("ws://") || r.starts_with("wss://") => Some(r.to_string()),
            Some(_) => return Err(Error::Malformed),
            None => None,
        };
        Ok(Self {
            id: b[..16].try_into().expect("16 bytes"),
            key: b[16..].try_into().expect("32 bytes"),
            relay,
        })
    }

    fn aad(&self) -> Vec<u8> {
        let mut a = AD.to_vec();
        a.extend_from_slice(&self.id);
        a
    }

    /// Seal what the relay will hold.
    pub fn seal(&self, p: &InvitePayload) -> Vec<u8> {
        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let ct = XChaCha20Poly1305::new((&self.key).into())
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &serde_json::to_vec(p).expect("serializes"),
                    aad: &self.aad(),
                },
            )
            .expect("encryption cannot fail");
        let mut out = nonce.to_vec();
        out.extend(ct);
        out
    }

    /// Open what the relay handed out. Fails if it was changed, or sealed for another link.
    pub fn open(&self, blob: &[u8]) -> Result<InvitePayload> {
        if blob.len() < 24 + 16 {
            return Err(Error::Corrupted);
        }
        let plain = XChaCha20Poly1305::new((&self.key).into())
            .decrypt(
                &XNonce::try_from(&blob[..24]).map_err(|_| Error::Corrupted)?,
                Payload {
                    msg: &blob[24..],
                    aad: &self.aad(),
                },
            )
            .map_err(|_| Error::Corrupted)?;
        serde_json::from_slice(&plain).map_err(|_| Error::Corrupted)
    }
}

/// What an invite holds: who made it, the name they offer, the token that proves the opener
/// has the link, and when it stops being valid (ms since the epoch).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InvitePayload {
    pub card: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub token: String,
    pub expires: u64,
}

/// A fresh one-time token, 32 lowercase hex characters.
pub fn new_token() -> String {
    let mut t = [0u8; 16];
    OsRng.fill_bytes(&mut t);
    hex::encode(t)
}

/// The body of an `InviteHello` envelope: the token from the invite.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub token: String,
}

impl Hello {
    pub fn valid(&self) -> bool {
        self.token.len() == 32
            && self
                .token
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> InvitePayload {
        InvitePayload {
            card: "spike2:card".into(),
            name: Some("Alice".into()),
            token: new_token(),
            expires: 123,
        }
    }

    #[test]
    fn a_link_round_trips_and_opens_its_own_invite_only() {
        let l = InviteLink::generate(Some("wss://relay.example".into()));
        let code = l.encode();
        assert!(code.starts_with(PREFIX) && code.ends_with("@wss://relay.example"));
        let back = InviteLink::decode(&format!("  {}\n", code)).unwrap();
        assert_eq!(back, l);
        let blob = l.seal(&payload());
        assert_eq!(back.open(&blob).unwrap().name.as_deref(), Some("Alice"));
        let other = InviteLink::generate(None);
        assert!(
            other.open(&blob).is_err(),
            "another link's key does not open it"
        );
        let mut moved = l.clone();
        moved.id[0] ^= 1;
        assert!(moved.open(&blob).is_err(), "the blob is bound to its id");
        let mut bad = blob.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(l.open(&bad).is_err());
    }

    #[test]
    fn malformed_links_are_refused() {
        assert!(InviteLink::decode("spike2:abc").is_err());
        assert!(InviteLink::decode("spikeinvite1:AAAA").is_err());
        let l = InviteLink::generate(None).encode();
        assert!(InviteLink::decode(&format!("{l}@http://x")).is_err());
        assert!(InviteLink::decode(&l).unwrap().relay.is_none());
    }

    #[test]
    fn hello_tokens_are_checked() {
        assert!(Hello { token: new_token() }.valid());
        assert!(!Hello { token: "zz".into() }.valid());
    }
}
