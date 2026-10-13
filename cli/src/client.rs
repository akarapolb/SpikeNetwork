//! Minimal relay client: authenticate, send, receive, ack.
//! See [`crate::transport`] for how the connection is made (wss, or ws to loopback).

use crate::transport::{dial, Target, Ws};
use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use spike_core::identity::{Identity, SignedPrekey};
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
}

impl Relay {
    pub async fn connect(target: impl Into<Target>, me: &Identity) -> Result<Self> {
        let ws = dial(&target.into()).await?;
        let mut r = Self {
            ws,
            pending: VecDeque::new(),
            device_id: String::new(),
            prekeys: 0,
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
                ServerMsg::Ready { device_id, prekeys } => {
                    r.device_id = device_id;
                    r.prekeys = prekeys;
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

    /// Take one of `device`'s prekeys; `None` if the relay has none left.
    pub async fn claim_prekey(&mut self, device: &str) -> Result<Option<SignedPrekey>> {
        self.write(&ClientMsg::ClaimPrekey {
            device: device.into(),
        })
        .await?;
        loop {
            match self.read().await? {
                ServerMsg::Prekey { prekey, .. } => {
                    return Ok(prekey.map(|p| SignedPrekey::from_wire(&p)).transpose()?)
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
