//! Minimal relay client: authenticate, send, receive, ack.
//! See [`crate::transport`] for how the connection is made (wss, or ws to loopback).

use crate::transport::{dial, Target, Ws};
use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use spike_core::identity::{Identity, SignedPrekey};
use spike_core::mailbox::{self, MailboxKey};
use spike_core::owner::DeviceList;
use spike_core::wire::{ClientMsg, GroupKind, ServerMsg, WireEntry, WireSeal};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

/// Credentials for the relay operator's TURN server (ADR 0028).
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct TurnServers {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub ttl_secs: u64,
}

#[derive(Debug, Clone)]
pub struct Delivery {
    pub id: String,
    pub blob: Vec<u8>,
}

/// One entry of a group mailbox (ADR 0025): its position, what it is, and its opaque contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupEntry {
    pub seq: u64,
    pub kind: GroupKind,
    pub blob: Vec<u8>,
}

/// Something the relay pushed: a one-to-one delivery, or an entry in a subscribed group mailbox
/// `(mailbox, entry, sealed)`.
#[derive(Debug, Clone)]
pub enum Incoming {
    Delivery(Delivery),
    Group((String, GroupEntry, bool)),
}

/// What a read of a group mailbox returned.
#[derive(Debug, Clone)]
pub struct GroupPage {
    pub entries: Vec<GroupEntry>,
    /// The position the next append will get (not the end of this page).
    pub next_seq: u64,
    /// The mailbox was closed by a commit that opened another.
    pub sealed: bool,
}

/// How an append to a group mailbox ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended {
    Seq(u64),
    /// A commit expected another position; the log is really at `next_seq`.
    Conflict {
        next_seq: u64,
    },
}

fn group_entry(e: WireEntry) -> Result<GroupEntry> {
    Ok(GroupEntry {
        seq: e.seq,
        kind: e.kind,
        blob: hex::decode(e.blob)?,
    })
}

pub struct Relay {
    ws: Ws,
    pending: VecDeque<Delivery>,
    pub device_id: String,
    /// How many of our prekeys the relay held when we connected (updated on publish).
    pub prekeys: usize,
    /// Age of our fallback prekey on the relay, if it holds one.
    pub fallback_age_secs: Option<u64>,
    /// While waiting for deliveries, ping this often.
    keepalive: Duration,
    /// How long an answer to a ping may take before the connection counts as lost.
    pong_wait: Duration,
    next_ping: Instant,
    /// Set after a ping went out; cleared by anything the relay sends.
    pong_due: Option<Instant>,
    /// Group entries pushed while this connection was waiting for something else.
    group_pending: VecDeque<(String, GroupEntry, bool)>,
}

/// Ping every 25 s (under the 60 s idle timeout many proxies use) and give up after 15 s of silence.
const KEEPALIVE: Duration = Duration::from_secs(25);
const PONG_WAIT: Duration = Duration::from_secs(15);

impl Relay {
    pub async fn connect(target: impl Into<Target>, me: &Identity) -> Result<Self> {
        let target = target.into();
        let audience = target.audience();
        let ws = dial(&target).await?;
        let mut r = Self {
            ws,
            pending: VecDeque::new(),
            device_id: String::new(),
            prekeys: 0,
            fallback_age_secs: None,
            keepalive: KEEPALIVE,
            pong_wait: PONG_WAIT,
            next_ping: Instant::now() + KEEPALIVE,
            pong_due: None,
            group_pending: VecDeque::new(),
        };
        let ServerMsg::Challenge { nonce } = r.read().await? else {
            bail!("relay did not send a challenge")
        };
        let nonce = hex::decode(nonce)?;
        r.write(&ClientMsg::Auth {
            ed25519: hex::encode(me.ed_public()),
            sig: hex::encode(me.sign_auth(&nonce, &audience)),
        })
        .await?;
        loop {
            match r.read().await? {
                ServerMsg::Ready {
                    device_id,
                    prekeys,
                    fallback_age_secs,
                } => {
                    r.device_id = device_id;
                    r.prekeys = prekeys;
                    r.fallback_age_secs = fallback_age_secs;
                    return Ok(r);
                }
                ServerMsg::Error { code, message } => bail!("relay refused: {code}: {message}"),
                _ => {}
            }
        }
    }

    /// Queue `blob` for `to`. Returns the relay's id for it.
    pub async fn send(&mut self, to: &str, blob: &[u8]) -> Result<String> {
        self.write(&ClientMsg::Send {
            to: to.into(),
            blob: hex::encode(blob),
        })
        .await?;
        loop {
            match self.read().await? {
                ServerMsg::Accepted { id } => return Ok(id),
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Change how often a waiting connection pings and how long it waits for the answer.
    /// The defaults (25 s and 15 s) suit real networks; tests use short ones.
    pub fn set_keepalive(&mut self, interval: Duration, pong_wait: Duration) {
        self.keepalive = interval;
        self.pong_wait = pong_wait;
        self.next_ping = Instant::now() + interval;
        self.pong_due = None;
    }

    /// Round-trip time to the relay: send a ping and wait for the pong. Deliveries that
    /// arrive meanwhile are kept for `next_delivery`.
    pub async fn ping(&mut self) -> Result<Duration> {
        let t = Instant::now();
        self.write(&ClientMsg::Ping).await?;
        loop {
            match self.read().await? {
                ServerMsg::Pong => return Ok(t.elapsed()),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                _ => {}
            }
        }
    }

    /// Next delivery, waiting up to `idle` (forever if `None`). `Ok(None)` means
    /// nothing arrived within the idle window. While waiting, the connection pings the
    /// relay now and then; if the relay stops answering, this returns an error so the
    /// caller can reconnect (a router may have dropped the connection without telling us).
    /// Group entries pushed meanwhile are kept for [`next_group_entry`](Self::next_group_entry).
    pub async fn next_delivery(&mut self, idle: Option<Duration>) -> Result<Option<Delivery>> {
        let deadline = idle.map(|d| Instant::now() + d);
        loop {
            match self.incoming_until(deadline, false).await? {
                None => return Ok(None),
                Some(Incoming::Delivery(d)) => return Ok(Some(d)),
                Some(Incoming::Group(e)) => self.group_pending.push_back(e),
            }
        }
    }

    /// Like [`next_delivery`](Self::next_delivery), but also returns the entries the relay pushes for
    /// group mailboxes this connection subscribed to. For a listener that only needs to know
    /// that *something* arrived.
    pub async fn next_incoming(&mut self, idle: Option<Duration>) -> Result<Option<Incoming>> {
        let deadline = idle.map(|d| Instant::now() + d);
        self.incoming_until(deadline, true).await
    }

    async fn incoming_until(
        &mut self,
        deadline: Option<Instant>,
        with_groups: bool,
    ) -> Result<Option<Incoming>> {
        loop {
            if let Some(d) = self.pending.pop_front() {
                return Ok(Some(Incoming::Delivery(d)));
            }
            if with_groups {
                if let Some(e) = self.group_pending.pop_front() {
                    return Ok(Some(Incoming::Group(e)));
                }
            }
            // Wake for the earliest of: the caller's idle window, the next ping, the pong deadline.
            let mut wake = self.next_ping;
            if let Some(due) = self.pong_due {
                wake = wake.min(due);
            }
            if let Some(d) = deadline {
                wake = wake.min(d);
            }
            let msg = match tokio::time::timeout_at(wake.into(), self.read_raw()).await {
                Ok(m) => {
                    // The relay is alive: anything it sends counts as an answer.
                    self.pong_due = None;
                    self.next_ping = Instant::now() + self.keepalive;
                    m?
                }
                Err(_) => {
                    let now = Instant::now();
                    if self.pong_due.is_some_and(|due| now >= due) {
                        bail!("lost the connection to the relay (no answer to a ping)");
                    }
                    if deadline.is_some_and(|d| now >= d) {
                        return Ok(None);
                    }
                    if now >= self.next_ping {
                        self.write(&ClientMsg::Ping).await?;
                        // The deadline runs from the first ping nobody has answered: a later
                        // ping must not push it back.
                        self.pong_due.get_or_insert(now + self.pong_wait);
                        self.next_ping = now + self.keepalive;
                    }
                    continue;
                }
            };
            match msg {
                ServerMsg::Deliver { id, blob } => {
                    return Ok(Some(Incoming::Delivery(Delivery {
                        id,
                        blob: hex::decode(blob)?,
                    })))
                }
                ServerMsg::GroupEntry {
                    mailbox,
                    entry,
                    sealed,
                } => {
                    return Ok(Some(Incoming::Group((
                        mailbox,
                        group_entry(entry)?,
                        sealed,
                    ))))
                }
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                _ => {}
            }
        }
    }

    // ---------------------------------------------------------- key packages and group mailboxes (ADR 0025)

    /// Upload one-time key packages (opaque bytes). Returns how many the relay now holds for us.
    pub async fn publish_key_packages(&mut self, packages: &[Vec<u8>]) -> Result<usize> {
        let packages = packages.iter().map(hex::encode).collect();
        match self
            .group_request(ClientMsg::PublishKeyPackages { packages })
            .await?
        {
            ServerMsg::KeyPackagesPublished { count } => Ok(count),
            other => bail!("unexpected answer to publishing key packages: {other:?}"),
        }
    }

    /// Replace our reusable last-resort key package.
    pub async fn publish_last_resort(&mut self, package: &[u8]) -> Result<()> {
        match self
            .group_request(ClientMsg::PublishLastResortKeyPackage {
                package: hex::encode(package),
            })
            .await?
        {
            ServerMsg::LastResortKeyPackagePublished => Ok(()),
            other => bail!("unexpected answer to publishing a last-resort key package: {other:?}"),
        }
    }

    /// Take one of `device`'s key packages: `(bytes, is_last_resort)`, or `None` if it has none.
    pub async fn claim_key_package(&mut self, device: &str) -> Result<Option<(Vec<u8>, bool)>> {
        match self
            .group_request(ClientMsg::ClaimKeyPackage {
                device: device.into(),
            })
            .await?
        {
            ServerMsg::KeyPackage {
                package,
                last_resort,
                ..
            } => Ok(package
                .map(|p| hex::decode(p).map(|b| (b, last_resort)))
                .transpose()?),
            other => bail!("unexpected answer to claiming a key package: {other:?}"),
        }
    }

    /// Send a request and return the relay's first answer to it. Deliveries and group pushes that
    /// arrive meanwhile are kept. Lets a test send a hand-made request.
    pub async fn request(&mut self, msg: ClientMsg) -> Result<ServerMsg> {
        self.write(&msg).await?;
        loop {
            match self.read().await? {
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                ServerMsg::Pong => {}
                other => return Ok(other),
            }
        }
    }

    /// Like [`request`](Self::request), but a relay error is an error.
    async fn group_request(&mut self, msg: ClientMsg) -> Result<ServerMsg> {
        match self.request(msg).await? {
            ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
            other => Ok(other),
        }
    }

    /// Register a mailbox under `key`. Whoever holds `key` can read and append.
    pub async fn group_create(&mut self, key: &MailboxKey, mailbox: &str) -> Result<()> {
        let pubkey = key.public_hex();
        let sig = key.sign(&mailbox::create_bytes(&self.device_id, mailbox, &pubkey));
        match self
            .group_request(ClientMsg::GroupCreate {
                mailbox: mailbox.into(),
                pubkey,
                sig,
            })
            .await?
        {
            ServerMsg::GroupCreated { .. } => Ok(()),
            other => bail!("unexpected answer to a mailbox registration: {other:?}"),
        }
    }

    /// Append an entry. A commit must give the position it expects (`expected_seq`); it may
    /// `seal` this mailbox, handing the group to a successor whose key proves it exists.
    pub async fn group_append(
        &mut self,
        key: &MailboxKey,
        mailbox: &str,
        kind: GroupKind,
        expected_seq: Option<u64>,
        blob: &[u8],
        seal: Option<(&str, &MailboxKey)>,
    ) -> Result<Appended> {
        let seal_pub = seal.map(|(id, k)| (id.to_string(), k.public_hex()));
        let sig = key.sign(&mailbox::append_bytes(
            &self.device_id,
            mailbox,
            kind,
            expected_seq,
            blob,
            seal_pub.as_ref().map(|(m, k)| (m.as_str(), k.as_str())),
        ));
        let seal = seal
            .zip(seal_pub)
            .map(|((_, successor), (mb, pubkey))| WireSeal {
                sig: successor.sign(&mailbox::create_bytes(&self.device_id, &mb, &pubkey)),
                mailbox: mb,
                pubkey,
            });
        match self
            .group_request(ClientMsg::GroupAppend {
                mailbox: mailbox.into(),
                kind,
                expected_seq,
                blob: hex::encode(blob),
                seal,
                sig,
            })
            .await?
        {
            ServerMsg::GroupAppended { seq, .. } => Ok(Appended::Seq(seq)),
            ServerMsg::GroupConflict { next_seq, .. } => Ok(Appended::Conflict { next_seq }),
            other => bail!("unexpected answer to an append: {other:?}"),
        }
    }

    /// Read up to `limit` entries after `after_seq`, oldest first (the relay caps a page at 200).
    pub async fn group_read(
        &mut self,
        key: &MailboxKey,
        mailbox: &str,
        after_seq: u64,
        limit: u32,
    ) -> Result<GroupPage> {
        let sig = key.sign(&mailbox::read_bytes(
            &self.device_id,
            mailbox,
            after_seq,
            limit,
        ));
        match self
            .group_request(ClientMsg::GroupRead {
                mailbox: mailbox.into(),
                after_seq,
                limit,
                sig,
            })
            .await?
        {
            ServerMsg::GroupEntries {
                entries,
                next_seq,
                sealed,
                ..
            } => Ok(GroupPage {
                entries: entries
                    .into_iter()
                    .map(group_entry)
                    .collect::<Result<_>>()?,
                next_seq,
                sealed,
            }),
            other => bail!("unexpected answer to a read: {other:?}"),
        }
    }

    /// Be pushed every entry appended from now on (see [`next_group_entry`](Self::next_group_entry)).
    /// Returns where the mailbox is: the next position, and whether it is sealed. Read from your
    /// own cursor after subscribing: pushes carry their position.
    pub async fn group_subscribe(
        &mut self,
        key: &MailboxKey,
        mailbox: &str,
    ) -> Result<(u64, bool)> {
        let sig = key.sign(&mailbox::subscribe_bytes(&self.device_id, mailbox));
        match self
            .group_request(ClientMsg::GroupSubscribe {
                mailbox: mailbox.into(),
                sig,
            })
            .await?
        {
            ServerMsg::GroupSubscribed {
                next_seq, sealed, ..
            } => Ok((next_seq, sealed)),
            other => bail!("unexpected answer to a subscription: {other:?}"),
        }
    }

    pub async fn group_unsubscribe(&mut self, mailbox: &str) -> Result<()> {
        self.write(&ClientMsg::GroupUnsubscribe {
            mailbox: mailbox.into(),
        })
        .await
    }

    /// The next pushed group entry (mailbox id, entry, whether that entry sealed the mailbox),
    /// waiting up to `idle`. `Ok(None)` means nothing arrived in that time.
    pub async fn next_group_entry(
        &mut self,
        idle: Duration,
    ) -> Result<Option<(String, GroupEntry, bool)>> {
        let deadline = Instant::now() + idle;
        loop {
            if let Some(e) = self.group_pending.pop_front() {
                return Ok(Some(e));
            }
            match tokio::time::timeout_at(deadline.into(), self.read_raw()).await {
                Err(_) => return Ok(None),
                Ok(m) => match m? {
                    ServerMsg::GroupEntry {
                        mailbox,
                        entry,
                        sealed,
                    } => return Ok(Some((mailbox, group_entry(entry)?, sealed))),
                    ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                        id,
                        blob: hex::decode(blob)?,
                    }),
                    ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                    _ => {}
                },
            }
        }
    }

    /// Upload prekeys. Returns how many the relay now holds for us.
    pub async fn publish_prekeys(&mut self, keys: &[SignedPrekey]) -> Result<usize> {
        let keys = keys.iter().map(SignedPrekey::to_wire).collect();
        self.write(&ClientMsg::PublishPrekeys { keys }).await?;
        loop {
            match self.read().await? {
                ServerMsg::Published { prekeys } => {
                    self.prekeys = prekeys;
                    return Ok(prekeys);
                }
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Upload a new fallback prekey, replacing the old one.
    pub async fn publish_fallback(&mut self, key: &SignedPrekey) -> Result<()> {
        self.write(&ClientMsg::PublishFallback { key: key.to_wire() })
            .await?;
        loop {
            match self.read().await? {
                ServerMsg::FallbackPublished => {
                    self.fallback_age_secs = Some(0);
                    return Ok(());
                }
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Take one of `device`'s prekeys; `None` if the relay has none at all.
    pub async fn claim_prekey(&mut self, device: &str) -> Result<Option<SignedPrekey>> {
        Ok(self.claim_prekey_info(device).await?.map(|(k, _)| k))
    }

    /// Like [`claim_prekey`](Self::claim_prekey), also saying whether the key
    /// was the reusable fallback (true) or a one-time key (false).
    pub async fn claim_prekey_info(
        &mut self,
        device: &str,
    ) -> Result<Option<(SignedPrekey, bool)>> {
        self.write(&ClientMsg::ClaimPrekey {
            device: device.into(),
        })
        .await?;
        loop {
            match self.read().await? {
                ServerMsg::Prekey {
                    prekey, fallback, ..
                } => {
                    return Ok(prekey
                        .map(|p| SignedPrekey::from_wire(&p))
                        .transpose()?
                        .map(|k| (k, fallback)))
                }
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Publish our person's signed device list. Returns the version the relay now holds.
    pub async fn publish_device_list(&mut self, list: &DeviceList) -> Result<u64> {
        self.write(&ClientMsg::PublishDeviceList {
            list: serde_json::to_string(list)?,
        })
        .await?;
        loop {
            match self.read().await? {
                ServerMsg::DeviceListStored { version } => return Ok(version),
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Fetch the signed device list the relay holds for `owner` (hex), unverified:
    /// callers must verify and apply the update rules before trusting it.
    pub async fn fetch_device_list(&mut self, owner: &str) -> Result<Option<DeviceList>> {
        self.write(&ClientMsg::FetchDeviceList {
            owner: owner.into(),
        })
        .await?;
        loop {
            match self.read().await? {
                ServerMsg::DeviceList { list, .. } => {
                    return Ok(match list {
                        Some(j) => Some(serde_json::from_str(&j)?),
                        None => None,
                    })
                }
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Tell the relay where to send a wake-up push for this device while it is not connected (UnifiedPush, ADR 0028).
    pub async fn register_push(&mut self, url: &str) -> Result<()> {
        self.write(&ClientMsg::RegisterPush { url: url.into() }).await?;
        loop {
            match self.read().await? {
                ServerMsg::PushRegistered => return Ok(()),
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Forget this device's push endpoint.
    pub async fn unregister_push(&mut self) -> Result<()> {
        self.write(&ClientMsg::UnregisterPush).await?;
        loop {
            match self.read().await? {
                ServerMsg::PushCleared => return Ok(()),
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Ask for credentials for the relay's TURN server. `None` if the relay has none (ADR 0028).
    pub async fn turn_credentials(&mut self) -> Result<Option<TurnServers>> {
        self.write(&ClientMsg::TurnCredentials).await?;
        loop {
            match self.read().await? {
                ServerMsg::TurnCredentials { urls, username, credential, ttl_secs } => {
                    return Ok(Some(TurnServers { urls, username, credential, ttl_secs }))
                }
                ServerMsg::Error { code, .. } if code == "no_turn" => return Ok(None),
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Upload one encrypted attachment chunk.
    pub async fn put_blob_chunk(
        &mut self,
        blob: &str,
        index: u32,
        total: u32,
        data: &[u8],
    ) -> Result<()> {
        self.write(&ClientMsg::PutBlobChunk {
            blob: blob.into(),
            index,
            total,
            data: hex::encode(data),
        })
        .await?;
        loop {
            match self.read().await? {
                ServerMsg::BlobChunkStored { .. } => return Ok(()),
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    /// Download one encrypted attachment chunk; `None` if the relay no longer has it.
    pub async fn get_blob_chunk(&mut self, blob: &str, index: u32) -> Result<Option<Vec<u8>>> {
        self.write(&ClientMsg::GetBlobChunk {
            blob: blob.into(),
            index,
        })
        .await?;
        loop {
            match self.read().await? {
                ServerMsg::BlobChunk { data, .. } => {
                    return Ok(data.map(hex::decode).transpose()?)
                }
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                ServerMsg::Deliver { id, blob } => self.pending.push_back(Delivery {
                    id,
                    blob: hex::decode(blob)?,
                }),
                _ => {}
            }
        }
    }

    pub async fn ack(&mut self, id: &str) -> Result<()> {
        self.write(&ClientMsg::Ack { id: id.into() }).await
    }

    async fn write(&mut self, m: &ClientMsg) -> Result<()> {
        self.ws
            .send(Message::Text(serde_json::to_string(m)?))
            .await?;
        Ok(())
    }

    /// The next message from the relay, whatever it is.
    async fn read_raw(&mut self) -> Result<ServerMsg> {
        loop {
            match self
                .ws
                .next()
                .await
                .ok_or_else(|| anyhow!("relay closed the connection"))??
            {
                Message::Text(t) => return Ok(serde_json::from_str(&t)?),
                Message::Close(_) => bail!("relay closed the connection"),
                _ => {}
            }
        }
    }

    /// The next message other than a group push. Pushes that arrive while something else is
    /// waited for are kept for [`next_group_entry`](Self::next_group_entry), not dropped.
    async fn read(&mut self) -> Result<ServerMsg> {
        loop {
            match self.read_raw().await? {
                ServerMsg::GroupEntry {
                    mailbox,
                    entry,
                    sealed,
                } => self
                    .group_pending
                    .push_back((mailbox, group_entry(entry)?, sealed)),
                other => return Ok(other),
            }
        }
    }
}
