//! Anything the relay hands a member: must never panic, and a refusal must leave the group as it was.
#![no_main]
use libfuzzer_sys::fuzz_target;
use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, OwnerKey};
use spike_mls::credential::MapDirectory;
use spike_mls::group::{Device, Group};
use spike_mls::policy::Policy;

fn device_id(i: &Identity) -> [u8; 16] {
    hex::decode(i.device_id()).unwrap().try_into().unwrap()
}

fuzz_target!(|data: &[u8]| {
    let (oa, ob) = (OwnerKey::generate(), OwnerKey::generate());
    let (ia, ib) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let entry = |i: &Identity| DeviceEntry { card: i.card().encode(), relay: None };
    let mut dir = MapDirectory::default();
    dir.insert(DeviceList::sign(&oa, 1, vec![entry(&ia)]).unwrap());
    dir.insert(DeviceList::sign(&ob, 1, vec![entry(&ib)]).unwrap());
    let (da, db) = (Device::new(&oa, device_id(&ia), 1).unwrap(), Device::new(&ob, device_id(&ib), 1).unwrap());
    let mut ga = Group::create(&da, Policy::new(vec![oa.public()])).unwrap();
    let out = ga.add(&da, &dir, &[db.key_package()]).unwrap();
    ga.confirm(&da).unwrap();
    let mut gb = Group::join(&db, &dir, out.welcome.as_ref().unwrap(), out.join_bundle.as_ref().unwrap()).unwrap();
    let (epoch, members) = (gb.epoch(), gb.members().len());
    if gb.process(&db, &dir, data).is_err() {
        assert_eq!(gb.epoch(), epoch);
        assert_eq!(gb.members().len(), members);
    }
});
