//! The chat store behind a trait (ADR 0033, step 4). The receive path and the sync code record, read, edit and
//! merge messages through `History`; the native implementation is the SQLite [`MessageStore`] as it has always
//! been. A browser brings its own (in memory, or IndexedDB) in a later stage.
//!
//! The list is the nine operations the moved modules call (`docs/adr/0033-call-sites.md`, section 7).
//! `prune_older_than` and `contains` are not here: only the command line uses them, so they stay on the native
//! store. The futures are `MaybeSend` for the same reason as in [`crate::store`]: native code spawns them, a
//! browser's handles are not `Send`.

use crate::messages::{Reaction, StoredMessage};
use anyhow::Result;
use std::future::Future;

pub use spike_core::clock::MaybeSend;

/// Unlike `Store` this does not require `Sync`: the SQLite connection behind the native implementation is not
/// `Sync`, and the store is opened where it is used, never shared between tasks. Shared code that must hold a
/// `&H` across an `.await` on a spawned task adds `H: Sync` itself; the browser stores need no such bound.
pub trait History: MaybeSend {
    /// Store a message. `false` if a message with this id is already stored (redelivery, replay, a second copy).
    /// Reactions that arrived before it, in the same conversation, join it here.
    fn insert(&self, m: &StoredMessage) -> impl Future<Output = Result<bool>> + MaybeSend;

    /// The stored message with exactly this id, if any.
    fn get(&self, id: &str) -> impl Future<Output = Result<Option<StoredMessage>>> + MaybeSend;

    /// Replace the content of an already stored message (same id and conversation). An error if it is not stored.
    fn update(&self, m: &StoredMessage) -> impl Future<Output = Result<()>> + MaybeSend;

    /// The message with exactly this id, or the only one whose id starts with `prefix` (at least 6 characters).
    /// An error if there is none, or more than one.
    fn find(&self, prefix: &str) -> impl Future<Output = Result<StoredMessage>> + MaybeSend;

    /// The last `limit` messages with `peer`, oldest first.
    fn conversation(
        &self,
        peer: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<StoredMessage>>> + MaybeSend;

    /// Every stored message, oldest first (history bundles for a new device).
    fn all(&self) -> impl Future<Output = Result<Vec<StoredMessage>>> + MaybeSend;

    /// Messages this device learned or changed at or after `since_ms` (the local clock).
    fn changed_since(
        &self,
        since_ms: u64,
    ) -> impl Future<Output = Result<Vec<StoredMessage>>> + MaybeSend;

    /// Fold one record from another of the person's own devices into the local history: unknown messages are
    /// inserted, a delete wins, a newer edit wins, reactions merge per person. Whether anything changed.
    fn merge_record(&self, r: &StoredMessage) -> impl Future<Output = Result<bool>> + MaybeSend;

    /// Keep a reaction to a message that is not here yet. Per person and message only the newest waits.
    fn park_reaction(
        &self,
        target: &str,
        conv: &str,
        reactor: &str,
        r: &Reaction,
    ) -> impl Future<Output = Result<()>> + MaybeSend;
}
