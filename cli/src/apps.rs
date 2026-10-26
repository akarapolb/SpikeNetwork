//! App channels (ADR 0030): carry another application's bytes over Spike, one to one or in a
//! small group, without showing them as chat.
//!
//! An application is registered on a device by its id (`work.center`). Envelopes for an id this
//! device has not registered are acknowledged and dropped (and counted, so Settings can say so).
//! `stored` data and state requests wait in a sealed inbox until the application takes them;
//! `live` data is never written down and is dropped when it arrives late. A snapshot (`AppState`)
//! is only accepted when this device asked for that channel's state, from that conversation.

use crate::groups;
use crate::ops;
use crate::state::Home;
use crate::transport::Target;
use crate::Outcome;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::app::{self, AppChannel, AppData, Mode};
use spike_core::attachment::Attachment;
use spike_core::envelope::{Envelope, Kind};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

const APPS: &str = "apps.json";
const INBOX: &str = "app-inbox.json";
/// Most events kept for applications to take, across all of them; the oldest go first.
pub const MAX_INBOX: usize = 5000;
/// Envelope ids remembered so a redelivered one is not handed over twice.
const MAX_SEEN: usize = 8192;
/// Applications that sent data here without being registered, shown in Settings.
const MAX_DROPPED: usize = 20;
/// How long a state request stays open for its answer.
pub const ASK_WINDOW_MS: u64 = 10 * 60 * 1000;
/// Longest display name for an application.
pub const MAX_APP_NAME: usize = 64;

/// The registry and inbox are read, changed and written back by receives and by applications taking
/// their events at the same time (different tasks of one process): one lock keeps those steps whole.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn locked() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// An application this device accepts data for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Registered {
    pub app: String,
    /// What the person sees ("Center Work").
    pub name: String,
    pub added: u64,
    /// SHA-256 (hex) of the token a program on this computer uses to act for it (desktop local API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Last time data went either way, ms.
    #[serde(default)]
    pub last: u64,
    #[serde(default)]
    pub received: u64,
    #[serde(default)]
    pub sent: u64,
}

/// An application that sent data here but is not registered on this device.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Dropped {
    pub app: String,
    pub count: u64,
    pub last: u64,
}

/// A state request this device sent and waits on: (app, channel, conversation).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct Asked {
    app: String,
    channel: String,
    /// The group id, or the contact's owner key.
    conv: String,
    at: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Registry {
    apps: Vec<Registered>,
    dropped: Vec<Dropped>,
    asked: Vec<Asked>,
    next_seq: u64,
    /// The desktop's local API is on (desktop only).
    #[serde(default)]
    api: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Bytes the other side sent (`data`).
    Data,
    /// Someone asks for the channel's whole state; answer with [`send_state`] to `reply`.
    StateRequest,
    /// The state you asked for; take its bytes with [`snapshot`].
    State,
}

/// Something for an application: what it is, which channel, and who sent it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AppEvent {
    /// Position in this device's inbox (take events after it). 0 for live data, which is never kept.
    pub seq: u64,
    pub kind: EventKind,
    pub app: String,
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// The sender's name as this device knows them, or "you" for your other devices.
    pub from: String,
    /// The sender's owner key (hex).
    pub owner: String,
    pub device: String,
    /// From one of your own other devices.
    pub own: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The envelope id.
    pub id: String,
    /// The sender's clock, ms.
    pub ts: u64,
    /// The bytes, base64 (data only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The snapshot's file (state only). Holds its key: kept sealed here, never handed out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Attachment>,
}

impl AppEvent {
    /// Where an answer to this event goes: the group member, or the contact.
    pub fn reply_to(&self, home: &Home) -> Result<To> {
        Ok(match &self.group {
            Some(g) => To::Member {
                group: g.clone(),
                owner: self.owner.clone(),
            },
            None => To::Contact(contact_name(home, &self.owner)?),
        })
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Inbox {
    events: Vec<AppEvent>,
    seen: VecDeque<String>,
}

/// Where app data goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum To {
    /// A contact, by name: all their devices and your own other devices.
    Contact(String),
    /// A small group you are in (by id or name): every member and your own other devices.
    Group(String),
    /// One member of a small group (by owner key), used to answer a state request.
    Member { group: String, owner: String },
}

fn load(home: &Home) -> Result<Registry> {
    home.load_json(APPS)
}

fn save(home: &Home, r: &Registry) -> Result<()> {
    home.save_json(APPS, r)
}

pub fn registered(home: &Home) -> Result<Vec<Registered>> {
    Ok(load(home)?.apps)
}

pub fn dropped(home: &Home) -> Result<Vec<Dropped>> {
    Ok(load(home)?.dropped)
}

pub fn is_registered(home: &Home, app_id: &str) -> Result<bool> {
    Ok(load(home)?.apps.iter().any(|a| a.app == app_id))
}

/// Accept data for `app_id` on this device from now on. Registering again renames it.
pub fn register(home: &Home, app_id: &str, name: &str) -> Result<()> {
    let _g = locked();
    if !app::valid_app_id(app_id) {
        bail!("'{app_id}' is not an app id (use lowercase reverse-DNS, like work.center)");
    }
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_APP_NAME || name.chars().any(char::is_control) {
        bail!("an app name is 1 to {MAX_APP_NAME} characters");
    }
    let mut r = load(home)?;
    match r.apps.iter_mut().find(|a| a.app == app_id) {
        Some(a) => a.name = name.to_string(),
        None => r.apps.push(Registered {
            app: app_id.to_string(),
            name: name.to_string(),
            added: now_ms(),
            token: None,
            last: 0,
            received: 0,
            sent: 0,
        }),
    }
    r.dropped.retain(|d| d.app != app_id);
    save(home, &r)
}

/// Stop accepting data for `app_id` here: its token stops working and what waited for it is gone.
pub fn remove(home: &Home, app_id: &str) -> Result<bool> {
    let _g = locked();
    let mut r = load(home)?;
    let before = r.apps.len();
    r.apps.retain(|a| a.app != app_id);
    r.asked.retain(|a| a.app != app_id);
    let gone = r.apps.len() != before;
    save(home, &r)?;
    let mut inbox: Inbox = home.load_json(INBOX)?;
    inbox.events.retain(|e| e.app != app_id);
    home.save_json(INBOX, &inbox)?;
    Ok(gone)
}

/// Remember the hash of a program's token for `app_id` (replacing any earlier one).
pub fn set_token(home: &Home, app_id: &str, token_hash: &str) -> Result<()> {
    let _g = locked();
    let mut r = load(home)?;
    let a = r
        .apps
        .iter_mut()
        .find(|a| a.app == app_id)
        .with_context(|| format!("{app_id} is not set up on this device"))?;
    a.token = Some(token_hash.to_string());
    save(home, &r)
}

/// The application a token hash belongs to, if any.
pub fn app_for_token(home: &Home, token_hash: &str) -> Result<Option<String>> {
    Ok(load(home)?
        .apps
        .into_iter()
        .find(|a| a.token.as_deref() == Some(token_hash))
        .map(|a| a.app))
}

pub fn api_enabled(home: &Home) -> Result<bool> {
    Ok(load(home)?.api)
}

pub fn set_api(home: &Home, on: bool) -> Result<()> {
    let _g = locked();
    let mut r = load(home)?;
    r.api = on;
    save(home, &r)
}

/// Events for `app_id` after `after`, oldest first (at most `max`). Events at or before `after`
/// were taken and are forgotten now: passing the last seq you handled is the acknowledgement.
pub fn take(home: &Home, app_id: &str, after: u64, max: usize) -> Result<Vec<AppEvent>> {
    let _g = locked();
    let mut inbox: Inbox = home.load_json(INBOX)?;
    let before = inbox.events.len();
    inbox
        .events
        .retain(|e| !(e.app == app_id && e.seq <= after && e.kind != EventKind::State));
    // A snapshot stays until it was read with `snapshot`, or the next one replaces it.
    if inbox.events.len() != before {
        home.save_json(INBOX, &inbox)?;
    }
    Ok(inbox
        .events
        .iter()
        .filter(|e| e.app == app_id && e.seq > after)
        .take(max)
        .cloned()
        .map(|mut e| {
            e.snapshot = None;
            e
        })
        .collect())
}

/// The snapshot of state event `seq`, downloaded and checked, then forgotten here.
pub async fn snapshot(
    home: &Home,
    relay: impl Into<Target>,
    app_id: &str,
    seq: u64,
) -> Result<Vec<u8>> {
    let inbox: Inbox = home.load_json(INBOX)?;
    let att = inbox
        .events
        .iter()
        .find(|e| e.app == app_id && e.seq == seq && e.kind == EventKind::State)
        .and_then(|e| e.snapshot.clone())
        .context("no such state for this app")?;
    let bytes = ops::download_file(home, &relay.into(), &att).await?;
    let _g = locked();
    let mut inbox: Inbox = home.load_json(INBOX)?;
    inbox.events.retain(|e| !(e.app == app_id && e.seq == seq));
    home.save_json(INBOX, &inbox)?;
    Ok(bytes)
}

/// Who sent an app envelope, as `process` established it.
pub(crate) struct From<'a> {
    pub owner: &'a str,
    pub name: String,
    pub device: &'a str,
    pub own: bool,
}

pub(crate) fn is_app_kind(k: Kind) -> bool {
    matches!(k, Kind::App | Kind::AppStateRequest | Kind::AppState)
}

/// Handle an app envelope from someone already allowed to send it (a contact, a group member the
/// group checks admitted, or our own device). Stores what must be kept before returning.
pub(crate) fn on_envelope(home: &Home, env: &Envelope, s: From<'_>) -> Result<Outcome> {
    let _g = locked();
    let (ch, data) = match env.kind {
        Kind::App => {
            let Ok(d) = serde_json::from_str::<AppData>(&env.body) else {
                return Ok(Outcome::Invalid);
            };
            (
                AppChannel {
                    app: d.app.clone(),
                    channel: d.channel.clone(),
                },
                Some(d),
            )
        }
        _ => match serde_json::from_str::<AppChannel>(&env.body) {
            Ok(c) => (c, None),
            Err(_) => return Ok(Outcome::Invalid),
        },
    };
    let mut reg = load(home)?;
    let now = now_ms();
    let Some(ai) = reg.apps.iter().position(|a| a.app == ch.app) else {
        // Nobody here takes it: acknowledge and drop, so it does not block the queue.
        match reg.dropped.iter_mut().find(|d| d.app == ch.app) {
            Some(d) => {
                d.count += 1;
                d.last = now;
            }
            None => {
                reg.dropped.push(Dropped {
                    app: ch.app.clone(),
                    count: 1,
                    last: now,
                });
                if reg.dropped.len() > MAX_DROPPED {
                    reg.dropped.sort_by_key(|d| std::cmp::Reverse(d.last));
                    reg.dropped.truncate(MAX_DROPPED);
                }
            }
        }
        save(home, &reg)?;
        return Ok(Outcome::Ignored);
    };
    let conv = env.group.clone().unwrap_or_else(|| s.owner.to_string());
    let mut event = AppEvent {
        seq: 0,
        kind: EventKind::Data,
        app: ch.app.clone(),
        channel: ch.channel.clone(),
        mode: data.as_ref().map(|d| d.mode),
        from: s.name,
        owner: s.owner.to_string(),
        device: s.device.to_string(),
        own: s.own,
        group: env.group.clone(),
        id: env.id.clone(),
        ts: env.ts,
        data: data.as_ref().map(|d| d.data.clone()),
        snapshot: None,
    };
    match env.kind {
        Kind::App if data.as_ref().is_some_and(|d| d.mode == Mode::Live) => {
            // Never written down; late is the same as lost.
            if now.saturating_sub(env.ts) > app::LIVE_MAX_AGE_MS {
                return Ok(Outcome::Ignored);
            }
            reg.apps[ai].last = now;
            reg.apps[ai].received += 1;
            save(home, &reg)?;
            return Ok(Outcome::App(event));
        }
        Kind::App => {}
        Kind::AppStateRequest => {
            // Your own device asking is you asking: nothing for you to answer.
            if s.own {
                return Ok(Outcome::Ignored);
            }
            event.kind = EventKind::StateRequest;
        }
        _ => {
            let asked = reg.asked.iter().position(|q| {
                q.app == ch.app
                    && q.channel == ch.channel
                    && q.conv == conv
                    && now < q.at + ASK_WINDOW_MS
            });
            let Some(i) = asked.filter(|_| !s.own) else {
                return Ok(Outcome::Rejected {
                    reason: format!("{} sent app state this device did not ask for", event.from),
                });
            };
            reg.asked.remove(i);
            event.kind = EventKind::State;
            event.snapshot = env.attachment.clone();
        }
    }
    let mut inbox: Inbox = home.load_json(INBOX)?;
    if inbox.seen.contains(&env.id) {
        save(home, &reg)?;
        return Ok(Outcome::Duplicate);
    }
    inbox.seen.push_back(env.id.clone());
    while inbox.seen.len() > MAX_SEEN {
        inbox.seen.pop_front();
    }
    reg.next_seq += 1;
    event.seq = reg.next_seq;
    if event.kind == EventKind::State {
        // A newer snapshot of the same channel replaces one not read yet.
        inbox.events.retain(|e| {
            !(e.kind == EventKind::State && e.app == event.app && e.channel == event.channel)
        });
    }
    inbox.events.push(event.clone());
    if inbox.events.len() > MAX_INBOX {
        let extra = inbox.events.len() - MAX_INBOX;
        inbox.events.drain(..extra);
    }
    reg.apps[ai].last = now;
    reg.apps[ai].received += 1;
    // Inbox first: if the registry write is lost, a redelivery is caught by `seen`.
    home.save_json(INBOX, &inbox)?;
    save(home, &reg)?;
    event.snapshot = None;
    Ok(Outcome::App(event))
}

/// The name of the contact with this owner key.
fn contact_name(home: &Home, owner: &str) -> Result<String> {
    home.load_contacts()?
        .into_iter()
        .find(|c| c.conversation_key().ok().as_deref() == Some(owner))
        .map(|c| c.name)
        .context("that person is not a contact")
}

/// What a send reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub id: String,
    /// Devices of other people that took it.
    pub delivered: usize,
}

fn check_registered(home: &Home, app_id: &str) -> Result<()> {
    if !is_registered(home, app_id)? {
        bail!("{app_id} is not set up on this device");
    }
    Ok(())
}

fn note_sent(home: &Home, app_id: &str) -> Result<()> {
    let _g = locked();
    let mut r = load(home)?;
    if let Some(a) = r.apps.iter_mut().find(|a| a.app == app_id) {
        a.sent += 1;
        a.last = now_ms();
    }
    save(home, &r)
}

async fn deliver(home: &Home, base: Target, to: &To, env: Envelope, quick: bool) -> Result<Sent> {
    match to {
        To::Contact(name) => {
            let r = ops::deliver_app(home, base, name, env, quick).await?;
            Ok(Sent {
                id: r.id.clone(),
                delivered: r.delivered(),
            })
        }
        To::Group(g) => {
            let r = groups::send_app(home, &base, g, None, env).await?;
            Ok(Sent {
                id: r.id.clone(),
                delivered: r
                    .devices
                    .iter()
                    .filter(|d| !d.own && d.error.is_none())
                    .count(),
            })
        }
        To::Member { group, owner } => {
            let r = groups::send_app(home, &base, group, Some(owner), env).await?;
            Ok(Sent {
                id: r.id.clone(),
                delivered: r
                    .devices
                    .iter()
                    .filter(|d| !d.own && d.error.is_none())
                    .count(),
            })
        }
    }
}

/// Send an application's bytes. `live` data skips the relay housekeeping a stored send does first.
pub async fn send(
    home: &Home,
    relay: impl Into<Target>,
    to: &To,
    app_id: &str,
    channel: &str,
    mode: Mode,
    bytes: &[u8],
) -> Result<Sent> {
    check_registered(home, app_id)?;
    let env = Envelope::app(&AppData::new(app_id, channel, mode, bytes)?)?;
    let sent = deliver(home, relay.into(), to, env, mode == Mode::Live).await?;
    note_sent(home, app_id)?;
    Ok(sent)
}

/// Ask a group (or a contact) for a channel's whole state. Whoever answers first is taken.
pub async fn request_state(
    home: &Home,
    relay: impl Into<Target>,
    to: &To,
    app_id: &str,
    channel: &str,
) -> Result<Sent> {
    check_registered(home, app_id)?;
    let conv = match to {
        To::Contact(name) => home
            .load_contacts()?
            .into_iter()
            .find(|c| &c.name == name)
            .with_context(|| format!("no contact '{name}'"))?
            .conversation_key()?,
        To::Group(g) => groups::find(home, g)?.id().to_string(),
        To::Member { .. } => bail!("ask the whole group for its state"),
    };
    let env = Envelope::app_state_request(&AppChannel::new(app_id, channel)?)?;
    // Open the window before sending, so a fast answer is not refused.
    let g = locked();
    let mut r = load(home)?;
    let now = now_ms();
    r.asked.retain(|q| now < q.at + ASK_WINDOW_MS);
    r.asked
        .retain(|q| !(q.app == app_id && q.channel == channel && q.conv == conv));
    r.asked.push(Asked {
        app: app_id.to_string(),
        channel: channel.to_string(),
        conv,
        at: now,
    });
    save(home, &r)?;
    drop(g);
    deliver(home, relay.into(), to, env, false).await
}

/// Answer a state request: upload `snapshot` as an encrypted file and send it to `to`.
pub async fn send_state(
    home: &Home,
    relay: impl Into<Target>,
    to: &To,
    app_id: &str,
    channel: &str,
    snapshot: &[u8],
) -> Result<Sent> {
    check_registered(home, app_id)?;
    if snapshot.is_empty() {
        bail!("the state is empty");
    }
    if snapshot.len() as u64 > spike_core::attachment::MAX_FILE {
        bail!(
            "app state is limited to {} MiB",
            spike_core::attachment::MAX_FILE / (1024 * 1024)
        );
    }
    let base: Target = relay.into();
    let ch = AppChannel::new(app_id, channel)?;
    let att = ops::upload_file(home, &base, "app-state", snapshot).await?;
    let sent = deliver(home, base, to, Envelope::app_state(&ch, att)?, false).await?;
    note_sent(home, app_id)?;
    Ok(sent)
}
