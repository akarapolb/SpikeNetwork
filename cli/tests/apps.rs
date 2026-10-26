//! App channels (ADR 0030), end to end on a real relay: data for another application reaches the
//! right devices, is never shown as chat, and only comes from people allowed to send it.

use spike_cli::apps::{self, EventKind, To};
use spike_cli::groups;
use spike_cli::{ops, state::Home, Outcome};
use spike_core::app::{AppData, Mode};
use spike_core::envelope::Envelope;
use spike_core::group::AddBy;
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard};
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::time::Duration;

const PASS: &str = "test passphrase apps";
const IDLE: Option<Duration> = Some(Duration::from_millis(300));
const APP: &str = "work.center";

async fn relay() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}
struct Dev(Home);
impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.path()).ok();
    }
}
fn new_home(tag: &str) -> Home {
    let n = hex::encode(&Identity::generate().unwrap().ed_public()[..6]);
    Home::create(
        std::env::temp_dir().join(format!("spike-apps-{tag}-{n}")),
        PASS,
        KdfParams::insecure_for_tests(),
        false,
    )
    .unwrap()
}
/// A person with `n` devices under one owner key, all on the default relay.
async fn person(tag: &str, n: usize, url: &str) -> (Vec<Dev>, String) {
    let devs: Vec<Dev> = (0..n)
        .map(|i| Dev(new_home(&format!("{tag}{i}"))))
        .collect();
    let owner = devs[0].0.load_owner().unwrap();
    let entries = devs
        .iter()
        .map(|d| DeviceEntry {
            card: d.0.load_identity().unwrap().card().encode(),
            relay: None,
        })
        .collect();
    let list = DeviceList::sign(&owner, 2, entries).unwrap();
    for d in &devs {
        d.0.install_identity(&owner, &list).unwrap();
        ops::publish_keys(&d.0, url).await.unwrap();
    }
    (devs, IdentityCard::encode(&list))
}
fn befriend(devs: &[Dev], name: &str, card: &str) {
    for d in devs {
        d.0.add_contact(name, card, true).unwrap();
    }
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}
fn app_events(got: &[Outcome]) -> Vec<apps::AppEvent> {
    got.iter()
        .filter_map(|o| match o {
            Outcome::App(e) => Some(e.clone()),
            _ => None,
        })
        .collect()
}
fn bytes_of(e: &apps::AppEvent) -> Vec<u8> {
    AppData {
        app: e.app.clone(),
        channel: e.channel.clone(),
        mode: e.mode.unwrap(),
        data: e.data.clone().unwrap(),
    }
    .bytes()
    .unwrap()
}
fn register(devs: &[&Dev]) {
    for d in devs {
        apps::register(&d.0, APP, "Center Work").unwrap();
    }
}
fn history_len(d: &Dev, peer: &str) -> usize {
    d.0.messages()
        .unwrap()
        .conversation(peer, 100)
        .unwrap()
        .len()
}

/// Alice (2 devices) and Bob (2 devices), contacts of each other.
async fn two_people() -> (String, Vec<Dev>, Vec<Dev>) {
    let url = relay().await;
    let (alice, ac) = person("alice", 2, &url).await;
    let (bob, bc) = person("bob", 2, &url).await;
    befriend(&alice, "bob", &bc);
    befriend(&bob, "alice", &ac);
    (url, alice, bob)
}

fn owner(d: &Dev) -> String {
    d.0.load_device_list().unwrap().owner
}

#[tokio::test]
async fn stored_data_waits_for_the_app_reaches_every_device_and_is_never_chat() {
    let (url, alice, bob) = two_people().await;
    register(&[&alice[0], &alice[1], &bob[0], &bob[1]]);
    let sent = apps::send(
        &alice[0].0,
        url.as_str(),
        &To::Contact("bob".into()),
        APP,
        "sheet-1",
        Mode::Stored,
        b"B12=42",
    )
    .await
    .unwrap();
    assert_eq!(sent.delivered, 2, "both of Bob's devices");

    // Bob's devices were offline; they catch up later, each exactly once.
    for d in &bob {
        let got = app_events(&recv(d, &url).await);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            (got[0].kind, got[0].from.as_str(), got[0].own),
            (EventKind::Data, "alice", false)
        );
        assert_eq!(bytes_of(&got[0]), b"B12=42");
        assert!(got[0].seq > 0);
        assert!(app_events(&recv(d, &url).await).is_empty(), "acknowledged");
        // The app takes it from the inbox; passing its position acknowledges it.
        let waiting = apps::take(&d.0, APP, 0, 100).unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, sent.id);
        assert!(apps::take(&d.0, APP, waiting[0].seq, 100)
            .unwrap()
            .is_empty());
        assert_eq!(history_len(d, &owner(&alice[0])), 0, "not in the chat");
    }
    // Alice's other device gets a copy, marked as hers.
    let got = app_events(&recv(&alice[1], &url).await);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].from.as_str(), got[0].own), ("you", true));
    assert_eq!(history_len(&alice[1], &owner(&bob[0])), 0);
    assert_eq!(apps::registered(&bob[0].0).unwrap()[0].received, 1);
    assert_eq!(apps::registered(&alice[0].0).unwrap()[0].sent, 1);
}

#[tokio::test]
async fn data_for_an_app_not_set_up_here_is_dropped_and_counted() {
    let (url, alice, bob) = two_people().await;
    register(&[&alice[0]]);
    apps::send(
        &alice[0].0,
        url.as_str(),
        &To::Contact("bob".into()),
        APP,
        "sheet-1",
        Mode::Stored,
        b"x",
    )
    .await
    .unwrap();
    let got = recv(&bob[0], &url).await;
    assert!(matches!(got.as_slice(), [Outcome::Ignored]), "{got:?}");
    assert!(
        recv(&bob[0], &url).await.is_empty(),
        "it did not block the queue"
    );
    let dropped = apps::dropped(&bob[0].0).unwrap();
    assert_eq!((dropped[0].app.as_str(), dropped[0].count), (APP, 1));
    assert!(apps::take(&bob[0].0, APP, 0, 100).unwrap().is_empty());
    // Setting it up clears the notice; sending needs it set up too.
    apps::register(&bob[0].0, APP, "Center Work").unwrap();
    assert!(apps::dropped(&bob[0].0).unwrap().is_empty());
    assert!(apps::send(
        &bob[1].0,
        url.as_str(),
        &To::Contact("alice".into()),
        APP,
        "s",
        Mode::Stored,
        b"x"
    )
    .await
    .is_err());
    // Removing an app forgets what waited for it.
    apps::send(
        &alice[0].0,
        url.as_str(),
        &To::Contact("bob".into()),
        APP,
        "sheet-1",
        Mode::Stored,
        b"y",
    )
    .await
    .unwrap();
    recv(&bob[0], &url).await;
    assert_eq!(apps::take(&bob[0].0, APP, 0, 100).unwrap().len(), 1);
    assert!(apps::remove(&bob[0].0, APP).unwrap());
    assert!(apps::take(&bob[0].0, APP, 0, 100).unwrap().is_empty());
}

#[tokio::test]
async fn live_data_is_never_kept_and_late_live_data_is_dropped() {
    let (url, alice, bob) = two_people().await;
    register(&[&alice[0], &bob[0], &bob[1]]);
    apps::send(
        &alice[0].0,
        url.as_str(),
        &To::Contact("bob".into()),
        APP,
        "sheet-1",
        Mode::Live,
        b"cursor B12",
    )
    .await
    .unwrap();
    let got = app_events(&recv(&bob[0], &url).await);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].seq, got[0].mode), (0, Some(Mode::Live)));
    assert_eq!(bytes_of(&got[0]), b"cursor B12");
    assert!(
        apps::take(&bob[0].0, APP, 0, 100).unwrap().is_empty(),
        "never written down"
    );

    // One that arrives a minute late was about a moment that passed.
    let mut env =
        Envelope::app(&AppData::new(APP, "sheet-1", Mode::Live, b"old cursor").unwrap()).unwrap();
    env.ts -= 60_000;
    ops::deliver_envelope(&alice[0].0, url.as_str(), "bob", env)
        .await
        .unwrap();
    let got = recv(&bob[1], &url).await;
    // Bob's second device gets both; the first live one is fresh, the late one is dropped.
    let evs = app_events(&got);
    assert_eq!(evs.len(), 1, "{got:?}");
    assert_eq!(bytes_of(&evs[0]), b"cursor B12");
    assert!(got.iter().any(|o| matches!(o, Outcome::Ignored)));
}

#[tokio::test]
async fn strangers_cannot_send_app_data() {
    let (url, alice, bob) = two_people().await;
    let (eve, _) = person("eve", 1, &url).await;
    register(&[&bob[0], &eve[0]]);
    // Eve has Bob's card, but Bob never added her.
    eve[0]
        .0
        .add_contact("bob", &bob[0].0.identity_card().unwrap(), false)
        .unwrap();
    let env = Envelope::app(&AppData::new(APP, "sheet-1", Mode::Stored, b"spam").unwrap()).unwrap();
    ops::deliver_envelope(&eve[0].0, url.as_str(), "bob", env)
        .await
        .unwrap();
    let got = recv(&bob[0], &url).await;
    assert!(app_events(&got).is_empty(), "{got:?}");
    assert!(apps::take(&bob[0].0, APP, 0, 100).unwrap().is_empty());
    assert!(
        spike_cli::requests::list(&bob[0].0).unwrap().is_empty(),
        "not a message request"
    );
    drop(alice);
}

/// Alice, Bob and Carol in a small group "Sheet"; Carol joins later.
#[tokio::test]
async fn a_late_joiner_gets_the_whole_state_then_newer_updates() {
    let url = relay().await;
    let (a, ac) = person("ga", 1, &url).await;
    let (b, bc) = person("gb", 1, &url).await;
    let (c, cc) = person("gc", 1, &url).await;
    befriend(&a, "bob", &bc);
    befriend(&b, "alice", &ac);
    befriend(&a, "carol", &cc);
    befriend(&c, "alice", &ac);
    let (alice, bob, carol) = (&a[0], &b[0], &c[0]);
    register(&[alice, bob, carol]);
    let (state, _) = groups::create(
        &alice.0,
        url.as_str(),
        "Sheet",
        &["bob".into()],
        AddBy::Admins,
    )
    .await
    .unwrap();
    let gid = state.group.clone();
    recv(bob, &url).await;
    let g = To::Group(gid.clone());
    apps::send(
        &alice.0,
        url.as_str(),
        &g,
        APP,
        "sheet-1",
        Mode::Stored,
        b"A1=1",
    )
    .await
    .unwrap();
    let got = app_events(&recv(bob, &url).await);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].group.as_deref(), Some(gid.as_str()));
    assert_eq!(history_len(bob, &gid), 0, "not in the group chat");

    groups::add(&alice.0, url.as_str(), "Sheet", &["carol".into()])
        .await
        .unwrap();
    recv(carol, &url).await;
    recv(bob, &url).await;

    // Unasked state is refused.
    let alice_owner = owner(alice);
    let carol_owner = owner(carol);
    let refused = apps::send_state(
        &bob.0,
        url.as_str(),
        &To::Member {
            group: gid.clone(),
            owner: carol_owner.clone(),
        },
        APP,
        "sheet-1",
        b"forged state",
    )
    .await
    .unwrap();
    assert!(refused.delivered > 0);
    let got = recv(carol, &url).await;
    assert!(
        got.iter().any(|o| matches!(o, Outcome::Rejected { .. })),
        "{got:?}"
    );

    // Carol asks the group; Alice's app answers with a snapshot.
    apps::request_state(&carol.0, url.as_str(), &g, APP, "sheet-1")
        .await
        .unwrap();
    let asks = app_events(&recv(alice, &url).await);
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].kind, EventKind::StateRequest);
    assert_eq!(asks[0].owner, carol_owner);
    let to = asks[0].reply_to(&alice.0).unwrap();
    assert_eq!(
        to,
        To::Member {
            group: gid.clone(),
            owner: carol_owner.clone()
        }
    );
    let snapshot = vec![7u8; 200_000];
    apps::send_state(&alice.0, url.as_str(), &to, APP, "sheet-1", &snapshot)
        .await
        .unwrap();
    // Bob saw the request too and leaves it unanswered.
    assert_eq!(
        app_events(&recv(bob, &url).await)[0].kind,
        EventKind::StateRequest
    );

    let got = app_events(&recv(carol, &url).await);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        (got[0].kind, got[0].owner.as_str()),
        (EventKind::State, alice_owner.as_str())
    );
    assert!(got[0].snapshot.is_none(), "the file key stays sealed here");
    let taken = apps::take(&carol.0, APP, 0, 100).unwrap();
    assert_eq!(taken.len(), 1);
    let bytes = apps::snapshot(&carol.0, url.as_str(), APP, taken[0].seq)
        .await
        .unwrap();
    assert_eq!(bytes, snapshot);
    assert!(
        apps::snapshot(&carol.0, url.as_str(), APP, taken[0].seq)
            .await
            .is_err(),
        "read once"
    );

    // A second answer to the same request is refused: the request was used.
    apps::send_state(&alice.0, url.as_str(), &to, APP, "sheet-1", b"again")
        .await
        .unwrap();
    assert!(recv(carol, &url)
        .await
        .iter()
        .any(|o| matches!(o, Outcome::Rejected { .. })));

    // Newer updates arrive after the snapshot.
    apps::send(
        &bob.0,
        url.as_str(),
        &g,
        APP,
        "sheet-1",
        Mode::Stored,
        b"A2=2",
    )
    .await
    .unwrap();
    let got = app_events(&recv(carol, &url).await);
    assert_eq!(got.len(), 1);
    assert_eq!(bytes_of(&got[0]), b"A2=2");
}

#[tokio::test]
async fn a_removed_member_cannot_send_app_data_to_the_group() {
    let url = relay().await;
    let (a, ac) = person("ra", 1, &url).await;
    let (b, bc) = person("rb", 1, &url).await;
    befriend(&a, "bob", &bc);
    befriend(&b, "alice", &ac);
    let (alice, bob) = (&a[0], &b[0]);
    register(&[alice, bob]);
    let (state, _) = groups::create(
        &alice.0,
        url.as_str(),
        "Sheet",
        &["bob".into()],
        AddBy::Admins,
    )
    .await
    .unwrap();
    recv(bob, &url).await;
    let old = groups::find(&bob.0, "Sheet")
        .unwrap()
        .current()
        .unwrap()
        .hash();
    groups::remove(&alice.0, url.as_str(), "Sheet", "bob")
        .await
        .unwrap();
    recv(bob, &url).await;
    assert!(apps::send(
        &bob.0,
        url.as_str(),
        &To::Group(state.group.clone()),
        APP,
        "sheet-1",
        Mode::Stored,
        b"x"
    )
    .await
    .is_err());
    let mut env =
        Envelope::app(&AppData::new(APP, "sheet-1", Mode::Stored, b"x").unwrap()).unwrap();
    env.group = Some(state.group.clone());
    env.gstate = Some(old);
    ops::deliver_envelope(&bob.0, url.as_str(), "alice", env)
        .await
        .unwrap();
    let got = recv(alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::Rejected { .. }]),
        "{got:?}"
    );
    assert!(apps::take(&alice.0, APP, 0, 100).unwrap().is_empty());
}
