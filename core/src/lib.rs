//! Spike Connect core library.
//!
//! Plaintext only ever exists inside this crate's callers; everything that
//! leaves a device is produced by [`session::Sessions::encrypt`] and is opaque
//! to relays. Cryptography is delegated to `vodozemac` (Olm: X3DH-style
//! asynchronous key agreement + Double Ratchet, externally audited).

pub mod action;
pub mod agent;
pub mod attachment;
pub mod envelope;
pub mod identity;
pub mod link;
pub mod merge;
pub mod owner;
pub mod session;
pub mod vault;
pub mod verifier;
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
    #[error("invalid device list: {0}")]
    BadDeviceList(&'static str),
    #[error("device list rollback refused: already saw version {seen}, offered {offered}")]
    Rollback { seen: u64, offered: u64 },
    #[error("two different device lists both claim version {0}: possible equivocation, refused")]
    Equivocation(u64),
    #[error("identity key changed")]
    KeyChange,
    #[error("wrong passphrase or corrupted vault")]
    WrongPassphrase,
    #[error("stored data is corrupted or was tampered with")]
    Corrupted,
    #[error("session error: {0}")]
    Session(String),
    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
