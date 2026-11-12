//! Device linking (multi-device milestone M4): the cryptography and framing.
//!
//! 1. The NEW device shows a link code (the QR code's contents, never sent
//!    through the relay): its device card, a fresh ephemeral X25519 public
//!    key, a random 16-byte secret and, optionally, its relay.
//! 2. The EXISTING device makes its own ephemeral key and sends a plain
//!    `hello` frame (its ephemeral public key) to the new device via the relay.
//! 3. Both derive, with HKDF-SHA256 over X25519(eph, eph), salted with the
//!    secret and bound to the card and both ephemeral keys, a 32-byte frame key
//!    and an 8-digit safety code. People compare the code on both screens: if
//!    anyone substituted a key, the codes differ.
//! 4. Only after confirmation does the existing device send the bundle
//!    (identity key, contacts, history) in AEAD-sealed, numbered chunks.

use crate::identity::DeviceCard;
use crate::{Error, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
use vodozemac::{Curve25519PublicKey, Curve25519SecretKey};

const CODE_PREFIX: &str = "spikelink1:";
/// The compact code: the same four fields as raw bytes in base64url, about 40% of the length of
/// version 1, short enough for a QR code a phone can scan. Version 1 is still accepted.
const CODE_PREFIX_V2: &str = "spikelink2:";
const MAGIC: &[u8; 4] = b"SPLK";
const HELLO: u8 = 1;
const DATA: u8 = 2;
const INFO: &[u8] = b"spike-link-v1";
/// Plaintext bytes per data frame; keeps each frame well under the relay's 64 KiB blob limit.
pub const CHUNK: usize = 40_000;
/// Refuse absurd bundles (about 400 MB) before allocating anything.
const MAX_FRAMES: u32 = 10_000;

/// What the new device shows as its QR code.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LinkOffer {
    pub card: String,
    pub eph: String,
    pub secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
}

impl LinkOffer {
    /// The compact (version 2) code, or the long version 1 form if a field is not well formed
    /// (which `decode` then refuses).
    pub fn encode(&self) -> String {
        self.encode_compact().unwrap_or_else(|| self.encode_v1())
    }

    pub fn encode_v1(&self) -> String {
        format!(
            "{CODE_PREFIX}{}",
            hex::encode(serde_json::to_vec(self).expect("serializes"))
        )
    }

    /// card (128) | ephemeral key (32) | secret (16) | relay URL (rest), base64url without padding.
    fn encode_compact(&self) -> Option<String> {
        use base64::Engine;
        let card = hex::decode(self.card.strip_prefix(crate::identity::CARD_PREFIX)?).ok()?;
        let eph = hex::decode(&self.eph).ok()?;
        let secret = hex::decode(&self.secret).ok()?;
        if card.len() != 128 || eph.len() != 32 || secret.len() != 16 {
            return None;
        }
        let mut b = [card, eph, secret].concat();
        if let Some(r) = &self.relay {
            b.extend_from_slice(r.as_bytes());
        }
        Some(format!(
            "{CODE_PREFIX_V2}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
        ))
    }

    fn decode_compact(body: &str) -> Result<Self> {
        use base64::Engine;
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| Error::BadCard("not a link code"))?;
        if b.len() < 176 {
            return Err(Error::BadCard("not a link code"));
        }
        let relay = match &b[176..] {
            [] => None,
            r => {
                Some(String::from_utf8(r.to_vec()).map_err(|_| Error::BadCard("not a link code"))?)
            }
        };
        Ok(Self {
            card: format!("{}{}", crate::identity::CARD_PREFIX, hex::encode(&b[..128])),
            eph: hex::encode(&b[128..160]),
            secret: hex::encode(&b[160..176]),
            relay,
        })
    }

    pub fn decode(s: &str) -> Result<Self> {
        let s = s.trim();
        let o = if let Some(body) = s.strip_prefix(CODE_PREFIX_V2) {
            Self::decode_compact(body)?
        } else {
            let body = s
                .strip_prefix(CODE_PREFIX)
                .ok_or(Error::BadCard("not a link code"))?;
            serde_json::from_slice(&hex::decode(body)?)
                .map_err(|_| Error::BadCard("not a link code"))?
        };
        DeviceCard::decode(&o.card)?;
        let eph: [u8; 32] = hex::decode(&o.eph)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        Curve25519PublicKey::from_slice(&eph).map_err(|_| Error::Malformed)?;
        if hex::decode(&o.secret)?.len() != 16 {
            return Err(Error::Malformed);
        }
        Ok(o)
    }

    pub fn device_card(&self) -> Result<DeviceCard> {
        DeviceCard::decode(&self.card)
    }
}

/// Held in memory by the new device while it waits. Never persisted.
pub struct NewDevice {
    eph: Curve25519SecretKey,
    pub offer: LinkOffer,
}

impl NewDevice {
    pub fn new(card: &DeviceCard, relay: Option<String>) -> Self {
        let eph = Curve25519SecretKey::new();
        let mut secret = [0u8; 16];
        OsRng.fill_bytes(&mut secret);
        let offer = LinkOffer {
            card: card.encode(),
            eph: hex::encode(Curve25519PublicKey::from(&eph).to_bytes()),
            secret: hex::encode(secret),
            relay,
        };
        Self { eph, offer }
    }

    /// Accept the existing device's hello frame and derive the session.
    pub fn on_hello(&self, frame: &[u8]) -> Result<LinkSession> {
        if frame.len() != 4 + 1 + 32 || &frame[..4] != MAGIC || frame[4] != HELLO {
            return Err(Error::Malformed);
        }
        let theirs: [u8; 32] = frame[5..].try_into().unwrap();
        let pk = Curve25519PublicKey::from_slice(&theirs).map_err(|_| Error::Malformed)?;
        let shared = self.eph.diffie_hellman(&pk).ok_or(Error::Malformed)?;
        LinkSession::derive(&self.offer, shared.as_bytes(), &theirs)
    }
}

/// Is this relay delivery a linking frame (rather than a normal message)?
pub fn is_link_frame(blob: &[u8]) -> bool {
    blob.len() > 4 && &blob[..4] == MAGIC
}

/// The shared state after the hello: frame key and the code people compare.
pub struct LinkSession {
    key: [u8; 32],
    /// Eight digits, shown as "1234 5678" on both screens.
    pub safety_code: String,
}

impl LinkSession {
    /// The existing device's side: make an ephemeral key, derive the session,
    /// and return the hello frame to send to the new device.
    pub fn approve(offer: &LinkOffer) -> Result<(Self, Vec<u8>)> {
        let eph = Curve25519SecretKey::new();
        let mine = Curve25519PublicKey::from(&eph).to_bytes();
        let theirs: [u8; 32] = hex::decode(&offer.eph)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let pk = Curve25519PublicKey::from_slice(&theirs).map_err(|_| Error::Malformed)?;
        let shared = eph.diffie_hellman(&pk).ok_or(Error::Malformed)?;
        let session = Self::derive(offer, shared.as_bytes(), &mine)?;
        let hello = [&MAGIC[..], &[HELLO], &mine].concat();
        Ok((session, hello))
    }

    fn derive(offer: &LinkOffer, shared: &[u8; 32], existing_eph: &[u8; 32]) -> Result<Self> {
        let salt = hex::decode(&offer.secret)?;
        let info = [
            INFO,
            offer.card.as_bytes(),
            offer.eph.as_bytes(),
            existing_eph,
        ]
        .concat();
        let mut okm = [0u8; 40];
        Hkdf::<Sha256>::new(Some(&salt), shared)
            .expand(&info, &mut okm)
            .map_err(|_| Error::Malformed)?;
        let n = u64::from_be_bytes(okm[32..].try_into().unwrap()) % 100_000_000;
        let digits = format!("{n:08}");
        Ok(Self {
            key: okm[..32].try_into().unwrap(),
            safety_code: format!("{} {}", &digits[..4], &digits[4..]),
        })
    }

    /// Split `payload` into sealed, numbered frames.
    pub fn seal(&self, payload: &[u8]) -> Result<Vec<Vec<u8>>> {
        let chunks: Vec<&[u8]> = if payload.is_empty() {
            vec![&[][..]]
        } else {
            payload.chunks(CHUNK).collect()
        };
        let total = u32::try_from(chunks.len()).map_err(|_| Error::TooLarge {
            max: CHUNK * MAX_FRAMES as usize,
        })?;
        if total > MAX_FRAMES {
            return Err(Error::TooLarge {
                max: CHUNK * MAX_FRAMES as usize,
            });
        }
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let mut out = Vec::with_capacity(chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            let header = [
                &MAGIC[..],
                &[DATA],
                &(i as u32).to_be_bytes(),
                &total.to_be_bytes(),
            ]
            .concat();
            let mut nonce = [0u8; 24];
            OsRng.fill_bytes(&mut nonce);
            let ct = cipher
                .encrypt(
                    &XNonce::from(nonce),
                    Payload {
                        msg: c,
                        aad: &header,
                    },
                )
                .map_err(|_| Error::Malformed)?;
            out.push([header, nonce.to_vec(), ct].concat());
        }
        Ok(out)
    }
}

/// Collects data frames on the new device until the whole payload is present.
pub struct Assembler<'s> {
    session: &'s LinkSession,
    total: Option<u32>,
    parts: BTreeMap<u32, Vec<u8>>,
}

impl<'s> Assembler<'s> {
    pub fn new(session: &'s LinkSession) -> Self {
        Self {
            session,
            total: None,
            parts: BTreeMap::new(),
        }
    }

    /// Add one frame. Frames that fail authentication, disagree on the total,
    /// or are hello frames are rejected; duplicates are ignored.
    pub fn add(&mut self, frame: &[u8]) -> Result<()> {
        if frame.len() < 13 + 24 + 16 || &frame[..4] != MAGIC || frame[4] != DATA {
            return Err(Error::Malformed);
        }
        let seq = u32::from_be_bytes(frame[5..9].try_into().unwrap());
        let total = u32::from_be_bytes(frame[9..13].try_into().unwrap());
        let plain = XChaCha20Poly1305::new((&self.session.key).into())
            .decrypt(
                &XNonce::try_from(&frame[13..37]).map_err(|_| Error::Malformed)?,
                Payload {
                    msg: &frame[37..],
                    aad: &frame[..13],
                },
            )
            .map_err(|_| Error::Corrupted)?;
        if total == 0
            || total > MAX_FRAMES
            || seq >= total
            || self.total.is_some_and(|t| t != total)
        {
            return Err(Error::Malformed);
        }
        self.total = Some(total);
        self.parts.entry(seq).or_insert(plain);
        Ok(())
    }

    /// The whole payload, once every frame has arrived.
    pub fn finish(&self) -> Option<Vec<u8>> {
        let total = self.total?;
        if self.parts.len() as u32 != total {
            return None;
        }
        Some(self.parts.values().flatten().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn setup() -> (NewDevice, LinkSession, LinkSession) {
        let n = NewDevice::new(
            &Identity::generate().unwrap().card(),
            Some("wss://r.example".into()),
        );
        let offer = LinkOffer::decode(&n.offer.encode()).unwrap();
        let (e_side, hello) = LinkSession::approve(&offer).unwrap();
        let n_side = n.on_hello(&hello).unwrap();
        (n, e_side, n_side)
    }

    #[test]
    fn both_sides_derive_the_same_code_and_key() {
        let (_, e, n) = setup();
        assert_eq!(e.safety_code, n.safety_code);
        assert_eq!(e.safety_code.len(), 9);
        let payload: Vec<u8> = (0..CHUNK * 2 + 123).map(|i| i as u8).collect();
        let mut a = Assembler::new(&n);
        let frames = e.seal(&payload).unwrap();
        assert_eq!(frames.len(), 3);
        for f in frames.iter().rev() {
            assert!(a.finish().is_none());
            a.add(f).unwrap();
        }
        a.add(&frames[0]).unwrap(); // duplicate ignored
        assert_eq!(a.finish().unwrap(), payload);
    }

    #[test]
    fn substituted_hello_gives_a_different_code() {
        // A relay replaces the existing device's hello with its own.
        let n = NewDevice::new(&Identity::generate().unwrap().card(), None);
        let offer = LinkOffer::decode(&n.offer.encode()).unwrap();
        let (e_side, _real_hello) = LinkSession::approve(&offer).unwrap();
        let (_, attacker_hello) = LinkSession::approve(&offer).unwrap();
        let n_side = n.on_hello(&attacker_hello).unwrap();
        assert_ne!(
            e_side.safety_code, n_side.safety_code,
            "people would see different codes and stop"
        );
        // And the real device's frames don't open under the attacker's session.
        let mut a = Assembler::new(&n_side);
        assert!(a.add(&e_side.seal(b"identity").unwrap()[0]).is_err());
    }

    #[test]
    fn tampered_reordered_or_mismatched_frames_are_rejected() {
        let (_, e, n) = setup();
        let frames = e.seal(&vec![7u8; CHUNK + 1]).unwrap();
        let mut a = Assembler::new(&n);
        let mut bad = frames[0].clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(a.add(&bad).is_err());
        let mut renumbered = frames[1].clone();
        renumbered[5..9].copy_from_slice(&0u32.to_be_bytes()); // header is authenticated
        assert!(a.add(&renumbered).is_err());
        let mut short_total = frames[0].clone();
        short_total[9..13].copy_from_slice(&1u32.to_be_bytes()); // can't truncate the bundle
        assert!(a.add(&short_total).is_err());
        // Frames from a different link session don't open.
        let (_, e2, _) = setup();
        assert!(a.add(&e2.seal(b"x").unwrap()[0]).is_err());
        assert!(a.finish().is_none());
    }

    #[test]
    fn link_codes_validate() {
        let n = NewDevice::new(&Identity::generate().unwrap().card(), None);
        let code = n.offer.encode();
        assert_eq!(LinkOffer::decode(&code).unwrap(), n.offer);
        assert!(LinkOffer::decode("spike2:abcd").is_err());
        let mut bad = n.offer.clone();
        bad.secret = "00".into();
        assert!(LinkOffer::decode(&bad.encode()).is_err());
        assert!(!is_link_frame(b"SPL"));
    }

    #[test]
    fn the_compact_code_is_short_and_the_long_one_still_works() {
        let n = NewDevice::new(
            &Identity::generate().unwrap().card(),
            Some("wss://relay.example.org".into()),
        );
        let (short, long) = (n.offer.encode(), n.offer.encode_v1());
        assert!(short.starts_with("spikelink2:") && long.starts_with("spikelink1:"));
        assert!(
            short.len() < 400 && long.len() > 800,
            "{} {}",
            short.len(),
            long.len()
        );
        assert_eq!(LinkOffer::decode(&short).unwrap(), n.offer);
        assert_eq!(LinkOffer::decode(&long).unwrap(), n.offer);
        // Damage is caught: a flipped character breaks the card signature or the field sizes.
        let mut chars: Vec<char> = short.chars().collect();
        chars[20] = if chars[20] == 'A' { 'B' } else { 'A' };
        assert!(LinkOffer::decode(&chars.iter().collect::<String>()).is_err());
        assert!(LinkOffer::decode("spikelink2:AAAA").is_err());
    }
}
