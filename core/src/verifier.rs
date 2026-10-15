//! The service side of agent approvals (Phase 2). See `docs/adr/0015-receipt-verifier.md`.
//!
//! A [`Receipt`] is only a signed fact. The service that actually performs an
//! action (a payment API, a file server) must check it before acting. This
//! module is that check:
//!
//! 1. The agent hands the service a [`Presentation`]: its signed request, the
//!    principal's receipt, and a proof that it holds the agent key, bound to
//!    this service (the *audience*) and the current time.
//! 2. The service calls [`redeem`] with what it is about to do (scope and
//!    amount). Every signature, the decision, the expiry, the scope, the
//!    amount and the audience are checked; then the request is marked spent so
//!    the same approval can never be used twice.
//!
//! Everything here is pure except the [`SpentStore`] the caller supplies.

use crate::agent::{is_hex, sign, verify, Decision, Receipt, SignedRequest, MAX_SKEW_MS};
use crate::owner::OwnerKey;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const PRESENT_CTX: &[u8] = b"spike-agent-presentation-v1";
/// Longest service name accepted as an audience.
pub const MAX_AUDIENCE: usize = 128;

/// Audiences name a service, e.g. `pay.example.com` or `files-prod`. Printable ASCII, no spaces.
pub fn valid_audience(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_AUDIENCE && s.bytes().all(|c| c.is_ascii_graphic())
}

/// What the agent's proof signs. Field order is the signing order; do not reorder.
#[derive(Serialize)]
struct ProofBody<'a> {
    audience: &'a str,
    request_hash: &'a str,
    presented: u64,
}

/// An approved request, as an agent presents it to the service that will act on it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub request: SignedRequest,
    pub receipt: Receipt,
    /// The service this presentation is for. A presentation captured on its
    /// way to one service is refused by every other.
    pub audience: String,
    /// When the agent made this presentation (ms since the epoch).
    pub presented: u64,
    /// The agent's signature over (audience, request hash, presented).
    pub proof: String,
}

fn proof_body(audience: &str, request_hash: &str, presented: u64) -> Vec<u8> {
    serde_json::to_vec(&ProofBody {
        audience,
        request_hash,
        presented,
    })
    .expect("serializes")
}

impl Presentation {
    /// Made by the agent. Refuses anything that a verifier would refuse anyway
    /// for reasons the agent can see: a different agent, a bad or denied receipt.
    pub fn create(
        agent: &OwnerKey,
        request: SignedRequest,
        receipt: Receipt,
        audience: &str,
        now: u64,
    ) -> Result<Self> {
        request.verify()?;
        receipt.verify_for(&request.req)?;
        if request.req.agent != hex::encode(agent.public()) {
            return Err(Error::BadSignature);
        }
        if receipt.body.decision != Decision::Approved || !valid_audience(audience) {
            return Err(Error::Malformed);
        }
        let hash = request.req.hash();
        let proof = sign(agent, PRESENT_CTX, &proof_body(audience, &hash, now));
        Ok(Self {
            request,
            receipt,
            audience: audience.into(),
            presented: now,
            proof,
        })
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("serializes")
    }
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|_| Error::Malformed)
    }
}

/// How a service is configured.
#[derive(Clone, Debug)]
pub struct Config {
    /// This service's name. Presentations for any other audience are refused.
    pub audience: String,
    /// Owner keys (hex) of the people whose approval this service accepts.
    /// Everyone else's receipts are refused, however well signed.
    pub principals: Vec<String>,
}

/// What the service is about to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action<'a> {
    pub scope: &'a str,
    /// Money the service will actually move, in cents. `None` = none.
    pub amount_cents: Option<u64>,
}

/// Why a presentation was refused. The text is safe to show to the agent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("malformed presentation")]
    Malformed,
    #[error("the request is not validly signed by its agent")]
    BadRequest,
    #[error("the approver is not trusted by this service")]
    UntrustedPrincipal,
    #[error("the receipt is not a valid answer to this request")]
    BadReceipt,
    #[error("the request was denied")]
    NotApproved,
    #[error("presented for a different service")]
    WrongAudience,
    #[error("proof of the agent key is missing or wrong")]
    BadProof,
    #[error("the presentation is too old or dated in the future")]
    StalePresentation,
    #[error("the request has expired")]
    Expired,
    #[error("the request is dated in the future")]
    NotYetValid,
    #[error("approved for scope '{approved}', not '{wanted}'")]
    WrongScope { approved: String, wanted: String },
    #[error("approved for {approved} cents, not {wanted}")]
    AmountTooHigh { approved: u64, wanted: u64 },
    #[error("this approval has already been used")]
    AlreadyUsed,
    #[error("the spent-request store failed: {0}")]
    Store(String),
}

/// What the service may now do: exactly this, once.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Authorized {
    pub request_id: String,
    pub request_hash: String,
    pub agent: String,
    pub principal: String,
    pub scope: String,
    /// What the human approved, in the agent's words.
    pub action: String,
    pub approved_cents: Option<u64>,
    /// Position of the principal's audit entry for this decision.
    pub audit_seq: u64,
}

/// Remembers which approvals have been used, at least until they expire.
pub trait SpentStore {
    /// Record `key` as used. Returns `Ok(false)` if it was already recorded.
    /// Must be atomic: two concurrent calls with one key never both get `true`.
    /// Entries may be forgotten once `now` passes their `keep_until`.
    fn spend(&mut self, key: &str, keep_until: u64, now: u64) -> std::result::Result<bool, String>;
}

/// In-memory [`SpentStore`]: for tests and single-process services that
/// accept losing it on restart. A restart forgets spent approvals that have not
/// yet expired, so prefer a durable store.
#[derive(Default, Debug)]
pub struct MemorySpent(BTreeMap<String, u64>);

impl SpentStore for MemorySpent {
    fn spend(&mut self, key: &str, keep_until: u64, now: u64) -> std::result::Result<bool, String> {
        self.0.retain(|_, until| *until > now);
        if self.0.contains_key(key) {
            return Ok(false);
        }
        self.0.insert(key.into(), keep_until);
        Ok(true)
    }
}

/// Every check except single use. Cheap checks first; nothing is recorded.
pub fn check(
    cfg: &Config,
    p: &Presentation,
    action: &Action,
    now: u64,
) -> std::result::Result<Authorized, Refusal> {
    let req = &p.request.req;
    if !valid_audience(&p.audience) || !is_hex(&p.proof, 128) {
        return Err(Refusal::Malformed);
    }
    p.request.verify().map_err(|_| Refusal::BadRequest)?;
    if !cfg.principals.iter().any(|k| k == &req.principal) {
        return Err(Refusal::UntrustedPrincipal);
    }
    p.receipt.verify_for(req).map_err(|_| Refusal::BadReceipt)?;
    if p.receipt.body.decision != Decision::Approved {
        return Err(Refusal::NotApproved);
    }
    if p.audience != cfg.audience {
        return Err(Refusal::WrongAudience);
    }
    let hash = req.hash();
    verify(
        &req.agent,
        &[PRESENT_CTX, &proof_body(&p.audience, &hash, p.presented)].concat(),
        &p.proof,
    )
    .map_err(|_| Refusal::BadProof)?;
    if p.presented > now + MAX_SKEW_MS || p.presented + MAX_SKEW_MS < now {
        return Err(Refusal::StalePresentation);
    }
    if now >= req.expires {
        return Err(Refusal::Expired);
    }
    if req.created > now + MAX_SKEW_MS {
        return Err(Refusal::NotYetValid);
    }
    // An approval signed after the request lapsed answers nothing.
    if p.receipt.body.decided > req.expires + MAX_SKEW_MS {
        return Err(Refusal::Expired);
    }
    if req.scope != action.scope {
        return Err(Refusal::WrongScope {
            approved: req.scope.clone(),
            wanted: action.scope.into(),
        });
    }
    let (approved, wanted) = (
        req.amount_cents.unwrap_or(0),
        action.amount_cents.unwrap_or(0),
    );
    if wanted > approved {
        return Err(Refusal::AmountTooHigh { approved, wanted });
    }
    Ok(Authorized {
        request_id: req.id.clone(),
        request_hash: hash,
        agent: req.agent.clone(),
        principal: req.principal.clone(),
        scope: req.scope.clone(),
        action: req.action.clone(),
        approved_cents: req.amount_cents,
        audit_seq: p.receipt.body.audit_seq,
    })
}

/// [`check`], then mark the request spent. Call this immediately before acting;
/// on `Ok` the service must act (or accept that the approval is used up).
pub fn redeem(
    cfg: &Config,
    store: &mut dyn SpentStore,
    p: &Presentation,
    action: &Action,
    now: u64,
) -> std::result::Result<Authorized, Refusal> {
    let ok = check(cfg, p, action, now)?;
    let req = &p.request.req;
    // Keyed by who approved, which agent, and the request id: one approval, one use.
    let key = format!("{}/{}/{}", req.principal, req.agent, req.id);
    match store.spend(&key, req.expires + MAX_SKEW_MS, now) {
        Ok(true) => Ok(ok),
        Ok(false) => Err(Refusal::AlreadyUsed),
        Err(e) => Err(Refusal::Store(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{DecidedBy, ReceiptBody, Request, GENESIS};

    const NOW: u64 = 1_800_000_000_000;
    const SVC: &str = "pay.example";

    struct World {
        agent: OwnerKey,
        principal: OwnerKey,
        cfg: Config,
    }
    fn pk(k: &OwnerKey) -> String {
        hex::encode(k.public())
    }
    fn world() -> World {
        let (agent, principal) = (OwnerKey::generate(), OwnerKey::generate());
        let cfg = Config {
            audience: SVC.into(),
            principals: vec![pk(&principal)],
        };
        World {
            agent,
            principal,
            cfg,
        }
    }
    fn signed(w: &World, cents: Option<u64>) -> SignedRequest {
        SignedRequest::sign(
            &w.agent,
            Request {
                id: "ab".repeat(16),
                agent: pk(&w.agent),
                principal: pk(&w.principal),
                scope: "payments.send".into(),
                action: "Buy 3 GPU hours".into(),
                amount_cents: cents,
                created: NOW,
                expires: NOW + 3_600_000,
            },
        )
        .unwrap()
    }
    fn receipt(by: &OwnerKey, req: &Request, decision: Decision) -> Receipt {
        Receipt::sign(
            by,
            ReceiptBody {
                request_id: req.id.clone(),
                request_hash: req.hash(),
                agent: req.agent.clone(),
                principal: req.principal.clone(),
                decision,
                by: DecidedBy::Human,
                decided: NOW + 1000,
                note: String::new(),
                audit_seq: 7,
                audit_hash: GENESIS.into(),
            },
        )
        .unwrap()
    }
    fn present(w: &World, cents: Option<u64>) -> Presentation {
        let s = signed(w, cents);
        let r = receipt(&w.principal, &s.req, Decision::Approved);
        Presentation::create(&w.agent, s, r, SVC, NOW + 2000).unwrap()
    }
    const PAY_1500: Action = Action {
        scope: "payments.send",
        amount_cents: Some(1500),
    };

    #[test]
    fn a_valid_approval_works_exactly_once() {
        let w = world();
        let p = present(&w, Some(1500));
        let p = Presentation::from_json(&p.to_json()).unwrap();
        let mut store = MemorySpent::default();
        let ok = redeem(&w.cfg, &mut store, &p, &PAY_1500, NOW + 3000).unwrap();
        assert_eq!(ok.approved_cents, Some(1500));
        assert_eq!(ok.audit_seq, 7);
        assert_eq!(
            redeem(&w.cfg, &mut store, &p, &PAY_1500, NOW + 4000),
            Err(Refusal::AlreadyUsed)
        );
        // A fresh presentation of the same approval is the same approval.
        let again = Presentation::create(
            &w.agent,
            p.request.clone(),
            p.receipt.clone(),
            SVC,
            NOW + 5000,
        )
        .unwrap();
        assert_eq!(
            redeem(&w.cfg, &mut store, &again, &PAY_1500, NOW + 5000),
            Err(Refusal::AlreadyUsed)
        );
    }

    #[test]
    fn less_money_is_fine_more_is_not_and_scope_must_match() {
        let w = world();
        let p = present(&w, Some(1500));
        let less = Action {
            scope: "payments.send",
            amount_cents: Some(900),
        };
        assert!(check(&w.cfg, &p, &less, NOW + 3000).is_ok());
        let more = Action {
            scope: "payments.send",
            amount_cents: Some(1501),
        };
        assert_eq!(
            check(&w.cfg, &p, &more, NOW + 3000),
            Err(Refusal::AmountTooHigh {
                approved: 1500,
                wanted: 1501
            })
        );
        let other = Action {
            scope: "files.delete",
            amount_cents: None,
        };
        assert!(matches!(
            check(&w.cfg, &p, &other, NOW + 3000),
            Err(Refusal::WrongScope { .. })
        ));
        // No money approved means no money moves.
        let p = present(&w, None);
        assert_eq!(
            check(&w.cfg, &p, &PAY_1500, NOW + 3000),
            Err(Refusal::AmountTooHigh {
                approved: 0,
                wanted: 1500
            })
        );
    }

    #[test]
    fn denied_untrusted_and_forged_receipts_are_refused() {
        let w = world();
        let s = signed(&w, Some(1500));
        let at = NOW + 3000;
        // Denied: the agent cannot present it, and a hand-built one is refused.
        let denied = receipt(&w.principal, &s.req, Decision::Denied);
        assert!(Presentation::create(&w.agent, s.clone(), denied.clone(), SVC, at).is_err());
        let mut p = present(&w, Some(1500));
        p.receipt = denied;
        assert_eq!(check(&w.cfg, &p, &PAY_1500, at), Err(Refusal::NotApproved));
        // Approved by someone this service does not trust.
        let stranger = World {
            agent: OwnerKey::from_hex(&w.agent.to_hex()).unwrap(),
            principal: OwnerKey::generate(),
            cfg: w.cfg.clone(),
        };
        let p = present(&stranger, Some(1500));
        assert_eq!(
            check(&w.cfg, &p, &PAY_1500, at),
            Err(Refusal::UntrustedPrincipal)
        );
        // The agent signing its own approval.
        let mut p = present(&w, Some(1500));
        let mut body = p.receipt.body.clone();
        body.principal = pk(&w.agent);
        p.receipt = Receipt::sign(&w.agent, body).unwrap();
        assert_eq!(check(&w.cfg, &p, &PAY_1500, at), Err(Refusal::BadReceipt));
        // Raising the amount after approval breaks the agent's signature.
        let mut p = present(&w, Some(1500));
        p.request.req.amount_cents = Some(150_000);
        assert_eq!(check(&w.cfg, &p, &PAY_1500, at), Err(Refusal::BadRequest));
        // A real receipt for a different request.
        let mut p = present(&w, Some(1500));
        let other = signed(&w, Some(10));
        p.receipt = receipt(&w.principal, &other.req, Decision::Approved);
        assert_eq!(check(&w.cfg, &p, &PAY_1500, at), Err(Refusal::BadReceipt));
    }

    #[test]
    fn stolen_or_redirected_presentations_are_refused() {
        let w = world();
        let at = NOW + 3000;
        // Captured on its way to another service.
        let p = present(&w, Some(1500));
        let elsewhere = Config {
            audience: "other.example".into(),
            ..w.cfg.clone()
        };
        assert_eq!(
            check(&elsewhere, &p, &PAY_1500, at),
            Err(Refusal::WrongAudience)
        );
        // Relabelled for this service by someone without the agent key.
        let mut relabelled = p.clone();
        relabelled.audience = "other.example".into();
        assert_eq!(
            check(&elsewhere, &relabelled, &PAY_1500, at),
            Err(Refusal::BadProof)
        );
        // A thief who has the request and receipt but not the agent key.
        let thief = OwnerKey::generate();
        let mut stolen = p.clone();
        stolen.proof = sign(
            &thief,
            PRESENT_CTX,
            &proof_body(SVC, &p.request.req.hash(), at),
        );
        stolen.presented = at;
        assert_eq!(
            check(&w.cfg, &stolen, &PAY_1500, at),
            Err(Refusal::BadProof)
        );
        assert!(
            Presentation::create(&thief, p.request.clone(), p.receipt.clone(), SVC, at).is_err()
        );
        // Garbage proof.
        let mut junk = p.clone();
        junk.proof = "zz".into();
        assert_eq!(check(&w.cfg, &junk, &PAY_1500, at), Err(Refusal::Malformed));
    }

    #[test]
    fn time_limits() {
        let w = world();
        let p = present(&w, Some(1500)); // presented at NOW + 2000
        assert_eq!(
            check(&w.cfg, &p, &PAY_1500, NOW + 2000 + MAX_SKEW_MS + 1),
            Err(Refusal::StalePresentation)
        );
        assert_eq!(
            check(&w.cfg, &p, &PAY_1500, NOW + 2000 - MAX_SKEW_MS - 1),
            Err(Refusal::StalePresentation)
        );
        // Fresh presentation, but the request itself has lapsed.
        let late = NOW + 3_600_000;
        let p2 = Presentation::create(&w.agent, p.request.clone(), p.receipt.clone(), SVC, late)
            .unwrap();
        assert_eq!(check(&w.cfg, &p2, &PAY_1500, late), Err(Refusal::Expired));
        // Approved long after the request lapsed.
        let s = signed(&w, Some(1500));
        let mut body = receipt(&w.principal, &s.req, Decision::Approved).body;
        body.decided = s.req.expires + MAX_SKEW_MS + 1;
        let r = Receipt::sign(&w.principal, body).unwrap();
        let p3 = Presentation::create(&w.agent, s, r, SVC, NOW + 3000).unwrap();
        assert_eq!(
            check(&w.cfg, &p3, &PAY_1500, NOW + 3000),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn the_memory_store_forgets_only_after_expiry() {
        let mut s = MemorySpent::default();
        assert_eq!(s.spend("k", 100, 10), Ok(true));
        assert_eq!(s.spend("k", 100, 99), Ok(false));
        assert_eq!(s.spend("k", 200, 101), Ok(true));
        assert!(valid_audience("pay.example.com"));
        assert!(!valid_audience("has space"));
        assert!(!valid_audience(""));
    }
}
