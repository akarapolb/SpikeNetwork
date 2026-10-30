//! Message requests (ADR 0021): first messages from people who are not contacts.
//!
//! A stranger who writes to you must introduce themselves with their signed identity card, and
//! the card must list the device that actually sent the message. A valid first message is held
//! here, in a sealed store, with its session in a separate quarantine (never mixed with your
//! contacts' sessions) until you accept, decline or block. Nothing is shown as a conversation,
//! attachments are stripped, and the sender is told nothing.

use crate::messages::{Direction, StoredMessage};
use crate::state::Home;
use crate::Outcome;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use spike_core::envelope::{Envelope, Kind};
use spike_core::identity::Identity;
use spike_core::owner::IdentityCard;

const REQUESTS: &str = "requests.enc";
const BLOCKED: &str = "blocked.enc";

/// Pending requests at once. Past this, a new stranger's first message is dropped.
pub const MAX_REQUESTS: usize = 50;
/// Messages held per request.
pub const MAX_HELD: usize = 5;
/// Longest held message, in bytes. Longer text is cut.
pub const MAX_HELD_BYTES: usize = 4096;
/// How long an undecided request is kept.
pub const TTL_MS: u64 = 30 * 24 * 3600 * 1000;

/// One text message held for a pending request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub id: String,
    pub ts: u64,
    pub lamport: u64,
    pub body: String,
    /// The name of a file that came with it and was dropped (files are never kept for a request).
    pub file: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The sending device's Curve25519 key, hex. Also the request's id.
    pub key: String,
    /// The sender's owner key, hex, from their validated card.
    pub owner: String,
    /// Their identity card, kept so accepting can add them as a contact.
    pub card: String,
    pub fingerprint: String,
    pub received: u64,
    pub held: Vec<Held>,
}

/// A person you blocked: every device key their card listed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub owner: String,
    pub fingerprint: String,
    pub keys: Vec<String>,
    pub at: u64,
}

fn now_ms() -> u64 {
    spike_core::clock::now_ms()
}

fn key_bytes(hex_key: &str) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_key, &mut out).map_err(|_| anyhow!("bad request id"))?;
    Ok(out)
}

fn load(home: &Home) -> Result<Vec<Request>> {
    home.load_json(REQUESTS)
}

/// Pending requests, oldest expired ones removed first.
pub fn list(home: &Home) -> Result<Vec<Request>> {
    purge_expired(home)?;
    let mut all = load(home)?;
    all.sort_by_key(|r| std::cmp::Reverse(r.received));
    Ok(all)
}

fn purge_expired(home: &Home) -> Result<()> {
    let now = now_ms();
    let mut all = load(home)?;
    let before = all.len();
    let (keep, gone): (Vec<_>, Vec<_>) = all.drain(..).partition(|r| r.received + TTL_MS > now);
    if gone.is_empty() {
        return Ok(());
    }
    let mut q = home.load_quarantine()?;
    for r in &gone {
        if let Ok(k) = key_bytes(&r.key) {
            q.forget(&k);
        }
    }
    home.save_quarantine(&q)?;
    home.save_json(REQUESTS, &keep)?;
    debug_assert!(keep.len() < before);
    Ok(())
}

pub fn blocked(home: &Home) -> Result<Vec<Blocked>> {
    home.load_json(BLOCKED)
}

/// Is this person (owner key) on the block list?
pub fn is_blocked_owner(home: &Home, owner: &str) -> Result<bool> {
    Ok(blocked(home)?.iter().any(|b| b.owner == owner))
}

pub fn is_blocked(home: &Home, device_key: &[u8; 32]) -> Result<bool> {
    let k = hex::encode(device_key);
    Ok(blocked(home)?.iter().any(|b| b.keys.contains(&k)))
}

/// Forget a block (by owner key or fingerprint prefix, at least 8 characters).
pub fn unblock(home: &Home, who: &str) -> Result<()> {
    let who = who.trim().to_lowercase();
    if who.len() < 8 {
        bail!("give at least 8 characters of the person's key or fingerprint");
    }
    let mut all = blocked(home)?;
    let n = all.len();
    all.retain(|b| {
        !(b.owner.starts_with(&who)
            || b.fingerprint
                .replace('-', "")
                .starts_with(&who.replace('-', "")))
    });
    if all.len() == n {
        bail!("nobody blocked matches '{who}'");
    }
    home.save_json(BLOCKED, &all)
}

fn hold(req: &mut Request, env: &Envelope) {
    if req.held.len() >= MAX_HELD || req.held.iter().any(|h| h.id == env.id) {
        return;
    }
    let mut body = env.body.clone();
    if body.len() > MAX_HELD_BYTES {
        let mut cut = MAX_HELD_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
    }
    req.held.push(Held {
        id: env.id.clone(),
        ts: env.ts,
        lamport: env.lamport,
        body,
        file: env.attachment.as_ref().map(|a| a.name.clone()),
    });
}

/// A first (pre-key) message from a device that is not a contact. Call it after any retry for a
/// contact's new device has failed. Always safe to acknowledge the message afterwards.
pub fn on_stranger(home: &Home, me: &mut Identity, blob: &[u8], key: [u8; 32]) -> Result<Outcome> {
    let dropped = || Outcome::UnknownSender {
        fingerprint_hint: hex::encode(&key[..6]),
        identity_key: key,
    };
    if is_blocked(home, &key)? {
        return Ok(Outcome::Ignored);
    }
    purge_expired(home)?;
    let mut all = load(home)?;
    let key_hex = hex::encode(key);
    let known = all.iter().any(|r| r.key == key_hex);
    if !known && all.len() >= MAX_REQUESTS {
        return Ok(dropped());
    }
    let mut q = home.load_quarantine()?;
    let had_session = q.has_peer(&key);
    let decrypted = q.decrypt_any(me, blob);
    // A pre-key message consumes one of our one-time prekeys even if we then refuse it.
    home.save_identity(me)?;
    let Ok((_, plain)) = decrypted else {
        return Ok(Outcome::Invalid);
    };
    let refuse = |q: &mut spike_core::session::Sessions| -> Result<Outcome> {
        if !had_session {
            q.forget(&key);
        }
        home.save_quarantine(q)?;
        Ok(dropped())
    };
    let Ok(env) = Envelope::from_bytes(&plain) else {
        return refuse(&mut q);
    };
    if env.kind == Kind::GroupState && !known {
        // Someone who is not a contact added us to a group (ADR 0024, section 6): an invite,
        // introduced and bound like any request, its session kept in quarantine until we accept.
        let Some((intro, card)) = crate::groups::introduction_for(home, &env, &key_hex) else {
            return refuse(&mut q);
        };
        if !card.devices().iter().any(|d| d.x25519 == key) {
            return refuse(&mut q);
        }
        return match crate::groups::on_stranger_invite(
            home,
            &env,
            &key_hex,
            &intro,
            &card.owner_hex(),
        )? {
            Some(o) => {
                home.save_quarantine(&q)?;
                Ok(o)
            }
            None => refuse(&mut q),
        };
    }
    if env.kind != Kind::Text || env.group.is_some() {
        return refuse(&mut q);
    }
    let idx = match all.iter().position(|r| r.key == key_hex) {
        Some(i) => i,
        None => {
            // A new request must introduce itself, and the introduction must name the device that sent it.
            let Some(card) = env
                .intro
                .as_deref()
                .and_then(|c| IdentityCard::decode(c).ok())
            else {
                return refuse(&mut q);
            };
            if !card.devices().iter().any(|d| d.x25519 == key) {
                return refuse(&mut q);
            }
            all.push(Request {
                key: key_hex.clone(),
                owner: card.owner_hex(),
                card: env.intro.clone().expect("checked above"),
                fingerprint: card.fingerprint(),
                received: now_ms(),
                held: vec![],
            });
            all.len() - 1
        }
    };
    hold(&mut all[idx], &env);
    home.save_quarantine(&q)?;
    home.save_json(REQUESTS, &all)?;
    Ok(Outcome::MessageRequest {
        id: key_hex,
        fingerprint: all[idx].fingerprint.clone(),
        held: all[idx].held.len(),
        new: !known,
    })
}

/// A later (ordinary, not pre-key) message from someone whose request is pending. `None` if it is not for one.
pub fn on_pending(home: &Home, me: &mut Identity, blob: &[u8]) -> Result<Option<Outcome>> {
    // Olm message type 1 is an ordinary message; type 0 (pre-key) goes through `on_stranger`.
    if blob.first() != Some(&1) {
        return Ok(None);
    }
    let mut q = home.load_quarantine()?;
    if q.peers().is_empty() {
        return Ok(None);
    }
    let Ok((who, plain)) = q.decrypt_any(me, blob) else {
        return Ok(None);
    };
    home.save_quarantine(&q)?;
    let who_hex = hex::encode(who);
    let mut all = load(home)?;
    let Some(req) = all.iter_mut().find(|r| r.key == who_hex) else {
        if crate::groups::is_invite_key(home, &who_hex)? {
            // Someone whose group invite waits for you: nothing is shown until you accept. A
            // further invite from them arrives on the same session and waits beside the first.
            if let Ok(env) = Envelope::from_bytes(&plain) {
                if env.kind == Kind::GroupState {
                    if let Some((card_str, card)) =
                        crate::groups::introduction_for(home, &env, &who_hex)
                    {
                        if let Some(o) = crate::groups::on_stranger_invite(
                            home,
                            &env,
                            &who_hex,
                            &card_str,
                            &card.owner_hex(),
                        )? {
                            return Ok(Some(o));
                        }
                    }
                }
            }
            return Ok(Some(Outcome::Ignored));
        }
        // A session with no request (it expired): drop it.
        q.forget(&who);
        home.save_quarantine(&q)?;
        return Ok(None);
    };
    let Ok(env) = Envelope::from_bytes(&plain) else {
        return Ok(Some(Outcome::Invalid));
    };
    if env.kind == Kind::Text && env.group.is_none() {
        hold(req, &env);
    }
    let (id, fingerprint, held) = (req.key.clone(), req.fingerprint.clone(), req.held.len());
    home.save_json(REQUESTS, &all)?;
    Ok(Some(Outcome::MessageRequest {
        id,
        fingerprint,
        held,
        new: false,
    }))
}

fn find<'a>(all: &'a [Request], id: &str) -> Result<&'a Request> {
    let id = id.trim().to_lowercase();
    if id.len() < 8 {
        bail!("give at least 8 characters of the request id");
    }
    let hits: Vec<&Request> = all.iter().filter(|r| r.key.starts_with(&id)).collect();
    match hits.as_slice() {
        [r] => Ok(r),
        [] => bail!("no message request starts with '{id}'"),
        _ => bail!("'{id}' matches several requests; type more characters"),
    }
}

/// Accept a request: the sender becomes a contact called `name` (not verified), their session
/// moves into your ordinary sessions, and the messages held for them enter the conversation.
/// Returns how many messages were added.
pub fn accept(home: &Home, id: &str, name: &str) -> Result<usize> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        bail!("give the contact a name of 1 to 64 characters");
    }
    let mut all = load(home)?;
    let req = find(&all, id)?.clone();
    let contacts = home.load_contacts()?;
    if contacts.iter().any(|c| c.name == name) {
        bail!("you already have a contact named '{name}': choose another name");
    }
    for c in &contacts {
        if c.conversation_key()? == req.owner {
            bail!("this person is already in your contacts as '{}'", c.name);
        }
    }
    let key = key_bytes(&req.key)?;
    let card = IdentityCard::decode(&req.card)
        .map_err(|e| anyhow!("their card is no longer valid: {e}"))?;
    let device = card
        .devices()
        .iter()
        .find(|d| d.x25519 == key)
        .ok_or_else(|| anyhow!("their card no longer lists the device that wrote to you"))?
        .device_id();
    home.add_contact(name, &req.card, false)?;
    let mut q = home.load_quarantine()?;
    let mut main = home.load_sessions()?;
    main.adopt(&mut q, &key);
    home.save_sessions(&main)?;
    home.save_quarantine(&q)?;
    let store = home.messages()?;
    let mut added = 0;
    for h in &req.held {
        let mut body = h.body.clone();
        if let Some(f) = &h.file {
            body.push_str(&format!(
                "\n[A file named \"{f}\" was sent before you accepted and was not kept.]"
            ));
        }
        if store.insert(&StoredMessage {
            id: h.id.clone(),
            peer: req.owner.clone(),
            direction: Direction::In,
            ts: h.ts,
            body,
            attachment: None,
            lamport: h.lamport,
            stamp_device: device.clone(),
            edited: false,
            deleted: false,
            changed_at: 0,
            author: None,
            shared_by: None,
            reply_to: None,
            reactions: Default::default(),
        })? {
            added += 1;
        }
        home.observe_lamport(h.lamport)?;
    }
    all.retain(|r| r.key != req.key);
    home.save_json(REQUESTS, &all)?;
    Ok(added)
}

/// Decline a request: it and its session are deleted. The sender is not told.
pub fn decline(home: &Home, id: &str) -> Result<()> {
    let mut all = load(home)?;
    let req = find(&all, id)?.clone();
    let mut q = home.load_quarantine()?;
    q.forget(&key_bytes(&req.key)?);
    home.save_quarantine(&q)?;
    all.retain(|r| r.key != req.key);
    home.save_json(REQUESTS, &all)
}

/// Block a person given their identity card: later first messages from any device in it are
/// dropped on arrival, before a session exists. Blocking someone twice changes nothing.
pub fn add_block(home: &Home, card: &str) -> Result<()> {
    let card = IdentityCard::decode(card)?;
    let owner = card.owner_hex();
    let mut list = blocked(home)?;
    if !list.iter().any(|b| b.owner == owner) {
        list.push(Blocked {
            owner,
            fingerprint: card.fingerprint(),
            keys: card
                .devices()
                .iter()
                .map(|d| hex::encode(d.x25519))
                .collect(),
            at: now_ms(),
        });
        home.save_json(BLOCKED, &list)?;
    }
    Ok(())
}

/// Decline and block: later messages from any device in their card are dropped on arrival.
pub fn block(home: &Home, id: &str) -> Result<()> {
    let all = load(home)?;
    let req = find(&all, id)?.clone();
    add_block(home, &req.card)?;
    decline(home, id)
}
