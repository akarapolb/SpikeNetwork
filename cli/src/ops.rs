//! High-level operations shared by the binary and the tests.

use crate::messages::{Direction, StoredMessage};
use crate::{client::Relay, process, state::Home, transport::Target, Outcome};
use anyhow::{anyhow, Context, Result};
use spike_core::envelope::Envelope;
use spike_core::identity::Identity;
use std::time::Duration;

/// Replenish prekeys when the relay holds fewer than this many of ours.
const LOW_WATER: usize = 10;
const TARGET: usize = 30;
/// Rotate the fallback prekey when the relay's copy is older than this (or missing).
/// Two generations of its private half are kept, so a key lives at most ~2x this.
pub const FALLBACK_ROTATE_SECS: u64 = 7 * 24 * 3600;

/// Make sure the relay holds enough of our one-time prekeys so others can start
/// sessions with us while we are offline. Returns how many were uploaded.
pub async fn top_up_prekeys(relay: &mut Relay, home: &Home, me: &mut Identity) -> Result<usize> {
    top_up_prekeys_with(relay, home, me, FALLBACK_ROTATE_SECS).await
}

/// As [`top_up_prekeys`], with an explicit fallback rotation age (for tests).
pub async fn top_up_prekeys_with(
    relay: &mut Relay,
    home: &Home,
    me: &mut Identity,
    rotate_after_secs: u64,
) -> Result<usize> {
    let mut uploaded = 0;
    if relay.prekeys < LOW_WATER {
        let keys = me.fresh_prekeys(TARGET - relay.prekeys);
        // Persist the private halves before the public halves leave the device.
        home.save_identity(me)?;
        relay.publish_prekeys(&keys).await?;
        uploaded = keys.len();
    }
    // The fallback is what keeps a drained device reachable, so keep it fresh.
    if relay
        .fallback_age_secs
        .is_none_or(|age| age > rotate_after_secs)
    {
        let key = me.fresh_fallback_prekey();
        home.save_identity(me)?;
        relay.publish_fallback(&key).await?;
    }
    Ok(uploaded)
}

#[derive(Debug)]
pub struct SendReport {
    /// Envelope id of the stored message.
    pub id: String,
    pub relay_id: String,
    pub bytes: usize,
    pub new_session: bool,
    pub verified: bool,
}

pub async fn send(
    home: &Home,
    relay: impl Into<Target>,
    name: &str,
    body: &[u8],
) -> Result<SendReport> {
    let mut me = home.load_identity()?;
    let mut sessions = home.load_sessions()?;
    let contacts = home.load_contacts()?;
    let c = contacts
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no contact '{name}'"))?;
    let peer = c.decode()?;

    let mut relay = Relay::connect(relay, &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;

    let new_session = !sessions.has_session(&peer);
    if new_session {
        let prekey = relay.claim_prekey(&peer.device_id()).await?.ok_or_else(|| {
            anyhow!("{name} has no prekeys on this relay; they must connect once first (`spike keys`)")
        })?;
        sessions.start_outbound(&me, &peer, &prekey)?;
    }
    let text = std::str::from_utf8(body).context("messages must be UTF-8 text")?;
    let env = Envelope::text(text)?;
    let blob = sessions.encrypt(&peer, &env.to_bytes())?;
    // The ratchet has advanced; persist before the message leaves the device.
    home.save_sessions(&sessions)?;
    let relay_id = relay.send(&peer.device_id(), &blob).await?;
    // Stored once the relay has accepted it; a failed send is not in history.
    home.messages()?.insert(&StoredMessage {
        id: env.id.clone(),
        peer: hex::encode(peer.x25519),
        direction: Direction::Out,
        ts: env.ts,
        body: env.body,
    })?;
    Ok(SendReport {
        id: env.id,
        relay_id,
        bytes: blob.len(),
        new_session,
        verified: c.verified,
    })
}

/// Connect, receive until `idle` passes with nothing new (forever if `None`),
/// calling `on` for each outcome and acking after it has been persisted.
pub async fn recv(
    home: &Home,
    relay: impl Into<Target>,
    idle: Option<Duration>,
    mut on: impl FnMut(Outcome),
) -> Result<()> {
    let mut me = home.load_identity()?;
    let mut sessions = home.load_sessions()?;
    let mut relay = Relay::connect(relay, &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;
    while let Some(d) = relay.next_delivery(idle).await? {
        on(process(home, &mut me, &mut sessions, &d.blob)?);
        relay.ack(&d.id).await?;
    }
    Ok(())
}

/// Connect once so the relay holds our prekeys. Returns how many it now has.
pub async fn publish_keys(home: &Home, relay: impl Into<Target>) -> Result<usize> {
    let mut me = home.load_identity()?;
    let mut relay = Relay::connect(relay, &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;
    Ok(relay.prekeys)
}
