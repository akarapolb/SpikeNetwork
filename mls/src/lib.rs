//! Spike Connect large groups: MLS (RFC 9420, via openmls) bound to the owner-key identity model.
//! See `docs/adr/0025-large-groups-with-mls.md`.

pub mod credential;
pub mod group;
pub mod outer;
pub mod policy;
pub mod state;
#[cfg(feature = "test-hooks")]
pub mod testing;

use openmls::prelude::Ciphersuite;

/// The one ciphersuite (ADR 0025 section 2).
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
