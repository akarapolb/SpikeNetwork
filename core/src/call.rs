//! Call setup (ADR 0028): the signals two people exchange, and the state machine that decides what
//! to do with them. No I/O and no media: the front ends own WebRTC; this module only says which
//! signal to send to whom, when to ring, and when a call is over. That makes every rule about
//! ringing across devices testable without a microphone.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

/// How long a call rings before the caller gives up, in milliseconds.
pub const RING_MS: u64 = 45_000;
/// An invite whose sender clock is further than this from ours is not rung for (replay, or held back by a relay).
pub const STALE_MS: u64 = 60_000;
/// Unanswered invites from one contact, per hour, before further ones are dropped silently.
pub const MAX_UNANSWERED_PER_HOUR: usize = 3;
pub const MAX_SDP: usize = 16 * 1024;
pub const MAX_CANDIDATES: usize = 16;
pub const MAX_CANDIDATE: usize = 512;
const SEEN_KEEP: usize = 256;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Hangup,
    Declined,
    Busy,
    Timeout,
    AnsweredElsewhere,
    Failed,
}

/// The body of a `Kind::Call` envelope.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Signal {
    Invite { call: String, video: bool, sdp: String },
    Answer { call: String, sdp: String },
    Ice { call: String, candidates: Vec<String> },
    End { call: String, reason: Reason },
    /// Mid-call, after a network change: the original caller's new offer with fresh connection details.
    Restart { call: String, sdp: String },
    /// The answer to a `Restart`.
    RestartAnswer { call: String, sdp: String },
    /// The person who did not place the call asks the caller to restart (it noticed its network change).
    RestartAsk { call: String },
}

fn is_hex32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

impl Signal {
    pub fn call(&self) -> &str {
        match self {
            Signal::Invite { call, .. }
            | Signal::Answer { call, .. }
            | Signal::Ice { call, .. }
            | Signal::End { call, .. }
            | Signal::Restart { call, .. }
            | Signal::RestartAnswer { call, .. }
            | Signal::RestartAsk { call } => call,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("signal serializes")
    }

    /// Parse and validate. Bad shapes, oversized SDP and too many candidates are refused outright.
    pub fn parse(json: &str) -> Result<Self> {
        let s: Signal = serde_json::from_str(json).map_err(|_| Error::Malformed)?;
        let sdp_ok = |sdp: &str| !sdp.is_empty() && sdp.len() <= MAX_SDP;
        let ok = is_hex32(s.call())
            && match &s {
                Signal::Invite { sdp, .. }
                | Signal::Answer { sdp, .. }
                | Signal::Restart { sdp, .. }
                | Signal::RestartAnswer { sdp, .. } => sdp_ok(sdp),
                Signal::Ice { candidates, .. } => {
                    !candidates.is_empty()
                        && candidates.len() <= MAX_CANDIDATES
                        && candidates.iter().all(|c| c.len() <= MAX_CANDIDATE)
                }
                Signal::End { .. } | Signal::RestartAsk { .. } => true,
            };
        if ok {
            Ok(s)
        } else {
            Err(Error::Malformed)
        }
    }
}

/// Who a signal is addressed to; the caller of the machine knows the device lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Every device of the other person.
    PeerAll,
    PeerDevice(String),
    /// Every other device of ours.
    OwnOthers,
}

/// Where a received signal came from. `peer` is the contact's conversation key; `None` is one of our own devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub device: String,
    pub peer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallResult {
    Completed,
    Missed,
    Declined,
    NoAnswer,
    Busy,
    AnsweredElsewhere,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The person placed a call. `sdp` is the front end's offer; `peer_devices` how many devices will ring.
    Dial { call: String, peer: String, peer_devices: usize, video: bool, sdp: String },
    Recv { from: Origin, signal: Signal, ts: u64 },
    /// Answer the ringing call with the front end's answer SDP.
    Accept { sdp: String },
    /// Decline the ringing call; `all` also stops our other devices ringing.
    Decline { all: bool },
    Hangup,
    /// Our side of the media failed (certificate mismatch, ICE failure).
    Fail,
    /// Candidates gathered locally.
    LocalIce { candidates: Vec<String> },
    /// The caller's new offer after a network change (only the person who placed the call sends this).
    SendRestart { sdp: String },
    SendRestartAnswer { sdp: String },
    /// Ask the caller to restart, because our network changed.
    AskRestart,
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send { to: Target, signal: Signal },
    /// Start ringing for this incoming call. `glare` means we were calling them too and theirs won:
    /// the front end should accept it at once.
    Ring { call: String, peer: String, video: bool, sdp: String, glare: bool },
    StopRinging { call: String },
    /// The callee answered: apply this answer SDP.
    Connect { call: String, sdp: String },
    RemoteIce { call: String, candidates: Vec<String> },
    /// The caller restarts the connection: answer with `Input::SendRestartAnswer`.
    Restart { call: String, sdp: String },
    /// The answer to our restart offer: apply it.
    RestartAnswer { call: String, sdp: String },
    /// The other side asks us to restart (we placed the call).
    RestartAsk { call: String },
    /// The call is over; write the call log and tear down media.
    Ended {
        call: String,
        peer: Option<String>,
        incoming: bool,
        result: CallResult,
        /// When it was answered (ms), if it was; the log computes duration from it.
        answered_at: Option<u64>,
    },
}

#[derive(Debug, Clone)]
enum State {
    Idle,
    Outgoing {
        call: String,
        peer: String,
        peer_devices: usize,
        started: u64,
        refused: HashSet<String>,
        busy: bool,
        /// Local candidates held until we know which device answered.
        pending_ice: Vec<String>,
    },
    Incoming {
        call: String,
        peer: String,
        device: String,
        started: u64,
    },
    Active {
        call: String,
        peer: String,
        device: String,
        incoming: bool,
        answered_at: u64,
    },
}

pub struct Machine {
    state: State,
    seen: VecDeque<String>,
    /// Calls a contact has already ended (hung up, or gave up ringing) that we have not rung for: if the invite arrives
    /// after its own end (the relay and the network do not promise order), it must not ring a call nobody is making.
    cancelled: VecDeque<(String, String)>,
    /// (contact, time) of invites that rang and were not answered.
    unanswered: Vec<(String, u64)>,
    now: u64,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Self { state: State::Idle, seen: VecDeque::new(), cancelled: VecDeque::new(), unanswered: Vec::new(), now: 0 }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// The id of the call in progress, if any.
    pub fn current(&self) -> Option<&str> {
        match &self.state {
            State::Idle => None,
            State::Outgoing { call, .. } | State::Incoming { call, .. } | State::Active { call, .. } => {
                Some(call)
            }
        }
    }

    fn remember(&mut self, call: &str) -> bool {
        if self.seen.iter().any(|c| c == call) {
            return false;
        }
        self.seen.push_back(call.to_string());
        while self.seen.len() > SEEN_KEEP {
            self.seen.pop_front();
        }
        true
    }

    fn send(to: Target, signal: Signal) -> Action {
        Action::Send { to, signal }
    }

    fn end(call: &str, reason: Reason) -> Signal {
        Signal::End { call: call.to_string(), reason }
    }

    fn miss(&mut self, peer: &str) {
        self.unanswered.push((peer.to_string(), self.now));
    }

    /// Advance the machine. `now` is our clock in milliseconds.
    pub fn step(&mut self, now: u64, input: Input) -> Vec<Action> {
        self.now = now;
        self.unanswered.retain(|(_, t)| now.saturating_sub(*t) < 3_600_000);
        match input {
            Input::Dial { call, peer, peer_devices, video, sdp } => {
                if !self.is_idle() || !is_hex32(&call) {
                    return vec![];
                }
                self.remember(&call);
                self.state = State::Outgoing {
                    call: call.clone(),
                    peer,
                    peer_devices: peer_devices.max(1),
                    started: now,
                    refused: HashSet::new(),
                    busy: false,
                    pending_ice: vec![],
                };
                vec![
                    Self::send(Target::PeerAll, Signal::Invite { call: call.clone(), video, sdp }),
                    // Our other devices should know, so they do not place a second call.
                    Self::send(Target::OwnOthers, Signal::Invite { call, video, sdp: "-".into() }),
                ]
            }
            Input::Recv { from, signal, ts } => self.recv(now, from, signal, ts),
            Input::Accept { sdp } => match std::mem::replace(&mut self.state, State::Idle) {
                State::Incoming { call, peer, device, .. } => {
                    self.state = State::Active {
                        call: call.clone(),
                        peer,
                        device: device.clone(),
                        incoming: true,
                        answered_at: now,
                    };
                    vec![
                        Action::StopRinging { call: call.clone() },
                        Self::send(Target::PeerDevice(device), Signal::Answer { call: call.clone(), sdp }),
                        Self::send(Target::OwnOthers, Self::end(&call, Reason::AnsweredElsewhere)),
                    ]
                }
                other => {
                    self.state = other;
                    vec![]
                }
            },
            Input::Decline { all } => match std::mem::replace(&mut self.state, State::Idle) {
                State::Incoming { call, peer, device, .. } => {
                    let mut out = vec![
                        Action::StopRinging { call: call.clone() },
                        Self::send(Target::PeerDevice(device), Self::end(&call, Reason::Declined)),
                    ];
                    if all {
                        out.push(Self::send(Target::OwnOthers, Self::end(&call, Reason::Declined)));
                    }
                    out.push(Action::Ended {
                        call,
                        peer: Some(peer),
                        incoming: true,
                        result: CallResult::Declined,
                        answered_at: None,
                    });
                    out
                }
                other => {
                    self.state = other;
                    vec![]
                }
            },
            Input::Hangup | Input::Fail => {
                let reason = if matches!(input, Input::Fail) { Reason::Failed } else { Reason::Hangup };
                match std::mem::replace(&mut self.state, State::Idle) {
                    State::Outgoing { call, peer, .. } => vec![
                        Self::send(Target::PeerAll, Self::end(&call, reason)),
                        Self::send(Target::OwnOthers, Self::end(&call, reason)),
                        Action::Ended {
                            call,
                            peer: Some(peer),
                            incoming: false,
                            result: if reason == Reason::Failed { CallResult::Failed } else { CallResult::NoAnswer },
                            answered_at: None,
                        },
                    ],
                    State::Active { call, peer, device, incoming, answered_at } => vec![
                        Self::send(Target::PeerDevice(device), Self::end(&call, reason)),
                        Action::Ended {
                            call,
                            peer: Some(peer),
                            incoming,
                            result: if reason == Reason::Failed { CallResult::Failed } else { CallResult::Completed },
                            answered_at: Some(answered_at),
                        },
                    ],
                    State::Incoming { call, peer, device, .. } => vec![
                        Action::StopRinging { call: call.clone() },
                        Self::send(Target::PeerDevice(device), Self::end(&call, Reason::Declined)),
                        Action::Ended {
                            call,
                            peer: Some(peer),
                            incoming: true,
                            result: CallResult::Declined,
                            answered_at: None,
                        },
                    ],
                    State::Idle => vec![],
                }
            }
            Input::LocalIce { candidates } => {
                if candidates.is_empty() {
                    return vec![];
                }
                match &mut self.state {
                    State::Outgoing { pending_ice, .. } => {
                        pending_ice.extend(candidates);
                        vec![]
                    }
                    State::Active { call, device, .. } => vec![Self::send(
                        Target::PeerDevice(device.clone()),
                        Signal::Ice { call: call.clone(), candidates },
                    )],
                    _ => vec![],
                }
            }
            Input::SendRestart { sdp } => match &self.state {
                State::Active { call, device, incoming: false, .. } => vec![Self::send(
                    Target::PeerDevice(device.clone()),
                    Signal::Restart { call: call.clone(), sdp },
                )],
                _ => vec![],
            },
            Input::SendRestartAnswer { sdp } => match &self.state {
                State::Active { call, device, incoming: true, .. } => vec![Self::send(
                    Target::PeerDevice(device.clone()),
                    Signal::RestartAnswer { call: call.clone(), sdp },
                )],
                _ => vec![],
            },
            Input::AskRestart => match &self.state {
                State::Active { call, device, incoming: true, .. } => vec![Self::send(
                    Target::PeerDevice(device.clone()),
                    Signal::RestartAsk { call: call.clone() },
                )],
                _ => vec![],
            },
            Input::Tick => match &self.state {
                State::Outgoing { started, .. } if now.saturating_sub(*started) >= RING_MS => {
                    let State::Outgoing { call, peer, refused, busy, peer_devices, .. } =
                        std::mem::replace(&mut self.state, State::Idle)
                    else {
                        unreachable!()
                    };
                    let result = if busy && refused.len() >= peer_devices { CallResult::Busy } else { CallResult::NoAnswer };
                    vec![
                        Self::send(Target::PeerAll, Self::end(&call, Reason::Timeout)),
                        Self::send(Target::OwnOthers, Self::end(&call, Reason::Timeout)),
                        Action::Ended { call, peer: Some(peer), incoming: false, result, answered_at: None },
                    ]
                }
                State::Incoming { started, .. } if now.saturating_sub(*started) >= RING_MS => {
                    let State::Incoming { call, peer, .. } = std::mem::replace(&mut self.state, State::Idle) else {
                        unreachable!()
                    };
                    self.miss(&peer);
                    vec![
                        Action::StopRinging { call: call.clone() },
                        Action::Ended { call, peer: Some(peer), incoming: true, result: CallResult::Missed, answered_at: None },
                    ]
                }
                _ => vec![],
            },
        }
    }

    fn recv(&mut self, now: u64, from: Origin, signal: Signal, ts: u64) -> Vec<Action> {
        match signal {
            Signal::Invite { call, video, sdp } => {
                // A placeholder invite from our own other device only tells us it is calling.
                let Some(peer) = from.peer.clone() else {
                    self.remember(&call);
                    return vec![];
                };
                if !self.remember(&call) {
                    return vec![];
                }
                if now.abs_diff(ts) > STALE_MS {
                    // Held back by the relay, or replayed: never ring, but tell the person they missed it.
                    return vec![Action::Ended {
                        call,
                        peer: Some(peer),
                        incoming: true,
                        result: CallResult::Missed,
                        answered_at: None,
                    }];
                }
                if self.cancelled.iter().any(|(c, p)| *c == call && *p == peer) {
                    // Its end came first: the caller already gave up. A missed call, not a ring.
                    self.miss(&peer);
                    return vec![Action::Ended {
                        call,
                        peer: Some(peer),
                        incoming: true,
                        result: CallResult::Missed,
                        answered_at: None,
                    }];
                }
                if self.unanswered.iter().filter(|(p, _)| *p == peer).count() >= MAX_UNANSWERED_PER_HOUR {
                    return vec![];
                }
                match &self.state {
                    State::Idle => {
                        self.state = State::Incoming {
                            call: call.clone(),
                            peer: peer.clone(),
                            device: from.device,
                            started: now,
                        };
                        vec![Action::Ring { call, peer, video, sdp, glare: false }]
                    }
                    // Both of us called at once: the smaller call id wins.
                    State::Outgoing { call: mine, peer: p, .. } if *p == peer => {
                        if call.as_str() < mine.as_str() {
                            let mine = mine.clone();
                            let out_peer = peer.clone();
                            self.state = State::Incoming {
                                call: call.clone(),
                                peer: peer.clone(),
                                device: from.device,
                                started: now,
                            };
                            vec![
                                Self::send(Target::PeerAll, Self::end(&mine, Reason::Busy)),
                                Self::send(Target::OwnOthers, Self::end(&mine, Reason::Busy)),
                                Action::Ended {
                                    call: mine,
                                    peer: Some(out_peer),
                                    incoming: false,
                                    result: CallResult::Busy,
                                    answered_at: None,
                                },
                                Action::Ring { call, peer, video, sdp, glare: true },
                            ]
                        } else {
                            vec![]
                        }
                    }
                    _ => vec![Self::send(Target::PeerDevice(from.device), Self::end(&call, Reason::Busy))],
                }
            }
            Signal::Answer { call, sdp } => {
                let State::Outgoing { call: mine, peer, pending_ice, .. } = &mut self.state else {
                    // A second device answering a call that is already active: tell it. The same device's answer again
                    // (the network delivered it twice) is not a second device and must not be told the call is over.
                    if let State::Active { call: mine, device, .. } = &self.state {
                        if *mine == call && from.peer.is_some() && from.device != *device {
                            return vec![Self::send(
                                Target::PeerDevice(from.device),
                                Self::end(&call, Reason::AnsweredElsewhere),
                            )];
                        }
                    }
                    return vec![];
                };
                if *mine != call || from.peer.as_deref() != Some(peer.as_str()) {
                    return vec![];
                }
                let (peer, held) = (peer.clone(), std::mem::take(pending_ice));
                self.state = State::Active {
                    call: call.clone(),
                    peer,
                    device: from.device.clone(),
                    incoming: false,
                    answered_at: now,
                };
                let mut out = vec![Action::Connect { call: call.clone(), sdp }];
                for chunk in held.chunks(MAX_CANDIDATES) {
                    out.push(Self::send(
                        Target::PeerDevice(from.device.clone()),
                        Signal::Ice { call: call.clone(), candidates: chunk.to_vec() },
                    ));
                }
                out
            }
            Signal::Ice { call, candidates } => match &self.state {
                State::Active { call: mine, device, peer, .. }
                    if *mine == call && *device == from.device && from.peer.as_deref() == Some(peer.as_str()) =>
                {
                    vec![Action::RemoteIce { call, candidates }]
                }
                _ => vec![],
            },
            Signal::End { call, reason } => self.recv_end(from, call, reason),
            // Restarts only count inside the active call, from the device we are talking to, in the right direction:
            // the caller offers, the callee answers or asks.
            Signal::Restart { call, sdp } => match &self.state {
                State::Active { call: mine, peer, device, incoming: true, .. }
                    if *mine == call && *device == from.device && from.peer.as_deref() == Some(peer.as_str()) =>
                {
                    vec![Action::Restart { call, sdp }]
                }
                _ => vec![],
            },
            Signal::RestartAnswer { call, sdp } => match &self.state {
                State::Active { call: mine, peer, device, incoming: false, .. }
                    if *mine == call && *device == from.device && from.peer.as_deref() == Some(peer.as_str()) =>
                {
                    vec![Action::RestartAnswer { call, sdp }]
                }
                _ => vec![],
            },
            Signal::RestartAsk { call } => match &self.state {
                State::Active { call: mine, peer, device, incoming: false, .. }
                    if *mine == call && *device == from.device && from.peer.as_deref() == Some(peer.as_str()) =>
                {
                    vec![Action::RestartAsk { call }]
                }
                _ => vec![],
            },
        }
    }

    fn recv_end(&mut self, from: Origin, call: String, reason: Reason) -> Vec<Action> {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Outgoing { call: mine, peer, peer_devices, started, mut refused, mut busy, pending_ice }
                if mine == call && from.peer.as_deref() == Some(peer.as_str()) =>
            {
                refused.insert(from.device);
                busy |= reason == Reason::Busy;
                if refused.len() >= peer_devices {
                    let result = if busy { CallResult::Busy } else { CallResult::Declined };
                    return vec![
                        Self::send(Target::OwnOthers, Self::end(&call, Reason::Hangup)),
                        Action::Ended { call, peer: Some(peer), incoming: false, result, answered_at: None },
                    ];
                }
                self.state = State::Outgoing { call: mine, peer, peer_devices, started, refused, busy, pending_ice };
                vec![]
            }
            State::Incoming { call: mine, peer, device, started }
                if mine == call
                    && ((from.peer.as_deref() == Some(peer.as_str()) && from.device == device)
                        || (from.peer.is_none()
                            && matches!(reason, Reason::AnsweredElsewhere | Reason::Declined))) =>
            {
                let (result, track_missed) = if from.peer.is_some() {
                    (CallResult::Missed, true)
                } else if reason == Reason::AnsweredElsewhere {
                    (CallResult::AnsweredElsewhere, false)
                } else {
                    (CallResult::Declined, false)
                };
                if track_missed {
                    self.miss(&peer);
                }
                let _ = started;
                vec![
                    Action::StopRinging { call: call.clone() },
                    Action::Ended { call, peer: Some(peer), incoming: true, result, answered_at: None },
                ]
            }
            State::Active { call: mine, peer, device, incoming, answered_at }
                if mine == call && from.peer.as_deref() == Some(peer.as_str()) && from.device == device =>
            {
                let result = if reason == Reason::Failed { CallResult::Failed } else { CallResult::Completed };
                vec![Action::Ended { call, peer: Some(peer), incoming, result, answered_at: Some(answered_at) }]
            }
            other => {
                self.state = other;
                // A caller's end for a call we know nothing about (yet): remember it for when the invite turns up.
                if let Some(peer) = from.peer {
                    if !self.cancelled.iter().any(|(c, p)| *c == call && *p == peer) {
                        self.cancelled.push_back((call, peer));
                        while self.cancelled.len() > SEEN_KEEP {
                            self.cancelled.pop_front();
                        }
                    }
                }
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    fn id(n: u8) -> String {
        format!("{:02x}", n).repeat(16)
    }
    fn contact(dev: &str) -> Origin {
        Origin { device: dev.into(), peer: Some("bob".into()) }
    }
    fn own(dev: &str) -> Origin {
        Origin { device: dev.into(), peer: None }
    }
    fn invite(call: &str) -> Signal {
        Signal::Invite { call: call.into(), video: false, sdp: "offer".into() }
    }
    fn dial(m: &mut Machine, call: &str, devices: usize) -> Vec<Action> {
        m.step(NOW, Input::Dial { call: call.into(), peer: "bob".into(), peer_devices: devices, video: false, sdp: "offer".into() })
    }
    fn recv(m: &mut Machine, now: u64, from: Origin, s: Signal) -> Vec<Action> {
        m.step(now, Input::Recv { from, signal: s, ts: NOW })
    }
    fn ended(a: &[Action]) -> Option<CallResult> {
        a.iter().find_map(|x| if let Action::Ended { result, .. } = x { Some(*result) } else { None })
    }

    #[test]
    fn an_invite_that_arrives_after_its_own_hangup_does_not_ring() {
        let mut m = Machine::new();
        // the caller's hang-up overtakes the invite
        assert!(recv(&mut m, NOW, contact("d1"), Signal::End { call: id(1), reason: Reason::Hangup }).is_empty());
        let a = recv(&mut m, NOW + 500, contact("d1"), invite(&id(1)));
        assert!(!a.iter().any(|x| matches!(x, Action::Ring { .. })), "{a:?}");
        assert_eq!(ended(&a), Some(CallResult::Missed));
        assert!(m.is_idle());
        // another call is unaffected
        let a = recv(&mut m, NOW + 600, contact("d1"), invite(&id(2)));
        assert!(a.iter().any(|x| matches!(x, Action::Ring { .. })));
    }

    #[test]
    fn the_same_answer_twice_does_not_end_the_call() {
        let mut m = Machine::new();
        dial(&mut m, &id(1), 1);
        let ans = Signal::Answer { call: id(1), sdp: "ans".into() };
        assert!(matches!(&recv(&mut m, NOW + 100, contact("d1"), ans.clone())[0], Action::Connect { .. }));
        // delivered again: ignored, the call carries on
        assert!(recv(&mut m, NOW + 150, contact("d1"), ans).is_empty());
        assert_eq!(m.current(), Some(id(1).as_str()));
    }

    #[test]
    fn signals_are_validated() {
        let ok = invite(&id(1)).to_json();
        assert!(Signal::parse(&ok).is_ok());
        assert!(Signal::parse(&invite("short").to_json()).is_err());
        let big = Signal::Invite { call: id(1), video: false, sdp: "x".repeat(MAX_SDP + 1) };
        assert!(Signal::parse(&big.to_json()).is_err());
        let empty = Signal::Answer { call: id(1), sdp: String::new() };
        assert!(Signal::parse(&empty.to_json()).is_err());
        let many = Signal::Ice { call: id(1), candidates: vec!["c".into(); MAX_CANDIDATES + 1] };
        assert!(Signal::parse(&many.to_json()).is_err());
        let none = Signal::Ice { call: id(1), candidates: vec![] };
        assert!(Signal::parse(&none.to_json()).is_err());
        assert!(Signal::parse(r#"{"op":"end","call":"00000000000000000000000000000000","reason":"nope"}"#).is_err());
        assert!(Signal::parse("{}").is_err());
    }

    #[test]
    fn outgoing_call_answered_by_first_device_only() {
        let mut m = Machine::new();
        let a = dial(&mut m, &id(1), 2);
        assert!(matches!(&a[0], Action::Send { to: Target::PeerAll, signal: Signal::Invite { .. } }));
        // candidates gathered while ringing are held back
        assert!(m.step(NOW, Input::LocalIce { candidates: vec!["c1".into()] }).is_empty());
        let a = recv(&mut m, NOW + 100, contact("d1"), Signal::Answer { call: id(1), sdp: "ans".into() });
        assert!(matches!(&a[0], Action::Connect { sdp, .. } if sdp == "ans"));
        assert!(matches!(&a[1], Action::Send { to: Target::PeerDevice(d), signal: Signal::Ice { .. } } if d == "d1"));
        // the second device's answer is turned away
        let a = recv(&mut m, NOW + 200, contact("d2"), Signal::Answer { call: id(1), sdp: "ans2".into() });
        assert_eq!(
            a,
            vec![Action::Send {
                to: Target::PeerDevice("d2".into()),
                signal: Signal::End { call: id(1), reason: Reason::AnsweredElsewhere }
            }]
        );
        // remote candidates only from the answering device
        assert!(recv(&mut m, NOW + 300, contact("d2"), Signal::Ice { call: id(1), candidates: vec!["x".into()] }).is_empty());
        let a = recv(&mut m, NOW + 300, contact("d1"), Signal::Ice { call: id(1), candidates: vec!["x".into()] });
        assert!(matches!(&a[0], Action::RemoteIce { .. }));
        // and it ends when that device hangs up
        let a = recv(&mut m, NOW + 5000, contact("d1"), Signal::End { call: id(1), reason: Reason::Hangup });
        assert_eq!(ended(&a), Some(CallResult::Completed));
        assert!(m.is_idle());
    }

    #[test]
    fn answer_from_a_stranger_or_wrong_call_is_ignored() {
        let mut m = Machine::new();
        dial(&mut m, &id(1), 1);
        let eve = Origin { device: "e".into(), peer: Some("eve".into()) };
        assert!(recv(&mut m, NOW, eve, Signal::Answer { call: id(1), sdp: "a".into() }).is_empty());
        assert!(recv(&mut m, NOW, own("mine2"), Signal::Answer { call: id(1), sdp: "a".into() }).is_empty());
        assert!(recv(&mut m, NOW, contact("d1"), Signal::Answer { call: id(2), sdp: "a".into() }).is_empty());
        assert_eq!(m.current(), Some(id(1).as_str()));
    }

    #[test]
    fn outgoing_declined_only_when_every_device_declined() {
        let mut m = Machine::new();
        dial(&mut m, &id(1), 2);
        let a = recv(&mut m, NOW, contact("d1"), Signal::End { call: id(1), reason: Reason::Declined });
        assert!(a.is_empty());
        let a = recv(&mut m, NOW, contact("d2"), Signal::End { call: id(1), reason: Reason::Busy });
        assert_eq!(ended(&a), Some(CallResult::Busy));
        assert!(m.is_idle());
    }

    #[test]
    fn outgoing_times_out() {
        let mut m = Machine::new();
        dial(&mut m, &id(1), 1);
        assert!(m.step(NOW + RING_MS - 1, Input::Tick).is_empty());
        let a = m.step(NOW + RING_MS, Input::Tick);
        assert_eq!(ended(&a), Some(CallResult::NoAnswer));
        assert!(matches!(&a[0], Action::Send { signal: Signal::End { reason: Reason::Timeout, .. }, .. }));
        assert!(m.is_idle());
    }

    #[test]
    fn caller_hangs_up_while_ringing() {
        let mut m = Machine::new();
        dial(&mut m, &id(1), 1);
        let a = m.step(NOW, Input::Hangup);
        assert!(matches!(&a[0], Action::Send { to: Target::PeerAll, signal: Signal::End { reason: Reason::Hangup, .. } }));
        assert!(m.is_idle());
    }

    #[test]
    fn incoming_rings_and_accepting_silences_our_other_devices() {
        let mut m = Machine::new();
        let a = recv(&mut m, NOW, contact("d1"), invite(&id(3)));
        assert!(matches!(&a[0], Action::Ring { glare: false, .. }));
        let a = m.step(NOW + 10, Input::Accept { sdp: "ans".into() });
        assert!(a.contains(&Action::Send {
            to: Target::PeerDevice("d1".into()),
            signal: Signal::Answer { call: id(3), sdp: "ans".into() }
        }));
        assert!(a.contains(&Action::Send {
            to: Target::OwnOthers,
            signal: Signal::End { call: id(3), reason: Reason::AnsweredElsewhere }
        }));
        let a = m.step(NOW + 5000, Input::Hangup);
        assert!(matches!(&a[0], Action::Send { to: Target::PeerDevice(d), .. } if d == "d1"));
        assert_eq!(ended(&a), Some(CallResult::Completed));
    }

    #[test]
    fn answered_on_another_of_our_devices_stops_ringing() {
        let mut m = Machine::new();
        recv(&mut m, NOW, contact("d1"), invite(&id(3)));
        let a = recv(&mut m, NOW + 1, own("mine2"), Signal::End { call: id(3), reason: Reason::AnsweredElsewhere });
        assert!(matches!(&a[0], Action::StopRinging { .. }));
        assert_eq!(ended(&a), Some(CallResult::AnsweredElsewhere));
        assert!(m.is_idle());
    }

    #[test]
    fn a_contact_cannot_end_a_call_as_if_it_were_our_device() {
        let mut m = Machine::new();
        recv(&mut m, NOW, contact("d1"), invite(&id(3)));
        // an end for this call from a different contact does nothing
        let eve = Origin { device: "e".into(), peer: Some("eve".into()) };
        assert!(recv(&mut m, NOW, eve, Signal::End { call: id(3), reason: Reason::AnsweredElsewhere }).is_empty());
        // and so does a contact's device other than the caller's
        assert!(recv(&mut m, NOW, contact("d9"), Signal::End { call: id(3), reason: Reason::Hangup }).is_empty());
        assert!(!m.is_idle());
    }

    #[test]
    fn decline_is_local_unless_asked() {
        let mut m = Machine::new();
        recv(&mut m, NOW, contact("d1"), invite(&id(3)));
        let a = m.step(NOW, Input::Decline { all: false });
        assert!(!a.iter().any(|x| matches!(x, Action::Send { to: Target::OwnOthers, .. })));
        assert_eq!(ended(&a), Some(CallResult::Declined));
        recv(&mut m, NOW, contact("d1"), invite(&id(4)));
        let a = m.step(NOW, Input::Decline { all: true });
        assert!(a.iter().any(|x| matches!(x, Action::Send { to: Target::OwnOthers, .. })));
    }

    #[test]
    fn busy_replies_busy_and_does_not_ring() {
        let mut m = Machine::new();
        recv(&mut m, NOW, contact("d1"), invite(&id(3)));
        let carol = Origin { device: "c1".into(), peer: Some("carol".into()) };
        let a = recv(&mut m, NOW, carol, invite(&id(4)));
        assert_eq!(
            a,
            vec![Action::Send {
                to: Target::PeerDevice("c1".into()),
                signal: Signal::End { call: id(4), reason: Reason::Busy }
            }]
        );
        assert_eq!(m.current(), Some(id(3).as_str()));
    }

    #[test]
    fn glare_smaller_id_wins_on_both_sides() {
        // we hold the larger id: we give way and ring
        let mut m = Machine::new();
        dial(&mut m, &id(9), 1);
        let a = recv(&mut m, NOW, contact("d1"), invite(&id(2)));
        assert!(a.iter().any(|x| matches!(x, Action::Ring { glare: true, .. })));
        assert_eq!(m.current(), Some(id(2).as_str()));
        // we hold the smaller id: we keep our call and ignore theirs
        let mut m = Machine::new();
        dial(&mut m, &id(2), 1);
        let a = recv(&mut m, NOW, contact("d1"), invite(&id(9)));
        assert!(a.is_empty());
        assert_eq!(m.current(), Some(id(2).as_str()));
    }

    #[test]
    fn stale_duplicate_and_replayed_invites_do_not_ring() {
        let mut m = Machine::new();
        let a = m.step(NOW + STALE_MS + 1, Input::Recv { from: contact("d1"), signal: invite(&id(3)), ts: NOW });
        assert_eq!(ended(&a), Some(CallResult::Missed));
        assert!(m.is_idle());
        // the same id again, even with a fresh time, is a replay
        let a = m.step(NOW, Input::Recv { from: contact("d1"), signal: invite(&id(3)), ts: NOW });
        assert!(a.is_empty());
        // a fresh one rings, a duplicate of it does not
        assert!(!recv(&mut m, NOW, contact("d1"), invite(&id(4))).is_empty());
        m.step(NOW, Input::Decline { all: false });
        assert!(recv(&mut m, NOW, contact("d1"), invite(&id(4))).is_empty());
    }

    #[test]
    fn ringing_that_nobody_answers_is_missed_and_rate_limited() {
        let mut m = Machine::new();
        for n in 0..MAX_UNANSWERED_PER_HOUR as u8 {
            let t = NOW + n as u64 * 100_000;
            let a = m.step(t, Input::Recv { from: contact("d1"), signal: invite(&id(10 + n)), ts: t });
            assert!(matches!(&a[0], Action::Ring { .. }));
            let a = m.step(t + RING_MS, Input::Tick);
            assert_eq!(ended(&a), Some(CallResult::Missed));
        }
        let t = NOW + 400_000;
        let a = m.step(t, Input::Recv { from: contact("d1"), signal: invite(&id(99)), ts: t });
        assert!(a.is_empty());
        // an hour later it is allowed again
        let t = NOW + 4_000_000;
        let a = m.step(t, Input::Recv { from: contact("d1"), signal: invite(&id(98)), ts: t });
        assert!(matches!(&a[0], Action::Ring { .. }));
    }

    #[test]
    fn caller_gives_up_while_it_rings_for_us() {
        let mut m = Machine::new();
        recv(&mut m, NOW, contact("d1"), invite(&id(3)));
        let a = recv(&mut m, NOW + 10, contact("d1"), Signal::End { call: id(3), reason: Reason::Timeout });
        assert_eq!(ended(&a), Some(CallResult::Missed));
        assert!(m.is_idle());
    }

    fn active_pair(caller: bool) -> Machine {
        let mut m = Machine::new();
        if caller {
            dial(&mut m, &id(1), 1);
            recv(&mut m, NOW, contact("d1"), Signal::Answer { call: id(1), sdp: "ans".into() });
        } else {
            recv(&mut m, NOW, contact("d1"), invite(&id(1)));
            m.step(NOW, Input::Accept { sdp: "ans".into() });
        }
        m
    }

    #[test]
    fn a_network_change_restarts_the_connection_caller_offers_callee_answers() {
        let mut caller = active_pair(true);
        let mut callee = active_pair(false);
        // the caller offers; only it may
        let a = caller.step(NOW, Input::SendRestart { sdp: "o2".into() });
        assert_eq!(
            a,
            vec![Action::Send {
                to: Target::PeerDevice("d1".into()),
                signal: Signal::Restart { call: id(1), sdp: "o2".into() }
            }]
        );
        assert!(callee.step(NOW, Input::SendRestart { sdp: "x".into() }).is_empty(), "the callee never offers");
        // the callee gets it, answers
        let a = recv(&mut callee, NOW, contact("d1"), Signal::Restart { call: id(1), sdp: "o2".into() });
        assert_eq!(a, vec![Action::Restart { call: id(1), sdp: "o2".into() }]);
        let a = callee.step(NOW, Input::SendRestartAnswer { sdp: "a2".into() });
        assert!(matches!(&a[0], Action::Send { signal: Signal::RestartAnswer { .. }, .. }));
        assert!(caller.step(NOW, Input::SendRestartAnswer { sdp: "x".into() }).is_empty(), "the caller never answers");
        // the caller applies the answer
        let a = recv(&mut caller, NOW, contact("d1"), Signal::RestartAnswer { call: id(1), sdp: "a2".into() });
        assert_eq!(a, vec![Action::RestartAnswer { call: id(1), sdp: "a2".into() }]);
        // the callee, whose network changed, asks the caller to restart
        let a = callee.step(NOW, Input::AskRestart);
        assert!(matches!(&a[0], Action::Send { signal: Signal::RestartAsk { .. }, .. }));
        let a = recv(&mut caller, NOW, contact("d1"), Signal::RestartAsk { call: id(1) });
        assert_eq!(a, vec![Action::RestartAsk { call: id(1) }]);
        assert!(caller.step(NOW, Input::AskRestart).is_empty(), "the caller restarts itself, it does not ask");
    }

    #[test]
    fn restart_signals_from_anyone_else_or_the_wrong_way_or_outside_a_call_are_ignored() {
        let mut caller = active_pair(true);
        let mut callee = active_pair(false);
        let eve = Origin { device: "e".into(), peer: Some("eve".into()) };
        let r = Signal::Restart { call: id(1), sdp: "o".into() };
        assert!(recv(&mut callee, NOW, eve.clone(), r.clone()).is_empty(), "another contact");
        assert!(recv(&mut callee, NOW, contact("d9"), r.clone()).is_empty(), "another device of the contact");
        assert!(recv(&mut callee, NOW, own("mine2"), r.clone()).is_empty(), "our own device");
        assert!(recv(&mut caller, NOW, contact("d1"), r.clone()).is_empty(), "the caller never receives an offer");
        assert!(recv(&mut callee, NOW, contact("d1"), Signal::RestartAnswer { call: id(1), sdp: "a".into() }).is_empty());
        assert!(recv(&mut callee, NOW, contact("d1"), Signal::RestartAsk { call: id(1) }).is_empty());
        assert!(recv(&mut callee, NOW, contact("d1"), Signal::Restart { call: id(2), sdp: "o".into() }).is_empty(), "another call");
        let mut idle = Machine::new();
        assert!(recv(&mut idle, NOW, contact("d1"), r).is_empty());
        let mut ringing = Machine::new();
        recv(&mut ringing, NOW, contact("d1"), invite(&id(1)));
        assert!(recv(&mut ringing, NOW, contact("d1"), Signal::Restart { call: id(1), sdp: "o".into() }).is_empty());
        assert!(Signal::parse(&Signal::Restart { call: id(1), sdp: String::new() }.to_json()).is_err());
        assert!(Signal::parse(&Signal::RestartAsk { call: id(1) }.to_json()).is_ok());
    }

    #[test]
    fn events_for_other_calls_and_idle_state_are_ignored() {
        let mut m = Machine::new();
        assert!(recv(&mut m, NOW, contact("d1"), Signal::Answer { call: id(1), sdp: "a".into() }).is_empty());
        assert!(recv(&mut m, NOW, contact("d1"), Signal::End { call: id(1), reason: Reason::Hangup }).is_empty());
        assert!(recv(&mut m, NOW, contact("d1"), Signal::Ice { call: id(1), candidates: vec!["c".into()] }).is_empty());
        assert!(m.step(NOW, Input::Accept { sdp: "a".into() }).is_empty());
        assert!(m.step(NOW, Input::Hangup).is_empty());
        // our own device's invite only marks the call id as seen
        assert!(recv(&mut m, NOW, own("mine2"), Signal::Invite { call: id(5), video: false, sdp: "-".into() }).is_empty());
        assert!(m.is_idle());
    }
}
