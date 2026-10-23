//! MLS over the real relay (ADR 0025): outer-sealed commits and messages on a group mailbox, the
//! relay's compare-and-append deciding which commit wins, and a removal moving the group to a
//! mailbox the removed member cannot reach.

mod support;

use spike_cli::client::{Appended, Relay};
use spike_core::identity::Identity;
use spike_core::wire::GroupKind;
use spike_mls::credential::MapDirectory;
use spike_mls::group::{Device, Group, Outgoing, Processed};
use spike_mls::outer::{self, Kind};
use spike_mls::policy::Policy;
use spike_relay::{serve, Config};
use support::{directory, Person};
use tokio::net::TcpListener;

async fn relay() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(serve(l, Config::default()));
    url
}

/// One device with a relay connection and its place in the group mailbox.
struct Node {
    relay: Relay,
    dev: Device,
    group: Option<Group>,
    /// The last relay position this node has processed in the current mailbox.
    cursor: u64,
    heard: Vec<Vec<u8>>,
}

impl Node {
    async fn new(url: &str, p: &Person) -> Self {
        let id = Identity::generate().unwrap();
        Self {
            relay: Relay::connect(url, &id).await.unwrap(),
            dev: p.mls_device(0),
            group: None,
            cursor: 0,
            heard: vec![],
        }
    }
    fn g(&mut self) -> &mut Group {
        self.group.as_mut().unwrap()
    }
    fn add(
        &mut self,
        dir: &MapDirectory,
        kps: &[Vec<u8>],
    ) -> Result<Outgoing, spike_mls::group::Error> {
        self.group.as_mut().unwrap().add(&self.dev, dir, kps)
    }
    fn remove(&mut self, devices: &[[u8; 16]]) -> Result<Outgoing, spike_mls::group::Error> {
        self.group.as_mut().unwrap().remove(&self.dev, devices)
    }
    fn confirm(&mut self) {
        self.group.as_mut().unwrap().confirm(&self.dev).unwrap()
    }
    fn discard(&mut self) {
        self.group.as_mut().unwrap().discard(&self.dev).unwrap()
    }

    /// Send a commit to the group mailbox, sealing it to its successor when it rotates.
    async fn publish(&mut self, out: &Outgoing) -> Appended {
        let g = self.group.as_ref().unwrap();
        let (gid, generation) = (g.id(), g.mailbox_generation());
        let m = g.mailbox_material();
        let blob = outer::seal(&m, &gid, generation, Kind::Commit, &out.commit);
        let next = out.successor.map(|(_, s)| outer::Material::derive(&s));
        let seal = next.as_ref().map(|n| (n.mailbox_id(), n.mailbox_key()));
        let seq = self.cursor + 1;
        self.relay
            .group_append(
                &m.mailbox_key(),
                &m.mailbox_id(),
                GroupKind::Commit,
                Some(seq),
                &blob,
                seal.as_ref().map(|(i, k)| (i.as_str(), k)),
            )
            .await
            .unwrap()
    }

    async fn say(&mut self, text: &[u8]) {
        let wire = self
            .group
            .as_mut()
            .unwrap()
            .encrypt(&self.dev, text)
            .unwrap();
        let g = self.group.as_ref().unwrap();
        let m = g.mailbox_material();
        let blob = outer::seal(&m, &g.id(), g.mailbox_generation(), Kind::Message, &wire);
        self.relay
            .group_append(
                &m.mailbox_key(),
                &m.mailbox_id(),
                GroupKind::Message,
                None,
                &blob,
                None,
            )
            .await
            .unwrap();
    }

    /// Read and process everything new. Returns how many commits were merged.
    async fn sync(&mut self, dir: &MapDirectory) -> usize {
        let mut merged = 0;
        loop {
            let g = self.group.as_ref().unwrap();
            let m = g.mailbox_material();
            let page = self
                .relay
                .group_read(&m.mailbox_key(), &m.mailbox_id(), self.cursor, 100)
                .await
                .unwrap();
            let generation = g.mailbox_generation();
            let gid = g.id();
            if page.entries.is_empty() {
                return merged;
            }
            for e in page.entries {
                self.cursor = e.seq;
                let kind = match e.kind {
                    GroupKind::Commit => Kind::Commit,
                    GroupKind::Message => Kind::Message,
                };
                let Ok(inner) = outer::open(&m, &gid, generation, kind, &e.blob) else {
                    continue;
                };
                match self.group.as_mut().unwrap().process(&self.dev, dir, &inner) {
                    Ok(Processed::Message(t)) => self.heard.push(t),
                    Ok(Processed::Commit {
                        rotated,
                        removed_me,
                        ..
                    }) => {
                        merged += 1;
                        if rotated && !removed_me {
                            // The successor opens at position 1.
                            self.cursor = 0;
                            break;
                        }
                        if removed_me {
                            return merged;
                        }
                    }
                    Ok(Processed::Own) => {}
                    Err(_) => {}
                }
            }
        }
    }
}

fn policy_with(admins: &[&Person]) -> Policy {
    Policy::new(admins.iter().map(|p| p.owner_pub()).collect())
}

async fn start(
    url: &str,
    alice: &Person,
    others: &[&Person],
    dir: &MapDirectory,
    admins: &[&Person],
) -> (Node, Vec<Node>) {
    let mut a = Node::new(url, alice).await;
    a.group = Some(Group::create(&a.dev, policy_with(admins)).unwrap());
    let m = a.g().mailbox_material();
    a.relay
        .group_create(&m.mailbox_key(), &m.mailbox_id())
        .await
        .unwrap();
    let mut nodes = Vec::new();
    for p in others {
        nodes.push(Node::new(url, p).await);
    }
    let kps: Vec<_> = nodes.iter().map(|n| n.dev.key_package()).collect();
    let out = a.add(dir, &kps).unwrap();
    assert_eq!(a.publish(&out).await, Appended::Seq(1));
    a.confirm();
    a.cursor = 1;
    for n in nodes.iter_mut() {
        n.group = Some(
            Group::join(
                &n.dev,
                dir,
                out.welcome.as_ref().unwrap(),
                out.join_bundle.as_ref().unwrap(),
            )
            .unwrap(),
        );
        n.cursor = 1; // they joined at this commit
    }
    (a, nodes)
}

#[tokio::test]
async fn members_talk_through_the_relay_and_the_relay_sees_only_sealed_bytes() {
    let url = relay().await;
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut rest) = start(&url, &alice, &[&bob, &carol], &dir, &[&alice]).await;
    let mut c = rest.pop().unwrap();
    let mut b = rest.pop().unwrap();

    a.say(b"hello everyone").await;
    c.say(b"hi alice").await;
    for n in [&mut a, &mut b, &mut c] {
        n.sync(&dir).await;
    }
    assert_eq!(b.heard, [b"hello everyone".to_vec(), b"hi alice".to_vec()]);
    assert_eq!(
        a.heard,
        [b"hi alice".to_vec()],
        "her own message comes back as Own, not text"
    );
    assert_eq!(c.heard, [b"hello everyone".to_vec()]);

    // What the relay stores is not an MLS message: without the mailbox key it is noise.
    let m = a.g().mailbox_material();
    let page = a
        .relay
        .group_read(&m.mailbox_key(), &m.mailbox_id(), 0, 100)
        .await
        .unwrap();
    assert!(!page.entries.is_empty());
    for e in &page.entries {
        assert!(!String::from_utf8_lossy(&e.blob).contains("hello"));
        let wrong = outer::Material::derive(&[9; 32]);
        assert!(outer::open(&wrong, &a.g().id(), 0, Kind::Message, &e.blob).is_err());
    }
}

#[tokio::test]
async fn the_relay_picks_the_winning_commit_and_the_loser_redoes_it_on_top() {
    let url = relay().await;
    let (alice, bob, carol, dave) = (
        Person::new(1),
        Person::new(1),
        Person::new(1),
        Person::new(1),
    );
    let dir = directory(&[&alice, &bob, &carol, &dave]);
    let (mut a, mut rest) = start(&url, &alice, &[&bob], &dir, &[&alice, &bob]).await;
    let mut b = rest.pop().unwrap();
    let (cn, dn) = (Node::new(&url, &carol).await, Node::new(&url, &dave).await);

    // Both build a commit on the same state and race for the same position.
    let mine = a.add(&dir, &[cn.dev.key_package()]).unwrap();
    let theirs = b.add(&dir, &[dn.dev.key_package()]).unwrap();
    assert_eq!(a.publish(&mine).await, Appended::Seq(2));
    let lost = b.publish(&theirs).await;
    assert!(
        matches!(lost, Appended::Conflict { next_seq: 3 }),
        "{lost:?}"
    );

    // Bob drops his pending commit, catches up on Alice's, and redoes his on top.
    b.discard();
    a.confirm();
    a.cursor = 2;
    assert_eq!(b.sync(&dir).await, 1);
    assert_eq!(b.g().members().len(), 3);
    let again = b.add(&dir, &[dn.dev.key_package()]).unwrap();
    assert_eq!(b.publish(&again).await, Appended::Seq(3));
    b.confirm();
    b.cursor = 3;
    assert_eq!(a.sync(&dir).await, 1);
    assert_eq!(a.g().members().len(), 4);
    assert_eq!(a.g().epoch(), b.g().epoch());
}

#[tokio::test]
async fn a_removal_moves_the_group_to_a_mailbox_the_removed_member_cannot_use() {
    let url = relay().await;
    let (alice, bob, carol) = (Person::new(1), Person::new(1), Person::new(1));
    let dir = directory(&[&alice, &bob, &carol]);
    let (mut a, mut rest) = start(&url, &alice, &[&bob, &carol], &dir, &[&alice]).await;
    let mut c = rest.pop().unwrap();
    let mut b = rest.pop().unwrap();
    let old = a.g().mailbox_material();
    let old_id = old.mailbox_id();

    let out = a.remove(&[carol.device_bytes(0)]).unwrap();
    assert_eq!(a.publish(&out).await, Appended::Seq(2));
    a.confirm();
    a.cursor = 0;

    // Bob follows by himself: the successor's id and key come out of the new epoch.
    assert_eq!(b.sync(&dir).await, 1);
    assert_eq!(b.g().mailbox_generation(), 1);
    let new = b.g().mailbox_material();
    assert_ne!(new.mailbox_id(), old_id);
    assert_eq!(new.mailbox_id(), a.g().mailbox_material().mailbox_id());

    // Carol reads the removal on the old mailbox, learns she is out, and has no way forward.
    c.sync(&dir).await;
    assert!(!c.g().is_active());
    assert_eq!(c.g().mailbox_material().mailbox_id(), old_id);
    let page = c
        .relay
        .group_read(&old.mailbox_key(), &old_id, 0, 10)
        .await
        .unwrap();
    assert!(page.sealed, "the old mailbox is closed");
    let e = c
        .relay
        .group_append(
            &old.mailbox_key(),
            &old_id,
            GroupKind::Message,
            None,
            b"let me in",
            None,
        )
        .await;
    assert!(e.is_err(), "nothing can be appended to a sealed mailbox");
    let e = c
        .relay
        .group_read(&old.mailbox_key(), &new.mailbox_id(), 0, 10)
        .await;
    assert!(e.is_err(), "the old key does not open the new mailbox");

    // The group carries on there.
    b.say(b"just us now").await;
    a.sync(&dir).await;
    assert_eq!(a.heard, [b"just us now".to_vec()]);
}
