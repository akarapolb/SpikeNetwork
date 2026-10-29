# ADR 0033: One client crate over storage, transport and time traits (so Spike can run in a browser)

Status: **Proposed. For your review; not built.** 2026-10-29, revised the same day after a review of the first
draft. Not reviewed by the owner.

Written for Center Work (an office suite on app channels, ADR 0030), whose web version needs the Spike network
without the desktop app. This is stage 1 of a larger plan. Two things are separate ADRs still to be written:
the browser identity and invitation model (no device linking to an account at first), and attachments, backup
and large groups in the browser. Center Work's own notes are in its repository
(`docs/spike-browser-client-proposal.md`).

## Problem

Spike's client logic (sessions, groups, large groups, app channels, device lists, device linking) lives in
`spike-cli`, next to the command line, and is written against native things:

1. `state::Home`, a directory of sealed files (`std::fs`, `PathBuf`, atomic write and `fsync`).
2. `client::Relay`, which owns a `tokio_tungstenite` socket (`transport::Ws`) and uses `tokio::time`.
3. The SQLite chat history (`messages.rs`), which the receive path writes to in many places (early reactions and
   deleted-message markers need it).
4. The system clock, called directly (`SystemTime::now()`, `Instant`) in seven places outside `Relay`.
5. `process`, `Outcome` and the `apply_*` functions, which live in `cli/src/lib.rs`; every module to be moved
   imports them.

So the same logic cannot run in a browser, and Center Work cannot reach the network from a web page without the
desktop app (the local API of ADR 0030 refuses browsers by design).

Checked on 2026-10-29: `spike-core` builds for `wasm32-unknown-unknown` once the `getrandom` crates get their
browser features (`js` for 0.2, `wasm_js` for 0.4). `spike-mls` fails as it is, and builds after two feature
changes (`openmls` needs its `js` feature, and `getrandom` its browser features); `spike-core` and `spike-mls`
then build in about 2 minutes. **Nothing was run in wasm yet**, and the advisories file must be read again after
that change. The relay does not read `Origin` in its WebSocket handshake.

## Decision

Move the client logic into a new crate, `spike-client`, that builds for native and for `wasm32`, and make it
depend on four small traits (`Store`, `History`, `Socket`, `Clock`) instead of files, SQLite, `tokio` and the
system clock. `spike-cli`, the desktop app and the Android library use it. No wire format, envelope, relay
protocol or cryptographic change.

### 1. `Store`: sealed named blobs, async

`Home` is built on three functions, `read_opt`, `read` and `write`; sealing is done by `Vault` in `spike-core`
(portable). Everything else in `Home` is built from them.

```rust
pub trait Store {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>>;
    /// Atomic: a reader sees the old bytes or the new, never half.
    async fn put(&self, name: &str, bytes: &[u8]) -> Result<()>;
    async fn delete(&self, name: &str) -> Result<()>;
}
```

- **The trait is async.** A browser's IndexedDB cannot be read synchronously, and the alternatives are worse:
  a write-behind cache (synchronous in memory, flushed later) can lose the latest ratchet state on a crash or a
  closed tab, which is a correctness and security problem for session state, not a performance one. The cost is
  that `Home`'s methods and their callers become `async`; most callers already are (`groups.rs` 24 async
  functions, `large.rs` 31, `ops.rs` 41). The synchronous ones in `state.rs` and `groups::list/find/...` need
  attention one by one.
- Native `Store`: the current directory with `write_private` (temp file, `fsync`, rename, `0600`), run on the
  blocking pool where needed. Same files on disk, so existing vaults open unchanged.
- Browser `Store`: IndexedDB, one object store, one transaction per `put`.
- `Home<S: Store>`; `spike-cli` and the `app` crate keep a type alias `Home = Home<FileStore>` (and
  `Relay = Relay<WsSocket, TokioClock>`), because `app/src/calls.rs` and `app/src/lib.rs` use them as bare paths.

### 2. `History`: the chat store, behind a trait

The receive path writes to the SQLite store in many places, so "no history in the browser" does not hold. The
store gets a trait with the operations the moved code uses (append, mark deleted, record a reaction or an
early reaction, read markers, query by peer). The native implementation is the current SQLite store, unchanged.
The browser implementation can be minimal (in memory, or IndexedDB for the last N messages); Center Work does
not show chat, but the receive logic still has to record and order what it sees. The exact operation list is
taken from the call sites in step 1 of the order of work, and goes in this ADR before it is accepted.

### 3. `Socket` and `Clock`: frames and time, everywhere

```rust
pub trait Socket {
    async fn send(&mut self, frame: Vec<u8>) -> Result<()>;
    async fn recv(&mut self) -> Result<Option<Vec<u8>>>;   // None when the relay closed
}
pub trait Clock {
    fn now_ms(&self) -> u64;
    async fn sleep(&self, ms: u64);
}
```

- `Relay<S: Socket, C: Clock>`: deadlines (`next_delivery(idle)`, `next_incoming`, keepalive) become
  `select(recv, sleep)`.
- **Time reaches beyond `Relay`.** `SystemTime::now()` and `Instant` are called directly in seven places; in a
  browser they panic on the first envelope. Each goes through `Clock` (wall time for envelope timestamps and the
  30 s live-data limit of ADR 0030, a monotonic reading for deadlines). A test lists them by search so none is
  missed, and `wasm32` CI catches a new one.
- `transport::Target` holds a file path (a CA file). The path moves out of the shared type: the shared crate takes
  a `Target` of address and TLS mode, and the native dial reads the file.
- Native: `Socket` wraps `tokio_tungstenite` (TLS and the loopback rule stay in the native crate); `Clock` wraps
  `tokio::time` and the system clock. Browser: `web_sys::WebSocket` (`wss://` only except loopback) and
  `setTimeout`, `Date.now`, `performance.now`.

### 4. Futures stay `Send` where the native app needs it

The native app spawns these futures, so they must stay `Send` there; a browser's `web_sys::WebSocket` and
IndexedDB handles are not `Send`. A plain `async fn` in a trait promises neither. The mechanism is a marker
trait and explicit return types:

```rust
#[cfg(not(target_arch = "wasm32"))] pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))] impl<T: Send> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]      pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]      impl<T> MaybeSend for T {}

pub trait Store {
    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + MaybeSend;
    // put, delete, and the Socket, Clock and History methods are written the same way
}
```

On native `MaybeSend` is `Send`, so spawning works and a non-`Send` future is a compile error there. On `wasm32`
it is empty, so the browser types need no wrapper and no thread hop. Generic shared code also requires `S: Sync` (and `Send` where it moves a value across a task), otherwise a
future that holds `&S` across an `.await` is not `Send` on native. Shared code that holds a future across an
`.await` must be written to the stricter (native) rule, which the native build enforces; the `wasm32` build cannot
loosen it. (`trait_variant::make` was considered; it generates a second trait and makes generic shared code
name two, so it is not used.) Step 3 of the order of work proves the mechanism on `Socket` and `Clock` before
it is applied to the rest.

### 5. What moves and what stays

| Moves into `spike-client` | Stays in `spike-cli` |
|---|---|
| `process`, `Outcome`, `apply_*` (from `lib.rs`), `groups`, `large`, `apps`, `link`, `ops`, `requests`, `sync`, `agent`, `client` (over traits), `state` (over `Store`) | `direct.rs` (it uses TCP and UDP sockets and a hostname lookup directly; a page can do neither, so direct connections stay native and, if the browser ever needs them, sit behind their own trait later), `main.rs` (commands, `rpassword`, `clap`), the native `Store`, `Socket`, `Clock` and TLS dial (`transport.rs`), the SQLite `History`, `verifier.rs` file parts, `backup.rs` file export |
| `large.rs` glue, once `spike-mls` builds for wasm (stage 4) | `spike-mls` gets the two feature changes above |

**`ops` and `direct`.** `ops.rs` moves but calls `crate::direct` (at `ops.rs` 230-233 and 518): it checks
`direct::enabled`, tries `try_send`, and falls back to the relay. With `direct` staying native, `ops` could not
compile in `spike-client`. A small hook solves it:

```rust
pub trait DirectPath {
    fn enabled(&self) -> bool;
    fn try_send(&self, to: &str, blob: &[u8]) -> impl Future<Output = Result<bool>> + MaybeSend;  // true if sent
}
```

`ops` takes `Option<&dyn DirectPath>`-style access (an optional argument, or a type parameter with a no-op
default). Native passes the real one, built on `direct.rs`; the browser passes none, so every send goes through
the relay, which is what happens today when `direct::enabled` is false. Step 5 of the order of work does this
before the move, so `ops` has no import of `direct` when it moves. Any other module found importing a native-only
module in step 1 gets the same treatment.

`spike-cli` re-exports every moved module so `desktop` (Tauri; it uses one item), `mobile/` (needs nothing) and the
shared `app` crate (which uses many) keep their old paths.

## Browser security (needed before a browser client ships; details in the browser identity ADR)

- **The vault in IndexedDB is readable by any script on the page's origin.** A browser vault is only as safe as
  the page's own code and the content security policy; the identity ADR sets what can be protected (a
  passphrase-derived key, a non-extractable key, no third-party scripts on that origin).
- **Two tabs can fork the ratchet.** Two tabs of one browser device writing session state at once can use one
  message key twice or diverge. The browser device needs a lock (Web Locks) or a single shared worker that owns
  the vault; the second tab asks the first. This is a requirement of the browser build, not an option.
- **The relay allows 4 live connections per device and accepts any `Origin`.** An optional origin allowlist in the
  relay (off by default) would let an operator limit which sites may connect; it is a relay change and gets its
  own decision.
- **Key derivation.** Argon2 at 64 MiB is heavy in a browser; the parameters, and running it in a worker, are
  decided in the identity ADR.
- **Documents.** ADR 0030 and the security documents say there is no browser client; they are updated when this
  is accepted.

## Order of work (each step keeps every existing test green)

1. Add a `wasm32` CI build of `spike-core` now (with the `getrandom` browser features), so a new time or file call is caught the moment it appears. Then list the call sites: every use of `Home` file access, SQLite, `SystemTime`/`Instant`, `Target` paths, and
   the `lib.rs` items the moved modules import. The result fixes the `History` operation list and the move list
   in this ADR.
2. Introduce `Store` (async) and make `Home` generic over it; native `Store` is the current code.
3. Introduce `Socket` and `Clock`; make `Relay` generic; route the seven direct time calls through `Clock`.
4. Introduce `History`; native implementation is the current store.
5. Create `spike-client`; move the modules and the `lib.rs` items; `spike-cli` depends on it, re-exports, and
   keeps the type aliases.
6. Add the `wasm32` CI build of `spike-client` and (after the feature changes) `spike-mls`. (The `spike-core` build is added in step 1, below, not here.)
7. Stage 2 of the plan: browser `Store`, `Socket`, `Clock`, `History`, the single-writer lock, the wasm API and
   the browser tests.

## Consequences

- Positive: one client for desktop, Android and web; no relay or wire change; Center Work's `Link` uses it
  directly.
- Cost: larger than first thought. Making storage async touches many functions; four traits spread generic
  parameters through signatures (type aliases limit this at the edges); `dyn` can be used at a few boundaries.
- Risk: atomicity and `0600` permissions must not weaken; the native `Store` keeps `write_private` as it is and
  the existing vault tests (tamper, truncation, copied from another vault) run against it.
- Risk: keepalive and timeout behaviour is subtle (ADR 0020 measured the listener fast path); relay and listener
  tests, and the desktop and Android drive scripts, must pass unchanged before merge.
- Risk: `spike-mls` was only **compiled** for wasm, never run in it. The browser plan depends on large groups working there, so stage 4 starts with a running test (two wasm members, one commit, one message), and if it fails the browser stays on one-to-one and small groups.
- Risk: the `openmls` feature change must be checked against the advisories file (ADR 0025 pins it exactly).
- Not covered here: the browser identity and invitation model, attachments, backup, and MLS in the browser.

## Tests that must pass before each step merges

`cargo test` for core, cli, relay, mls and app; `cli/tests/apps.rs`; `app/tests/local_api.rs`;
`node desktop/drive_apps.mjs`; `ANDROID_SERIAL=… node android/drive_apps.mjs`; the large-group demo and the fuzz
targets for `mls`; and from step 6 the `wasm32` builds.
