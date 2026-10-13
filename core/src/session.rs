//! Sealed one-shot messages using Noise_X (via the `snow` crate).
//!
//! Properties: confidential to the recipient's static key, authenticated as
//! coming from the sender's static key, deliverable while the recipient is
//! offline. Limitations (see docs/adr/0001): no forward secrecy against later
//! compromise of the recipient's static key, and no replay protection in the
//! transport; callers must de-duplicate on [`Opened::id`].

use crate::identity::{DeviceCard, Identity};
use crate::{Error, Result, NOISE_PARAMS};
use rand::{rngs::OsRng, RngCore};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_BODY: usize = 60_000;
const HEADER: usize = 16 + 8;

#[derive(Debug)]
pub struct Opened {
    /// Authenticated X25519 static key of the sender. Map it to a contact.
    pub sender_x25519: [u8; 32],
    /// Random per-message id, for de-duplication by the receiver.
    pub id: [u8; 16],
    /// Sender's clock, seconds since the epoch. Informational only.
    pub ts: u64,
    pub body: Vec<u8>,
}

pub fn seal(me: &Identity, peer: &DeviceCard, body: &[u8]) -> Result<Vec<u8>> {
    if body.len() > MAX_BODY {
        return Err(Error::TooLarge { max: MAX_BODY });
    }
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut payload = Vec::with_capacity(HEADER + body.len());
    payload.extend_from_slice(&id);
    payload.extend_from_slice(&ts.to_be_bytes());
    payload.extend_from_slice(body);

    let mut hs = snow::Builder::new(NOISE_PARAMS.parse()?)
        .local_private_key(me.x_secret())
        .remote_public_key(&peer.x25519)
        .build_initiator()?;
    // e (32) + encrypted s (32 + 16) + encrypted payload (+16 tag)
    let mut out = vec![0u8; payload.len() + 96];
    let n = hs.write_message(&payload, &mut out)?;
    out.truncate(n);
    Ok(out)
}

pub fn open(me: &Identity, sealed: &[u8]) -> Result<Opened> {
    let mut hs = snow::Builder::new(NOISE_PARAMS.parse()?)
        .local_private_key(me.x_secret())
        .build_responder()?;
    let mut buf = vec![0u8; sealed.len()];
    let n = hs.read_message(sealed, &mut buf)?;
    let sender: [u8; 32] = hs
        .get_remote_static()
        .and_then(|k| k.try_into().ok())
        .ok_or(Error::Malformed)?;
    if n < HEADER {
        return Err(Error::Malformed);
    }
    Ok(Opened {
        sender_x25519: sender,
        id: buf[..16].try_into().unwrap(),
        ts: u64::from_be_bytes(buf[16..24].try_into().unwrap()),
        body: buf[HEADER..n].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_authenticates_sender() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let sealed = seal(&a, &b.card(), b"hello bob").unwrap();
        let o = open(&b, &sealed).unwrap();
        assert_eq!(o.body, b"hello bob");
        assert_eq!(o.sender_x25519, a.x_public());
    }

    #[test]
    fn plaintext_not_in_ciphertext() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let secret = b"very-recognisable-plaintext";
        let sealed = seal(&a, &b.card(), secret).unwrap();
        assert!(!sealed.windows(secret.len()).any(|w| w == secret));
    }

    #[test]
    fn third_party_cannot_open() {
        let (a, b, eve) = (
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
        );
        let sealed = seal(&a, &b.card(), b"x").unwrap();
        assert!(open(&eve, &sealed).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let sealed = seal(&a, &b.card(), b"x").unwrap();
        for i in [0, 40, sealed.len() - 1] {
            let mut t = sealed.clone();
            t[i] ^= 1;
            assert!(open(&b, &t).is_err(), "flip at {i} accepted");
        }
    }

    #[test]
    fn oversize_rejected() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        assert!(matches!(
            seal(&a, &b.card(), &vec![0; MAX_BODY + 1]),
            Err(Error::TooLarge { .. })
        ));
        assert!(seal(&a, &b.card(), &vec![0; MAX_BODY]).is_ok());
    }

    #[test]
    fn card_roundtrip_and_binding() {
        let a = Identity::generate().unwrap();
        let c = a.card();
        assert_eq!(DeviceCard::decode(&c.encode()).unwrap(), c);
        // Swapping in a different X25519 key must break the signature.
        let other = Identity::generate().unwrap().card();
        let mut raw = hex::decode(&c.encode()["spike1:".len()..]).unwrap();
        raw[32..64].copy_from_slice(&other.x25519);
        assert!(DeviceCard::decode(&format!("spike1:{}", hex::encode(raw))).is_err());
    }

    #[test]
    fn identity_persists() {
        let a = Identity::generate().unwrap();
        let b = Identity::from_json(&a.to_json().unwrap()).unwrap();
        assert_eq!(a.card(), b.card());
    }
}
