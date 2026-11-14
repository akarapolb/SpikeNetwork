//! When a message last arrived from each of a contact's devices.
//!
//! Learned only from what this device receives: the local clock at the moment an authentic
//! message from that device was opened here. Nothing comes from the relay, and nothing is sent
//! to anyone. Kept sealed in the state directory like the rest of the state.

use crate::state::Home;
use anyhow::Result;
use std::collections::BTreeMap;

const FILE: &str = "device_seen.json";

/// Rewrite the file at most this often per device; finer detail is never shown.
const GRANULARITY_MS: u64 = 30_000;

/// owner key (hex) -> device id -> ms since the epoch, local clock.
type Seen = BTreeMap<String, BTreeMap<String, u64>>;

/// Note that a message from `device` (a device id) of the person with owner key `owner` arrived at `at`.
pub fn record(home: &Home, owner: &str, device: &str, at: u64) -> Result<()> {
    let mut all: Seen = home.load_json(FILE)?;
    let slot = all
        .entry(owner.to_string())
        .or_default()
        .entry(device.to_string())
        .or_insert(0);
    if at < slot.saturating_add(GRANULARITY_MS) && *slot != 0 {
        return Ok(());
    }
    *slot = at;
    home.save_json(FILE, &all)
}

/// When each device of `owner` was last heard from (device id -> ms). Devices never heard from are absent.
pub fn for_owner(home: &Home, owner: &str) -> Result<BTreeMap<String, u64>> {
    let mut all: Seen = home.load_json(FILE)?;
    Ok(all.remove(owner).unwrap_or_default())
}
