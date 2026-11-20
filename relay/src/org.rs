//! Organisations on the relay (ADR 0023, ADR 0035): the sign-in gate for members, the two member
//! requests (fetch rules, leave), and the admin console's connection.
//!
//! The admin console sees the tenant's device ids, the day each enrolled, a coarse activity bucket and
//! the rules version each fetched, plus tenant totals. Nothing here returns per-device traffic, times
//! or addresses, and nothing is logged.

use crate::store::tenants::ACTIVE;
use crate::store::Redeem;
use crate::{err, internal, send_json, Config, Metrics, Shared};
use futures_util::{Sink, Stream, StreamExt};
use spike_core::tenant::{is_valid_tenant_id, verify_admin_auth, EnrolToken, SignedRules};
use spike_core::wire::{ClientMsg, ServerMsg};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

fn now_secs() -> u64 {
    spike_core::clock::now_ms() / 1000
}

/// Run at sign-in, on a blocking thread, after the device's signature checked out. `Ok(true)` means
/// the device signs in as a visitor: an enrolled relay lets devices outside the organisation deliver
/// to its members (owner decision 1: members talk to anyone), but keeps nothing for them.
pub(crate) fn gate(
    state: &Shared,
    cfg: &Config,
    me: &str,
    enrol: Option<&str>,
) -> Result<bool, ServerMsg> {
    let store = &state.store;
    if let Some(token) = enrol {
        let body = EnrolToken::decode(token)
            .and_then(|t| t.open())
            .map_err(|_| err("token_invalid", "this organisation token is not valid"))?;
        if body.expired(now_secs()) {
            return Err(err(
                "token_expired",
                "this organisation token has expired: ask for a new one",
            ));
        }
        let r = store.redeem(me, &body).map_err(|_| internal())?;
        match r {
            Redeem::Enrolled | Redeem::AlreadyMember => {}
            Redeem::NoTenant | Redeem::WrongKey => {
                return Err(err(
                    "token_invalid",
                    "this relay does not know the organisation in this token",
                ))
            }
            Redeem::Suspended => {
                return Err(err(
                    "tenant_suspended",
                    "this organisation is suspended on this relay",
                ))
            }
            Redeem::Withdrawn => {
                return Err(err(
                    "token_withdrawn",
                    "the organisation withdrew this token",
                ))
            }
            Redeem::Used => {
                return Err(err(
                    "token_used",
                    "this token was already used by another device",
                ))
            }
            Redeem::Full => {
                return Err(err(
                    "tenant_over_limit",
                    "devices: the organisation has no device places left",
                ))
            }
            Redeem::OtherTenant => {
                return Err(err(
                    "already_enrolled",
                    "this device is already in another organisation on this relay",
                ))
            }
        }
    }
    match store.membership(me).map_err(|_| internal())? {
        Some((_, state)) if state != ACTIVE => Err(err(
            "tenant_suspended",
            "this organisation is suspended on this relay",
        )),
        Some(_) => {
            let _ = store.touch(me);
            Ok(false)
        }
        None => Ok(cfg.enrolled),
    }
}

pub(crate) fn on_fetch_rules(state: &Shared, me: &str) -> ServerMsg {
    match state.store.rules_for(me) {
        Ok(Some((tenant, rules))) => ServerMsg::Rules {
            tenant: Some(tenant),
            rules,
        },
        Ok(None) => ServerMsg::Rules {
            tenant: None,
            rules: None,
        },
        Err(_) => internal(),
    }
}

pub(crate) fn on_leave(state: &Shared, me: &str) -> ServerMsg {
    match state.store.leave(me) {
        Ok(_) => ServerMsg::LeftTenant,
        Err(_) => internal(),
    }
}

/// Tell a tenant's connected members that rules changed. They verify the document themselves.
fn push_rules(state: &Shared, tenant: &str, doc: &str) {
    let Ok(members) = state.store.tenant_members(tenant) else {
        return;
    };
    let st = state.state.lock().unwrap();
    for d in members {
        for (_, conn) in st.online.get(&d).into_iter().flatten() {
            let _ = conn.try_send(ServerMsg::RulesChanged {
                rules: doc.to_string(),
            });
        }
    }
}

fn admin_dispatch(state: &Shared, cfg: &Config, tenant: &str, msg: ClientMsg) -> ServerMsg {
    let store = &state.store;
    match msg {
        ClientMsg::Ping => ServerMsg::Pong,
        ClientMsg::ListDevices => match store.tenant_devices(tenant) {
            Ok(devices) => ServerMsg::Devices { devices },
            Err(_) => internal(),
        },
        ClientMsg::RevokeDevice { device } => match store.revoke(tenant, &device) {
            Ok(true) => {
                // End its live connections now; it is refused (enrolled mode) or a plain device from here on.
                state.state.lock().unwrap().online.remove(&device);
                ServerMsg::DeviceRevoked { device }
            }
            Ok(false) => err("not_found", "no such device in this organisation"),
            Err(_) => internal(),
        },
        ClientMsg::CancelToken {
            token_id,
            not_after,
        } => {
            if hex::decode(&token_id).map(|b| b.len()) != Ok(16) {
                return err("bad_request", "invalid token id");
            }
            match store.cancel_token(tenant, &token_id, not_after) {
                Ok(true) => ServerMsg::TokenCancelled { token_id },
                Ok(false) => err(
                    "token_used",
                    "a device already used this token: revoke the device instead",
                ),
                Err(_) => internal(),
            }
        }
        ClientMsg::PublishRules { rules } => {
            let Ok(Some(t)) = store.tenant(tenant) else {
                return internal();
            };
            let Ok(pk) = hex::decode(&t.admin_pub).map(<[u8; 32]>::try_from) else {
                return internal();
            };
            let Ok(pk) = pk else { return internal() };
            let opened = SignedRules::decode(&rules).and_then(|s| s.open(&pk, tenant));
            let Ok(r) = opened else {
                return err(
                    "bad_rules",
                    "rules are not signed by this organisation's admin key",
                );
            };
            match store.put_rules(tenant, r.version, &rules) {
                Ok(true) => {
                    push_rules(state, tenant, &rules);
                    ServerMsg::RulesStored { version: r.version }
                }
                Ok(false) => err(
                    "stale_rules",
                    &format!("rules version must be higher than {}", t.rules_version),
                ),
                Err(_) => internal(),
            }
        }
        ClientMsg::TenantUsage => match store.tenant_usage(tenant) {
            Ok(Some(mut u)) => {
                u.relay_uptime_secs = state.started.elapsed().as_secs();
                u.enrolled_mode = cfg.enrolled;
                ServerMsg::Usage { usage: u }
            }
            Ok(None) => err("not_found", "no such organisation"),
            Err(_) => internal(),
        },
        _ => err(
            "forbidden",
            "an admin connection may only make organisation requests",
        ),
    }
}

/// Serve an admin console connection: verify the admin key's answer, then admin requests only.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_admin<T, R, E>(
    mut tx: T,
    mut rx: R,
    state: Arc<Shared>,
    cfg: Arc<Config>,
    nonce: [u8; 32],
    audience: String,
    tenant: String,
    sig: String,
) -> Result<(), ()>
where
    T: Sink<Message> + Unpin,
    R: Stream<Item = Result<Message, E>> + Unpin,
{
    let found = {
        let (state, tenant) = (state.clone(), tenant.clone());
        tokio::task::spawn_blocking(move || {
            if is_valid_tenant_id(&tenant) {
                state.store.tenant(&tenant).ok().flatten()
            } else {
                None
            }
        })
        .await
        .map_err(|_| ())?
    };
    let admin_pub = found.as_ref().and_then(|t| {
        let pk: [u8; 32] = hex::decode(&t.admin_pub).ok()?.try_into().ok()?;
        let s: [u8; 64] = hex::decode(&sig).ok()?.try_into().ok()?;
        verify_admin_auth(&pk, &tenant, &nonce, &audience, &s).then_some(t.admin_pub.clone())
    });
    let (Some(t), Some(admin_pub)) = (found, admin_pub) else {
        let _ = send_json(&mut tx, &err("auth_failed", "signature rejected")).await;
        Metrics::inc(&state.metrics.auth_failed);
        state.metrics.error("auth_failed");
        return Err(());
    };
    send_json(
        &mut tx,
        &ServerMsg::AdminReady {
            tenant: t.id.clone(),
            name: t.name,
            state: t.state,
        },
    )
    .await?;
    let mut bucket = crate::TokenBucket::new(cfg.req_rate, cfg.req_burst);
    loop {
        let frame = tokio::time::timeout(cfg.idle_timeout, rx.next())
            .await
            .map_err(|_| ())?;
        let Some(Ok(frame)) = frame else {
            return Ok(());
        };
        let Message::Text(text) = frame else {
            if matches!(frame, Message::Close(_)) {
                return Ok(());
            }
            continue;
        };
        let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
            send_json(&mut tx, &err("bad_request", "unexpected message")).await?;
            continue;
        };
        if !matches!(msg, ClientMsg::Ping) && !bucket.allow(1.0) {
            send_json(
                &mut tx,
                &err("rate_limited", "too many requests: slow down"),
            )
            .await?;
            continue;
        }
        let (st, cf, ten, key) = (
            state.clone(),
            cfg.clone(),
            tenant.clone(),
            admin_pub.clone(),
        );
        let reply = tokio::task::spawn_blocking(move || {
            // A rotated admin key ends the old key's sessions at their next request.
            match st.store.tenant(&ten) {
                Ok(Some(t)) if t.admin_pub == key => Some(admin_dispatch(&st, &cf, &ten, msg)),
                _ => None,
            }
        })
        .await
        .map_err(|_| ())?;
        let Some(reply) = reply else {
            let _ = send_json(
                &mut tx,
                &err("auth_failed", "the organisation's admin key changed"),
            )
            .await;
            return Ok(());
        };
        if let ServerMsg::Error { code, .. } = &reply {
            state.metrics.error(code);
        }
        send_json(&mut tx, &reply).await?;
    }
}
