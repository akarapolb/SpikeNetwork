//! Agent directory listings (ADR 0036). A [`Listing`] is a signed statement by a *lister*:
//! "the agent whose owner key is `agent` is called `name`, made by `maker`, and asks for
//! these scopes". The app calls an agent "identity verified" only when the lister is one the
//! person chose to trust; a listing the agent signed itself is shown as listed by its maker
//! only. A listing never grants anything: the grant starts empty and is the person's choice.
//!
//! Pure: no I/O, and `now` is passed in.

use crate::agent::{is_hex, sign, valid_scope, verify};
use crate::owner::{IdentityCard, OwnerKey};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

const LISTING_CTX: &[u8] = b"spike-agent-listing-v1";
/// Longest a listing stays valid, so a lister's word has to be renewed.
pub const MAX_LISTING_LIFETIME_MS: u64 = 90 * 86_400_000;
const MAX_NAME: usize = 64;
const MAX_ABOUT: usize = 200;

/// Field order is the signing order; do not reorder.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ListingBody {
    /// The lister's owner key (hex). Equal to `agent` when the agent lists itself.
    pub lister: String,
    /// The agent's owner key (hex); its identity card must carry the same key.
    pub agent: String,
    /// The agent's identity card, so the person can add it from the listing.
    pub card: String,
    pub name: String,
    pub maker: String,
    pub about: String,
    /// What the maker says the agent needs. Shown, never granted by itself.
    pub scopes: Vec<String>,
    pub issued: u64,
    pub expires: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub body: ListingBody,
    pub sig: String,
}

fn text_ok(s: &str, max: usize) -> bool {
    !s.trim().is_empty()
        && s.chars().count() <= max
        && !s
            .chars()
            .any(|c| c.is_control() || crate::action::invisible(c))
}

fn well_formed(b: &ListingBody) -> bool {
    is_hex(&b.lister, 64)
        && is_hex(&b.agent, 64)
        && text_ok(&b.name, MAX_NAME)
        && text_ok(&b.maker, MAX_NAME)
        && (b.about.is_empty() || text_ok(&b.about, MAX_ABOUT))
        && b.scopes.len() <= 32
        && b.scopes.iter().all(|s| valid_scope(s))
        && b.expires > b.issued
        && b.expires - b.issued <= MAX_LISTING_LIFETIME_MS
        && IdentityCard::decode(&b.card).is_ok_and(|c| c.owner_hex() == b.agent)
}

impl Listing {
    pub fn sign(lister: &OwnerKey, body: ListingBody) -> Result<Self> {
        if body.lister != hex::encode(lister.public()) || !well_formed(&body) {
            return Err(Error::Malformed);
        }
        let bytes = serde_json::to_vec(&body)?;
        Ok(Self {
            sig: sign(lister, LISTING_CTX, &bytes),
            body,
        })
    }

    /// The signature and contents check out, and it is in date at `now`.
    pub fn verify(&self, now: u64) -> Result<()> {
        if !well_formed(&self.body) {
            return Err(Error::Malformed);
        }
        let bytes = serde_json::to_vec(&self.body)?;
        verify(
            &self.body.lister,
            &[LISTING_CTX, &bytes].concat(),
            &self.sig,
        )?;
        if now >= self.body.expires || self.body.issued > now + crate::agent::MAX_SKEW_MS {
            return Err(Error::Malformed);
        }
        Ok(())
    }

    /// Signed by the agent itself: nobody else vouches for who runs it.
    pub fn self_listed(&self) -> bool {
        self.body.lister == self.body.agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::{DeviceEntry, DeviceList};

    fn card_of(owner: &OwnerKey) -> String {
        let dev = crate::identity::Identity::generate().unwrap();
        let entry = DeviceEntry {
            card: dev.card().encode(),
            relay: None,
        };
        let list = DeviceList::sign(owner, 1, vec![entry]).unwrap();
        IdentityCard::encode(&list)
    }

    fn body(lister: &OwnerKey, agent: &OwnerKey, now: u64) -> ListingBody {
        ListingBody {
            lister: hex::encode(lister.public()),
            agent: hex::encode(agent.public()),
            card: card_of(agent),
            name: "Ledger Desk".into(),
            maker: "Ledger Desk Ltd".into(),
            about: "Files receipts and asks before sending an invoice.".into(),
            scopes: vec!["files.read".into(), "payments.send".into()],
            issued: now,
            expires: now + 30 * 86_400_000,
        }
    }

    #[test]
    fn signed_listing_verifies_and_tampering_fails() {
        let (lister, agent) = (OwnerKey::generate(), OwnerKey::generate());
        let l = Listing::sign(&lister, body(&lister, &agent, 1000)).unwrap();
        l.verify(2000).unwrap();
        assert!(!l.self_listed());
        let mut t = l.clone();
        t.body.name = "Ledger Desk Pro".into();
        assert!(t.verify(2000).is_err());
        let mut t = l.clone();
        t.body.scopes.push("files.delete".into());
        assert!(t.verify(2000).is_err());
        // Expired, or dated in the future.
        assert!(l.verify(1000 + 31 * 86_400_000).is_err());
        assert!(
            Listing::sign(&lister, body(&lister, &agent, 10 * 60 * 1000))
                .unwrap()
                .verify(0)
                .is_err()
        );
    }

    #[test]
    fn card_must_match_the_agent_and_lifetime_is_bounded() {
        let (lister, agent, other) = (
            OwnerKey::generate(),
            OwnerKey::generate(),
            OwnerKey::generate(),
        );
        let mut b = body(&lister, &agent, 0);
        b.card = card_of(&other);
        assert!(Listing::sign(&lister, b).is_err());
        let mut b = body(&lister, &agent, 0);
        b.expires = MAX_LISTING_LIFETIME_MS + 1;
        assert!(Listing::sign(&lister, b).is_err());
        // Only the named lister can sign.
        assert!(Listing::sign(&other, body(&lister, &agent, 0)).is_err());
        let own = Listing::sign(&agent, body(&agent, &agent, 0)).unwrap();
        assert!(own.self_listed());
    }
}
