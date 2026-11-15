//! One-time invite links (ADR 0039).
//!
//! Making a link seals your card, offered name and a one-time token under a fresh key and leaves
//! the sealed blob on your home relay, which hands it out once and keeps it at most 7 days. Opening
//! a link takes the blob (with a throwaway key, so the relay cannot tell who), adds the inviter as a
//! contact, not verified, and sends them an `InviteHello` with the token. The inviter's device
//! accepts that hello once, while the invite is open, and adds the sender as a contact, not verified.

use crate::client::Relay;
use crate::state::Home;
use crate::transport::Target;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::envelope::{valid_intro_name, Envelope};
use spike_core::identity::Identity;
use spike_core::invite::{self, Hello, InviteLink, InvitePayload};
use spike_core::owner::IdentityCard;

const INVITES: &str = "invites.enc";
/// What a person opening your link is told when it no longer works.
pub const USED_OR_EXPIRED: &str = "This invite link was already used or has expired.";
/// Used invites are remembered this long after they expire, then forgotten.
const KEEP_MS: u64 = 30 * 24 * 3600 * 1000;

/// One invite this device made.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    /// The relay's id for it (32 hex).
    pub id: String,
    pub token: String,
    pub created: u64,
    pub expires: u64,
    /// When it was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    /// Who used it: their owner key, or "contact" for someone already in your contacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

fn now_ms() -> u64 {
    spike_core::clock::now_ms()
}

fn load(home: &Home) -> Result<Vec<Invite>> {
    let now = now_ms();
    let mut all: Vec<Invite> = home.load_json(INVITES)?;
    all.retain(|i| i.expires + KEEP_MS > now);
    Ok(all)
}

/// This device's invites, newest first.
pub fn list(home: &Home) -> Result<Vec<Invite>> {
    let mut all = load(home)?;
    all.sort_by_key(|i| std::cmp::Reverse(i.created));
    Ok(all)
}

/// Make a one-time link, valid for 7 days, and leave its sealed invite on the home relay.
pub async fn create(home: &Home, relay: impl Into<Target>) -> Result<String> {
    create_with_ttl(home, relay, invite::TTL_MS).await
}

/// As [`create`], with another lifetime for the token and the sealed payload (tests).
pub async fn create_with_ttl(home: &Home, relay: impl Into<Target>, ttl_ms: u64) -> Result<String> {
    let base: Target = relay.into();
    let link = InviteLink::generate(Some(base.url.clone()));
    let now = now_ms();
    let entry = Invite {
        id: link.id_hex(),
        token: invite::new_token(),
        created: now,
        expires: now + ttl_ms,
        used: None,
        by: None,
    };
    let payload = InvitePayload {
        card: home.identity_card()?,
        name: home.profile_name()?,
        token: entry.token.clone(),
        expires: entry.expires,
    };
    let me = home.load_identity()?;
    let mut r = Relay::connect(base, &me).await?;
    r.put_invite(&entry.id, &link.seal(&payload))
        .await
        .context("leaving the invite on your relay")?;
    // Saved once the relay holds it, so no token is open that nobody can ever reach.
    let mut all = load(home)?;
    all.push(entry);
    home.save_json(INVITES, &all)?;
    Ok(link.encode())
}

/// A name no contact has yet: `wanted` if it is free, else "wanted 2", "wanted 3", ...
pub fn free_name(home: &Home, wanted: &str) -> Result<String> {
    let taken: Vec<String> = home.load_contacts()?.into_iter().map(|c| c.name).collect();
    if !taken.iter().any(|n| n == wanted) {
        return Ok(wanted.into());
    }
    (2..)
        .map(|n| format!("{wanted} {n}"))
        .find(|n| !taken.contains(n))
        .ok_or_else(|| anyhow!("no free name"))
}

/// What opening a link did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redeemed {
    /// The contact's name here.
    pub name: String,
    /// Their conversation key (owner key, hex).
    pub peer: String,
    /// They were already a contact (the link is used up all the same).
    pub existing: bool,
}

/// Open someone's invite link: take the invite from their relay, add them as a contact (not
/// verified) called `name`, or the name they offer, and send the hello that tells them.
pub async fn redeem(
    home: &Home,
    relay: impl Into<Target>,
    code: &str,
    name: Option<&str>,
) -> Result<Redeemed> {
    let base: Target = relay.into();
    let link = InviteLink::decode(code).map_err(|_| anyhow!("this is not an invite link"))?;
    let at = match &link.relay {
        Some(url) if *url != base.url => Target {
            url: url.clone(),
            opts: base.opts.clone(),
        },
        _ => base.clone(),
    };
    // A throwaway key: the relay learns that its invite was taken, not by whom.
    let throwaway = Identity::generate()?;
    let mut r = Relay::connect(at, &throwaway)
        .await
        .context("reaching the relay the invite is on")?;
    let blob = r
        .take_invite(&link.id_hex())
        .await?
        .ok_or_else(|| anyhow!(USED_OR_EXPIRED))?;
    drop(r);
    let p = link
        .open(&blob)
        .map_err(|_| anyhow!("this invite link is damaged"))?;
    let hello = Hello {
        token: p.token.clone(),
    };
    if p.expires <= now_ms() || !hello.valid() {
        bail!(USED_OR_EXPIRED);
    }
    let card =
        IdentityCard::decode(&p.card).map_err(|e| anyhow!("the invite holds a bad card: {e}"))?;
    let peer = card.owner_hex();
    if peer == IdentityCard::decode(&home.identity_card()?)?.owner_hex() {
        bail!("this is your own invite link");
    }
    let known = home
        .load_contacts()?
        .into_iter()
        .find(|c| c.conversation_key().ok().as_deref() == Some(peer.as_str()));
    let (name, existing) = match known {
        Some(c) => (c.name, true),
        None => {
            let wanted = name
                .map(str::trim)
                .filter(|n| valid_intro_name(n))
                .or(p.name.as_deref().filter(|n| valid_intro_name(n)))
                .unwrap_or("New contact");
            let name = free_name(home, wanted)?;
            home.add_contact(&name, &p.card, false)?;
            (name, false)
        }
    };
    crate::ops::deliver_envelope(home, base, &name, Envelope::invite_hello(&p.token)?)
        .await
        .with_context(|| format!("{name} was added, but telling them failed"))?;
    Ok(Redeemed {
        name,
        peer,
        existing,
    })
}

/// Whether `token` opens one of our invites that is unused and not expired.
pub(crate) fn is_open(home: &Home, token: &str) -> Result<bool> {
    let now = now_ms();
    Ok(load(home)?
        .iter()
        .any(|i| i.token == token && i.used.is_none() && i.expires > now))
}

/// Mark the invite with `token` used by `by`. False if it was not open.
pub(crate) fn use_up(home: &Home, token: &str, by: &str) -> Result<bool> {
    let now = now_ms();
    let mut all = load(home)?;
    let Some(i) = all
        .iter_mut()
        .find(|i| i.token == token && i.used.is_none() && i.expires > now)
    else {
        return Ok(false);
    };
    i.used = Some(now);
    i.by = Some(by.into());
    home.save_json(INVITES, &all)?;
    Ok(true)
}

/// A contact opened our link: nobody new to add, but the invite is used up.
pub(crate) fn on_contact_hello(home: &Home, body: &str) -> Result<()> {
    if let Ok(h) = serde_json::from_str::<Hello>(body) {
        use_up(home, &h.token, "contact")?;
    }
    Ok(())
}
