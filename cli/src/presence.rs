//! Read receipts, "online now" and typing (ADR 0037).
//!
//! Three switches, all off until the owner turns them on, per device. What a contact shares is kept
//! only while the matching switch here is on too (reciprocal), and only the latest value per contact
//! is kept, sealed with the rest of the state. Presence goes only to a verified contact, and only
//! while their conversation is open (the app decides when; this module refuses the rest).

use crate::state::Home;
use crate::transport::Target;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::envelope::{Envelope, Status};
use std::collections::BTreeMap;

const FILE: &str = "presence.enc";
/// A typing signal shows for this long unless another one follows.
pub const TYPING_MS: u64 = 8_000;
/// "Online" shows for this long after the last presence signal.
pub const ONLINE_MS: u64 = 120_000;
/// Online and typing signals older than this when they arrive (an offline phone collecting its
/// queue) are dropped: they would claim someone is here who has long gone.
const STALE_MS: u64 = 60_000;

/// What this device shares, and so also what it shows of others.
#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Share {
    #[serde(default)]
    pub receipts: bool,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub typing: bool,
}

/// The latest a contact shared, by their owner key. Times are this device's clock except `read_upto`
/// (the sender's clock, which is the clock of the messages it covers: ours).
#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    #[serde(default)]
    pub read_upto: u64,
    #[serde(default)]
    pub read_at: u64,
    #[serde(default)]
    pub online_at: u64,
    #[serde(default)]
    pub typing_until: u64,
    /// The newest read point we told them (so a receipt goes once per change).
    #[serde(default)]
    pub sent_upto: u64,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
struct Stored {
    #[serde(default)]
    share: Share,
    #[serde(default)]
    peers: BTreeMap<String, Peer>,
}

fn load(home: &Home) -> Result<Stored> {
    home.load_json(FILE)
}

fn save(home: &Home, s: &Stored) -> Result<()> {
    home.save_json(FILE, s)
}

pub fn share(home: &Home) -> Result<Share> {
    Ok(load(home)?.share)
}

/// Change the switches. Turning one off forgets what was kept under it.
pub fn set_share(home: &Home, share: Share) -> Result<()> {
    let mut s = load(home)?;
    s.share = share;
    for p in s.peers.values_mut() {
        if !share.receipts {
            p.read_upto = 0;
            p.read_at = 0;
        }
        if !share.online {
            p.online_at = 0;
        }
        if !share.typing {
            p.typing_until = 0;
        }
    }
    save(home, &s)
}

/// What to show about a contact now: nothing that a switch here hides, nothing stale.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shown {
    /// Their read point on our messages (our clock), when they told us (this device's clock).
    pub read_upto: Option<u64>,
    pub read_at: Option<u64>,
    pub online: bool,
    pub typing: bool,
}

pub fn shown(home: &Home, peer: &str, now: u64) -> Result<Shown> {
    let s = load(home)?;
    let Some(p) = s.peers.get(peer) else {
        return Ok(Shown::default());
    };
    let read = s.share.receipts && p.read_upto > 0;
    Ok(Shown {
        read_upto: read.then_some(p.read_upto),
        read_at: read.then_some(p.read_at),
        online: s.share.online && p.online_at > 0 && now.saturating_sub(p.online_at) < ONLINE_MS,
        typing: s.share.typing && p.typing_until > now,
    })
}

/// A status from a contact's device (`peer`, their owner key; `sent`, their clock). Kept only while
/// the matching switch is on here. Returns whether anything changed.
pub fn on_status(home: &Home, peer: &str, status: Status, sent: u64, now: u64) -> Result<bool> {
    let mut s = load(home)?;
    let share = s.share;
    let fresh = now.saturating_sub(sent) < STALE_MS;
    let p = s.peers.entry(peer.to_string()).or_default();
    let changed = match status {
        // A read point only moves forward.
        Status::Read { upto } if share.receipts && upto > p.read_upto => {
            p.read_upto = upto;
            p.read_at = now;
            true
        }
        Status::Online if share.online && fresh => {
            p.online_at = now;
            true
        }
        Status::Typing { on } if share.typing && (fresh || !on) => {
            p.typing_until = if on { now + TYPING_MS } else { 0 };
            true
        }
        _ => false,
    };
    if changed {
        save(home, &s)?;
    }
    Ok(changed)
}

/// They sent a message: whatever they were typing is done.
pub fn on_message(home: &Home, peer: &str) -> Result<()> {
    let mut s = load(home)?;
    if let Some(p) = s.peers.get_mut(peer) {
        if p.typing_until != 0 {
            p.typing_until = 0;
            save(home, &s)?;
        }
    }
    Ok(())
}

/// Send `status` to contact `name`'s devices (not ours), if the switch allows it. Online needs a
/// verified contact. Returns false when nothing was sent because a switch or rule says no; a read
/// point that was already told is not sent again.
pub async fn send(home: &Home, base: Target, name: &str, status: Status) -> Result<bool> {
    let share = share(home)?;
    let contacts = home.load_contacts()?;
    let c = contacts
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no contact '{name}'"))?;
    let peer = c.conversation_key()?;
    let allowed = match status {
        Status::Read { .. } => share.receipts,
        Status::Online => share.online && c.verified,
        Status::Typing { .. } => share.typing,
    };
    if !allowed {
        return Ok(false);
    }
    if let Status::Read { upto } = status {
        if upto == 0 || upto <= load(home)?.peers.get(&peer).map_or(0, |p| p.sent_upto) {
            return Ok(false);
        }
    }
    if !home.this_device_is_listed()? {
        bail!("this device has been removed from your identity and can no longer send");
    }
    crate::ops::deliver_status(home, base, name, Envelope::status(status)?).await?;
    if let Status::Read { upto } = status {
        let mut s = load(home)?;
        let p = s.peers.entry(peer).or_default();
        p.sent_upto = p.sent_upto.max(upto);
        save(home, &s)?;
    }
    Ok(true)
}
