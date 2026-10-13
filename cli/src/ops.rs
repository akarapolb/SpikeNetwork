//! High-level operations shared by the binary and the tests.

use crate::{client::Relay, process, state::Home, Outcome};
use anyhow::{anyhow, Context, Result};
use spike_core::identity::Identity;
use std::time::Duration;

/// Replenish prekeys when the relay holds fewer than this many of ours.
const LOW_WATER: usize = 10;
const TARGET: usize = 30;

/// Make sure the relay holds enough of our one-time prekeys so others can start
/// sessions with us while we are offline. Returns how many were uploaded.
pub async fn top_up_prekeys(relay: &mut Relay, home: &Home, me: &mut Identity) -> Result<usize> {
    if relay.prekeys >= LOW_WATER {
        return Ok(0);
    }
    let keys = me.fresh_prekeys(TARGET - relay.prekeys);
    // Persist the private halves before the public halves leave the device.
    home.save_identity(me)?;
    relay.publish_prekeys(&keys).await?;
    Ok(keys.len())
}

#[derive(Debug)]
pub struct SendReport {
    pub relay_id: String,
    pub bytes: usize,
    pub new_session: bool,
    pub verified: bool,
}

pub async fn send(home: &Home, url: &str, name: &str, body: &[u8]) -> Result<SendReport> {
    let mut me = home.load_identity()?;
    let mut sessions = home.load_sessions()?;
    let contacts = home.load_contacts()?;
    let c = contacts
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no contact '{name}'"))?;
    let peer = c.decode()?;

    let mut relay = Relay::connect(url, &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;

    let new_session = !sessions.has_session(&peer);
    if new_session {
        let prekey = relay.claim_prekey(&peer.device_id()).await?.ok_or_else(|| {
            anyhow!("{name} has no prekeys on this relay; they must connect once first (`spike keys`)")
        })?;
        sessions.start_outbound(&me, &peer, &prekey)?;
    }
    let blob = sessions.encrypt(&peer, body)?;
    // The ratchet has advanced; persist before the message leaves the device.
    home.save_sessions(&sessions)?;
    let relay_id = relay.send(&peer.device_id(), &blob).await?;
    Ok(SendReport {
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
    url: &str,
    idle: Option<Duration>,
    mut on: impl FnMut(Outcome),
) -> Result<()> {
    let mut me = home.load_identity()?;
    let mut sessions = home.load_sessions()?;
    let mut relay = Relay::connect(url, &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;
    while let Some(d) = relay.next_delivery(idle).await? {
        on(process(home, &mut me, &mut sessions, &d.blob)?);
        relay.ack(&d.id).await?;
    }
    Ok(())
}

/// Connect once so the relay holds our prekeys. Returns how many it now has.
pub async fn publish_keys(home: &Home, url: &str) -> Result<usize> {
    let mut me = home.load_identity()?;
    let mut relay = Relay::connect(url, &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;
    Ok(relay.prekeys)
}
