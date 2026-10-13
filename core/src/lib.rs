//! Spike Connect core library.
//!
//! Plaintext only ever exists inside this crate's callers; everything that
//! leaves a device is produced by [`session::Sessions::encrypt`] and is opaque
//! to relays. Cryptography is delegated to `vodozemac` (Olm: X3DH-style
//! asynchronous key agreement + Double Ratchet, externally audited).

pub mod identity;
pub mod session;
pub mod wire;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid device card: {0}")]
    BadCard(&'static str),
    #[error("signature verification failed")]
    BadSignature,
    #[error("message too large (max {max} bytes)")]
    TooLarge { max: usize },
    #[error("no session with this contact; a one-time prekey is needed")]
    NoSession,
    #[error("malformed message")]
    Malformed,
    #[error("session error: {0}")]
    Session(String),
    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
