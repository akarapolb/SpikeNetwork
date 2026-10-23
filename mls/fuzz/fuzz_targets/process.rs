//! Anything the relay hands a member: must never panic; a refusal must leave the group as it was;
//! nothing altered may read as a different message.
#![no_main]
#[path = "../common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;
use spike_mls::group::Processed;

fuzz_target!(|data: &[u8]| {
    let w = common::world();
    let (bob, mut group) = w.bob();
    let input = common::pick(data, &[&w.commit, &w.message]);
    let (epoch, members) = (group.epoch(), group.members().len());
    match group.process(&bob, &w.dir, &input) {
        Err(_) => {
            assert_eq!(group.epoch(), epoch);
            assert_eq!(group.members().len(), members);
        }
        Ok(Processed::Message(body, _)) => {
            assert_eq!(body, b"a real message", "a forged message was read")
        }
        // The real commit (or an edit that changed nothing that matters): one more member.
        Ok(Processed::Commit { .. }) => assert_eq!(group.members().len(), members + 1),
        Ok(Processed::Own) => {}
    }
});
