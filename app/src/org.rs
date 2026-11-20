//! Organisations (ADR 0035) for the desktop and Android screens: joining an organisation's relay,
//! the rules shown to members and carried out here, leaving, and the organisation console for the
//! computer that holds the admin key.

use crate::{Session, DEFAULT_RELAY};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_cli::org::{self, AdminRelay, Membership};
use spike_cli::transport::Target;
use spike_core::tenant::{admin_fingerprint, AgentRule, BackupRule, Rules};
use spike_core::wire::WireTenantDevice;
use std::time::Duration;

const NET_TIMEOUT: Duration = Duration::from_secs(20);

/// The rules as the screens show and edit them. Strings, so both apps can use one shape.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct RulesDto {
    pub version: u64,
    pub lock_idle_minutes: Option<u32>,
    pub keep_messages_days: Option<u32>,
    /// `allowed`, `key_required`, `off`, or none.
    pub backups: Option<String>,
    /// `any`, `verified_only`, `off`, or none.
    pub agents: Option<String>,
}

fn word<T: Serialize>(v: &Option<T>) -> Option<String> {
    v.as_ref()
        .and_then(|x| serde_json::to_value(x).ok())
        .and_then(|v| v.as_str().map(String::from))
}

fn parse<T: for<'a> Deserialize<'a>>(v: &Option<String>, what: &str) -> Result<Option<T>> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => serde_json::from_value(serde_json::Value::String(s.into()))
            .map(Some)
            .with_context(|| format!("unknown {what} rule '{s}'")),
    }
}

impl From<&Rules> for RulesDto {
    fn from(r: &Rules) -> Self {
        Self {
            version: r.version,
            lock_idle_minutes: r.lock_idle_minutes,
            keep_messages_days: r.keep_messages_days,
            backups: word(&r.backups),
            agents: word(&r.agents),
        }
    }
}

impl RulesDto {
    fn to_rules(&self) -> Result<Rules> {
        Ok(Rules {
            lock_idle_minutes: self.lock_idle_minutes,
            keep_messages_days: self.keep_messages_days,
            backups: parse::<BackupRule>(&self.backups, "backup")?,
            agents: parse::<AgentRule>(&self.agents, "agent")?,
            ..Default::default()
        })
    }
}

/// What the join sheet shows, read from the signed token before anything is sent.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct OrgTokenDto {
    pub tenant: String,
    pub org_name: String,
    pub relay: String,
    /// Unix seconds.
    pub valid_until: u64,
    pub admin_fingerprint: String,
}

/// This device's organisation, for Settings and the rules screen.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct OrgDto {
    pub tenant: String,
    pub org_name: String,
    pub relay: String,
    pub admin_fingerprint: String,
    pub joined_ms: u64,
    pub previous_relay: String,
    /// The organisation removed this device; its rules no longer apply.
    pub removed: bool,
    pub rules: Option<RulesDto>,
}

fn org_dto(m: &Membership) -> OrgDto {
    let fp = hex::decode(&m.admin_pub)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .map(|k| admin_fingerprint(&k))
        .unwrap_or_default();
    OrgDto {
        tenant: m.tenant.clone(),
        org_name: m.org_name.clone(),
        relay: m.relay.clone(),
        admin_fingerprint: fp,
        joined_ms: m.joined_ms,
        previous_relay: m.previous_relay.clone(),
        removed: m.removed,
        rules: m.rules.as_ref().map(RulesDto::from),
    }
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct IssuedTokenDto {
    pub token_id: String,
    pub label: String,
    pub not_after: u64,
    pub issued_ms: u64,
    pub withdrawn: bool,
}

/// The console's own state: the key it holds and the tokens it issued (labels stay here).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AdminDto {
    pub tenant: String,
    pub org_name: String,
    pub relay: String,
    /// The public half, for the relay operator to register.
    pub admin_pub: String,
    pub fingerprint: String,
    pub tokens: Vec<IssuedTokenDto>,
    pub rules: Option<RulesDto>,
}

/// What the relay told the console. Totals and per-device buckets only (ADR 0035).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ConsoleDto {
    pub name: String,
    pub state: String,
    pub devices: Vec<WireTenantDevice>,
    pub device_count: u64,
    pub device_limit: u64,
    pub stored_bytes: u64,
    pub storage_limit: u64,
    pub rules_version: u64,
    pub relay_uptime_secs: u64,
    pub enrolled_mode: bool,
    /// Milliseconds the relay took to answer.
    pub rtt_ms: u64,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NewTokenDto {
    pub token: String,
    pub token_id: String,
    pub not_after: u64,
}

async fn timed<T>(what: &str, f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    match tokio::time::timeout(NET_TIMEOUT, f).await {
        Ok(r) => r,
        Err(_) => bail!("no answer from {what} within 20 seconds"),
    }
}

/// Take the app's network lock, giving up at `deadline`: a call stuck on a dead connection must not
/// leave the join screen waiting forever (the time limit on the join itself only starts once it has
/// the lock).
async fn lock_net(
    net: &tokio::sync::Mutex<()>,
    deadline: tokio::time::Instant,
) -> Result<tokio::sync::MutexGuard<'_, ()>> {
    tokio::time::timeout_at(deadline, net.lock())
        .await
        .map_err(|_| {
            anyhow!("the app is still busy with another connection: try again in a moment")
        })
}

impl Session {
    /// The same trust settings (extra certificate, plain ws) as the home relay, for another address.
    fn target_at(&self, url: &str) -> Target {
        let mut t = self.target();
        t.url = url.to_string();
        t
    }

    // ------------------------------------------------------------------------------- member

    pub fn org_inspect(&self, token: &str) -> Result<OrgTokenDto> {
        let b = org::inspect(token)?;
        let fp = admin_fingerprint(&b.admin_key()?);
        Ok(OrgTokenDto {
            tenant: b.tenant,
            org_name: b.org_name,
            relay: b.relay,
            valid_until: b.not_after,
            admin_fingerprint: fp,
        })
    }

    /// "Use this relay": enrol with the token, take the rules, make the organisation's relay this
    /// device's home relay.
    pub async fn org_join(&self, token: &str) -> Result<OrgDto> {
        let body = org::inspect(token)?;
        // One deadline for both the wait for the network and the join itself.
        let deadline = tokio::time::Instant::now() + NET_TIMEOUT;
        let _net = lock_net(&self.net, deadline).await?;
        let new = self.target_at(&body.relay);
        let old = self.target();
        let m =
            match tokio::time::timeout_at(deadline, org::join(&self.home, token, new, Some(old)))
                .await
            {
                Ok(r) => r?,
                Err(_) => bail!(
                    "no answer from {} within {} seconds: check the connection and try again",
                    body.relay,
                    NET_TIMEOUT.as_secs()
                ),
            };
        self.set_relay(&m.relay)?;
        Ok(org_dto(&m))
    }

    pub fn org_status(&self) -> Result<Option<OrgDto>> {
        Ok(org::membership(&self.home)?.as_ref().map(org_dto))
    }

    /// Fetch the organisation's rules (called with each sync). Returns a note for the screen when
    /// something the person should know changed.
    pub(crate) async fn org_refresh(&self) -> Option<String> {
        let m = org::membership(&self.home).ok().flatten()?;
        if m.removed {
            return None;
        }
        let me = self.home.load_identity().ok()?;
        let target = self.target_at(&m.relay);
        let r = timed(&m.relay, async {
            let mut relay = spike_cli::client::Relay::connect(target, &me).await?;
            org::refresh(&self.home, &mut relay).await
        })
        .await;
        match r {
            Ok(u) if u.removed => Some(format!(
                "{} removed this device from its relay. Choose another relay in Settings.",
                m.org_name
            )),
            Ok(u) if u.changed => {
                Some(format!("{} changed the rules for this device.", m.org_name))
            }
            Ok(_) => None,
            // A relay that offers forged or older rules is refused, and the held rules stay in force.
            Err(e)
                if format!("{e:#}").contains("rollback")
                    || format!("{e:#}").contains("signature") =>
            {
                Some(format!(
                    "Refused rules from the relay that were not newer or not signed by {}.",
                    m.org_name
                ))
            }
            Err(_) => None,
        }
    }

    /// Leave the organisation and move to `next` (default: the relay used before joining).
    pub async fn org_leave(&self, next: Option<String>) -> Result<()> {
        let m = org::membership(&self.home)?.context("this device is not in an organisation")?;
        let next = next
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(m.previous_relay.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| DEFAULT_RELAY.to_string());
        if !(next.starts_with("ws://") || next.starts_with("wss://")) {
            bail!("the relay address must look like wss://relay.example");
        }
        let _net = self.net.lock().await;
        let org_target = (!m.removed).then(|| self.target_at(&m.relay));
        timed(
            &next,
            org::leave(&self.home, org_target, self.target_at(&next)),
        )
        .await?;
        self.set_relay(&next)
    }

    /// The rules this device must follow now (none outside an organisation or once removed).
    pub fn org_rules(&self) -> Result<Option<Rules>> {
        org::rules_in_force(&self.home)
    }

    pub(crate) fn org_check_backup(&self) -> Result<()> {
        if let Some(r) = self.org_rules()? {
            if r.backups == Some(BackupRule::Off) {
                bail!("your organisation does not allow backups on this device");
            }
        }
        Ok(())
    }

    pub(crate) fn org_check_agent(&self, contact_verified: bool) -> Result<()> {
        match self.org_rules()?.and_then(|r| r.agents) {
            Some(AgentRule::Off) => bail!("your organisation does not allow agents on this device"),
            Some(AgentRule::VerifiedOnly) if !contact_verified => {
                bail!("your organisation allows only verified agents: compare this agent's safety code first")
            }
            _ => Ok(()),
        }
    }

    // -------------------------------------------------------------------------------- admin

    fn admin_dto(a: &org::AdminState) -> Result<AdminDto> {
        Ok(AdminDto {
            tenant: a.tenant.clone(),
            org_name: a.org_name.clone(),
            relay: a.relay.clone(),
            admin_pub: a.admin_pub()?,
            fingerprint: a.fingerprint()?,
            tokens: a
                .tokens
                .iter()
                .rev()
                .map(|t| IssuedTokenDto {
                    token_id: t.token_id.clone(),
                    label: t.label.clone(),
                    not_after: t.not_after,
                    issued_ms: t.issued_ms,
                    withdrawn: t.withdrawn,
                })
                .collect(),
            rules: a.rules.as_ref().map(RulesDto::from),
        })
    }

    pub fn org_admin(&self) -> Result<Option<AdminDto>> {
        org::admin(&self.home)?
            .as_ref()
            .map(Self::admin_dto)
            .transpose()
    }

    pub fn org_admin_create(&self, tenant: &str, name: &str, relay: &str) -> Result<AdminDto> {
        Self::admin_dto(&org::create_admin(&self.home, tenant.trim(), name, relay)?)
    }

    async fn admin_conn(&self) -> Result<(AdminRelay, org::AdminState)> {
        let a = org::admin(&self.home)?.context("this computer holds no organisation admin key")?;
        let r = timed(&a.relay, AdminRelay::connect(self.target_at(&a.relay), &a)).await?;
        Ok((r, a))
    }

    pub async fn org_console(&self) -> Result<ConsoleDto> {
        let t = std::time::Instant::now();
        let (mut r, _) = self.admin_conn().await?;
        let u = r.usage().await?;
        let rtt_ms = t.elapsed().as_millis() as u64;
        let devices = r.devices().await?;
        Ok(ConsoleDto {
            name: r.name.clone(),
            state: u.state,
            devices,
            device_count: u.devices,
            device_limit: u.device_limit,
            stored_bytes: u.stored_bytes,
            storage_limit: u.storage_limit,
            rules_version: u.rules_version,
            relay_uptime_secs: u.relay_uptime_secs,
            enrolled_mode: u.enrolled_mode,
            rtt_ms,
        })
    }

    pub fn org_issue_token(&self, label: &str, hours: u64) -> Result<NewTokenDto> {
        let (token, t) = org::issue_token(&self.home, label, hours.clamp(1, 168) * 3600)?;
        Ok(NewTokenDto {
            token,
            token_id: t.token_id,
            not_after: t.not_after,
        })
    }

    pub async fn org_withdraw_token(&self, token_id: &str) -> Result<()> {
        let (mut r, _) = self.admin_conn().await?;
        org::withdraw_token(&self.home, &mut r, token_id).await
    }

    pub async fn org_revoke(&self, device: &str) -> Result<()> {
        let (mut r, _) = self.admin_conn().await?;
        r.revoke(device).await
    }

    pub async fn org_publish_rules(&self, rules: RulesDto) -> Result<u64> {
        let rules = rules.to_rules()?;
        let (mut r, _) = self.admin_conn().await?;
        org::publish_rules(&self.home, &mut r, rules).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_words_round_trip() {
        let d = RulesDto {
            version: 0,
            lock_idle_minutes: Some(5),
            keep_messages_days: None,
            backups: Some("key_required".into()),
            agents: Some("verified_only".into()),
        };
        let r = d.to_rules().unwrap();
        assert_eq!(r.backups, Some(BackupRule::KeyRequired));
        assert_eq!(RulesDto::from(&r).agents.as_deref(), Some("verified_only"));
        let bad = RulesDto {
            agents: Some("some".into()),
            ..Default::default()
        };
        assert!(bad.to_rules().is_err());
        let blank = RulesDto {
            backups: Some(" ".into()),
            ..Default::default()
        };
        assert_eq!(blank.to_rules().unwrap().backups, None);
    }

    #[tokio::test]
    async fn waiting_for_the_network_gives_up() {
        let net = tokio::sync::Mutex::new(());
        let held = net.lock().await;
        let soon = tokio::time::Instant::now() + Duration::from_millis(50);
        let e = lock_net(&net, soon)
            .await
            .expect_err("a held lock times out");
        assert!(e.to_string().contains("busy"));
        drop(held);
        let later = tokio::time::Instant::now() + Duration::from_millis(50);
        assert!(lock_net(&net, later).await.is_ok());
    }
}
