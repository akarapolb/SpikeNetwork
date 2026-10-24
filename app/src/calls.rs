//! Calls as the screens see them (ADR 0028). The Rust side carries signalling and decides what to
//! do (`spike_core::call::Machine`); the screen owns the media (WebRTC) and tells this module what
//! its peer connection produced (offer, answer, candidates). The screen then waits on
//! [`Session::call_wait`] for the things it has to act on: ring, connect, remote candidates, end.

use super::Session;
use anyhow::{bail, Result};
use rand::{rngs::OsRng, RngCore};
use serde::Serialize;
use spike_cli::messages::{Direction, StoredMessage};
use spike_cli::{ops, Outcome};
use spike_core::call::{Action, CallResult, Input, Machine, Origin};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Shared between the sync paths (which receive signals) and the session (which sends and answers).
#[derive(Default)]
pub(crate) struct Hub {
    machine: Mutex<Machine>,
    /// Actions waiting for the session to carry out.
    queue: Mutex<VecDeque<Action>>,
    /// Who each call is with (conversation key, name), by call id.
    peers: Mutex<HashMap<String, (String, String)>>,
    /// Which calls are video, by call id.
    video: Mutex<HashMap<String, bool>>,
    pub(crate) notify: tokio::sync::Notify,
}

impl Hub {
    /// Feed a received call signal in. Anything else is not for us.
    pub(crate) fn on_outcome(&self, o: &Outcome) {
        let Outcome::Call { name, peer, device, signal, ts } = o else { return };
        if let (Some(peer), Some(name)) = (peer, name) {
            let mut p = self.peers.lock().expect("calls lock");
            if p.len() > 64 {
                p.clear();
            }
            p.entry(signal.call().to_string())
                .or_insert_with(|| (peer.clone(), name.clone()));
        }
        let from = Origin { device: device.clone(), peer: peer.clone() };
        self.step(Input::Recv { from, signal: signal.clone(), ts: *ts });
    }

    fn step(&self, input: Input) {
        let actions = self.machine.lock().expect("calls lock").step(now_ms(), input);
        if !actions.is_empty() {
            self.queue.lock().expect("calls lock").extend(actions);
            self.notify.notify_one();
        }
    }
}

/// What the screen has to act on.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallEventDto {
    /// Ring: show the incoming-call screen. `glare` means accept at once (we were calling each other).
    Ring { call: String, name: String, video: bool, sdp: String, glare: bool },
    StopRinging { call: String },
    /// The other side answered: set this as the remote description.
    Connect { call: String, sdp: String },
    RemoteIce { call: String, candidates: Vec<String> },
    /// The call is over: tear down media. `result` is one of completed, missed, declined, no_answer,
    /// busy, answered_elsewhere, failed.
    Ended {
        call: String,
        name: String,
        incoming: bool,
        result: &'static str,
        seconds: u64,
    },
}

fn result_name(r: CallResult) -> &'static str {
    match r {
        CallResult::Completed => "completed",
        CallResult::Missed => "missed",
        CallResult::Declined => "declined",
        CallResult::NoAnswer => "no_answer",
        CallResult::Busy => "busy",
        CallResult::AnsweredElsewhere => "answered_elsewhere",
        CallResult::Failed => "failed",
    }
}

impl Session {
    fn peer_of(&self, call: &str) -> Option<(String, String)> {
        self.calls.peers.lock().expect("calls lock").get(call).cloned()
    }

    /// Place a call to a contact. `sdp` is the offer the screen made; returns the call id.
    pub async fn call_start(&self, name: &str, video: bool, sdp: &str) -> Result<String> {
        let contacts = self.home.load_contacts()?;
        let Some(c) = contacts.iter().find(|c| c.name == name) else {
            bail!("no contact '{name}'");
        };
        let key = c.conversation_key()?;
        let devices = c.identity()?.device_relays().len();
        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let call = hex::encode(id);
        if !self.calls.machine.lock().expect("calls lock").is_idle() {
            bail!("already in a call");
        }
        self.calls
            .peers
            .lock()
            .expect("calls lock")
            .insert(call.clone(), (key.clone(), name.to_string()));
        self.calls.video.lock().expect("calls lock").insert(call.clone(), video);
        self.calls.step(Input::Dial {
            call: call.clone(),
            peer: key,
            peer_devices: devices,
            video,
            sdp: sdp.to_string(),
        });
        self.call_flush().await;
        Ok(call)
    }

    pub async fn call_accept(&self, sdp: &str) -> Result<()> {
        self.calls.step(Input::Accept { sdp: sdp.to_string() });
        self.call_flush().await;
        Ok(())
    }

    pub async fn call_decline(&self, all: bool) -> Result<()> {
        self.calls.step(Input::Decline { all });
        self.call_flush().await;
        Ok(())
    }

    pub async fn call_hangup(&self) -> Result<()> {
        self.calls.step(Input::Hangup);
        self.call_flush().await;
        Ok(())
    }

    /// The screen's media failed (ICE failed, certificate mismatch): end the call.
    pub async fn call_fail(&self) -> Result<()> {
        self.calls.step(Input::Fail);
        self.call_flush().await;
        Ok(())
    }

    /// Candidates the screen gathered.
    pub async fn call_local_ice(&self, candidates: Vec<String>) -> Result<()> {
        self.calls.step(Input::LocalIce { candidates });
        self.call_flush().await;
        Ok(())
    }

    /// Send what is queued to send. Failures are not errors for the caller: a lost `End` or `Ice` is
    /// covered by timeouts, and the call screen already shows what the person needs.
    async fn call_flush(&self) {
        // Sending can itself produce nothing new, so a snapshot of the sends is enough; UI actions
        // stay queued for `call_wait`.
        let sends: Vec<Action> = {
            let mut q = self.calls.queue.lock().expect("calls lock");
            let (s, rest): (Vec<_>, Vec<_>) = q.drain(..).partition(|a| matches!(a, Action::Send { .. }));
            q.extend(rest);
            s
        };
        if !sends.is_empty() {
            self.calls.notify.notify_one();
        }
        for a in sends {
            let Action::Send { to, signal } = a else { continue };
            let Some((_, name)) = self.peer_of(signal.call()) else { continue };
            // A relay lets one connection per device be current, so a send can lose a race with the
            // listener's connection: try again a couple of times before giving up on this signal.
            for attempt in 0..3 {
                let net = self.net.lock().await;
                let sent = ops::send_call(&self.home, self.target(), &name, &signal, &to).await;
                drop(net);
                if sent.is_ok() {
                    break;
                }
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            }
        }
    }

    /// Wait up to `ms` for something the screen must act on. Also drives ring timeouts.
    pub async fn call_wait(&self, ms: u64) -> Vec<CallEventDto> {
        let end = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            self.calls.step(Input::Tick);
            self.call_flush().await;
            let actions: Vec<Action> = self.calls.queue.lock().expect("calls lock").drain(..).collect();
            let events: Vec<CallEventDto> = actions.into_iter().filter_map(|a| self.to_event(a)).collect();
            if !events.is_empty() {
                return events;
            }
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return vec![];
            }
            // Wake for a signal, or at least once a second so a ring timeout is noticed.
            let _ = tokio::time::timeout(
                left.min(Duration::from_secs(1)),
                self.calls.notify.notified(),
            )
            .await;
        }
    }

    /// One line in the conversation for a finished call, kept on this device only (ADR 0028).
    fn log_call(&self, call: &str, peer: &str, incoming: bool, result: CallResult, seconds: u64, video: bool) {
        let kind = if video { "video call" } else { "voice call" };
        let body = match result {
            CallResult::Completed => format!("{}, {}:{:02}", cap(kind), seconds / 60, seconds % 60),
            CallResult::Missed => format!("Missed {kind}"),
            CallResult::Declined => format!("Declined {kind}"),
            CallResult::NoAnswer => format!("{}, no answer", cap(kind)),
            CallResult::Busy => format!("{}, busy", cap(kind)),
            CallResult::Failed => format!("{} failed", cap(kind)),
            CallResult::AnsweredElsewhere => return,
        };
        let Ok(me) = self.home.load_identity() else { return };
        let Ok(lamport) = self.home.next_lamport() else { return };
        let Ok(store) = self.home.messages() else { return };
        let _ = store.insert(&StoredMessage {
            id: format!("call-{call}"),
            peer: peer.to_string(),
            direction: if incoming { Direction::In } else { Direction::Out },
            ts: now_ms(),
            body,
            attachment: None,
            lamport,
            stamp_device: me.device_id(),
            edited: false,
            deleted: false,
            changed_at: 0,
            author: None,
            shared_by: None,
        });
    }

    fn to_event(&self, a: Action) -> Option<CallEventDto> {
        let name_of = |call: &str| self.peer_of(call).map(|p| p.1).unwrap_or_default();
        Some(match a {
            Action::Send { .. } => return None,
            Action::Ring { call, peer, video, sdp, glare } => {
                let name = self
                    .peer_of(&call)
                    .map(|p| p.1)
                    .or_else(|| {
                        self.home.load_contacts().ok()?.iter().find_map(|c| {
                            (c.conversation_key().ok().as_deref() == Some(peer.as_str())).then(|| c.name.clone())
                        })
                    })
                    .unwrap_or_default();
                self.calls.video.lock().expect("calls lock").insert(call.clone(), video);
                CallEventDto::Ring { call, name, video, sdp, glare }
            }
            Action::StopRinging { call } => CallEventDto::StopRinging { call },
            Action::Connect { call, sdp } => CallEventDto::Connect { call, sdp },
            Action::RemoteIce { call, candidates } => CallEventDto::RemoteIce { call, candidates },
            Action::Ended { call, peer, incoming, result, answered_at } => {
                let seconds = answered_at.map_or(0, |t| now_ms().saturating_sub(t) / 1000);
                if let Some(peer) = &peer {
                    let video = self.calls.video.lock().expect("calls lock").remove(&call).unwrap_or(false);
                    self.log_call(&call, peer, incoming, result, seconds, video);
                }
                CallEventDto::Ended { name: name_of(&call), seconds, call, incoming, result: result_name(result) }
            }
        })
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}
