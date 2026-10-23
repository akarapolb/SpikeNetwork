//! The credential that binds an MLS leaf to a person (ADR 0025, section 3).
//!
//! A leaf's `BasicCredential` identity is
//! `format(1) | owner_pub(32) | device_id(16) | list_version(8, big endian) | owner_sig(64)`.
//! The owner key signs the leaf's signature key, the device and the device-list version it was
//! issued under. The signature alone proves the owner vouched for the device at some point; whether
//! the device is still the owner's is decided against the owner's current signed device list, which
//! comes from a [`Directory`]. A device that is in a newer list than we hold is *stale*, not valid:
//! the caller refreshes the directory and tries again.

use openmls::prelude::{BasicCredential, Credential, CredentialType};
use spike_core::owner::{verify_raw, DeviceList, OwnerKey};
use std::collections::HashMap;

pub const FORMAT: u8 = 1;
pub const IDENTITY_LEN: usize = 1 + 32 + 16 + 8 + 64;
const DOMAIN: &[u8] = b"spike-mls-leaf-v2";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("malformed credential")]
    Malformed,
    #[error("the owner's signature does not cover this leaf")]
    BadSignature,
    #[error("no valid device list for this owner")]
    UnknownOwner,
    #[error("the device is not in the owner's current list")]
    Revoked,
    #[error("the device list is newer than ours (have {have}, need {need})")]
    StaleDirectory { have: u64, need: u64 },
}

/// Where current, signed device lists come from. Implementations may return any list; callers
/// check it themselves.
pub trait Directory {
    fn device_list(&self, owner: &[u8; 32]) -> Option<DeviceList>;
}

/// A directory held in memory, keyed by owner key.
#[derive(Default, Clone)]
pub struct MapDirectory(HashMap<[u8; 32], DeviceList>);

impl MapDirectory {
    /// Store a list as given. Nothing is checked here; `verify_leaf` checks what it reads.
    pub fn insert(&mut self, list: DeviceList) {
        if let Some(owner) = hex::decode(&list.owner)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
        {
            self.0.insert(owner, list);
        }
    }
}

impl Directory for MapDirectory {
    fn device_list(&self, owner: &[u8; 32]) -> Option<DeviceList> {
        self.0.get(owner).cloned()
    }
}

/// Who a leaf belongs to, once its credential has been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafIdentity {
    pub owner: [u8; 32],
    pub device: [u8; 16],
    pub list_version: u64,
}

fn signed_bytes(owner: &[u8; 32], leaf_key: &[u8], device: &[u8; 16], version: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(DOMAIN.len() + 32 + leaf_key.len() + 16 + 8 + 4);
    v.extend_from_slice(DOMAIN);
    v.extend_from_slice(owner);
    v.extend_from_slice(&(leaf_key.len() as u32).to_be_bytes());
    v.extend_from_slice(leaf_key);
    v.extend_from_slice(device);
    v.extend_from_slice(&version.to_be_bytes());
    v
}

/// The credential identity bytes for a device whose MLS signature key is `leaf_key`.
pub fn certify(owner: &OwnerKey, leaf_key: &[u8], device: [u8; 16], list_version: u64) -> Vec<u8> {
    let pubkey = owner.public();
    let sig = owner.sign_raw(&signed_bytes(&pubkey, leaf_key, &device, list_version));
    let mut v = Vec::with_capacity(IDENTITY_LEN);
    v.push(FORMAT);
    v.extend_from_slice(&pubkey);
    v.extend_from_slice(&device);
    v.extend_from_slice(&list_version.to_be_bytes());
    v.extend_from_slice(&sig);
    v
}

/// Read who a credential says it is, without checking anything. For members already in the group,
/// whose credentials were checked when they joined.
pub fn parse(cred: &Credential) -> Result<(LeafIdentity, [u8; 64]), CredentialError> {
    if cred.credential_type() != CredentialType::Basic {
        return Err(CredentialError::Malformed);
    }
    let basic = BasicCredential::try_from(cred.clone()).map_err(|_| CredentialError::Malformed)?;
    let id = basic.identity();
    if id.len() != IDENTITY_LEN || id[0] != FORMAT {
        return Err(CredentialError::Malformed);
    }
    Ok((
        LeafIdentity {
            owner: id[1..33].try_into().expect("length checked"),
            device: id[33..49].try_into().expect("length checked"),
            list_version: u64::from_be_bytes(id[49..57].try_into().expect("length checked")),
        },
        id[57..].try_into().expect("length checked"),
    ))
}

/// Check that `cred` says "this leaf is device D of owner O", that O signed that for this exact
/// leaf key, and that O's current signed list (from `dir`) still names D.
pub fn verify_leaf(
    dir: &dyn Directory,
    cred: &Credential,
    leaf_key: &[u8],
) -> Result<LeafIdentity, CredentialError> {
    let (
        LeafIdentity {
            owner,
            device,
            list_version,
        },
        sig,
    ) = parse(cred)?;
    if !verify_raw(
        &owner,
        &signed_bytes(&owner, leaf_key, &device, list_version),
        &sig,
    ) {
        return Err(CredentialError::BadSignature);
    }

    // The list must be a real one, signed by this owner. Anything else is not their list.
    let list = dir
        .device_list(&owner)
        .filter(|l| hex::decode(&l.owner).ok().as_deref() == Some(&owner[..]) && l.verify().is_ok())
        .ok_or(CredentialError::UnknownOwner)?;
    if list_version > list.version {
        return Err(CredentialError::StaleDirectory {
            have: list.version,
            need: list_version,
        });
    }
    let named = list
        .verify()
        .map_err(|_| CredentialError::UnknownOwner)?
        .iter()
        .any(|c| hex::decode(c.device_id()).ok().as_deref() == Some(&device[..]));
    if !named {
        return Err(CredentialError::Revoked);
    }
    Ok(LeafIdentity {
        owner,
        device,
        list_version,
    })
}
