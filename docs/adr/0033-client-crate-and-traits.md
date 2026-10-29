# ADR 0033: One client crate over storage and transport traits (so Spike can run in a browser)

Status: **Proposed. For your review; not built.** 2026-10-29. Not reviewed.

Written for Center Work (an office suite on app channels, ADR 0030), whose web version needs the Spike network
without the desktop app. Stage 1 of a larger plan; the browser identity and invitation model (stage 2) is a
separate ADR still to be written. Numbers below that name Center Work's own notes are in its repository
(`docs/spike-browser-client-proposal.md`).

## Problem

Spike's client logic (sessions, groups, large groups, app channels, device lists, device linking) lives in the
`spike-cli` crate, next to the command line, and is written against two concrete native things:

1. `state::Home`, a directory of sealed files (`std::fs`, `PathBuf`, atomic write and `fsync`).
2. `client::Relay`, which owns a `tokio_tungstenite` socket (`transport::Ws`) and uses `tokio::time` for
   timeouts and keepalive.

So the same logic cannot run in a browser (no files, no `tokio`, no TCP), and Center Work, the first
application on app channels (ADR 0030), cannot use the network from a web page without the desktop app.

`spike-core` already builds for `wasm32-unknown-unknown` (checked 2026-10-29; it needed the `getrandom` crates'
browser features). The relay does not refuse a page's WebSocket (it never reads `Origin`).

## Decision

Move the client logic into a new crate, `spike-client`, that builds for native and for `wasm32`, and make it
depend on two small traits instead of on files and `tokio`. `spike-cli`, the desktop app and the Android
library become users of it. No wire format, envelope, relay or cryptographic change.

### 1. `Store`: sealed named blobs

`Home` already sits on a very small base: `read_opt(name)`, `read(name)`, `write(name, bytes)`, and the
sealing is done by `Vault` in `spike-core` (portable). Everything else in `Home` (identity, sessions, contacts,
`load_json` / `save_json`, attachments, read markers, lamport counter) is built from those three.

```rust
pub trait Store {
    /// The bytes last written under `name`, or None. Names are the existing file names ("contacts.enc", "att:<tag>"...).
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>>;
    /// Replace `name` atomically: a reader sees the old bytes or the new, never half.
    fn put(&self, name: &str, bytes: &[u8]) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}
```

- `Home` becomes `Home<S: Store>` (or holds a `Box<dyn Store>`); its methods keep their names, so callers in
  `groups.rs`, `large.rs`, `apps.rs`, `link.rs`, `ops.rs` change by a type parameter, not by logic.
- Native `Store`: the current directory, with `write_private` (temp file, `fsync`, rename, `0600`). Same files on disk, so existing vaults open unchanged.
- Browser `Store`: IndexedDB, one object store, a transaction per `put` (atomic by construction). The browser
  `Store` is `async` underneath; see section 3.
- Chat history (`messages.rs`, SQLite) is **not** part of this crate's first cut: the browser keeps no chat history, and the native apps keep their store behind the existing interface.

### 2. `Socket` and `Clock`: frames and time

`Relay` only does four things with its socket: write a frame (`write`), read a frame (`read_raw`), ping/pong for
keepalive, and wait with a deadline (`timeout_at`).

```rust
pub trait Socket {
    async fn send(&mut self, frame: Vec<u8>) -> Result<()>;   // text or binary frame, as today
    async fn recv(&mut self) -> Result<Option<Vec<u8>>>;       // None when the relay closed
}
pub trait Clock {
    fn now_ms(&self) -> u64;
    async fn sleep(&self, ms: u64);                              // used to build deadlines
}
```

- `Relay<S: Socket, C: Clock>`. Deadlines (`next_delivery(idle)`, `next_incoming`, keepalive) become
  `select(recv, sleep)` over these two; no direct `tokio::time`.
- Native: `Socket` wraps `tokio_tungstenite` (TLS and the loopback rule from `transport.rs` stay there); `Clock` wraps `tokio::time`.
- Browser: `Socket` wraps `web_sys::WebSocket` (`wss://` only except loopback); `Clock` wraps `setTimeout`.
- Authentication, the wire messages (`ClientMsg` / `ServerMsg`) and everything the relay sees are unchanged.

### 3. Async without a runtime

The logic is `async fn` already (`groups.rs` 24, `large.rs` 31, `ops.rs` 41) but only waits on `Relay`. It
should depend on `futures` only. `tokio` stays in the native `Socket`/`Clock`; the browser drives futures with
`wasm-bindgen-futures`. Futures must not need `Send` on wasm, so the traits are written without `Send` bounds and
native code that needs them wraps or boxes at its edge.

### 4. What moves and what stays

| Moves into `spike-client` | Stays in `spike-cli` |
|---|---|
| `groups`, `large`, `apps`, `link`, `direct`, `ops`, `requests`, `sync`, `agent`, `client` (over traits), `state` (over `Store`) | `main.rs` (commands, `rpassword`, `clap`), `transport.rs` native dial and TLS, `messages.rs` (SQLite history), `verifier.rs` file watcher parts, `backup.rs` file export |
| `mls` large-group glue (`large.rs`), once `spike-mls` builds for wasm (stage 4) | `spike-mls` itself is unchanged until stage 4 |

`spike-cli` re-exports the moved modules so `desktop` (Tauri), `app` and `android` keep compiling without
edits in the first step.

## Order of work (each step keeps every existing test green)

1. Introduce `Store` and make `Home` generic over it; native `Store` is the current code. Tests unchanged.
2. Introduce `Socket` and `Clock`; `Relay` generic over them; native implementations. Tests unchanged.
3. Create `spike-client`; move the modules; `spike-cli` depends on it and re-exports.
4. Add a `wasm32` CI build of `spike-client` and `spike-core` (with the `getrandom` features).
5. (Stage 2 of the proposal) browser `Store` and `Socket`, the wasm API, the browser tests.

## Consequences

- Positive: one client for desktop, Android and web; the browser build needs no change to the relay or the wire protocol; Center Work's `Link` can use it directly.
- Cost: a refactor of code every client runs. Generic parameters spread through signatures; `dyn` could be used at a few boundaries to limit that.
- Risk: moving `fsync`-and-rename storage behind a trait must not weaken atomicity or the `0600` permissions; the native `Store` keeps `write_private` as it is and the existing vault tests (tamper, truncation, copied-from-another-vault) run against it.
- Risk: keepalive and timeout behaviour is subtle (ADR 0020 measured the listener fast path). The relay and listener tests, and the desktop and Android driver scripts, must pass unchanged before merge.
- Not covered here: the browser identity and invitation model (separate ADR), attachments in the browser, MLS in the browser, key protection in IndexedDB.

## Tests that must pass before each step merges

`cargo test` for core, cli, relay, mls and app; `cli/tests/apps.rs`; `app/tests/local_api.rs`;
`node desktop/drive_apps.mjs`; `ANDROID_SERIAL=… node android/drive_apps.mjs`; the large-group demo and fuzz targets for `mls`.
