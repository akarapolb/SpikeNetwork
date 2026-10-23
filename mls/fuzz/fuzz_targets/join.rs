//! A Welcome and a join bundle from anyone: must never panic, and nothing but the real pair joins.
#![no_main]
#[path = "../common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;
use spike_mls::group::Group;

fuzz_target!(|data: &[u8]| {
    let w = common::world();
    let dave = w.dave();
    let Some((&which, rest)) = data.split_first() else {
        return;
    };
    // Mutate the Welcome, or the bundle, or feed both raw.
    let (welcome, bundle) = match which % 3 {
        0 => (common::mutate(&w.welcome, rest), w.bundle.clone()),
        1 => (w.welcome.clone(), common::mutate(&w.bundle, rest)),
        _ => {
            let split = rest.first().copied().unwrap_or(0) as usize % rest.len().max(1);
            let (a, b) = rest.split_at(split);
            (a.to_vec(), b.to_vec())
        }
    };
    let real = welcome == w.welcome && bundle == w.bundle;
    let joined = Group::join(&dave, &w.dir, &welcome, &bundle);
    if !real {
        assert!(joined.is_err(), "an altered Welcome or bundle was accepted");
    }
});
