//! Minimal relay client: authenticate, send, receive, ack.
//! See [`crate::transport`] for how the connection is made (wss, or ws to loopback).

use crate::transport::{dial, Target, Ws};
use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use spike_core::identity::{Identity, SignedPrekey};
use spike_core::owner::DeviceList;
use spike_core::wire::{ClientMsg, ServerMsg};
use std::collections::VecDeque;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub struct Delivery {
    pub id: String,
    pub blob: Vec<u8>,
}

pub struct Relay {
    ws: Ws,
    pending: VecDeque<Delivery>,
    pub device_id: String,
    /// How many of our prekeys the relay held when we connected (updated on publish).
    pub prekeys: usize,
    /// Age of our fallback prekey on the relay, if it holds one.
    pub fallback_age_secs: Option<u64>,
}

impl Relay {
    pub async fn connect(target: impl Into<Target>, me: &Identity) -> Result<Self> {
        let ws = dial(&target.into()).await?;
        let mut r = Self {
            ws,
            pending: VecDeque::new(),
            device_id: String::new(),
            prekeys: 0,
            fallback_age_secs: None,
        };
        let ServerMsg::Challenge { nonce } = r.read().await? else {
            bail!("relay did not send a challenge")
        };
        let nonce = hex::decode(nonce)?;
        r.write(&ClientMsg::Auth {
            ed25519: hex::encode(me.ed_public()),
            sig: hex::encode(me.sign_auth(&nonce)),
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

    /// Next delivery, waiting up to `idle` (forever if `None`). `Ok(None)` means
    /// nothing arrived within the idle window.
    pub async fn next_delivery(&mut self, idle: Option<Duration>) -> Result<Option<Delivery>> {
        loop {
            if let Some(d) = self.pending.pop_front() {
                return Ok(Some(d));
            }
            let msg = match idle {
                Some(d) => match tokio::time::timeout(d, self.read()).await {
                    Ok(m) => m?,
                    Err(_) => return Ok(None),
                },
                None => self.read().await?,
            };
            match msg {
                ServerMsg::Deliver { id, blob } => {
                    return Ok(Some(Delivery {
                        id,
                        blob: hex::decode(blob)?,
                    }))
                }
                ServerMsg::Error { code, message } => bail!("relay error: {code}: {message}"),
                _ => {}
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

    async fn read(&mut self) -> Result<ServerMsg> {
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
}
