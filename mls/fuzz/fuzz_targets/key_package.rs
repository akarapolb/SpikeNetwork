//! A key package from anyone, offered to a committer: must never panic, and an altered one is refused.
#![no_main]
#[path = "../common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;
use spike_mls::group::Error;

fuzz_target!(|data: &[u8]| {
    let w = common::world();
    let (bob, mut group) = w.bob();
    let input = common::pick(data, &[&w.key_package]);
    let real = input == w.key_package;
    // Whoever commits, a malformed or altered key package must be refused before anything is built.
    match group.add(&bob, &w.dir, &[input]) {
        Ok(_) if !real => panic!("an altered key package was accepted"),
        Ok(_) => {}
        Err(Error::PendingCommit) => panic!("refused for the wrong reason"),
        Err(_) => assert!(!group.has_pending()),
    }
});
