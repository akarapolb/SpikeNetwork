//! What travels *inside* an encrypted session message: a small, versioned
//! envelope with a stable random id. The id lets every device store a message
//! exactly once (idempotent inserts), which is what multi-device fan-out,
//! redelivery after a crash, and history transfer all rely on.
//! Relays never see an envelope; it is only ever the plaintext of a session.

use crate::{Error, Result};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: u8 = 1;
/// Largest text body accepted, in bytes (the session layer caps the whole envelope).
pub const MAX_TEXT: usize = 50_000;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    /// Replace the text of an earlier message by the same author (`target`).
    Edit,
    /// Delete an earlier message by the same author (`target`).
    Delete,
    /// Housekeeping between a person's OWN devices (read state, catch-up,
    /// file requests). The body is JSON owned by the client. Receivers must
    /// refuse it from anyone who is not one of their own devices.
    Sync,
    /// An agent's signed request to its principal (body: `agent::SignedRequest` JSON).
    AgentRequest,
    /// The principal's signed answer (body: `agent::Receipt` JSON).
    AgentReceipt,
    /// Signed group states (body: JSON array of `group::GroupState`, oldest first; ADR 0024).
    GroupState,
    /// Ask a member for the group states after a version (body: `{"after": n}`).
    GroupStateRequest,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub v: u8,
    /// 128-bit random id, lowercase hex. Globally unique in practice.
    pub id: String,
    /// Sender's clock, milliseconds since the Unix epoch. Display only; never trusted for ordering security.
    pub ts: u64,
    pub kind: Kind,
    /// Logical clock (Lamport): the sender's counter after seeing everything it
    /// has seen. Orders concurrent edits from different devices without
    /// trusting wall clocks. 0 on messages from older versions.
    #[serde(default)]
    pub lamport: u64,
    /// The message an edit or delete applies to (its envelope id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub body: String,
    /// The conversation's other person (their owner key, hex). Set on every
    /// message so that copies delivered to the sender's *own* other devices
    /// know which conversation they belong to. Ignored on messages from others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// A file sent with this message (the body is its caption, possibly empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<crate::attachment::Attachment>,
    /// The sender's identity card, attached when a new session is started so a
    /// recipient who has never heard of them can tell who is writing (ADR 0021).
    /// Only trusted after it is checked against the device that sent the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<String>,
    /// The group this message belongs to (ADR 0024). When set, `to` is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Hash of the group state the sender used, so receivers can tell an
    /// out-of-date member list from their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gstate: Option<String>,
}

/// Longest introduction accepted: a card with a generous number of devices.
pub const MAX_INTRO: usize = 16 * 1024;

impl Envelope {
    pub fn text(body: &str) -> Result<Self> {
        if body.len() > MAX_TEXT {
            return Err(Error::TooLarge { max: MAX_TEXT });
        }
        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok(Self {
            v: VERSION,
            id: hex::encode(id),
            ts,
            kind: Kind::Text,
            lamport: 0,
            target: None,
            body: body.to_string(),
            to: None,
            attachment: None,
            intro: None,
            group: None,
            gstate: None,
        })
    }

    /// An edit: new full text for message `target`.
    pub fn edit(target: &str, body: &str) -> Result<Self> {
        let mut e = Self::text(body)?;
        e.kind = Kind::Edit;
        e.target = Some(target.to_string());
        Ok(e)
    }

    /// A sync message for the sender's own devices; `json` is the client's payload.
    pub fn sync(json: &str) -> Result<Self> {
        let mut e = Self::text(json)?;
        e.kind = Kind::Sync;
        Ok(e)
    }

    /// A signed agent request or receipt (`json`), addressed like a message.
    pub fn agent(kind: Kind, json: &str) -> Result<Self> {
        debug_assert!(matches!(kind, Kind::AgentRequest | Kind::AgentReceipt));
        let mut e = Self::text(json)?;
        e.kind = kind;
        Ok(e)
    }

    /// Group states or a request for them, addressed to one group (ADR 0024).
    pub fn group_control(kind: Kind, group: &str, json: &str) -> Result<Self> {
        debug_assert!(matches!(kind, Kind::GroupState | Kind::GroupStateRequest));
        let mut e = Self::text(json)?;
        e.kind = kind;
        e.group = Some(group.to_string());
        Ok(e)
    }

    /// A delete of message `target`.
    pub fn delete(target: &str) -> Result<Self> {
        let mut e = Self::text("")?;
        e.kind = Kind::Delete;
        e.target = Some(target.to_string());
        Ok(e)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("envelope serializes")
    }

    /// Parse and validate. Anything malformed, too large, or of an unknown
    /// version or kind is rejected rather than half-understood.
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let e: Envelope = serde_json::from_slice(b).map_err(|_| Error::Malformed)?;
        let id_ok = e.id.len() == 32
            && e.id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
        let to_ok = e.to.as_ref().is_none_or(|t| {
            t.len() == 64
                && t.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        });
        let att_ok = e.attachment.as_ref().is_none_or(|a| a.validate().is_ok());
        // Only a text message or a group invite introduces its sender, and the card must be a sane size.
        let intro_ok = e.intro.as_ref().is_none_or(|c| {
            c.len() <= MAX_INTRO
                && matches!(e.kind, Kind::Text | Kind::GroupState)
                && !c.chars().any(char::is_control)
        });
        let hex_of = |s: &str, n: usize| {
            s.len() == n
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        };
        let group_ok = e.group.as_deref().is_none_or(|g| hex_of(g, 32))
            && e.gstate.as_deref().is_none_or(|h| hex_of(h, 64))
            && match e.kind {
                // Group control always names its group; private kinds never do.
                Kind::GroupState | Kind::GroupStateRequest => e.group.is_some(),
                Kind::Sync | Kind::AgentRequest | Kind::AgentReceipt => {
                    e.group.is_none() && e.gstate.is_none()
                }
                // A group message says which member list it was sent with.
                Kind::Text | Kind::Edit | Kind::Delete => e.group.is_some() == e.gstate.is_some(),
            };
        let target_ok = match e.kind {
            Kind::Text
            | Kind::Sync
            | Kind::AgentRequest
            | Kind::AgentReceipt
            | Kind::GroupState
            | Kind::GroupStateRequest => e.target.is_none(),
            Kind::Edit | Kind::Delete => e.target.as_ref().is_some_and(|t| {
                t.len() == 32
                    && t.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            }),
        };
        // Edits carry text only; deletes carry nothing.
        let shape_ok = match e.kind {
            Kind::Text => true,
            Kind::Edit => e.attachment.is_none(),
            Kind::Delete => e.attachment.is_none() && e.body.is_empty(),
            Kind::Sync
            | Kind::AgentRequest
            | Kind::AgentReceipt
            | Kind::GroupState
            | Kind::GroupStateRequest => e.attachment.is_none() && !e.body.is_empty(),
        };
        if e.v != VERSION
            || !id_ok
            || !to_ok
            || !att_ok
            || !intro_ok
            || !group_ok
            || !target_ok
            || !shape_ok
            || e.body.len() > MAX_TEXT
        {
            return Err(Error::Malformed);
        }
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_fields_are_checked() {
        let g = "ab".repeat(16);
        let h = "cd".repeat(32);
        let mut e = Envelope::text("hi all").unwrap();
        e.group = Some(g.clone());
        assert!(
            Envelope::from_bytes(&e.to_bytes()).is_err(),
            "a group message names its state"
        );
        e.gstate = Some(h.clone());
        let back = Envelope::from_bytes(&e.to_bytes()).unwrap();
        assert_eq!(
            (back.group.as_deref(), back.gstate.as_deref()),
            (Some(g.as_str()), Some(h.as_str()))
        );
        let mut upper = e.clone();
        upper.group = Some("AB".repeat(16));
        assert!(Envelope::from_bytes(&upper.to_bytes()).is_err());
        let mut sync = Envelope::sync("{}").unwrap();
        sync.group = Some(g.clone());
        assert!(
            Envelope::from_bytes(&sync.to_bytes()).is_err(),
            "own-device sync is never a group message"
        );
        let mut ctl = Envelope::group_control(Kind::GroupState, &g, "[]").unwrap();
        ctl.intro = Some("spike2:abcd".into());
        assert!(
            Envelope::from_bytes(&ctl.to_bytes()).is_ok(),
            "an invite may introduce its sender"
        );
        ctl.group = None;
        assert!(Envelope::from_bytes(&ctl.to_bytes()).is_err());
        assert!(
            !String::from_utf8(Envelope::text("x").unwrap().to_bytes())
                .unwrap()
                .contains("group"),
            "absent unless set"
        );
    }

    #[test]
    fn an_introduction_is_text_only_and_bounded() {
        let mut e = Envelope::text("hi, I am new").unwrap();
        assert!(
            !String::from_utf8(e.to_bytes()).unwrap().contains("intro"),
            "absent unless set"
        );
        e.intro = Some("spike2:abcd".into());
        assert_eq!(
            Envelope::from_bytes(&e.to_bytes())
                .unwrap()
                .intro
                .as_deref(),
            Some("spike2:abcd")
        );
        let mut big = e.clone();
        big.intro = Some("a".repeat(MAX_INTRO + 1));
        assert!(Envelope::from_bytes(&big.to_bytes()).is_err());
        let mut ctrl = e.clone();
        ctrl.intro = Some(
            "spike2:ab
cd"
            .into(),
        );
        assert!(Envelope::from_bytes(&ctrl.to_bytes()).is_err());
        // Edits, deletes and sync messages never carry one.
        let mut edit = Envelope::edit(&e.id, "x").unwrap();
        edit.intro = Some("spike2:abcd".into());
        assert!(Envelope::from_bytes(&edit.to_bytes()).is_err());
    }

    #[test]
    fn roundtrip_and_unique_ids() {
        let a = Envelope::text("hello").unwrap();
        let b = Envelope::text("hello").unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(Envelope::from_bytes(&a.to_bytes()).unwrap(), a);
        assert!(a.ts > 1_700_000_000_000, "timestamp is in milliseconds");
    }

    #[test]
    fn rejects_malformed_unknown_and_oversize() {
        let good = Envelope::text("x").unwrap();
        let mut bad_v = good.clone();
        bad_v.v = 2;
        let mut bad_id = good.clone();
        bad_id.id = "not-hex".into();
        let mut bad_to = good.clone();
        bad_to.to = Some("bob".into());
        for e in [bad_v, bad_id, bad_to] {
            assert!(Envelope::from_bytes(&e.to_bytes()).is_err());
        }
        let unknown_kind =
            br#"{"v":1,"id":"00112233445566778899aabbccddeeff","ts":1,"kind":"payment","body":""}"#;
        assert!(Envelope::from_bytes(unknown_kind).is_err());
        assert!(Envelope::from_bytes(b"plain old text").is_err());
        assert!(matches!(
            Envelope::text(&"a".repeat(MAX_TEXT + 1)),
            Err(Error::TooLarge { .. })
        ));
    }

    #[test]
    fn recipient_field_roundtrips_and_is_optional() {
        let mut e = Envelope::text("x").unwrap();
        e.to = Some("ab".repeat(32));
        assert_eq!(Envelope::from_bytes(&e.to_bytes()).unwrap(), e);
        let old =
            br#"{"v":1,"id":"00112233445566778899aabbccddeeff","ts":1,"kind":"text","body":"hi"}"#;
        assert_eq!(
            Envelope::from_bytes(old).unwrap().to,
            None,
            "envelopes without the field still parse"
        );
    }

    #[test]
    fn attachment_descriptors_are_validated_on_receipt() {
        let (att, _) = crate::attachment::seal_file("a.txt", b"hello").unwrap();
        let mut e = Envelope::text("caption").unwrap();
        e.attachment = Some(att.clone());
        assert_eq!(
            Envelope::from_bytes(&e.to_bytes()).unwrap().attachment,
            Some(att.clone())
        );
        e.attachment = Some(crate::attachment::Attachment { chunks: 99, ..att });
        assert!(Envelope::from_bytes(&e.to_bytes()).is_err());
    }

    #[test]
    fn edit_and_delete_envelopes_validate() {
        let t = "ab".repeat(16);
        let e = Envelope::edit(&t, "fixed typo").unwrap();
        assert_eq!(Envelope::from_bytes(&e.to_bytes()).unwrap(), e);
        let d = Envelope::delete(&t).unwrap();
        assert_eq!(Envelope::from_bytes(&d.to_bytes()).unwrap(), d);
        // Missing, malformed or misplaced targets are refused.
        let mut no_target = e.clone();
        no_target.target = None;
        let mut bad_target = e.clone();
        bad_target.target = Some("zz".into());
        let mut text_with_target = Envelope::text("x").unwrap();
        text_with_target.target = Some(t.clone());
        let mut delete_with_body = d.clone();
        delete_with_body.body = "x".into();
        for bad in [no_target, bad_target, text_with_target, delete_with_body] {
            assert!(Envelope::from_bytes(&bad.to_bytes()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sync_envelopes_validate() {
        let s = Envelope::sync("{\"type\":\"read\"}").unwrap();
        assert_eq!(Envelope::from_bytes(&s.to_bytes()).unwrap(), s);
        let mut empty = s.clone();
        empty.body.clear();
        let mut with_target = s.clone();
        with_target.target = Some("ab".repeat(16));
        for bad in [empty, with_target] {
            assert!(Envelope::from_bytes(&bad.to_bytes()).is_err());
        }
    }

    #[test]
    fn fits_in_a_session_message() {
        let e = Envelope::text(&"a".repeat(MAX_TEXT)).unwrap();
        assert!(e.to_bytes().len() <= crate::session::MAX_BODY);
    }
}
