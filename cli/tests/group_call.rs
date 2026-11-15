//! Group call signalling (ADR 0032, phase B0), end to end on a real relay: members who are not contacts of each other
//! start, join and leave a call through the group, pairs are made only the right way round, a pair with a
//! non-contact is relay-only, and signals for one device reach only that device. No media.

use spike_cli::groups::{self};
use spike_cli::{ops, state::Home, Outcome};
use spike_core::clock::now_ms;
use spike_core::group::AddBy;
use spike_core::group_call::{Action, Input, Machine, Origin, Signal, Target};
use spike_core::identity::Identity;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase group calls";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));

async fn relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

struct Dev(Home, PathBuf);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.1).ok();
    }
}
async fn dev(tag: &str, url: &str) -> Dev {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    let dir = std::env::temp_dir().join(format!("spike-gcall-{tag}-{n}"));
    let d = Dev(
        Home::create(&dir, PASS, KdfParams::insecure_for_tests(), false).unwrap(),
        dir,
    );
    ops::publish_keys(&d.0, url).await.unwrap();
    d
}
fn owner(d: &Dev) -> String {
    d.0.load_device_list().unwrap().owner
}
fn device(d: &Dev) -> String {
    d.0.load_identity().unwrap().device_id()
}

/// One person in the test: their app and their call machine, and everything the machine asked for.
struct P {
    name: &'static str,
    dev: Dev,
    m: Machine,
    seen: Vec<Action>,
}

struct World {
    url: String,
    group: String,
    people: Vec<P>,
    /// device id -> owner key, to address one device.
    owner_of: BTreeMap<String, String>,
}

impl World {
    fn p(&mut self, name: &str) -> &mut P {
        self.people.iter_mut().find(|p| p.name == name).unwrap()
    }

    fn index(&self, name: &str) -> usize {
        self.people.iter().position(|p| p.name == name).unwrap()
    }

    /// Run one input on one person and carry out what the machine asks, until it asks for nothing more.
    async fn run(&mut self, name: &str, input: Input) {
        let i = self.index(name);
        let mut queue = vec![input];
        while let Some(input) = queue.pop() {
            let actions = self.people[i].m.step(now_ms(), input);
            for a in actions {
                match &a {
                    Action::Send { to, signal } => {
                        let owner = match to {
                            Target::Device(d) => self.owner_of.get(d).cloned(),
                            Target::AllMembers => None,
                        };
                        groups::send_group_call(
                            &self.people[i].dev.0,
                            &spike_cli::transport::Target::from(self.url.as_str()),
                            &self.group,
                            signal,
                            to,
                            owner.as_deref(),
                        )
                        .await
                        .unwrap_or_else(|e| panic!("{name} could not send {signal:?}: {e:#}"));
                    }
                    Action::Pair {
                        device, dial: true, ..
                    } => queue.push(Input::LocalOffer {
                        device: device.clone(),
                        video: false,
                        sdp: format!("offer from {name}"),
                    }),
                    Action::RemoteOffer { device, .. } => queue.push(Input::LocalAnswer {
                        device: device.clone(),
                        sdp: format!("answer from {name}"),
                    }),
                    Action::MoveToSfu { .. } => {}
                    _ => {}
                }
                self.people[i].seen.push(a);
            }
        }
    }

    /// Everyone reads what is waiting for them and the machines react; repeat until quiet.
    async fn settle(&mut self) {
        for _ in 0..8 {
            let mut any = false;
            for k in 0..self.people.len() {
                let mut got = vec![];
                ops::recv(&self.people[k].dev.0, self.url.as_str(), IDLE, |o| {
                    got.push(o)
                })
                .await
                .unwrap();
                for o in got {
                    if let Outcome::GroupCall {
                        owner,
                        device,
                        contact,
                        own,
                        group,
                        signal,
                        ts,
                        ..
                    } = o
                    {
                        any = true;
                        let name = self.people[k].name;
                        self.run(
                            name,
                            Input::Recv {
                                from: Origin {
                                    owner,
                                    device,
                                    own,
                                    contact,
                                },
                                group,
                                signal,
                                ts,
                            },
                        )
                        .await;
                    }
                }
            }
            if !any {
                return;
            }
        }
    }
}

/// Alice knows Bob and Carol; Bob and Carol do not know each other. All three are in the group "Pilot".
async fn world() -> World {
    let url = relay().await;
    let (alice, bob, carol) = (
        dev("alice", &url).await,
        dev("bob", &url).await,
        dev("carol", &url).await,
    );
    for (a, an, b, bn) in [
        (&alice, "bob", &bob, "alice"),
        (&alice, "carol", &carol, "alice"),
    ] {
        a.0.add_contact(an, &b.0.identity_card().unwrap(), false)
            .unwrap();
        b.0.add_contact(bn, &a.0.identity_card().unwrap(), false)
            .unwrap();
    }
    groups::create(
        &alice.0,
        url.as_str(),
        "Pilot",
        &["bob".into(), "carol".into()],
        AddBy::Admins,
    )
    .await
    .unwrap();
    for d in [&bob, &carol] {
        ops::recv(&d.0, url.as_str(), IDLE, |_| {}).await.unwrap();
    }
    let group = groups::find(&alice.0, "Pilot").unwrap().id().to_string();
    let mut owner_of = BTreeMap::new();
    let mut people = vec![];
    for (name, d) in [("alice", alice), ("bob", bob), ("carol", carol)] {
        owner_of.insert(device(&d), owner(&d));
        let mut m = Machine::new(&owner(&d), &device(&d));
        m.step(
            now_ms(),
            Input::Group {
                group: group.clone(),
                members: 3,
                muted: false,
            },
        );
        people.push(P {
            name,
            dev: d,
            m,
            seen: vec![],
        });
    }
    World {
        url,
        group,
        people,
        owner_of,
    }
}

fn call_id() -> String {
    format!("{:032x}", now_ms())
}

fn rang(p: &P) -> bool {
    p.seen.iter().any(|a| matches!(a, Action::Ring { .. }))
}

#[tokio::test]
async fn members_who_are_not_contacts_call_each_other_through_the_group() {
    let mut w = world().await;
    let call = call_id();
    let (a_dev, b_dev, c_dev) = (
        device(&w.p("alice").dev),
        device(&w.p("bob").dev),
        device(&w.p("carol").dev),
    );

    // Alice starts: Bob and Carol are rung, though Carol and Bob are strangers to each other.
    w.run(
        "alice",
        Input::Start {
            call: call.clone(),
            group: w.group.clone(),
            video: false,
            sfu: false,
        },
    )
    .await;
    w.settle().await;
    assert!(rang(w.p("bob")) && rang(w.p("carol")));
    assert!(!rang(w.p("alice")));

    // Bob joins and dials Alice, a contact: direct candidates are fine. Only Alice's device gets the offer.
    w.run(
        "bob",
        Input::Join {
            call: call.clone(),
            video: false,
        },
    )
    .await;
    w.settle().await;
    let alice_offer = w.p("alice").seen.iter().find_map(|a| match a {
        Action::RemoteOffer {
            device, relay_only, ..
        } => Some((device.clone(), *relay_only)),
        _ => None,
    });
    assert_eq!(alice_offer, Some((b_dev.clone(), false)));
    assert!(
        !w.p("carol")
            .seen
            .iter()
            .any(|a| matches!(a, Action::RemoteOffer { .. })),
        "a signal for one device reached another"
    );
    // Alice's answer came back to Bob only.
    assert!(w
        .p("bob")
        .seen
        .iter()
        .any(|a| matches!(a, Action::RemoteAnswer { device, .. } if *device == a_dev)));

    // Carol joins and dials both earlier people. Bob is not her contact: that pair is relay-only.
    w.run(
        "carol",
        Input::Join {
            call: call.clone(),
            video: false,
        },
    )
    .await;
    w.settle().await;
    let offer_to = |w: &mut World, who: &str| {
        w.p(who).seen.iter().find_map(|a| match a {
            Action::RemoteOffer {
                device, relay_only, ..
            } if *device == c_dev => Some(*relay_only),
            _ => None,
        })
    };
    assert_eq!(offer_to(&mut w, "alice"), Some(false));
    assert_eq!(
        offer_to(&mut w, "bob"),
        Some(true),
        "a non-contact pair must be relay-only"
    );

    // The bar shows three people everywhere, and everyone agrees.
    for who in ["alice", "bob", "carol"] {
        let group = w.group.clone();
        let bar = w.p(who).m.bar(&group).unwrap();
        assert_eq!((bar.people, bar.joined), (3, true), "{who}");
    }

    // Bob leaves; the others still have a call, with two people. Then Carol and Alice leave: it ends for everyone.
    w.run("bob", Input::Leave).await;
    w.settle().await;
    for who in ["alice", "carol"] {
        let group = w.group.clone();
        assert_eq!(w.p(who).m.bar(&group).unwrap().people, 2, "{who}");
    }
    w.run("carol", Input::Leave).await;
    w.settle().await;
    w.run("alice", Input::Leave).await;
    w.settle().await;
    for who in ["alice", "bob", "carol"] {
        assert_eq!(w.p(who).m.running(), 0, "{who} still has the call");
        assert!(w
            .p(who)
            .seen
            .iter()
            .any(|a| matches!(a, Action::Ended { .. })));
    }
}

#[tokio::test]
async fn a_call_signal_is_never_stored_as_a_message() {
    let mut w = world().await;
    w.run(
        "alice",
        Input::Start {
            call: call_id(),
            group: w.group.clone(),
            video: false,
            sfu: false,
        },
    )
    .await;
    w.settle().await;
    for who in ["alice", "bob", "carol"] {
        let (_, history) = groups::history(&w.p(who).dev.0, "Pilot", 50).unwrap();
        assert!(
            history.iter().all(|m| !m.body.contains("\"op\"")),
            "{who} stored a call signal as a message: {history:?}"
        );
    }
}

#[tokio::test]
async fn a_signal_for_one_device_of_someone_who_is_not_a_member_is_refused() {
    let mut w = world().await;
    let stranger = dev("stranger", &w.url).await;
    let (url, group) = (w.url.clone(), w.group.clone());
    let call = call_id();
    let signal = Signal::Join {
        call,
        video: false,
        at: 1,
    };
    let err = groups::send_group_call(
        &w.p("alice").dev.0,
        &spike_cli::transport::Target::from(url.as_str()),
        &group,
        &signal,
        &Target::Device(device(&stranger)),
        Some(&owner(&stranger)),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("not another member"), "{err:#}");
}

#[tokio::test]
async fn a_member_who_left_the_group_can_no_longer_send_call_signals() {
    let mut w = world().await;
    let (url, group) = (w.url.clone(), w.group.clone());
    groups::leave(&w.p("carol").dev.0, url.as_str(), "Pilot")
        .await
        .unwrap();
    let signal = Signal::Join {
        call: call_id(),
        video: false,
        at: 1,
    };
    let err = groups::send_group_call(
        &w.p("carol").dev.0,
        &spike_cli::transport::Target::from(url.as_str()),
        &group,
        &signal,
        &Target::AllMembers,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("not in this group")
            || format!("{err:#}").contains("no longer a member"),
        "{err:#}"
    );
}
