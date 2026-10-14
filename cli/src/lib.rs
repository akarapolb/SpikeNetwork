pub mod client;
pub mod messages;
pub mod ops;
pub mod state;
pub mod transport;

use messages::{Direction, StoredMessage};
use spike_core::envelope::Envelope;
use spike_core::identity::{DeviceCard, Identity};
use spike_core::session::{Opened, Sessions};
use state::Home;

/// What happened to one delivered blob. Every outcome is safe to acknowledge
/// to the relay: nothing here can be fixed by redelivery.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Message {
        from: String,
        verified: bool,
        body: Vec<u8>,
        /// Envelope id, the same on every device that stores this message.
        id: String,
        /// Sender's clock, ms since the epoch.
        ts: u64,
    },
    /// Decrypted fine, but this message id is already stored (e.g. redelivered after a crash).
    Duplicate,
    /// A first message from a key that is not a contact. Dropped; no session is created.
    UnknownSender { fingerprint_hint: String },
    /// Not decryptable: corrupted, tampered, replayed, or not for us.
    Invalid,
}

/// Decrypt one blob, store the message, then persist the ratchet state, all
/// *before* returning, so the caller can safely ack afterwards.
///
/// Order matters: the message is stored first. If the process dies before the
/// ratchet state is saved, the relay redelivers, the old state decrypts it again,
/// and the duplicate insert is ignored. Nothing is lost and nothing is doubled.
pub fn process(
    home: &Home,
    me: &mut Identity,
    sessions: &mut Sessions,
    blob: &[u8],
) -> anyhow::Result<Outcome> {
    let contacts = home.load_contacts()?;
    let cards: Vec<DeviceCard> = contacts
        .iter()
        .map(|c| c.decode())
        .collect::<anyhow::Result<_>>()?;
    match sessions.decrypt(me, &cards, blob) {
        Ok(Opened::Message { contact, body }) => {
            let Ok(env) = Envelope::from_bytes(&body) else {
                // Authentic but not something we understand: keep the ratchet in step, drop it.
                home.save_sessions(sessions)?;
                home.save_identity(me)?;
                return Ok(Outcome::Invalid);
            };
            let fresh = home.messages()?.insert(&StoredMessage {
                id: env.id.clone(),
                peer: hex::encode(cards[contact].x25519),
                direction: Direction::In,
                ts: env.ts,
                body: env.body.clone(),
            })?;
            home.save_sessions(sessions)?;
            home.save_identity(me)?;
            if !fresh {
                return Ok(Outcome::Duplicate);
            }
            let c = &contacts[contact];
            Ok(Outcome::Message {
                from: c.name.clone(),
                verified: c.verified,
                body: env.body.into_bytes(),
                id: env.id,
                ts: env.ts,
            })
        }
        Ok(Opened::UnknownSender { identity_key }) => Ok(Outcome::UnknownSender {
            fingerprint_hint: hex::encode(&identity_key[..6]),
        }),
        Err(_) => Ok(Outcome::Invalid),
    }
}
