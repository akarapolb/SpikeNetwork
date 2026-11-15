//! Tenants and enrollment on the relay (ADR 0023).
//!
//! With `Config::require_enrollment` off (the default, "open" mode) none of this runs: anyone may
//! sign in, tokens are ignored, and tenants can be prepared by the operator for later.
//!
//! With it on:
//! - A device enrolled in an active tenant signs in as a **member**, held to its tenant's limits.
//! - A device that presents a valid token for itself is enrolled first (the token is redeemed once;
//!   the enrollment then lasts until revoked).
//! - Any other device signs in as a **visitor** (unless `enroll_visitors` is off): it may reach the
//!   relay's members (send to them, claim their keys, read their attachments, use group mailboxes
//!   it holds a key for) but stores nothing for itself. Without this, nobody outside an
//!   organisation could message its members, because a sender delivers through the recipient's
//!   home relay.
//!
//! The relay learns which tenant a device id belongs to and nothing about who uses either.

use super::{err, Config, Shared};
use crate::limits::TokenBucket;
use crate::store::Enrolled;
use spike_core::enroll::{
    limit_name, AdminBody, AdminOp, AdminRequest, EnrollToken, Revocation, TenantLimits,
    TenantUsage,
};
use spike_core::wire::{ClientMsg, ServerMsg};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How far an operator request's time may be from the relay's clock.
const ADMIN_SKEW_SECS: u64 = 300;
/// How far in the future a token's expiry may be beyond `enroll_max_token_secs` (clock skew).
const TOKEN_SKEW_SECS: u64 = 600;

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Live, in-memory numbers per tenant: open connections, the shared request bucket, refusals.
#[derive(Default)]
pub(crate) struct Runtime {
    tenants: HashMap<String, Live>,
    /// Operator request nonces seen recently, so a captured request cannot be replayed.
    nonces: HashMap<String, Instant>,
}

struct Live {
    conns: u64,
    bucket: TokenBucket,
    refused: u64,
}

impl Live {
    fn new(l: &TenantLimits) -> Self {
        Self {
            conns: 0,
            bucket: TokenBucket::new(l.req_rate, l.req_burst()),
            refused: 0,
        }
    }
}

/// Who signed in, as far as tenancy is concerned.
pub(crate) enum Admitted {
    /// Open mode: no tenancy.
    Open,
    /// An enrolled device of this tenant.
    Member(String),
    /// A device from elsewhere, allowed to reach this relay's members only.
    Visitor,
}

/// Holds one of a tenant's connection places until the connection ends.
pub(crate) struct TenantConn {
    state: Arc<Shared>,
    tenant: String,
}

impl Drop for TenantConn {
    fn drop(&mut self) {
        let mut rt = self.state.tenancy.lock().unwrap();
        if let Some(l) = rt.tenants.get_mut(&self.tenant) {
            l.conns = l.conns.saturating_sub(1);
        }
    }
}

/// The refusal for a tenant limit, counted for the operator's usage numbers. The message starts
/// with the limit's name so a client can say which one in words.
pub(crate) fn over_limit(state: &Shared, tenant: &str, name: &str) -> ServerMsg {
    if let Some(l) = state.tenancy.lock().unwrap().tenants.get_mut(tenant) {
        l.refused += 1;
    }
    let words = match name {
        limit_name::DEVICES => "the organisation has enrolled as many devices as it may",
        limit_name::QUEUE_DEPTH => {
            "the recipient has as many waiting messages as its organisation allows"
        }
        limit_name::ATTACHMENT_BYTES => "the organisation's attachment storage is full",
        limit_name::CONNECTIONS => "the organisation has as many open connections as it may",
        limit_name::REQ_RATE => "the organisation's devices are asking too fast: slow down",
        _ => "an organisation limit was reached",
    };
    err("tenant_over_limit", &format!("{name}: {words}"))
}

/// Decide whether a signed-in device may stay, enrolling it if it brought a valid token. Runs on a
/// blocking thread. `Err` is the refusal to send before hanging up.
pub(crate) fn admit(
    state: &Arc<Shared>,
    cfg: &Config,
    device: &str,
    token: Option<&str>,
) -> Result<(Admitted, Option<TenantConn>), ServerMsg> {
    if !cfg.require_enrollment {
        return Ok((Admitted::Open, None));
    }
    let store = &state.store;
    let internal = || err("internal", "storage error");
    let tenant = match store.enrollment(device).map_err(|_| internal())? {
        Some(t) => {
            // Already enrolled: a token for the same tenant is harmless (even if expired); one for
            // another tenant is refused, since a device belongs to one tenant at a time.
            if let Some(tok) = token.and_then(|s| EnrollToken::decode(s).ok()) {
                if tok.tenant != t {
                    return Err(err(
                        "enroll_conflict",
                        "this device is already enrolled in another organisation on this relay",
                    ));
                }
            }
            t
        }
        None => match token {
            Some(tok) => redeem(state, cfg, device, tok)?,
            None if cfg.enroll_visitors => return Ok((Admitted::Visitor, None)),
            None => {
                return Err(err(
                    "not_enrolled",
                    "this relay only admits enrolled devices: ask your organisation for an enrollment token",
                ))
            }
        },
    };
    let row = store
        .tenant(&tenant)
        .map_err(|_| internal())?
        .ok_or_else(|| {
            err(
                "not_enrolled",
                "this device's organisation no longer exists here",
            )
        })?;
    if row.suspended {
        return Err(err(
            "tenant_suspended",
            "this organisation's use of the relay is suspended by the operator",
        ));
    }
    // Take a connection place.
    {
        let mut rt = state.tenancy.lock().unwrap();
        let live = rt
            .tenants
            .entry(tenant.clone())
            .or_insert_with(|| Live::new(&row.limits));
        if row.limits.connections > 0 && live.conns >= row.limits.connections {
            drop(rt);
            return Err(over_limit(state, &tenant, limit_name::CONNECTIONS));
        }
        live.conns += 1;
    }
    let guard = TenantConn {
        state: state.clone(),
        tenant: tenant.clone(),
    };
    Ok((Admitted::Member(tenant), Some(guard)))
}

/// Check a token and record the enrollment. Returns the tenant.
fn redeem(state: &Shared, cfg: &Config, device: &str, token: &str) -> Result<String, ServerMsg> {
    let invalid = || {
        err(
            "enroll_invalid",
            "the enrollment token is not valid for this device",
        )
    };
    let tok = EnrollToken::decode(token).map_err(|_| invalid())?;
    if tok.device_id != device {
        return Err(err(
            "enroll_invalid",
            "the enrollment token was issued for another device",
        ));
    }
    let row = state
        .store
        .tenant(&tok.tenant)
        .map_err(|_| err("internal", "storage error"))?
        .ok_or_else(invalid)?;
    tok.verify(&row.admin_pub).map_err(|_| invalid())?;
    let now = now_secs();
    if tok.not_after <= now {
        return Err(err(
            "enroll_expired",
            "the enrollment token has expired: ask for a new one",
        ));
    }
    if tok.not_after > now + cfg.enroll_max_token_secs + TOKEN_SKEW_SECS {
        return Err(err(
            "enroll_invalid",
            "the enrollment token is valid for longer than this relay accepts",
        ));
    }
    if row.suspended {
        return Err(err(
            "tenant_suspended",
            "this organisation's use of the relay is suspended by the operator",
        ));
    }
    match state
        .store
        .enroll(device, &tok.tenant, &tok.token_id, row.limits.devices)
        .map_err(|_| err("internal", "storage error"))?
    {
        Enrolled::Ok => {
            crate::metrics::Metrics::inc(&state.metrics.enrollments_redeemed);
            Ok(tok.tenant)
        }
        Enrolled::OtherTenant => Err(err(
            "enroll_conflict",
            "this device is already enrolled in another organisation on this relay",
        )),
        Enrolled::Revoked => Err(err(
            "enroll_revoked",
            "this device was removed from the organisation",
        )),
        Enrolled::OverDeviceLimit => {
            state
                .tenancy
                .lock()
                .unwrap()
                .tenants
                .entry(tok.tenant.clone())
                .or_insert_with(|| Live::new(&row.limits));
            Err(over_limit(state, &tok.tenant, limit_name::DEVICES))
        }
    }
}

/// Take `cost` from a tenant's shared request budget.
pub(crate) fn allow_request(state: &Shared, tenant: &str, cost: f64) -> bool {
    match state.tenancy.lock().unwrap().tenants.get_mut(tenant) {
        Some(l) => cost <= 0.0 || l.bucket.allow(cost),
        // Deleted while connected: the connection is being closed.
        None => false,
    }
}

/// What a visitor may ask for: only what reaches members, nothing that stores data for itself.
pub(crate) fn visitor_may(m: &ClientMsg) -> bool {
    matches!(
        m,
        ClientMsg::Send { .. }
            | ClientMsg::ClaimPrekey { .. }
            | ClientMsg::ClaimKeyPackage { .. }
            | ClientMsg::FetchDeviceList { .. }
            | ClientMsg::GetBlobChunk { .. }
            | ClientMsg::GroupAppend { .. }
            | ClientMsg::GroupRead { .. }
            | ClientMsg::GroupSubscribe { .. }
            | ClientMsg::GroupUnsubscribe { .. }
            | ClientMsg::Revoke { .. }
            | ClientMsg::Ping
    )
}

pub(crate) fn visitor_refusal() -> ServerMsg {
    err(
        "not_enrolled",
        "this device is a visitor on this relay: it may reach the relay's members, but not store anything of its own here",
    )
}

/// The queue cap for a recipient: the relay's, or the recipient tenant's if tighter. The `Some`
/// tenant is returned when the tenant's limit is the one that binds.
pub(crate) fn queue_cap(state: &Shared, cfg: &Config, to: &str) -> (usize, Option<String>) {
    if !cfg.require_enrollment {
        return (cfg.queue_cap, None);
    }
    let Ok(Some(t)) = state.store.enrollment(to) else {
        return (cfg.queue_cap, None);
    };
    match state.store.tenant(&t) {
        Ok(Some(row))
            if row.limits.queue_depth > 0 && (row.limits.queue_depth as usize) < cfg.queue_cap =>
        {
            (row.limits.queue_depth as usize, Some(t))
        }
        _ => (cfg.queue_cap, None),
    }
}

/// Whether `to` is enrolled here (visitors may only send to members).
pub(crate) fn is_member(state: &Shared, to: &str) -> bool {
    matches!(state.store.enrollment(to), Ok(Some(_)))
}

/// Close every live connection of these devices (suspend, delete, revoke). Dropping a device's
/// senders ends its serve loops, as eviction does.
pub(crate) fn close_devices(state: &Shared, devices: &[String]) {
    let mut st = state.state.lock().unwrap();
    for d in devices {
        st.online.remove(d);
    }
}

/// A tenant admin's revocation, handed in on any signed-in connection.
pub(crate) fn on_revoke(state: &Shared, revocation: &str) -> ServerMsg {
    let Ok(r) = Revocation::decode(revocation) else {
        return err("bad_request", "not a revocation");
    };
    let row = match state.store.tenant(&r.tenant) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return err(
                "bad_signature",
                "no such organisation, or not its admin key",
            )
        }
        Err(_) => return err("internal", "storage error"),
    };
    if r.verify(&row.admin_pub).is_err() {
        return err(
            "bad_signature",
            "no such organisation, or not its admin key",
        );
    }
    match state.store.revoke(&r.tenant, &r.device_id) {
        Ok(_) => {
            close_devices(state, std::slice::from_ref(&r.device_id));
            ServerMsg::Revoked {
                device_id: r.device_id,
            }
        }
        Err(_) => err("internal", "storage error"),
    }
}

// ---------------------------------------------------------------- operator interface

/// Answer one operator request (the body of `POST /admin`). Returns an HTTP status and a JSON body.
/// Runs on a blocking thread.
pub(crate) fn admin(state: &Shared, cfg: &Config, body: &[u8]) -> (&'static str, String) {
    let reply = |status: &'static str, v: serde_json::Value| (status, v.to_string());
    let refuse = |status: &'static str, why: &str| {
        reply(status, serde_json::json!({ "ok": false, "error": why }))
    };
    let Some(operator) = &cfg.operator_key else {
        return refuse("404 Not Found", "the operator interface is not enabled");
    };
    let Ok(req) = serde_json::from_slice::<AdminRequest>(body) else {
        return refuse("400 Bad Request", "not an operator request");
    };
    let Ok(AdminBody {
        op,
        issued_at,
        nonce,
    }) = req.open(operator)
    else {
        return refuse("401 Unauthorized", "not signed by the operator key");
    };
    let now = now_secs();
    if issued_at.abs_diff(now) > ADMIN_SKEW_SECS {
        return refuse(
            "401 Unauthorized",
            "the request is too old or from the future: check the clocks",
        );
    }
    {
        let mut rt = state.tenancy.lock().unwrap();
        let window = Duration::from_secs(2 * ADMIN_SKEW_SECS);
        rt.nonces.retain(|_, seen| seen.elapsed() < window);
        if rt.nonces.insert(nonce, Instant::now()).is_some() {
            return refuse("401 Unauthorized", "this request was already used");
        }
    }
    let store = &state.store;
    let ok = |extra: serde_json::Value| {
        reply("200 OK", serde_json::json!({ "ok": true, "result": extra }))
    };
    let internal = || refuse("500 Internal Server Error", "storage error");
    let no_such = || refuse("404 Not Found", "no such tenant");
    let valid_key = |k: &str| {
        k.len() == 64
            && k.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    match op {
        AdminOp::CreateTenant {
            tenant,
            admin_pub,
            limits,
        } => {
            if !spike_core::enroll::is_valid_tenant_id(&tenant) || !valid_key(&admin_pub) {
                return refuse("400 Bad Request", "invalid tenant id or admin key");
            }
            match store.create_tenant(&tenant, &admin_pub, &limits) {
                Ok(true) => ok(serde_json::json!({ "tenant": tenant })),
                Ok(false) => refuse("409 Conflict", "that tenant id is taken"),
                Err(_) => internal(),
            }
        }
        AdminOp::SetLimits { tenant, limits } => match store.set_tenant_limits(&tenant, &limits) {
            Ok(true) => {
                // The shared request budget follows the new rate at once; connection counts stay.
                if let Some(l) = state.tenancy.lock().unwrap().tenants.get_mut(&tenant) {
                    l.bucket = TokenBucket::new(limits.req_rate, limits.req_burst());
                }
                ok(serde_json::json!({ "tenant": tenant }))
            }
            Ok(false) => no_such(),
            Err(_) => internal(),
        },
        AdminOp::SetAdminKey { tenant, admin_pub } => {
            if !valid_key(&admin_pub) {
                return refuse("400 Bad Request", "invalid admin key");
            }
            match store.set_tenant_admin_key(&tenant, &admin_pub) {
                Ok(true) => ok(serde_json::json!({ "tenant": tenant })),
                Ok(false) => no_such(),
                Err(_) => internal(),
            }
        }
        AdminOp::Suspend { tenant } => match store.set_tenant_suspended(&tenant, true) {
            Ok(true) => {
                if let Ok(devices) = store.tenant_devices(&tenant) {
                    close_devices(state, &devices);
                }
                ok(serde_json::json!({ "tenant": tenant, "state": "suspended" }))
            }
            Ok(false) => no_such(),
            Err(_) => internal(),
        },
        AdminOp::Resume { tenant } => match store.set_tenant_suspended(&tenant, false) {
            Ok(true) => ok(serde_json::json!({ "tenant": tenant, "state": "active" })),
            Ok(false) => no_such(),
            Err(_) => internal(),
        },
        AdminOp::Delete { tenant } => match store.delete_tenant(&tenant) {
            Ok(Some(devices)) => {
                close_devices(state, &devices);
                state.tenancy.lock().unwrap().tenants.remove(&tenant);
                ok(serde_json::json!({ "tenant": tenant, "devices_removed": devices.len() }))
            }
            Ok(None) => no_such(),
            Err(_) => internal(),
        },
        AdminOp::Usage { tenant } => {
            let rows = match &tenant {
                Some(t) => match store.tenant(t) {
                    Ok(Some(r)) => vec![r],
                    Ok(None) => return no_such(),
                    Err(_) => return internal(),
                },
                None => match store.tenants() {
                    Ok(r) => r,
                    Err(_) => return internal(),
                },
            };
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let Ok(t) = store.tenant_totals(&row.id) else {
                    return internal();
                };
                let (connections, refused) = state
                    .tenancy
                    .lock()
                    .unwrap()
                    .tenants
                    .get(&row.id)
                    .map(|l| (l.conns, l.refused))
                    .unwrap_or_default();
                out.push(TenantUsage {
                    tenant: row.id,
                    state: if row.suspended { "suspended" } else { "active" }.into(),
                    limits: row.limits,
                    devices: t.devices,
                    connections,
                    queued_messages: t.queued_messages,
                    queued_bytes: t.queued_bytes,
                    attachment_bytes: t.attachment_bytes,
                    refused_over_limit: refused,
                });
            }
            ok(serde_json::to_value(out).unwrap_or_default())
        }
    }
}

/// Shared handle type used by `lib.rs`.
pub(crate) type Tenancy = Mutex<Runtime>;
