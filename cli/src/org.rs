//! Organisations, client side (ADR 0035).
//!
//! *Member*: join an organisation's relay with a token, keep its rules (verified against the admin key
//! pinned at join, version only goes up), carry them out, and leave.
//!
//! *Admin*: hold the organisation's admin key on this computer, issue tokens, and talk to the relay's
//! admin interface (devices, revoke, withdraw a token, rules, usage).
//!
//! Both are stored sealed in the home like every other secret.

use crate::client::Relay;
use crate::socket::{Connector, Socket, WsConnector, WsSocket};
use crate::state::Home;
use crate::transport::Target;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::owner::OwnerKey;
use spike_core::tenant::{
    admin_fingerprint, is_valid_tenant_id, sign_admin_auth, EnrolToken, Rules, SignedRules,
    TokenBody,
};
use spike_core::wire::{ClientMsg, ServerMsg, WireTenantDevice, WireTenantUsage};

const MEMBERSHIP_FILE: &str = "org-membership.enc";
const ADMIN_FILE: &str = "org-admin.enc";

fn now_secs() -> u64 {
    spike_core::clock::now_ms() / 1000
}

// ------------------------------------------------------------------------------------- member

/// This device's organisation. Present from a successful join until the person leaves.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Membership {
    pub tenant: String,
    pub org_name: String,
    pub relay: String,
    /// Pinned at join: rules are trusted only from this key.
    pub admin_pub: String,
    pub joined_ms: u64,
    /// The relay this device used before joining, offered again when it leaves.
    pub previous_relay: String,
    pub rules: Option<Rules>,
    /// The relay no longer lists this device in the organisation (the admin removed it).
    pub removed: bool,
}

impl Membership {
    pub fn rules_version(&self) -> u64 {
        self.rules.as_ref().map_or(0, |r| r.version)
    }
    fn admin_key(&self) -> Result<[u8; 32]> {
        hex::decode(&self.admin_pub)?
            .try_into()
            .map_err(|_| anyhow!("stored admin key is malformed"))
    }
}

pub fn membership(home: &Home) -> Result<Option<Membership>> {
    let m: Option<Membership> = home.load_json(MEMBERSHIP_FILE)?;
    Ok(m)
}

fn save_membership(home: &Home, m: Option<&Membership>) -> Result<()> {
    home.save_json(MEMBERSHIP_FILE, &m)
}

/// Read a token for the join sheet: signature checked, expiry checked. Nothing is sent anywhere.
pub fn inspect(token: &str) -> Result<TokenBody> {
    let body = EnrolToken::decode(token)
        .and_then(|t| t.open())
        .map_err(|e| anyhow!("this is not a valid organisation token ({e})"))?;
    if body.expired(now_secs()) {
        bail!("this organisation token has expired: ask your organisation for a new one");
    }
    if !(body.relay.starts_with("wss://") || body.relay.starts_with("ws://")) {
        bail!("the token names a relay address that is not a WebSocket address");
    }
    Ok(body)
}

/// Words for a refused join or sign-in, by the relay's error code.
pub fn explain(code: &str) -> &'static str {
    match code {
        "token_expired" => "This token has expired. Ask your organisation for a new one.",
        "token_used" => {
            "This token was already used by another device. Ask your organisation for a new one."
        }
        "token_withdrawn" => "Your organisation withdrew this token.",
        "token_invalid" => "This relay does not recognise the organisation in this token.",
        "tenant_suspended" => "This organisation is suspended on its relay.",
        "tenant_over_limit" => {
            "The organisation has no device places left. Ask its admin to make room."
        }
        "already_enrolled" => "This device is already in another organisation on that relay.",
        "not_enrolled" => "This relay only admits devices an organisation enrolled.",
        "device_revoked" => "Your organisation removed this device.",
        _ => "The relay refused.",
    }
}

/// Re-sign our device list so this device reads from `relay` (or the sender's relay for `None`),
/// and publish it on `publish_to`. Contacts then deliver to the new home relay (ADR 0009).
pub async fn publish_home_relay(
    home: &Home,
    relay: Option<String>,
    publish_to: &mut Relay,
) -> Result<()> {
    let me = home.load_identity()?;
    let owner = home.load_owner()?;
    if let Some(list) = home
        .load_device_list()?
        .with_relay(&owner, &me.device_id(), relay)?
    {
        home.save_device_list(&list)?;
        publish_to.publish_device_list(&list).await?;
    }
    Ok(())
}

/// Join: sign in to the organisation's relay with the token, take its rules, and make it this
/// device's home relay in the signed device list. `old` is the relay used until now; the new list is
/// published there too, so contacts who look there learn where to deliver.
pub async fn join(
    home: &Home,
    token: &str,
    new: Target,
    old: Option<Target>,
) -> Result<Membership> {
    let body = inspect(token)?;
    if membership(home)?.is_some_and(|m| !m.removed) {
        bail!("this device is already in an organisation: leave it first");
    }
    let me = home.load_identity()?;
    let mut relay = Relay::connect_enrol(new.clone(), &me, token)
        .await
        .map_err(|e| match e.downcast_ref::<crate::client::RelayRefused>() {
            Some(r) => anyhow!("{}", explain(&r.code)),
            None => e.context(format!("could not reach {}", new.url)),
        })?;
    let mut m = Membership {
        tenant: body.tenant.clone(),
        org_name: body.org_name.clone(),
        relay: new.url.clone(),
        admin_pub: body.admin_pub.clone(),
        joined_ms: spike_core::clock::now_ms(),
        previous_relay: old.as_ref().map(|t| t.url.clone()).unwrap_or_default(),
        rules: None,
        removed: false,
    };
    apply_fetched(&mut m, relay.fetch_rules().await?)?;
    crate::ops::publish_keys(home, new.clone()).await?;
    publish_home_relay(home, Some(new.url.clone()), &mut relay).await?;
    if let Some(old) = old.filter(|o| o.url != new.url) {
        // Best effort: the old relay may be unreachable, and contacts also find the list on the new one.
        if let Ok(mut r) = Relay::connect(old, &me).await {
            let _ = r.publish_device_list(&home.load_device_list()?).await;
        }
    }
    save_membership(home, Some(&m))?;
    Ok(m)
}

fn apply_fetched(m: &mut Membership, fetched: (Option<String>, Option<String>)) -> Result<bool> {
    let (tenant, doc) = fetched;
    if tenant.as_deref() != Some(m.tenant.as_str()) {
        let changed = !m.removed;
        m.removed = true;
        return Ok(changed);
    }
    m.removed = false;
    let Some(doc) = doc else { return Ok(false) };
    let signed = SignedRules::decode(&doc)?;
    match signed.accept(&m.admin_key()?, &m.tenant, m.rules_version())? {
        Some(r) => {
            m.rules = Some(r);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// What changed when rules were refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RulesUpdate {
    pub changed: bool,
    pub removed: bool,
    /// Messages deleted under "messages kept on this device".
    pub pruned: usize,
}

/// Refresh rules on an open connection to the organisation's relay and carry out the retention rule.
/// A relay offering older or forged rules is refused (the error says so) and the held rules stay.
pub async fn refresh(home: &Home, relay: &mut Relay) -> Result<RulesUpdate> {
    let Some(mut m) = membership(home)? else {
        return Ok(RulesUpdate::default());
    };
    let fetched = relay.fetch_rules().await?;
    let changed = apply_fetched(&mut m, fetched)?;
    save_membership(home, Some(&m))?;
    let pruned = enforce_retention(home, &m)?;
    Ok(RulesUpdate {
        changed,
        removed: m.removed,
        pruned,
    })
}

/// "Messages kept on this device": delete older messages in every conversation (owner decision 3).
pub fn enforce_retention(home: &Home, m: &Membership) -> Result<usize> {
    let Some(days) = m.rules.as_ref().and_then(|r| r.keep_messages_days) else {
        return Ok(0);
    };
    if m.removed {
        return Ok(0);
    }
    let cutoff = spike_core::clock::now_ms().saturating_sub(days as u64 * 86_400_000);
    home.messages()?.prune_older_than(cutoff)
}

/// Leave the organisation: tell its relay (best effort if it already removed us), forget the
/// membership, and point the device list at `next` (the relay chosen to use next).
pub async fn leave(home: &Home, org: Option<Target>, next: Target) -> Result<()> {
    let me = home.load_identity()?;
    if let Some(org) = org {
        if let Ok(mut r) = Relay::connect(org, &me).await {
            let _ = r.leave_tenant().await;
        }
    }
    let mut r = Relay::connect(next.clone(), &me)
        .await
        .with_context(|| format!("could not reach {}", next.url))?;
    crate::ops::publish_keys(home, next.clone()).await?;
    publish_home_relay(home, Some(next.url.clone()), &mut r).await?;
    save_membership(home, None)
}

/// The rules in force on this device, if any. A removed membership enforces nothing.
pub fn rules_in_force(home: &Home) -> Result<Option<Rules>> {
    Ok(membership(home)?
        .filter(|m| !m.removed)
        .and_then(|m| m.rules))
}

// -------------------------------------------------------------------------------------- admin

/// A token this console issued. The label never leaves this computer (ADR 0035).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IssuedToken {
    pub token_id: String,
    pub label: String,
    pub not_after: u64,
    pub issued_ms: u64,
    pub withdrawn: bool,
}

/// The organisation admin key and what the console remembers, sealed in this home.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AdminState {
    pub tenant: String,
    pub org_name: String,
    pub relay: String,
    admin_key: String,
    pub tokens: Vec<IssuedToken>,
    /// The last rules this console published.
    pub rules: Option<Rules>,
}

impl AdminState {
    pub fn key(&self) -> Result<OwnerKey> {
        Ok(OwnerKey::from_hex(&self.admin_key)?)
    }
    pub fn admin_pub(&self) -> Result<String> {
        Ok(hex::encode(self.key()?.public()))
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(admin_fingerprint(&self.key()?.public()))
    }
}

pub fn admin(home: &Home) -> Result<Option<AdminState>> {
    let a: Option<AdminState> = home.load_json(ADMIN_FILE)?;
    Ok(a)
}

pub fn save_admin(home: &Home, a: Option<&AdminState>) -> Result<()> {
    home.save_json(ADMIN_FILE, &a)
}

/// Make a new admin key for an organisation. The operator registers its public half on the relay
/// (`spike-relay tenant create <id> --admin-pub <hex>`). Refuses to overwrite an existing key.
pub fn create_admin(home: &Home, tenant: &str, org_name: &str, relay: &str) -> Result<AdminState> {
    if admin(home)?.is_some() {
        bail!("this computer already holds an organisation admin key");
    }
    if !is_valid_tenant_id(tenant) {
        bail!("the organisation id must be 1 to 32 of a-z, 0-9 and -");
    }
    let name = org_name.trim();
    if name.is_empty() || name.chars().count() > spike_core::tenant::MAX_ORG_NAME {
        bail!("the organisation name must be 1 to 64 characters");
    }
    let relay = relay.trim();
    if !(relay.starts_with("wss://") || relay.starts_with("ws://")) {
        bail!("the relay address must look like wss://relay.example");
    }
    let a = AdminState {
        tenant: tenant.into(),
        org_name: name.into(),
        relay: relay.into(),
        admin_key: OwnerKey::generate().to_hex(),
        tokens: vec![],
        rules: None,
    };
    save_admin(home, Some(&a))?;
    Ok(a)
}

/// Sign a single-use token (local only: the relay learns of it when a device uses it).
pub fn issue_token(home: &Home, label: &str, valid_secs: u64) -> Result<(String, IssuedToken)> {
    let mut a = admin(home)?.context("this computer holds no organisation admin key")?;
    let (t, body) = EnrolToken::issue(
        &a.key()?,
        &a.tenant,
        &a.relay,
        &a.org_name,
        valid_secs,
        now_secs(),
    )?;
    let issued = IssuedToken {
        token_id: body.token_id,
        label: label.trim().chars().take(64).collect(),
        not_after: body.not_after,
        issued_ms: spike_core::clock::now_ms(),
        withdrawn: false,
    };
    // Keep the list short: drop tokens that expired more than a week ago.
    let cut = now_secs().saturating_sub(7 * 86_400);
    a.tokens.retain(|t| t.not_after > cut);
    a.tokens.push(issued.clone());
    save_admin(home, Some(&a))?;
    Ok((t.encode(), issued))
}

/// The admin console's connection to the organisation's relay.
pub struct AdminRelay<S: Socket = WsSocket> {
    ws: S,
    pub tenant: String,
    pub name: String,
    pub state: String,
}

impl AdminRelay {
    pub async fn connect(target: Target, a: &AdminState) -> Result<Self> {
        let audience = target.audience();
        let mut ws = WsConnector.connect(&target).await?;
        let first = recv(&mut ws).await?;
        let ServerMsg::Challenge { nonce } = first else {
            bail!("relay did not send a challenge")
        };
        let sig = sign_admin_auth(&a.key()?, &a.tenant, &hex::decode(nonce)?, &audience);
        send(
            &mut ws,
            &ClientMsg::AdminAuth {
                tenant: a.tenant.clone(),
                sig: hex::encode(sig),
            },
        )
        .await?;
        match recv(&mut ws).await? {
            ServerMsg::AdminReady { tenant, name, state } => Ok(Self { ws, tenant, name, state }),
            ServerMsg::Error { code, .. } if code == "auth_failed" => bail!(
                "the relay does not know this admin key for {}: ask the relay operator to register it",
                a.tenant
            ),
            ServerMsg::Error { code, message } => bail!("relay refused: {code}: {message}"),
            _ => bail!("unexpected answer from the relay"),
        }
    }
}

async fn send<S: Socket>(ws: &mut S, m: &ClientMsg) -> Result<()> {
    ws.send(serde_json::to_string(m)?).await
}

async fn recv<S: Socket>(ws: &mut S) -> Result<ServerMsg> {
    let t = ws
        .recv()
        .await?
        .ok_or_else(|| anyhow!("relay closed the connection"))?;
    Ok(serde_json::from_str(&t)?)
}

impl<S: Socket> AdminRelay<S> {
    async fn ask(&mut self, m: ClientMsg) -> Result<ServerMsg> {
        send(&mut self.ws, &m).await?;
        match recv(&mut self.ws).await? {
            ServerMsg::Error { code, message } => bail!("{code}: {message}"),
            other => Ok(other),
        }
    }

    pub async fn devices(&mut self) -> Result<Vec<WireTenantDevice>> {
        match self.ask(ClientMsg::ListDevices).await? {
            ServerMsg::Devices { devices } => Ok(devices),
            _ => bail!("unexpected answer from the relay"),
        }
    }

    pub async fn revoke(&mut self, device: &str) -> Result<()> {
        match self
            .ask(ClientMsg::RevokeDevice {
                device: device.into(),
            })
            .await?
        {
            ServerMsg::DeviceRevoked { .. } => Ok(()),
            _ => bail!("unexpected answer from the relay"),
        }
    }

    pub async fn cancel_token(&mut self, token_id: &str, not_after: u64) -> Result<()> {
        match self
            .ask(ClientMsg::CancelToken {
                token_id: token_id.into(),
                not_after,
            })
            .await?
        {
            ServerMsg::TokenCancelled { .. } => Ok(()),
            _ => bail!("unexpected answer from the relay"),
        }
    }

    pub async fn publish_rules(&mut self, rules: &SignedRules) -> Result<u64> {
        match self
            .ask(ClientMsg::PublishRules {
                rules: rules.encode(),
            })
            .await?
        {
            ServerMsg::RulesStored { version } => Ok(version),
            _ => bail!("unexpected answer from the relay"),
        }
    }

    pub async fn usage(&mut self) -> Result<WireTenantUsage> {
        match self.ask(ClientMsg::TenantUsage).await? {
            ServerMsg::Usage { usage } => Ok(usage),
            _ => bail!("unexpected answer from the relay"),
        }
    }
}

/// Sign and publish new rules from the console. The version is one above what the console last
/// published (or what the relay says it holds, whichever is higher).
pub async fn publish_rules(home: &Home, relay: &mut AdminRelay, mut rules: Rules) -> Result<u64> {
    let mut a = admin(home)?.context("this computer holds no organisation admin key")?;
    let held = relay.usage().await?.rules_version;
    rules.v = 1;
    rules.tenant = a.tenant.clone();
    rules.version = held.max(a.rules.as_ref().map_or(0, |r| r.version)) + 1;
    rules.issued_at = now_secs();
    let signed = SignedRules::sign(&a.key()?, &rules)?;
    let v = relay.publish_rules(&signed).await?;
    a.rules = Some(rules);
    save_admin(home, Some(&a))?;
    Ok(v)
}

/// Withdraw an unused token, here and on the relay.
pub async fn withdraw_token(home: &Home, relay: &mut AdminRelay, token_id: &str) -> Result<()> {
    let mut a = admin(home)?.context("this computer holds no organisation admin key")?;
    let t = a
        .tokens
        .iter_mut()
        .find(|t| t.token_id == token_id)
        .context("no such token")?;
    relay.cancel_token(token_id, t.not_after).await?;
    t.withdrawn = true;
    save_admin(home, Some(&a))
}
