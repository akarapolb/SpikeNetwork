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
cargo test                     # 17 tests (15 API against a real relay, 2 unit), a few seconds
```
You need a relay and the WebView2 runtime (built into Windows 11). A relay on this computer is fine for a trial:
```sh
cargo run -q -p spike-relay -- --listen 127.0.0.1:8787 --insecure-no-tls
```
State lives in the platform app-data directory (`network.spike.connect`). To run a second identity on one machine, set `SPIKE_DESKTOP_HOME` to another directory. A desktop home is a normal `Home`: the CLI can open it with `--home`.

## Two computers
See `../docs/deploy-relay.md` to run a relay another computer can reach. In the app: **Settings → Home relay** (your relay's `wss://` address) and, for a self-signed certificate, **Trust for a relay you run yourself → Choose certificate file**. **Test connection** says whether it works and why not. Plain `ws://` to another computer is refused unless you allow it in Settings, with a warning.

## Installer
`cargo install tauri-cli --version "^2" --locked` once (about 10 minutes), then:
```sh
cd desktop/src-tauri
cargo tauri build --bundles nsis
```
The installer lands in `target/release/bundle/nsis/` (per-user, no administrator rights). It is **unsigned**: Windows SmartScreen will warn, and there is no auto-update. A signing certificate and an update channel come before any release to other people.

## Built
Create and unlock an identity; chats; send and receive; add contacts by pasted card; verify; relay setting; **instant delivery** (a background listener; ADR 0020), **edit and delete** your own messages, **removing a device**, **relay trust** (custom certificate, test button, opt-in plain `ws://`), **idle auto-lock** and **attention** (window title count, taskbar flash), **attachments** (attach button, files up to 100 MB encrypted before upload, Save writes to Downloads and never overwrites); **agent grants** (create, change, revoke, typed-only, limits) and approvals; **devices and linking** (a Devices screen, "link this computer" at first start, and "Link a new device" on your main device, with the safety-code comparison and history choice).

## Not built yet
Message requests, groups, calls, search, Windows Hello, a tray icon and desktop notification banners (the taskbar flashes and the title shows a count instead), auto-update, signed builds, a keepalive for silently dropped connections. The listener and the polling pause while this computer is waiting to be linked, because a sync would consume the link frames.

## Developer notes
- Raw file bytes go over Tauri's binary IPC as the request body, with the contact, file name and caption as percent-encoded headers (`x-contact`, `x-filename`, `x-caption`). The UI's `invoke` wrapper must pass the third `options` argument through.
- `SPIKE_DESKTOP_DOWNLOADS` overrides where Save writes (default: the Downloads folder).
- Debug builds print what the listener does to stderr (`[listener] connecting`, `woken by the relay`); release builds are silent.
- To drive the real app from a script: start it with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222` and a separate `WEBVIEW2_USER_DATA_FOLDER`, then connect over the Chrome DevTools protocol (evaluate JavaScript, capture screenshots). Several instances need different `SPIKE_DESKTOP_HOME` and port values.
- Linking codes are about 850 characters (a device card plus an ephemeral key): paste them, they are not meant to be typed. A QR code is future work.

## Known gaps
- The passphrase crosses the bridge once as a JavaScript string, which cannot be zeroized (ADR 0019).
- Only Windows has been run. Linux needs WebKitGTK; macOS needs signing and notarisation.
- To bundle an installer you need `tauri-cli` (`cargo install tauri-cli --version "^2"`) and `"bundle": {"active": true}`; not set up.
