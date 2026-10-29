# ADR 0033, step 1: the call-site list

Follow-up to [ADR 0033](0033-client-crate-and-traits.md). Taken on 2026-10-29 from `main` (5954aea) plus the ADR.
Nothing in the code was changed for this list except the wasm32 CI build of `spike-core` (end of this file).
Line numbers are of that commit and will drift; the counts are what the later steps check against.

"Moved modules" means `lib.rs` (`process`, `Outcome`, `apply_*`), `groups`, `large`, `apps`, `link`, `ops`,
`requests`, `sync`, `agent`, `client` and `state`: the ones ADR 0033 section 5 moves into `spike-client`.

## 1. Time (`Clock`)

Wall clock, `SystemTime::now()` (milliseconds since the epoch):

| Where | What it stamps |
|---|---|
| `core/src/envelope.rs:118` | the timestamp of every envelope (in `spike-core`, which already builds for wasm) |
| `cli/src/groups.rs:96` | group messages and records |
| `cli/src/apps.rs:46` | app-channel data (the 30 s live-data limit of ADR 0030) |
| `cli/src/link.rs:251` | the history cut-off when linking a device |
| `cli/src/ops.rs:875` | one-to-one sends |
| `cli/src/requests.rs:64` | contact requests |
| `cli/src/agent.rs:52` | agent requests |

Monotonic clock, `Instant` (deadlines, keepalive), all in `cli/src/client.rs`: lines 110, 176, 183, 204, 218, 248,
252, 500, with `tokio::time::timeout_at` at 244 and 505.

`tokio::time::timeout` outside `Relay`: `cli/src/ops.rs:750` (a 3 s connect limit).

So: **seven** wall-clock calls (one of them in `core`), **nine** monotonic ones, **three** `tokio::time` calls.
`mls/src` has none.

A caution for the wasm32 CI build below: `SystemTime::now()` compiles for `wasm32-unknown-unknown` and panics when
it runs. The build cannot see it. `core/src/envelope.rs:118` is the one that already matters, because
`Envelope::new` is the first thing a browser would call. It must go through `Clock` (or take the time as an
argument) before anything runs in wasm.

## 2. Files and the vault (`Store`)

All in `cli/src/state.rs`; no moved module other than `state` touches `std::fs`.

- `Home` holds a `dir: PathBuf`. `Home::exists(dir)` (89), `create(dir, ...)` (95), `unlock(dir, ...)` (127) and
  `path()` (139) take or return paths. `unlock` reads the header file (129).
- The three primitives: `read_opt` (320), `read` (425), `write` (432), built on `write_private` (448: temp file,
  `fsync`, rename, `0600`) and `make_private_dir` (437). Every `load_*` and `save_*` in `Home` (identity, sessions,
  quarantine, owner, device list, contacts, read markers, last sync, lamport, `load_bytes`/`save_bytes`,
  `load_json`/`save_json`) goes through them.
- **Bypassing the primitives:** attachments, `save_attachment` (287), `load_attachment` (296) and
  `delete_attachment` (366) read and write `attachments/<tag>.enc` directly with `fs`, and the SQLite file is
  opened by path (`messages.rs:144`).
- Users of the generic accessors in the moved modules: `load_json`/`save_json`/`load_bytes`/`save_bytes` 60 calls
  in total (`large.rs` uses `load_bytes`/`save_bytes` 4 times for MLS group state).
- Callers outside the moved modules: `app/src/lib.rs:95` and `:1463` build `home.path().join(CA_FILE)`;
  `app/src/lib.rs:425,440,446` call `Home::exists/create/unlock`. These stay native.

## 3. Attachments

Call sites of `save_attachment` / `load_attachment` / `delete_attachment` in the moved modules:

- `lib.rs` 220, 644
- `groups.rs` 868, 1870
- `link.rs` 161, 272
- `ops.rs` 291, 336, 340, 1050

Ten in all. They must go through `Store` (name `attachments/<tag>`), as ADR 0033 section 3b says.

## 4. `Target` paths

`transport::Target { url, opts: TransportOpts }`, and `TransportOpts` has `ca_file: Option<PathBuf>`
(`transport.rs:25`), read at `transport.rs:132`. The moved modules name `Target` in:

- `groups.rs` (about 25 signatures, `impl Into<Target>` and `&Target`)
- `large.rs` (`at`, `connect` and the public functions; it also builds a `Target { .. }` at line 398)
- `ops.rs`, `apps.rs`, `link.rs`, `client.rs`

The path is only read in the native dial, so the plan in ADR section 3 holds: the shared `Target` keeps the URL and
TLS mode, and the CA file moves to the native side. The only builder outside is `app/src/lib.rs:95`.

## 5. `Relay::connect` (`Connector`)

17 call sites in the moved modules, plus one inside `client.rs`:

| File | Lines |
|---|---|
| `ops.rs` (10) | 137, 266, 299, 455, 675, 750, 922, 943, 1057, 1077 |
| `link.rs` (5) | 83, 112, 200, 246, 291 |
| `groups.rs` (1) | 374 |
| `large.rs` (1) | 406 |
| `client.rs` | 101 (`dial(&target)`, the native call itself) |

## 6. Modules that import a native-only module

| Moved module | Imports | Handled by |
|---|---|---|
| `ops.rs` | `crate::direct` (lines 230-233 and 518) | `DirectPath` hook (ADR 0033 section 5) |
| `groups`, `large`, `apps`, `link`, `ops`, `client` | `crate::transport::{Target, ...}` | `Target` split (section 4); `dial` and `Ws` via `Connector` |
| `groups`, `large`, `requests`, `link`, `sync`, `ops`, `state`, `lib.rs` | `crate::messages` | `History` (section 7) |
| `state.rs` | `std::fs`, `PathBuf` | `Store` (section 2) |

No moved module imports `backup`, `verifier` or `main`. No moved module uses `std::net`, `tokio::net`,
`std::process`, `std::env`, threads or `tokio::spawn`. Dependencies of `spike-cli` that must not be pulled into
`spike-client`: `tokio` features `net` and `rt-multi-thread` (neither builds for wasm32), `tokio-tungstenite`,
`rustls` and `tokio-rustls`, `webpki-roots`, `rusqlite`, `rpassword`, `clap`.

## 7. The `History` operation list

`MessageStore` (SQLite, `messages.rs`) is reached through `home.messages()`: `lib.rs` 6 times, `groups.rs` 7,
`ops.rs` 9, `large.rs` 4, `link.rs` 3, `requests.rs` 1. The methods the moved modules call, with the number of
call sites (by file; counted by search on 2026-10-29, so treat them as within one or two):

| Operation | Calls | Where |
|---|---|---|
| `insert(&StoredMessage) -> bool` | 9 | `lib.rs` 3, `groups.rs` 2, `large.rs` 1, `link.rs` 1, `ops.rs` 1, `requests.rs` 1 |
| `get(id) -> Option<StoredMessage>` | 8 | `lib.rs` 3, `groups.rs` 4, `ops.rs` 1 |
| `update(&StoredMessage)` | 6 | `lib.rs` 4, `groups.rs` 2 |
| `find(prefix) -> StoredMessage` | 4 | `ops.rs` |
| `conversation(peer, limit) -> Vec` | 5 | `groups.rs` 3, `large.rs` 1, `ops.rs` 1 (the read markers and group history) |
| `all() -> Vec` | 2 | `link.rs` (history to bring to a new device) |
| `changed_since(ms) -> Vec` | 1 | `ops.rs` (catching up another of your devices) |
| `merge_record(&StoredMessage) -> bool` | 1 | `lib.rs` |
| `park_reaction(target, conv, reactor, &Reaction)` | 1 | `lib.rs` (`apply_reaction`) |

Not used by any moved module: `prune_older_than` (only `cli/src/main.rs:1454`, stays native) and `contains`.
So `History` has **nine** operations, not the longer list in the first draft:
`insert`, `get`, `update`, `find`, `conversation`, `all`, `changed_since`, `merge_record`, `park_reaction`.

The data types `StoredMessage`, `Direction`, `Reaction`, `MergeState` and the pure function `fold_reaction` are
used everywhere and have no SQLite in them; they move with the moved modules (into `spike-client`, or a small
`spike-types` module beside it). Only `MessageStore` (the SQL) stays in `spike-cli` as the native `History`.

Browser `History`: `insert`/`get`/`update`/`find`/`conversation` are enough to receive and order messages;
`all`, `changed_since` and `merge_record` serve linking and catching up, which a browser does not do in stage 1 and
can return empty.

## 8. What the other crates import from `spike-cli`

So the re-exports in step 5 keep them compiling (count of mentions in `app`, `desktop`, `mobile`):

`client::Relay` 4, `messages::StoredMessage` 2, `direct` 2, `client::Incoming` 2, `backup` 2, `client::Delivery` 1,
`messages::Direction` 1, `large` (3 items), `groups`, `link`, `requests`, `apps` (2), `agent`, `state`,
`transport`. `mobile/` uses none. The desktop uses one item (`client::Incoming::Delivery`).

## wasm32 CI

`.github/workflows/ci.yml` has a new job `wasm-core`: `cargo build -p spike-core --target
wasm32-unknown-unknown`. For it to build, `core/Cargo.toml` gets a target-only section that turns on the
`getrandom` browser features (`js` for 0.2, `wasm_js` for 0.4); no other target is affected. It is compile-only:
see the `SystemTime` caution in section 1. Run locally on 2026-10-29, it builds (2 min 24 s cold).
