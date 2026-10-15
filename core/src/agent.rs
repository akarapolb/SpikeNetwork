//! Agent requests with human approval (Phase 2). See `docs/adr/0014-agent-requests.md`.
//!
//! An *agent* is an ordinary Spike identity (an [`OwnerKey`]) that a person
//! added as a contact. It asks its *principal* (a person) to allow an action:
//!
//! 1. The agent signs a [`Request`] (scope, amount, expiry) with its owner key.
//! 2. The principal's policy, a signed [`Grant`], decides: deny, ask the human,
//!    or approve automatically for small amounts.
//! 3. The principal signs a [`Receipt`] (approved or denied) and sends it back.
//! 4. Every decision is appended to a hash-chained, signed [`AuditEntry`] log.
//!
//! The log records metadata (scope, amount, decision), never message content.
//! Everything here is pure: no I/O and no clocks except where `now` is passed in.

use crate::owner::OwnerKey;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vodozemac::{Ed25519PublicKey, Ed25519Signature};

const REQ_CTX: &[u8] = b"spike-agent-request-v1";
const GRANT_CTX: &[u8] = b"spike-agent-grant-v1";
const RECEIPT_CTX: &[u8] = b"spike-agent-receipt-v1";
const AUDIT_CTX: &[u8] = b"spike-audit-entry-v1";

/// Longest a request may stay valid, so a stale approval cannot be replayed later.
pub const MAX_REQUEST_LIFETIME_MS: u64 = 24 * 3600 * 1000;
/// How far in the future a request's creation time may be (clock skew allowance).
pub const MAX_SKEW_MS: u64 = 5 * 60 * 1000;
const MAX_TEXT: usize = 500;

pub(crate) fn verify(pub_hex: &str, msg: &[u8], sig_hex: &str) -> Result<()> {
    let pk: [u8; 32] = hex::decode(pub_hex)?
        .try_into()
        .map_err(|_| Error::Malformed)?;
    let pk = Ed25519PublicKey::from_slice(&pk).map_err(|_| Error::Malformed)?;
    let sig =
        Ed25519Signature::from_slice(&hex::decode(sig_hex)?).map_err(|_| Error::BadSignature)?;
    pk.verify(msg, &sig).map_err(|_| Error::BadSignature)
}

pub(crate) fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn sha(ctx: &[u8], body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(ctx);
    h.update(body);
    hex::encode(h.finalize())
}

pub(crate) fn sign(owner: &OwnerKey, ctx: &[u8], body: &[u8]) -> String {
    hex::encode(owner.sign_raw(&[ctx, body].concat()))
}

// ---------------------------------------------------------------- request

/// What an agent asks for. Field order is the signing order; do not reorder.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// 128-bit random id, hex.
    pub id: String,
    /// The agent's owner key (hex). Must be the key that signed.
    pub agent: String,
    /// The person the request is addressed to (owner key, hex). Binds the
    /// request to one principal so it cannot be replayed to another.
    pub principal: String,
    /// Permission class, e.g. `files.read` or `payments.send`.
    pub scope: String,
    /// What the agent wants to do, in words a human will read and approve.
    pub action: String,
    /// Money involved, in the smallest unit (cents). `None` if none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_cents: Option<u64>,
    pub created: u64,
    pub expires: u64,
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        let ok = is_hex(&self.id, 32)
            && is_hex(&self.agent, 64)
            && is_hex(&self.principal, 64)
            && valid_scope(&self.scope)
            && !self.action.is_empty()
            && self.action.len() <= MAX_TEXT
            && !self.action.chars().any(char::is_control)
            && self.expires > self.created
            && self.expires - self.created <= MAX_REQUEST_LIFETIME_MS;
        if ok {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serializes")
    }
    /// Stable identifier of exactly this request, used in receipts and the audit log.
    pub fn hash(&self) -> String {
        sha(REQ_CTX, &self.bytes())
    }
}

/// Scopes are short dotted lowercase names: `a-z`, digits, `.`, `_`, `-`.
pub fn valid_scope(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SignedRequest {
    pub req: Request,
    pub sig: String,
}

impl SignedRequest {
    pub fn sign(agent: &OwnerKey, req: Request) -> Result<Self> {
        req.validate()?;
        if req.agent != hex::encode(agent.public()) {
            return Err(Error::BadSignature);
        }
        let sig = sign(agent, REQ_CTX, &req.bytes());
        Ok(Self { req, sig })
    }
    /// Checks shape and the agent's signature.
    pub fn verify(&self) -> Result<()> {
        self.req.validate()?;
        verify(
            &self.req.agent,
            &[REQ_CTX, &self.req.bytes()].concat(),
            &self.sig,
        )
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("serializes")
    }
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|_| Error::Malformed)
    }
}

// ------------------------------------------------------------------ grant

/// The principal's standing policy for one agent, signed by the principal.
/// Anything outside a grant is denied; there is no implicit permission.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GrantBody {
    pub id: String,
    pub principal: String,
    pub agent: String,
    pub scopes: Vec<String>,
    pub issued: u64,
    pub expires: u64,
    /// Requests above this amount are denied outright. `None` = no money allowed at all.
    pub max_cents: Option<u64>,
    /// Requests at or below this amount are approved without asking. `None` =
    /// always ask a human. `Some(0)` approves requests with no money in them.
    pub auto_below_cents: Option<u64>,
    /// At most this many requests per rolling hour; more are denied.
    pub per_hour: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub body: GrantBody,
    pub sig: String,
}

impl Grant {
    pub fn sign(principal: &OwnerKey, body: GrantBody) -> Result<Self> {
        let ok = is_hex(&body.id, 32)
            && is_hex(&body.agent, 64)
            && body.principal == hex::encode(principal.public())
            && !body.scopes.is_empty()
            && body.scopes.len() <= 32
            && body.scopes.iter().all(|s| valid_scope(s))
            && body.expires > body.issued
            && body.per_hour > 0;
        if !ok {
            return Err(Error::Malformed);
        }
        let bytes = serde_json::to_vec(&body)?;
        Ok(Self {
            sig: sign(principal, GRANT_CTX, &bytes),
            body,
        })
    }
    pub fn verify(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.body)?;
        verify(
            &self.body.principal,
            &[GRANT_CTX, &bytes].concat(),
            &self.sig,
        )
    }
}

/// What the policy says about one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Within the auto-approve limit: approved without asking.
    AutoApprove,
    /// Allowed, but a human must decide.
    AskHuman,
    Deny(String),
}

/// Evaluate `req` against the agent's `grant`. `recent` is how many requests
/// from this agent were received in the last hour (not counting this one).
/// Order matters: cheap, absolute checks first.
pub fn evaluate(grant: Option<&Grant>, req: &Request, now: u64, recent: u32) -> Verdict {
    let Some(g) = grant else {
        return Verdict::Deny("this agent has no grant".into());
    };
    let g = &g.body;
    if g.agent != req.agent || g.principal != req.principal {
        return Verdict::Deny("grant is for a different agent".into());
    }
    if now >= g.expires {
        return Verdict::Deny("the grant has expired".into());
    }
    if now >= req.expires {
        return Verdict::Deny("the request has expired".into());
    }
    if req.created > now + MAX_SKEW_MS {
        return Verdict::Deny("the request is dated in the future".into());
    }
    if !g.scopes.iter().any(|s| s == &req.scope) {
        return Verdict::Deny(format!("scope '{}' is not granted", req.scope));
    }
    let amount = req.amount_cents.unwrap_or(0);
    match g.max_cents {
        Some(max) if amount > max => {
            return Verdict::Deny(format!("amount {amount} is above the limit {max}"))
        }
        None if amount > 0 => return Verdict::Deny("this grant allows no money".into()),
        _ => {}
    }
    if recent >= g.per_hour {
        return Verdict::Deny(format!("more than {} requests in an hour", g.per_hour));
    }
    match g.auto_below_cents {
        Some(limit) if amount <= limit => Verdict::AutoApprove,
        _ => Verdict::AskHuman,
    }
}

// ---------------------------------------------------------------- receipt

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approved,
    Denied,
}

/// Who decided.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    Human,
    /// The grant's own rules (auto-approve or deny).
    Policy,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ReceiptBody {
    pub request_id: String,
    pub request_hash: String,
    pub agent: String,
    pub principal: String,
    pub decision: Decision,
    pub by: DecidedBy,
    pub decided: u64,
    /// A short reason or note. Shown to the agent.
    pub note: String,
    /// Position and hash of the audit entry that recorded this decision, so
    /// the agent holds proof of the log's state and silent truncation shows.
    pub audit_seq: u64,
    pub audit_hash: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub body: ReceiptBody,
    pub sig: String,
}

impl Receipt {
    pub fn sign(principal: &OwnerKey, body: ReceiptBody) -> Result<Self> {
        if body.principal != hex::encode(principal.public())
            || !is_hex(&body.request_id, 32)
            || !is_hex(&body.request_hash, 64)
            || body.note.len() > MAX_TEXT
        {
            return Err(Error::Malformed);
        }
        let bytes = serde_json::to_vec(&body)?;
        Ok(Self {
            sig: sign(principal, RECEIPT_CTX, &bytes),
            body,
        })
    }
    /// Signature by the principal, and that it answers exactly `req`.
    pub fn verify_for(&self, req: &Request) -> Result<()> {
        let b = &self.body;
        if b.request_id != req.id
            || b.request_hash != req.hash()
            || b.agent != req.agent
            || b.principal != req.principal
        {
            return Err(Error::Malformed);
        }
        let bytes = serde_json::to_vec(b)?;
        verify(&b.principal, &[RECEIPT_CTX, &bytes].concat(), &self.sig)
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("serializes")
    }
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|_| Error::Malformed)
    }
}

// -------------------------------------------------------------- audit log

/// One line of the tamper-evident log. `hash` covers every other field plus the
/// previous entry's hash; `sig` is the owner's signature over `hash`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    pub seq: u64,
    pub ts: u64,
    /// `request`, `approved`, `denied`, `grant`, `revoke`.
    pub event: String,
    pub agent: String,
    /// Request hash for request events; grant id for grant events.
    pub subject: String,
    /// Scope and amount only, never content: e.g. `payments.send 1500`.
    pub detail: String,
    pub prev: String,
    pub hash: String,
    pub sig: String,
}

/// Zero hash that starts every chain.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Serialize)]
struct EntryBody<'a> {
    seq: u64,
    ts: u64,
    event: &'a str,
    agent: &'a str,
    subject: &'a str,
    detail: &'a str,
    prev: &'a str,
}

impl AuditEntry {
    fn hash_of(
        seq: u64,
        ts: u64,
        event: &str,
        agent: &str,
        subject: &str,
        detail: &str,
        prev: &str,
    ) -> String {
        let body = serde_json::to_vec(&EntryBody {
            seq,
            ts,
            event,
            agent,
            subject,
            detail,
            prev,
        })
        .expect("serializes");
        sha(AUDIT_CTX, &body)
    }

    fn recompute(&self) -> String {
        Self::hash_of(
            self.seq,
            self.ts,
            &self.event,
            &self.agent,
            &self.subject,
            &self.detail,
            &self.prev,
        )
    }

    /// Next entry after `prev` (`None` starts the chain at sequence 1).
    pub fn next(
        owner: &OwnerKey,
        prev: Option<&AuditEntry>,
        ts: u64,
        event: &str,
        agent: &str,
        subject: &str,
        detail: &str,
    ) -> Self {
        let seq = prev.map_or(1, |p| p.seq + 1);
        let prev_hash = prev.map_or(GENESIS, |p| p.hash.as_str());
        let hash = Self::hash_of(seq, ts, event, agent, subject, detail, prev_hash);
        let sig = hex::encode(owner.sign_raw(hash.as_bytes()));
        Self {
            seq,
            ts,
            event: event.into(),
            agent: agent.into(),
            subject: subject.into(),
            detail: detail.into(),
            prev: prev_hash.into(),
            hash,
            sig,
        }
    }
}

/// Check a whole log: contiguous sequence, every link, every hash, every
/// signature. Returns the head (seq, hash), or `None` for an empty log.
/// Editing or removing any entry but the last breaks a link; removing the last
/// is caught by comparing against a receipt's `audit_seq`/`audit_hash`.
pub fn verify_log(entries: &[AuditEntry], owner_pub_hex: &str) -> Result<Option<(u64, String)>> {
    let mut prev = GENESIS.to_string();
    for (i, e) in entries.iter().enumerate() {
        let want = e.recompute();
        if e.seq != i as u64 + 1 || e.prev != prev || e.hash != want {
            return Err(Error::Corrupted);
        }
        verify(owner_pub_hex, e.hash.as_bytes(), &e.sig).map_err(|_| Error::Corrupted)?;
        prev = e.hash.clone();
    }
    Ok(entries.last().map(|e| (e.seq, e.hash.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn pk(k: &OwnerKey) -> String {
        hex::encode(k.public())
    }
    fn request(agent: &OwnerKey, principal: &OwnerKey, scope: &str, cents: Option<u64>) -> Request {
        Request {
            id: "ab".repeat(16),
            agent: pk(agent),
            principal: pk(principal),
            scope: scope.into(),
            action: "Buy 3 GPU hours".into(),
            amount_cents: cents,
            created: NOW,
            expires: NOW + 3_600_000,
        }
    }
    fn grant(agent: &OwnerKey, principal: &OwnerKey) -> Grant {
        Grant::sign(
            principal,
            GrantBody {
                id: "cd".repeat(16),
                principal: pk(principal),
                agent: pk(agent),
                scopes: vec!["payments.send".into(), "files.read".into()],
                issued: NOW - 1000,
                expires: NOW + 86_400_000,
                max_cents: Some(5000),
                auto_below_cents: Some(500),
                per_hour: 3,
            },
        )
        .unwrap()
    }

    #[test]
    fn request_signature_binds_every_field() {
        let (a, p) = (OwnerKey::generate(), OwnerKey::generate());
        let s = SignedRequest::sign(&a, request(&a, &p, "payments.send", Some(100))).unwrap();
        s.verify().unwrap();
        let mut bigger = s.clone();
        bigger.req.amount_cents = Some(99_999);
        assert!(bigger.verify().is_err());
        let mut other_principal = s.clone();
        other_principal.req.principal = pk(&a);
        assert!(other_principal.verify().is_err());
        // Someone else cannot sign for the agent.
        assert!(SignedRequest::sign(&p, request(&a, &p, "files.read", None)).is_err());
        // Over-long lifetimes and control characters are refused.
        let mut long = request(&a, &p, "files.read", None);
        long.expires = long.created + MAX_REQUEST_LIFETIME_MS + 1;
        assert!(SignedRequest::sign(&a, long).is_err());
        let mut ctrl = request(&a, &p, "files.read", None);
        ctrl.action = "ok\u{202e}\n".into();
        assert!(SignedRequest::sign(&a, ctrl).is_err());
        assert!(SignedRequest::sign(&a, request(&a, &p, "Bad Scope", None)).is_err());
    }

    #[test]
    fn policy_decisions() {
        let (a, p) = (OwnerKey::generate(), OwnerKey::generate());
        let g = grant(&a, &p);
        g.verify().unwrap();
        let ev = |scope: &str, cents: Option<u64>, now: u64, recent: u32| {
            evaluate(Some(&g), &request(&a, &p, scope, cents), now, recent)
        };
        assert_eq!(ev("payments.send", Some(500), NOW, 0), Verdict::AutoApprove);
        assert_eq!(ev("files.read", None, NOW, 0), Verdict::AutoApprove);
        assert_eq!(ev("payments.send", Some(501), NOW, 0), Verdict::AskHuman);
        assert_eq!(ev("payments.send", Some(5000), NOW, 0), Verdict::AskHuman);
        for denied in [
            ev("payments.send", Some(5001), NOW, 0),
            ev("admin.delete", None, NOW, 0),
            ev("files.read", None, NOW, 3),
            ev("files.read", None, NOW + 4_000_000, 0), // request expired
            ev("files.read", None, NOW + 90_000_000, 0), // grant expired
            ev("files.read", None, NOW - 10 * 60_000, 0), // dated in the future
        ] {
            assert!(matches!(denied, Verdict::Deny(_)), "{denied:?}");
        }
        assert!(matches!(
            evaluate(None, &request(&a, &p, "files.read", None), NOW, 0),
            Verdict::Deny(_)
        ));
        // A grant for one agent does not cover another.
        let other = OwnerKey::generate();
        assert!(matches!(
            evaluate(Some(&g), &request(&other, &p, "files.read", None), NOW, 0),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn money_needs_an_explicit_limit_and_asking_is_the_default() {
        let (a, p) = (OwnerKey::generate(), OwnerKey::generate());
        let mut body = grant(&a, &p).body;
        body.max_cents = None;
        body.auto_below_cents = None;
        let g = Grant::sign(&p, body).unwrap();
        let r = |c| request(&a, &p, "payments.send", c);
        assert!(matches!(
            evaluate(Some(&g), &r(Some(1)), NOW, 0),
            Verdict::Deny(_)
        ));
        assert_eq!(evaluate(Some(&g), &r(None), NOW, 0), Verdict::AskHuman);
    }

    #[test]
    fn grant_tampering_is_detected() {
        let (a, p) = (OwnerKey::generate(), OwnerKey::generate());
        let mut g = grant(&a, &p);
        g.body.max_cents = Some(10_000_000);
        assert!(g.verify().is_err());
    }

    #[test]
    fn receipts_answer_exactly_one_request() {
        let (a, p) = (OwnerKey::generate(), OwnerKey::generate());
        let req = request(&a, &p, "payments.send", Some(1500));
        let body = ReceiptBody {
            request_id: req.id.clone(),
            request_hash: req.hash(),
            agent: req.agent.clone(),
            principal: req.principal.clone(),
            decision: Decision::Approved,
            by: DecidedBy::Human,
            decided: NOW,
            note: String::new(),
            audit_seq: 1,
            audit_hash: GENESIS.into(),
        };
        let r = Receipt::sign(&p, body).unwrap();
        r.verify_for(&req).unwrap();
        // Flip the decision: signature fails.
        let mut flipped = r.clone();
        flipped.body.decision = Decision::Denied;
        assert!(flipped.verify_for(&req).is_err());
        // The same receipt does not answer a different request (even one with the same id).
        let mut other = req.clone();
        other.amount_cents = Some(150_000);
        assert!(r.verify_for(&other).is_err());
        // The agent cannot forge approval from itself.
        assert!(Receipt::sign(&a, r.body.clone()).is_err());
    }

    #[test]
    fn audit_log_detects_edit_removal_reorder_and_forgery() {
        let p = OwnerKey::generate();
        let mut log: Vec<AuditEntry> = Vec::new();
        for (i, ev) in ["request", "approved", "request", "denied"]
            .iter()
            .enumerate()
        {
            let e = AuditEntry::next(
                &p,
                log.last(),
                NOW + i as u64,
                ev,
                "agent",
                "subj",
                "files.read",
            );
            log.push(e);
        }
        let head = verify_log(&log, &pk(&p)).unwrap().unwrap();
        assert_eq!(head.0, 4);
        assert_eq!(verify_log(&[], &pk(&p)).unwrap(), None);

        let mut edited = log.clone();
        edited[1].event = "denied".into();
        let mut removed = log.clone();
        removed.remove(1);
        let mut swapped = log.clone();
        swapped.swap(1, 2);
        // Editing an entry and fixing its hash still breaks the next link and the signature.
        let mut rehashed = log.clone();
        rehashed[1].event = "denied".into();
        rehashed[1].hash = rehashed[1].recompute();
        let forger = OwnerKey::generate();
        let mut forged = log.clone();
        forged[3] = AuditEntry::next(&forger, Some(&log[2]), NOW, "denied", "agent", "subj", "x");
        for bad in [edited, removed, swapped, rehashed, forged] {
            assert!(verify_log(&bad, &pk(&p)).is_err());
        }
        // Truncating the tail is invisible to the log alone; the head differs.
        let short = verify_log(&log[..3], &pk(&p)).unwrap().unwrap();
        assert_ne!(short, head, "a receipt's audit head exposes truncation");
    }
}
