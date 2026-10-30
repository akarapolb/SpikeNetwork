# Review of ADR 0033 (client crate and traits), 2026-10-29

Review of `docs/adr/0033-client-crate-and-traits.md` (branch `adr/client-crate-and-traits`). Read-only: no shared
code changed. The first draft was reviewed; the accepted revision (bf4533d) already covers most of this.

## Covered by the accepted revision
History behind a trait, `process`/`Outcome` moved, `Target` without a file path, time through `Clock` (also in
`spike-core`), async `Store`, `MaybeSend`, `Relay`/`Home` aliases, no `tokio` in the shared crate, browser
security section (origin, two tabs, Argon2, relay origin allowlist).

## Still open
1. `dyn` contradicts the traits: methods return `impl Future`, so `Option<&dyn DirectPath>` and "dyn at a few
   boundaries" do not compile. Use a generic parameter with a no-op default, or boxed futures for those cases.
2. `Home::path()` (used at `app/src/lib.rs:95` and `:1463` for the CA file) and file listing (backup, attachment
   cleanup) have no replacement; add `Store::list` and say what replaces `path()`.
3. Wording: the `History` operation list "goes in this ADR before it is accepted" but status is Accepted; step 6 has
   a stray "below, not here".

## Compile impact when `spike-cli` re-exports
`mobile/` uses nothing from `spike_cli` (only `spike-app`). Desktop uses `client::Incoming::Delivery`
(`desktop/src-tauri/src/lib.rs:317`). `app` uses `client`, `apps`, `groups`, `large`, `link`, `requests`,
`agent`, `state::{Added, Home}`, `messages`, `transport`, `ops`, `Outcome`, `backup`, `direct`. It compiles if
those paths stay re-exported, `Relay` and `Home` are aliases to the native types (`app/src/calls.rs:180,208,216`),
and futures stay `Send` for `tokio::spawn`.

## spike-mls on wasm32-unknown-unknown (tried, temp copy)
Plain build fails: `getrandom` 0.2 wants `js`, 0.4 wants `wasm_js`; openmls 0.9.2 has a `compile_error!`
(`src/lib.rs:149`) without its `js` feature, and `web_time` is unresolved. With openmls `features = ["js"]` (pin
kept) plus `getrandom` 0.2 `[js]`, 0.3 and 0.4 `[wasm_js]`, `spike-core` and `spike-mls` build (dev, ~2 min), with or
without the `getrandom_backend` rustflag. Not run in wasm; no release build or size check. Gate the flags under
`cfg(target_arch = "wasm32")`; re-read `mls/advisories-reviewed.txt` after.
