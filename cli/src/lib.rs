pub mod client;
pub mod ops;
pub mod state;

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
    },
    /// A first message from a key that is not a contact. Dropped; no session is created.
    UnknownSender { fingerprint_hint: String },
    /// Not decryptable: corrupted, tampered, replayed, or not for us.
    Invalid,
}

/// Decrypt one blob and persist the resulting ratchet state *before* returning,
/// so the caller can safely ack afterwards.
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
            home.save_sessions(sessions)?;
            home.save_identity(me)?;
            let c = &contacts[contact];
            Ok(Outcome::Message {
                from: c.name.clone(),
                verified: c.verified,
                body,
            })
        }
        Ok(Opened::UnknownSender { identity_key }) => Ok(Outcome::UnknownSender {
            fingerprint_hint: hex::encode(&identity_key[..6]),
        }),
        Err(_) => Ok(Outcome::Invalid),
    }
}
