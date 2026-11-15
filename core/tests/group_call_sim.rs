//! Randomized tests of the group call state machine (ADR 0032): several people, some with two devices, start, join,
//! leave and dismiss calls in one group while the signals between all of them arrive late, out of order and twice.
//! Whatever happens, the rules must hold: nobody rings twice for a call, a ringing phone always stops ringing, nobody
//! is left in a call on two devices, a pair of connections is never opened twice, and once everything has settled all
//! devices agree on who is in each call and the pairs match a full mesh. When everyone has left, every call is gone.
//! Each seed is a different history; a failure names its seed so it can be replayed (`SIM_SEED=n`, `SIM_TRACE=1`; `SIM_SEEDS=n` runs n histories instead of 400).

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use spike_core::call::RING_MS;
use spike_core::group_call::*;
use std::collections::{BTreeSet, HashSet};

const SEEDS: u64 = 400;
const STEPS: usize = 260;

struct Dev {
    m: Machine,
    owner: usize,
    name: String,
    ringing: Option<String>,
    rang: HashSet<String>,
    pairs: HashSet<String>,
}

struct Wire {
    at: u64,
    to: usize,
    from: usize,
    signal: Signal,
    sent: u64,
}

struct Sim {
    devs: Vec<Dev>,
    persons: usize,
    wire: Vec<Wire>,
    now: u64,
    rng: StdRng,
    seed: u64,
    calls_started: u32,
    /// Which pairs of people are contacts (symmetric).
    contact: Vec<Vec<bool>>,
}

fn hex32(n: u32) -> String {
    format!("{n:032x}")
}

impl Sim {
    #[allow(clippy::needless_range_loop)] // a symmetric matrix is clearest with indices
    fn new(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let persons = rng.gen_range(3..=6);
        let mut devs = vec![];
        for p in 0..persons {
            let n = if rng.gen_bool(0.3) { 2 } else { 1 };
            for k in 0..n {
                let name = format!("d{p}{k}");
                devs.push(Dev {
                    m: Machine::new(&format!("p{p}"), &name),
                    owner: p,
                    name,
                    ringing: None,
                    rang: HashSet::new(),
                    pairs: HashSet::new(),
                });
            }
        }
        let mut contact = vec![vec![true; persons]; persons];
        for a in 0..persons {
            for b in 0..a {
                let c = rng.gen_bool(0.7);
                contact[a][b] = c;
                contact[b][a] = c;
            }
        }
        let mut sim = Self {
            devs,
            persons,
            wire: vec![],
            now: 1_000_000,
            rng,
            seed,
            calls_started: 0,
            contact,
        };
        for i in 0..sim.devs.len() {
            sim.feed(
                i,
                Input::Group {
                    group: "g".into(),
                    members: persons,
                    muted: false,
                },
            );
        }
        sim
    }

    fn fail(&self, what: &str) -> ! {
        panic!("seed {}: {what}", self.seed);
    }

    /// Run one input on one device, check what it did, and put what it sent on the wire. Follow-up inputs the
    /// front end would give at once (an offer for a new pair, an answer, a call server) are fed straight away.
    fn feed(&mut self, who: usize, input: Input) {
        let mut queue = vec![input];
        while let Some(input) = queue.pop() {
            let shown = format!("{input:?}");
            let actions = self.devs[who].m.step(self.now, input);
            if std::env::var("SIM_TRACE").is_ok() {
                eprintln!(
                    "t={} dev {} <- {}\n   -> {actions:?}",
                    self.now,
                    self.devs[who].name,
                    shown.chars().take(150).collect::<String>()
                );
            }
            for a in actions {
                match a {
                    Action::Send { to, signal } => self.send(who, to, signal),
                    Action::Ring { call, .. } => {
                        if !self.devs[who].rang.insert(call.clone()) {
                            self.fail(&format!("{} rang twice for {call}", self.devs[who].name));
                        }
                        if let Some(old) = &self.devs[who].ringing {
                            self.fail(&format!(
                                "{} rings for {call} while still ringing for {old}",
                                self.devs[who].name
                            ));
                        }
                        self.devs[who].ringing = Some(call);
                    }
                    Action::StopRinging { call } => {
                        if self.devs[who].ringing.as_deref() != Some(call.as_str()) {
                            self.fail(&format!(
                                "{} stopped ringing for {call} but rings for {:?}",
                                self.devs[who].name, self.devs[who].ringing
                            ));
                        }
                        self.devs[who].ringing = None;
                    }
                    Action::Pair {
                        device, dial: true, ..
                    } => {
                        if !self.devs[who].pairs.insert(device.clone()) {
                            self.fail(&format!(
                                "{} opened a second pair with {device}",
                                self.devs[who].name
                            ));
                        }
                        queue.push(Input::LocalOffer {
                            device,
                            video: false,
                            sdp: "offer".into(),
                        });
                    }
                    Action::Pair { dial: false, .. } => {}
                    Action::RemoteOffer { device, .. } => {
                        if !self.devs[who].pairs.insert(device.clone()) {
                            self.fail(&format!(
                                "{} took a second offer from {device}",
                                self.devs[who].name
                            ));
                        }
                        queue.push(Input::LocalAnswer {
                            device,
                            sdp: "answer".into(),
                        });
                    }
                    Action::Unpair { device, .. } => {
                        if !self.devs[who].pairs.remove(&device) {
                            self.fail(&format!(
                                "{} unpaired {device} without a pair",
                                self.devs[who].name
                            ));
                        }
                    }
                    Action::MoveToSfu { .. } => queue.push(Input::SfuReady {
                        url: "wss://sfu.example/r".into(),
                        room: "abc123".into(),
                    }),
                    Action::SfuRoom { .. } => {
                        // The call runs on a server: pairs are dropped by `Unpair` actions; nothing else to do.
                    }
                    Action::Left { call, .. } => {
                        if self.devs[who].m.current() == Some(call.as_str()) {
                            self.fail(&format!(
                                "{} reported leaving {call} but is still in it",
                                self.devs[who].name
                            ));
                        }
                    }
                    Action::Participants { .. }
                    | Action::Bar { .. }
                    | Action::Refused { .. }
                    | Action::Missed { .. }
                    | Action::Ended { .. }
                    | Action::RemoteAnswer { .. }
                    | Action::RemoteIce { .. }
                    | Action::RemoteKey { .. }
                    | Action::RemoteKeyAck { .. } => {}
                }
            }
        }
    }

    fn send(&mut self, from: usize, to: Target, signal: Signal) {
        let targets: Vec<usize> = match &to {
            Target::AllMembers => (0..self.devs.len()).filter(|j| *j != from).collect(),
            Target::Device(d) => (0..self.devs.len())
                .filter(|j| self.devs[*j].name == *d && *j != from)
                .collect(),
        };
        for j in targets {
            let copies = if self.rng.gen_bool(0.05) { 2 } else { 1 };
            for _ in 0..copies {
                let delay = self.rng.gen_range(0..3_000);
                self.wire.push(Wire {
                    at: self.now + delay,
                    to: j,
                    from,
                    signal: signal.clone(),
                    sent: self.now,
                });
            }
        }
    }

    fn deliver_due(&mut self) {
        loop {
            let due: Vec<usize> = (0..self.wire.len())
                .filter(|i| self.wire[*i].at <= self.now)
                .collect();
            if due.is_empty() {
                return;
            }
            let pick = due[self.rng.gen_range(0..due.len())];
            let w = self.wire.swap_remove(pick);
            let (fo, to) = (self.devs[w.from].owner, self.devs[w.to].owner);
            let from = Origin {
                owner: format!("p{fo}"),
                device: self.devs[w.from].name.clone(),
                own: fo == to,
                contact: fo == to || self.contact[fo][to],
            };
            self.feed(
                w.to,
                Input::Recv {
                    from,
                    group: "g".into(),
                    signal: w.signal,
                    ts: w.sent,
                },
            );
        }
    }

    fn all_calls(&self) -> BTreeSet<String> {
        (0..self.calls_started).map(|n| hex32(n + 1)).collect()
    }

    fn random_action(&mut self) {
        let who = self.rng.gen_range(0..self.devs.len());
        let in_call = self.devs[who].m.current().map(str::to_string);
        match self.rng.gen_range(0..10) {
            0 | 1 if in_call.is_none() => {
                // Start only while no call is on in the group as far as this device knows, as the app would.
                if self.devs[who].m.bar("g").is_none() {
                    self.calls_started += 1;
                    let call = hex32(self.calls_started);
                    let sfu = self.rng.gen_bool(0.5);
                    self.feed(
                        who,
                        Input::Start {
                            call,
                            group: "g".into(),
                            video: false,
                            sfu,
                        },
                    );
                }
            }
            2..=5 if in_call.is_none() => {
                if let Some(bar) = self.devs[who].m.bar("g") {
                    self.feed(
                        who,
                        Input::Join {
                            call: bar.call,
                            video: false,
                        },
                    );
                }
            }
            6 | 7 if in_call.is_some() => self.feed(who, Input::Leave),
            8 => {
                if let Some(call) = self.devs[who].ringing.clone() {
                    self.feed(who, Input::Dismiss { call });
                }
            }
            _ => self.feed(who, Input::Tick),
        }
    }

    fn step(&mut self) {
        self.random_action();
        self.now += self.rng.gen_range(20..900);
        self.deliver_due();
    }

    /// Deliver everything still on the wire.
    fn drain(&mut self) {
        let mut guard = 0;
        while !self.wire.is_empty() {
            self.now += 500;
            self.deliver_due();
            guard += 1;
            assert!(guard < 1_000, "seed {}: the wire never drained", self.seed);
        }
    }

    fn tick_all(&mut self) {
        for i in 0..self.devs.len() {
            self.feed(i, Input::Tick);
        }
    }

    /// Everything has arrived: what the devices believe must agree.
    fn check_settled(&mut self) {
        self.drain();
        // Nobody keeps ringing after the ring time.
        self.now += RING_MS + 1_000;
        self.tick_all();
        self.drain();
        for d in &self.devs {
            if let Some(c) = &d.ringing {
                self.fail(&format!("{} is still ringing for {c}", d.name));
            }
        }
        for call in self.all_calls() {
            let in_call: BTreeSet<String> = self
                .devs
                .iter()
                .filter(|d| d.m.current() == Some(call.as_str()))
                .map(|d| d.name.clone())
                .collect();
            // One device per person.
            let mut owners = HashSet::new();
            for d in self.devs.iter().filter(|d| in_call.contains(&d.name)) {
                if !owners.insert(d.owner) {
                    self.fail(&format!("person p{} is in {call} on two devices", d.owner));
                }
            }
            for d in &self.devs {
                let seen: BTreeSet<String> =
                    d.m.people(&call).iter().map(|p| p.device.clone()).collect();
                let tracks = d.m.tracks(&call);
                if tracks && seen != in_call {
                    self.fail(&format!(
                        "{} sees {seen:?} in {call}, but {in_call:?} are in it",
                        d.name
                    ));
                }
                if !tracks && !seen.is_empty() {
                    self.fail(&format!("{} lists people for a call it dropped", d.name));
                }
            }
            // A mesh has a pair between every two people of it; a call on a server has none.
            let on_server = self
                .devs
                .iter()
                .any(|d| d.m.bar("g").is_some_and(|b| b.call == call && b.sfu));
            for d in self.devs.iter().filter(|d| in_call.contains(&d.name)) {
                let want: HashSet<String> = if on_server || in_call.len() > MESH_MAX {
                    HashSet::new()
                } else {
                    in_call.iter().filter(|n| **n != d.name).cloned().collect()
                };
                if d.pairs != want && in_call.len() <= MESH_MAX {
                    self.fail(&format!(
                        "{} has pairs {:?} in {call}, a mesh of {in_call:?} needs {want:?}",
                        d.name, d.pairs
                    ));
                }
            }
        }
        // Nobody is on a call they have no pairs for after leaving.
        for d in &self.devs {
            if d.m.current().is_none() && !d.pairs.is_empty() {
                self.fail(&format!(
                    "{} is not in a call but keeps pairs {:?}",
                    d.name, d.pairs
                ));
            }
        }
    }

    /// Everyone leaves; after a minute of nobody, every call is gone.
    fn finish(&mut self) {
        for i in 0..self.devs.len() {
            if self.devs[i].m.current().is_some() {
                self.feed(i, Input::Leave);
            }
        }
        self.drain();
        self.now += EMPTY_END_MS + 1_000;
        self.tick_all();
        self.drain();
        self.now += EMPTY_END_MS + 1_000;
        self.tick_all();
        for d in &self.devs {
            if d.m.running() != 0 {
                self.fail(&format!(
                    "{} still tracks a call after everyone left",
                    d.name
                ));
            }
            if d.m.current().is_some() || !d.pairs.is_empty() || d.ringing.is_some() {
                self.fail(&format!(
                    "{} is left in a call, with pairs or ringing",
                    d.name
                ));
            }
        }
        let _ = self.persons;
    }
}

fn run(seed: u64) {
    let mut sim = Sim::new(seed);
    for _ in 0..STEPS {
        sim.step();
    }
    sim.check_settled();
    sim.finish();
}

#[test]
fn many_random_histories_keep_the_rules() {
    if let Some(seed) = std::env::var("SIM_SEED").ok().and_then(|s| s.parse().ok()) {
        run(seed);
        return;
    }
    let seeds = std::env::var("SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEEDS);
    for seed in 0..seeds {
        run(seed);
    }
}
