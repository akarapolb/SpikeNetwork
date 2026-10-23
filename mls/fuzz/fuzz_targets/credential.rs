//! A credential identity from anyone: must never panic, and nothing random may verify.
#![no_main]
use libfuzzer_sys::fuzz_target;
use openmls::prelude::BasicCredential;
use spike_mls::credential::{verify_leaf, MapDirectory};

fuzz_target!(|data: &[u8]| {
    let cred = BasicCredential::new(data.to_vec()).into();
    assert!(verify_leaf(&MapDirectory::default(), &cred, &[7; 32]).is_err());
});
