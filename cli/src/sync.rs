//! Messages exchanged only between a person's OWN devices (envelope kind
//! `sync`, milestone M5): read state, catch-up after a long absence, and
//! file requests. They are end-to-end encrypted like any message, and a
//! device refuses them from anyone who is not in its own signed device list.

use crate::messages::StoredMessage;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use spike_core::agent::SignedRequest;
use spike_core::attachment::Attachment;
use spike_core::envelope::{Envelope, Kind, MAX_TEXT};

/// Largest serialised catch-up batch, comfortably inside one envelope.
const BATCH_BYTES: usize = 30_000;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncMsg {
    /// "I have read this conversation up to `upto` (sender time, ms)." Merged with max.
    Read { peer: String, upto: u64 },
    /// "Send me everything you learned or changed since `since` (your clock, ms)."
    CatchUpRequest { since: u64 },
    /// A slice of the answer. `last` marks the final slice (an empty answer is one empty slice).
    CatchUpBatch {
        messages: Vec<StoredMessage>,
        last: bool,
    },
    /// "Please make this message's attachment available to me again."
    FileRequest { msg_id: String },
    /// A fresh descriptor for the same file (new key, new blob on the answering device's relay).
    FileOffer {
        msg_id: String,
        attachment: Attachment,
    },
    /// From the approver device: "this request waits for a human; you may decide it too."
    AgentPending {
        request: SignedRequest,
        from: String,
    },
    /// To the approver device: "the human decided this request here."
    AgentDecision {
        request_id: String,
        approve: bool,
        note: String,
    },
    /// "I accepted, declined or blocked the invite to this group": every device of yours that
    /// holds the same invite settles it too (ADR 0024). A block carries the inviter's card.
    GroupInvite {
        group: String,
        decision: crate::groups::InviteDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card: Option<String>,
    },
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A conversation is a person's owner key (64 hex) or a group id (32 hex); a group message also
/// names its author, who is an owner key.
fn peer_ok(m: &StoredMessage) -> bool {
    match &m.author {
        None => is_hex(&m.peer, 64),
        Some(author) => is_hex(author, 64) && is_hex(&m.peer, 32),
    }
}

impl SyncMsg {
    pub fn to_envelope(&self) -> Result<Envelope> {
        Ok(Envelope::sync(&serde_json::to_string(self)?)?)
    }

    /// Parse and validate the payload of a received sync envelope.
    pub fn from_envelope(e: &Envelope) -> Result<Self> {
        if e.kind != Kind::Sync {
            bail!("not a sync message");
        }
        let m: SyncMsg = serde_json::from_str(&e.body)?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<()> {
        let ok = match self {
            SyncMsg::Read { peer, .. } => is_hex(peer, 64) || is_hex(peer, 32),
            SyncMsg::CatchUpRequest { .. } => true,
            SyncMsg::FileRequest { msg_id } => is_hex(msg_id, 32),
            SyncMsg::FileOffer { msg_id, attachment } => {
                is_hex(msg_id, 32) && attachment.validate().is_ok()
            }
            SyncMsg::AgentPending { request, from } => {
                request.verify().is_ok() && from.len() <= 64 && !from.chars().any(char::is_control)
            }
            SyncMsg::AgentDecision {
                request_id, note, ..
            } => is_hex(request_id, 32) && note.len() <= 200,
            SyncMsg::GroupInvite { group, card, .. } => {
                is_hex(group, 32)
                    && card.as_ref().is_none_or(|c| {
                        c.len() <= spike_core::envelope::MAX_INTRO
                            && !c.chars().any(char::is_control)
                    })
            }
            SyncMsg::CatchUpBatch { messages, .. } => messages.iter().all(|m| {
                is_hex(&m.id, 32)
                    && peer_ok(m)
                    && m.body.len() <= MAX_TEXT
                    && m.attachment.as_ref().is_none_or(|a| a.validate().is_ok())
            }),
        };
        if ok {
            Ok(())
        } else {
            bail!("malformed sync message")
        }
    }
}

/// Split `messages` into batches that each fit in one envelope. Always yields
/// at least one batch, and the final one has `last = true`.
pub fn batches(messages: Vec<StoredMessage>) -> Vec<SyncMsg> {
    let mut out: Vec<Vec<StoredMessage>> = vec![Vec::new()];
    let mut size = 0usize;
    for m in messages {
        let n = serde_json::to_string(&m).map(|s| s.len()).unwrap_or(0) + 1;
        if size + n > BATCH_BYTES && !out.last().unwrap().is_empty() {
            out.push(Vec::new());
            size = 0;
        }
        size += n;
        out.last_mut().unwrap().push(m);
    }
    let total = out.len();
    out.into_iter()
        .enumerate()
        .map(|(i, messages)| SyncMsg::CatchUpBatch {
            messages,
            last: i + 1 == total,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Direction;

    fn msg(i: usize, body_len: usize) -> StoredMessage {
        StoredMessage {
            id: format!("{i:032x}"),
            peer: "ab".repeat(32),
            direction: Direction::In,
            ts: i as u64,
            body: "x".repeat(body_len),
            attachment: None,
            lamport: i as u64,
            stamp_device: "d".into(),
            edited: false,
            deleted: false,
            changed_at: 0,
            author: None,
            shared_by: None,
        }
    }

    #[test]
    fn batches_fit_and_the_last_is_marked() {
        let out = batches((0..40).map(|i| msg(i, 2_000)).collect());
        assert!(out.len() > 1);
        let mut total = 0;
        for (i, b) in out.iter().enumerate() {
            let SyncMsg::CatchUpBatch { messages, last } = b else {
                panic!()
            };
            assert_eq!(*last, i + 1 == out.len());
            total += messages.len();
            let env = b.to_envelope().unwrap();
            assert!(
                env.to_bytes().len() <= spike_core::session::MAX_BODY,
                "batch fits in a session message"
            );
            assert_eq!(
                SyncMsg::from_envelope(&Envelope::from_bytes(&env.to_bytes()).unwrap()).unwrap(),
                *b
            );
        }
        assert_eq!(total, 40);
        let empty = batches(vec![]);
        assert_eq!(
            empty,
            vec![SyncMsg::CatchUpBatch {
                messages: vec![],
                last: true
            }]
        );
    }

    #[test]
    fn invite_decisions_are_checked() {
        use crate::groups::InviteDecision::{Accept, Block};
        let ok = |m: SyncMsg| SyncMsg::from_envelope(&m.to_envelope().unwrap()).is_ok();
        let group = "cd".repeat(16);
        assert!(ok(SyncMsg::GroupInvite {
            group: group.clone(),
            decision: Accept,
            card: None
        }));
        assert!(ok(SyncMsg::GroupInvite {
            group: group.clone(),
            decision: Block,
            card: Some("spike2:ab".into())
        }));
        assert!(!ok(SyncMsg::GroupInvite {
            group: "nothex".into(),
            decision: Accept,
            card: None
        }));
        let huge = "a".repeat(spike_core::envelope::MAX_INTRO + 1);
        assert!(!ok(SyncMsg::GroupInvite {
            group,
            decision: Block,
            card: Some(huge)
        }));
    }

    #[test]
    fn group_messages_and_group_read_markers_are_accepted() {
        let owner = "ab".repeat(32);
        let group = "cd".repeat(16);
        let ok = SyncMsg::CatchUpBatch {
            messages: vec![StoredMessage {
                peer: group.clone(),
                author: Some(owner.clone()),
                shared_by: None,
                ..msg(1, 1)
            }],
            last: true,
        };
        assert!(SyncMsg::from_envelope(&ok.to_envelope().unwrap()).is_ok());
        let read = SyncMsg::Read {
            peer: group.clone(),
            upto: 5,
        };
        assert!(SyncMsg::from_envelope(&read.to_envelope().unwrap()).is_ok());
        // A group id without an author, or an owner key with one, is not a consistent record.
        for (peer, author) in [(group.clone(), None), (owner.clone(), Some(owner.clone()))] {
            let bad = SyncMsg::CatchUpBatch {
                messages: vec![StoredMessage {
                    peer,
                    author,
                    ..msg(1, 1)
                }],
                last: true,
            };
            assert!(SyncMsg::from_envelope(&bad.to_envelope().unwrap()).is_err());
        }
    }

    #[test]
    fn malformed_payloads_are_refused() {
        let bad = [
            SyncMsg::Read {
                peer: "nothex".into(),
                upto: 1,
            },
            SyncMsg::FileRequest {
                msg_id: "short".into(),
            },
            SyncMsg::CatchUpBatch {
                messages: vec![StoredMessage {
                    id: "x".into(),
                    ..msg(1, 1)
                }],
                last: true,
            },
        ];
        for b in bad {
            assert!(
                SyncMsg::from_envelope(&b.to_envelope().unwrap()).is_err(),
                "{b:?}"
            );
        }
        let text = Envelope::text("hello").unwrap();
        assert!(SyncMsg::from_envelope(&text).is_err());
    }
}
