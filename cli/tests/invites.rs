//! One-time invite links (ADR 0039): a link opened once makes two people contacts, not verified.

use spike_cli::{invites, ops, state::Home, Outcome};
use spike_core::envelope::Envelope;
use spike_core::identity::Identity;
use spike_core::vault::KdfParams;
use spike_relay::{serve, Config};
use std::path::PathBuf;
use std::time::Duration;

const PASS: &str = "test passphrase invites";
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
    let dir = std::env::temp_dir().join(format!("spike-inv-{tag}-{n}"));
    let d = Dev(
        Home::create(&dir, PASS, KdfParams::insecure_for_tests(), false).unwrap(),
        dir,
    );
    ops::publish_keys(&d.0, url).await.unwrap();
    d
}
async fn recv(d: &Dev, url: &str) -> Vec<Outcome> {
    let mut out = vec![];
    ops::recv(&d.0, url, IDLE, |o| out.push(o)).await.unwrap();
    out
}
fn contact(d: &Dev, name: &str) -> Option<bool> {
    d.0.load_contacts()
        .unwrap()
        .into_iter()
        .find(|c| c.name == name)
        .map(|c| c.verified)
}

#[tokio::test]
async fn a_link_opened_once_makes_both_people_contacts_not_verified() {
    let url = relay().await;
    let (alice, bob, carol) = (
        dev("alice", &url).await,
        dev("bob", &url).await,
        dev("carol", &url).await,
    );
    alice.0.set_profile_name("Alice Moreau").unwrap();
    bob.0.set_profile_name("Bob Tanaka").unwrap();

    let link = invites::create(&alice.0, &url).await.unwrap();
    assert!(link.starts_with("spikeinvite1:"));
    assert!(
        !link.contains(&alice.0.identity_card().unwrap()),
        "the link does not carry the card"
    );

    let r = invites::redeem(&bob.0, &url, &link, None).await.unwrap();
    assert_eq!(r.name, "Alice Moreau", "the name the inviter offers");
    assert!(!r.existing);
    assert_eq!(
        contact(&bob, "Alice Moreau"),
        Some(false),
        "arrives unverified"
    );

    let got = recv(&alice, &url).await;
    assert!(
        matches!(got.as_slice(), [Outcome::InviteJoined { name, .. }] if name == "Bob Tanaka"),
        "{got:?}"
    );
    assert_eq!(
        contact(&alice, "Bob Tanaka"),
        Some(false),
        "arrives unverified on the inviter's side too"
    );
    assert!(
        spike_cli::requests::list(&alice.0).unwrap().is_empty(),
        "not left as a request"
    );
    assert!(invites::list(&alice.0).unwrap()[0].used.is_some());

    // They can talk straight away, both ways.
    ops::send(&alice.0, &url, "Bob Tanaka", b"hi bob")
        .await
        .unwrap();
    let got = recv(&bob, &url).await;
    assert!(
        got.iter().any(
            |o| matches!(o, Outcome::Message { body, verified: false, .. } if body == b"hi bob")
        ),
        "{got:?}"
    );
    ops::send(&bob.0, &url, "Alice Moreau", b"hi alice")
        .await
        .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        got.iter()
            .any(|o| matches!(o, Outcome::Message { body, .. } if body == b"hi alice")),
        "{got:?}"
    );

    // A second opening fails and adds nobody.
    let err = invites::redeem(&carol.0, &url, &link, Some("Alice"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), invites::USED_OR_EXPIRED);
    assert!(carol.0.load_contacts().unwrap().is_empty());
}

#[tokio::test]
async fn an_expired_invite_cannot_be_opened() {
    let url = relay().await;
    let (alice, bob) = (dev("alice", &url).await, dev("bob", &url).await);
    let link = invites::create_with_ttl(&alice.0, &url, 1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let err = invites::redeem(&bob.0, &url, &link, Some("Alice"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), invites::USED_OR_EXPIRED);
    assert!(bob.0.load_contacts().unwrap().is_empty());
}

#[tokio::test]
async fn the_relay_drops_an_invite_after_its_ttl() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(
        l,
        Config {
            invite_ttl: Duration::from_millis(1),
            ..Config::default()
        },
    ));
    let (alice, bob) = (dev("alice", &url).await, dev("bob", &url).await);
    let link = invites::create(&alice.0, &url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let err = invites::redeem(&bob.0, &url, &link, None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), invites::USED_OR_EXPIRED);
}

#[tokio::test]
async fn a_hello_with_a_wrong_token_is_dropped() {
    let url = relay().await;
    let (alice, mallory) = (dev("alice", &url).await, dev("mallory", &url).await);
    invites::create(&alice.0, &url).await.unwrap();
    // Mallory has Alice's card from somewhere, but not the link.
    mallory
        .0
        .add_contact("alice", &alice.0.identity_card().unwrap(), false)
        .unwrap();
    let guess = spike_core::invite::new_token();
    ops::deliver_envelope(
        &mallory.0,
        &url,
        "alice",
        Envelope::invite_hello(&guess).unwrap(),
    )
    .await
    .unwrap();
    let got = recv(&alice, &url).await;
    assert!(
        !got.iter()
            .any(|o| matches!(o, Outcome::InviteJoined { .. })),
        "{got:?}"
    );
    assert!(alice.0.load_contacts().unwrap().is_empty());
    assert!(spike_cli::requests::list(&alice.0).unwrap().is_empty());
    assert!(
        invites::list(&alice.0).unwrap()[0].used.is_none(),
        "the real invite stays open"
    );
}

#[tokio::test]
async fn opening_your_own_link_or_a_damaged_one_fails() {
    let url = relay().await;
    let alice = dev("alice", &url).await;
    let link = invites::create(&alice.0, &url).await.unwrap();
    assert!(invites::redeem(&alice.0, &url, "spike2:nope", None)
        .await
        .is_err());
    let err = invites::redeem(&alice.0, &url, &link, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("your own"), "{err}");
}
