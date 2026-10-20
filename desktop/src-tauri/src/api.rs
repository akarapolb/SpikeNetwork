//! Everything the desktop UI can do, as plain functions over a [`Session`].
//! The Tauri commands in `lib.rs` are one-line wrappers, so this module is
//! tested without a window (see `tests/`). See `docs/adr/0019-tauri-desktop-shell.md`.
//!
//! Keys never leave this process: the UI only gets names, message text and
//! request summaries. All network operations are serialized by one lock,
//! because `send` and `recv` each load and save the ratchet state and would
//! corrupt it if they overlapped.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde::Serialize;
use spike_cli::agent::{self, Status};
use spike_cli::client::Relay;
use spike_cli::link;
use spike_cli::messages::Direction;
use spike_cli::state::{Added, Home};
use spike_cli::transport::{validate_ca_pem, Target, TransportOpts};
use spike_cli::{ops, Outcome};
use spike_core::action;
use spike_core::agent::{DecidedBy, Decision};
use spike_core::link::LinkSession;
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
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
    home: Home,
    relay: Mutex<RelaySettings>,
    net: tokio::sync::Mutex<()>,
    /// Set while this device waits to be linked: nothing else may read the relay mailbox.
    paused: AtomicBool,
    /// This (new) device is waiting to be linked: its code and, once the
    /// existing device answered, the safety code to compare.
    link_new: Mutex<Option<(link::Waiting, Option<LinkSession>)>>,
    /// This (existing) device compared codes with a new device and is about to share.
    link_old: Mutex<Option<link::Pending>>,
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
    pub waiting: usize,
    pub relay: String,
    pub relay_trust: RelayTrustDto,
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
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SyncDto {
    pub received: usize,
    pub agent_requests: usize,
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
            home,
            relay: Mutex::new(settings),
            net: tokio::sync::Mutex::new(()),
            paused: AtomicBool::new(false),
            link_new: Mutex::new(None),
            link_old: Mutex::new(None),
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
        let rs = self.settings();
        Target {
            url: rs.url,
            opts: TransportOpts {
                ca_file: rs.ca_pem.is_some().then(|| self.home.path().join(CA_FILE)),
                allow_insecure: rs.allow_insecure,
            },
        }
    }

    /// Connect and sign in to the relay, to show the person whether it works.
    pub async fn test_relay(&self) -> Result<String> {
        let me = self.home.load_identity()?;
        let target = self.target();
        let url = target.url.clone();
        let _net = self.net.lock().await;
        match tokio::time::timeout(Duration::from_secs(15), Relay::connect(target, &me)).await {
            Err(_) => bail!("no answer from {url} within 15 seconds"),
            Ok(Err(e)) => Err(e.context(format!("could not connect to {url}"))),
            Ok(Ok(_)) => Ok(format!(
                "Connected to {url}{}.",
                if url.starts_with("wss://") {
                    " (certificate verified)"
                } else {
                    ""
                }
            )),
        }
    }

    // ------------------------------------------------------ live connection

    /// Open the connection the listener waits on. It takes the network lock only
    /// while connecting: any operation that runs later connects too, which makes
    /// the relay drop this one (one live connection per device), and the listener
    /// then waits for the lock before reconnecting.
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
        Ok(unread + waiting)
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
            waiting,
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
        Ok(self
            .home
            .messages()?
            .conversation(&key, limit.clamp(1, 500))?
            .into_iter()
            .map(|m| MessageDto {
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
            })
            .collect())
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
        let text = text.trim_end();
        if text.trim().is_empty() {
            bail!("nothing to send");
        }
        if text.len() > MAX_MESSAGE {
            bail!("messages are limited to {} KiB", MAX_MESSAGE / 1024);
        }
        let _net = self.net.lock().await;
        let r = ops::send(&self.home, self.target(), name, text.as_bytes()).await?;
        Ok(r.delivered())
    }

    /// Fetch what is waiting at the relay. `idle` is how long to wait for more
    /// before disconnecting.
    pub async fn sync(&self, idle: Duration) -> Result<SyncDto> {
        let _net = self.net.lock().await;
        let mut out = SyncDto {
            received: 0,
            agent_requests: 0,
            notes: vec![],
        };
        ops::recv(&self.home, self.target(), Some(idle), |o| match o {
            Outcome::Message { .. } | Outcome::SentCopy { .. } => out.received += 1,
            Outcome::AgentRequest { verdict, .. } if verdict == "pending" => {
                out.agent_requests += 1
            }
            Outcome::AgentRemotePending { .. } => out.agent_requests += 1,
            Outcome::UnknownSender { .. } => out
                .notes
                .push("A message from someone who is not in your contacts was dropped.".into()),
            Outcome::Rejected { reason } => out.notes.push(format!("Ignored a change: {reason}")),
            _ => {}
        })
        .await?;
        Ok(out)
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

fn preview(body: &str, file: Option<&str>) -> String {
    let text: String = body.chars().filter(|c| !c.is_control()).take(80).collect();
    match (text.is_empty(), file) {
        (true, Some(f)) => format!("File: {}", spike_core::attachment::safe_file_name(f)),
        (_, Some(_)) => format!("{text} (file)"),
        _ => text,
    }
}
