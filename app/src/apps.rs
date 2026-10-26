//! App channels as the screens and other programs see them (ADR 0030). The engine (`spike_cli::apps`)
//! keeps stored events in a sealed inbox; this module adds what only a running app can: live events
//! held in memory for a moment, and a wait that wakes as soon as something arrives.

use super::Session;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use spike_cli::apps::{self, AppEvent, To};
use spike_cli::{groups, Outcome};
use spike_core::app::{Mode, LIVE_MAX_AGE_MS};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Live events kept per app for a program to pick up.
const MAX_LIVE: usize = 200;
/// Most events handed over in one go.
pub const MAX_BATCH: usize = 200;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Shared by the receive paths (which feed it) and whoever waits for app events.
#[derive(Default)]
pub(crate) struct Hub {
    /// Live events by app id, with the time they arrived (ms).
    live: Mutex<HashMap<String, VecDeque<(u64, AppEvent)>>>,
    pub(crate) notify: tokio::sync::Notify,
}

impl Hub {
    pub(crate) fn on_outcome(&self, o: &Outcome) {
        let Outcome::App(e) = o else {
            return;
        };
        if e.seq == 0 {
            let mut live = self.live.lock().expect("apps lock");
            let q = live.entry(e.app.clone()).or_default();
            q.push_back((now_ms(), e.clone()));
            while q.len() > MAX_LIVE {
                q.pop_front();
            }
        }
        self.notify.notify_waiters();
    }

    fn take_live(&self, app: &str) -> Vec<AppEvent> {
        let now = now_ms();
        let mut live = self.live.lock().expect("apps lock");
        live.remove(app)
            .unwrap_or_default()
            .into_iter()
            .filter(|(at, _)| now.saturating_sub(*at) <= LIVE_MAX_AGE_MS)
            .map(|(_, e)| e)
            .collect()
    }

    pub(crate) fn forget(&self, app: &str) {
        self.live.lock().expect("apps lock").remove(app);
    }
}

/// An application set up on this device, for Settings.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AppDto {
    pub app: String,
    pub name: String,
    pub added: u64,
    pub last: u64,
    pub received: u64,
    pub sent: u64,
    /// A program on this computer holds a token for it (desktop local API).
    pub connected: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DroppedAppDto {
    pub app: String,
    pub count: u64,
    pub last: u64,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AppsDto {
    pub apps: Vec<AppDto>,
    /// Apps whose data reached this device but are not set up here.
    pub dropped: Vec<DroppedAppDto>,
    /// The desktop's local API is turned on.
    pub api: bool,
}

/// Where a program sends: `{"contact": name}`, `{"group": id}`, or `{"group": id, "member": owner}`.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ToDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

impl ToDto {
    pub fn to(&self) -> Result<To> {
        Ok(match (&self.contact, &self.group, &self.member) {
            (Some(c), None, None) => To::Contact(c.clone()),
            (None, Some(g), None) => To::Group(g.clone()),
            (None, Some(g), Some(m)) => To::Member {
                group: g.clone(),
                owner: m.clone(),
            },
            _ => bail!("say where: a contact, a group, or a group and one of its members"),
        })
    }
}

/// Who a program can send to: contacts by name and small groups by id.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TargetsDto {
    pub contacts: Vec<TargetContact>,
    pub groups: Vec<TargetGroup>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TargetContact {
    pub name: String,
    pub owner: String,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TargetGroup {
    pub id: String,
    pub name: String,
}

impl Session {
    pub fn apps(&self) -> Result<AppsDto> {
        Ok(AppsDto {
            apps: apps::registered(&self.home)?
                .into_iter()
                .map(|a| AppDto {
                    connected: a.token.is_some(),
                    app: a.app,
                    name: a.name,
                    added: a.added,
                    last: a.last,
                    received: a.received,
                    sent: a.sent,
                })
                .collect(),
            dropped: apps::dropped(&self.home)?
                .into_iter()
                .map(|d| DroppedAppDto {
                    app: d.app,
                    count: d.count,
                    last: d.last,
                })
                .collect(),
            api: apps::api_enabled(&self.home)?,
        })
    }

    /// Accept data for `app` on this device (a program linking the library does this itself).
    pub fn app_register(&self, app: &str, name: &str) -> Result<()> {
        apps::register(&self.home, app, name)
    }

    /// Stop accepting data for `app` here; a program's token for it stops working.
    pub fn app_remove(&self, app: &str) -> Result<()> {
        if !apps::remove(&self.home, app)? {
            bail!("{app} is not set up on this device");
        }
        self.apps.forget(app);
        Ok(())
    }

    pub fn app_set_api(&self, on: bool) -> Result<()> {
        apps::set_api(&self.home, on)
    }

    pub fn app_api(&self) -> Result<bool> {
        apps::api_enabled(&self.home)
    }

    pub(crate) fn app_set_token(&self, app: &str, token_hash: &str) -> Result<()> {
        apps::set_token(&self.home, app, token_hash)
    }

    pub(crate) fn app_for_token(&self, token_hash: &str) -> Result<Option<String>> {
        apps::app_for_token(&self.home, token_hash)
    }

    pub fn app_targets(&self) -> Result<TargetsDto> {
        let contacts = self
            .home
            .load_contacts()?
            .into_iter()
            .filter_map(|c| {
                Some(TargetContact {
                    owner: c.conversation_key().ok()?,
                    name: c.name,
                })
            })
            .collect();
        let groups = groups::list(&self.home)?
            .into_iter()
            .filter(|r| r.status == groups::Status::Joined)
            .filter_map(|r| {
                Some(TargetGroup {
                    name: r.current().ok()?.name,
                    id: r.id().to_string(),
                })
            })
            .chain(
                spike_cli::large::list(&self.home)?
                    .into_iter()
                    .filter(|r| r.status == spike_cli::large::Status::Active)
                    .map(|r| TargetGroup {
                        name: r.name,
                        id: r.id,
                    }),
            )
            .collect();
        Ok(TargetsDto { contacts, groups })
    }

    /// Send an application's bytes; returns the envelope id.
    pub async fn app_send(
        &self,
        to: &To,
        app: &str,
        channel: &str,
        mode: Mode,
        bytes: &[u8],
    ) -> Result<String> {
        let _net = self.net.lock().await;
        Ok(
            apps::send(&self.home, self.target(), to, app, channel, mode, bytes)
                .await?
                .id,
        )
    }

    pub async fn app_request_state(&self, to: &To, app: &str, channel: &str) -> Result<String> {
        let _net = self.net.lock().await;
        Ok(
            apps::request_state(&self.home, self.target(), to, app, channel)
                .await?
                .id,
        )
    }

    pub async fn app_send_state(
        &self,
        to: &To,
        app: &str,
        channel: &str,
        snapshot: &[u8],
    ) -> Result<String> {
        let _net = self.net.lock().await;
        Ok(
            apps::send_state(&self.home, self.target(), to, app, channel, snapshot)
                .await?
                .id,
        )
    }

    /// The bytes of state event `seq` (read once).
    pub async fn app_snapshot(&self, app: &str, seq: u64) -> Result<Vec<u8>> {
        let _net = self.net.lock().await;
        apps::snapshot(&self.home, self.target(), app, seq).await
    }

    /// Events for `app` after `after` (which acknowledges everything up to it), waiting up to `wait`
    /// for the first one. Stored events come in order; live ones (seq 0) follow and are handed over
    /// once. This does not fetch from the relay: the app's listener or a sync does that.
    pub async fn app_events(&self, app: &str, after: u64, wait: Duration) -> Result<Vec<AppEvent>> {
        if !apps::is_registered(&self.home, app)? {
            bail!("{app} is not set up on this device");
        }
        let until = Instant::now() + wait;
        loop {
            let notified = self.apps.notify.notified();
            let mut out = apps::take(&self.home, app, after, MAX_BATCH)?;
            out.extend(self.apps.take_live(app));
            let left = until.saturating_duration_since(Instant::now());
            if !out.is_empty() || left.is_zero() {
                return Ok(out);
            }
            // Events released later (a group message that waited for its member list) do not
            // notify, so look again at least every second.
            let _ = tokio::time::timeout(left.min(Duration::from_secs(1)), notified).await;
        }
    }
}
