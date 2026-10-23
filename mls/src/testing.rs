//! Test hooks (feature `test-hooks`): make commits the honest API refuses to make, so tests can
//! show that every *receiver* refuses them too. Never enabled outside tests.

use crate::group::{Device, Group};
use crate::policy::Policy;
use ::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;

pub fn clear_pending(dev: &Device, group: &mut Group) {
    group
        .mls
        .clear_pending_commit(dev.provider.storage())
        .unwrap();
}

/// Add a key package with no credential, policy or authority checks. Leaves the commit pending.
pub fn unchecked_add(dev: &Device, group: &mut Group, key_package: &[u8]) -> Vec<u8> {
    clear_pending(dev, group);
    let kp = KeyPackageIn::tls_deserialize_exact(key_package)
        .unwrap()
        .validate(dev.provider.crypto(), ProtocolVersion::Mls10)
        .unwrap();
    let (commit, _w, _i) = group
        .mls
        .add_members(&dev.provider, &dev.signer, &[kp])
        .unwrap();
    commit.tls_serialize_detached().unwrap()
}

/// Remove a device with no authority check. Leaves the commit pending.
pub fn unchecked_remove(dev: &Device, group: &mut Group, device: &[u8; 16]) -> Vec<u8> {
    clear_pending(dev, group);
    let index = group
        .mls
        .members()
        .find(|m| {
            crate::credential::parse(&m.credential)
                .map(|(i, _)| i.device == *device)
                .unwrap_or(false)
        })
        .unwrap()
        .index;
    let (commit, _w, _i) = group
        .mls
        .remove_members(&dev.provider, &dev.signer, &[index])
        .unwrap();
    commit.tls_serialize_detached().unwrap()
}

/// Replace the policy with no authority check. Leaves the commit pending.
pub fn unchecked_policy(dev: &Device, group: &mut Group, policy: &Policy) -> Vec<u8> {
    clear_pending(dev, group);
    group
        .commit_policy(dev, policy)
        .unwrap()
        .tls_serialize_detached()
        .unwrap()
}

/// Add several key packages with no checks, returning the commit and the welcome.
pub fn unchecked_add_many(
    dev: &Device,
    group: &mut Group,
    key_packages: &[Vec<u8>],
) -> (Vec<u8>, Vec<u8>) {
    clear_pending(dev, group);
    let kps: Vec<_> = key_packages
        .iter()
        .map(|b| {
            KeyPackageIn::tls_deserialize_exact(b)
                .unwrap()
                .validate(dev.provider.crypto(), ProtocolVersion::Mls10)
                .unwrap()
        })
        .collect();
    let (commit, welcome, _i) = group
        .mls
        .add_members(&dev.provider, &dev.signer, &kps)
        .unwrap();
    (
        commit.tls_serialize_detached().unwrap(),
        welcome.tls_serialize_detached().unwrap(),
    )
}

/// Add a key package in a commit with **no update path** (legal in MLS when a commit only adds),
/// which the honest API never makes. Leaves the commit pending.
pub fn unchecked_add_without_path(dev: &Device, group: &mut Group, key_package: &[u8]) -> Vec<u8> {
    clear_pending(dev, group);
    let kp = KeyPackageIn::tls_deserialize_exact(key_package)
        .unwrap()
        .validate(dev.provider.crypto(), ProtocolVersion::Mls10)
        .unwrap();
    let bundle = group
        .mls
        .commit_builder()
        .propose_adds([kp])
        .force_self_update(false)
        .load_psks(dev.provider.storage())
        .unwrap()
        .build(
            dev.provider.rand(),
            dev.provider.crypto(),
            &dev.signer,
            |_| true,
        )
        .unwrap()
        .stage_commit(&dev.provider)
        .unwrap();
    bundle.commit().tls_serialize_detached().unwrap()
}
