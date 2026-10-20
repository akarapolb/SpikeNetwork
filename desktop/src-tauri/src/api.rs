//! Everything the desktop UI can do, as plain functions over a [`Session`].
//! The Tauri commands in `lib.rs` are one-line wrappers, so this module is
//! tested without a window (see `tests/`). See `docs/adr/0019-tauri-desktop-shell.md`.
//!
//! Keys never leave this process: the UI only gets names, message text and
//! request summaries. All network operations are serialized by one lock,
//! because `send` and `recv` each load and save the ratchet state and would
//! corrupt it if they overlapped.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use spike_cli::agent::{self, Status};
use spike_cli::messages::Direction;
use spike_cli::state::{Added, Home};
use spike_cli::transport::Target;
use spike_cli::{ops, Outcome};
use spike_core::agent::{DecidedBy, Decision};
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use zeroize::Zeroizing;

pub const MIN_PASSPHRASE_CHARS: usize = 10;
pub const DEFAULT_RELAY: &str = "ws://127.0.0.1:8787";
const RELAY_FILE: &str = "desktop-relay.enc";
const MAX_NAME: usize = 64;
const MAX_MESSAGE: usize = 16 * 1024;

/// An unlocked state directory plus the relay it talks to.
pub struct Session {
    home: Home,
    relay: Mutex<String>,
    net: tokio::sync::Mutex<()>,
}

// ------------------------------------------------------------------ DTOs

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub fingerprint: String,
    pub card: String,
    pub devices: usize,
    pub approver: bool,
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
        let saved: String = home.load_json(RELAY_FILE)?;
        let relay = if saved.is_empty() {
            DEFAULT_RELAY.to_string()
        } else {
            saved
        };
        Ok(Self {
            home,
            relay: Mutex::new(relay),
            net: tokio::sync::Mutex::new(()),
        })
    }

    pub fn relay(&self) -> String {
        self.relay.lock().expect("relay lock").clone()
    }

    pub fn set_relay(&self, url: &str) -> Result<()> {
        let url = url.trim();
        if !(url.starts_with("ws://") || url.starts_with("wss://"))
            || url.len() > 256
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            bail!("the relay address must look like wss://relay.example or ws://127.0.0.1:8787");
        }
        self.home.save_json(RELAY_FILE, &url.to_string())?;
        *self.relay.lock().expect("relay lock") = url.into();
        Ok(())
    }

    fn target(&self) -> Target {
        Target::from(self.relay().as_str())
    }

    // ------------------------------------------------------------ read side

    fn device_id(&self) -> Result<String> {
        Ok(self.home.load_identity()?.device_id())
    }

    pub fn overview(&self) -> Result<Overview> {
        let card = self.home.identity_card()?;
        let me = IdentityCard::decode(&card)?;
        let approver = agent::is_approver(&self.home, &self.device_id()?)?;
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
                approver,
            },
            contacts,
            waiting,
            relay: self.relay(),
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
