//! Client <-> relay protocol: JSON text frames over WebSocket. Blobs are hex.
//! The relay never receives sender identity inside a blob and never forwards
//! the sender's device id to the recipient.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WirePrekey {
    pub key: String,
    pub sig: String,
}

/// What an entry in a group mailbox is (ADR 0025). A `Commit` changes the group's membership:
/// the relay accepts exactly one per position (compare-and-append) and keeps it longest. A
/// `Message` carries group traffic: no ordering constraint, shorter retention.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    Message,
    Commit,
}

impl GroupKind {
    /// The byte signed and stored for this kind.
    pub fn byte(self) -> u8 {
        match self {
            Self::Message => 0,
            Self::Commit => 1,
        }
    }

    pub fn from_byte(b: i64) -> Option<Self> {
        match b {
            0 => Some(Self::Message),
            1 => Some(Self::Commit),
            _ => None,
        }
    }
}

/// A commit that closes its mailbox hands the group over to a new one. The new mailbox's key
/// signs `mailbox::create_bytes` to prove it exists.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WireSeal {
    pub mailbox: String,
    pub pubkey: String,
    pub sig: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WireEntry {
    pub seq: u64,
    pub kind: GroupKind,
    pub blob: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Answer to `Challenge`: Ed25519 public key and signature over the nonce.
    Auth {
        ed25519: String,
        sig: String,
    },
    /// Queue `blob` for the device with id `to`.
    Send {
        to: String,
        blob: String,
    },
    /// Recipient has durably handled delivery `id`; relay may delete it.
    Ack {
        id: String,
    },
    /// Upload signed one-time prekeys so others can start sessions while we are offline.
    PublishPrekeys {
        keys: Vec<WirePrekey>,
    },
    /// Replace our fallback prekey: a reusable last resort handed out only when
    /// no one-time prekeys remain.
    PublishFallback {
        key: WirePrekey,
    },
    /// Take one of `device`'s prekeys (each is handed out at most once).
    ClaimPrekey {
        device: String,
    },
    /// Publish our person's signed device list (JSON). The relay checks the
    /// signature, that the publishing device is in it, and that the version
    /// only goes up.
    PublishDeviceList {
        list: String,
    },
    /// Fetch the latest signed device list for an owner key (hex).
    FetchDeviceList {
        owner: String,
    },
    /// Store one encrypted attachment chunk. The first chunk of a blob fixes
    /// its uploader and chunk count; later chunks must match.
    PutBlobChunk {
        blob: String,
        index: u32,
        total: u32,
        data: String,
    },
    /// Fetch one chunk of a blob (anyone who knows the random blob id may).
    GetBlobChunk {
        blob: String,
        index: u32,
    },
    /// Upload MLS key packages (opaque to the relay) so others can add this device to a group
    /// while it is offline. Each is handed out once.
    PublishKeyPackages {
        packages: Vec<String>,
    },
    /// Replace this device's *last-resort* key package: reusable, handed out only when no
    /// one-time package is left, so a drained device stays addable.
    PublishLastResortKeyPackage {
        package: String,
    },
    /// Take one of `device`'s key packages (each one-time package at most once).
    ClaimKeyPackage {
        device: String,
    },
    /// Register a group mailbox (ADR 0025): its id and the public key every request to it must be
    /// signed with. `sig` is the new key signing `mailbox::create_bytes`.
    GroupCreate {
        mailbox: String,
        pubkey: String,
        sig: String,
    },
    /// Append one entry. A `Commit` must say which position it expects (`expected_seq`, the
    /// mailbox's next position as the sender last saw it) and is refused with `GroupConflict` if
    /// another commit got there first; it may also `seal` the mailbox and open its successor.
    GroupAppend {
        mailbox: String,
        kind: GroupKind,
        #[serde(default)]
        expected_seq: Option<u64>,
        blob: String,
        #[serde(default)]
        seal: Option<WireSeal>,
        sig: String,
    },
    /// Read up to `limit` entries after position `after_seq`, oldest first.
    GroupRead {
        mailbox: String,
        after_seq: u64,
        limit: u32,
        sig: String,
    },
    /// Be pushed every entry appended to this mailbox from now on, on this connection. Read from
    /// your own cursor afterwards: pushes carry their position, so nothing is lost or doubled.
    GroupSubscribe {
        mailbox: String,
        sig: String,
    },
    GroupUnsubscribe {
        mailbox: String,
    },
    /// Ask for short-lived credentials for the relay operator's TURN server (ADR 0028). The relay answers
    /// `TurnCredentials`, or `Error` with code `no_turn` if it has none configured.
    TurnCredentials,
    /// Keepalive: the relay answers `Pong`. A client waiting for deliveries sends one now
    /// and then, so a connection that died silently (a router dropped it) is noticed, and
    /// so the relay can close connections that have gone quiet.
    Ping,
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
    /// Reply to `PutBlobChunk`.
    BlobChunkStored {
        blob: String,
        index: u32,
    },
    /// Reply to `GetBlobChunk`; `data` is `None` if the chunk is unknown or expired.
    BlobChunk {
        blob: String,
        index: u32,
        data: Option<String>,
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
    /// Reply to `PublishKeyPackages`: how many one-time packages the relay now holds for us.
    KeyPackagesPublished {
        count: usize,
    },
    /// Reply to `PublishLastResortKeyPackage`.
    LastResortKeyPackagePublished,
    /// Reply to `ClaimKeyPackage`; `package` is `None` when the device has none at all.
    /// `last_resort` is true when it is the reusable one.
    KeyPackage {
        device: String,
        package: Option<String>,
        last_resort: bool,
    },
    /// Reply to `GroupCreate`.
    GroupCreated {
        mailbox: String,
    },
    /// Reply to `GroupAppend`: the entry's position.
    GroupAppended {
        mailbox: String,
        seq: u64,
    },
    /// Reply to a `Commit` that did not get the position it expected: try again from `next_seq`.
    GroupConflict {
        mailbox: String,
        next_seq: u64,
    },
    /// Reply to `GroupRead`. `next_seq` is the position the next append will get; `sealed` says
    /// the mailbox was closed by a commit that opened another.
    GroupEntries {
        mailbox: String,
        entries: Vec<WireEntry>,
        next_seq: u64,
        sealed: bool,
    },
    /// Reply to `GroupSubscribe`.
    GroupSubscribed {
        mailbox: String,
        next_seq: u64,
        sealed: bool,
    },
    /// Pushed to subscribers when an entry is appended.
    GroupEntry {
        mailbox: String,
        entry: WireEntry,
        sealed: bool,
    },
    /// Reply to `TurnCredentials`: where the TURN server is, and a login that works until `ttl_secs` from now.
    TurnCredentials {
        urls: Vec<String>,
        username: String,
        credential: String,
        ttl_secs: u64,
    },
    /// Reply to `Ping`.
    Pong,
    Error {
        code: String,
        message: String,
    },
}
