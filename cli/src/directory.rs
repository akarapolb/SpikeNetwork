//! The agent directory on this device (ADR 0036): the listers the person chose to trust and the
//! signed listings they imported. Nothing is trusted by default; nothing here grants anything.

use crate::state::Home;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::listing::{Listing, ListingBody};
use spike_core::owner::IdentityCard;

const LISTERS: &str = "agent-listers.enc";
const LISTINGS: &str = "agent-listings.enc";
/// More than this many stored listings and the oldest are dropped.
const MAX_LISTINGS: usize = 500;
const MAX_LISTERS: usize = 32;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A key the person trusts to say who runs an agent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Lister {
    pub name: String,
    pub key: String,
}

pub fn listers(home: &Home) -> Result<Vec<Lister>> {
    home.load_json(LISTERS)
}

/// A key as 64 hex characters, or an identity card (its owner key is used).
fn key_of(text: &str) -> Result<String> {
    let t = text.trim();
    if t.len() == 64 && t.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Ok(t.to_ascii_lowercase());
    }
    Ok(IdentityCard::decode(t)
        .map_err(|_| anyhow::anyhow!("that is not a lister key or an identity card"))?
        .owner_hex())
}

pub fn add_lister(home: &Home, name: &str, key: &str) -> Result<Lister> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        bail!("give the lister a name of 1 to 64 characters");
    }
    let key = key_of(key)?;
    if key == hex::encode(home.load_owner()?.public()) {
        bail!("that is your own key");
    }
    let mut all = listers(home)?;
    all.retain(|l| l.key != key);
    if all.len() >= MAX_LISTERS {
        bail!("at most {MAX_LISTERS} listers");
    }
    let l = Lister {
        name: name.into(),
        key,
    };
    all.push(l.clone());
    home.save_json(LISTERS, &all)?;
    Ok(l)
}

pub fn remove_lister(home: &Home, key: &str) -> Result<bool> {
    let mut all = listers(home)?;
    let n = all.len();
    all.retain(|l| l.key != key);
    home.save_json(LISTERS, &all)?;
    Ok(all.len() != n)
}

fn listings(home: &Home) -> Result<Vec<Listing>> {
    home.load_json(LISTINGS)
}

/// Read listings from text: one listing, a JSON array of them, or one per line. Each is checked
/// on its own; a newer listing from the same lister for the same agent replaces the older one.
/// Returns how many were taken and how many were refused.
pub fn import(home: &Home, text: &str) -> Result<(usize, usize)> {
    let t = text.trim();
    let parsed: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(t) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(v) => vec![v],
        Err(_) => t
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l))
            .collect::<Result<_, _>>()
            .context("that is not a list of agent listings")?,
    };
    let now = now_ms();
    let mut all = listings(home)?;
    let (mut ok, mut bad) = (0, 0);
    for v in parsed {
        match serde_json::from_value::<Listing>(v)
            .ok()
            .filter(|l| l.verify(now).is_ok())
        {
            Some(l) => {
                let older = all
                    .iter()
                    .position(|x| x.body.lister == l.body.lister && x.body.agent == l.body.agent);
                match older {
                    Some(i) if all[i].body.issued >= l.body.issued => {}
                    Some(i) => all[i] = l,
                    None => all.push(l),
                }
                ok += 1;
            }
            None => bad += 1,
        }
    }
    all.retain(|l| l.verify(now).is_ok());
    all.sort_by_key(|l| std::cmp::Reverse(l.body.issued));
    all.truncate(MAX_LISTINGS);
    home.save_json(LISTINGS, &all)?;
    Ok((ok, bad))
}

/// One agent in the directory: the best listing for it, and whether anyone you trust vouches.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub agent: String,
    pub card: String,
    pub name: String,
    pub maker: String,
    pub about: String,
    pub scopes: Vec<String>,
    /// A lister you trust signed this listing.
    pub verified: bool,
    /// Who vouches, in words: the lister's name, "its maker", or "a lister you do not trust".
    pub listed_by: String,
    pub expires: u64,
}

pub fn entries(home: &Home) -> Result<Vec<Entry>> {
    let now = now_ms();
    let trusted = listers(home)?;
    let mut out: Vec<Entry> = Vec::new();
    for l in listings(home)?
        .into_iter()
        .filter(|l| l.verify(now).is_ok())
    {
        let by = trusted
            .iter()
            .find(|t| t.key == l.body.lister && !l.self_listed());
        let e = Entry {
            agent: l.body.agent.clone(),
            card: l.body.card.clone(),
            name: l.body.name.clone(),
            maker: l.body.maker.clone(),
            about: l.body.about.clone(),
            scopes: l.body.scopes.clone(),
            verified: by.is_some(),
            listed_by: match by {
                Some(t) => t.name.clone(),
                None if l.self_listed() => "its maker".into(),
                None => "a lister you do not trust".into(),
            },
            expires: l.body.expires,
        };
        match out.iter().position(|x| x.agent == e.agent) {
            Some(i) if !out[i].verified && e.verified => out[i] = e,
            Some(_) => {}
            None => out.push(e),
        }
    }
    out.sort_by(|a, b| {
        b.verified
            .cmp(&a.verified)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

pub fn find(home: &Home, agent: &str) -> Result<Entry> {
    entries(home)?
        .into_iter()
        .find(|e| e.agent == agent)
        .context("that agent is not in the directory any more")
}

/// Sign a listing with this identity's owner key: for the agent itself (`card` None) or, as a
/// lister, for another agent's card.
pub fn sign(
    home: &Home,
    card: Option<&str>,
    name: &str,
    maker: &str,
    about: &str,
    scopes: Vec<String>,
    days: u64,
) -> Result<Listing> {
    let owner = home.load_owner()?;
    let card = match card {
        Some(c) => c.trim().to_string(),
        None => home.identity_card()?,
    };
    let agent = IdentityCard::decode(&card)
        .map_err(|_| anyhow::anyhow!("that is not a Spike identity card"))?
        .owner_hex();
    let now = now_ms();
    Listing::sign(
        &owner,
        ListingBody {
            lister: hex::encode(owner.public()),
            agent,
            card,
            name: name.trim().into(),
            maker: maker.trim().into(),
            about: about.trim().into(),
            scopes,
            issued: now,
            expires: now + days.clamp(1, 90) * 86_400_000,
        },
    )
    .map_err(|_| anyhow::anyhow!("a listing needs a name and maker (up to 64 characters), a short description, valid scopes, and 1 to 90 days"))
}
