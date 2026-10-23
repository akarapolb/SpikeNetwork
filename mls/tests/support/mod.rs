//! Shared test helpers: people with owner keys, real signed device lists, and directories.
#![allow(dead_code)]

use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, OwnerKey};
use spike_mls::credential::MapDirectory;

/// A person: an owner key, some devices, and the signed list that names them.
pub struct Person {
    pub owner: OwnerKey,
    pub devices: Vec<Identity>,
    pub list: DeviceList,
}

fn entry(d: &Identity) -> DeviceEntry {
    DeviceEntry {
        card: d.card().encode(),
        relay: None,
    }
}

impl Person {
    pub fn new(devices: usize) -> Self {
        let owner = OwnerKey::generate();
        let devices: Vec<Identity> = (0..devices)
            .map(|_| Identity::generate().unwrap())
            .collect();
        let list = DeviceList::sign(&owner, 1, devices.iter().map(entry).collect()).unwrap();
        Self {
            owner,
            devices,
            list,
        }
    }

    pub fn owner_pub(&self) -> [u8; 32] {
        self.owner.public()
    }

    /// The 16 bytes an MLS credential carries for device `i`.
    pub fn device_bytes(&self, i: usize) -> [u8; 16] {
        hex::decode(self.devices[i].device_id())
            .unwrap()
            .try_into()
            .unwrap()
    }

    /// Link another device: a new signed list, version + 1. Returns its index.
    pub fn add_device(&mut self) -> usize {
        let d = Identity::generate().unwrap();
        self.list = self.list.with_device(&self.owner, entry(&d)).unwrap();
        self.devices.push(d);
        self.devices.len() - 1
    }

    /// Revoke device `i`: a new signed list, version + 1, without it.
    pub fn revoke(&mut self, i: usize) {
        let id = self.devices[i].device_id();
        self.list = self.list.without_device(&self.owner, &id).unwrap();
    }

    /// An MLS device for device `i`, certified under the current list version.
    pub fn mls_device(&self, i: usize) -> spike_mls::group::Device {
        spike_mls::group::Device::new(&self.owner, self.device_bytes(i), self.list.version).unwrap()
    }
}

/// A directory that knows each person's current list.
pub fn directory(people: &[&Person]) -> MapDirectory {
    let mut d = MapDirectory::default();
    for p in people {
        d.insert(p.list.clone());
    }
    d
}
