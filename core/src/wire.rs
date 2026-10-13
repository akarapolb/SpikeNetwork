//! Client <-> relay protocol: JSON text frames over WebSocket. Blobs are hex.
//! The relay never receives sender identity inside a blob and never forwards
//! the sender's device id to the recipient.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WirePrekey {
    pub key: String,
    pub sig: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Answer to `Challenge`: Ed25519 public key and signature over the nonce.
    Auth { ed25519: String, sig: String },
    /// Queue `blob` for the device with id `to`.
    Send { to: String, blob: String },
    /// Recipient has durably handled delivery `id`; relay may delete it.
    Ack { id: String },
    /// Upload signed one-time prekeys so others can start sessions while we are offline.
    PublishPrekeys { keys: Vec<WirePrekey> },
    /// Take one of `device`'s prekeys (each is handed out at most once).
    ClaimPrekey { device: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Challenge {
        nonce: String,
    },
    /// `prekeys` is how many of our prekeys the relay still holds.
    Ready {
        device_id: String,
        prekeys: usize,
    },
    Accepted {
        id: String,
    },
    Deliver {
        id: String,
        blob: String,
    },
    /// Reply to `PublishPrekeys`: prekeys now stored.
    Published {
        prekeys: usize,
    },
    /// Reply to `ClaimPrekey`; `prekey` is `None` when the device has none left.
    Prekey {
        device: String,
        prekey: Option<WirePrekey>,
    },
    Error {
        code: String,
        message: String,
    },
}
