//! App channels (ADR 0030): bytes that another application sends over Spike. Spike learns nothing
//! about them; it checks only that they name an application and a channel, and carries them.
//!
//! The application's id, the channel and the bytes all travel inside the encrypted envelope, as
//! its JSON body, so a relay never sees which application or channel a delivery belongs to.

use base64::Engine;
use serde::{Deserialize, Serialize};

/// Longest application id, in bytes (reverse-DNS style, `work.center`).
pub const MAX_APP_ID: usize = 64;
/// Longest channel name, in bytes.
pub const MAX_CHANNEL: usize = 64;
/// Most bytes one app envelope carries. Bigger data (a whole file's state) goes as a snapshot.
pub const MAX_APP_DATA: usize = 32 * 1024;
/// A `live` envelope older than this when it arrives is dropped: it was about a moment that passed.
pub const LIVE_MAX_AGE_MS: u64 = 30_000;

/// How an app envelope is delivered.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Kept until the application takes it, like a message (but never shown as chat).
    Stored,
    /// Only for whoever is there right now; never kept (a cursor position, "is typing").
    Live,
}

/// The body of a `Kind::App` envelope.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AppData {
    pub app: String,
    pub channel: String,
    pub mode: Mode,
    /// The application's bytes, base64 (standard alphabet, padded).
    pub data: String,
}

/// The body of a `Kind::AppStateRequest` (ask for a channel's whole state) and of a `Kind::AppState`
/// (the answer; the snapshot itself is the envelope's attachment).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AppChannel {
    pub app: String,
    pub channel: String,
}

/// `work.center`: lowercase letters, digits, `-` and dots, at least one dot, no empty part.
pub fn valid_app_id(s: &str) -> bool {
    s.len() <= MAX_APP_ID
        && s.contains('.')
        && s.split('.').all(|p| {
            !p.is_empty()
                && !p.starts_with('-')
                && p.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

/// Any name the application likes (a file id), 1 to 64 bytes, no control characters.
pub fn valid_channel(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_CHANNEL && !s.chars().any(char::is_control)
}

impl AppData {
    pub fn new(app: &str, channel: &str, mode: Mode, bytes: &[u8]) -> crate::Result<Self> {
        if bytes.len() > MAX_APP_DATA {
            return Err(crate::Error::TooLarge { max: MAX_APP_DATA });
        }
        let d = Self {
            app: app.to_string(),
            channel: channel.to_string(),
            mode,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        d.bytes()?;
        Ok(d)
    }

    /// The application's bytes, after checking the names and the size.
    pub fn bytes(&self) -> crate::Result<Vec<u8>> {
        if !valid_app_id(&self.app) || !valid_channel(&self.channel) {
            return Err(crate::Error::Malformed);
        }
        let b = base64::engine::general_purpose::STANDARD
            .decode(&self.data)
            .map_err(|_| crate::Error::Malformed)?;
        if b.len() > MAX_APP_DATA {
            return Err(crate::Error::TooLarge { max: MAX_APP_DATA });
        }
        Ok(b)
    }
}

impl AppChannel {
    pub fn new(app: &str, channel: &str) -> crate::Result<Self> {
        let c = Self {
            app: app.to_string(),
            channel: channel.to_string(),
        };
        if !c.valid() {
            return Err(crate::Error::Malformed);
        }
        Ok(c)
    }

    pub fn valid(&self) -> bool {
        valid_app_id(&self.app) && valid_channel(&self.channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_look_like_reverse_dns() {
        for ok in ["work.center", "com.example.sheet-2", "a.b"] {
            assert!(valid_app_id(ok), "{ok}");
        }
        for bad in [
            "",
            "work",
            "Work.Center",
            "work..center",
            ".work",
            "work.",
            "work.-x",
            "work center.x",
            &format!("{}.x", "a".repeat(MAX_APP_ID)),
        ] {
            assert!(!valid_app_id(bad), "{bad}");
        }
    }

    #[test]
    fn channels_are_short_printable_names() {
        assert!(valid_channel("sheet:4f2a/B12 ✓"));
        assert!(!valid_channel(""));
        assert!(!valid_channel("a\nb"));
        assert!(!valid_channel(&"x".repeat(MAX_CHANNEL + 1)));
    }

    #[test]
    fn data_roundtrips_and_is_bounded() {
        let d = AppData::new("work.center", "doc1", Mode::Live, b"\x00\x01hi").unwrap();
        assert_eq!(d.bytes().unwrap(), b"\x00\x01hi");
        assert!(AppData::new("work.center", "doc1", Mode::Stored, &[0; MAX_APP_DATA]).is_ok());
        assert!(AppData::new("work.center", "doc1", Mode::Stored, &[0; MAX_APP_DATA + 1]).is_err());
        assert!(AppData::new("nodot", "doc1", Mode::Stored, b"x").is_err());
        let mut bad = d.clone();
        bad.data = "not base64!".into();
        assert!(bad.bytes().is_err());
    }
}
