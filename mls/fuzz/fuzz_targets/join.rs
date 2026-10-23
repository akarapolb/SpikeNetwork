//! A Welcome and a join bundle from anyone: must never panic.
#![no_main]
use libfuzzer_sys::fuzz_target;
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, OwnerKey};
use spike_mls::credential::MapDirectory;
use spike_mls::group::{Device, Group};

fuzz_target!(|data: &[u8]| {
    let o = OwnerKey::generate();
    let i = Identity::generate().unwrap();
    let mut dir = MapDirectory::default();
    dir.insert(DeviceList::sign(&o, 1, vec![DeviceEntry { card: i.card().encode(), relay: None }]).unwrap());
    let d = Device::new(&o, hex::decode(i.device_id()).unwrap().try_into().unwrap(), 1).unwrap();
    let split = data.first().copied().unwrap_or(0) as usize % (data.len().max(1));
    let (welcome, bundle) = data.split_at(split);
    let _ = Group::join(&d, &dir, welcome, bundle);
});
