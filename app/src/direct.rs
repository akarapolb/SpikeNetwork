//! Direct connections on the local network, as the screens see them (ADR 0027): a switch, the
//! addresses to give another device, an address per contact, and a way to hear that something
//! arrived directly. The wire protocol and the listener are in `spike_cli::direct`.

use super::{tally, target_for, Session, SyncDto};
use anyhow::{bail, Result};
use serde::Serialize;
use spike_cli::direct as cli;
use spike_cli::Outcome;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct State {
    listener: Mutex<Option<cli::Listener>>,
    inbox: Arc<Inbox>,
}

/// What direct deliveries produced since the screen last asked.
#[derive(Default)]
struct Inbox {
    pending: Mutex<Option<SyncDto>>,
    notify: Arc<tokio::sync::Notify>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DirectContactDto {
    pub name: String,
    /// The address a person typed for them (empty if none).
    pub address: String,
    /// Where they last called us from.
    pub learned: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DirectDto {
    pub enabled: bool,
    /// The listener is running.
    pub listening: bool,
    pub port: u16,
    /// What to give another device: `ip:port`.
    pub addresses: Vec<String>,
    /// Messages this run sent directly, sent to the relay after a direct try failed, and received directly.
    pub sent_direct: u64,
    pub sent_fallback: u64,
    pub received: u64,
    pub contacts: Vec<DirectContactDto>,
}

impl Session {
    pub fn direct_status(&self) -> Result<DirectDto> {
        let st = cli::load(&self.home)?;
        let listening = self.direct.listener.lock().expect("direct lock").is_some();
        let (sent_direct, sent_fallback, received) = cli::counters();
        let mut contacts = vec![];
        for c in self.home.load_contacts()? {
            let id = c.identity()?;
            let learned = id
                .devices()
                .iter()
                .flat_map(|d| st.learned.get(&d.device_id()).cloned().unwrap_or_default())
                .collect();
            contacts.push(DirectContactDto {
                address: st
                    .typed
                    .get(&id.owner_hex())
                    .and_then(|v| v.first().cloned())
                    .unwrap_or_default(),
                learned,
                name: c.name,
            });
        }
        Ok(DirectDto {
            enabled: st.enabled,
            listening,
            port: st.port,
            addresses: if listening {
                cli::local_addresses(st.port)
            } else {
                vec![]
            },
            sent_direct,
            sent_fallback,
            received,
            contacts,
        })
    }

    /// Turn direct connections on and start listening.
    pub async fn direct_enable(&self) -> Result<DirectDto> {
        cli::set_enabled(&self.home, true)?;
        self.direct_resume().await?;
        self.direct_status()
    }

    /// Turn them off and stop listening.
    pub fn direct_disable(&self) -> Result<DirectDto> {
        cli::set_enabled(&self.home, false)?;
        *self.direct.listener.lock().expect("direct lock") = None;
        self.direct_status()
    }

    /// Start listening if the setting is on and nothing is listening yet. Call after unlocking.
    pub async fn direct_resume(&self) -> Result<()> {
        if !cli::enabled(&self.home) || self.direct.listener.lock().expect("direct lock").is_some()
        {
            return Ok(());
        }
        let inbox = self.direct.inbox.clone();
        let calls = self.calls.clone();
        let apps = self.apps.clone();
        let sink: cli::Sink = Arc::new(move |o: Outcome| {
            calls.on_outcome(&o);
            apps.on_outcome(&o);
            let mut pending = inbox.pending.lock().expect("inbox lock");
            let out = pending.get_or_insert_with(|| SyncDto {
                received: 0,
                agent_requests: 0,
                message_requests: 0,
                groups: 0,
                changed: 0,
                notes: vec![],
            });
            if tally(out, &o) {
                inbox.notify.notify_one();
            }
        });
        let (rs, home) = (self.relay.clone(), self.home.clone());
        let listener = cli::listen(
            self.home.clone(),
            self.net.clone(),
            Arc::new(move || target_for(&rs.lock().expect("relay lock"), &home)),
            sink,
        )
        .await?;
        *self.direct.listener.lock().expect("direct lock") = Some(listener);
        Ok(())
    }

    /// Set (or with an empty `address`, clear) the address to try for a contact.
    pub fn direct_set_address(&self, name: &str, address: &str) -> Result<()> {
        let contacts = self.home.load_contacts()?;
        let Some(c) = contacts.iter().find(|c| c.name == name) else {
            bail!("no contact '{name}'");
        };
        cli::set_typed_address(&self.home, &c.conversation_key()?, Some(address))
    }

    /// Poked when something arrives directly, so a listener can wait without holding the session.
    pub fn direct_signal(&self) -> Arc<tokio::sync::Notify> {
        self.direct.inbox.notify.clone()
    }

    /// Wait up to `ms` for something to arrive directly. `None` if nothing did. What arrived is
    /// already stored; the caller only needs to refresh what it shows.
    pub async fn direct_wait(&self, ms: u64) -> Option<SyncDto> {
        let take = || self.direct.inbox.pending.lock().expect("inbox lock").take();
        if let Some(x) = take() {
            return Some(x);
        }
        let _ = tokio::time::timeout(
            Duration::from_millis(ms),
            self.direct.inbox.notify.notified(),
        )
        .await;
        take()
    }
}
