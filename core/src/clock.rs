//! Time, and the `Send` markers that let one trait serve native code and the browser (ADR 0033, step 3).
//!
//! `SystemTime::now()` compiles for `wasm32-unknown-unknown` and panics when it runs, so no code that has to
//! run in a browser may call it directly. [`now_ms`] is the wall clock for every platform: the system clock
//! natively, the page's `Date.now()` in the browser. [`Clock`] is the same thing as a trait, with a monotonic
//! reading and a sleep, for code that has deadlines (the relay client) and for tests that want to control time.

use std::future::Future;

/// `Send` on native, nothing on `wasm32`. The native app spawns the client's futures, so they must be `Send`
/// there; a browser's `WebSocket` and IndexedDB handles are not `Send`, and nothing on that side spawns across
/// threads. A plain `async fn` in a trait promises neither, so trait methods return
/// `impl Future<..> + MaybeSend` instead.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}

/// `Sync` on native, nothing on `wasm32`.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync> MaybeSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSync for T {}

/// Milliseconds since the Unix epoch on the wall clock of this platform. 0 if the clock is before 1970.
#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Milliseconds since the Unix epoch on the page's clock (`Date.now()`).
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Time as the client needs it. Written so that a browser can provide it (`Date.now`, `performance.now`,
/// `setTimeout`) and a test can drive it by hand.
pub trait Clock: MaybeSend + MaybeSync {
    /// Wall-clock milliseconds since the Unix epoch (what envelopes are stamped with).
    fn now_ms(&self) -> u64;
    /// A monotonic reading in milliseconds, from an arbitrary start. Only differences mean anything.
    fn mono_ms(&self) -> u64;
    /// Complete after `ms` milliseconds.
    fn sleep(&self, ms: u64) -> impl Future<Output = ()> + MaybeSend;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wall_clock_is_after_2024_and_does_not_go_backwards() {
        let a = now_ms();
        let b = now_ms();
        assert!(a > 1_700_000_000_000, "{a}");
        assert!(b >= a);
    }
}
