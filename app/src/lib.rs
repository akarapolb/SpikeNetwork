//! Everything the desktop UI can do, as plain functions over a [`Session`].
//! The Tauri commands in `lib.rs` are one-line wrappers, so this module is
//! tested without a window (see `tests/`). See `docs/adr/0019-tauri-desktop-shell.md`.
//!
//! Keys never leave this process: the UI only gets names, message text and
//! request summaries. All network operations are serialized by one lock,
//! because `send` and `recv` each load and save the ratchet state and would
//! corrupt it if they overlapped.

pub mod apps;
pub mod backup;
pub mod bridge;
mod calls;
mod direct;
pub mod local_api;
pub use calls::{CallEventDto, IceServerDto};
pub use direct::DirectDto;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde::Serialize;
use spike_cli::agent::{self, Status};
use spike_cli::client::Relay;
use spike_cli::groups;
use spike_cli::large;
use spike_cli::link;
use spike_cli::messages::Direction;
use spike_cli::requests;
use spike_cli::state::{Added, Home};
use spike_cli::transport::{validate_ca_pem, Target, TransportOpts};
use spike_cli::{ops, Outcome};
use spike_core::action;
use spike_core::agent::{DecidedBy, Decision};
use spike_core::group::{AddBy, GroupState, History, MAX_MEMBERS};
use spike_core::link::LinkSession;
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zeroize::Zeroizing;

pub const MIN_PASSPHRASE_CHARS: usize = 10;
pub const DEFAULT_RELAY: &str = "ws://127.0.0.1:8787";
const RELAY_FILE: &str = "desktop-relay.enc";
const RELAY_SETTINGS: &str = "desktop-relay2.enc";
/// The trusted certificate, written from the sealed settings each time the home is opened.
const CA_FILE: &str = "relay-ca.pem";
const MAX_CA_PEM: usize = 64 * 1024;
const MAX_NAME: usize = 64;
const MAX_MESSAGE: usize = 16 * 1024;

/// Which relay to use and how far to trust it. Sealed in the home with everything else.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
struct RelaySettings {
    url: String,
    /// An extra trusted CA certificate (PEM) for a self-hosted relay.
    ca_pem: Option<String>,
    /// Allow plain `ws://` to a non-loopback address. Content stays end-to-end
    /// encrypted; the connection and metadata are exposed.
    allow_insecure: bool,
}

/// An unlocked state directory plus the relay it talks to.
pub struct Session {
    home: Arc<Home>,
    relay: Arc<Mutex<RelaySettings>>,
    net: Arc<tokio::sync::Mutex<()>>,
    /// Direct connections on the local network (ADR 0027).
    direct: direct::State,
    /// Calls in progress (ADR 0028).
    calls: Arc<calls::Hub>,
    /// Live app events and waiters for app events (ADR 0030).
    apps: Arc<apps::Hub>,
    /// Set while this device waits to be linked: nothing else may read the relay mailbox.
    paused: AtomicBool,
    /// This (new) device is waiting to be linked: its code and, once the
    /// existing device answered, the safety code to compare.
    link_new: Mutex<Option<(link::Waiting, Option<LinkSession>)>>,
    /// This (existing) device compared codes with a new device and is about to share.
    link_old: Mutex<Option<link::Pending>>,
    /// Key packages for large groups were topped up since this session started.
    kp_checked: AtomicBool,
    /// Poked when the large groups we listen to change (joined, left, a mailbox rotated), so the
    /// listener reconnects and subscribes to the right ones.
    subs: std::sync::Arc<tokio::sync::Notify>,
}

fn target_for(rs: &RelaySettings, home: &Home) -> Target {
    Target {
        url: rs.url.clone(),
        opts: TransportOpts {
            ca_file: rs.ca_pem.is_some().then(|| home.path().join(CA_FILE)),
            allow_insecure: rs.allow_insecure,
        },
    }
}

/// Count one outcome into a sync's totals. True if it changes what the screen shows.
pub(crate) fn tally(out: &mut SyncDto, o: &Outcome) -> bool {
    match o {
        Outcome::Message { .. } | Outcome::SentCopy { .. } => out.received += 1,
        Outcome::AgentRequest { verdict, .. } if verdict == "pending" => out.agent_requests += 1,
        Outcome::AgentRemotePending { .. } => out.agent_requests += 1,
        Outcome::MessageRequest { new: true, .. } => out.message_requests += 1,
        Outcome::GroupMessage { .. } => out.received += 1,
        Outcome::Edited { .. } | Outcome::Deleted { .. } | Outcome::Reacted { .. } => {
            out.changed += 1
        }
        Outcome::GroupInvite { .. } => out.message_requests += 1,
        Outcome::GroupJoined { .. }
        | Outcome::GroupChanged { .. }
        | Outcome::GroupHistory { .. }
        | Outcome::LargeInvite { .. }
        | Outcome::GroupInviteSettled { .. } => out.groups += 1,
        Outcome::GroupRemoved { group, .. } => {
            out.groups += 1;
            out.notes.push(format!(
                "You were removed from {group}. Its history stays on this computer."
            ));
        }
        Outcome::UnknownSender { .. } => out.notes.push(
            "A message from someone who is not in your contacts and did not introduce themselves was dropped."
                .into(),
        ),
        Outcome::Rejected { reason } => out.notes.push(format!("Ignored a change: {reason}")),
        _ => return false,
    }
    true
}

// ------------------------------------------------------------------ DTOs

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeviceDto {
    /// First characters of the device id: enough to tell devices apart.
    pub id: String,
    pub this: bool,
    /// The first device in the signed list decides agent requests.
    pub approver: bool,
    pub relay: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub fingerprint: String,
    pub card: String,
    pub devices: usize,
    pub device_list: Vec<DeviceDto>,
    pub list_version: u64,
    pub approver: bool,
    /// This device was removed from your identity by another device.
    pub removed: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct InstalledDto {
    pub version: u64,
    pub contacts: usize,
    pub messages: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct LinkedDto {
    pub version: u64,
    pub contacts: usize,
    pub messages: usize,
    pub files: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ActionTypeDto {
    pub name: String,
    pub fields: Vec<String>,
    pub money: bool,
    pub template: String,
}

/// What the UI sends to create a grant. Amounts are in the smallest unit (cents).
#[derive(Deserialize, Debug, Clone)]
pub struct GrantRequest {
    pub agent: String,
    pub scopes: Vec<String>,
    pub days: u64,
    pub max_cents: Option<u64>,
    pub auto_below_cents: Option<u64>,
    pub per_hour: u32,
    pub daily_cents: Option<u64>,
    pub typed_only: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ContactDto {
    pub name: String,
    pub verified: bool,
    pub fingerprint: String,
    pub devices: usize,
    pub unread: usize,
    pub agent: bool,
    pub last: Option<String>,
    pub last_ts: Option<u64>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub me: Me,
    pub contacts: Vec<ContactDto>,
    /// Groups you are in, were invited to, or left but still have a history for (ADR 0024).
    pub groups: Vec<GroupDto>,
    pub waiting: usize,
    /// Message requests from strangers, waiting for your decision (ADR 0021).
    pub requests: usize,
    pub relay: String,
    pub relay_trust: RelayTrustDto,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GroupDto {
    /// The group id (hex). Pass it back to the other group calls.
    pub id: String,
    pub name: String,
    pub members: usize,
    pub unread: usize,
    /// `joined`, `invited` (nothing is shown until you accept) or `left` (history only).
    pub status: String,
    /// Who added you, while `invited`.
    pub invited_by: Option<String>,
    /// Members (not you) who are not verified contacts.
    pub unverified: usize,
    /// A large group (MLS, ADR 0025) rather than a small one.
    pub large: bool,
    pub last: Option<String>,
    pub last_ts: Option<u64>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MemberDto {
    /// Owner key (hex): what remove and make-admin take.
    pub owner: String,
    /// "you", a contact's name, or "~" and the start of their fingerprint.
    pub name: String,
    pub you: bool,
    pub admin: bool,
    pub contact: bool,
    pub verified: bool,
    pub fingerprint: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GroupDetailDto {
    pub id: String,
    pub name: String,
    pub status: String,
    pub invited_by: Option<String>,
    pub members: Vec<MemberDto>,
    pub me_admin: bool,
    /// You may add people now (you are in it, the settings allow it, and there is room).
    pub can_add: bool,
    /// `admins` or `any`.
    pub add_by: String,
    pub version: u64,
    pub max: usize,
    pub unverified: usize,
    /// What new members see: `join` (messages from when they join) or `week` (the last 7 days).
    pub history: String,
    /// An invite from someone who is not a contact: it can be declined *and* the inviter blocked.
    pub can_block: bool,
    /// A large group (MLS, ADR 0025): no files or history choice yet.
    pub large: bool,
    /// Changes every member refused (they broke a rule); they are skipped, the group carries on.
    pub refused: u64,
    /// Why the latest one was refused.
    pub refused_why: Option<String>,
}

/// What a group change or message reached.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GroupSendDto {
    pub group: String,
    /// Other members who got it on at least one device, and how many others there are.
    pub reached: usize,
    pub people: usize,
    pub notes: Vec<String>,
}

/// One text message held for a message request. Shown only when you ask to see it.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct HeldDto {
    pub ts: u64,
    pub body: String,
    /// The name of a file that came with it and was not kept.
    pub file: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MessageRequestDto {
    pub id: String,
    pub fingerprint: String,
    pub received: u64,
    pub held: Vec<HeldDto>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BlockedDto {
    pub owner: String,
    pub fingerprint: String,
    pub at: u64,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct RelayTrustDto {
    /// How many extra CA certificates are trusted (0 = only the built-in roots).
    pub ca_certs: usize,
    pub allow_insecure: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FileDto {
    pub name: String,
    pub size: u64,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MessageDto {
    pub id: String,
    pub out: bool,
    pub ts: u64,
    pub body: String,
    pub edited: bool,
    pub deleted: bool,
    pub file: Option<FileDto>,
    /// In a group: who wrote it (not you). `None` in a one-to-one chat.
    pub from: Option<String>,
    /// In a group: the author is a verified contact. Always true in a one-to-one chat.
    pub from_verified: bool,
    /// In a group: this came as history from someone else when you joined (their name), so the
    /// author shown is only as reliable as they are.
    pub shared_by: Option<String>,
    /// The message this one replies to, as a short quote. `None` when it is not a reply, or the
    /// message it replies to is not on this device.
    pub reply: Option<ReplyDto>,
    /// Reactions, most used first: each emoji once, with how many people chose it.
    pub reactions: Vec<ReactionDto>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ReplyDto {
    pub id: String,
    /// You wrote the message being replied to.
    pub out: bool,
    /// In a group: who wrote it (not you).
    pub from: Option<String>,
    /// The start of its text (or its file name); empty if it was deleted.
    pub text: String,
    pub deleted: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ReactionDto {
    pub emoji: String,
    pub count: usize,
    /// One of them is yours.
    pub mine: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SyncDto {
    pub received: usize,
    pub agent_requests: usize,
    /// New message requests from strangers.
    pub message_requests: usize,
    /// Group changes, joins and removals.
    pub groups: usize,
    /// Messages already shown that changed: edited, deleted, or reacted to.
    pub changed: usize,
    pub notes: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GrantDto {
    pub agent: String,
    pub scopes: Vec<String>,
    pub max_cents: Option<u64>,
    pub auto_below_cents: Option<u64>,
    pub per_hour: u32,
    pub daily_cents: Option<u64>,
    pub typed_only: bool,
    pub expires: u64,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct RequestDto {
    pub id: String,
    pub from: String,
    pub scope: String,
    /// The agent's own words. Never the headline for a typed request.
    pub note: String,
    pub amount_cents: Option<u64>,
    /// The line rendered from the signed typed fields; `None` for free text.
    pub summary: Option<String>,
    pub typed: Option<BTreeMap<String, String>>,
    /// `waiting`, `approved` or `denied`.
    pub status: String,
    /// `you` or `policy` once decided.
    pub by: Option<String>,
    pub received: u64,
    pub expires: u64,
    /// Waiting on another of your devices (the approver) to record it.
    pub forwarded: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentsDto {
    pub approver: bool,
    pub grants: Vec<GrantDto>,
    pub requests: Vec<RequestDto>,
}

// -------------------------------------------------------------- lifecycle

/// Does this directory hold an identity?
pub fn exists(dir: &Path) -> bool {
    Home::exists(dir)
}

fn check_passphrase(p: &str) -> Result<()> {
    if p.chars().count() < MIN_PASSPHRASE_CHARS {
        bail!("passphrase must be at least {MIN_PASSPHRASE_CHARS} characters");
    }
    Ok(())
}

impl Session {
    /// Create a new identity in `dir` and unlock it. Refuses to overwrite one.
    pub fn create(dir: &Path, passphrase: String, kdf: KdfParams) -> Result<Self> {
        let passphrase = Zeroizing::new(passphrase);
        check_passphrase(&passphrase)?;
        let home = Home::create(dir, &passphrase, kdf, false)?;
        Self::wrap(home)
    }

    pub fn unlock(dir: &Path, passphrase: String) -> Result<Self> {
        let passphrase = Zeroizing::new(passphrase);
        let home = Home::unlock(dir, &passphrase)?;
        Self::wrap(home)
    }

    fn wrap(home: Home) -> Result<Self> {
        let mut settings: RelaySettings = home.load_json(RELAY_SETTINGS)?;
        if settings.url.is_empty() {
            // Homes made before trust settings existed kept only the address.
            let old: String = home.load_json(RELAY_FILE)?;
            settings.url = if old.is_empty() {
                DEFAULT_RELAY.to_string()
            } else {
                old
            };
        }
        write_ca(&home, &settings)?;
        Ok(Self {
            home: Arc::new(home),
            relay: Arc::new(Mutex::new(settings)),
            net: Arc::new(tokio::sync::Mutex::new(())),
            direct: direct::State::default(),
            calls: Arc::default(),
            apps: Arc::default(),
            paused: AtomicBool::new(false),
            link_new: Mutex::new(None),
            link_old: Mutex::new(None),
            kp_checked: AtomicBool::new(false),
            subs: std::sync::Arc::new(tokio::sync::Notify::new()),
        })
    }

    pub fn relay(&self) -> String {
        self.relay.lock().expect("relay lock").url.clone()
    }

    fn settings(&self) -> RelaySettings {
        self.relay.lock().expect("relay lock").clone()
    }

    fn save_settings(&self, rs: RelaySettings) -> Result<()> {
        self.home.save_json(RELAY_SETTINGS, &rs)?;
        write_ca(&self.home, &rs)?;
        *self.relay.lock().expect("relay lock") = rs;
        Ok(())
    }

    pub fn set_relay(&self, url: &str) -> Result<()> {
        let url = url.trim();
        if !(url.starts_with("ws://") || url.starts_with("wss://"))
            || url.len() > 256
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            bail!("the relay address must look like wss://relay.example or ws://127.0.0.1:8787");
        }
        let mut rs = self.settings();
        rs.url = url.into();
        self.save_settings(rs)
    }

    /// Trust one more certificate authority (PEM text), or `None` to go back to the built-in
    /// roots only. Leaves the plain-ws setting alone. Returns how many certificates were trusted.
    pub fn set_relay_ca(&self, ca_pem: Option<String>) -> Result<usize> {
        let ca_pem = ca_pem
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        let certs = match &ca_pem {
            Some(p) => {
                if p.len() > MAX_CA_PEM {
                    bail!("that certificate file is too large");
                }
                validate_ca_pem(p)?
            }
            None => 0,
        };
        let mut rs = self.settings();
        rs.ca_pem = ca_pem;
        self.save_settings(rs)?;
        Ok(certs)
    }

    /// Allow (or stop allowing) plain `ws://` to a non-loopback address. Leaves the certificate alone.
    pub fn set_allow_insecure(&self, allow: bool) -> Result<()> {
        let mut rs = self.settings();
        rs.allow_insecure = allow;
        self.save_settings(rs)
    }

    fn target(&self) -> Target {
        target_for(&self.settings(), &self.home)
    }

    /// Connect and sign in to the relay, to show the person whether it works.
    pub async fn test_relay(&self) -> Result<String> {
        let me = self.home.load_identity()?;
        let target = self.target();
        let url = target.url.clone();
        let _net = self.net.lock().await;
        let probe = async {
            let mut relay = Relay::connect(target, &me).await?;
            relay.ping().await
        };
        match tokio::time::timeout(Duration::from_secs(15), probe).await {
            Err(_) => bail!("no answer from {url} within 15 seconds"),
            Ok(Err(e)) => Err(e.context(format!("could not connect to {url}"))),
            Ok(Ok(rtt)) => Ok(format!(
                "Connected to {url}{}. The relay answered in {} ms.",
                if url.starts_with("wss://") {
                    " (certificate verified)"
                } else {
                    ""
                },
                rtt.as_millis().max(1)
            )),
        }
    }

    // ------------------------------------------------------ live connection

    /// Open the connection the listener waits on. It takes the network lock only
    /// while connecting: any operation that runs later connects too, which makes
    /// the relay drop this one (one live connection per device), and the listener
    /// then waits for the lock before reconnecting.
    /// What the listener waits on besides the relay: poked when our large groups' mailboxes change.
    pub fn subscription_signal(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.subs.clone()
    }

    /// Ask the relay to push new entries of our large groups to the listener's connection, so a
    /// message in one is noticed as fast as a one-to-one message. Best effort: a group that cannot
    /// be subscribed to (too many, another relay) is still read by the periodic sync.
    pub async fn listener_subscribe(&self, relay: &mut Relay) {
        let url = self.settings().url;
        if let Ok(subs) = large::subscriptions(&self.home, &url) {
            for (id, key) in subs.into_iter().take(50) {
                let _ = relay.group_subscribe(&key, &id).await;
            }
        }
    }

    pub async fn listener_connect(&self) -> Result<Relay> {
        let me = self.home.load_identity()?;
        let target = self.target();
        let _net = self.net.lock().await;
        Relay::connect(target, &me).await
    }

    /// True while something (linking) needs the relay mailbox to itself.
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
    }

    /// Unread messages plus requests waiting for you: what the window title shows.
    pub fn attention(&self) -> Result<usize> {
        let unread: usize = ops::unread_counts(&self.home)?.iter().map(|(_, n)| n).sum();
        let waiting = self
            .agents()?
            .requests
            .iter()
            .filter(|r| r.status == "waiting")
            .count();
        let group_unread: usize = groups::unread_counts(&self.home)?
            .iter()
            .map(|(_, n)| n)
            .sum();
        let invites = groups::list(&self.home)?
            .iter()
            .filter(|r| r.status == groups::Status::Invited)
            .count();
        let large_unread: usize = large::unread_counts(&self.home)?
            .iter()
            .map(|(_, n)| n)
            .sum();
        let large_invites = large::list(&self.home)?
            .iter()
            .filter(|r| r.status == large::Status::Invited)
            .count();
        Ok(unread
            + group_unread
            + large_unread
            + invites
            + large_invites
            + waiting
            + requests::list(&self.home)?.len())
    }

    // ------------------------------------------------------------ read side

    fn device_id(&self) -> Result<String> {
        Ok(self.home.load_identity()?.device_id())
    }

    pub fn overview(&self) -> Result<Overview> {
        let card = self.home.identity_card()?;
        let me = IdentityCard::decode(&card)?;
        let my_id = self.device_id()?;
        let approver = agent::is_approver(&self.home, &my_id)?;
        let device_list: Vec<DeviceDto> = me
            .device_relays()
            .into_iter()
            .enumerate()
            .map(|(i, (d, relay))| {
                let id = d.device_id();
                DeviceDto {
                    this: id == my_id,
                    approver: i == 0,
                    id: id.chars().take(12).collect(),
                    relay,
                }
            })
            .collect();
        let unread: BTreeMap<String, usize> = ops::unread_counts(&self.home)?.into_iter().collect();
        let grants = agent::list_grants(&self.home)?;
        let store = self.home.messages()?;
        let mut contacts = Vec::new();
        for c in self.home.load_contacts()? {
            let id = c.identity()?;
            let key = id.owner_hex();
            let last = store.conversation(&key, usize::MAX)?.into_iter().last();
            contacts.push(ContactDto {
                unread: unread.get(&c.name).copied().unwrap_or(0),
                agent: grants.iter().any(|g| g.body.agent == key),
                verified: c.verified,
                fingerprint: id.fingerprint(),
                devices: id.devices().len(),
                last: last.as_ref().map(|m| {
                    if m.deleted {
                        "Message deleted".into()
                    } else {
                        let who = if m.direction == Direction::Out {
                            "You: "
                        } else {
                            ""
                        };
                        format!(
                            "{who}{}",
                            preview(&m.body, m.attachment.as_ref().map(|a| a.name.as_str()))
                        )
                    }
                }),
                last_ts: last.map(|m| m.ts),
                name: c.name,
            });
        }
        contacts.sort_by(|a, b| b.last_ts.cmp(&a.last_ts).then_with(|| a.name.cmp(&b.name)));
        let waiting = self
            .agents()?
            .requests
            .iter()
            .filter(|r| r.status == "waiting")
            .count();
        Ok(Overview {
            me: Me {
                fingerprint: me.fingerprint(),
                card,
                devices: me.devices().len(),
                device_list,
                list_version: me.version(),
                approver,
                removed: !self.home.this_device_is_listed()?,
            },
            contacts,
            groups: self.group_rows()?,
            waiting,
            requests: requests::list(&self.home)?.len(),
            relay: self.relay(),
            relay_trust: RelayTrustDto {
                ca_certs: match self.settings().ca_pem {
                    Some(p) => validate_ca_pem(&p).unwrap_or(0),
                    None => 0,
                },
                allow_insecure: self.settings().allow_insecure,
            },
        })
    }

    fn contact_key(&self, name: &str) -> Result<String> {
        self.home
            .load_contacts()?
            .into_iter()
            .find(|c| c.name == name)
            .with_context(|| format!("no contact '{name}'"))?
            .conversation_key()
    }

    pub fn conversation(&self, name: &str, limit: usize) -> Result<Vec<MessageDto>> {
        let key = self.contact_key(name)?;
        let me = self.my_owner()?;
        let msgs = self
            .home
            .messages()?
            .conversation(&key, limit.clamp(1, 500))?;
        let replies = self.replies_in(&key, &msgs, false)?;
        Ok(msgs
            .into_iter()
            .map(|m| {
                let reply = replies.get(&m.id).cloned();
                let mut dto = message_dto(m, None, true, &me);
                dto.reply = reply;
                dto
            })
            .collect())
    }

    /// For each message in `msgs` that replies to another message of the same conversation
    /// (`conv`), a short quote of that message. In a group (`group`), it says who wrote it.
    fn replies_in(
        &self,
        conv: &str,
        msgs: &[spike_cli::messages::StoredMessage],
        group: bool,
    ) -> Result<std::collections::HashMap<String, ReplyDto>> {
        let store = self.home.messages()?;
        let mut out = std::collections::HashMap::new();
        for m in msgs {
            let Some(target) = m.reply_to.as_deref() else {
                continue;
            };
            let t = match msgs.iter().find(|x| x.id == target) {
                Some(t) => Some(t.clone()),
                None => store.get(target)?,
            };
            let Some(t) = t.filter(|t| t.peer == conv) else {
                continue;
            };
            let from = if group && t.direction == Direction::In {
                t.author
                    .as_deref()
                    .map(|a| groups::display_name(&self.home, a))
            } else {
                None
            };
            let text = if t.deleted {
                String::new()
            } else {
                preview(&t.body, t.attachment.as_ref().map(|a| a.name.as_str()))
            };
            out.insert(
                m.id.clone(),
                ReplyDto {
                    id: t.id.clone(),
                    out: t.direction == Direction::Out,
                    from,
                    text,
                    deleted: t.deleted,
                },
            );
        }
        Ok(out)
    }

    // ----------------------------------------------------------- contacts

    pub fn add_contact(&self, name: &str, card: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control)
        {
            bail!("give the contact a name of 1 to {MAX_NAME} characters");
        }
        let card = card.trim();
        let new =
            IdentityCard::decode(card).map_err(|_| anyhow!("that is not a Spike identity card"))?;
        if new.owner_hex() == IdentityCard::decode(&self.home.identity_card()?)?.owner_hex() {
            bail!("that is your own card");
        }
        Ok(match self.home.add_contact(name, card, false)? {
            Added::New => format!(
                "{name} added, not verified. Compare {} with them over a second channel.",
                new.fingerprint()
            ),
            Added::AlreadyKnown => format!("{name} is already in your contacts."),
            Added::Updated { version } => {
                format!("Updated {name}'s devices (list v{version}). Verification kept.")
            }
            Added::Upgraded => format!("{name} upgraded to a signed identity. Verify again."),
            Added::Replaced => format!("{name}'s identity key changed. Verify again."),
        })
    }

    /// Mark a contact verified. `fingerprint` is what the person compared on
    /// screen; if the stored key changed since, this refuses.
    pub fn mark_verified(&self, name: &str, fingerprint: &str) -> Result<()> {
        let mut contacts = self.home.load_contacts()?;
        let c = contacts
            .iter_mut()
            .find(|c| c.name == name)
            .with_context(|| format!("no contact '{name}'"))?;
        let actual = c.identity()?.fingerprint();
        if actual != fingerprint.trim().to_lowercase() {
            bail!("that fingerprint does not match the stored key; not verified");
        }
        c.verified = true;
        self.home.save_contacts(&contacts)?;
        Ok(())
    }

    // ------------------------------------------------------------ network

    pub async fn send(&self, name: &str, text: &str) -> Result<usize> {
        self.send_reply(name, text, None).await
    }

    /// Send a message that replies to an earlier one of the conversation (`reply_to`, its id).
    pub async fn send_reply(
        &self,
        name: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<usize> {
        let text = text.trim_end();
        if text.trim().is_empty() {
            bail!("nothing to send");
        }
        if text.len() > MAX_MESSAGE {
            bail!("messages are limited to {} KiB", MAX_MESSAGE / 1024);
        }
        let _net = self.net.lock().await;
        let r = match reply_to {
            Some(t) => {
                let key = self.contact_key(name)?;
                self.reply_target(&key, t)?;
                ops::send_reply(&self.home, self.target(), name, t, text).await?
            }
            None => ops::send(&self.home, self.target(), name, text.as_bytes()).await?,
        };
        Ok(r.delivered())
    }

    /// A reply must point at a message of the same conversation that is still there.
    fn reply_target(&self, conv: &str, id: &str) -> Result<()> {
        match self.home.messages()?.get(id)? {
            Some(m) if m.peer == conv && !m.deleted => Ok(()),
            _ => bail!("the message you are replying to is not in this conversation"),
        }
    }

    /// React to a message with one emoji, or take your reaction back (`emoji` empty).
    pub async fn react(&self, id: &str, emoji: &str) -> Result<()> {
        let emoji = emoji.trim();
        if emoji.len() > spike_core::envelope::MAX_REACTION
            || emoji.chars().any(char::is_whitespace)
        {
            bail!("a reaction is one emoji");
        }
        let _net = self.net.lock().await;
        ops::react(&self.home, self.target(), id, emoji).await
    }

    /// Fetch what is waiting at the relay. `idle` is how long to wait for more
    /// before disconnecting.
    pub async fn sync(&self, idle: Duration) -> Result<SyncDto> {
        self.sync_with(idle, |_, _| {}).await
    }

    /// [`Session::sync`], telling `progress` (with the running totals and the unread/waiting count)
    /// as soon as each message or request has been stored, instead of after the idle wait that
    /// ends the sync. This is what lets the screen update ~0.7 s sooner.
    pub async fn sync_with(
        &self,
        idle: Duration,
        mut progress: impl FnMut(&SyncDto, usize),
    ) -> Result<SyncDto> {
        let _net = self.net.lock().await;
        let mut out = SyncDto {
            received: 0,
            agent_requests: 0,
            message_requests: 0,
            groups: 0,
            changed: 0,
            notes: vec![],
        };
        ops::recv(&self.home, self.target(), Some(idle), |o| {
            self.calls.on_outcome(&o);
            self.apps.on_outcome(&o);
            if tally(&mut out, &o) {
                progress(&out, self.attention().unwrap_or(0));
            }
        })
        .await?;
        self.sync_large(&mut out, &mut progress).await;
        Ok(out)
    }

    /// Large groups (ADR 0025) are polled from their own mailboxes after the one-to-one queue.
    /// A failure here becomes a note: it must not hide what the queue just delivered.
    async fn sync_large(&self, out: &mut SyncDto, progress: &mut impl FnMut(&SyncDto, usize)) {
        if large::list(&self.home).map_or(true, |l| l.is_empty())
            && self.kp_checked.load(Ordering::SeqCst)
        {
            return;
        }
        if !self.kp_checked.swap(true, Ordering::SeqCst) {
            // Once per session: make sure people can add this device to large groups.
            if let Err(e) = large::top_up_key_packages(&self.home, self.target(), 20).await {
                out.notes
                    .push(format!("Could not publish keys for large groups: {e:#}"));
                self.kp_checked.store(false, Ordering::SeqCst);
            }
        }
        match large::sync(&self.home, self.target()).await {
            Ok(events) => {
                // A group joined, moved to a new mailbox or lost: listen to the right ones.
                if events.iter().any(|e| {
                    !matches!(
                        e,
                        large::Event::Message { .. } | large::Event::Reacted { .. }
                    )
                }) {
                    self.subs.notify_one();
                }
                for e in events {
                    match e {
                        large::Event::Message { .. } => out.received += 1,
                        large::Event::Reacted { .. } => out.changed += 1,
                        large::Event::Changed { .. } | large::Event::OwnDevices => out.groups += 1,
                        large::Event::Left { group, who } => {
                            out.groups += 1;
                            out.notes
                                .push(format!("{who} is leaving {group}; an admin removes them."));
                        }
                        large::Event::Removed { group } => {
                            out.groups += 1;
                            out.notes.push(format!(
                                "You were removed from {group}. Its history stays on this computer."
                            ));
                        }
                        large::Event::Refused { group, why, .. } => out
                            .notes
                            .push(format!("Ignored a change to {group}: {why}")),
                    }
                    progress(out, self.attention().unwrap_or(0));
                }
            }
            Err(e) => out
                .notes
                .push(format!("Could not read your large groups: {e:#}")),
        }
    }

    /// Mark a conversation read here, and on your other devices if reachable.
    pub async fn mark_read(&self, name: &str) -> Result<()> {
        let _net = self.net.lock().await;
        if ops::mark_read(&self.home, self.target(), name)
            .await
            .is_err()
        {
            // Offline: still clear the badge on this device.
            let key = self.contact_key(name)?;
            let upto = self
                .home
                .messages()?
                .conversation(&key, usize::MAX)?
                .iter()
                .filter(|m| m.direction == Direction::In)
                .map(|m| m.ts)
                .max()
                .unwrap_or(0);
            self.home.advance_read_marker(&key, upto)?;
        }
        Ok(())
    }

    // ---------------------------------------------------- message requests

    /// Strangers who introduced themselves and wrote to you, newest first (ADR 0021).
    pub fn message_requests(&self) -> Result<Vec<MessageRequestDto>> {
        Ok(requests::list(&self.home)?
            .into_iter()
            .map(|r| MessageRequestDto {
                id: r.key,
                fingerprint: r.fingerprint,
                received: r.received,
                held: r
                    .held
                    .into_iter()
                    .map(|h| HeldDto {
                        ts: h.ts,
                        body: h.body,
                        file: h.file,
                    })
                    .collect(),
            })
            .collect())
    }

    /// Accept: they become a contact (not verified) called `name`, and what they sent joins the chat.
    pub fn accept_request(&self, id: &str, name: &str) -> Result<usize> {
        requests::accept(&self.home, id, name)
    }

    pub fn decline_request(&self, id: &str) -> Result<()> {
        requests::decline(&self.home, id)
    }

    pub fn block_request(&self, id: &str) -> Result<()> {
        requests::block(&self.home, id)
    }

    pub fn blocked(&self) -> Result<Vec<BlockedDto>> {
        Ok(requests::blocked(&self.home)?
            .into_iter()
            .map(|b| BlockedDto {
                owner: b.owner,
                fingerprint: b.fingerprint,
                at: b.at,
            })
            .collect())
    }

    pub fn unblock(&self, owner: &str) -> Result<()> {
        requests::unblock(&self.home, owner)
    }

    // ----------------------------------------------------- edit and delete

    /// Replace the text of a message you sent, on every device that has it.
    pub async fn edit(&self, id: &str, text: &str) -> Result<()> {
        let text = text.trim_end();
        if text.trim().is_empty() {
            bail!("a message cannot be empty; delete it instead");
        }
        if text.len() > MAX_MESSAGE {
            bail!("messages are limited to {} KiB", MAX_MESSAGE / 1024);
        }
        let _net = self.net.lock().await;
        ops::edit(&self.home, self.target(), id, text).await?;
        Ok(())
    }

    /// Delete a message you sent, everywhere. The row stays as a tombstone so a late copy cannot bring it back.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let _net = self.net.lock().await;
        ops::delete(&self.home, self.target(), id).await?;
        Ok(())
    }

    // ------------------------------------------------------------ devices

    /// Remove another of your devices: sign a new device list without it and publish it.
    /// The removed device keeps what it already has but receives nothing new.
    pub async fn remove_device(&self, id_prefix: &str) -> Result<()> {
        if !agent::is_approver(&self.home, &self.device_id()?)? {
            bail!("remove devices from your main device (the first one you set up)");
        }
        let prefix = id_prefix.trim().to_lowercase();
        if prefix.len() < 8 {
            bail!("give at least 8 characters of the device id");
        }
        let owner = self.home.load_owner()?;
        let list = self.home.load_device_list()?;
        let mine = self.device_id()?;
        let hits: Vec<String> = list
            .verify()?
            .iter()
            .map(|d| d.device_id())
            .filter(|id| id.starts_with(&prefix))
            .collect();
        let target = match hits.as_slice() {
            [id] if *id == mine => {
                bail!("this is the device you are using; remove it from another one")
            }
            [id] => id.clone(),
            [] => bail!("no device of yours has an id starting with '{prefix}'"),
            _ => bail!("'{prefix}' matches several devices; type more characters"),
        };
        let updated = list.without_device(&owner, &target)?;
        let _net = self.net.lock().await;
        self.home.save_device_list(&updated)?;
        ops::publish_keys(&self.home, self.target()).await?;
        Ok(())
    }

    // ------------------------------------------------------------- agents

    fn name_of(&self, key: &str) -> String {
        self.home
            .load_contacts()
            .ok()
            .and_then(|cs| {
                cs.into_iter()
                    .find(|c| c.conversation_key().ok().as_deref() == Some(key))
            })
            .map_or_else(|| format!("{}…", &key[..8.min(key.len())]), |c| c.name)
    }

    pub fn agents(&self) -> Result<AgentsDto> {
        let approver = agent::is_approver(&self.home, &self.device_id()?)?;
        let grants = agent::list_grants(&self.home)?
            .into_iter()
            .map(|g| GrantDto {
                agent: self.name_of(&g.body.agent),
                scopes: g.body.scopes,
                max_cents: g.body.max_cents,
                auto_below_cents: g.body.auto_below_cents,
                per_hour: g.body.per_hour,
                daily_cents: g.body.daily_cents,
                typed_only: g.body.typed_only,
                expires: g.body.expires,
            })
            .collect();
        let mut requests: Vec<RequestDto> = Vec::new();
        for r in agent::list_inbox(&self.home)? {
            let q = &r.signed.req;
            let (status, by) = match &r.status {
                Status::Pending => ("waiting", None),
                Status::Decided { receipt, .. } => (
                    if receipt.body.decision == Decision::Approved {
                        "approved"
                    } else {
                        "denied"
                    },
                    Some(
                        if receipt.body.by == DecidedBy::Policy {
                            "policy"
                        } else {
                            "you"
                        }
                        .to_string(),
                    ),
                ),
            };
            requests.push(dto(q, &r.from, status, by, r.received, false));
        }
        for r in agent::list_remote(&self.home)? {
            requests.push(dto(
                &r.signed.req,
                &r.from,
                "waiting",
                None,
                r.received,
                true,
            ));
        }
        requests.sort_by(|a, b| {
            (a.status != "waiting")
                .cmp(&(b.status != "waiting"))
                .then(b.received.cmp(&a.received))
        });
        Ok(AgentsDto {
            approver,
            grants,
            requests,
        })
    }

    /// The human's decision. On the approver device it is recorded and the
    /// receipt sent; on another device it is forwarded to the approver.
    pub async fn decide(&self, id: &str, approve: bool) -> Result<String> {
        let _net = self.net.lock().await;
        let sent = ops::agent_decide(&self.home, self.target(), id, approve, "").await?;
        let word = if approve { "Approved" } else { "Denied" };
        Ok(if !agent::is_approver(&self.home, &self.device_id()?)? {
            format!("{word}. Sent to your main device, which signs the receipt when it next syncs.")
        } else if sent {
            format!("{word}. Signed receipt sent.")
        } else {
            format!("{word}. The receipt is queued until the agent is reachable.")
        })
    }
}

// ------------------------------------------------------------- grants

/// The known typed action types, for the grant form.
/// A QR code for `text` as an SVG image (dark modules on white, with the quiet zone scanners need).
/// Made here so the code never leaves the app and no script has to be loaded into the page.
pub fn qr_svg(text: &str) -> Result<String> {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};
    if text.len() > 1200 {
        bail!("that is too long for a QR code");
    }
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|e| anyhow!("could not make a QR code: {e}"))?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

pub fn action_types() -> Vec<ActionTypeDto> {
    action::REGISTRY
        .iter()
        .map(|t| ActionTypeDto {
            name: t.name.into(),
            fields: t.fields.iter().map(|(n, _)| (*n).to_string()).collect(),
            money: t.money == action::Money::Required,
            template: t.template.into(),
        })
        .collect()
}

impl Session {
    /// Give `g.agent` (a contact) a standing permission, replacing any earlier one.
    /// Only the approver device decides requests, so only it may hold grants.
    pub fn grant(&self, g: GrantRequest) -> Result<()> {
        if !agent::is_approver(&self.home, &self.device_id()?)? {
            bail!("grants are kept on your main device; create this one there");
        }
        if g.scopes.is_empty()
            || g.scopes.len() > 32
            || !g.scopes.iter().all(|s| spike_core::agent::valid_scope(s))
        {
            bail!("choose at least one permission");
        }
        if !(1..=365).contains(&g.days) {
            bail!("a grant lasts 1 to 365 days");
        }
        if !(1..=1000).contains(&g.per_hour) {
            bail!("requests per hour must be between 1 and 1000");
        }
        let key = self.contact_key(&g.agent)?;
        agent::grant(
            &self.home,
            &key,
            g.scopes,
            g.days,
            g.max_cents,
            g.auto_below_cents,
            g.per_hour,
            g.daily_cents,
            g.typed_only,
        )?;
        Ok(())
    }

    /// Remove an agent's grant. Approvals already given live until they expire.
    pub fn revoke(&self, agent_name: &str) -> Result<bool> {
        let key = self.contact_key(agent_name)?;
        agent::revoke(&self.home, &key)
    }

    // -------------------------------------------------------- attachments

    /// Encrypt a file, upload it to the relay, and send the message carrying it.
    pub async fn send_file(
        &self,
        name: &str,
        file_name: &str,
        bytes: &[u8],
        caption: &str,
    ) -> Result<usize> {
        if bytes.is_empty() {
            bail!("that file is empty");
        }
        if bytes.len() as u64 > spike_core::attachment::MAX_FILE {
            bail!(
                "files are limited to {} MiB",
                spike_core::attachment::MAX_FILE / (1024 * 1024)
            );
        }
        let file_name = spike_core::attachment::safe_file_name(file_name);
        let _net = self.net.lock().await;
        let r = ops::send_file(
            &self.home,
            self.target(),
            name,
            &file_name,
            bytes,
            caption.trim_end(),
        )
        .await?;
        Ok(r.delivered())
    }

    /// The file of message `id` (a full message id): from this device if kept,
    /// else fetched from the relay, checked and kept. Returns (name, bytes).
    pub async fn attachment(&self, id: &str) -> Result<(String, Vec<u8>)> {
        let _net = self.net.lock().await;
        let (a, bytes) = ops::fetch_attachment(&self.home, self.target(), id).await?;
        Ok((spike_core::attachment::safe_file_name(&a.name), bytes))
    }

    // ------------------------------------------------------------ linking

    /// NEW device: make a link code to type on a device that already has your identity.
    pub fn link_offer(&self) -> Result<String> {
        let w = link::offer(&self.home, self.target())?;
        let code = w.code.clone();
        // A normal receive would take the link frames off the relay and acknowledge them.
        self.set_paused(true);
        *self.link_new.lock().expect("link lock") = Some((w, None));
        Ok(code)
    }

    /// NEW device: wait for the existing device and return the safety code to compare.
    /// Holds the network lock while waiting, so nothing else touches the relay
    /// mailbox. Call again after a timeout; the code stays valid.
    pub async fn link_wait(&self, timeout: Duration) -> Result<String> {
        let _net = self.net.lock().await;
        let (w, _) = self
            .link_new
            .lock()
            .expect("link lock")
            .take()
            .ok_or_else(|| anyhow!("no link in progress; start again"))?;
        let r = link::await_hello(&self.home, &w, timeout).await;
        match r {
            Ok(session) => {
                let code = session.safety_code.clone();
                *self.link_new.lock().expect("link lock") = Some((w, Some(session)));
                Ok(code)
            }
            Err(e) => {
                *self.link_new.lock().expect("link lock") = Some((w, None));
                Err(e)
            }
        }
    }

    /// NEW device: the person confirmed both screens show the same code.
    pub async fn link_receive(&self, timeout: Duration) -> Result<InstalledDto> {
        let _net = self.net.lock().await;
        let (w, session) = self
            .link_new
            .lock()
            .expect("link lock")
            .take()
            .ok_or_else(|| anyhow!("no link in progress; start again"))?;
        let Some(session) = session else {
            *self.link_new.lock().expect("link lock") = Some((w, None));
            bail!("compare the safety codes first");
        };
        let r = link::receive(&self.home, &w, &session, timeout).await;
        self.set_paused(false);
        let r = r?;
        Ok(InstalledDto {
            version: r.version,
            contacts: r.contacts,
            messages: r.messages,
        })
    }

    /// Either side: give up. Nothing was installed or shared.
    pub fn link_cancel(&self) {
        self.set_paused(false);
        *self.link_new.lock().expect("link lock") = None;
        *self.link_old.lock().expect("link lock") = None;
    }

    /// EXISTING device: type the new device's code; returns the safety code to compare.
    pub async fn link_begin(&self, code: &str) -> Result<String> {
        if !agent::is_approver(&self.home, &self.device_id()?)? {
            bail!("link new devices from your main device (the first one you set up)");
        }
        let _net = self.net.lock().await;
        let p = link::begin(&self.home, self.target(), code.trim()).await?;
        let safety = p.safety_code.clone();
        *self.link_old.lock().expect("link lock") = Some(p);
        Ok(safety)
    }

    /// EXISTING device: the person confirmed the codes match. `history` is
    /// `all`, `recent` (last `days` days) or `none`.
    pub async fn link_finish(&self, history: &str, days: u64) -> Result<LinkedDto> {
        let mode = match history {
            "all" => link::History::All,
            "recent" => link::History::Recent {
                days: days.clamp(1, 3650),
            },
            "none" => link::History::None,
            other => bail!("unknown history choice '{other}'"),
        };
        let p = self
            .link_old
            .lock()
            .expect("link lock")
            .take()
            .ok_or_else(|| anyhow!("no link in progress; enter the code again"))?;
        let _net = self.net.lock().await;
        let r = link::finish(&self.home, self.target(), p, mode).await?;
        Ok(LinkedDto {
            version: r.version,
            contacts: r.contacts,
            messages: r.messages,
            files: r.files,
        })
    }
}

/// Put the sealed CA certificate where the transport reads it (or remove a stale file).
fn write_ca(home: &Home, rs: &RelaySettings) -> Result<()> {
    let path = home.path().join(CA_FILE);
    match &rs.ca_pem {
        Some(pem) => std::fs::write(&path, pem).context("saving the trusted certificate")?,
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

fn dto(
    q: &spike_core::agent::Request,
    from: &str,
    status: &str,
    by: Option<String>,
    received: u64,
    forwarded: bool,
) -> RequestDto {
    RequestDto {
        id: q.id.clone(),
        from: from.into(),
        scope: q.scope.clone(),
        note: q.action.clone(),
        amount_cents: q.amount_cents,
        summary: q.summary(),
        typed: q.typed.clone(),
        status: status.into(),
        by,
        received,
        expires: q.expires,
        forwarded,
    }
}

fn message_dto(
    m: spike_cli::messages::StoredMessage,
    from: Option<String>,
    from_verified: bool,
    me: &str,
) -> MessageDto {
    // Each emoji once, with its count; taken-back reactions (empty) are not shown.
    let mut reactions: Vec<ReactionDto> = Vec::new();
    if !m.deleted {
        for (who, r) in &m.reactions {
            if r.emoji.is_empty() {
                continue;
            }
            match reactions.iter_mut().find(|x| x.emoji == r.emoji) {
                Some(x) => {
                    x.count += 1;
                    x.mine |= who == me;
                }
                None => reactions.push(ReactionDto {
                    emoji: r.emoji.clone(),
                    count: 1,
                    mine: who == me,
                }),
            }
        }
        reactions.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.emoji.cmp(&b.emoji)));
    }
    MessageDto {
        reply: None,
        reactions,
        out: m.direction == Direction::Out,
        body: if m.deleted { String::new() } else { m.body },
        file: if m.deleted {
            None
        } else {
            m.attachment.map(|a| FileDto {
                name: spike_core::attachment::safe_file_name(&a.name),
                size: a.size,
            })
        },
        id: m.id,
        ts: m.ts,
        edited: m.edited,
        deleted: m.deleted,
        from,
        from_verified,
        shared_by: None,
    }
}

fn preview(body: &str, file: Option<&str>) -> String {
    let text: String = body.chars().filter(|c| !c.is_control()).take(80).collect();
    match (text.is_empty(), file) {
        (true, Some(f)) => format!("File: {}", spike_core::attachment::safe_file_name(f)),
        (_, Some(_)) => format!("{text} (file)"),
        _ => text,
    }
}

// ------------------------------------------------------------- groups (ADR 0024)

impl Session {
    fn my_owner(&self) -> Result<String> {
        Ok(self.home.load_device_list()?.owner)
    }

    /// Members other than you who are not verified contacts.
    fn unverified_members(&self, s: &GroupState) -> usize {
        let contacts = self.home.load_contacts().unwrap_or_default();
        let me = self.my_owner().unwrap_or_default();
        s.members
            .iter()
            .filter(|m| m.owner != me)
            .filter(|m| {
                !contacts.iter().any(|c| {
                    c.verified && c.conversation_key().ok().as_deref() == Some(m.owner.as_str())
                })
            })
            .count()
    }

    fn author_verified(&self, owner: &str) -> bool {
        self.home.load_contacts().is_ok_and(|cs| {
            cs.iter()
                .any(|c| c.verified && c.conversation_key().ok().as_deref() == Some(owner))
        })
    }

    /// The rows for the chat list: groups you are in, invites, and groups you left that
    /// still have a history. Ordering is the UI's job.
    fn group_rows(&self) -> Result<Vec<GroupDto>> {
        let unread: BTreeMap<String, usize> =
            groups::unread_counts(&self.home)?.into_iter().collect();
        let store = self.home.messages()?;
        let mut out = Vec::new();
        for r in groups::list(&self.home)? {
            let s = r.current()?;
            let last = store
                .conversation(r.id(), usize::MAX)?
                .into_iter()
                .rfind(|m| !groups::is_ghost(m));
            if r.status == groups::Status::Left && last.is_none() {
                continue;
            }
            out.push(GroupDto {
                id: r.id().to_string(),
                members: s.members.len(),
                unread: unread.get(r.id()).copied().unwrap_or(0),
                status: status_word(r.status).into(),
                invited_by: r
                    .invited_by
                    .as_deref()
                    .map(|o| groups::display_name(&self.home, o)),
                unverified: self.unverified_members(&s),
                large: false,
                last: last.as_ref().map(|m| {
                    if m.deleted {
                        "Message deleted".into()
                    } else {
                        let who = match (m.direction, m.author.as_deref()) {
                            (Direction::Out, _) => "You".to_string(),
                            (_, Some(a)) => groups::display_name(&self.home, a),
                            _ => "?".to_string(),
                        };
                        format!(
                            "{who}: {}",
                            preview(&m.body, m.attachment.as_ref().map(|a| a.name.as_str()))
                        )
                    }
                }),
                last_ts: last.map(|m| m.ts),
                name: s.name,
            });
        }
        out.extend(self.large_rows()?);
        Ok(out)
    }

    /// Members, settings and your rights in one group.
    pub fn group(&self, id: &str) -> Result<GroupDetailDto> {
        if self.is_large(id) {
            return self.large_detail(id);
        }
        let r = groups::find(&self.home, id)?;
        let s = r.current()?;
        let me = self.my_owner()?;
        let contacts = self.home.load_contacts()?;
        let cards = groups::member_cards(&self.home)?;
        let members = s
            .members
            .iter()
            .map(|m| {
                let contact = contacts
                    .iter()
                    .find(|c| c.conversation_key().ok().as_deref() == Some(m.owner.as_str()));
                let fingerprint = contact
                    .and_then(|c| c.identity().ok())
                    .or_else(|| {
                        cards
                            .get(&m.owner)
                            .and_then(|c| IdentityCard::decode(c).ok())
                    })
                    .map(|c| c.fingerprint());
                MemberDto {
                    owner: m.owner.clone(),
                    name: groups::display_name(&self.home, &m.owner),
                    you: m.owner == me,
                    admin: s.is_admin(&m.owner),
                    contact: contact.is_some(),
                    verified: contact.is_some_and(|c| c.verified),
                    fingerprint,
                }
            })
            .collect();
        let me_admin = s.is_admin(&me);
        Ok(GroupDetailDto {
            id: r.id().to_string(),
            status: status_word(r.status).into(),
            invited_by: r
                .invited_by
                .as_deref()
                .map(|o| groups::display_name(&self.home, o)),
            members,
            me_admin,
            can_add: r.status == groups::Status::Joined
                && s.members.len() < MAX_MEMBERS
                && (me_admin || s.add_by == AddBy::Any),
            add_by: if s.add_by == AddBy::Any {
                "any"
            } else {
                "admins"
            }
            .into(),
            version: s.version,
            max: MAX_MEMBERS,
            unverified: self.unverified_members(&s),
            history: if s.history == History::Week {
                "week"
            } else {
                "join"
            }
            .into(),
            can_block: r.status == groups::Status::Invited && r.stranger.is_some(),
            large: false,
            refused: 0,
            refused_why: None,
            name: s.name,
        })
    }

    pub fn group_conversation(&self, id: &str, limit: usize) -> Result<Vec<MessageDto>> {
        let msgs = if self.is_large(id) {
            large::history(&self.home, id, limit.clamp(1, 500))?.1
        } else {
            groups::history(&self.home, id, limit.clamp(1, 500))?.1
        };
        let me = self.my_owner()?;
        let gid = msgs.first().map(|m| m.peer.clone()).unwrap_or_default();
        let replies = self.replies_in(&gid, &msgs, true)?;
        Ok(msgs
            .into_iter()
            .map(|m| {
                let reply = replies.get(&m.id).cloned();
                let author = m.author.clone().unwrap_or_default();
                let (from, verified) = if m.direction == Direction::Out {
                    (None, true)
                } else {
                    (
                        Some(groups::display_name(&self.home, &author)),
                        self.author_verified(&author),
                    )
                };
                let shared = m
                    .shared_by
                    .as_deref()
                    .map(|o| groups::display_name(&self.home, o));
                let mut dto = message_dto(m, from, verified, &me);
                dto.shared_by = shared;
                dto.reply = reply;
                dto
            })
            .collect())
    }

    fn sent_dto(group: &str, r: &groups::GroupSend) -> GroupSendDto {
        GroupSendDto {
            group: group.to_string(),
            reached: r.reached,
            people: r.people,
            notes: r.notes.clone(),
        }
    }

    /// Create a group with some of your contacts. You are its only admin.
    pub async fn group_create(
        &self,
        name: &str,
        members: &[String],
        any_can_add: bool,
    ) -> Result<GroupSendDto> {
        self.group_create_with(name, members, any_can_add, false)
            .await
    }

    /// [`Session::group_create`], also choosing what new members see: messages from when they
    /// join, or (`history_week`) the last 7 days.
    pub async fn group_create_with(
        &self,
        name: &str,
        members: &[String],
        any_can_add: bool,
        history_week: bool,
    ) -> Result<GroupSendDto> {
        if members.is_empty() {
            bail!("choose at least one person to add");
        }
        let add_by = if any_can_add {
            AddBy::Any
        } else {
            AddBy::Admins
        };
        let _net = self.net.lock().await;
        let history = if history_week {
            History::Week
        } else {
            History::Join
        };
        let (s, r) =
            groups::create_with(&self.home, self.target(), name, members, add_by, history).await?;
        Ok(Self::sent_dto(&s.group, &r))
    }

    pub async fn group_send(&self, id: &str, text: &str) -> Result<GroupSendDto> {
        self.group_send_reply(id, text, None).await
    }

    /// Send a group message that replies to an earlier one of the group (`reply_to`, its id).
    pub async fn group_send_reply(
        &self,
        id: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<GroupSendDto> {
        let text = text.trim_end();
        if text.trim().is_empty() {
            bail!("nothing to send");
        }
        if text.len() > MAX_MESSAGE {
            bail!("messages are limited to {} KiB", MAX_MESSAGE / 1024);
        }
        if self.is_large(id) {
            if let Some(t) = reply_to {
                self.reply_target(&large::find(&self.home, id)?.id, t)?;
            }
            return self.large_send(id, text, reply_to).await;
        }
        let gid = groups::find(&self.home, id)?.id().to_string();
        let _net = self.net.lock().await;
        let r = match reply_to {
            Some(t) => {
                self.reply_target(&gid, t)?;
                groups::send_reply(&self.home, self.target(), &gid, t, text).await?
            }
            None => groups::send(&self.home, self.target(), &gid, text).await?,
        };
        Ok(Self::sent_dto(&gid, &r))
    }

    /// Send a file to a group: encrypted once, uploaded once, every member fetches the same blob.
    pub async fn group_send_file(
        &self,
        id: &str,
        file_name: &str,
        bytes: &[u8],
        caption: &str,
    ) -> Result<GroupSendDto> {
        self.not_large(id, "sending files")?;
        let gid = groups::find(&self.home, id)?.id().to_string();
        let file_name = spike_core::attachment::safe_file_name(file_name);
        let _net = self.net.lock().await;
        let r = groups::send_file(
            &self.home,
            self.target(),
            &gid,
            &file_name,
            bytes,
            caption.trim_end(),
        )
        .await?;
        Ok(Self::sent_dto(&gid, &r))
    }

    /// Choose what new members see: messages from when they join, or the last 7 days.
    pub async fn group_set_history(&self, id: &str, week: bool) -> Result<GroupSendDto> {
        self.not_large(id, "choosing what new members see")?;
        let history = if week { History::Week } else { History::Join };
        let _net = self.net.lock().await;
        let (s, r) = groups::set_history(&self.home, self.target(), id, history).await?;
        Ok(Self::sent_dto(&s.group, &r))
    }

    pub async fn group_add(&self, id: &str, names: &[String]) -> Result<GroupSendDto> {
        if names.is_empty() {
            bail!("choose at least one person to add");
        }
        if self.is_large(id) {
            return self.large_add(id, names).await;
        }
        let _net = self.net.lock().await;
        let (s, r) = groups::add(&self.home, self.target(), id, names).await?;
        Ok(Self::sent_dto(&s.group, &r))
    }

    /// Remove a member by owner key (from [`MemberDto::owner`]).
    pub async fn group_remove(&self, id: &str, owner: &str) -> Result<GroupSendDto> {
        if self.is_large(id) {
            return self.large_remove(id, owner).await;
        }
        let _net = self.net.lock().await;
        let (s, r) = groups::remove(&self.home, self.target(), id, owner).await?;
        Ok(Self::sent_dto(&s.group, &r))
    }

    pub async fn group_make_admin(&self, id: &str, owner: &str) -> Result<GroupSendDto> {
        if self.is_large(id) {
            let _net = self.net.lock().await;
            large::make_admin(&self.home, self.target(), id, owner).await?;
            return Ok(Self::large_done(id));
        }
        let _net = self.net.lock().await;
        let (s, r) = groups::make_admin(&self.home, self.target(), id, owner).await?;
        Ok(Self::sent_dto(&s.group, &r))
    }

    pub async fn group_rename(&self, id: &str, name: &str) -> Result<GroupSendDto> {
        if self.is_large(id) {
            let _net = self.net.lock().await;
            large::rename(&self.home, self.target(), id, name).await?;
            return Ok(Self::large_done(id));
        }
        let _net = self.net.lock().await;
        let (s, r) = groups::rename(&self.home, self.target(), id, name).await?;
        Ok(Self::sent_dto(&s.group, &r))
    }

    pub async fn group_set_add_by(&self, id: &str, any: bool) -> Result<GroupSendDto> {
        if self.is_large(id) {
            let _net = self.net.lock().await;
            large::set_members_can_add(&self.home, self.target(), id, any).await?;
            return Ok(Self::large_done(id));
        }
        let add_by = if any { AddBy::Any } else { AddBy::Admins };
        let _net = self.net.lock().await;
        let (s, r) = groups::set_add_by(&self.home, self.target(), id, add_by).await?;
        Ok(Self::sent_dto(&s.group, &r))
    }

    /// Leave a group. Your history stays on this computer; nobody can add you back without your OK.
    pub async fn group_leave(&self, id: &str) -> Result<()> {
        if self.is_large(id) {
            let _net = self.net.lock().await;
            large::leave(&self.home, self.target(), id).await?;
            self.subs.notify_one();
            return Ok(());
        }
        let _net = self.net.lock().await;
        groups::leave(&self.home, self.target(), id).await?;
        Ok(())
    }

    /// Accept an invite. Your other devices that hold the same invite join too.
    pub async fn group_accept(&self, id: &str) -> Result<()> {
        if self.is_large(id) {
            let _net = self.net.lock().await;
            large::accept(&self.home, self.target(), id).await?;
            self.subs.notify_one();
            return Ok(());
        }
        self.invite_decision(id, groups::InviteDecision::Accept)
            .await
    }

    /// Decline an invite. The inviter is not told; your other devices decline too.
    pub async fn group_decline(&self, id: &str) -> Result<()> {
        if self.is_large(id) {
            return large::decline(&self.home, self.target(), id).await;
        }
        self.invite_decision(id, groups::InviteDecision::Decline)
            .await
    }

    /// Decline an invite and block the inviter (someone who is not a contact), on all your devices.
    pub async fn group_block(&self, id: &str) -> Result<()> {
        self.not_large(id, "blocking an inviter")?;
        self.invite_decision(id, groups::InviteDecision::Block)
            .await
    }

    async fn invite_decision(&self, id: &str, d: groups::InviteDecision) -> Result<()> {
        let _net = self.net.lock().await;
        // Settled here first; telling the other devices is best effort (offline, they decide themselves).
        groups::decide_invite(&self.home, self.target(), id, d).await?;
        Ok(())
    }

    /// Mark a group read here, and on your other devices if reachable.
    pub async fn group_mark_read(&self, id: &str) -> Result<()> {
        if self.is_large(id) {
            return large::mark_read(&self.home, id);
        }
        groups::find(&self.home, id)?;
        let _net = self.net.lock().await;
        // The marker moves on this device first; telling the others may fail while offline.
        let _ = groups::mark_read(&self.home, self.target(), id).await;
        Ok(())
    }

    /// Keep a group member who is not a contact as a contact (not verified), under `name`.
    pub fn add_member_as_contact(&self, owner: &str, name: &str) -> Result<String> {
        let card = groups::member_cards(&self.home)?
            .get(owner)
            .cloned()
            .ok_or_else(|| anyhow!("no card is known for that member yet"))?;
        self.add_contact(name, &card)
    }
}

// ------------------------------------------------------- large groups (ADR 0025)

fn large_status_word(s: large::Status) -> &'static str {
    match s {
        large::Status::Active => "joined",
        large::Status::Invited => "invited",
        large::Status::Removed | large::Status::Left => "left",
    }
}

impl Session {
    fn is_large(&self, id: &str) -> bool {
        large::list(&self.home).is_ok_and(|l| l.iter().any(|r| r.id == id))
    }

    fn not_large(&self, id: &str, what: &str) -> Result<()> {
        if self.is_large(id) {
            bail!("{what} is not available for a large group yet");
        }
        Ok(())
    }

    /// What a change to a large group reports: it has no per-person delivery.
    fn large_done(id: &str) -> GroupSendDto {
        GroupSendDto {
            group: id.to_string(),
            reached: 0,
            people: 0,
            notes: vec![],
        }
    }

    fn large_rows(&self) -> Result<Vec<GroupDto>> {
        let unread: BTreeMap<String, usize> =
            large::unread_counts(&self.home)?.into_iter().collect();
        let store = self.home.messages()?;
        let mut out = Vec::new();
        for r in large::list(&self.home)? {
            let last = store
                .conversation(&r.id, usize::MAX)?
                .into_iter()
                .next_back();
            if r.status == large::Status::Removed && last.is_none() {
                continue;
            }
            out.push(GroupDto {
                unread: unread.get(&r.id).copied().unwrap_or(0),
                status: large_status_word(r.status).into(),
                invited_by: r.invite.as_ref().map(|i| i.from.clone()),
                unverified: self.large_unverified(&r.id),
                large: true,
                last: last.as_ref().map(|m| {
                    if m.deleted {
                        "Message deleted".into()
                    } else {
                        let who = match (m.direction, m.author.as_deref()) {
                            (Direction::Out, _) => "You".to_string(),
                            (_, Some(a)) => groups::display_name(&self.home, a),
                            _ => "?".to_string(),
                        };
                        format!("{who}: {}", preview(&m.body, None))
                    }
                }),
                last_ts: last.map(|m| m.ts),
                members: r.members,
                name: r.name,
                id: r.id,
            });
        }
        Ok(out)
    }

    /// People in the group (not you) who are not verified contacts.
    fn large_unverified(&self, id: &str) -> usize {
        let me = self.my_owner().unwrap_or_default();
        let contacts = self.home.load_contacts().unwrap_or_default();
        large::members(&self.home, id)
            .map(|ms| {
                ms.iter()
                    .filter(|m| m.owner != me)
                    .filter(|m| {
                        !contacts.iter().any(|c| {
                            c.verified
                                && c.conversation_key().ok().as_deref() == Some(m.owner.as_str())
                        })
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn large_detail(&self, id: &str) -> Result<GroupDetailDto> {
        let r = large::find(&self.home, id)?;
        let me = self.my_owner()?;
        let contacts = self.home.load_contacts()?;
        let cards = groups::member_cards(&self.home)?;
        let infos = if r.status == large::Status::Active {
            large::members(&self.home, id)?
        } else {
            vec![]
        };
        let members: Vec<MemberDto> = infos
            .iter()
            .map(|m| {
                let contact = contacts
                    .iter()
                    .find(|c| c.conversation_key().ok().as_deref() == Some(m.owner.as_str()));
                let fingerprint = contact
                    .and_then(|c| c.identity().ok())
                    .or_else(|| {
                        cards
                            .get(&m.owner)
                            .and_then(|c| IdentityCard::decode(c).ok())
                    })
                    .map(|c| c.fingerprint());
                MemberDto {
                    owner: m.owner.clone(),
                    name: groups::display_name(&self.home, &m.owner),
                    you: m.owner == me,
                    admin: m.admin,
                    contact: contact.is_some(),
                    verified: contact.is_some_and(|c| c.verified),
                    fingerprint,
                }
            })
            .collect();
        // You first, then by name: the owner keys they come in have no useful order.
        let mut members = members;
        members.sort_by(|a, b| b.you.cmp(&a.you).then_with(|| a.name.cmp(&b.name)));
        let me_admin = infos.iter().any(|m| m.owner == me && m.admin);
        let settings = large::settings(&self.home, id)?;
        Ok(GroupDetailDto {
            id: r.id.clone(),
            name: r.name.clone(),
            status: large_status_word(r.status).into(),
            invited_by: r.invite.as_ref().map(|i| i.from.clone()),
            can_add: r.status == large::Status::Active
                && (me_admin || settings.members_can_add)
                && members.len() < LARGE_MAX,
            members,
            me_admin,
            add_by: if settings.members_can_add {
                "any"
            } else {
                "admins"
            }
            .into(),
            version: r.cursor,
            max: LARGE_MAX,
            unverified: self.large_unverified(&r.id),
            history: "join".into(),
            can_block: false,
            large: true,
            refused: settings.refused_total,
            refused_why: settings.last_refused,
        })
    }

    async fn large_send(
        &self,
        id: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<GroupSendDto> {
        let _net = self.net.lock().await;
        match reply_to {
            Some(t) => large::send_reply(&self.home, self.target(), id, t, text).await?,
            None => large::send(&self.home, self.target(), id, text).await?,
        };
        let people = large::find(&self.home, id)?.members.saturating_sub(1);
        // Everyone reads the one stored copy; there is no per-person delivery to report.
        Ok(GroupSendDto {
            group: id.to_string(),
            reached: people,
            people,
            notes: vec![],
        })
    }

    async fn large_add(&self, id: &str, names: &[String]) -> Result<GroupSendDto> {
        let _net = self.net.lock().await;
        let mut notes = Vec::new();
        let mut added = 0;
        for n in names {
            match large::add(&self.home, self.target(), id, n).await {
                Ok(_) => added += 1,
                Err(e) => notes.push(format!("{n}: {e:#}")),
            }
        }
        if added == 0 {
            bail!("{}", notes.join("; "));
        }
        Ok(GroupSendDto {
            group: id.to_string(),
            reached: added,
            people: names.len(),
            notes,
        })
    }

    async fn large_remove(&self, id: &str, owner: &str) -> Result<GroupSendDto> {
        let key: [u8; 32] = hex::decode(owner)?
            .try_into()
            .map_err(|_| anyhow!("that is not a member's key"))?;
        let name = groups::display_name(&self.home, owner);
        let _net = self.net.lock().await;
        large::remove_owner(&self.home, self.target(), id, &key, &name).await?;
        self.subs.notify_one();
        Ok(GroupSendDto {
            group: id.to_string(),
            reached: 1,
            people: 1,
            notes: vec![],
        })
    }

    /// Create a large group (MLS, up to 500 people) and invite some of your contacts. They join
    /// when they accept.
    pub async fn group_create_large(&self, name: &str, members: &[String]) -> Result<GroupSendDto> {
        if members.is_empty() {
            bail!("choose at least one person to add");
        }
        let _net = self.net.lock().await;
        if !self.kp_checked.load(Ordering::SeqCst) {
            // Our own keys do not matter to create, but a person added later will need them.
            let _ = large::top_up_key_packages(&self.home, self.target(), 20).await;
            self.kp_checked.store(true, Ordering::SeqCst);
        }
        let rec = large::create(&self.home, self.target(), name).await?;
        self.subs.notify_one();
        let mut notes = Vec::new();
        let mut reached = 0;
        for m in members {
            match large::add(&self.home, self.target(), &rec.id, m).await {
                Ok(_) => reached += 1,
                Err(e) => notes.push(format!("{m}: {e:#}")),
            }
        }
        Ok(GroupSendDto {
            group: rec.id,
            reached,
            people: members.len(),
            notes,
        })
    }
}

/// The first-release cap for a large group (ADR 0025).
const LARGE_MAX: usize = 500;

fn status_word(s: groups::Status) -> &'static str {
    match s {
        groups::Status::Joined => "joined",
        groups::Status::Invited => "invited",
        groups::Status::Left => "left",
    }
}
