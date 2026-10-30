//! What the relay client needs from the outside world besides storage (ADR 0033, step 3): a way to open a
//! connection ([`Connector`]), a connection that carries text frames ([`Socket`]), and a clock
//! ([`spike_core::clock::Clock`]). The native implementations are here: `tokio-tungstenite` with the TLS rules
//! of [`crate::transport`], and `tokio`'s timers. A browser brings `WebSocket` and `setTimeout` instead.
//!
//! The relay speaks JSON in WebSocket text frames, so a frame here is a `String`.

use crate::transport::{dial, Target, Ws};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use spike_core::clock::{Clock, MaybeSend, MaybeSync};
use std::future::Future;
use std::sync::OnceLock;
use std::time::Instant;
use tokio_tungstenite::tungstenite::Message;

/// A connection that carries the relay's text frames.
pub trait Socket: MaybeSend {
    /// Send one text frame.
    fn send(&mut self, frame: String) -> impl Future<Output = Result<()>> + MaybeSend;
    /// The next text frame; `None` once the relay has closed the connection. Must be cancel-safe: the caller
    /// drops this future when a timer fires first, and no frame may be lost by that.
    fn recv(&mut self) -> impl Future<Output = Result<Option<String>>> + MaybeSend;
}

/// Opens connections to a relay.
pub trait Connector: MaybeSend + MaybeSync {
    type Socket: Socket;
    fn connect(&self, target: &Target) -> impl Future<Output = Result<Self::Socket>> + MaybeSend;
}

/// The native socket: a `tokio-tungstenite` WebSocket (TLS or loopback, see [`crate::transport`]).
pub struct WsSocket(Ws);

impl Socket for WsSocket {
    async fn send(&mut self, frame: String) -> Result<()> {
        self.0.send(Message::Text(frame.into())).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<String>> {
        loop {
            match self.0.next().await {
                None => return Ok(None),
                Some(m) => match m? {
                    Message::Text(t) => return Ok(Some(t.as_str().to_string())),
                    Message::Close(_) => return Ok(None),
                    _ => {}
                },
            }
        }
    }
}

/// Opens [`WsSocket`]s with [`crate::transport::dial`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WsConnector;

impl Connector for WsConnector {
    type Socket = WsSocket;

    async fn connect(&self, target: &Target) -> Result<WsSocket> {
        Ok(WsSocket(dial(target).await?))
    }
}

/// The native clock: the system clock and `tokio`'s timers.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioClock;

impl Clock for TokioClock {
    fn now_ms(&self) -> u64 {
        spike_core::clock::now_ms()
    }

    fn mono_ms(&self) -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u64
    }

    async fn sleep(&self, ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await
    }
}

/// Run `fut`, giving up (`None`) after `ms` milliseconds on `clock`. The `Clock` version of `tokio::time::timeout`.
pub async fn timeout<C: Clock, F: Future>(clock: &C, ms: u64, fut: F) -> Option<F::Output> {
    use futures_util::future::{select, Either};
    use futures_util::pin_mut;
    let sleep = clock.sleep(ms);
    pin_mut!(fut);
    pin_mut!(sleep);
    match select(fut, sleep).await {
        Either::Left((v, _)) => Some(v),
        Either::Right(_) => None,
    }
}
