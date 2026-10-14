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
    /// Replace our fallback prekey: a reusable last resort handed out only when
    /// no one-time prekeys remain.
    PublishFallback { key: WirePrekey },
    /// Take one of `device`'s prekeys (each is handed out at most once).
    ClaimPrekey { device: String },
    /// Publish our person's signed device list (JSON). The relay checks the
    /// signature, that the publishing device is in it, and that the version
    /// only goes up.
    PublishDeviceList { list: String },
    /// Fetch the latest signed device list for an owner key (hex).
    FetchDeviceList { owner: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Challenge {
        nonce: String,
    },
    /// `prekeys` is how many of our one-time prekeys the relay still holds;
    /// `fallback_age_secs` is the age of our stored fallback prekey, if any.
    Ready {
        device_id: String,
        prekeys: usize,
        #[serde(default)]
        fallback_age_secs: Option<u64>,
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
    /// Reply to `PublishFallback`.
    FallbackPublished,
    /// Reply to `PublishDeviceList`.
    DeviceListStored {
        version: u64,
    },
    /// Reply to `FetchDeviceList`; `list` is the signed list JSON, if the relay has one.
    DeviceList {
        owner: String,
        list: Option<String>,
    },
    /// Reply to `ClaimPrekey`; `prekey` is `None` when the device has none at
    /// all. `fallback` is true when it is the reusable fallback, not a one-time key.
    Prekey {
        device: String,
        prekey: Option<WirePrekey>,
        #[serde(default)]
        fallback: bool,
    },
    Error {
        code: String,
        message: String,
    },
}
