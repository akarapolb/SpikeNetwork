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

/// Known to fail today: `Envelope::text` reads `SystemTime::now()` (core/src/envelope.rs), which panics on
/// wasm32-unknown-unknown. Step 3 of ADR 0033 routes it through `Clock`; remove the `ignore` then.
/// Run it meanwhile with `-- --include-ignored` to see the panic.
#[wasm_bindgen_test]
#[ignore = "SystemTime::now() panics in wasm until Clock is injected (ADR 0033 step 3)"]
fn an_envelope_can_be_made_in_the_browser() {
    let e = Envelope::text("hello from wasm").expect("an envelope");
    assert_eq!(e.body, "hello from wasm");
}
