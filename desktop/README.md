# Spike Connect desktop (Tauri 2)

First slice of the desktop app (ADR 0019). The screens follow `../docs/desktop-prototype/`. **Unreviewed prototype: do not use it for real secrets.**

## Layout
| Path | What |
|---|---|
| `src-tauri/src/api.rs` | All the app's logic as plain functions over a `Session` (tested without a window) |
| `src-tauri/src/lib.rs` | Thin Tauri commands over `api`; holds the unlocked session in memory |
| `src-tauri/tests/api.rs` | Tests against a real relay |
| `ui/` | The webview UI: `index.html`, `app.css`, `app.js`. No framework, no npm, no build step, no remote content (strict CSP) |

Keys and cryptography stay in the Rust process. The UI only gets names, message text and request summaries.

## Run it
It is its own Cargo workspace, so the main `cargo test --all` does not build it.
```sh
cd desktop/src-tauri
cargo run                      # first build compiles Tauri: about 10 minutes
cargo test                     # 5 API tests, a few seconds
```
You need a relay (loopback plain `ws://` is fine for a trial) and the WebView2 runtime (built into Windows 11):
```sh
cargo run -q -p spike-relay -- --listen 127.0.0.1:8787 --insecure-no-tls
```
State lives in the platform app-data directory (`network.spike.connect`). To run a second identity on one machine, set `SPIKE_DESKTOP_HOME` to another directory. A desktop home is a normal `Home`: the CLI can open it with `--home`.

## Not built yet
Devices and linking, attachments, edit and delete in the UI, creating agent grants (use `spike agent-grant`), groups, calls, message requests, search, Windows Hello, idle auto-lock, tray and notifications, installer, auto-update, signed builds. Receiving is by polling every 15 seconds.

## Known gaps
- The passphrase crosses the bridge once as a JavaScript string, which cannot be zeroized (ADR 0019).
- Only Windows has been run. Linux needs WebKitGTK; macOS needs signing and notarisation.
- To bundle an installer you need `tauri-cli` (`cargo install tauri-cli --version "^2"`) and `"bundle": {"active": true}`; not set up.
