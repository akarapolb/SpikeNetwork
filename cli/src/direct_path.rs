//! The one thing `ops` needs from the native `direct` module (ADR 0033, step 5): whether direct connections are
//! on, an attempt to deliver a blob straight to a device, and a note when that failed. `Home` carries an optional
//! `DirectPath`. The native constructors set the real one; a browser sets none, so every send goes through the
//! relay, which is what already happens when direct connections are off.
//!
//! The hook is object safe (`Home` holds it as `Arc<dyn DirectPath>`), so its methods take the `Home` type-erased, as `&AnyHome`,
//! rather than as a generic argument. An implementation that does not recognise it says "off".

use crate::store::{MaybeSend, MaybeSync};
use anyhow::Result;
use spike_core::identity::{DeviceCard, Identity};
use std::any::Any;
use std::future::Future;
use std::pin::Pin;

/// The `Home`, type-erased. `Send + Sync` on native so the boxed future below can be spawned.
#[cfg(not(target_arch = "wasm32"))]
pub type AnyHome = dyn Any + Send + Sync;
#[cfg(target_arch = "wasm32")]
pub type AnyHome = dyn Any;

/// A boxed future, `Send` wherever the native app needs it (see [`crate::store`]).
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait DirectPath: MaybeSend + MaybeSync {
    /// Whether to try a direct connection first.
    fn enabled(&self, home: &AnyHome) -> bool;
    /// Offer `blob` to `card`'s device directly. `Ok` means the device stored it and said so; any error means it
    /// did not, and the caller uses the relay.
    fn try_send<'a>(
        &'a self,
        home: &'a AnyHome,
        me: &'a Identity,
        card: &'a DeviceCard,
        blob: &'a [u8],
    ) -> BoxFuture<'a, Result<()>>;
    /// A direct attempt failed and the relay is used instead.
    fn note_fallback(&self, e: &anyhow::Error);
}
