//! Spike Connect core library (Phase 0).
//!
//! Plaintext only ever exists inside this crate's callers; everything that
//! leaves a device is produced by [`session::seal`] and is opaque to relays.

pub mod identity;
pub mod session;
pub mod wire;

/// Noise pattern used for sealed messages. `X` is a one-way pattern: the sender
/// is authenticated by its static key and the recipient's static key is known
/// in advance, so a message can be delivered while the recipient is offline.
pub const NOISE_PARAMS: &str = "Noise_X_25519_ChaChaPoly_BLAKE2s";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid device card: {0}")]
    BadCard(&'static str),
    #[error("signature verification failed")]
    BadSignature,
    #[error("message too large (max {max} bytes)")]
    TooLarge { max: usize },
    #[error("malformed message")]
    Malformed,
    #[error("noise: {0}")]
    Noise(#[from] snow::Error),
    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
