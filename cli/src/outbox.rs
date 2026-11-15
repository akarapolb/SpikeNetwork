//! Copies of one-to-one messages that could not be handed to a relay yet (ADR 0040).
//!
//! When a message (or an edit, deletion or reaction) is sent, each recipient device gets its own
//! copy through the relay that device reads from. If that relay cannot be reached, or turns the
//! copy away for a reason that passes (busy, full), the copy is kept here and sent again later:
//! on every sync, on reconnect, and before the next message to the same person.
//!
//! What is kept is the envelope's plaintext, not the sealed blob. It is encrypted again at retry
//! time with whatever session is current then, because a blob sealed days ago may belong to a
//! session or chain the recipient has since dropped. A copy that was in fact accepted before we
//! noticed (a reply lost on the way back) arrives twice; receivers already ignore a second copy
//! of the same message id, and edits, deletions and reactions apply by Lamport time, so that is
//! harmless. The file is sealed like the rest of the state directory (`outbox.enc`).
//!
//! A copy is given up on after [`EXPIRY_MS`], or at once if the relay refuses it for good (too
//! large, malformed); a message none of whose counted copies reached anyone is then recorded as
//! not sent, which the apps show.

use crate::client::RelayRefused;
use crate::ops::Pool;
use crate::state::Home;
use crate::transport::Target;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use spike_core::identity::DeviceCard;
use spike_core::owner::IdentityCard;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const FILE: &str = "outbox.enc";

/// How long a copy is kept before it is given up on: 30 days.
pub const EXPIRY_MS: u64 = 30 * 24 * 3600 * 1000;

/// First wait before a copy is tried again on its own (a send to the same person always tries).
const FIRST_BACKOFF_MS: u64 = 15_000;
/// The longest wait between two tries.
const MAX_BACKOFF_MS: u64 = 5 * 60_000;
/// How many given-up message ids are remembered.
const MAX_GAVE_UP: usize = 1000;

/// A relay could not be reached at all (no connection, or the connection broke).
#[derive(Debug)]
pub struct Unreachable {
    pub url: String,
    pub why: String,
}

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "connecting to {}: {}", self.url, self.why)
    }
}
impl std::error::Error for Unreachable {}

/// Mark a connection or transport failure on `url` as [`Unreachable`]. A sign-in refusal and an
/// error the relay itself answered with stay what they are.
pub(crate) fn unreachable(url: &str, e: anyhow::Error) -> anyhow::Error {
    if e.downcast_ref::<RelayRefused>().is_some() || e.downcast_ref::<Unreachable>().is_some() {
        return e;
    }
    let why = format!("{e:#}");
    if why.starts_with("relay error:") {
        return e;
    }
    anyhow::Error::new(Unreachable {
        url: url.to_string(),
        why,
    })
}

/// Whether a copy that failed this way is worth sending again later.
pub fn retryable(e: &anyhow::Error) -> bool {
    if e.chain().any(|c| c.is::<Unreachable>()) {
        return true;
    }
    let s = format!("{e:#}");
    // Passing refusals: the relay is busy or the mailbox is full, or the device has not published
    // prekeys there yet.
    [
        "relay error: rate_limited",
        "relay error: queue_full",
        "relay error: mailbox_full",
        "relay error: quota_exceeded",
        "relay error: internal",
        "has no prekeys",
    ]
    .iter()
    .any(|p| s.contains(p))
}

/// Whether this failure was the relay being out of reach (rather than refusing).
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Unreachable>())
}

/// One undelivered copy, for one device.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    /// Envelope id of the message (or edit, deletion, reaction) this is a copy of.
    pub msg_id: String,
    /// The conversation the message belongs to (peer owner key, or our own for notes).
    pub conv: String,
    /// Owner key (hex) of the identity whose signed list names the device.
    pub owner: String,
    pub device_id: String,
    /// One of our own other devices.
    pub own: bool,
    /// Delivering this copy is what makes the message count as sent (a recipient's device; for a
    /// note to yourself, your own). Sent copies to our own devices do not count.
    pub counts: bool,
    /// The envelope plaintext, hex. Encrypted again at retry time.
    plaintext: String,
    pub queued_at: u64,
    pub attempts: u32,
    pub next_at: u64,
    pub last_error: String,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct Book {
    entries: Vec<Entry>,
    /// Messages one of whose counted copies has been accepted already: not waiting any more.
    accepted: BTreeSet<String>,
    /// Messages given up on before any counted copy was accepted: id -> when.
    gave_up: BTreeMap<String, u64>,
}

/// What the apps show about one of our messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Handed to a relay (or nothing is known against it).
    Sent,
    /// No recipient device has it yet; a copy is waiting in the outbox.
    Waiting,
    /// Given up on before it reached anyone.
    NotSent,
}

/// The state of every message that is not plainly sent, by message id.
pub struct States {
    waiting: HashMap<String, usize>,
    gave_up: HashSet<String>,
}

impl States {
    pub fn of(&self, msg_id: &str) -> State {
        if self.waiting.contains_key(msg_id) {
            State::Waiting
        } else if self.gave_up.contains(msg_id) {
            State::NotSent
        } else {
            State::Sent
        }
    }
}

/// Read the states once (for one screen of messages).
pub fn states(home: &Home) -> Result<States> {
    let b: Book = home.load_json(FILE)?;
    let mut waiting = HashMap::new();
    for e in &b.entries {
        if e.counts && !b.accepted.contains(&e.msg_id) {
            *waiting.entry(e.msg_id.clone()).or_insert(0) += 1;
        }
    }
    Ok(States {
        waiting,
        gave_up: b.gave_up.into_keys().collect(),
    })
}

/// Conversation key -> how many of its messages are waiting to be sent.
pub fn waiting_by_conversation(home: &Home) -> Result<HashMap<String, usize>> {
    let b: Book = home.load_json(FILE)?;
    let mut ids: HashMap<String, BTreeSet<&str>> = HashMap::new();
    for e in &b.entries {
        if e.counts && !b.accepted.contains(&e.msg_id) {
            ids.entry(e.conv.clone()).or_default().insert(&e.msg_id);
        }
    }
    Ok(ids.into_iter().map(|(k, v)| (k, v.len())).collect())
}

/// Every copy still queued (for tests and diagnostics).
pub fn entries(home: &Home) -> Result<Vec<Entry>> {
    Ok(home.load_json::<Book>(FILE)?.entries)
}

/// Message ids given up on, with when.
pub fn given_up(home: &Home) -> Result<BTreeMap<String, u64>> {
    Ok(home.load_json::<Book>(FILE)?.gave_up)
}

/// A copy to keep, as `ops` hands it over after a send.
pub(crate) struct Failed {
    pub card: DeviceCard,
    pub owner: String,
    pub own: bool,
    pub counts: bool,
    pub error: String,
}

/// Keep the copies of one message that could not be handed over. `accepted`: one of its counted
/// copies did reach a relay, so the message is not waiting even while these are.
pub(crate) fn keep(
    home: &Home,
    msg_id: &str,
    conv: &str,
    plaintext: &[u8],
    failed: Vec<Failed>,
    accepted: bool,
    now: u64,
) -> Result<()> {
    if failed.is_empty() {
        return Ok(());
    }
    let mut b: Book = home.load_json(FILE)?;
    for f in failed {
        b.entries.push(Entry {
            msg_id: msg_id.to_string(),
            conv: conv.to_string(),
            owner: f.owner,
            device_id: f.card.device_id(),
            own: f.own,
            counts: f.counts,
            plaintext: hex::encode(plaintext),
            queued_at: now,
            attempts: 1,
            // The next sync tries it at once; backoff starts after a retry fails.
            next_at: now,
            last_error: f.error,
        });
    }
    if accepted {
        b.accepted.insert(msg_id.to_string());
    }
    home.save_json(FILE, &b)
}

/// What one pass over the outbox did.
#[derive(Debug, Default)]
pub struct Flushed {
    /// Copies handed to a relay this time.
    pub delivered: usize,
    /// Copies tried and still waiting.
    pub failed: usize,
    /// Copies dropped: expired, refused for good, or their device is no longer listed.
    pub dropped: usize,
    /// Relays that could not be reached during this pass.
    pub unreachable: HashSet<String>,
}

impl Flushed {
    /// Whether ratchet state on disk may have changed (callers holding sessions must reload).
    pub fn touched(&self) -> bool {
        self.delivered + self.failed > 0
    }
}

/// Try every copy that is due. Called after each sync and when the connection comes back.
pub async fn flush(home: &Home, base: impl Into<Target>) -> Result<Flushed> {
    flush_at(home, base, spike_core::clock::now_ms()).await
}

/// [`flush`] as if the time were `now` (ms). Tests use it to reach the expiry.
pub async fn flush_at(home: &Home, base: impl Into<Target>, now: u64) -> Result<Flushed> {
    run(home, &base.into(), now, |e| e.next_at <= now).await
}

/// Try every waiting copy now, whatever its backoff: the connection has just come back.
pub async fn flush_all(home: &Home, base: impl Into<Target>) -> Result<Flushed> {
    run(home, &base.into(), spike_core::clock::now_ms(), |_| true).await
}

/// Before a send to `owner` (and our own copies): try that person's waiting copies now, whatever
/// their backoff, so older messages go first. Returns the relays found out of reach, which the
/// send then does not try again.
pub(crate) async fn flush_for(home: &Home, base: &Target, owner: &str) -> Result<Flushed> {
    let now = spike_core::clock::now_ms();
    let mine = home.load_device_list()?.owner;
    run(home, base, now, |e| {
        e.owner == owner || (e.owner == mine && e.next_at <= now)
    })
    .await
}

fn backoff(attempts: u32) -> u64 {
    FIRST_BACKOFF_MS
        .saturating_mul(1u64 << attempts.min(16))
        .min(MAX_BACKOFF_MS)
}

/// The device an entry is for, and the relay it reads from, from the current signed lists.
fn locate(
    e: &Entry,
    mine: &IdentityCard,
    contacts: &[crate::state::Contact],
) -> Option<(DeviceCard, Option<String>)> {
    let list = if e.own {
        mine.clone()
    } else {
        contacts
            .iter()
            .filter_map(|c| c.identity().ok())
            .find(|id| id.owner_hex() == e.owner)?
    };
    list.device_relays()
        .into_iter()
        .find(|(d, _)| d.device_id() == e.device_id)
}

async fn run(
    home: &Home,
    base: &Target,
    now: u64,
    pick: impl Fn(&Entry) -> bool,
) -> Result<Flushed> {
    let mut out = Flushed::default();
    let mut b: Book = home.load_json(FILE)?;
    if b.entries.is_empty() {
        return Ok(out);
    }
    let before = (b.entries.len(), b.accepted.len(), b.gave_up.len());

    // Expired copies are given up on first.
    let (expired, kept): (Vec<Entry>, Vec<Entry>) = b
        .entries
        .drain(..)
        .partition(|e| now.saturating_sub(e.queued_at) >= EXPIRY_MS);
    b.entries = kept;
    let mut lost: Vec<Entry> = expired;
    out.dropped += lost.len();

    if b.entries.iter().any(&pick) {
        let me = home.load_identity()?;
        let mut sessions = home.load_sessions()?;
        let mine = IdentityCard::decode(&home.identity_card()?)?;
        let contacts = home.load_contacts()?;
        let mut pool = Pool {
            base: base.clone(),
            me: &me,
            conns: HashMap::new(),
        };
        let mut left = Vec::with_capacity(b.entries.len());
        for mut e in std::mem::take(&mut b.entries) {
            if !pick(&e) {
                left.push(e);
                continue;
            }
            let Some((card, relay)) = locate(&e, &mine, &contacts) else {
                // The device left its owner's list, or the contact is gone.
                out.dropped += 1;
                lost.push(e);
                continue;
            };
            let url = relay.unwrap_or_else(|| base.url.clone());
            let result = if out.unreachable.contains(&url) {
                Err(anyhow::Error::new(Unreachable {
                    url: url.clone(),
                    why: "not reachable a moment ago".into(),
                }))
            } else {
                let Ok(plaintext) = hex::decode(&e.plaintext) else {
                    out.dropped += 1;
                    continue;
                };
                crate::ops::send_to_device(&mut pool, home, &mut sessions, &card, &url, &plaintext)
                    .await
            };
            match result {
                Ok(_) => {
                    out.delivered += 1;
                    if e.counts {
                        b.accepted.insert(e.msg_id.clone());
                    }
                }
                Err(err) if retryable(&err) => {
                    if is_unreachable(&err) {
                        out.unreachable.insert(url);
                    }
                    out.failed += 1;
                    e.attempts += 1;
                    e.next_at = now + backoff(e.attempts);
                    e.last_error = format!("{err:#}");
                    left.push(e);
                }
                Err(err) => {
                    eprintln!(
                        "note: gave up on a message copy for device {}: {err:#}",
                        &e.device_id[..e.device_id.len().min(8)]
                    );
                    out.dropped += 1;
                    lost.push(e);
                }
            }
        }
        b.entries = left;
    }

    // A message none of whose counted copies got through, and none is left to try: not sent.
    for e in &lost {
        let still = b.entries.iter().any(|x| x.msg_id == e.msg_id && x.counts);
        if e.counts && !still && !b.accepted.contains(&e.msg_id) {
            b.gave_up.insert(e.msg_id.clone(), now);
        }
    }
    // Forget acceptances of messages with nothing left queued, and the oldest give-ups.
    let queued: HashSet<&str> = b.entries.iter().map(|e| e.msg_id.as_str()).collect();
    b.accepted.retain(|id| queued.contains(id.as_str()));
    while b.gave_up.len() > MAX_GAVE_UP {
        let oldest = b
            .gave_up
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(id, _)| id.clone())
            .expect("not empty");
        b.gave_up.remove(&oldest);
    }
    if out.touched()
        || out.dropped > 0
        || before != (b.entries.len(), b.accepted.len(), b.gave_up.len())
    {
        home.save_json(FILE, &b)?;
    }
    Ok(out)
}
