//! Asynchronous, ratcheting 1:1 sessions (Olm via `vodozemac`).
//!
//! Starting a session needs one of the peer's signed one-time prekeys (served
//! by a relay). After that every message advances a Double Ratchet, which
//! gives forward secrecy and post-compromise recovery, and rejects replays
//! because each message key is used once and then deleted. The first message
//! in a session is a "pre-key" message that lets the recipient derive the same
//! state without being online.

use crate::identity::{verify_prekey, DeviceCard, Identity, SignedPrekey};
use crate::{Error, Result};
use std::collections::BTreeMap;
use vodozemac::olm::{OlmMessage, Session, SessionConfig, SessionPickle};
use vodozemac::Curve25519PublicKey;

pub const MAX_BODY: usize = 60_000;
/// Sessions kept per peer. Simultaneous first messages can create two.
const MAX_SESSIONS_PER_PEER: usize = 4;

/// All sessions on this device, keyed by the peer's hex Curve25519 identity key.
/// Most recently used first. Persist it after every encrypt/decrypt.
#[derive(Default)]
pub struct Sessions {
    by_peer: BTreeMap<String, Vec<Session>>,
}

#[derive(Debug)]
pub enum Opened {
    /// `contact` indexes into the `contacts` slice passed to [`Sessions::decrypt`].
    Message { contact: usize, body: Vec<u8> },
    /// A valid-looking first message from a key that is not a contact. No
    /// session is created and no prekey is consumed.
    UnknownSender { identity_key: [u8; 32] },
}

fn peer_key(x25519: &[u8; 32]) -> String {
    hex::encode(x25519)
}

fn session_err(e: impl std::fmt::Display) -> Error {
    Error::Session(e.to_string())
}

impl Sessions {
    pub fn to_json(&self) -> Result<String> {
        let m: BTreeMap<&String, Vec<SessionPickle>> = self
            .by_peer
            .iter()
            .map(|(k, v)| (k, v.iter().map(Session::pickle).collect()))
            .collect();
        Ok(serde_json::to_string_pretty(&m)?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        let m: BTreeMap<String, Vec<SessionPickle>> = serde_json::from_str(s)?;
        Ok(Self {
            by_peer: m
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(Session::from_pickle).collect()))
                .collect(),
        })
    }

    /// Used for the *quarantine* store (ADR 0021), which holds sessions with people who are not
    /// contacts yet. Decrypt a message from whoever sent it, creating the inbound session for
    /// a first (pre-key) message. Returns the sender's device key and the plaintext. A pre-key
    /// message consumes one of our one-time prekeys, so persist `me` afterwards.
    pub fn decrypt_any(&mut self, me: &mut Identity, blob: &[u8]) -> Result<([u8; 32], Vec<u8>)> {
        let (&kind, bytes) = blob.split_first().ok_or(Error::Malformed)?;
        let msg = OlmMessage::from_parts(kind as usize, bytes).map_err(|_| Error::Malformed)?;
        match &msg {
            OlmMessage::PreKey(pk) => {
                let sender = pk.identity_key().to_bytes();
                let key = peer_key(&sender);
                if let Some(list) = self.by_peer.get_mut(&key) {
                    if let Some(pos) = list.iter().position(|s| s.session_id() == pk.session_id()) {
                        let body = list[pos].decrypt(&msg).map_err(session_err)?;
                        list[..=pos].rotate_right(1);
                        return Ok((sender, body));
                    }
                }
                let r = me
                    .account_mut()
                    .create_inbound_session(SessionConfig::default(), pk.identity_key(), pk)
                    .map_err(session_err)?;
                self.insert(key, r.session);
                Ok((sender, r.plaintext))
            }
            OlmMessage::Normal(_) => {
                for (key, list) in self.by_peer.iter_mut() {
                    for pos in 0..list.len() {
                        if let Ok(body) = list[pos].decrypt(&msg) {
                            list[..=pos].rotate_right(1);
                            let mut sender = [0u8; 32];
                            hex::decode_to_slice(key, &mut sender).map_err(|_| Error::Corrupted)?;
                            return Ok((sender, body));
                        }
                    }
                }
                Err(Error::Session(
                    "no session could decrypt this message".into(),
                ))
            }
        }
    }

    /// The device keys this store holds sessions for.
    pub fn peers(&self) -> Vec<[u8; 32]> {
        self.by_peer
            .keys()
            .filter_map(|k| {
                let mut out = [0u8; 32];
                hex::decode_to_slice(k, &mut out).ok().map(|()| out)
            })
            .collect()
    }

    pub fn has_peer(&self, device_key: &[u8; 32]) -> bool {
        self.by_peer
            .get(&peer_key(device_key))
            .is_some_and(|v| !v.is_empty())
    }

    /// Move every session with `device_key` out of `from` into this store (a request was accepted).
    pub fn adopt(&mut self, from: &mut Sessions, device_key: &[u8; 32]) {
        let key = peer_key(device_key);
        if let Some(list) = from.by_peer.remove(&key) {
            for s in list.into_iter().rev() {
                self.insert(key.clone(), s);
            }
        }
    }

    /// Drop every session with `device_key` (a request was declined or refused).
    pub fn forget(&mut self, device_key: &[u8; 32]) {
        self.by_peer.remove(&peer_key(device_key));
    }

    pub fn has_session(&self, peer: &DeviceCard) -> bool {
        self.by_peer
            .get(&peer_key(&peer.x25519))
            .is_some_and(|v| !v.is_empty())
    }

    /// Begin a session with `peer` using one of their prekeys. The prekey's
    /// signature is checked against the peer's card, so a relay cannot
    /// substitute its own.
    pub fn start_outbound(
        &mut self,
        me: &Identity,
        peer: &DeviceCard,
        prekey: &SignedPrekey,
    ) -> Result<()> {
        verify_prekey(&peer.ed25519, prekey)?;
        let session = me
            .account()
            .create_outbound_session(
                SessionConfig::default(),
                Curve25519PublicKey::from_slice(&peer.x25519).map_err(session_err)?,
                Curve25519PublicKey::from_slice(&prekey.key).map_err(session_err)?,
            )
            .map_err(session_err)?;
        self.insert(peer_key(&peer.x25519), session);
        Ok(())
    }

    /// Encrypt `body` for `peer`. Returns the opaque blob to hand to a relay.
    pub fn encrypt(&mut self, peer: &DeviceCard, body: &[u8]) -> Result<Vec<u8>> {
        if body.len() > MAX_BODY {
            return Err(Error::TooLarge { max: MAX_BODY });
        }
        let session = self
            .by_peer
            .get_mut(&peer_key(&peer.x25519))
            .and_then(|v| v.first_mut())
            .ok_or(Error::NoSession)?;
        let (kind, bytes) = session.encrypt(body).map_err(session_err)?.to_parts();
        let mut out = Vec::with_capacity(1 + bytes.len());
        out.push(kind as u8);
        out.extend_from_slice(&bytes);
        Ok(out)
    }

    /// Decrypt a blob from any contact. `me` is mutated when a pre-key message
    /// consumes one of our one-time prekeys, so persist both afterwards.
    pub fn decrypt(
        &mut self,
        me: &mut Identity,
        contacts: &[DeviceCard],
        blob: &[u8],
    ) -> Result<Opened> {
        let (&kind, bytes) = blob.split_first().ok_or(Error::Malformed)?;
        let msg = OlmMessage::from_parts(kind as usize, bytes).map_err(|_| Error::Malformed)?;

        match &msg {
            OlmMessage::PreKey(pk) => {
                let sender = pk.identity_key().to_bytes();
                let Some(idx) = contacts.iter().position(|c| c.x25519 == sender) else {
                    return Ok(Opened::UnknownSender {
                        identity_key: sender,
                    });
                };
                let key = peer_key(&sender);
                // Already have the session this message belongs to? Use it (this
                // is how a replayed pre-key message is rejected).
                if let Some(list) = self.by_peer.get_mut(&key) {
                    if let Some(pos) = list.iter().position(|s| s.session_id() == pk.session_id()) {
                        let body = list[pos].decrypt(&msg).map_err(session_err)?;
                        list[..=pos].rotate_right(1);
                        return Ok(Opened::Message { contact: idx, body });
                    }
                }
                let r = me
                    .account_mut()
                    .create_inbound_session(SessionConfig::default(), pk.identity_key(), pk)
                    .map_err(session_err)?;
                self.insert(key, r.session);
                Ok(Opened::Message {
                    contact: idx,
                    body: r.plaintext,
                })
            }
            OlmMessage::Normal(_) => {
                for (idx, c) in contacts.iter().enumerate() {
                    let Some(list) = self.by_peer.get_mut(&peer_key(&c.x25519)) else {
                        continue;
                    };
                    for pos in 0..list.len() {
                        if let Ok(body) = list[pos].decrypt(&msg) {
                            list[..=pos].rotate_right(1);
                            return Ok(Opened::Message { contact: idx, body });
                        }
                    }
                }
                Err(Error::Session(
                    "no session could decrypt this message".into(),
                ))
            }
        }
    }

    fn insert(&mut self, key: String, session: Session) {
        let list = self.by_peer.entry(key).or_default();
        list.insert(0, session);
        list.truncate(MAX_SESSIONS_PER_PEER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dev {
        id: Identity,
        sessions: Sessions,
    }

    fn dev() -> Dev {
        Dev {
            id: Identity::generate().unwrap(),
            sessions: Sessions::default(),
        }
    }

    #[test]
    fn a_strangers_messages_live_in_quarantine_until_adopted() {
        let (mut alice, mut bob) = (dev(), dev());
        connect(&mut alice, &mut bob);
        let first = alice
            .sessions
            .encrypt(&bob.id.card(), b"hello, I am new")
            .unwrap();

        // Bob has never heard of Alice: the normal path refuses to create a session.
        let a_key = alice.id.card().x25519;
        assert!(matches!(
            bob.sessions.decrypt(&mut bob.id, &[], &first).unwrap(),
            Opened::UnknownSender { identity_key } if identity_key == a_key
        ));
        assert!(
            !bob.sessions.has_peer(&a_key),
            "and nothing was created in the main store"
        );

        // The quarantine store creates the session and reads the message.
        let mut quarantine = Sessions::default();
        let (who, body) = quarantine.decrypt_any(&mut bob.id, &first).unwrap();
        assert_eq!((who, body.as_slice()), (a_key, &b"hello, I am new"[..]));
        assert_eq!(quarantine.peers(), vec![a_key]);
        // A replay of the same pre-key message is refused, not delivered twice.
        assert!(quarantine.decrypt_any(&mut bob.id, &first).is_err());
        // A pre-key message again (Alice has not yet heard back, so every message is pre-key).
        let third = alice
            .sessions
            .encrypt(&bob.id.card(), b"and another")
            .unwrap();
        let (_, body) = quarantine.decrypt_any(&mut bob.id, &third).unwrap();
        assert_eq!(body, b"and another");
        // Garbage is refused.
        assert!(quarantine.decrypt_any(&mut bob.id, &[1, 2, 3]).is_err());

        // Accepting moves the sessions: now Bob reads Alice as a contact, and the quarantine is empty.
        bob.sessions.adopt(&mut quarantine, &a_key);
        assert!(quarantine.peers().is_empty() && bob.sessions.has_peer(&a_key));
        let reply = alice
            .sessions
            .encrypt(&bob.id.card(), b"now a contact")
            .unwrap();
        assert_eq!(
            open(&mut bob, &alice.id.card(), &reply).unwrap(),
            b"now a contact"
        );

        // Declining drops them.
        let mut q2 = Sessions::default();
        let (mut carol, mut dave) = (dev(), dev());
        connect(&mut carol, &mut dave);
        let m = carol.sessions.encrypt(&dave.id.card(), b"hi").unwrap();
        q2.decrypt_any(&mut dave.id, &m).unwrap();
        q2.forget(&carol.id.card().x25519);
        assert!(q2.peers().is_empty());
    }

    /// Alice starts a session with Bob using one of Bob's prekeys.
    fn connect(a: &mut Dev, b: &mut Dev) {
        let pk = b.id.fresh_prekeys(1).remove(0);
        a.sessions.start_outbound(&a.id, &b.id.card(), &pk).unwrap();
    }

    fn open(d: &mut Dev, from: &DeviceCard, blob: &[u8]) -> Result<Vec<u8>> {
        match d
            .sessions
            .decrypt(&mut d.id, std::slice::from_ref(from), blob)?
        {
            Opened::Message { body, .. } => Ok(body),
            Opened::UnknownSender { .. } => Err(Error::Malformed),
        }
    }

    #[test]
    fn two_way_conversation_ratchets() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let (ac, bc) = (a.id.card(), b.id.card());
        let m1 = a.sessions.encrypt(&bc, b"hi bob").unwrap();
        assert_eq!(open(&mut b, &ac, &m1).unwrap(), b"hi bob");
        let m2 = b.sessions.encrypt(&ac, b"hi alice").unwrap();
        assert_eq!(open(&mut a, &bc, &m2).unwrap(), b"hi alice");
        for i in 0..5 {
            let m = a
                .sessions
                .encrypt(&bc, format!("msg {i}").as_bytes())
                .unwrap();
            assert_eq!(
                open(&mut b, &ac, &m).unwrap(),
                format!("msg {i}").as_bytes()
            );
        }
    }

    #[test]
    fn plaintext_not_in_ciphertext() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let secret = b"very-recognisable-plaintext";
        let m = a.sessions.encrypt(&b.id.card(), secret).unwrap();
        assert!(!m.windows(secret.len()).any(|w| w == secret));
    }

    #[test]
    fn out_of_order_delivery_works() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let (ac, bc) = (a.id.card(), b.id.card());
        let m1 = a.sessions.encrypt(&bc, b"one").unwrap();
        let m2 = a.sessions.encrypt(&bc, b"two").unwrap();
        let m3 = a.sessions.encrypt(&bc, b"three").unwrap();
        assert_eq!(open(&mut b, &ac, &m1).unwrap(), b"one");
        assert_eq!(open(&mut b, &ac, &m3).unwrap(), b"three");
        assert_eq!(open(&mut b, &ac, &m2).unwrap(), b"two");
    }

    #[test]
    fn replay_is_rejected() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let (ac, bc) = (a.id.card(), b.id.card());
        let first = a.sessions.encrypt(&bc, b"first").unwrap();
        assert!(open(&mut b, &ac, &first).is_ok());
        assert!(
            open(&mut b, &ac, &first).is_err(),
            "replayed pre-key message accepted"
        );
        let second = a.sessions.encrypt(&bc, b"second").unwrap();
        assert!(open(&mut b, &ac, &second).is_ok());
        assert!(
            open(&mut b, &ac, &second).is_err(),
            "replayed message accepted"
        );
    }

    #[test]
    fn tampering_is_detected() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let (ac, bc) = (a.id.card(), b.id.card());
        let m = a.sessions.encrypt(&bc, b"x").unwrap();
        for i in [1, m.len() / 2, m.len() - 1] {
            let mut t = m.clone();
            t[i] ^= 1;
            assert!(open(&mut b, &ac, &t).is_err(), "flip at {i} accepted");
        }
        // The untouched original still works afterwards.
        assert_eq!(open(&mut b, &ac, &m).unwrap(), b"x");
    }

    #[test]
    fn third_party_cannot_read() {
        let (mut a, mut b, mut eve) = (dev(), dev(), dev());
        connect(&mut a, &mut b);
        let m = a.sessions.encrypt(&b.id.card(), b"x").unwrap();
        assert!(open(&mut eve, &a.id.card(), &m).is_err());
    }

    #[test]
    fn unknown_sender_creates_no_session_and_keeps_prekey() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let m = a.sessions.encrypt(&b.id.card(), b"x").unwrap();
        let r = b.sessions.decrypt(&mut b.id, &[], &m).unwrap();
        assert!(matches!(r, Opened::UnknownSender { .. }));
        // Bob can still accept it once Alice is a contact.
        assert!(open(&mut b, &a.id.card(), &m).is_ok());
    }

    #[test]
    fn forged_prekey_is_refused() {
        let (mut a, b, mut evil) = (dev(), dev(), dev());
        let forged = evil.id.fresh_prekeys(1).remove(0);
        assert!(a
            .sessions
            .start_outbound(&a.id, &b.id.card(), &forged)
            .is_err());
        assert!(matches!(
            a.sessions.encrypt(&b.id.card(), b"x"),
            Err(Error::NoSession)
        ));
    }

    #[test]
    fn fallback_prekey_lets_many_senders_start_sessions_and_blocks_replay() {
        let (mut a, mut b, mut c) = (dev(), dev(), dev());
        let fallback = b.id.fresh_fallback_prekey();
        // Two different senders use the same reusable fallback key.
        a.sessions
            .start_outbound(&a.id, &b.id.card(), &fallback)
            .unwrap();
        c.sessions
            .start_outbound(&c.id, &b.id.card(), &fallback)
            .unwrap();
        let (ma, mc) = (
            a.sessions.encrypt(&b.id.card(), b"from a").unwrap(),
            c.sessions.encrypt(&b.id.card(), b"from c").unwrap(),
        );
        let contacts = [a.id.card(), c.id.card()];
        for (m, want) in [(&ma, &b"from a"[..]), (&mc, &b"from c"[..])] {
            match b.sessions.decrypt(&mut b.id, &contacts, m).unwrap() {
                Opened::Message { body, .. } => assert_eq!(body, want),
                o => panic!("{o:?}"),
            }
        }
        // The key is reusable, so a replayed first message must still be refused.
        assert!(
            b.sessions.decrypt(&mut b.id, &contacts, &ma).is_err(),
            "replay via fallback accepted"
        );
    }

    #[test]
    fn rotated_fallback_still_opens_in_flight_messages_once_then_expires() {
        let (mut a, mut b) = (dev(), dev());
        let old = b.id.fresh_fallback_prekey();
        a.sessions
            .start_outbound(&a.id, &b.id.card(), &old)
            .unwrap();
        let in_flight = a.sessions.encrypt(&b.id.card(), b"in flight").unwrap();
        let _new = b.id.fresh_fallback_prekey(); // one rotation: old key still held
        assert!(
            open(&mut b, &a.id.card(), &in_flight).is_ok(),
            "message to the previous fallback key lost"
        );

        let (mut a2, mut b2) = (dev(), dev());
        let old = b2.id.fresh_fallback_prekey();
        a2.sessions
            .start_outbound(&a2.id, &b2.id.card(), &old)
            .unwrap();
        let too_late = a2.sessions.encrypt(&b2.id.card(), b"too late").unwrap();
        b2.id.fresh_fallback_prekey();
        b2.id.fresh_fallback_prekey(); // two rotations: old private key forgotten
        assert!(
            open(&mut b2, &a2.id.card(), &too_late).is_err(),
            "private fallback key kept too long"
        );
    }

    #[test]
    fn prekey_is_single_use() {
        let (mut a, mut b, mut c) = (dev(), dev(), dev());
        let pk = b.id.fresh_prekeys(1).remove(0);
        a.sessions.start_outbound(&a.id, &b.id.card(), &pk).unwrap();
        c.sessions.start_outbound(&c.id, &b.id.card(), &pk).unwrap();
        let ma = a.sessions.encrypt(&b.id.card(), b"from a").unwrap();
        let mc = c.sessions.encrypt(&b.id.card(), b"from c").unwrap();
        let contacts = [a.id.card(), c.id.card()];
        assert!(matches!(
            b.sessions.decrypt(&mut b.id, &contacts, &ma).unwrap(),
            Opened::Message { .. }
        ));
        assert!(
            b.sessions.decrypt(&mut b.id, &contacts, &mc).is_err(),
            "reused prekey must not open a second session"
        );
    }

    #[test]
    fn stolen_state_cannot_read_past_messages() {
        // Forward secrecy: an attacker who copies Bob's *current* keys and sessions and
        // also holds earlier captured ciphertext still cannot read it.
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let (ac, bc) = (a.id.card(), b.id.card());
        let m1 = a.sessions.encrypt(&bc, b"old secret 1").unwrap();
        open(&mut b, &ac, &m1).unwrap();
        let r1 = b.sessions.encrypt(&ac, b"reply").unwrap();
        open(&mut a, &bc, &r1).unwrap();
        let m2 = a.sessions.encrypt(&bc, b"old secret 2").unwrap();
        open(&mut b, &ac, &m2).unwrap();
        let m3 = a.sessions.encrypt(&bc, b"newest").unwrap();
        open(&mut b, &ac, &m3).unwrap();

        // "Theft": serialize Bob's full state, restore it elsewhere.
        let mut thief = Dev {
            id: Identity::from_json(&b.id.to_json().unwrap()).unwrap(),
            sessions: Sessions::from_json(&b.sessions.to_json().unwrap()).unwrap(),
        };
        for captured in [&m1, &m2, &m3] {
            assert!(
                open(&mut thief, &ac, captured).is_err(),
                "stolen state decrypted past traffic"
            );
        }
    }

    #[test]
    fn state_survives_persistence() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        let (ac, bc) = (a.id.card(), b.id.card());
        let m1 = a.sessions.encrypt(&bc, b"one").unwrap();
        open(&mut b, &ac, &m1).unwrap();
        let mut a2 = Dev {
            id: Identity::from_json(&a.id.to_json().unwrap()).unwrap(),
            sessions: Sessions::from_json(&a.sessions.to_json().unwrap()).unwrap(),
        };
        let m2 = a2.sessions.encrypt(&bc, b"two").unwrap();
        assert_eq!(open(&mut b, &ac, &m2).unwrap(), b"two");
    }

    #[test]
    fn oversize_rejected() {
        let (mut a, mut b) = (dev(), dev());
        connect(&mut a, &mut b);
        assert!(matches!(
            a.sessions.encrypt(&b.id.card(), &vec![0; MAX_BODY + 1]),
            Err(Error::TooLarge { .. })
        ));
    }

    #[test]
    fn card_roundtrip_and_binding() {
        let a = Identity::generate().unwrap();
        let c = a.card();
        assert_eq!(DeviceCard::decode(&c.encode()).unwrap(), c);
        let other = Identity::generate().unwrap().card();
        let mut raw = hex::decode(&c.encode()["spike1:".len()..]).unwrap();
        raw[32..64].copy_from_slice(&other.x25519);
        assert!(DeviceCard::decode(&format!("spike1:{}", hex::encode(raw))).is_err());
    }
}
