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
    /// A message was edited. `who` is the contact's name, or "you" for another of your devices.
    Edited { who: String, id: String },
    /// A message was deleted.
    Deleted { who: String, id: String },
    /// A valid edit or delete that changes nothing here (older than what we have,
    /// for a message we never had, or already applied).
    Ignored,
    /// An edit or delete from someone who is not the message's author. Not applied.
    Rejected { reason: String },
    /// A first message from a key that is not a contact. Dropped; no session is created.
    UnknownSender { fingerprint_hint: String },
    /// Not decryptable: corrupted, tampered, replayed, or not for us.
    Invalid,
}

/// Apply an edit or delete to the local history under the merge rules in
/// `spike_core::merge`. `sender_is_me` says whether the change comes from one of
/// our own devices; `peer` is the conversation (the other person's owner key).
pub fn apply_change(
    home: &Home,
    env: &Envelope,
    sender_is_me: bool,
    peer: &str,
    sender_device: &str,
) -> anyhow::Result<MergeAction> {
    use spike_core::envelope::Kind;
    let target = env
        .target
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("change without a target"))?;
    let store = home.messages()?;
    let current = store.get(target)?;
    // A message filed under a different conversation is not the sender's to change.
    let current = match current {
        Some(m) if m.peer != peer => return Ok(MergeAction::NotAuthor),
        other => other,
    };
    let stamp = (env.lamport, sender_device.to_string());
    let action = spike_core::merge::decide(
        env.kind,
        sender_is_me,
        current.as_ref().map(StoredMessage::merge_state).as_ref(),
        &stamp,
    );
    match (&action, current) {
        (MergeAction::ApplyEdit, Some(mut m)) => {
            m.body = env.body.clone();
            m.edited = true;
            m.lamport = stamp.0;
            m.stamp_device = stamp.1;
            store.update(&m)?;
        }
        (MergeAction::ApplyDelete, Some(mut m)) => {
            m.body.clear();
            m.attachment = None;
            m.deleted = true;
            store.update(&m)?;
            home.delete_attachment(&m.id)?;
        }
        (MergeAction::Tombstone, _) if env.kind == Kind::Delete => {
            // The message has not arrived yet: leave a tombstone so it never appears.
            store.insert(&StoredMessage {
                id: target.to_string(),
                peer: peer.to_string(),
                direction: if sender_is_me {
                    Direction::Out
                } else {
                    Direction::In
                },
                ts: env.ts,
                body: String::new(),
                attachment: None,
                lamport: stamp.0,
                stamp_device: stamp.1,
                edited: false,
                deleted: true,
            })?;
        }
        _ => {}
    }
    Ok(action)
}

pub use spike_core::merge::Action as MergeAction;

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
    use spike_core::envelope::Kind;
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
    let persist = |sessions: &Sessions, me: &Identity| -> anyhow::Result<()> {
        home.save_sessions(sessions)?;
        home.save_identity(me)
    };
    match sessions.decrypt(me, &cards, blob) {
        Ok(Opened::Message {
            contact: device,
            body,
        }) => {
            let Ok(env) = Envelope::from_bytes(&body) else {
                // Authentic but not something we understand: keep the ratchet in step, drop it.
                persist(sessions, me)?;
                return Ok(Outcome::Invalid);
            };
            let sender_device = cards[device].device_id();
            home.observe_lamport(env.lamport)?;
            match owner_of[device] {
                // From one of our own other devices.
                None => {
                    let Some(to) = env.to.clone() else {
                        persist(sessions, me)?;
                        return Ok(Outcome::Invalid);
                    };
                    let name = contacts
                        .iter()
                        .find(|c| c.conversation_key().ok().as_deref() == Some(to.as_str()))
                        .map_or_else(|| to.clone(), |c| c.name.clone());
                    if env.kind != Kind::Text {
                        let action = apply_change(home, &env, true, &to, &sender_device)?;
                        persist(sessions, me)?;
                        return Ok(change_outcome(
                            env.kind,
                            action,
                            "you".into(),
                            env.target.clone().unwrap_or_default(),
                        ));
                    }
                    let fresh = home.messages()?.insert(&StoredMessage {
                        id: env.id.clone(),
                        peer: to,
                        direction: Direction::Out,
                        ts: env.ts,
                        body: env.body.clone(),
                        attachment: env.attachment.clone(),
                        lamport: env.lamport,
                        stamp_device: sender_device,
                        edited: false,
                        deleted: false,
                    })?;
                    persist(sessions, me)?;
                    if !fresh {
                        return Ok(Outcome::Duplicate);
                    }
                    Ok(Outcome::SentCopy {
                        to: name,
                        body: env.body.into_bytes(),
                        id: env.id,
                    })
                }
                // From a contact.
                Some(contact) => {
                    let peer = contacts[contact].conversation_key()?;
                    if env.kind != Kind::Text {
                        let action = apply_change(home, &env, false, &peer, &sender_device)?;
                        persist(sessions, me)?;
                        return Ok(change_outcome(
                            env.kind,
                            action,
                            contacts[contact].name.clone(),
                            env.target.clone().unwrap_or_default(),
                        ));
                    }
                    let fresh = home.messages()?.insert(&StoredMessage {
                        id: env.id.clone(),
                        peer,
                        direction: Direction::In,
                        ts: env.ts,
                        body: env.body.clone(),
                        attachment: env.attachment.clone(),
                        lamport: env.lamport,
                        stamp_device: sender_device,
                        edited: false,
                        deleted: false,
                    })?;
                    persist(sessions, me)?;
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
            }
        }
        Ok(Opened::UnknownSender { identity_key }) => Ok(Outcome::UnknownSender {
            fingerprint_hint: hex::encode(&identity_key[..6]),
        }),
        Err(_) => Ok(Outcome::Invalid),
    }
}

fn change_outcome(
    kind: spike_core::envelope::Kind,
    action: MergeAction,
    who: String,
    id: String,
) -> Outcome {
    use spike_core::envelope::Kind;
    match (action, kind) {
        (MergeAction::ApplyEdit, _) => Outcome::Edited { who, id },
        (MergeAction::ApplyDelete, _) | (MergeAction::Tombstone, Kind::Delete) => {
            Outcome::Deleted { who, id }
        }
        (MergeAction::NotAuthor, _) => Outcome::Rejected {
            reason: format!("{who} tried to change a message they did not write"),
        },
        _ => Outcome::Ignored,
    }
}
