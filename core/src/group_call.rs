//! Group call setup (ADR 0032, phase B0): the signals members of a group exchange, and the state machine that
//! decides what to do with them. No I/O and no media, like `call`: the front ends own WebRTC; this module only says
//! which signal to send to whom, when to ring, which pairs of devices connect (a mesh of up to `MESH_MAX` people),
//! when a call moves to a call server, and when it is over.
//!
//! What the caller must have checked before it hands a signal in: the envelope is a `Kind::GroupCall`, we are a
//! member of the group it names, and the sending device belongs to a member of our current member list (ADR 0024
//! section 4). The machine does not know the member list; it is told the group's size and whether the person muted
//! it (`Input::Group`), and whether the sender is a contact (`Origin::contact`, which decides relay-only media).
//!
//! Two rules keep a mesh free of races. The device that joined *later* dials the earlier ones (order: the `at` each
//! device put in its own `Start`/`Join`, then the device id; both sides read the same two numbers, so they agree
//! whatever their clocks say). And a signal that arrives before the one it depends on (a pair signal before the
//! sender's `Join`, a `Join` before the `Start`) is held for a short while and replayed.

use crate::call::{
    is_hex32, CallResult, Reason, MAX_CANDIDATE, MAX_CANDIDATES, MAX_SDP, MAX_UNANSWERED_PER_HOUR,
    RING_MS, STALE_MS,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// Most people in a mesh call (ADR 0032 section 1). A fifth person moves the call to a call server.
pub const MESH_MAX: usize = 4;
/// A small group rings only up to this many members; larger ones show the join bar without ringing.
pub const RING_MAX_MEMBERS: usize = 8;
/// At most one ring per group in this time; a call started inside it shows the join bar only.
pub const RING_GROUP_COOLDOWN_MS: u64 = 600_000;
/// An empty call ends on its own after this long.
pub const EMPTY_END_MS: u64 = 60_000;
/// Signals that arrived before what they depend on are kept this long.
pub const EARLY_KEEP_MS: u64 = 60_000;
const EARLY_MAX: usize = 64;
const ENDED_KEEP: usize = 256;
const ENDED_MS: u64 = 24 * 3_600_000;
const MAX_URL: usize = 256;
const MAX_ROOM: usize = 64;

/// The body of a `Kind::GroupCall` envelope. The group and the member list it was sent with travel in the envelope.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Signal {
    /// "Ann started a call." `sfu`: the starter's relay can move the call to a call server when a fifth person joins.
    /// `at` is the sender's clock in ms; it fixes the order of joining (see the module note).
    Start {
        call: String,
        video: bool,
        sfu: bool,
        at: u64,
    },
    /// This device joins the call.
    Join { call: String, video: bool, at: u64 },
    /// This device leaves the call. `at` is later than the `at` of the `Join` it ends.
    Leave { call: String, at: u64 },
    /// The host moves the call to a call server: everyone connects there and drops the pairwise connections.
    Move {
        call: String,
        url: String,
        room: String,
    },
    /// A call key for `generation`, sealed by the session it travels in (hex, 32 bytes). Used from phase D2.
    Key {
        call: String,
        generation: u16,
        key: String,
    },
    /// A device has the key of `generation`.
    KeyAck { call: String, generation: u16 },
    /// The call is over.
    End { call: String, reason: Reason },
    /// Mesh, one pair: the later joiner's offer to one earlier participant (sent to that one device). `at` is the
    /// dialer's own join stamp; it names the pair, so a late signal of an earlier join of the same device cannot be
    /// mistaken for one of this join.
    Invite {
        call: String,
        video: bool,
        sdp: String,
        at: u64,
    },
    /// Mesh, one pair: the answer. `of` is the `at` of the invite it answers.
    Answer { call: String, sdp: String, of: u64 },
    /// Mesh, one pair: candidates (`of`: as for `Answer`).
    Ice {
        call: String,
        candidates: Vec<String>,
        of: u64,
    },
    /// Mesh, one pair: this connection is over (`failed`, or `hangup`).
    PairEnd {
        call: String,
        reason: Reason,
        of: u64,
    },
}

impl Signal {
    pub fn call(&self) -> &str {
        match self {
            Signal::Start { call, .. }
            | Signal::Join { call, .. }
            | Signal::Leave { call, .. }
            | Signal::Move { call, .. }
            | Signal::Key { call, .. }
            | Signal::KeyAck { call, .. }
            | Signal::End { call, .. }
            | Signal::Invite { call, .. }
            | Signal::Answer { call, .. }
            | Signal::Ice { call, .. }
            | Signal::PairEnd { call, .. } => call,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("signal serializes")
    }

    /// Parse and validate. Bad shapes, oversized SDP, too many candidates, odd keys and addresses are refused.
    pub fn parse(json: &str) -> Result<Self> {
        let s: Signal = serde_json::from_str(json).map_err(|_| Error::Malformed)?;
        let sdp_ok = |sdp: &str| !sdp.is_empty() && sdp.len() <= MAX_SDP;
        let hex = |v: &str, len: usize| {
            v.len() == len
                && v.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        };
        let ok = is_hex32(s.call())
            && match &s {
                Signal::Start { .. }
                | Signal::Join { .. }
                | Signal::Leave { .. }
                | Signal::KeyAck { .. }
                | Signal::End { .. }
                | Signal::PairEnd { .. } => true,
                Signal::Invite { sdp, .. } | Signal::Answer { sdp, .. } => sdp_ok(sdp),
                Signal::Ice { candidates, .. } => {
                    !candidates.is_empty()
                        && candidates.len() <= MAX_CANDIDATES
                        && candidates.iter().all(|c| c.len() <= MAX_CANDIDATE)
                }
                Signal::Key { key, .. } => hex(key, 64),
                Signal::Move { url, room, .. } => {
                    (url.starts_with("wss://") || url.starts_with("ws://"))
                        && url.len() <= MAX_URL
                        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
                        && !room.is_empty()
                        && room.len() <= MAX_ROOM
                        && room.bytes().all(|c| c.is_ascii_alphanumeric())
                }
            };
        if ok {
            Ok(s)
        } else {
            Err(Error::Malformed)
        }
    }
}

/// Who a signal goes to; the caller knows the group's members and their devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Every device of every other member, and our own other devices.
    AllMembers,
    /// One device (by id), for the pairwise signals.
    Device(String),
}

/// Where a received signal came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The sender's owner key.
    pub owner: String,
    pub device: String,
    /// One of our own other devices.
    pub own: bool,
    /// The sender is in our contacts. A pair that is not two contacts connects through the relay only.
    pub contact: bool,
}

/// One person in a call (one device per person).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub owner: String,
    pub device: String,
    pub video: bool,
    /// The sender's clock when it joined; with the device id it fixes who dials whom.
    pub at: u64,
    pub contact: bool,
}

impl Participant {
    fn key(&self) -> (u64, &str) {
        (self.at, &self.device)
    }
}

/// What the call bar of a group shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarInfo {
    pub call: String,
    pub people: usize,
    pub video: bool,
    /// We are in it.
    pub joined: bool,
    /// One of our own other devices is in it.
    pub elsewhere: bool,
    /// The call runs on a call server.
    pub sfu: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// We are already in a call.
    Busy,
    /// Another device of ours is in this call.
    Elsewhere,
    /// A mesh is full and the call cannot move to a call server.
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Facts about a group: how many members it has and whether the person muted its calls. Send before the
    /// first signal of a group, and again when they change. A group nothing is known about does not ring.
    Group {
        group: String,
        members: usize,
        muted: bool,
    },
    /// The person starts a call in `group`. `sfu`: our relay can host a call server.
    Start {
        call: String,
        group: String,
        video: bool,
        sfu: bool,
    },
    Recv {
        from: Origin,
        group: String,
        signal: Signal,
        ts: u64,
    },
    /// Join a call the bar shows or the phone is ringing for.
    Join {
        call: String,
        video: bool,
    },
    /// Leave the call we are in.
    Leave,
    /// Stop ringing for a call without joining it.
    Dismiss {
        call: String,
    },
    /// The front end made its offer for the pair with `device` (after `Action::Pair { dial: true }`).
    LocalOffer {
        device: String,
        video: bool,
        sdp: String,
    },
    /// The front end's answer to `Action::RemoteOffer`.
    LocalAnswer {
        device: String,
        sdp: String,
    },
    LocalIce {
        device: String,
        candidates: Vec<String>,
    },
    /// Media with `device` failed.
    PairFailed {
        device: String,
    },
    /// A call server is ready for the call we host (after `Action::MoveToSfu`).
    SfuReady {
        url: String,
        room: String,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send {
        to: Target,
        signal: Signal,
    },
    /// Ring for a call in `group`, started by `from` (an owner key).
    Ring {
        call: String,
        group: String,
        from: String,
        video: bool,
    },
    StopRinging {
        call: String,
    },
    /// The group's call bar changed: the running call, or nothing.
    Bar {
        group: String,
        running: Option<BarInfo>,
    },
    /// Everyone now in the call, in joining order.
    Participants {
        call: String,
        people: Vec<Participant>,
    },
    /// Open a connection to one participant. When `dial`, make an offer and hand it back as `Input::LocalOffer`;
    /// otherwise wait for their `RemoteOffer`. `relay_only`: gather relay candidates only (they are not a contact).
    Pair {
        call: String,
        owner: String,
        device: String,
        dial: bool,
        relay_only: bool,
    },
    Unpair {
        call: String,
        device: String,
    },
    RemoteOffer {
        call: String,
        owner: String,
        device: String,
        video: bool,
        sdp: String,
        relay_only: bool,
    },
    RemoteAnswer {
        call: String,
        device: String,
        sdp: String,
    },
    RemoteIce {
        call: String,
        device: String,
        candidates: Vec<String>,
    },
    RemoteKey {
        call: String,
        device: String,
        generation: u16,
        key: String,
    },
    RemoteKeyAck {
        call: String,
        device: String,
        generation: u16,
    },
    /// We host the call and a fifth person joined: get a call server and answer with `Input::SfuReady`.
    MoveToSfu {
        call: String,
    },
    /// The call runs on a call server now: connect there, drop the pairs.
    SfuRoom {
        call: String,
        url: String,
        room: String,
    },
    Refused {
        call: String,
        why: Refusal,
    },
    /// We were in the call and are not any more (we left, or it ended): write the call log.
    Left {
        call: String,
        group: String,
        joined_at: u64,
        peak: usize,
    },
    /// A call rang for us and we did not join it: write a missed call.
    Missed {
        call: String,
        group: String,
        from: String,
    },
    /// The call is over for everyone (the bar goes away); `result` is how it ended for this device.
    Ended {
        call: String,
        group: String,
        result: CallResult,
    },
}

#[derive(Debug, Clone)]
struct Running {
    call: String,
    group: String,
    starter: String,
    video: bool,
    sfu_capable: bool,
    sfu: Option<(String, String)>,
    people: Vec<Participant>,
    /// device -> `at` of its last leave; an older `Join` is a reordered one and is ignored.
    gone: BTreeMap<String, u64>,
    empty_since: Option<u64>,
    ringing: Option<u64>,
    joined_at: Option<u64>,
    peak: usize,
    /// device -> the pair with it.
    pairs: BTreeMap<String, Pair>,
    move_requested: bool,
}

/// A pairwise connection: who dialed, and the dialer's join stamp that names it.
#[derive(Debug, Clone, Copy)]
struct Pair {
    dialed: bool,
    id: u64,
}

#[derive(Debug, Clone)]
struct Early {
    from: Origin,
    group: String,
    signal: Signal,
    ts: u64,
    kept: u64,
}

#[derive(Debug, Clone, Copy)]
struct GroupInfo {
    members: usize,
    muted: bool,
}

pub struct Machine {
    owner: String,
    device: String,
    groups: BTreeMap<String, GroupInfo>,
    calls: BTreeMap<String, Running>,
    /// The call we are in.
    mine: Option<String>,
    ended: VecDeque<(String, u64)>,
    /// (owner, time) of rings nobody answered.
    unanswered: Vec<(String, u64)>,
    last_ring: BTreeMap<String, u64>,
    early: VecDeque<Early>,
    now: u64,
    /// The last `at` we put in a signal; each is strictly later than the one before, so a leave always orders after
    /// the join it ends even within one millisecond.
    last_at: u64,
}

impl Machine {
    /// `owner` and `device` are ours.
    pub fn new(owner: &str, device: &str) -> Self {
        Self {
            owner: owner.to_string(),
            device: device.to_string(),
            groups: BTreeMap::new(),
            calls: BTreeMap::new(),
            mine: None,
            ended: VecDeque::new(),
            unanswered: Vec::new(),
            last_ring: BTreeMap::new(),
            early: VecDeque::new(),
            now: 0,
            last_at: 0,
        }
    }

    /// The call we are in.
    pub fn current(&self) -> Option<&str> {
        self.mine.as_deref()
    }

    /// The call we are ringing for, if any.
    pub fn ringing(&self) -> Option<&str> {
        self.calls
            .values()
            .find(|r| r.ringing.is_some())
            .map(|r| r.call.as_str())
    }

    /// The running call of a group, as the bar shows it.
    pub fn bar(&self, group: &str) -> Option<BarInfo> {
        self.calls
            .values()
            .find(|r| r.group == group)
            .map(|r| self.bar_of(r))
    }

    /// Who is in a call.
    pub fn people(&self, call: &str) -> Vec<Participant> {
        self.calls
            .get(call)
            .map(|r| r.people.clone())
            .unwrap_or_default()
    }

    /// Whether this machine still tracks `call` (running, not ended).
    pub fn tracks(&self, call: &str) -> bool {
        self.calls.contains_key(call)
    }

    /// Calls this machine still tracks (running, not ended).
    pub fn running(&self) -> usize {
        self.calls.len()
    }

    fn me_in(&self, call: &str) -> bool {
        self.mine.as_deref() == Some(call)
    }

    fn bar_of(&self, r: &Running) -> BarInfo {
        BarInfo {
            call: r.call.clone(),
            people: r.people.len(),
            video: r.video,
            joined: self.me_in(&r.call),
            elsewhere: r
                .people
                .iter()
                .any(|p| p.owner == self.owner && p.device != self.device),
            sfu: r.sfu.is_some(),
        }
    }

    fn bar_action(&self, call: &str) -> Option<Action> {
        self.calls.get(call).map(|r| Action::Bar {
            group: r.group.clone(),
            running: Some(self.bar_of(r)),
        })
    }

    fn was_ended(&self, call: &str) -> bool {
        self.ended.iter().any(|(c, _)| c == call)
    }

    fn mark_ended(&mut self, call: &str) {
        if !self.was_ended(call) {
            self.ended.push_back((call.to_string(), self.now));
        }
        while self.ended.len() > ENDED_KEEP {
            self.ended.pop_front();
        }
    }

    /// Advance the machine. `now` is our clock in milliseconds.
    pub fn step(&mut self, now: u64, input: Input) -> Vec<Action> {
        self.now = now;
        self.unanswered
            .retain(|(_, t)| now.saturating_sub(*t) < 3_600_000);
        self.ended
            .retain(|(_, t)| now.saturating_sub(*t) < ENDED_MS);
        let mut out = vec![];
        match input {
            Input::Group {
                group,
                members,
                muted,
            } => {
                self.groups.insert(group, GroupInfo { members, muted });
            }
            Input::Start {
                call,
                group,
                video,
                sfu,
            } => self.start(call, group, video, sfu, &mut out),
            Input::Recv {
                from,
                group,
                signal,
                ts,
            } => self.recv(from, group, signal, ts, &mut out),
            Input::Join { call, video } => self.join(&call, video, &mut out),
            Input::Leave => {
                if let Some(call) = self.mine.clone() {
                    self.leave(&call, Reason::Hangup, &mut out);
                }
            }
            Input::Dismiss { call } => {
                if let Some(r) = self.calls.get_mut(&call) {
                    if r.ringing.take().is_some() {
                        out.push(Action::StopRinging { call });
                    }
                }
            }
            Input::LocalOffer { device, video, sdp } => {
                if let Some((call, pair)) = self.pair_of(&device) {
                    out.push(Self::pair_send(
                        &device,
                        Signal::Invite {
                            call,
                            video,
                            sdp,
                            at: pair.id,
                        },
                    ));
                }
            }
            Input::LocalAnswer { device, sdp } => {
                if let Some((call, pair)) = self.pair_of(&device) {
                    out.push(Self::pair_send(
                        &device,
                        Signal::Answer {
                            call,
                            sdp,
                            of: pair.id,
                        },
                    ));
                }
            }
            Input::LocalIce { device, candidates } => {
                if let Some((call, pair)) = self.pair_of(&device) {
                    out.push(Self::pair_send(
                        &device,
                        Signal::Ice {
                            call,
                            candidates,
                            of: pair.id,
                        },
                    ));
                }
            }
            Input::PairFailed { device } => {
                if let Some((call, pair)) = self.pair_of(&device) {
                    if let Some(r) = self.calls.get_mut(&call) {
                        r.pairs.remove(&device);
                    }
                    out.push(Self::pair_send(
                        &device,
                        Signal::PairEnd {
                            call,
                            reason: Reason::Failed,
                            of: pair.id,
                        },
                    ));
                }
            }
            Input::SfuReady { url, room } => self.sfu_ready(url, room, &mut out),
            Input::Tick => self.tick(&mut out),
        }
        out
    }

    /// The call we are in and the pair we have with `device` in it.
    fn pair_of(&self, device: &str) -> Option<(String, Pair)> {
        let call = self.mine.clone()?;
        let pair = *self.calls.get(&call)?.pairs.get(device)?;
        Some((call, pair))
    }

    fn pair_send(device: &str, signal: Signal) -> Action {
        Action::Send {
            to: Target::Device(device.to_string()),
            signal,
        }
    }

    fn all(signal: Signal) -> Action {
        Action::Send {
            to: Target::AllMembers,
            signal,
        }
    }

    /// The `at` of our next join, leave or start.
    fn stamp(&mut self) -> u64 {
        self.last_at = self.now.max(self.last_at + 1);
        self.last_at
    }

    fn me_participant(&self, video: bool, at: u64) -> Participant {
        Participant {
            owner: self.owner.clone(),
            device: self.device.clone(),
            video,
            at,
            contact: true,
        }
    }

    // ---------- starting and joining ----------

    fn start(
        &mut self,
        call: String,
        group: String,
        video: bool,
        sfu: bool,
        out: &mut Vec<Action>,
    ) {
        if self.mine.is_some() || !is_hex32(&call) || self.calls.contains_key(&call) {
            out.push(Action::Refused {
                call,
                why: Refusal::Busy,
            });
            return;
        }
        // One call per group at a time: starting while the group's bar shows one would make two.
        if self.calls.values().any(|r| r.group == group) {
            out.push(Action::Refused {
                call,
                why: Refusal::Busy,
            });
            return;
        }
        let now = self.now;
        let at = self.stamp();
        let me = self.me_participant(video, at);
        self.calls.insert(
            call.clone(),
            Running {
                call: call.clone(),
                group,
                starter: self.owner.clone(),
                video,
                sfu_capable: sfu,
                sfu: None,
                people: vec![me],
                gone: BTreeMap::new(),
                empty_since: None,
                ringing: None,
                joined_at: Some(now),
                peak: 1,
                pairs: BTreeMap::new(),
                move_requested: false,
            },
        );
        self.mine = Some(call.clone());
        out.push(Self::all(Signal::Start {
            call: call.clone(),
            video,
            sfu,
            at,
        }));
        self.changed(&call, out);
    }

    fn join(&mut self, call: &str, video: bool, out: &mut Vec<Action>) {
        let Some(r) = self.calls.get(call) else {
            return;
        };
        if self.me_in(call) {
            return;
        }
        let refusal = if self.mine.is_some() {
            Some(Refusal::Busy)
        } else if r
            .people
            .iter()
            .any(|p| p.owner == self.owner && p.device != self.device)
        {
            Some(Refusal::Elsewhere)
        } else if r.sfu.is_none() && !r.sfu_capable && r.people.len() >= MESH_MAX {
            Some(Refusal::Full)
        } else {
            None
        };
        if let Some(why) = refusal {
            out.push(Action::Refused {
                call: call.to_string(),
                why,
            });
            return;
        }
        let now = self.now;
        // Our join must sort after anything we already know, or two devices could disagree on who dials.
        let at = self.stamp();
        let me = self.me_participant(video, at);
        let r = self.calls.get_mut(call).expect("checked");
        r.people.retain(|p| p.device != me.device);
        r.people.push(me);
        r.people.sort_by(|a, b| a.key().cmp(&b.key()));
        r.joined_at = Some(now);
        r.empty_since = None;
        r.peak = r.peak.max(r.people.len());
        if r.ringing.take().is_some() {
            out.push(Action::StopRinging {
                call: call.to_string(),
            });
        }
        self.mine = Some(call.to_string());
        out.push(Self::all(Signal::Join {
            call: call.to_string(),
            video,
            at,
        }));
        self.changed(call, out);
    }

    // ---------- leaving and ending ----------

    /// We leave `call` (`reason` hangup, or full / elsewhere when we are the one that must give way).
    fn leave(&mut self, call: &str, reason: Reason, out: &mut Vec<Action>) {
        if !self.me_in(call) {
            return;
        }
        let now = self.now;
        let at = self.stamp();
        let me = self.device.clone();
        let Some(r) = self.calls.get_mut(call) else {
            self.mine = None;
            return;
        };
        r.people.retain(|p| p.device != me);
        r.gone.insert(me, at);
        let joined_at = r.joined_at.take().unwrap_or(now);
        let peak = r.peak;
        let group = r.group.clone();
        let pairs: Vec<String> = std::mem::take(&mut r.pairs).into_keys().collect();
        let last = r.people.is_empty();
        if last {
            r.empty_since = Some(now);
        }
        self.mine = None;
        for device in pairs {
            out.push(Action::Unpair {
                call: call.to_string(),
                device,
            });
        }
        out.push(Self::all(Signal::Leave {
            call: call.to_string(),
            at,
        }));
        if last {
            out.push(Self::all(Signal::End {
                call: call.to_string(),
                reason,
            }));
        }
        out.push(Action::Left {
            call: call.to_string(),
            group,
            joined_at,
            peak,
        });
        if last {
            self.finish(call, CallResult::Completed, out);
        } else {
            self.refresh(call, out);
        }
    }

    /// The call is over for everyone: forget it and tell the front end.
    fn finish(&mut self, call: &str, result: CallResult, out: &mut Vec<Action>) {
        let Some(r) = self.calls.remove(call) else {
            return;
        };
        self.mark_ended(call);
        if r.ringing.is_some() {
            out.push(Action::StopRinging {
                call: call.to_string(),
            });
            out.push(Action::Missed {
                call: call.to_string(),
                group: r.group.clone(),
                from: r.starter.clone(),
            });
            self.unanswered.push((r.starter.clone(), self.now));
        }
        if self.me_in(call) {
            self.mine = None;
            for device in r.pairs.keys() {
                out.push(Action::Unpair {
                    call: call.to_string(),
                    device: device.clone(),
                });
            }
            out.push(Action::Left {
                call: call.to_string(),
                group: r.group.clone(),
                joined_at: r.joined_at.unwrap_or(self.now),
                peak: r.peak,
            });
        }
        out.push(Action::Ended {
            call: call.to_string(),
            group: r.group.clone(),
            result,
        });
        out.push(Action::Bar {
            group: r.group,
            running: None,
        });
    }

    // ---------- receiving ----------

    fn recv(
        &mut self,
        from: Origin,
        group: String,
        signal: Signal,
        ts: u64,
        out: &mut Vec<Action>,
    ) {
        let call = signal.call().to_string();
        if self.was_ended(&call) {
            return;
        }
        match &signal {
            Signal::Start { video, sfu, at, .. } => {
                let (video, sfu, at) = (*video, *sfu, *at);
                self.on_start(from, group, call, video, sfu, at, ts, out)
            }
            _ if !self.calls.contains_key(&call) => self.defer(from, group, signal, ts),
            Signal::Join { video, at, .. } => {
                let (video, at) = (*video, *at);
                self.on_join(from, &call, video, at, out)
            }
            Signal::Leave { at, .. } => {
                let at = *at;
                self.on_leave(from, &call, at, out)
            }
            // An end or a move from someone whose join we have not seen yet waits for it: deciding now would leave
            // devices that saw things in another order disagreeing about whether the call is over.
            Signal::End { .. } if !self.may_end(&call, &from) => {
                self.defer(from, group, signal, ts)
            }
            Signal::End { .. } => self.on_end(&call, out),
            Signal::Move { .. } if !self.was_in(&call, &from.device) => {
                self.defer(from, group, signal, ts)
            }
            Signal::Move { url, room, .. } => {
                let (url, room) = (url.clone(), room.clone());
                self.on_move(&call, url, room, out)
            }
            Signal::Key {
                generation, key, ..
            } => {
                if self.me_in(&call) && self.is_person(&call, &from.device) {
                    out.push(Action::RemoteKey {
                        call,
                        device: from.device,
                        generation: *generation,
                        key: key.clone(),
                    });
                }
            }
            Signal::KeyAck { generation, .. } => {
                if self.me_in(&call) && self.is_person(&call, &from.device) {
                    out.push(Action::RemoteKeyAck {
                        call,
                        device: from.device,
                        generation: *generation,
                    });
                }
            }
            Signal::Invite { .. }
            | Signal::Answer { .. }
            | Signal::Ice { .. }
            | Signal::PairEnd { .. } => self.on_pair(from, group, signal, ts, out),
        }
    }

    fn is_person(&self, call: &str, device: &str) -> bool {
        self.calls
            .get(call)
            .is_some_and(|r| r.people.iter().any(|p| p.device == device))
    }

    /// Hold a signal that depends on something we have not seen yet.
    fn defer(&mut self, from: Origin, group: String, signal: Signal, ts: u64) {
        let kept = self.now;
        self.early
            .retain(|e| kept.saturating_sub(e.kept) < EARLY_KEEP_MS);
        if self.early.len() >= EARLY_MAX {
            self.early.pop_front();
        }
        self.early.push_back(Early {
            from,
            group,
            signal,
            ts,
            kept,
        });
    }

    /// Try again the signals held for `call`.
    fn replay(&mut self, call: &str, out: &mut Vec<Action>) {
        let (mine, rest): (Vec<Early>, Vec<Early>) = std::mem::take(&mut self.early)
            .into_iter()
            .partition(|e| e.signal.call() == call);
        self.early = rest.into();
        for e in mine {
            self.recv(e.from, e.group, e.signal, e.ts, out);
        }
    }

    #[allow(clippy::too_many_arguments)] // one signal and what the caller knows about it
    fn on_start(
        &mut self,
        from: Origin,
        group: String,
        call: String,
        video: bool,
        sfu: bool,
        at: u64,
        ts: u64,
        out: &mut Vec<Action>,
    ) {
        if self.calls.contains_key(&call) {
            return;
        }
        let now = self.now;
        // Held back by a relay, or sent long ago: not a call to ring for (ADR 0028 freshness).
        if now.abs_diff(ts) > STALE_MS {
            if !from.own {
                self.mark_ended(&call);
                out.push(Action::Missed {
                    call,
                    group,
                    from: from.owner,
                });
            }
            return;
        }
        let starter = Participant {
            owner: from.owner.clone(),
            device: from.device.clone(),
            video,
            at,
            contact: from.contact,
        };
        self.calls.insert(
            call.clone(),
            Running {
                call: call.clone(),
                group: group.clone(),
                starter: from.owner.clone(),
                video,
                sfu_capable: sfu,
                sfu: None,
                people: vec![starter],
                gone: BTreeMap::new(),
                empty_since: None,
                ringing: None,
                joined_at: None,
                peak: 1,
                pairs: BTreeMap::new(),
                move_requested: false,
            },
        );
        // Two people started a call in the same group at once: the later start gives way and joins the earlier one,
        // as in a one-to-one call. Only a call nobody else has joined yet gives way.
        if !from.own {
            if let Some(mine) = self.mine.clone() {
                let theirs = (at, call.clone());
                let glare = self.calls.get(&mine).is_some_and(|r| {
                    r.group == group
                        && r.people.len() == 1
                        && r.people[0].device == self.device
                        && (r.people[0].at, r.call.clone()) > theirs
                });
                if glare {
                    self.leave(&mine, Reason::AnsweredElsewhere, out);
                    self.changed(&call, out);
                    self.join(&call, video, out);
                    self.replay(&call, out);
                    return;
                }
            }
        }
        if !from.own && self.should_ring(&group, &from.owner) {
            if let Some(r) = self.calls.get_mut(&call) {
                r.ringing = Some(now);
            }
            self.last_ring.insert(group.clone(), now);
            out.push(Action::Ring {
                call: call.clone(),
                group,
                from: from.owner,
                video,
            });
        }
        self.changed(&call, out);
        self.replay(&call, out);
    }

    fn should_ring(&self, group: &str, from: &str) -> bool {
        let Some(g) = self.groups.get(group) else {
            return false;
        };
        let cooled = self
            .last_ring
            .get(group)
            .is_none_or(|t| self.now.saturating_sub(*t) >= RING_GROUP_COOLDOWN_MS);
        let misses = self.unanswered.iter().filter(|(o, _)| o == from).count();
        !g.muted
            && g.members <= RING_MAX_MEMBERS
            && self.mine.is_none()
            && self.ringing().is_none()
            && cooled
            && misses < MAX_UNANSWERED_PER_HOUR
    }

    fn on_join(&mut self, from: Origin, call: &str, video: bool, at: u64, out: &mut Vec<Action>) {
        let (own, device) = (from.own, from.device.clone());
        {
            let r = self.calls.get_mut(call).expect("checked");
            if r.gone.get(&device).is_some_and(|g| at <= *g) {
                return;
            }
            let old = r.people.iter().find(|p| p.device == device).map(|p| p.at);
            // A copy, or an older join that arrived after a newer one: nothing changes.
            if old.is_some_and(|o| o >= at) {
                return;
            }
            if old.is_some_and(|o| o < at) && r.pairs.remove(&device).is_some() {
                out.push(Action::Unpair {
                    call: call.to_string(),
                    device: device.clone(),
                });
            }
            r.people.retain(|p| p.device != device);
            r.people.push(Participant {
                owner: from.owner,
                device: device.clone(),
                video,
                at,
                contact: from.contact,
            });
            r.people.sort_by(|a, b| a.key().cmp(&b.key()));
            r.empty_since = None;
            r.peak = r.peak.max(r.people.len());
            // One of our own devices took the call: stop ringing here.
            if own && r.ringing.take().is_some() {
                out.push(Action::StopRinging {
                    call: call.to_string(),
                });
            }
        }
        // Both of our devices joined at once: the later one gives way.
        if own && self.me_in(call) {
            let mine_key = self
                .calls
                .get(call)
                .and_then(|r| r.people.iter().find(|p| p.device == self.device))
                .map(|p| (p.at, p.device.clone()));
            if mine_key.is_some_and(|k| k > (at, device.clone())) {
                self.leave(call, Reason::AnsweredElsewhere, out);
                out.push(Action::Refused {
                    call: call.to_string(),
                    why: Refusal::Elsewhere,
                });
                return;
            }
        }
        self.changed(call, out);
        self.replay(call, out);
    }

    fn on_leave(&mut self, from: Origin, call: &str, at: u64, out: &mut Vec<Action>) {
        let now = self.now;
        {
            let r = self.calls.get_mut(call).expect("checked");
            let before = r.people.len();
            r.people.retain(|p| p.device != from.device || p.at > at);
            let g = r.gone.entry(from.device.clone()).or_insert(0);
            *g = (*g).max(at);
            if r.people.len() == before {
                // Nothing to change, but now we know the device was in the call: what waited for that can go on.
                self.replay(call, out);
                return;
            }
            if r.people.is_empty() {
                r.empty_since = Some(now);
            }
        }
        self.changed(call, out);
        self.replay(call, out);
    }

    /// Whoever is in the call, or was (the last one out sends the end after its own leave), or nobody is.
    fn may_end(&self, call: &str, from: &Origin) -> bool {
        self.calls.get(call).is_some_and(|r| {
            r.people.is_empty()
                || r.people.iter().any(|p| p.owner == from.owner)
                || r.gone.contains_key(&from.device)
        })
    }

    /// The device is in the call, or was.
    fn was_in(&self, call: &str, device: &str) -> bool {
        self.calls.get(call).is_some_and(|r| {
            r.people.iter().any(|p| p.device == device) || r.gone.contains_key(device)
        })
    }

    fn on_end(&mut self, call: &str, out: &mut Vec<Action>) {
        let result = if self.me_in(call) {
            CallResult::Completed
        } else {
            CallResult::Missed
        };
        self.finish(call, result, out);
    }

    fn on_move(&mut self, call: &str, url: String, room: String, out: &mut Vec<Action>) {
        let r = self.calls.get_mut(call).expect("checked");
        // Only once.
        if r.sfu.is_some() {
            return;
        }
        r.sfu = Some((url.clone(), room.clone()));
        let was_in = self.me_in(call);
        if was_in {
            out.push(Action::SfuRoom {
                call: call.to_string(),
                url,
                room,
            });
        }
        self.changed(call, out);
    }

    fn on_pair(
        &mut self,
        from: Origin,
        group: String,
        signal: Signal,
        ts: u64,
        out: &mut Vec<Action>,
    ) {
        let call = signal.call().to_string();
        if from.own || !self.me_in(&call) {
            return;
        }
        let device = from.device.clone();
        let mine_key = self
            .calls
            .get(&call)
            .and_then(|r| r.people.iter().find(|p| p.device == self.device))
            .map(|p| (p.at, p.device.clone()));
        let Some(mine_key) = mine_key else {
            return;
        };
        let theirs = self
            .calls
            .get(&call)
            .and_then(|r| r.people.iter().find(|p| p.device == device))
            .map(|p| p.at);
        // The id of the pair a signal belongs to: the dialer's join stamp.
        let id = match &signal {
            Signal::Invite { at, .. } => *at,
            Signal::Answer { of, .. } | Signal::Ice { of, .. } | Signal::PairEnd { of, .. } => *of,
            _ => return,
        };
        if let Signal::Invite { .. } = &signal {
            match theirs {
                // Their `Join` has not arrived yet, or this is a newer join than the one we know: wait for it.
                None => return self.defer(from, group, signal, ts),
                Some(t) if t < id => return self.defer(from, group, signal, ts),
                // An offer of a join they have since replaced.
                Some(t) if t > id => return,
                Some(_) => {}
            }
        } else if theirs.is_none() {
            return self.defer(from, group, signal, ts);
        }
        let r = self.calls.get_mut(&call).expect("checked");
        match signal {
            Signal::Invite { video, sdp, .. } => {
                // The later joiner dials; an offer from an earlier one, one on a call server, or one for a pair
                // we already have is not part of the protocol. A newer offer replaces the pair of an older join.
                if r.sfu.is_some() || (id, device.clone()) <= mine_key {
                    return;
                }
                if let Some(old) = r.pairs.get(&device) {
                    if old.id >= id {
                        return;
                    }
                    out.push(Action::Unpair {
                        call: call.clone(),
                        device: device.clone(),
                    });
                }
                r.pairs.insert(device.clone(), Pair { dialed: false, id });
                out.push(Action::RemoteOffer {
                    call,
                    owner: from.owner,
                    device,
                    video,
                    sdp,
                    relay_only: !from.contact,
                });
            }
            Signal::Answer { sdp, .. }
                if r.pairs.get(&device).is_some_and(|p| p.dialed && p.id == id) =>
            {
                out.push(Action::RemoteAnswer { call, device, sdp });
            }
            Signal::Ice { candidates, .. } if r.pairs.get(&device).is_some_and(|p| p.id == id) => {
                out.push(Action::RemoteIce {
                    call,
                    device,
                    candidates,
                });
            }
            Signal::PairEnd { .. } if r.pairs.get(&device).is_some_and(|p| p.id == id) => {
                r.pairs.remove(&device);
                out.push(Action::Unpair { call, device });
            }
            _ => {}
        }
    }

    // ---------- the call server ----------

    fn sfu_ready(&mut self, url: String, room: String, out: &mut Vec<Action>) {
        let Some(call) = self.mine.clone() else {
            return;
        };
        let device = self.device.clone();
        let Some(r) = self.calls.get_mut(&call) else {
            return;
        };
        if r.sfu.is_some() || !r.move_requested || host_of(&r.people) != Some(device.as_str()) {
            return;
        }
        r.sfu = Some((url.clone(), room.clone()));
        out.push(Self::all(Signal::Move {
            call: call.clone(),
            url: url.clone(),
            room: room.clone(),
        }));
        out.push(Action::SfuRoom {
            call: call.clone(),
            url,
            room,
        });
        self.changed(&call, out);
    }

    // ---------- timers ----------

    fn tick(&mut self, out: &mut Vec<Action>) {
        let now = self.now;
        self.early
            .retain(|e| now.saturating_sub(e.kept) < EARLY_KEEP_MS);
        let calls: Vec<String> = self.calls.keys().cloned().collect();
        for call in calls {
            let Some(r) = self.calls.get_mut(&call) else {
                continue;
            };
            // Rang for 45 s and nobody of ours answered.
            if r.ringing.is_some_and(|t| now.saturating_sub(t) >= RING_MS) {
                r.ringing = None;
                let (group, from) = (r.group.clone(), r.starter.clone());
                out.push(Action::StopRinging { call: call.clone() });
                out.push(Action::Missed {
                    call: call.clone(),
                    group,
                    from: from.clone(),
                });
                self.unanswered.push((from, now));
            }
            // Empty for a minute: everyone's timer ends it, so nobody needs to send anything.
            let Some(r) = self.calls.get(&call) else {
                continue;
            };
            if r.empty_since
                .is_some_and(|t| now.saturating_sub(t) >= EMPTY_END_MS)
            {
                self.finish(&call, CallResult::Completed, out);
            }
        }
    }

    // ---------- keeping the pairs and the bar right ----------

    /// People changed: settle host, pairs, the move to a call server, and tell the front end.
    fn changed(&mut self, call: &str, out: &mut Vec<Action>) {
        self.refresh(call, out);
        self.overflow(call, out);
        self.request_move(call, out);
    }

    /// Pairs, participants and the bar.
    fn refresh(&mut self, call: &str, out: &mut Vec<Action>) {
        let me_in = self.me_in(call);
        let my = self.device.clone();
        let my_owner = self.owner.clone();
        let Some(r) = self.calls.get_mut(call) else {
            return;
        };
        let mine_key = r
            .people
            .iter()
            .find(|p| p.device == my)
            .map(|p| (p.at, p.device.clone()));
        // Pairs with people who left, or all of them once the call runs on a call server.
        let drop: Vec<String> = r
            .pairs
            .keys()
            .filter(|d| r.sfu.is_some() || !r.people.iter().any(|p| &p.device == *d))
            .cloned()
            .collect();
        for device in drop {
            r.pairs.remove(&device);
            out.push(Action::Unpair {
                call: call.to_string(),
                device,
            });
        }
        // We dial the people who joined before us (a mesh of up to MESH_MAX; a bigger one waits for the server).
        if me_in && r.sfu.is_none() && r.people.len() <= MESH_MAX {
            if let Some(mk) = mine_key {
                for p in r.people.clone() {
                    if p.owner != my_owner
                        && (p.at, p.device.clone()) < mk
                        && !r.pairs.contains_key(&p.device)
                    {
                        r.pairs.insert(
                            p.device.clone(),
                            Pair {
                                dialed: true,
                                id: mk.0,
                            },
                        );
                        out.push(Action::Pair {
                            call: call.to_string(),
                            owner: p.owner.clone(),
                            device: p.device.clone(),
                            dial: true,
                            relay_only: !p.contact,
                        });
                    }
                }
            }
        }
        out.push(Action::Participants {
            call: call.to_string(),
            people: r.people.clone(),
        });
        if let Some(bar) = self.bar_action(call) {
            out.push(bar);
        }
    }

    /// More than a mesh holds and no call server to go to: the people past the fourth give way.
    fn overflow(&mut self, call: &str, out: &mut Vec<Action>) {
        let Some(r) = self.calls.get(call) else {
            return;
        };
        if r.sfu.is_some() || r.sfu_capable || r.people.len() <= MESH_MAX {
            return;
        }
        let late = r
            .people
            .iter()
            .position(|p| p.device == self.device)
            .is_some_and(|i| i >= MESH_MAX);
        if late && self.me_in(call) {
            self.leave(call, Reason::Failed, out);
            out.push(Action::Refused {
                call: call.to_string(),
                why: Refusal::Full,
            });
        }
    }

    /// The host asks for a call server when a fifth person is in a mesh.
    fn request_move(&mut self, call: &str, out: &mut Vec<Action>) {
        let me_in = self.me_in(call);
        let my = self.device.clone();
        let Some(r) = self.calls.get_mut(call) else {
            return;
        };
        if me_in
            && r.sfu.is_none()
            && r.sfu_capable
            && !r.move_requested
            && r.people.len() > MESH_MAX
            && host_of(&r.people) == Some(my.as_str())
        {
            r.move_requested = true;
            out.push(Action::MoveToSfu {
                call: call.to_string(),
            });
        }
    }
}

/// The host: the device that has been in the call longest (ties: the lowest device id). Everyone computes the same.
fn host_of(people: &[Participant]) -> Option<&str> {
    people
        .iter()
        .min_by(|a, b| a.key().cmp(&b.key()))
        .map(|p| p.device.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn id(n: u8) -> String {
        format!("{n:02x}").repeat(16)
    }

    fn origin(owner: &str, device: &str) -> Origin {
        Origin {
            owner: owner.into(),
            device: device.into(),
            own: false,
            contact: true,
        }
    }

    fn recv(m: &mut Machine, now: u64, from: Origin, group: &str, signal: Signal) -> Vec<Action> {
        m.step(
            now,
            Input::Recv {
                from,
                group: group.into(),
                signal,
                ts: now,
            },
        )
    }

    fn machine(owner: &str, device: &str, members: usize) -> Machine {
        let mut m = Machine::new(owner, device);
        m.step(
            NOW,
            Input::Group {
                group: "g".into(),
                members,
                muted: false,
            },
        );
        m
    }

    fn start_sig(call: &str, at: u64) -> Signal {
        Signal::Start {
            call: call.into(),
            video: false,
            sfu: false,
            at,
        }
    }

    fn join_sig(call: &str, at: u64) -> Signal {
        Signal::Join {
            call: call.into(),
            video: false,
            at,
        }
    }

    fn has(actions: &[Action], f: impl Fn(&Action) -> bool) -> bool {
        actions.iter().any(f)
    }

    #[test]
    fn signals_are_validated() {
        let call = id(1);
        let ok = Signal::Join {
            call: call.clone(),
            video: true,
            at: 5,
        };
        assert_eq!(Signal::parse(&ok.to_json()).unwrap(), ok);
        assert!(Signal::parse("not json").is_err());
        assert!(Signal::parse(
            &Signal::Join {
                call: "short".into(),
                video: false,
                at: 1
            }
            .to_json()
        )
        .is_err());
        let big = Signal::Invite {
            call: call.clone(),
            video: false,
            sdp: "x".repeat(MAX_SDP + 1),
            at: 1,
        };
        assert!(Signal::parse(&big.to_json()).is_err());
        let empty_sdp = Signal::Answer {
            call: call.clone(),
            sdp: String::new(),
            of: 1,
        };
        assert!(Signal::parse(&empty_sdp.to_json()).is_err());
        let many = Signal::Ice {
            call: call.clone(),
            candidates: vec!["c".into(); MAX_CANDIDATES + 1],
            of: 1,
        };
        assert!(Signal::parse(&many.to_json()).is_err());
        let none = Signal::Ice {
            call: call.clone(),
            candidates: vec![],
            of: 1,
        };
        assert!(Signal::parse(&none.to_json()).is_err());
        let bad_key = Signal::Key {
            call: call.clone(),
            generation: 1,
            key: "ab".into(),
        };
        assert!(Signal::parse(&bad_key.to_json()).is_err());
        let good_key = Signal::Key {
            call: call.clone(),
            generation: 1,
            key: "ab".repeat(32),
        };
        assert!(Signal::parse(&good_key.to_json()).is_ok());
        for url in ["https://x.example", "wss://a b", "wss://ok.example:443/sfu"] {
            let mv = Signal::Move {
                call: call.clone(),
                url: url.into(),
                room: "abc123".into(),
            };
            assert_eq!(
                Signal::parse(&mv.to_json()).is_ok(),
                url.starts_with("wss://ok")
            );
        }
        let bad_room = Signal::Move {
            call,
            url: "wss://ok.example".into(),
            room: "no spaces".into(),
        };
        assert!(Signal::parse(&bad_room.to_json()).is_err());
    }

    #[test]
    fn a_member_who_is_not_a_contact_starts_a_call_and_it_rings() {
        let mut m = machine("bob", "dev-b", 3);
        let mut from = origin("ann", "dev-a");
        from.contact = false;
        let call = id(1);
        let a = recv(&mut m, NOW, from, "g", start_sig(&call, NOW));
        assert!(has(
            &a,
            |x| matches!(x, Action::Ring { from, .. } if from == "ann")
        ));
        assert_eq!(m.ringing(), Some(call.as_str()));
        let bar = m.bar("g").unwrap();
        assert_eq!((bar.people, bar.joined), (1, false));
    }

    #[test]
    fn starting_sends_to_everyone_and_shows_the_bar() {
        let mut m = machine("ann", "dev-a", 3);
        let call = id(1);
        let a = m.step(
            NOW,
            Input::Start {
                call: call.clone(),
                group: "g".into(),
                video: false,
                sfu: false,
            },
        );
        assert!(has(&a, |x| matches!(
            x,
            Action::Send { to: Target::AllMembers, signal: Signal::Start { at, .. } } if *at == NOW
        )));
        assert_eq!(m.current(), Some(call.as_str()));
        assert!(m.bar("g").unwrap().joined);
        // A second start while in a call is refused.
        let b = m.step(
            NOW,
            Input::Start {
                call: id(2),
                group: "h".into(),
                video: false,
                sfu: false,
            },
        );
        assert!(has(&b, |x| matches!(
            x,
            Action::Refused {
                why: Refusal::Busy,
                ..
            }
        )));
    }

    #[test]
    fn joining_stops_the_ringing_and_dials_the_earlier_people() {
        let mut m = machine("bob", "dev-b", 3);
        let call = id(1);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        let a = m.step(
            NOW + 5_000,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        assert!(has(&a, |x| matches!(x, Action::StopRinging { .. })));
        assert!(has(&a, |x| matches!(
            x,
            Action::Send {
                signal: Signal::Join { .. },
                ..
            }
        )));
        // The later joiner dials the starter.
        assert!(has(&a, |x| matches!(
            x,
            Action::Pair { device, dial: true, relay_only: false, .. } if device == "dev-a"
        )));
        assert_eq!(m.ringing(), None);
    }

    #[test]
    fn a_pair_with_someone_who_is_not_a_contact_is_relay_only() {
        let mut m = machine("bob", "dev-b", 3);
        let call = id(1);
        let mut ann = origin("ann", "dev-a");
        ann.contact = false;
        recv(&mut m, NOW, ann, "g", start_sig(&call, NOW));
        let a = m.step(NOW + 1_000, Input::Join { call, video: false });
        assert!(has(&a, |x| matches!(
            x,
            Action::Pair {
                dial: true,
                relay_only: true,
                ..
            }
        )));
    }

    #[test]
    fn the_earlier_joiner_waits_for_the_offer_and_the_later_one_dials() {
        let call = id(1);
        let mut ann = machine("ann", "dev-a", 3);
        ann.step(
            NOW,
            Input::Start {
                call: call.clone(),
                group: "g".into(),
                video: false,
                sfu: false,
            },
        );
        // Bob joins later: Ann does not dial him.
        let a = recv(
            &mut ann,
            NOW + 2_000,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 2_000),
        );
        assert!(!has(&a, |x| matches!(x, Action::Pair { .. })));
        // His offer arrives and is surfaced, relay-only follows his contact flag.
        let b = recv(
            &mut ann,
            NOW + 2_100,
            origin("bob", "dev-b"),
            "g",
            Signal::Invite {
                call: call.clone(),
                video: false,
                sdp: "offer".into(),
                at: NOW + 2_000,
            },
        );
        assert!(has(
            &b,
            |x| matches!(x, Action::RemoteOffer { device, .. } if device == "dev-b")
        ));
        // An offer from the earlier joiner to the later one is not part of the protocol.
        let mut bob = machine("bob", "dev-b", 3);
        recv(
            &mut bob,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        bob.step(
            NOW + 2_000,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        let c = recv(
            &mut bob,
            NOW + 2_100,
            origin("ann", "dev-a"),
            "g",
            Signal::Invite {
                call,
                video: false,
                sdp: "wrong way".into(),
                at: NOW,
            },
        );
        assert!(!has(&c, |x| matches!(x, Action::RemoteOffer { .. })));
    }

    #[test]
    fn a_pair_signal_before_the_join_is_held_and_replayed() {
        let call = id(1);
        let mut ann = machine("ann", "dev-a", 3);
        ann.step(
            NOW,
            Input::Start {
                call: call.clone(),
                group: "g".into(),
                video: false,
                sfu: false,
            },
        );
        let held = recv(
            &mut ann,
            NOW + 2_100,
            origin("bob", "dev-b"),
            "g",
            Signal::Invite {
                call: call.clone(),
                video: false,
                sdp: "offer".into(),
                at: NOW + 2_000,
            },
        );
        assert!(held.is_empty());
        let a = recv(
            &mut ann,
            NOW + 2_200,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 2_000),
        );
        assert!(has(&a, |x| matches!(x, Action::RemoteOffer { .. })));
    }

    #[test]
    fn a_join_before_the_start_is_held_and_replayed() {
        let call = id(1);
        let mut m = machine("cy", "dev-c", 3);
        let held = recv(
            &mut m,
            NOW,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 1),
        );
        assert!(held.is_empty());
        let a = recv(
            &mut m,
            NOW + 10,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        let people = a.iter().rev().find_map(|x| match x {
            Action::Participants { people, .. } => Some(people.len()),
            _ => None,
        });
        assert_eq!(people, Some(2));
    }

    #[test]
    fn a_reordered_join_after_the_leave_is_ignored() {
        let call = id(1);
        let mut m = machine("cy", "dev-c", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        recv(
            &mut m,
            NOW + 10,
            origin("bob", "dev-b"),
            "g",
            Signal::Leave {
                call: call.clone(),
                at: NOW + 500,
            },
        );
        recv(
            &mut m,
            NOW + 20,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 100),
        );
        assert_eq!(m.people(&call).len(), 1);
        // A real rejoin is later than the leave and counts.
        recv(
            &mut m,
            NOW + 30,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 900),
        );
        assert_eq!(m.people(&call).len(), 2);
    }

    #[test]
    fn our_other_device_taking_the_call_stops_the_ringing_and_blocks_a_second_join() {
        let call = id(1);
        let mut m = machine("bob", "dev-b1", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        assert_eq!(m.ringing(), Some(call.as_str()));
        let mut own = origin("bob", "dev-b2");
        own.own = true;
        let a = recv(&mut m, NOW + 100, own, "g", join_sig(&call, NOW + 100));
        assert!(has(&a, |x| matches!(x, Action::StopRinging { .. })));
        assert!(m.bar("g").unwrap().elsewhere);
        let b = m.step(NOW + 200, Input::Join { call, video: false });
        assert!(has(&b, |x| matches!(
            x,
            Action::Refused {
                why: Refusal::Elsewhere,
                ..
            }
        )));
        assert_eq!(m.current(), None);
    }

    #[test]
    fn two_of_our_devices_joining_at_once_leave_one() {
        let call = id(1);
        let mut m = machine("bob", "dev-b2", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        m.step(
            NOW + 100,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        // The other device joined slightly earlier by its own clock: this one (later) gives way.
        let mut own = origin("bob", "dev-b1");
        own.own = true;
        let a = recv(&mut m, NOW + 150, own, "g", join_sig(&call, NOW + 90));
        assert!(has(&a, |x| matches!(x, Action::Left { .. })));
        assert_eq!(m.current(), None);
    }

    #[test]
    fn a_stale_start_is_a_missed_call_not_a_ring() {
        let mut m = machine("bob", "dev-b", 3);
        let call = id(1);
        let a = m.step(
            NOW + STALE_MS + 1,
            Input::Recv {
                from: origin("ann", "dev-a"),
                group: "g".into(),
                signal: start_sig(&call, NOW),
                ts: NOW,
            },
        );
        assert!(has(&a, |x| matches!(x, Action::Missed { .. })));
        assert!(!has(&a, |x| matches!(x, Action::Ring { .. })));
        assert_eq!(m.bar("g"), None);
        // A copy of the same start is not rung for later either.
        let b = recv(
            &mut m,
            NOW + STALE_MS + 2,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW + STALE_MS + 2),
        );
        assert!(b.is_empty());
    }

    #[test]
    fn a_call_that_ended_is_not_rung_for_again() {
        let call = id(1);
        let mut m = machine("bob", "dev-b", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        recv(
            &mut m,
            NOW + 10,
            origin("ann", "dev-a"),
            "g",
            Signal::End {
                call: call.clone(),
                reason: Reason::Hangup,
            },
        );
        assert_eq!(m.running(), 0);
        let a = recv(
            &mut m,
            NOW + 20,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW + 20),
        );
        assert!(a.is_empty());
    }

    #[test]
    fn two_starts_at_once_the_later_one_joins_the_earlier() {
        let (c1, c2) = (id(1), id(2));
        // Bob started at 200, Ann at 100: Bob's solo call gives way and he joins Ann's.
        let mut bob = machine("bob", "dev-b", 3);
        bob.step(
            NOW + 200,
            Input::Start {
                call: c2.clone(),
                group: "g".into(),
                video: false,
                sfu: false,
            },
        );
        let a = recv(
            &mut bob,
            NOW + 250,
            origin("ann", "dev-a"),
            "g",
            start_sig(&c1, NOW + 100),
        );
        assert!(has(
            &a,
            |x| matches!(x, Action::Left { call, .. } if *call == c2)
        ));
        assert!(has(&a, |x| matches!(
            x,
            Action::Send { signal: Signal::Join { call, .. }, .. } if *call == c1
        )));
        assert!(has(&a, |x| matches!(x, Action::Pair { dial: true, .. })));
        assert_eq!(bob.current(), Some(c1.as_str()));
        // Ann, whose start is earlier, does not give way to Bob's.
        let mut ann = machine("ann", "dev-a", 3);
        ann.step(
            NOW + 100,
            Input::Start {
                call: c1.clone(),
                group: "g".into(),
                video: false,
                sfu: false,
            },
        );
        recv(
            &mut ann,
            NOW + 260,
            origin("bob", "dev-b"),
            "g",
            start_sig(&c2, NOW + 200),
        );
        assert_eq!(ann.current(), Some(c1.as_str()));
    }

    #[test]
    fn a_duplicate_start_does_not_ring_twice() {
        let call = id(1);
        let mut m = machine("bob", "dev-b", 3);
        let a = recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        let b = recv(
            &mut m,
            NOW + 1,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        assert!(has(&a, |x| matches!(x, Action::Ring { .. })));
        assert!(b.is_empty());
    }

    #[test]
    fn big_groups_and_muted_groups_get_the_bar_without_ringing() {
        for (members, muted) in [(RING_MAX_MEMBERS + 1, false), (3, true)] {
            let mut m = Machine::new("bob", "dev-b");
            m.step(
                NOW,
                Input::Group {
                    group: "g".into(),
                    members,
                    muted,
                },
            );
            let a = recv(
                &mut m,
                NOW,
                origin("ann", "dev-a"),
                "g",
                start_sig(&id(1), NOW),
            );
            assert!(!has(&a, |x| matches!(x, Action::Ring { .. })));
            assert!(m.bar("g").is_some());
        }
        // A group nothing is known about does not ring either.
        let mut m = Machine::new("bob", "dev-b");
        let a = recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&id(1), NOW),
        );
        assert!(!has(&a, |x| matches!(x, Action::Ring { .. })));
    }

    #[test]
    fn one_ring_per_group_in_ten_minutes() {
        let mut m = machine("bob", "dev-b", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&id(1), NOW),
        );
        recv(
            &mut m,
            NOW + 1_000,
            origin("ann", "dev-a"),
            "g",
            Signal::End {
                call: id(1),
                reason: Reason::Hangup,
            },
        );
        let a = recv(
            &mut m,
            NOW + 60_000,
            origin("ann", "dev-a"),
            "g",
            start_sig(&id(2), NOW + 60_000),
        );
        assert!(!has(&a, |x| matches!(x, Action::Ring { .. })));
        assert!(m.bar("g").is_some());
        recv(
            &mut m,
            NOW + 61_000,
            origin("ann", "dev-a"),
            "g",
            Signal::End {
                call: id(2),
                reason: Reason::Hangup,
            },
        );
        let later = NOW + RING_GROUP_COOLDOWN_MS + 5_000;
        let b = recv(
            &mut m,
            later,
            origin("ann", "dev-a"),
            "g",
            start_sig(&id(3), later),
        );
        assert!(has(&b, |x| matches!(x, Action::Ring { .. })));
    }

    #[test]
    fn three_unanswered_rings_in_an_hour_silence_the_fourth() {
        let mut m = machine("bob", "dev-b", 3);
        let mut t = NOW;
        for n in 1..=MAX_UNANSWERED_PER_HOUR as u8 {
            let a = recv(&mut m, t, origin("ann", "dev-a"), "g", start_sig(&id(n), t));
            assert!(has(&a, |x| matches!(x, Action::Ring { .. })), "ring {n}");
            // Nobody answers: the ring times out.
            t += RING_MS;
            let b = m.step(t, Input::Tick);
            assert!(has(&b, |x| matches!(x, Action::Missed { .. })));
            recv(
                &mut m,
                t,
                origin("ann", "dev-a"),
                "g",
                Signal::End {
                    call: id(n),
                    reason: Reason::Hangup,
                },
            );
            t += RING_GROUP_COOLDOWN_MS;
        }
        let a = recv(&mut m, t, origin("ann", "dev-a"), "g", start_sig(&id(9), t));
        assert!(!has(&a, |x| matches!(x, Action::Ring { .. })));
        assert!(m.bar("g").is_some());
    }

    #[test]
    fn a_ring_nobody_answers_times_out_as_missed() {
        let call = id(1);
        let mut m = machine("bob", "dev-b", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        assert!(m.step(NOW + RING_MS - 1, Input::Tick).is_empty() || m.ringing().is_some());
        let a = m.step(NOW + RING_MS, Input::Tick);
        assert!(has(&a, |x| matches!(x, Action::StopRinging { .. })));
        assert!(has(&a, |x| matches!(x, Action::Missed { .. })));
        assert_eq!(m.ringing(), None);
        // The call goes on in the bar.
        assert!(m.bar("g").is_some());
    }

    #[test]
    fn dismissing_stops_the_ringing_only() {
        let call = id(1);
        let mut m = machine("bob", "dev-b", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        let a = m.step(NOW + 1, Input::Dismiss { call });
        assert!(has(&a, |x| matches!(x, Action::StopRinging { .. })));
        assert!(m.bar("g").is_some());
    }

    #[test]
    fn busy_in_another_call_means_no_ring() {
        let mut m = machine("bob", "dev-b", 3);
        m.step(
            NOW,
            Input::Group {
                group: "h".into(),
                members: 3,
                muted: false,
            },
        );
        m.step(
            NOW,
            Input::Start {
                call: id(1),
                group: "h".into(),
                video: false,
                sfu: false,
            },
        );
        let a = recv(
            &mut m,
            NOW + 5,
            origin("ann", "dev-a"),
            "g",
            start_sig(&id(2), NOW + 5),
        );
        assert!(!has(&a, |x| matches!(x, Action::Ring { .. })));
        assert!(m.bar("g").is_some());
        let b = m.step(
            NOW + 6,
            Input::Join {
                call: id(2),
                video: false,
            },
        );
        assert!(has(&b, |x| matches!(
            x,
            Action::Refused {
                why: Refusal::Busy,
                ..
            }
        )));
    }

    /// Four people are in; a fifth cannot join a mesh whose starter has no call server.
    fn four_in(m: &mut Machine, call: &str, sfu: bool) {
        recv(
            m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            Signal::Start {
                call: call.into(),
                video: false,
                sfu,
                at: NOW,
            },
        );
        for (i, (o, d)) in [("bob", "dev-b"), ("cy", "dev-c"), ("di", "dev-d")]
            .into_iter()
            .enumerate()
        {
            recv(
                m,
                NOW + 10,
                origin(o, d),
                "g",
                join_sig(call, NOW + 100 * (i as u64 + 1)),
            );
        }
    }

    #[test]
    fn a_fifth_person_is_refused_when_there_is_no_call_server() {
        let call = id(1);
        let mut m = machine("ed", "dev-e", 6);
        four_in(&mut m, &call, false);
        assert_eq!(m.people(&call).len(), 4);
        let a = m.step(NOW + 1_000, Input::Join { call, video: false });
        assert!(has(&a, |x| matches!(
            x,
            Action::Refused {
                why: Refusal::Full,
                ..
            }
        )));
        assert_eq!(m.current(), None);
    }

    #[test]
    fn two_late_joiners_at_once_leave_the_last_out_when_there_is_no_call_server() {
        // The fifth joined at the same time as the fourth, so neither saw a full call: the later one gives way.
        let call = id(1);
        let mut m = machine("ed", "dev-e", 6);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        for (o, d, at) in [("bob", "dev-b", 100), ("cy", "dev-c", 200)] {
            recv(
                &mut m,
                NOW + 10,
                origin(o, d),
                "g",
                join_sig(&call, NOW + at),
            );
        }
        m.step(
            NOW + 250,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        let a = recv(
            &mut m,
            NOW + 260,
            origin("di", "dev-d"),
            "g",
            join_sig(&call, NOW + 240),
        );
        assert!(has(&a, |x| matches!(
            x,
            Action::Refused {
                why: Refusal::Full,
                ..
            }
        )));
        assert_eq!(m.current(), None);
        assert_eq!(m.people(&call).len(), 4, "the four earlier joiners stay");
    }

    #[test]
    fn the_host_asks_for_a_call_server_when_a_fifth_person_joins_and_everyone_moves() {
        let call = id(1);
        // Ann hosts.
        let mut ann = machine("ann", "dev-a", 6);
        ann.step(
            NOW,
            Input::Start {
                call: call.clone(),
                group: "g".into(),
                video: false,
                sfu: true,
            },
        );
        for (i, (o, d)) in [("bob", "dev-b"), ("cy", "dev-c"), ("di", "dev-d")]
            .into_iter()
            .enumerate()
        {
            let a = recv(
                &mut ann,
                NOW + 10,
                origin(o, d),
                "g",
                join_sig(&call, NOW + 100 * (i as u64 + 1)),
            );
            assert!(!has(&a, |x| matches!(x, Action::MoveToSfu { .. })));
        }
        let a = recv(
            &mut ann,
            NOW + 20,
            origin("ed", "dev-e"),
            "g",
            join_sig(&call, NOW + 500),
        );
        assert!(has(&a, |x| matches!(x, Action::MoveToSfu { .. })));
        // Asked once only.
        let again = recv(
            &mut ann,
            NOW + 30,
            origin("fy", "dev-f"),
            "g",
            join_sig(&call, NOW + 600),
        );
        assert!(!has(&again, |x| matches!(x, Action::MoveToSfu { .. })));
        // The server is ready: everyone is told, and the pairs go.
        let b = ann.step(
            NOW + 40,
            Input::SfuReady {
                url: "wss://sfu.example/r".into(),
                room: "abc".into(),
            },
        );
        assert!(has(&b, |x| matches!(
            x,
            Action::Send {
                signal: Signal::Move { .. },
                ..
            }
        )));
        assert!(has(&b, |x| matches!(x, Action::SfuRoom { .. })));
        assert!(ann.bar("g").unwrap().sfu);
        // Another device receiving the move connects to the server.
        let mut bob = machine("bob", "dev-b", 6);
        recv(
            &mut bob,
            NOW,
            origin("ann", "dev-a"),
            "g",
            Signal::Start {
                call: call.clone(),
                video: false,
                sfu: true,
                at: NOW,
            },
        );
        bob.step(
            NOW + 100,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        recv(
            &mut bob,
            NOW + 120,
            origin("cy", "dev-c"),
            "g",
            join_sig(&call, NOW + 200),
        );
        let c = recv(
            &mut bob,
            NOW + 130,
            origin("ann", "dev-a"),
            "g",
            Signal::Move {
                call: call.clone(),
                url: "wss://sfu.example/r".into(),
                room: "abc".into(),
            },
        );
        assert!(has(&c, |x| matches!(x, Action::SfuRoom { .. })));
        assert!(has(&c, |x| matches!(x, Action::Unpair { .. })));
        // A move from someone who is not in the call is ignored.
        let mut cy = machine("cy", "dev-c", 6);
        recv(
            &mut cy,
            NOW,
            origin("ann", "dev-a"),
            "g",
            Signal::Start {
                call: call.clone(),
                video: false,
                sfu: true,
                at: NOW,
            },
        );
        let d = recv(
            &mut cy,
            NOW + 1,
            origin("mallory", "dev-m"),
            "g",
            Signal::Move {
                call,
                url: "wss://evil.example".into(),
                room: "x".into(),
            },
        );
        assert!(!has(&d, |x| matches!(x, Action::SfuRoom { .. })));
        assert!(!cy.bar("g").unwrap().sfu);
    }

    #[test]
    fn the_host_is_whoever_has_been_in_longest_and_changes_when_they_leave() {
        let call = id(1);
        let mut ann = machine("ann", "dev-a", 6);
        ann.step(
            NOW,
            Input::Start {
                call: call.clone(),
                group: "g".into(),
                video: false,
                sfu: true,
            },
        );
        recv(
            &mut ann,
            NOW + 10,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 100),
        );
        recv(
            &mut ann,
            NOW + 10,
            origin("cy", "dev-c"),
            "g",
            join_sig(&call, NOW + 200),
        );
        // A fifth and fourth person: Bob (now the longest in, once Ann leaves) is asked for the server.
        let mut bob = machine("bob", "dev-b", 6);
        recv(
            &mut bob,
            NOW,
            origin("ann", "dev-a"),
            "g",
            Signal::Start {
                call: call.clone(),
                video: false,
                sfu: true,
                at: NOW,
            },
        );
        bob.step(
            NOW + 100,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        recv(
            &mut bob,
            NOW + 110,
            origin("cy", "dev-c"),
            "g",
            join_sig(&call, NOW + 200),
        );
        recv(
            &mut bob,
            NOW + 110,
            origin("di", "dev-d"),
            "g",
            join_sig(&call, NOW + 300),
        );
        recv(
            &mut bob,
            NOW + 120,
            origin("ann", "dev-a"),
            "g",
            Signal::Leave {
                call: call.clone(),
                at: NOW + 400,
            },
        );
        let a = recv(
            &mut bob,
            NOW + 130,
            origin("ed", "dev-e"),
            "g",
            join_sig(&call, NOW + 500),
        );
        assert!(
            !has(&a, |x| matches!(x, Action::MoveToSfu { .. })),
            "four people are still a mesh"
        );
        let b = recv(
            &mut bob,
            NOW + 140,
            origin("fy", "dev-f"),
            "g",
            join_sig(&call, NOW + 600),
        );
        assert!(
            has(&b, |x| matches!(x, Action::MoveToSfu { .. })),
            "bob hosts now"
        );
    }

    #[test]
    fn leaving_unpairs_and_the_last_one_out_ends_the_call() {
        let call = id(1);
        let mut ann = machine("ann", "dev-a", 3);
        ann.step(
            NOW,
            Input::Start {
                call: call.clone(),
                group: "g".into(),
                video: false,
                sfu: false,
            },
        );
        recv(
            &mut ann,
            NOW + 5,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 100),
        );
        let a = recv(
            &mut ann,
            NOW + 6,
            origin("bob", "dev-b"),
            "g",
            Signal::Leave {
                call: call.clone(),
                at: NOW + 200,
            },
        );
        assert_eq!(ann.people(&call).len(), 1);
        assert!(
            !has(&a, |x| matches!(x, Action::Ended { .. })),
            "Ann is still in it"
        );
        let b = ann.step(NOW + 300, Input::Leave);
        assert!(has(&b, |x| matches!(
            x,
            Action::Send {
                signal: Signal::Leave { .. },
                ..
            }
        )));
        assert!(has(&b, |x| matches!(
            x,
            Action::Send {
                signal: Signal::End { .. },
                ..
            }
        )));
        assert!(has(&b, |x| matches!(x, Action::Left { .. })));
        assert!(has(&b, |x| matches!(x, Action::Ended { .. })));
        assert_eq!(ann.current(), None);
        assert_eq!(ann.running(), 0);
    }

    #[test]
    fn leaving_with_others_still_in_keeps_the_call() {
        let call = id(1);
        let mut ann = machine("ann", "dev-a", 3);
        ann.step(
            NOW,
            Input::Start {
                call: call.clone(),
                group: "g".into(),
                video: false,
                sfu: false,
            },
        );
        recv(
            &mut ann,
            NOW + 5,
            origin("bob", "dev-b"),
            "g",
            join_sig(&call, NOW + 100),
        );
        let b = ann.step(NOW + 300, Input::Leave);
        assert!(has(&b, |x| matches!(x, Action::Left { .. })));
        assert!(!has(&b, |x| matches!(
            x,
            Action::Send {
                signal: Signal::End { .. },
                ..
            }
        )));
        assert!(!has(&b, |x| matches!(x, Action::Ended { .. })));
        assert!(ann.bar("g").is_some());
        // Ann can come back.
        let c = ann.step(NOW + 400, Input::Join { call, video: false });
        assert!(has(&c, |x| matches!(x, Action::Pair { dial: true, .. })));
    }

    #[test]
    fn an_empty_call_ends_on_its_own_after_a_minute() {
        let call = id(1);
        let mut m = machine("bob", "dev-b", 3);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        recv(
            &mut m,
            NOW + 10,
            origin("ann", "dev-a"),
            "g",
            Signal::Leave {
                call: call.clone(),
                at: NOW + 50,
            },
        );
        assert!(m
            .step(NOW + EMPTY_END_MS - 100, Input::Tick)
            .iter()
            .all(|a| !matches!(a, Action::Ended { .. })));
        let a = m.step(NOW + 10 + EMPTY_END_MS, Input::Tick);
        assert!(has(&a, |x| matches!(x, Action::Ended { .. })));
        assert!(has(&a, |x| matches!(x, Action::Bar { running: None, .. })));
    }

    #[test]
    fn an_end_from_someone_outside_the_call_is_ignored() {
        let call = id(1);
        let mut m = machine("bob", "dev-b", 5);
        recv(
            &mut m,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        let a = recv(
            &mut m,
            NOW + 5,
            origin("mallory", "dev-m"),
            "g",
            Signal::End {
                call: call.clone(),
                reason: Reason::Hangup,
            },
        );
        assert!(a.is_empty());
        assert!(m.bar("g").is_some());
        let b = recv(
            &mut m,
            NOW + 6,
            origin("ann", "dev-a"),
            "g",
            Signal::End {
                call,
                reason: Reason::Hangup,
            },
        );
        assert!(has(&b, |x| matches!(x, Action::Ended { .. })));
    }

    #[test]
    fn answers_and_candidates_only_flow_on_pairs_we_made() {
        let call = id(1);
        let mut bob = machine("bob", "dev-b", 3);
        recv(
            &mut bob,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        bob.step(
            NOW + 100,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        // We dialed Ann: her answer and candidates are surfaced.
        let a = recv(
            &mut bob,
            NOW + 150,
            origin("ann", "dev-a"),
            "g",
            Signal::Answer {
                call: call.clone(),
                sdp: "ans".into(),
                of: NOW + 100,
            },
        );
        assert!(has(&a, |x| matches!(x, Action::RemoteAnswer { .. })));
        let b = recv(
            &mut bob,
            NOW + 160,
            origin("ann", "dev-a"),
            "g",
            Signal::Ice {
                call: call.clone(),
                candidates: vec!["c".into()],
                of: NOW + 100,
            },
        );
        assert!(has(&b, |x| matches!(x, Action::RemoteIce { .. })));
        // An answer from a participant we did not dial is not.
        recv(
            &mut bob,
            NOW + 170,
            origin("cy", "dev-c"),
            "g",
            join_sig(&call, NOW + 200),
        );
        let c = recv(
            &mut bob,
            NOW + 180,
            origin("cy", "dev-c"),
            "g",
            Signal::Answer {
                call: call.clone(),
                sdp: "ans".into(),
                of: NOW + 100,
            },
        );
        assert!(c.is_empty());
        // Pair signals from someone not in the call are held, never surfaced.
        let d = recv(
            &mut bob,
            NOW + 190,
            origin("mallory", "dev-m"),
            "g",
            Signal::Invite {
                call,
                video: false,
                sdp: "x".into(),
                at: NOW + 190,
            },
        );
        assert!(d.is_empty());
    }

    #[test]
    fn pair_signals_after_we_left_are_dropped() {
        let call = id(1);
        let mut bob = machine("bob", "dev-b", 3);
        recv(
            &mut bob,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        bob.step(
            NOW + 100,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        bob.step(NOW + 200, Input::Leave);
        let a = recv(
            &mut bob,
            NOW + 210,
            origin("ann", "dev-a"),
            "g",
            Signal::Ice {
                call,
                candidates: vec!["c".into()],
                of: NOW + 100,
            },
        );
        assert!(a.is_empty());
    }

    #[test]
    fn local_offers_and_answers_go_to_one_device() {
        let call = id(1);
        let mut bob = machine("bob", "dev-b", 3);
        recv(
            &mut bob,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        bob.step(NOW + 100, Input::Join { call, video: false });
        let a = bob.step(
            NOW + 110,
            Input::LocalOffer {
                device: "dev-a".into(),
                video: false,
                sdp: "o".into(),
            },
        );
        assert!(matches!(
            &a[0],
            Action::Send { to: Target::Device(d), signal: Signal::Invite { .. } } if d == "dev-a"
        ));
        let b = bob.step(
            NOW + 120,
            Input::PairFailed {
                device: "dev-a".into(),
            },
        );
        assert!(matches!(
            &b[0],
            Action::Send {
                signal: Signal::PairEnd {
                    reason: Reason::Failed,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn keys_are_passed_on_only_from_people_in_the_call_we_are_in() {
        let call = id(1);
        let mut bob = machine("bob", "dev-b", 3);
        recv(
            &mut bob,
            NOW,
            origin("ann", "dev-a"),
            "g",
            start_sig(&call, NOW),
        );
        let key = Signal::Key {
            call: call.clone(),
            generation: 1,
            key: "ab".repeat(32),
        };
        // Not in the call yet: nothing.
        assert!(recv(&mut bob, NOW + 1, origin("ann", "dev-a"), "g", key.clone()).is_empty());
        bob.step(
            NOW + 100,
            Input::Join {
                call: call.clone(),
                video: false,
            },
        );
        let a = recv(
            &mut bob,
            NOW + 110,
            origin("ann", "dev-a"),
            "g",
            key.clone(),
        );
        assert!(has(&a, |x| matches!(
            x,
            Action::RemoteKey { generation: 1, .. }
        )));
        assert!(recv(&mut bob, NOW + 120, origin("mallory", "dev-m"), "g", key).is_empty());
    }
}
