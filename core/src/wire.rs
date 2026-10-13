//! Client <-> relay protocol: JSON text frames over WebSocket. Blobs are hex.
//! The relay never receives sender identity inside a blob and never forwards
//! the sender's device id to the recipient.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Answer to `Challenge`: Ed25519 public key and signature over the nonce.
    Auth { ed25519: String, sig: String },
    /// Queue `blob` for the device with id `to`.
    Send { to: String, blob: String },
    /// Recipient has durably handled delivery `id`; relay may delete it.
    Ack { id: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Challenge { nonce: String },
    Ready { device_id: String },
    Accepted { id: String },
    Deliver { id: String, blob: String },
    Error { code: String, message: String },
}
