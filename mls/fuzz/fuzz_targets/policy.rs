//! The policy extension is attacker-controlled bytes: decoding must never panic, and whatever
//! decodes must be valid and re-encode to something that decodes to the same thing.
#![no_main]
use libfuzzer_sys::fuzz_target;
use spike_mls::policy::Policy;

fuzz_target!(|data: &[u8]| {
    if let Ok(p) = Policy::decode(data) {
        assert!(p.validate().is_ok());
        assert_eq!(Policy::decode(&p.encode()).unwrap(), p);
    }
});
