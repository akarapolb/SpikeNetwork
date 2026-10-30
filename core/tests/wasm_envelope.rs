//! Runs only on wasm32 (ADR 0033): `cargo test -p spike-core --target wasm32-unknown-unknown`, with
//! `wasm-bindgen-test-runner` as the target runner (see the `wasm-core` job in ci.yml). It catches what a compile
//! cannot: a call that builds for the browser and panics when it runs, such as `SystemTime::now()`.
#![cfg(target_arch = "wasm32")]

use spike_core::envelope::Envelope;
use wasm_bindgen_test::*;

/// The browser's random source works (getrandom's `js` / `wasm_js` features), so keys and ids can be made.
#[wasm_bindgen_test]
fn random_bytes_come_from_the_browser() {
    use rand::RngCore;
    let (mut a, mut b) = ([0u8; 16], [0u8; 16]);
    rand::rngs::OsRng.fill_bytes(&mut a);
    rand::rngs::OsRng.fill_bytes(&mut b);
    assert_ne!(a, b);
}

/// An envelope is stamped with the wall clock. `SystemTime::now()` panics on wasm32-unknown-unknown; the stamp
/// now comes from `spike_core::clock::now_ms()`, which reads the page's `Date.now()` there (ADR 0033, step 3).
#[wasm_bindgen_test]
fn an_envelope_can_be_made_in_the_browser() {
    let before = spike_core::clock::now_ms();
    let e = Envelope::text("hello from wasm").expect("an envelope");
    assert_eq!(e.body, "hello from wasm");
    assert!(
        e.ts >= before && before > 1_700_000_000_000,
        "{} {before}",
        e.ts
    );
}
