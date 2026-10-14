//! High-level operations shared by the binary and the tests.
//!
//! Multi-device (milestone M3): a message is encrypted separately for every
//! device of the recipient and for the sender's own other devices ("sent
//! copies"), and each copy is delivered to the relay that device reads from.
//! Device lists are published on and fetched from relays, and only accepted
//! under the update rules of ADR 0008.

use crate::messages::{Direction, StoredMessage};
use crate::state::{Added, OwnUpdate};
use crate::sync::{self, SyncMsg};
use crate::{client::Relay, process, state::Home, transport::Target, Outcome};
use anyhow::{anyhow, bail, Context, Result};
use spike_core::attachment::{seal_file, Attachment};
use spike_core::envelope::{Envelope, Kind};
use spike_core::identity::{DeviceCard, Identity};
use spike_core::owner::IdentityCard;
use spike_core::session::Sessions;
use std::collections::HashMap;
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

/// Bring our own device list up to date with the relay, then publish ours.
/// Returns human-readable notes about anything that changed.
pub async fn sync_own_list(relay: &mut Relay, home: &Home) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let mine = home.load_device_list()?;
    if let Some(offered) = relay.fetch_device_list(&mine.owner).await? {
        match home.accept_own_list(&offered) {
            Ok(OwnUpdate::Unchanged) => {}
            Ok(OwnUpdate::Updated { version }) => {
                notes.push(format!("your device list updated to v{version}"))
            }
            Ok(OwnUpdate::RemovedThisDevice { version }) => notes.push(format!(
                "THIS DEVICE WAS REMOVED from your identity (device list v{version})"
            )),
            Err(e) => notes.push(format!("ignored your device list from the relay: {e}")),
        }
    }
    if home.this_device_is_listed()? {
        // A rollback reply here only means the relay already has something newer.
        if let Err(e) = relay.publish_device_list(&home.load_device_list()?).await {
            notes.push(format!("could not publish your device list: {e}"));
        }
    }
    Ok(notes)
}

/// Fetch the latest device list of one contact (or all, if `name` is None)
/// and apply it under the update rules. Legacy single-device contacts are skipped.
pub async fn refresh_contacts(
    relay: &mut Relay,
    home: &Home,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for c in home.load_contacts()? {
        if name.is_some_and(|n| n != c.name) {
            continue;
        }
        let id = c.identity()?;
        if id.is_legacy() {
            continue;
        }
        let Some(list) = relay.fetch_device_list(&id.owner_hex()).await? else {
            continue;
        };
        match home.add_contact(&c.name, &IdentityCard::encode(&list), false) {
            Ok(Added::Updated { version }) => notes.push(format!(
                "{}'s devices changed (list v{version}, {} devices)",
                c.name,
                list.devices.len()
            )),
            Ok(_) => {}
            Err(e) => notes.push(format!(
                "ignored {}'s device list from the relay: {e}",
                c.name
            )),
        }
    }
    Ok(notes)
}

/// Connections to every relay a send needs, opened on demand.
struct Pool<'a> {
    base: Target,
    me: &'a Identity,
    conns: HashMap<String, Relay>,
}

impl<'a> Pool<'a> {
    async fn get(&mut self, url: &str) -> Result<&mut Relay> {
        if !self.conns.contains_key(url) {
            let t = Target {
                url: url.into(),
                opts: self.base.opts.clone(),
            };
            let r = Relay::connect(t, self.me)
                .await
                .with_context(|| format!("connecting to {url}"))?;
            self.conns.insert(url.into(), r);
        }
        Ok(self.conns.get_mut(url).expect("just inserted"))
    }
}

/// The outcome for one recipient device.
#[derive(Debug)]
pub struct DeviceSend {
    pub device_id: String,
    /// One of the sender's own other devices (a sent copy).
    pub own: bool,
    pub new_session: bool,
    /// `None` if the relay accepted it.
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct SendReport {
    /// Envelope id of the stored message.
    pub id: String,
    pub verified: bool,
    pub devices: Vec<DeviceSend>,
    /// Device-list changes noticed while sending.
    pub notes: Vec<String>,
}

impl SendReport {
    /// Whether a new session was started with any of the recipient's devices.
    pub fn new_session(&self) -> bool {
        self.devices.iter().any(|d| !d.own && d.new_session)
    }
    /// How many of the recipient's devices got it.
    pub fn delivered(&self) -> usize {
        self.devices
            .iter()
            .filter(|d| !d.own && d.error.is_none())
            .count()
    }
}

async fn send_to_device(
    pool: &mut Pool<'_>,
    home: &Home,
    sessions: &mut Sessions,
    card: &DeviceCard,
    url: &str,
    plaintext: &[u8],
) -> Result<bool> {
    let me = pool.me;
    let relay = pool.get(url).await?;
    let new = !sessions.has_session(card);
    if new {
        let prekey = relay
            .claim_prekey(&card.device_id())
            .await?
            .ok_or_else(|| anyhow!("device has no prekeys on {url}; it must connect once first"))?;
        sessions.start_outbound(me, card, &prekey)?;
    }
    let blob = sessions.encrypt(card, plaintext)?;
    // The ratchet has advanced; persist before the message leaves the device.
    home.save_sessions(sessions)?;
    relay.send(&card.device_id(), &blob).await?;
    Ok(new)
}

pub async fn send(
    home: &Home,
    relay: impl Into<Target>,
    name: &str,
    body: &[u8],
) -> Result<SendReport> {
    let text = std::str::from_utf8(body).context("messages must be UTF-8 text")?;
    deliver(home, relay.into(), name, Envelope::text(text)?).await
}

/// Encrypt a file, upload its chunks to our home relay, then send the message
/// carrying its descriptor (and `caption`) to every device, like any message.
pub async fn send_file(
    home: &Home,
    relay: impl Into<Target>,
    name: &str,
    file_name: &str,
    bytes: &[u8],
    caption: &str,
) -> Result<SendReport> {
    let base: Target = relay.into();
    let (mut att, chunks) = seal_file(file_name, bytes)?;
    att.relay = Some(base.url.clone());
    {
        let me = home.load_identity()?;
        let mut r = Relay::connect(base.clone(), &me).await?;
        for (i, c) in chunks.iter().enumerate() {
            r.put_blob_chunk(&att.blob, i as u32, att.chunks, c)
                .await
                .context("uploading attachment")?;
        }
    }
    let mut env = Envelope::text(caption)?;
    env.attachment = Some(att);
    let report = deliver(home, base, name, env).await?;
    // The sender keeps its own copy, so its history never needs the relay.
    home.save_attachment(&report.id, bytes)?;
    Ok(report)
}

/// The decrypted bytes of a message's attachment: from this device if kept,
/// otherwise downloaded from the relay holding it, checked, and kept.
pub async fn fetch_attachment(
    home: &Home,
    relay: impl Into<Target>,
    msg_id: &str,
) -> Result<(Attachment, Vec<u8>)> {
    let base: Target = relay.into();
    let m = home.messages()?.find(msg_id)?;
    let att = m
        .attachment
        .clone()
        .ok_or_else(|| anyhow!("message {} has no attachment", &m.id[..8]))?;
    if let Some(bytes) = home.load_attachment(&m.id)? {
        return Ok((att, bytes));
    }
    let url = att.relay.clone().unwrap_or_else(|| base.url.clone());
    let me = home.load_identity()?;
    let mut r = Relay::connect(
        Target {
            url: url.clone(),
            opts: base.opts.clone(),
        },
        &me,
    )
    .await?;
    let mut bytes = Vec::with_capacity(att.size as usize);
    for i in 0..att.chunks {
        let sealed = r
            .get_blob_chunk(&att.blob, i)
            .await?
            .ok_or_else(|| anyhow!("the attachment is no longer on {url} (relays keep files for a limited time); open it on a device that already has it"))?;
        bytes.extend(
            att.open_chunk(i, &sealed)
                .map_err(|_| anyhow!("attachment chunk {i} failed its integrity check"))?,
        );
    }
    att.verify(&bytes)
        .map_err(|_| anyhow!("attachment failed its integrity check (size or hash mismatch)"))?;
    home.save_attachment(&m.id, &bytes)?;
    Ok((att, bytes))
}

/// Edit a message you sent. The new text reaches every device of the
/// recipient and your own other devices, which all merge it the same way.
pub async fn edit(
    home: &Home,
    relay: impl Into<Target>,
    msg_id: &str,
    new_text: &str,
) -> Result<SendReport> {
    let (m, contact) = own_message(home, msg_id)?;
    if m.deleted {
        bail!("that message was deleted");
    }
    deliver(
        home,
        relay.into(),
        &contact,
        Envelope::edit(&m.id, new_text)?,
    )
    .await
}

/// Delete a message you sent, on every device that has it.
pub async fn delete(home: &Home, relay: impl Into<Target>, msg_id: &str) -> Result<SendReport> {
    let (m, contact) = own_message(home, msg_id)?;
    deliver(home, relay.into(), &contact, Envelope::delete(&m.id)?).await
}

/// The message with this id (or unique prefix) if you wrote it, and the contact's name.
fn own_message(home: &Home, msg_id: &str) -> Result<(StoredMessage, String)> {
    let m = home.messages()?.find(msg_id)?;
    if m.direction != Direction::Out {
        bail!("you can only edit or delete messages you sent");
    }
    let contacts = home.load_contacts()?;
    let c = contacts
        .iter()
        .find(|c| c.conversation_key().ok().as_deref() == Some(m.peer.as_str()))
        .ok_or_else(|| anyhow!("no contact for this conversation any more"))?;
    Ok((m.clone(), c.name.clone()))
}

/// Send any envelope to a contact's devices (and your own other devices) like a
/// message. Public so tests can send hand-made envelopes.
pub async fn deliver_envelope(
    home: &Home,
    relay: impl Into<Target>,
    name: &str,
    env: Envelope,
) -> Result<SendReport> {
    deliver(home, relay.into(), name, env).await
}

async fn deliver(home: &Home, base: Target, name: &str, mut env: Envelope) -> Result<SendReport> {
    let mut me = home.load_identity()?;
    let mut sessions = home.load_sessions()?;

    // Our home relay: keep prekeys topped up and both device lists current.
    let mut notes;
    {
        let mut home_relay = Relay::connect(base.clone(), &me).await?;
        top_up_prekeys(&mut home_relay, home, &mut me).await?;
        notes = sync_own_list(&mut home_relay, home).await?;
        notes.extend(refresh_contacts(&mut home_relay, home, Some(name)).await?);
    }
    if !home.this_device_is_listed()? {
        bail!("this device has been removed from your identity and can no longer send");
    }

    let contacts = home.load_contacts()?;
    let c = contacts
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no contact '{name}'"))?;
    let peer = c.identity()?;
    env.to = Some(peer.owner_hex());
    env.lamport = home.next_lamport()?;
    let plaintext = env.to_bytes();

    let my_id = me.device_id();
    let mine = IdentityCard::decode(&home.identity_card()?)?;
    let mut targets: Vec<(DeviceCard, Option<String>, bool)> = peer
        .device_relays()
        .into_iter()
        .map(|(d, r)| (d, r, false))
        .collect();
    targets.extend(
        mine.device_relays()
            .into_iter()
            .filter(|(d, _)| d.device_id() != my_id)
            .map(|(d, r)| (d, r, true)),
    );

    let mut pool = Pool {
        base: base.clone(),
        me: &me,
        conns: HashMap::new(),
    };
    let mut devices = Vec::new();
    for (card, relay, own) in targets {
        let url = relay.unwrap_or_else(|| base.url.clone());
        let r = send_to_device(&mut pool, home, &mut sessions, &card, &url, &plaintext).await;
        devices.push(DeviceSend {
            device_id: card.device_id(),
            own,
            new_session: *r.as_ref().unwrap_or(&false),
            error: r.err().map(|e| format!("{e:#}")),
        });
    }
    if !devices.iter().any(|d| !d.own && d.error.is_none()) {
        let why: Vec<String> = devices
            .iter()
            .filter(|d| !d.own)
            .filter_map(|d| d.error.clone())
            .collect();
        bail!(
            "could not deliver to any of {name}'s devices: {}",
            why.join("; ")
        );
    }
    // Applied locally once at least one of the recipient's devices has it.
    match env.kind {
        Kind::Text => {
            home.messages()?.insert(&StoredMessage {
                id: env.id.clone(),
                peer: peer.owner_hex(),
                direction: Direction::Out,
                ts: env.ts,
                body: env.body,
                attachment: env.attachment,
                lamport: env.lamport,
                stamp_device: my_id.clone(),
                edited: false,
                deleted: false,
                changed_at: 0,
            })?;
        }
        Kind::Edit | Kind::Delete => {
            crate::apply_change(home, &env, true, &peer.owner_hex(), &my_id)?;
        }
        // Sync messages change nothing in the sender's own history.
        Kind::Sync => {}
    }
    Ok(SendReport {
        id: env.id,
        verified: c.verified,
        devices,
        notes,
    })
}

/// Connect, receive until `idle` passes with nothing new (forever if `None`),
/// calling `on` for each outcome and acking after it has been persisted.
/// A message from a device we don't know yet triggers one refresh of contacts'
/// device lists, then a retry, so contacts' new devices are picked up.
/// Requests from our own other devices (catch-up, files) are answered here.
pub async fn recv(
    home: &Home,
    relay: impl Into<Target>,
    idle: Option<Duration>,
    mut on: impl FnMut(Outcome),
) -> Result<()> {
    let base: Target = relay.into();
    let started = now_ms();
    let mut me = home.load_identity()?;
    let mut sessions = home.load_sessions()?;
    let mut relay = Relay::connect(base.clone(), &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;
    sync_own_list(&mut relay, home).await?;
    while let Some(d) = relay.next_delivery(idle).await? {
        let mut outcome = process(home, &mut me, &mut sessions, &d.blob)?;
        if matches!(outcome, Outcome::UnknownSender { .. }) {
            refresh_contacts(&mut relay, home, None).await?;
            sync_own_list(&mut relay, home).await?;
            outcome = process(home, &mut me, &mut sessions, &d.blob)?;
        }
        // Acknowledge first: the request is now durably handled on our side, and
        // an answer that fails can be asked for again.
        relay.ack(&d.id).await?;
        if let Outcome::SyncRequest { from_device, msg } = &outcome {
            if let Err(e) = answer_request(home, &base, &me, &mut sessions, from_device, msg).await
            {
                eprintln!("note: could not answer a request from your other device: {e:#}");
            }
        }
        on(outcome);
    }
    // We have collected everything the relay held for us as of `started`.
    if idle.is_some() {
        home.set_last_sync(started)?;
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How far before our last successful sync a catch-up request reaches back, to
/// absorb clock differences between our devices. Duplicates are harmless.
const CATCH_UP_MARGIN_MS: u64 = 24 * 3600 * 1000;

/// Our own other devices, with the relay each reads from.
fn own_other_devices(home: &Home, me: &Identity) -> Result<Vec<(DeviceCard, Option<String>)>> {
    let mine = IdentityCard::decode(&home.identity_card()?)?;
    let my_id = me.device_id();
    Ok(mine
        .device_relays()
        .into_iter()
        .filter(|(d, _)| d.device_id() != my_id)
        .collect())
}

/// Send one sync message to one of our own devices over its session.
async fn push_sync(
    home: &Home,
    base: &Target,
    me: &Identity,
    sessions: &mut Sessions,
    device: &(DeviceCard, Option<String>),
    msg: &SyncMsg,
) -> Result<()> {
    let mut env = msg.to_envelope()?;
    env.lamport = home.next_lamport()?;
    let url = device.1.clone().unwrap_or_else(|| base.url.clone());
    let mut pool = Pool {
        base: base.clone(),
        me,
        conns: HashMap::new(),
    };
    send_to_device(&mut pool, home, sessions, &device.0, &url, &env.to_bytes()).await?;
    Ok(())
}

/// Send `msg` to every other device of ours. Returns how many accepted it.
async fn broadcast_own(home: &Home, base: Target, msg: &SyncMsg) -> Result<usize> {
    let me = home.load_identity()?;
    let mut sessions = home.load_sessions()?;
    {
        let mut r = Relay::connect(base.clone(), &me).await?;
        sync_own_list(&mut r, home).await?;
    }
    let mut ok = 0;
    for d in own_other_devices(home, &me)? {
        match push_sync(home, &base, &me, &mut sessions, &d, msg).await {
            Ok(()) => ok += 1,
            Err(e) => eprintln!(
                "note: could not reach your device {}: {e:#}",
                &d.0.device_id()[..8]
            ),
        }
    }
    Ok(ok)
}

/// Mark a conversation read up to its newest incoming message, here and on
/// your other devices. Returns how many other devices were told.
pub async fn mark_read(home: &Home, relay: impl Into<Target>, name: &str) -> Result<usize> {
    let contacts = home.load_contacts()?;
    let c = contacts
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no contact '{name}'"))?;
    let peer = c.conversation_key()?;
    let upto = home
        .messages()?
        .conversation(&peer, usize::MAX)?
        .iter()
        .filter(|m| m.direction == Direction::In)
        .map(|m| m.ts)
        .max()
        .unwrap_or(0);
    home.advance_read_marker(&peer, upto)?;
    broadcast_own(home, relay.into(), &SyncMsg::Read { peer, upto }).await
}

/// Incoming messages in each conversation newer than its read marker.
pub fn unread_counts(home: &Home) -> Result<Vec<(String, usize)>> {
    let store = home.messages()?;
    let mut out = Vec::new();
    for c in home.load_contacts()? {
        let peer = c.conversation_key()?;
        let marker = home.read_marker(&peer)?;
        let n = store
            .conversation(&peer, usize::MAX)?
            .iter()
            .filter(|m| m.direction == Direction::In && !m.deleted && m.ts > marker)
            .count();
        out.push((c.name, n));
    }
    Ok(out)
}

/// Ask your other devices for everything they learned or changed since this
/// device last synced. They answer the next time they receive (`spike recv`).
pub async fn request_catch_up(home: &Home, relay: impl Into<Target>) -> Result<usize> {
    let since = home.last_sync().saturating_sub(CATCH_UP_MARGIN_MS);
    broadcast_own(home, relay.into(), &SyncMsg::CatchUpRequest { since }).await
}

/// Ask your other devices to make a message's attachment available again
/// (for example after the relay expired it).
pub async fn request_file(home: &Home, relay: impl Into<Target>, msg_id: &str) -> Result<usize> {
    let m = home.messages()?.find(msg_id)?;
    if m.attachment.is_none() {
        bail!("that message has no attachment");
    }
    broadcast_own(home, relay.into(), &SyncMsg::FileRequest { msg_id: m.id }).await
}

/// Answer a request from one of our own devices (called from `recv`).
async fn answer_request(
    home: &Home,
    base: &Target,
    me: &Identity,
    sessions: &mut Sessions,
    from_device: &str,
    req: &SyncMsg,
) -> Result<()> {
    let device = own_other_devices(home, me)?
        .into_iter()
        .find(|(d, _)| d.device_id() == from_device)
        .ok_or_else(|| anyhow!("request came from a device that is not in your device list"))?;
    match req {
        SyncMsg::CatchUpRequest { since } => {
            let changed = home.messages()?.changed_since(*since)?;
            for b in sync::batches(changed) {
                push_sync(home, base, me, sessions, &device, &b).await?;
            }
        }
        SyncMsg::FileRequest { msg_id } => {
            let store = home.messages()?;
            let Some(m) = store.get(msg_id)? else {
                return Ok(());
            };
            let (Some(att), false) = (m.attachment.as_ref(), m.deleted) else {
                return Ok(());
            };
            let Some(bytes) = home.load_attachment(&m.id)? else {
                return Ok(());
            };
            // Re-encrypt under a fresh key and upload to our relay; the requester
            // verifies the same hash, size and name before using the new descriptor.
            let (mut fresh, chunks) = seal_file(&att.name, &bytes)?;
            fresh.relay = Some(base.url.clone());
            let mut r = Relay::connect(base.clone(), me).await?;
            for (i, c) in chunks.iter().enumerate() {
                r.put_blob_chunk(&fresh.blob, i as u32, fresh.chunks, c)
                    .await?;
            }
            let offer = SyncMsg::FileOffer {
                msg_id: m.id.clone(),
                attachment: fresh,
            };
            push_sync(home, base, me, sessions, &device, &offer).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Connect once so the relay holds our prekeys and our current device list.
/// Returns how many one-time prekeys it now has.
pub async fn publish_keys(home: &Home, relay: impl Into<Target>) -> Result<usize> {
    let mut me = home.load_identity()?;
    let mut relay = Relay::connect(relay, &me).await?;
    top_up_prekeys(&mut relay, home, &mut me).await?;
    sync_own_list(&mut relay, home).await?;
    Ok(relay.prekeys)
}
