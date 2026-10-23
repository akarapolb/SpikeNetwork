//! A key package from anyone, offered to a committer: must never panic.
#![no_main]
use libfuzzer_sys::fuzz_target;
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, OwnerKey};
use spike_mls::credential::MapDirectory;
use spike_mls::group::{Device, Group};
use spike_mls::policy::Policy;

fuzz_target!(|data: &[u8]| {
    let o = OwnerKey::generate();
    let i = Identity::generate().unwrap();
    let mut dir = MapDirectory::default();
    dir.insert(DeviceList::sign(&o, 1, vec![DeviceEntry { card: i.card().encode(), relay: None }]).unwrap());
    let d = Device::new(&o, hex::decode(i.device_id()).unwrap().try_into().unwrap(), 1).unwrap();
    let mut g = Group::create(&d, Policy::new(vec![o.public()])).unwrap();
    let _ = g.add(&d, &dir, &[data.to_vec()]);
});
