pub mod client;
pub mod link;
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
    /// A copy of a message we sent from another of our own devices.
    SentCopy {
        /// Contact name if known, else the recipient's owner key.
        to: String,
        body: Vec<u8>,
        id: String,
    },
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
    // Every device of every contact, plus our own other devices, remembering
    // whose each one is. `None` marks one of our own devices.
    let mut cards: Vec<DeviceCard> = Vec::new();
    let mut owner_of: Vec<Option<usize>> = Vec::new();
    for (i, c) in contacts.iter().enumerate() {
        for d in c.identity()?.devices() {
            cards.push(d.clone());
            owner_of.push(Some(i));
        }
    }
    let my_id = me.device_id();
    for d in home.load_device_list()?.verify()? {
        if d.device_id() != my_id {
            cards.push(d);
            owner_of.push(None);
        }
    }
    match sessions.decrypt(me, &cards, blob) {
        Ok(Opened::Message {
            contact: device,
            body,
        }) => {
            let Ok(env) = Envelope::from_bytes(&body) else {
                // Authentic but not something we understand: keep the ratchet in step, drop it.
                home.save_sessions(sessions)?;
                home.save_identity(me)?;
                return Ok(Outcome::Invalid);
            };
            // A copy of our own sent message: file it under the conversation it names.
            let Some(contact) = owner_of[device] else {
                let Some(to) = env.to.clone() else {
                    home.save_sessions(sessions)?;
                    home.save_identity(me)?;
                    return Ok(Outcome::Invalid);
                };
                let fresh = home.messages()?.insert(&StoredMessage {
                    id: env.id.clone(),
                    peer: to.clone(),
                    direction: Direction::Out,
                    ts: env.ts,
                    body: env.body.clone(),
                })?;
                home.save_sessions(sessions)?;
                home.save_identity(me)?;
                if !fresh {
                    return Ok(Outcome::Duplicate);
                }
                let name = contacts
                    .iter()
                    .find(|c| c.conversation_key().ok().as_deref() == Some(to.as_str()))
                    .map_or(to, |c| c.name.clone());
                return Ok(Outcome::SentCopy {
                    to: name,
                    body: env.body.into_bytes(),
                    id: env.id,
                });
            };
            let fresh = home.messages()?.insert(&StoredMessage {
                id: env.id.clone(),
                peer: contacts[contact].conversation_key()?,
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
