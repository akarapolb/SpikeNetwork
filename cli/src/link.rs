//! Device linking flows (multi-device milestone M4). Crypto and framing live in
//! `spike_core::link`; this module moves frames through relays and installs the
//! bundle. See `docs/adr/0010-device-linking.md`.
//!
//! New device:      `offer` -> show code -> `await_hello` -> compare code -> `receive`
//! Existing device: `begin(code)` -> compare code -> `finish(mode)`

use crate::messages::StoredMessage;
use crate::state::{Contact, Home};
use crate::{client::Relay, ops, transport::Target};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::link::{is_link_frame, Assembler, LinkOffer, LinkSession, NewDevice};
use spike_core::owner::{DeviceEntry, DeviceList, OwnerKey};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How much history the new device receives. Text only: attachments do not exist yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum History {
    /// Every stored message (the design's default for text).
    All,
    /// Only messages from the last N days.
    Recent { days: u64 },
    /// Start fresh: no history.
    None,
}

/// What travels, encrypted, from the existing device to the new one.
#[derive(Serialize, Deserialize)]
struct Bundle {
    v: u8,
    /// The person's owner key (secret). See ADR 0010 for why every device holds it.
    owner_key: String,
    device_list: DeviceList,
    contacts: Vec<Contact>,
    messages: Vec<StoredMessage>,
}

// ---------- new device ----------

/// The new device's waiting state. The ephemeral key lives only in memory.
pub struct Waiting {
    pub code: String,
    new_device: NewDevice,
    target: Target,
}

/// Create a link code for this (new) device. Its home relay becomes `relay`.
pub fn offer(home: &Home, relay: impl Into<Target>) -> Result<Waiting> {
    let target: Target = relay.into();
    let card = home.load_identity()?.card();
    let new_device = NewDevice::new(&card, Some(target.url.clone()));
    Ok(Waiting {
        code: new_device.offer.encode(),
        new_device,
        target,
    })
}

/// Wait for the existing device's hello and derive the safety code.
pub async fn await_hello(home: &Home, w: &Waiting, timeout: Duration) -> Result<LinkSession> {
    let me = home.load_identity()?;
    let mut relay = Relay::connect(w.target.clone(), &me).await?;
    while let Some(d) = relay.next_delivery(Some(timeout)).await? {
        relay.ack(&d.id).await?;
        if is_link_frame(&d.blob) {
            if let Ok(s) = w.new_device.on_hello(&d.blob) {
                return Ok(s);
            }
        }
    }
    bail!("timed out waiting for the existing device (did it run `spike link-device` with this code?)")
}

/// What the new device installed.
#[derive(Debug)]
pub struct Installed {
    pub version: u64,
    pub contacts: usize,
    pub messages: usize,
}

/// Receive the bundle and install it. Call only after the person confirmed
/// that both screens show the same safety code.
pub async fn receive(
    home: &Home,
    w: &Waiting,
    session: &LinkSession,
    timeout: Duration,
) -> Result<Installed> {
    let me = home.load_identity()?;
    let mut relay = Relay::connect(w.target.clone(), &me).await?;
    let mut asm = Assembler::new(session);
    let payload = loop {
        let Some(d) = relay.next_delivery(Some(timeout)).await? else {
            bail!("timed out waiting for the history bundle");
        };
        relay.ack(&d.id).await?;
        // Frames that do not open under this session (strangers, an attacker) are skipped.
        if is_link_frame(&d.blob) && asm.add(&d.blob).is_ok() {
            if let Some(p) = asm.finish() {
                break p;
            }
        }
    };
    let b: Bundle = serde_json::from_slice(&payload).context("history bundle is malformed")?;
    if b.v != 1 {
        bail!("unsupported bundle version {}", b.v);
    }
    let owner = OwnerKey::from_hex(&b.owner_key)?;
    // Checks the list is signed by this owner and includes this device.
    home.install_identity(&owner, &b.device_list)?;
    let mut contacts = home.load_contacts()?;
    for c in &b.contacts {
        match contacts.iter_mut().find(|x| x.name == c.name) {
            Some(x) => *x = c.clone(),
            None => contacts.push(c.clone()),
        }
    }
    home.save_contacts(&contacts)?;
    let store = home.messages()?;
    let mut n = 0;
    for m in &b.messages {
        n += usize::from(store.insert(m)?);
    }
    drop(store);
    // Join the network: prekeys and the (already newer) device list on our relay.
    ops::publish_keys(home, w.target.clone()).await?;
    Ok(Installed {
        version: b.device_list.version,
        contacts: b.contacts.len(),
        messages: n,
    })
}

// ---------- existing device ----------

/// The existing device's state between the hello and the person's confirmation.
pub struct Pending {
    offer: LinkOffer,
    session: LinkSession,
    pub safety_code: String,
    pub new_device_id: String,
}

/// Read the new device's code and send it a hello. Nothing is shared yet.
pub async fn begin(home: &Home, relay: impl Into<Target>, code: &str) -> Result<Pending> {
    let base: Target = relay.into();
    let offer = LinkOffer::decode(code)?;
    let card = offer.device_card()?;
    if home
        .load_device_list()?
        .verify()?
        .iter()
        .any(|c| c == &card)
    {
        bail!("that device is already linked to your identity");
    }
    let (session, hello) = LinkSession::approve(&offer)?;
    let url = offer.relay.clone().unwrap_or_else(|| base.url.clone());
    let me = home.load_identity()?;
    let mut r = Relay::connect(
        Target {
            url,
            opts: base.opts.clone(),
        },
        &me,
    )
    .await?;
    r.send(&card.device_id(), &hello).await?;
    Ok(Pending {
        safety_code: session.safety_code.clone(),
        new_device_id: card.device_id(),
        offer,
        session,
    })
}

/// What the existing device did.
#[derive(Debug)]
pub struct Linked {
    pub version: u64,
    pub messages: usize,
    pub contacts: usize,
}

/// After the person confirmed matching codes: sign and publish the new device
/// list, then send the encrypted bundle.
pub async fn finish(
    home: &Home,
    relay: impl Into<Target>,
    p: Pending,
    history: History,
) -> Result<Linked> {
    let base: Target = relay.into();
    let owner = home.load_owner()?;
    let list = home.load_device_list()?.with_device(
        &owner,
        DeviceEntry {
            card: p.offer.card.clone(),
            relay: p.offer.relay.clone(),
        },
    )?;
    home.save_device_list(&list)?;
    let me = home.load_identity()?;
    Relay::connect(base.clone(), &me)
        .await?
        .publish_device_list(&list)
        .await?;

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let messages: Vec<StoredMessage> = match history {
        History::None => Vec::new(),
        History::All => home.messages()?.all()?,
        History::Recent { days } => {
            let cut = now.saturating_sub(days * 86_400_000);
            home.messages()?
                .all()?
                .into_iter()
                .filter(|m| m.ts >= cut)
                .collect()
        }
    };
    let contacts = home.load_contacts()?;
    let bundle = Bundle {
        v: 1,
        owner_key: owner.to_hex(),
        device_list: list.clone(),
        contacts,
        messages,
    };
    let frames = p.session.seal(&serde_json::to_vec(&bundle)?)?;
    let url = p.offer.relay.clone().unwrap_or_else(|| base.url.clone());
    let mut r = Relay::connect(
        Target {
            url,
            opts: base.opts.clone(),
        },
        &me,
    )
    .await?;
    for f in &frames {
        r.send(&p.new_device_id, f).await?;
    }
    Ok(Linked {
        version: list.version,
        messages: bundle.messages.len(),
        contacts: bundle.contacts.len(),
    })
}
