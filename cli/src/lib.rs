pub mod client;
pub mod link;
pub mod messages;
pub mod ops;
pub mod state;
pub mod sync;
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
    /// Another of your devices marked a conversation read (merged by max).
    ReadSynced { peer: String },
    /// A catch-up answer from another of your devices was merged; `changed`
    /// counts messages added or updated. `last` marks the final slice.
    CaughtUp { changed: usize, last: bool },
    /// Another of your devices re-supplied an attachment; `spike fetch` can now get it.
    FileReady { id: String },
    /// Another of your devices asks for help (catch-up or a file). The caller answers.
    SyncRequest {
        from_device: String,
        msg: sync::SyncMsg,
    },
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
            m.lamport = stamp.0;
            m.stamp_device = stamp.1;
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
                changed_at: 0,
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
                    if env.kind == Kind::Sync {
                        let outcome = match sync::SyncMsg::from_envelope(&env) {
                            Ok(msg) => apply_sync(home, &contacts, msg, &sender_device)?,
                            Err(_) => Outcome::Invalid,
                        };
                        persist(sessions, me)?;
                        return Ok(outcome);
                    }
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
                        changed_at: 0,
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
                    if env.kind == Kind::Sync {
                        // Sync messages are for a person's own devices only. A contact has no business sending one.
                        persist(sessions, me)?;
                        return Ok(Outcome::Rejected {
                            reason: format!(
                                "{} sent a message reserved for your own devices",
                                contacts[contact].name
                            ),
                        });
                    }
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
                        changed_at: 0,
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

/// Apply a sync message that came from one of our own devices.
fn apply_sync(
    home: &Home,
    contacts: &[state::Contact],
    msg: sync::SyncMsg,
    sender_device: &str,
) -> anyhow::Result<Outcome> {
    use sync::SyncMsg;
    Ok(match msg {
        SyncMsg::Read { peer, upto } => {
            home.advance_read_marker(&peer, upto)?;
            let name = contacts
                .iter()
                .find(|c| c.conversation_key().ok().as_deref() == Some(peer.as_str()))
                .map_or(peer, |c| c.name.clone());
            Outcome::ReadSynced { peer: name }
        }
        SyncMsg::CatchUpBatch { messages, last } => {
            let store = home.messages()?;
            let mut changed = 0;
            for r in &messages {
                if store.merge_record(r)? {
                    changed += 1;
                }
                if r.deleted {
                    home.delete_attachment(&r.id)?;
                }
                home.observe_lamport(r.lamport)?;
            }
            Outcome::CaughtUp { changed, last }
        }
        SyncMsg::FileOffer { msg_id, attachment } => {
            let store = home.messages()?;
            match store.get(&msg_id)? {
                Some(mut m)
                    if m.attachment.as_ref().is_some_and(|a| {
                        a.sha256 == attachment.sha256
                            && a.size == attachment.size
                            && a.name == attachment.name
                    }) =>
                {
                    // Same file (same hash, size, name), new key and blob on a fresh relay copy.
                    m.attachment = Some(attachment);
                    store.update(&m)?;
                    Outcome::FileReady { id: msg_id }
                }
                _ => Outcome::Rejected {
                    reason: "a file offer did not match the message it names".into(),
                },
            }
        }
        req @ (SyncMsg::CatchUpRequest { .. } | SyncMsg::FileRequest { .. }) => {
            Outcome::SyncRequest {
                from_device: sender_device.to_string(),
                msg: req,
            }
        }
    })
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
