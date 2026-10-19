//! Agent requests with human approval (Phase 2): storage and flow around the
//! signed objects in `spike_core::agent`. See `docs/adr/0014-agent-requests.md`.
//!
//! Roles are per conversation, not per install: the same home can be an agent
//! toward one contact and a principal toward another.
//!
//! Principal side: `on_request` (from `process`) -> policy -> `decide` (human).
//! Agent side:     `new_request` -> send -> `on_receipt` (from `process`).
//!
//! Only the *approver device* (first in the owner's signed device list)
//! handles requests, so one request is decided and logged once. See the ADR.

use crate::Outcome;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use spike_core::agent::{
    evaluate, verify_log, AuditEntry, DecidedBy, Decision, Grant, GrantBody, Receipt, ReceiptBody,
    Request, SignedRequest, Verdict,
};
use spike_core::owner::OwnerKey;
use std::collections::BTreeMap;

const GRANTS: &str = "agent-grants.enc";
const INBOX: &str = "agent-inbox.enc";
const OUTBOX: &str = "agent-outbox.enc";
const AUDIT: &str = "agent-audit.enc";
const REMOTE: &str = "agent-remote.enc";

/// Requests waiting for a human are capped, so an agent cannot fill the screen or the disk.
pub const MAX_PENDING: usize = 100;
const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;

/// Money approved or still awaiting a decision for `agent` in the last 24 hours,
/// leaving out the request `skip` (the one being decided). Denied requests count nothing.
fn committed_24h(all: &[Inbound], agent: &str, now: u64, skip: Option<&str>) -> u64 {
    all.iter()
        .filter(|r| {
            r.signed.req.agent == agent
                && r.received + DAY_MS > now
                && skip != Some(r.signed.req.id.as_str())
                && match &r.status {
                    Status::Pending => true,
                    Status::Decided { receipt, .. } => receipt.body.decision == Decision::Approved,
                }
        })
        .map(|r| r.signed.req.amount_cents.unwrap_or(0))
        .fold(0, u64::saturating_add)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ------------------------------------------------------------ principal side

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Pending,
    Decided { receipt: Receipt, delivered: bool },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Inbound {
    pub signed: SignedRequest,
    /// Contact name at the time.
    pub from: String,
    pub received: u64,
    pub status: Status,
}

use crate::state::Home;

fn inbox(home: &Home) -> Result<Vec<Inbound>> {
    home.load_json(INBOX)
}
fn grants(home: &Home) -> Result<Vec<Grant>> {
    home.load_json(GRANTS)
}

pub fn list_inbox(home: &Home) -> Result<Vec<Inbound>> {
    inbox(home)
}
pub fn list_grants(home: &Home) -> Result<Vec<Grant>> {
    grants(home)
}

fn audit_log(home: &Home) -> Result<Vec<AuditEntry>> {
    home.load_json(AUDIT)
}

fn append_audit(
    home: &Home,
    owner: &OwnerKey,
    event: &str,
    agent: &str,
    subject: &str,
    detail: &str,
) -> Result<AuditEntry> {
    let mut log = audit_log(home)?;
    let e = AuditEntry::next(owner, log.last(), now_ms(), event, agent, subject, detail);
    log.push(e.clone());
    home.save_json(AUDIT, &log)?;
    Ok(e)
}

/// The log, verified end to end. Fails if anything was edited, removed or reordered.
pub fn verified_audit(home: &Home) -> Result<Vec<AuditEntry>> {
    let log = audit_log(home)?;
    let owner = hex::encode(home.load_owner()?.public());
    verify_log(&log, &owner).map_err(|_| anyhow!("the audit log has been tampered with"))?;
    Ok(log)
}

/// Give `agent` (a contact's owner key) a standing policy, replacing any earlier grant.
#[allow(clippy::too_many_arguments)] // one argument per policy field; a struct would only move them
pub fn grant(
    home: &Home,
    agent: &str,
    scopes: Vec<String>,
    days: u64,
    max_cents: Option<u64>,
    auto_below_cents: Option<u64>,
    per_hour: u32,
    daily_cents: Option<u64>,
    typed_only: bool,
) -> Result<Grant> {
    let owner = home.load_owner()?;
    let now = now_ms();
    let mut id = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    if let (Some(daily), Some(max)) = (daily_cents, max_cents) {
        if daily < max {
            bail!("--daily-cents cannot be below --max-cents");
        }
    }
    if typed_only {
        if let Some(s) = scopes
            .iter()
            .find(|s| spike_core::action::lookup(s).is_none())
        {
            bail!("--typed-only: '{s}' is not a known action type (see `spike agent-types`)");
        }
    }
    if let (Some(auto), Some(max)) = (auto_below_cents, max_cents) {
        if auto > max {
            bail!("--auto-below-cents cannot exceed --max-cents");
        }
    }
    let g = Grant::sign(
        &owner,
        GrantBody {
            id: hex::encode(id),
            principal: hex::encode(owner.public()),
            agent: agent.into(),
            scopes,
            issued: now,
            expires: now + days.max(1) * 86_400_000,
            max_cents,
            auto_below_cents,
            per_hour,
            daily_cents,
            typed_only,
        },
    )?;
    let mut all = grants(home)?;
    all.retain(|x| x.body.agent != agent);
    all.push(g.clone());
    home.save_json(GRANTS, &all)?;
    append_audit(
        home,
        &owner,
        "grant",
        agent,
        &g.body.id,
        &g.body.scopes.join(","),
    )?;
    Ok(g)
}

/// Remove an agent's grant. Pending requests from it can no longer be approved.
pub fn revoke(home: &Home, agent: &str) -> Result<bool> {
    let mut all = grants(home)?;
    let Some(g) = all.iter().find(|g| g.body.agent == agent).cloned() else {
        return Ok(false);
    };
    all.retain(|x| x.body.agent != agent);
    home.save_json(GRANTS, &all)?;
    append_audit(home, &home.load_owner()?, "revoke", agent, &g.body.id, "")?;
    Ok(true)
}

/// Whether this device is the one that decides requests: the first device in
/// the owner's signed list.
pub fn is_approver(home: &Home, my_device_id: &str) -> Result<bool> {
    Ok(home
        .load_device_list()?
        .verify()?
        .first()
        .is_some_and(|d| d.device_id() == my_device_id))
}

fn make_receipt(
    home: &Home,
    owner: &OwnerKey,
    req: &Request,
    decision: Decision,
    by: DecidedBy,
    note: &str,
) -> Result<Receipt> {
    let entry = append_audit(
        home,
        owner,
        if decision == Decision::Approved {
            "approved"
        } else {
            "denied"
        },
        &req.agent,
        &req.hash(),
        &format!("{} {}", req.scope, req.amount_cents.unwrap_or(0)),
    )?;
    Ok(Receipt::sign(
        owner,
        ReceiptBody {
            request_id: req.id.clone(),
            request_hash: req.hash(),
            agent: req.agent.clone(),
            principal: req.principal.clone(),
            decision,
            by,
            decided: now_ms(),
            note: note.chars().take(200).collect(),
            audit_seq: entry.seq,
            audit_hash: entry.hash,
        },
    )?)
}

/// A signed request arrived from contact `from` (whose owner key is `from_key`).
/// Verifies it, applies the agent's grant, and either decides it or leaves it
/// for a human. Returns an outcome; a decided request has a receipt to send.
pub fn on_request(
    home: &Home,
    my_device_id: &str,
    from: &str,
    from_key: &str,
    json: &str,
) -> Result<Outcome> {
    if !is_approver(home, my_device_id)? {
        return Ok(Outcome::Ignored);
    }
    let bad = |reason: &str| Outcome::Rejected {
        reason: format!("request from {from}: {reason}"),
    };
    let Ok(signed) = SignedRequest::from_json(json) else {
        return Ok(bad("malformed"));
    };
    let owner = home.load_owner()?;
    if signed.verify().is_err() {
        return Ok(bad("bad signature"));
    }
    let req = &signed.req;
    if req.agent != from_key {
        return Ok(bad("signed by a key other than the sender's"));
    }
    if req.principal != hex::encode(owner.public()) {
        return Ok(bad("addressed to someone else"));
    }
    let mut all = inbox(home)?;
    if all.iter().any(|r| r.signed.req.id == req.id) {
        return Ok(Outcome::Duplicate);
    }
    let now = now_ms();
    let recent = all
        .iter()
        .filter(|r| r.signed.req.agent == req.agent && r.received + HOUR_MS > now)
        .count() as u32;
    let pending = all.iter().filter(|r| r.status == Status::Pending).count();
    let grant = grants(home)?
        .into_iter()
        .find(|g| g.body.agent == req.agent);
    let committed = committed_24h(&all, &req.agent, now, None);
    let verdict = match evaluate(grant.as_ref(), req, now, recent, committed) {
        Verdict::AskHuman if pending >= MAX_PENDING => {
            Verdict::Deny("too many requests are waiting for approval".into())
        }
        v => v,
    };
    append_audit(
        home,
        &owner,
        "request",
        &req.agent,
        &req.hash(),
        &format!("{} {}", req.scope, req.amount_cents.unwrap_or(0)),
    )?;
    let (status, label) = match &verdict {
        Verdict::AskHuman => (Status::Pending, "pending".to_string()),
        Verdict::AutoApprove => {
            let r = make_receipt(
                home,
                &owner,
                req,
                Decision::Approved,
                DecidedBy::Policy,
                "within the auto-approve limit",
            )?;
            (
                Status::Decided {
                    receipt: r,
                    delivered: false,
                },
                "approved by policy".to_string(),
            )
        }
        Verdict::Deny(why) => {
            let r = make_receipt(home, &owner, req, Decision::Denied, DecidedBy::Policy, why)?;
            (
                Status::Decided {
                    receipt: r,
                    delivered: false,
                },
                format!("denied by policy: {why}"),
            )
        }
    };
    let out = Outcome::AgentRequest {
        from: from.into(),
        id: req.id.clone(),
        scope: req.scope.clone(),
        action: req.action.clone(),
        amount_cents: req.amount_cents,
        summary: req.summary(),
        verdict: label,
    };
    all.push(Inbound {
        signed,
        from: from.into(),
        received: now,
        status,
    });
    home.save_json(INBOX, &all)?;
    Ok(out)
}

// ------------------------------------------------- deciding from another device

/// A request the approver device forwarded to this device to be decided here.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Remote {
    pub signed: SignedRequest,
    pub from: String,
    pub received: u64,
}

/// Forwarded requests that have not expired.
pub fn list_remote(home: &Home) -> Result<Vec<Remote>> {
    let now = now_ms();
    Ok(home
        .load_json::<Vec<Remote>>(REMOTE)?
        .into_iter()
        .filter(|r| r.signed.req.expires > now)
        .collect())
}

/// A waiting request, for forwarding (approver side).
pub fn pending_request(home: &Home, id: &str) -> Result<Option<Inbound>> {
    Ok(inbox(home)?
        .into_iter()
        .find(|r| r.signed.req.id == id && r.status == Status::Pending))
}

/// Another device of ours forwarded a waiting request. Accepted only from the
/// approver device, only for requests addressed to us, and only if validly
/// signed by the agent. The approver stays the one place that decides and logs.
pub fn on_pending(
    home: &Home,
    my_device_id: &str,
    sender_device: &str,
    from: &str,
    signed: SignedRequest,
) -> Result<Outcome> {
    let bad = |why: &str| Outcome::Rejected {
        reason: format!("forwarded request: {why}"),
    };
    if is_approver(home, my_device_id)? {
        return Ok(Outcome::Ignored);
    }
    if !is_approver(home, sender_device)? {
        return Ok(bad("not from your approver device"));
    }
    let req = &signed.req;
    if signed.verify().is_err() || req.principal != hex::encode(home.load_owner()?.public()) {
        return Ok(bad("not a valid request addressed to you"));
    }
    let now = now_ms();
    if req.expires <= now {
        return Ok(Outcome::Ignored);
    }
    let mut all = home.load_json::<Vec<Remote>>(REMOTE)?;
    all.retain(|r| r.signed.req.expires > now);
    if all.iter().any(|r| r.signed.req.id == req.id) {
        return Ok(Outcome::Duplicate);
    }
    if all.len() >= MAX_PENDING {
        return Ok(bad("too many are waiting"));
    }
    let out = Outcome::AgentRemotePending {
        from: from.into(),
        id: req.id.clone(),
        scope: req.scope.clone(),
        action: req.action.clone(),
        amount_cents: req.amount_cents,
        summary: req.summary(),
    };
    all.push(Remote {
        signed,
        from: from.into(),
        received: now,
    });
    home.save_json(REMOTE, &all)?;
    Ok(out)
}

/// Find a forwarded request by id prefix (4+ characters, unambiguous).
pub fn find_remote(home: &Home, prefix: &str) -> Result<Remote> {
    if prefix.len() < 4 {
        bail!("give at least 4 characters of the request id");
    }
    let hits: Vec<Remote> = list_remote(home)?
        .into_iter()
        .filter(|r| r.signed.req.id.starts_with(prefix))
        .collect();
    match hits.as_slice() {
        [r] => Ok(r.clone()),
        [] => bail!("no waiting request starts with '{prefix}'"),
        _ => bail!("'{prefix}' matches several requests; type more characters"),
    }
}

pub fn forget_remote(home: &Home, id: &str) -> Result<()> {
    let mut all = home.load_json::<Vec<Remote>>(REMOTE)?;
    all.retain(|r| r.signed.req.id != id);
    home.save_json(REMOTE, &all)
}

/// The approver device receives a decision made on another of our devices
/// (already authenticated as ours by the session it arrived on). It applies it
/// exactly as if the human had decided here, including the grant re-check.
pub fn on_remote_decision(
    home: &Home,
    my_device_id: &str,
    request_id: &str,
    approve: bool,
    note: &str,
) -> Result<Outcome> {
    if !is_approver(home, my_device_id)? {
        return Ok(Outcome::Ignored);
    }
    Ok(match decide(home, request_id, approve, note) {
        Ok(_) => Outcome::AgentRemoteDecision {
            id: request_id.into(),
            approved: approve,
        },
        Err(e) => Outcome::Rejected {
            reason: format!("decision from your other device: {e}"),
        },
    })
}

/// Find a request by id prefix (at least 4 hex characters, unambiguous).
fn find(all: &[Inbound], prefix: &str) -> Result<usize> {
    if prefix.len() < 4 {
        bail!("give at least 4 characters of the request id");
    }
    let hits: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, r)| r.signed.req.id.starts_with(prefix))
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [i] => Ok(*i),
        [] => bail!("no request starts with '{prefix}'"),
        _ => bail!("'{prefix}' matches several requests; type more characters"),
    }
}

/// The human's decision on a pending request. Approval is re-checked against
/// the grant as it is *now*: a revoked or expired grant cannot approve.
pub fn decide(home: &Home, prefix: &str, approve: bool, note: &str) -> Result<Inbound> {
    let mut all = inbox(home)?;
    let i = find(&all, prefix)?;
    if all[i].status != Status::Pending {
        bail!("that request was already decided");
    }
    let req = all[i].signed.req.clone();
    let owner = home.load_owner()?;
    if approve {
        let grant = grants(home)?
            .into_iter()
            .find(|g| g.body.agent == req.agent);
        let now = now_ms();
        let others = committed_24h(&all, &req.agent, now, Some(&req.id));
        if let Verdict::Deny(why) = evaluate(grant.as_ref(), &req, now, 0, others) {
            bail!("cannot approve: {why} (deny it instead)");
        }
    }
    let r = make_receipt(
        home,
        &owner,
        &req,
        if approve {
            Decision::Approved
        } else {
            Decision::Denied
        },
        DecidedBy::Human,
        note,
    )?;
    all[i].status = Status::Decided {
        receipt: r,
        delivered: false,
    };
    home.save_json(INBOX, &all)?;
    Ok(all[i].clone())
}

/// Decided requests whose receipts have not reached the agent yet.
pub fn undelivered(home: &Home) -> Result<Vec<Inbound>> {
    Ok(inbox(home)?
        .into_iter()
        .filter(|r| {
            matches!(
                r.status,
                Status::Decided {
                    delivered: false,
                    ..
                }
            )
        })
        .collect())
}

pub fn mark_delivered(home: &Home, id: &str) -> Result<()> {
    let mut all = inbox(home)?;
    for r in all.iter_mut().filter(|r| r.signed.req.id == id) {
        if let Status::Decided { delivered, .. } = &mut r.status {
            *delivered = true;
        }
    }
    home.save_json(INBOX, &all)
}

// ---------------------------------------------------------------- agent side

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Outbound {
    pub signed: SignedRequest,
    /// Contact name of the principal.
    pub to: String,
    pub receipt: Option<Receipt>,
}

#[derive(Serialize, Deserialize, Default)]
struct Outbox {
    requests: Vec<Outbound>,
    /// Newest audit position seen from each principal: (seq, hash).
    heads: BTreeMap<String, (u64, String)>,
}

pub fn list_outbox(home: &Home) -> Result<Vec<Outbound>> {
    Ok(home.load_json::<Outbox>(OUTBOX)?.requests)
}

/// Sign a new request to `principal` (a contact's owner key) and remember it.
#[allow(clippy::too_many_arguments)] // one argument per request field
pub fn new_request(
    home: &Home,
    to_name: &str,
    principal: &str,
    scope: &str,
    action: &str,
    amount_cents: Option<u64>,
    typed: Option<spike_core::action::Fields>,
    minutes: u64,
) -> Result<SignedRequest> {
    if let Some(f) = &typed {
        spike_core::action::validate(scope, f, amount_cents)
            .map_err(|e| anyhow!("typed action {scope}: {e}"))?;
    }
    let owner = home.load_owner()?;
    let now = now_ms();
    let mut id = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let signed = SignedRequest::sign(
        &owner,
        Request {
            id: hex::encode(id),
            agent: hex::encode(owner.public()),
            principal: principal.into(),
            scope: scope.into(),
            action: action.into(),
            amount_cents,
            typed,
            created: now,
            expires: now + minutes.max(1) * 60_000,
        },
    )?;
    let mut ob: Outbox = home.load_json(OUTBOX)?;
    ob.requests.push(Outbound {
        signed: signed.clone(),
        to: to_name.into(),
        receipt: None,
    });
    home.save_json(OUTBOX, &ob)?;
    Ok(signed)
}

/// A receipt arrived from contact `from` (owner key `from_key`).
pub fn on_receipt(home: &Home, from: &str, from_key: &str, json: &str) -> Result<Outcome> {
    let bad = |reason: &str| Outcome::Rejected {
        reason: format!("receipt from {from}: {reason}"),
    };
    let Ok(receipt) = Receipt::from_json(json) else {
        return Ok(bad("malformed"));
    };
    let mut ob: Outbox = home.load_json(OUTBOX)?;
    let Some(rec) = ob
        .requests
        .iter_mut()
        .find(|r| r.signed.req.id == receipt.body.request_id)
    else {
        return Ok(bad("answers a request we never made"));
    };
    if rec.signed.req.principal != from_key {
        return Ok(bad("not from the person the request was sent to"));
    }
    if receipt.verify_for(&rec.signed.req).is_err() {
        return Ok(bad("signature or request does not match"));
    }
    if rec.receipt.is_some() {
        return Ok(Outcome::Duplicate);
    }
    let b = &receipt.body;
    // The principal's audit position may only move forward, and never fork.
    if let Some((seq, hash)) = ob.heads.get(from_key) {
        if b.audit_seq < *seq || (b.audit_seq == *seq && b.audit_hash != *hash) {
            return Ok(bad(
                "its audit log went backwards or forked: possible tampering",
            ));
        }
    }
    ob.heads
        .insert(from_key.into(), (b.audit_seq, b.audit_hash.clone()));
    let out = Outcome::AgentReceipt {
        from: from.into(),
        id: b.request_id.clone(),
        approved: b.decision == Decision::Approved,
        by_policy: b.by == DecidedBy::Policy,
        note: b.note.clone(),
    };
    rec.receipt = Some(receipt);
    home.save_json(OUTBOX, &ob)?;
    Ok(out)
}
