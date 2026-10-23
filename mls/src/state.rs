//! Persistence on the vault (ADR 0025 section 8): a device's MLS keys and openmls storage, and the
//! mailbox state of each group, sealed together under one vault name. Written after every change
//! that matters; losing it means rejoining the group, never a weaker group.

use crate::group::{Device, Error, Group};
use openmls_traits::OpenMlsProvider as _;
use serde::{Deserialize, Serialize};
use spike_core::vault::Vault;
use std::collections::{BTreeMap, HashMap};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    v: u8,
    signer_public: String,
    identity: String,
    device: String,
    list_version: u64,
    /// openmls' own storage, hex key -> hex value.
    storage: BTreeMap<String, String>,
    groups: Vec<SavedGroup>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedGroup {
    id: String,
    generation: u64,
    secret: String,
}

fn bad(what: &'static str) -> Error {
    Error::Malformed(what)
}

/// Seal a device and its groups under `name`.
pub fn save(vault: &Vault, name: &str, dev: &Device, groups: &[&Group]) -> Result<Vec<u8>, Error> {
    let json = export(dev, groups)?;
    vault.seal(name, &json).map_err(|_| bad("seal"))
}

/// The state as plaintext bytes, for a caller that seals it itself. Handle as a secret.
pub fn export(dev: &Device, groups: &[&Group]) -> Result<Vec<u8>, Error> {
    let storage = dev
        .provider
        .storage()
        .values
        .read()
        .map_err(|_| bad("storage lock"))?
        .iter()
        .map(|(k, v)| (hex::encode(k), hex::encode(v)))
        .collect();
    let saved = Saved {
        v: 1,
        signer_public: hex::encode(dev.signer.public()),
        identity: hex::encode(dev.identity_bytes()),
        device: hex::encode(dev.device),
        list_version: dev.list_version,
        storage,
        groups: groups
            .iter()
            .map(|g| {
                let (generation, secret) = g.mailbox_parts();
                SavedGroup {
                    id: hex::encode(g.id()),
                    generation,
                    secret: hex::encode(secret),
                }
            })
            .collect(),
    };
    serde_json::to_vec(&saved).map_err(|_| bad("serialize"))
}

/// Open what [`save`] sealed.
pub fn load(vault: &Vault, name: &str, sealed: &[u8]) -> Result<(Device, Vec<Group>), Error> {
    let json = vault
        .open(name, sealed)
        .map_err(|_| bad("state does not open"))?;
    import(&json)
}

/// Read what [`export`] wrote.
pub fn import(json: &[u8]) -> Result<(Device, Vec<Group>), Error> {
    let s: Saved = serde_json::from_slice(json).map_err(|_| bad("state is malformed"))?;
    if s.v != 1 {
        return Err(bad("unknown state version"));
    }
    let h = |x: &str| hex::decode(x).map_err(|_| bad("state is malformed"));
    let storage: HashMap<Vec<u8>, Vec<u8>> = s
        .storage
        .iter()
        .map(|(k, v)| Ok((h(k)?, h(v)?)))
        .collect::<Result<_, Error>>()?;
    let dev = Device::from_parts(
        h(&s.signer_public)?,
        h(&s.identity)?,
        h(&s.device)?.try_into().map_err(|_| bad("device id"))?,
        s.list_version,
        storage,
    )?;
    let mut groups = Vec::new();
    for g in &s.groups {
        let secret: [u8; 32] = h(&g.secret)?.try_into().map_err(|_| bad("secret"))?;
        groups.push(Group::restore(&dev, &h(&g.id)?, g.generation, secret)?);
    }
    Ok((dev, groups))
}
