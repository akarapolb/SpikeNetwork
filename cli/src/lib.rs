pub mod client;
pub mod state;

use spike_core::identity::Identity;
use spike_core::session;
use state::Home;

/// What happened to one delivered blob. Every outcome is safe to acknowledge
/// to the relay: nothing here can be fixed by redelivery.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Message {
        from: String,
        verified: bool,
        ts: u64,
        body: Vec<u8>,
    },
    /// Already processed (relay redelivery or a replayed ciphertext).
    Duplicate,
    /// Authentic, but the sender's key is not in our contacts.
    UnknownSender { fingerprint_hint: String },
    /// Not decryptable or malformed: corrupted, tampered, or not for us.
    Invalid,
}

pub fn process(home: &Home, me: &Identity, blob: &[u8]) -> anyhow::Result<Outcome> {
    let Ok(opened) = session::open(me, blob) else {
        return Ok(Outcome::Invalid);
    };
    let contacts = home.load_contacts()?;
    let sender = contacts.iter().find(|c| {
        c.decode()
            .map(|card| card.x25519 == opened.sender_x25519)
            .unwrap_or(false)
    });
    let Some(sender) = sender else {
        return Ok(Outcome::UnknownSender {
            fingerprint_hint: hex::encode(&opened.sender_x25519[..6]),
        });
    };
    // Record before the caller acks, so a crash re-delivers rather than loses.
    if !home.mark_seen(&hex::encode(opened.id))? {
        return Ok(Outcome::Duplicate);
    }
    Ok(Outcome::Message {
        from: sender.name.clone(),
        verified: sender.verified,
        ts: opened.ts,
        body: opened.body,
    })
}
