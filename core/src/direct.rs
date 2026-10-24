//! Direct device-to-device connections (ADR 0027): the handshake and the frame cipher, with no I/O.
//!
//! Two devices that can reach each other authenticate with their device (Ed25519) keys, agree fresh
//! keys with X25519 ephemerals, and then seal every frame with XChaCha20-Poly1305. Nothing is
//! trusted because of the network address it came from. The caller moves the bytes and decides who
//! is allowed to connect; this module only says who the other side proved to be.
//!
//! ```text
//! I -> R   version | E_i
//! R -> I   E_r | seal(hs_r, R_ed | sig_R)          sig_R over H("R", E_i, E_r, R_ed)
//! I -> R   seal(hs_i, I_ed | port | sig_I)          sig_I over H("I", E_i, E_r, R_ed, I_ed, port, sig_R)
//! ```

use crate::identity::{device_id_from_ed, verify_direct, Identity};
use crate::{Error, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use vodozemac::{Curve25519PublicKey, Curve25519SecretKey};

pub const VERSION: u8 = 1;
const CTX: &[u8] = b"spike-direct-v1";
/// Largest sealed frame accepted (the blob a relay would carry, plus a tag and a few bytes).
pub const MAX_FRAME: usize = 256 * 1024;
const TAG: usize = 16;

fn bad<T>() -> Result<T> {
    Err(Error::Session("direct handshake failed".into()))
}

fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(CTX);
    for p in parts {
        h.update((p.len() as u32).to_be_bytes());
        h.update(p);
    }
    h.finalize().into()
}

fn expand(hk: &Hkdf<Sha256>, info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hk.expand(info, &mut out)
        .expect("32 bytes is a valid length");
    out
}

fn nonce(n: u64) -> XNonce {
    let mut b = [0u8; 24];
    b[16..].copy_from_slice(&n.to_be_bytes());
    *XNonce::from_slice(&b)
}

fn seal_once(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    XChaCha20Poly1305::new(key.into())
        .encrypt(&nonce(0), plain)
        .expect("encryption of a small buffer cannot fail")
}

fn open_once(key: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>> {
    XChaCha20Poly1305::new(key.into())
        .decrypt(&nonce(0), sealed)
        .map_err(|_| Error::Session("direct handshake failed".into()))
}

fn dh(secret: &Curve25519SecretKey, public: &[u8; 32]) -> Result<[u8; 32]> {
    secret
        .diffie_hellman(&Curve25519PublicKey::from_bytes(*public))
        .map(|s| *s.as_bytes())
        .ok_or_else(|| Error::Session("direct handshake failed".into()))
}

/// Keys that only the two ephemerals determine; they protect the identities in messages 2 and 3.
struct Handshake {
    hs_r: [u8; 32],
    hs_i: [u8; 32],
    t1: [u8; 32],
    shared: [u8; 32],
}

fn handshake_keys(shared: [u8; 32], e_i: &[u8; 32], e_r: &[u8; 32]) -> Handshake {
    let t1 = hash(&[b"t1", e_i, e_r]);
    let hk = Hkdf::<Sha256>::new(Some(&t1), &shared);
    Handshake {
        hs_r: expand(&hk, b"hs-r"),
        hs_i: expand(&hk, b"hs-i"),
        t1,
        shared,
    }
}

/// The two directions of an established connection.
pub struct Channel {
    send: XChaCha20Poly1305,
    recv: XChaCha20Poly1305,
    send_n: u64,
    recv_n: u64,
}

impl Channel {
    fn new(shared: &[u8; 32], transcript: &[u8; 32], initiator: bool) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(transcript), shared);
        let i2r = expand(&hk, b"app-i2r");
        let r2i = expand(&hk, b"app-r2i");
        let (s, r) = if initiator { (i2r, r2i) } else { (r2i, i2r) };
        Self {
            send: XChaCha20Poly1305::new((&s).into()),
            recv: XChaCha20Poly1305::new((&r).into()),
            send_n: 0,
            recv_n: 0,
        }
    }

    /// Seal the next frame. Frames must be opened in the order they were sealed.
    pub fn seal(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        if plain.len() + TAG > MAX_FRAME {
            return Err(Error::TooLarge {
                max: MAX_FRAME - TAG,
            });
        }
        let ct = self
            .send
            .encrypt(&nonce(self.send_n), plain)
            .map_err(|_| Error::Session("direct frame could not be sealed".into()))?;
        self.send_n += 1;
        Ok(ct)
    }

    /// Open the next frame. A replayed, reordered, dropped or altered frame fails.
    pub fn open(&mut self, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() > MAX_FRAME {
            return Err(Error::Malformed);
        }
        let plain = self
            .recv
            .decrypt(&nonce(self.recv_n), sealed)
            .map_err(|_| Error::Session("direct frame failed its integrity check".into()))?;
        self.recv_n += 1;
        Ok(plain)
    }
}

/// What the other side proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub ed25519: [u8; 32],
    pub device_id: String,
    /// The port the initiator says it listens on (0: it does not). Authenticated, but only a hint.
    pub port: u16,
}

// ------------------------------------------------------------------ initiator

pub struct Initiator {
    secret: Curve25519SecretKey,
    e_i: [u8; 32],
}

/// Start a connection: the first message to send.
pub fn initiate() -> (Initiator, Vec<u8>) {
    let secret = Curve25519SecretKey::new();
    let e_i = Curve25519PublicKey::from(&secret).to_bytes();
    let mut m = vec![VERSION];
    m.extend_from_slice(&e_i);
    (Initiator { secret, e_i }, m)
}

impl Initiator {
    /// Read the responder's answer. It must be the device `expect_device_id`; anything else is
    /// refused before we reveal who we are. Returns the third message and the open channel.
    pub fn finish(
        self,
        me: &Identity,
        listen_port: u16,
        expect_device_id: &str,
        msg2: &[u8],
    ) -> Result<(Vec<u8>, Channel)> {
        if msg2.len() != 32 + 32 + 64 + TAG {
            return bad();
        }
        let e_r: [u8; 32] = msg2[..32].try_into().unwrap();
        let shared = dh(&self.secret, &e_r)?;
        let hs = handshake_keys(shared, &self.e_i, &e_r);
        let plain = open_once(&hs.hs_r, &msg2[32..])?;
        let r_ed: [u8; 32] = plain[..32].try_into().unwrap();
        let sig_r: [u8; 64] = plain[32..].try_into().unwrap();
        if device_id_from_ed(&r_ed) != expect_device_id {
            return bad();
        }
        verify_direct(&r_ed, &hash(&[b"R", &hs.t1, &r_ed]), &sig_r)?;
        let port = listen_port.to_be_bytes();
        let i_ed = me.ed_public();
        let sig_i = me.sign_direct(&hash(&[b"I", &hs.t1, &r_ed, &i_ed, &port, &sig_r]));
        let mut body = Vec::with_capacity(32 + 2 + 64);
        body.extend_from_slice(&i_ed);
        body.extend_from_slice(&port);
        body.extend_from_slice(&sig_i);
        let transcript = hash(&[b"t3", &hs.t1, &r_ed, &i_ed, &port, &sig_r, &sig_i]);
        Ok((
            seal_once(&hs.hs_i, &body),
            Channel::new(&hs.shared, &transcript, true),
        ))
    }
}

// ------------------------------------------------------------------ responder

pub struct Responder {
    hs: Handshake,
    r_ed: [u8; 32],
    sig_r: [u8; 64],
}

/// Answer a first message: the second message to send, and the state that reads the third.
pub fn respond(me: &Identity, msg1: &[u8]) -> Result<(Responder, Vec<u8>)> {
    if msg1.len() != 33 || msg1[0] != VERSION {
        return bad();
    }
    let e_i: [u8; 32] = msg1[1..].try_into().unwrap();
    let secret = Curve25519SecretKey::new();
    let e_r = Curve25519PublicKey::from(&secret).to_bytes();
    let shared = dh(&secret, &e_i)?;
    let hs = handshake_keys(shared, &e_i, &e_r);
    let r_ed = me.ed_public();
    let sig_r = me.sign_direct(&hash(&[b"R", &hs.t1, &r_ed]));
    let mut body = Vec::with_capacity(96);
    body.extend_from_slice(&r_ed);
    body.extend_from_slice(&sig_r);
    let mut m = e_r.to_vec();
    m.extend_from_slice(&seal_once(&hs.hs_r, &body));
    Ok((Responder { hs, r_ed, sig_r }, m))
}

impl Responder {
    /// Read the initiator's proof. The caller must still decide whether `Peer` is allowed to talk to us.
    pub fn finish(self, msg3: &[u8]) -> Result<(Peer, Channel)> {
        if msg3.len() != 32 + 2 + 64 + TAG {
            return bad();
        }
        let plain = open_once(&self.hs.hs_i, msg3)?;
        let i_ed: [u8; 32] = plain[..32].try_into().unwrap();
        let port: [u8; 2] = plain[32..34].try_into().unwrap();
        let sig_i: [u8; 64] = plain[34..].try_into().unwrap();
        verify_direct(
            &i_ed,
            &hash(&[b"I", &self.hs.t1, &self.r_ed, &i_ed, &port, &self.sig_r]),
            &sig_i,
        )?;
        let transcript = hash(&[
            b"t3",
            &self.hs.t1,
            &self.r_ed,
            &i_ed,
            &port,
            &self.sig_r,
            &sig_i,
        ]);
        Ok((
            Peer {
                ed25519: i_ed,
                device_id: device_id_from_ed(&i_ed),
                port: u16::from_be_bytes(port),
            },
            Channel::new(&self.hs.shared, &transcript, false),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect(a: &Identity, b: &Identity, port: u16) -> (Channel, Channel, Peer) {
        let (init, m1) = initiate();
        let (resp, m2) = respond(b, &m1).unwrap();
        let (m3, ca) = init.finish(a, port, &b.device_id(), &m2).unwrap();
        let (peer, cb) = resp.finish(&m3).unwrap();
        (ca, cb, peer)
    }

    #[test]
    fn both_sides_learn_who_the_other_is_and_can_talk() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (mut ca, mut cb, peer) = connect(&a, &b, 4242);
        assert_eq!(peer.device_id, a.device_id());
        assert_eq!(peer.port, 4242);
        let f = ca.seal(b"hello").unwrap();
        assert_eq!(cb.open(&f).unwrap(), b"hello");
        let g = cb.seal(b"back").unwrap();
        assert_eq!(ca.open(&g).unwrap(), b"back");
        // Many frames in a row keep working.
        for i in 0..20u8 {
            let f = ca.seal(&[i; 50]).unwrap();
            assert_eq!(cb.open(&f).unwrap(), vec![i; 50]);
        }
    }

    #[test]
    fn the_initiator_refuses_a_responder_that_is_not_who_it_dialled() {
        let (a, b, evil) = (
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
        );
        let (init, m1) = initiate();
        // Someone else answers at b's address.
        let (_resp, m2) = respond(&evil, &m1).unwrap();
        assert!(init.finish(&a, 0, &b.device_id(), &m2).is_err());
    }

    #[test]
    fn a_man_in_the_middle_cannot_splice_two_handshakes() {
        let (a, b, m) = (
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
        );
        // The attacker answers a's hello with its own identity but claims b's: signatures cannot match.
        let (init, m1) = initiate();
        let (_r, m2) = respond(&m, &m1).unwrap();
        assert!(init.finish(&a, 0, &b.device_id(), &m2).is_err());
        // Altering any byte of message 2 or 3 fails.
        let (init, m1) = initiate();
        let (resp, m2) = respond(&b, &m1).unwrap();
        for i in [0, 40, m2.len() - 1] {
            let mut x = m2.clone();
            x[i] ^= 1;
            assert!(clone_init(&init).finish(&a, 0, &b.device_id(), &x).is_err());
        }
        let (m3, _) = init.finish(&a, 0, &b.device_id(), &m2).unwrap();
        let mut y = m3.clone();
        y[3] ^= 1;
        assert!(resp.finish(&y).is_err());
    }

    fn clone_init(i: &Initiator) -> Initiator {
        Initiator {
            secret: i.secret.clone(),
            e_i: i.e_i,
        }
    }

    #[test]
    fn a_signature_made_for_a_relay_cannot_stand_in_for_a_direct_one() {
        let a = Identity::generate().unwrap();
        let nonce = [7u8; 32];
        let relay_sig = a.sign_auth(&nonce, "relay.example");
        assert!(verify_direct(&a.ed_public(), &nonce, &relay_sig).is_err());
        assert!(verify_direct(&a.ed_public(), &nonce, &a.sign_direct(&nonce)).is_ok());
    }

    #[test]
    fn frames_are_bound_to_their_order_and_direction() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (mut ca, mut cb, _) = connect(&a, &b, 0);
        let f1 = ca.seal(b"one").unwrap();
        let f2 = ca.seal(b"two").unwrap();
        // Out of order fails, and a failure does not advance the counter.
        assert!(cb.open(&f2).is_err());
        assert_eq!(cb.open(&f1).unwrap(), b"one");
        assert_eq!(cb.open(&f2).unwrap(), b"two");
        // Replay fails.
        assert!(cb.open(&f1).is_err());
        // A frame cannot be reflected back at its sender.
        let f3 = ca.seal(b"three").unwrap();
        assert!(ca.open(&f3).is_err());
        // Tampering fails.
        let mut f4 = ca.seal(b"four").unwrap();
        f4[0] ^= 1;
        assert!(cb.open(&f4).is_err());
    }

    #[test]
    fn two_connections_between_the_same_devices_share_nothing() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (mut c1, _, _) = connect(&a, &b, 0);
        let (_, mut d2, _) = connect(&a, &b, 0);
        let f = c1.seal(b"x").unwrap();
        assert!(d2.open(&f).is_err());
    }

    #[test]
    fn oversized_and_malformed_input_is_refused() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        assert!(respond(&b, &[]).is_err());
        assert!(respond(&b, &[2; 33]).is_err());
        assert!(respond(&b, &[VERSION; 10]).is_err());
        // An all-zero ephemeral (no contribution) is refused.
        let mut m = vec![VERSION];
        m.extend_from_slice(&[0u8; 32]);
        assert!(respond(&b, &m).is_err());
        let (mut ca, _, _) = connect(&a, &b, 0);
        assert!(ca.seal(&vec![0; MAX_FRAME]).is_err());
        assert!(ca.open(&vec![0; MAX_FRAME + 1]).is_err());
    }
}
