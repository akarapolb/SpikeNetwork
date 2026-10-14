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
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub v: u8,
    /// 128-bit random id, lowercase hex. Globally unique in practice.
    pub id: String,
    /// Sender's clock, milliseconds since the Unix epoch. Display only; never trusted for ordering security.
    pub ts: u64,
    pub kind: Kind,
    pub body: String,
    /// The conversation's other person (their owner key, hex). Set on every
    /// message so that copies delivered to the sender's *own* other devices
    /// know which conversation they belong to. Ignored on messages from others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// A file sent with this message (the body is its caption, possibly empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<crate::attachment::Attachment>,
}

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
            body: body.to_string(),
            to: None,
            attachment: None,
        })
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
        if e.v != VERSION || !id_ok || !to_ok || !att_ok || e.body.len() > MAX_TEXT {
            return Err(Error::Malformed);
        }
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fits_in_a_session_message() {
        let e = Envelope::text(&"a".repeat(MAX_TEXT)).unwrap();
        assert!(e.to_bytes().len() <= crate::session::MAX_BODY);
    }
}
