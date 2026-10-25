//! Randomized tests of the call state machine (ADR 0028): two people, each with one device, call, answer, decline and
//! hang up at random while the signals between them arrive late, out of order and twice. Whatever happens, the rules
//! must hold: a machine never goes quiet without telling the front end the call ended, a ringing phone always stops
//! ringing, a call is never rung for twice, and once everything has settled the two sides agree (both idle, or both on
//! the same call). Each seed is a different history; a failure names its seed so it can be replayed.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use spike_core::call::*;
use std::collections::HashSet;

const SEEDS: u64 = 400;
const STEPS: usize = 220;

struct Side {
    m: Machine,
    /// How this side's own contact names the other person, and this side's device id.
    peer: &'static str,
    device: &'static str,
    /// The call the front end is ringing for, if any.
    ringing: Option<String>,
    rang: HashSet<String>,
    ended: HashSet<String>,
}

struct Wire {
    at: u64,
    to: usize,
    signal: Signal,
    sent: u64,
}

struct Sim {
    sides: [Side; 2],
    wire: Vec<Wire>,
    now: u64,
    rng: StdRng,
    seed: u64,
}

fn hex32(rng: &mut StdRng) -> String {
    (0..16)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect()
}

impl Sim {
    fn new(seed: u64) -> Self {
        let side = |peer, device| Side {
            m: Machine::new(),
            peer,
            device,
            ringing: None,
            rang: HashSet::new(),
            ended: HashSet::new(),
        };
        Self {
            sides: [side("bob", "dev-a"), side("alice", "dev-b")],
            wire: vec![],
            now: 1_000_000,
            rng: StdRng::seed_from_u64(seed),
            seed,
        }
    }

    /// Run one input on one side, check what it did, and put what it sent on the wire.
    fn step(&mut self, who: usize, input: Input) {
        let seed = self.seed;
        let mut take_at_once = false;
        let before = self.sides[who].m.current().map(str::to_string);
        let shown = format!("{input:?}");
        let actions = self.sides[who].m.step(self.now, input);
        if std::env::var("SIM_TRACE").is_ok() {
            eprintln!(
                "t={} side {who} <- {}
        -> {actions:?}",
                self.now,
                shown.chars().take(160).collect::<String>()
            );
        }
        let after = self.sides[who].m.current().map(str::to_string);
        for a in &actions {
            match a {
                Action::Send { to, signal } => {
                    // The other person has one device; our own other devices do not exist here.
                    let reaches = match to {
                        Target::PeerAll => true,
                        Target::PeerDevice(d) => d == self.sides[1 - who].device,
                        Target::OwnOthers => false,
                    };
                    if reaches {
                        let copies = if self.rng.gen_ratio(1, 8) { 2 } else { 1 };
                        for _ in 0..copies {
                            let delay = self.rng.gen_range(0..2500);
                            self.wire.push(Wire {
                                at: self.now + delay,
                                to: 1 - who,
                                signal: signal.clone(),
                                sent: self.now,
                            });
                        }
                    }
                }
                Action::Ring { call, glare, .. } => {
                    let s = &mut self.sides[who];
                    assert!(
                        s.rang.insert(call.clone()),
                        "seed {seed}: rang twice for call {call}"
                    );
                    assert_eq!(
                        s.m.current(),
                        Some(call.as_str()),
                        "seed {seed}: rang for a call that is not the current one"
                    );
                    s.ringing = Some(call.clone());
                    // We were calling them too and theirs won: the front end takes it without ringing.
                    take_at_once |= *glare;
                }
                Action::StopRinging { call } => {
                    let s = &mut self.sides[who];
                    if s.ringing.as_deref() == Some(call.as_str()) {
                        s.ringing = None;
                    }
                }
                Action::Ended { call, .. } => {
                    let s = &mut self.sides[who];
                    assert!(
                        s.ended.insert(call.clone()),
                        "seed {seed}: call {call} ended twice on one side"
                    );
                    if s.ringing.as_deref() == Some(call.as_str()) {
                        s.ringing = None; // the front end tears the call screen down with the end
                    }
                }
                _ => {}
            }
        }
        if take_at_once {
            self.step(
                who,
                Input::Accept {
                    sdp: "answer".into(),
                },
            );
        }
        if let (Some(c), None) = (&before, &after) {
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::Ended { call, .. } if call == c)),
                "seed {seed}: the call {c} stopped without the front end being told"
            );
        }
    }

    /// Deliver everything on the wire that is due now, in random order.
    fn deliver(&mut self) {
        let mut due: Vec<Wire> = vec![];
        let mut i = 0;
        while i < self.wire.len() {
            if self.wire[i].at <= self.now {
                due.push(self.wire.swap_remove(i));
            } else {
                i += 1;
            }
        }
        while !due.is_empty() {
            let w = due.swap_remove(self.rng.gen_range(0..due.len()));
            let from = Origin {
                device: self.sides[1 - w.to].device.into(),
                peer: Some(self.sides[w.to].peer.into()),
            };
            self.step(
                w.to,
                Input::Recv {
                    from,
                    signal: w.signal,
                    ts: w.sent,
                },
            );
        }
    }

    fn user(&mut self) {
        let who = self.rng.gen_range(0..2);
        let ringing = self.sides[who].ringing.is_some();
        match self.rng.gen_range(0..100) {
            0..=19 => {
                let call = hex32(&mut self.rng);
                let peer = self.sides[who].peer.to_string();
                let video = self.rng.gen_bool(0.3);
                self.step(
                    who,
                    Input::Dial {
                        call,
                        peer,
                        peer_devices: 1,
                        video,
                        sdp: "offer".into(),
                    },
                );
            }
            20..=39 if ringing => self.step(
                who,
                Input::Accept {
                    sdp: "answer".into(),
                },
            ),
            40..=47 if ringing => {
                let all = self.rng.gen_bool(0.5);
                self.step(who, Input::Decline { all })
            }
            48..=57 => self.step(who, Input::Hangup),
            58..=60 => self.step(who, Input::Fail),
            61..=70 => self.step(
                who,
                Input::LocalIce {
                    candidates: vec!["cand".into()],
                },
            ),
            71..=74 => self.step(who, Input::SendRestart { sdp: "re".into() }),
            75..=78 => self.step(who, Input::SendRestartAnswer { sdp: "rea".into() }),
            79..=81 => self.step(who, Input::AskRestart),
            _ => self.step(who, Input::Tick),
        }
    }

    fn settle(&mut self) {
        // No more people acting: let every signal arrive and every timer run out.
        let end = self.now + 3 * RING_MS;
        while self.now < end {
            self.now += 500;
            self.deliver();
            for who in 0..2 {
                self.step(who, Input::Tick);
            }
            // The person answers or declines what is still ringing, as a person would eventually.
            for who in 0..2 {
                if self.sides[who].ringing.is_some() && self.rng.gen_ratio(1, 20) {
                    let accept = self.rng.gen_bool(0.5);
                    self.step(
                        who,
                        if accept {
                            Input::Accept {
                                sdp: "answer".into(),
                            }
                        } else {
                            Input::Decline { all: false }
                        },
                    );
                }
            }
        }
    }
}

#[test]
fn replay_one_seed() {
    // SIM_SEED=5 SIM_TRACE=1 cargo test -p spike-core --test call_sim replay_one_seed -- --nocapture
    if let Some(seed) = std::env::var("SIM_SEED").ok().and_then(|s| s.parse().ok()) {
        run(seed);
    }
}

fn run(seed: u64) {
    let mut sim = Sim::new(seed);
    for _ in 0..STEPS {
        sim.now += sim.rng.gen_range(0..1500);
        sim.deliver();
        sim.user();
        // Every so often a long quiet gap, as when a phone is asleep.
        if sim.rng.gen_ratio(1, 40) {
            sim.now += sim.rng.gen_range(5_000..80_000);
        }
    }
    sim.settle();
    let seed = sim.seed;
    // 1. Nobody is still ringing.
    for (i, s) in sim.sides.iter().enumerate() {
        assert!(
            s.ringing.is_none(),
            "seed {seed}: side {i} is still ringing after everything settled"
        );
    }
    // 2. A call still going is going on both sides, and it is the same call.
    let (a, b) = (
        sim.sides[0].m.current().map(str::to_string),
        sim.sides[1].m.current().map(str::to_string),
    );
    assert!(
        a == b,
        "seed {seed}: the sides disagree about the call in progress: {a:?} vs {b:?}"
    );
    // 3. Hanging up ends it for both.
    if a.is_some() {
        sim.step(0, Input::Hangup);
        let end = sim.now + 10_000;
        while sim.now < end {
            sim.now += 500;
            sim.deliver();
        }
        assert!(
            sim.sides[0].m.is_idle() && sim.sides[1].m.is_idle(),
            "seed {seed}: a hang-up did not end the call on both sides"
        );
    }
}

#[test]
fn random_histories_keep_the_rules() {
    for seed in 0..std::env::var("SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEEDS)
    {
        run(seed);
    }
}

#[test]
fn an_invite_is_never_rung_for_after_it_was_replayed() {
    // The same invite delivered again later, after the call ended, does not ring again (replay protection).
    let mut m = Machine::new();
    let call: String = "ab".repeat(16);
    let from = Origin {
        device: "d1".into(),
        peer: Some("bob".into()),
    };
    let inv = Signal::Invite {
        call: call.clone(),
        video: false,
        sdp: "offer".into(),
    };
    let now = 1_000_000;
    let a = m.step(
        now,
        Input::Recv {
            from: from.clone(),
            signal: inv.clone(),
            ts: now,
        },
    );
    assert!(a.iter().any(|x| matches!(x, Action::Ring { .. })));
    m.step(now + 1000, Input::Decline { all: false });
    assert!(m.is_idle());
    for k in 0..5 {
        let a = m.step(
            now + 2000 + k,
            Input::Recv {
                from: from.clone(),
                signal: inv.clone(),
                ts: now,
            },
        );
        assert!(a.is_empty(), "replayed invite produced {a:?}");
    }
}

#[test]
fn an_invite_from_a_stranger_or_from_far_in_the_past_does_not_ring() {
    let mut m = Machine::new();
    let now = 10_000_000;
    let inv = |c: &str| Signal::Invite {
        call: c.repeat(16),
        video: false,
        sdp: "o".into(),
    };
    // from our own device list but not a contact: peer None is one of our own devices, which may ring us
    // (answering elsewhere); an unknown contact is simply not delivered with a peer key at all, so the machine
    // only sees contacts and own devices. A stale invite is dropped either way.
    let from = Origin {
        device: "d1".into(),
        peer: Some("bob".into()),
    };
    let a = m.step(
        now,
        Input::Recv {
            from: from.clone(),
            signal: inv("cd"),
            ts: now - STALE_MS - 1,
        },
    );
    assert!(
        !a.iter().any(|x| matches!(x, Action::Ring { .. })),
        "stale invite rang: {a:?}"
    );
    let a = m.step(
        now,
        Input::Recv {
            from,
            signal: inv("ef"),
            ts: now + STALE_MS + 1,
        },
    );
    assert!(
        !a.iter().any(|x| matches!(x, Action::Ring { .. })),
        "invite from the future rang: {a:?}"
    );
    assert!(m.is_idle());
}

#[test]
fn only_three_unanswered_invites_an_hour_ring_from_one_contact() {
    let mut m = Machine::new();
    let from = Origin {
        device: "d1".into(),
        peer: Some("bob".into()),
    };
    let mut now = 5_000_000u64;
    let mut rang = 0;
    for k in 0..6u8 {
        let call: String = format!("{:02x}", k + 1).repeat(16);
        let a = m.step(
            now,
            Input::Recv {
                from: from.clone(),
                signal: Signal::Invite {
                    call,
                    video: false,
                    sdp: "o".into(),
                },
                ts: now,
            },
        );
        if a.iter().any(|x| matches!(x, Action::Ring { .. })) {
            rang += 1;
        }
        // nobody answers: it rings out
        now += RING_MS + 1;
        m.step(now, Input::Tick);
        assert!(m.is_idle());
        now += 1000;
    }
    assert_eq!(
        rang, MAX_UNANSWERED_PER_HOUR,
        "rang {rang} times for six unanswered invites"
    );
}

#[test]
fn glare_the_smaller_call_id_wins_on_both_sides() {
    // Both people dial each other at the same moment; the signals cross.
    let (lo, hi): (String, String) = ("11".repeat(16), "99".repeat(16));
    let mut a = Machine::new();
    let mut b = Machine::new();
    let now = 2_000_000;
    a.step(
        now,
        Input::Dial {
            call: lo.clone(),
            peer: "bob".into(),
            peer_devices: 1,
            video: false,
            sdp: "o".into(),
        },
    );
    b.step(
        now,
        Input::Dial {
            call: hi.clone(),
            peer: "alice".into(),
            peer_devices: 1,
            video: false,
            sdp: "o".into(),
        },
    );
    let to_b = Origin {
        device: "dev-a".into(),
        peer: Some("alice".into()),
    };
    let to_a = Origin {
        device: "dev-b".into(),
        peer: Some("bob".into()),
    };
    let at_b = b.step(
        now + 10,
        Input::Recv {
            from: to_b,
            signal: Signal::Invite {
                call: lo.clone(),
                video: false,
                sdp: "o".into(),
            },
            ts: now,
        },
    );
    let at_a = a.step(
        now + 10,
        Input::Recv {
            from: to_a,
            signal: Signal::Invite {
                call: hi.clone(),
                video: false,
                sdp: "o".into(),
            },
            ts: now,
        },
    );
    // b, whose call id is larger, gives way and rings for a's call (glare); a keeps its own call and ignores b's.
    assert!(
        at_b.iter()
            .any(|x| matches!(x, Action::Ring { call, glare: true, .. } if *call == lo)),
        "{at_b:?}"
    );
    assert!(
        !at_a.iter().any(|x| matches!(x, Action::Ring { .. })),
        "{at_a:?}"
    );
    assert_eq!(a.current(), Some(lo.as_str()));
    assert_eq!(b.current(), Some(lo.as_str()));
}
