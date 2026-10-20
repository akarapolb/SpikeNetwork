# ADR 0019: Desktop app on Tauri 2

Status: **Proposed** (first slice implemented and exercised on Windows in `desktop/`)
Date: 2026-10-19

## Context
The pilot needs desktop-to-desktop chat people can click (roadmap MVP scope). Until now there was only the `spike` CLI. Tauri vs Electron was an open question since session 1; the user chose **Tauri desktop** on 2026-10-19. The screens are agreed in `docs/desktop-prototype/`.

## Decision
- **Tauri 2** (stable; Tauri 3 is still alpha). The shell is a Rust process that links `spike-core` and `spike-cli` (the library half) directly. The UI is plain HTML, CSS and JavaScript in a webview, with **no Node build step and no npm dependencies** (`withGlobalTauri`), so there is no JavaScript supply chain to review.
- **Keys and cryptography never enter the webview.** The unlocked `Home` lives in the Rust process. The UI only receives what it displays (names, message text, request summaries) through a small set of `#[tauri::command]` functions. The passphrase crosses the bridge once, at unlock or create, and is zeroized on the Rust side. A JavaScript string cannot be zeroized: the passphrase may linger in webview memory until garbage-collected. Accepted for the first slice; a native passphrase prompt is the fix.
- **Logic is in `desktop/src-tauri/src/api.rs`,** plain functions over a `Session`, tested without a window against a real relay (like `cli/tests`). The Tauri commands are one-line wrappers, so the security-relevant behaviour is covered by `cargo test`.
- **Outside the main Cargo workspace** (`exclude = ["desktop"]`, its own lockfile and `target/`), so `cargo test --all` for core, relay and CLI does not need a webview toolchain, and CI for those stays fast.
- **Strict content security policy**: `default-src 'self'`, no remote scripts, fonts or images, no inline script. The UI uses system fonts for now (the prototype's web fonts would be a network request).
- **State directory:** the platform app-data directory (`network.spike.connect`), a device of its own. It is a normal `Home`, so the CLI can open it with `--home`.
- **Relay:** a URL in settings (default `ws://127.0.0.1:8787`, loopback only without TLS, as in the CLI). Receiving is instant through a background listener (ADR 0020) with a 60-second poll as a safety net.

## In this slice
**Second batch (same day):** attachments, creating and revoking agent grants, and device linking, all from the window. Linking holds the network lock while waiting and pauses polling, because `recv` would consume (and ack) the link frames. Raw file bytes use Tauri's binary IPC; saving writes to the Downloads folder with `create_new`, so files are never overwritten. Only the main device (first in the signed list) creates grants and links new devices, matching ADR 0014/0016.

**Third batch (2026-10-20):** a relay trust setting (a custom CA certificate, sealed in the home, and an explicit, warned opt-in for plain `ws://` on a trusted network, with a **Test connection** button); **instant delivery** through a background listener (ADR 0020); **edit and delete** your own messages; **removing a device** (main device only; the removed one is told on its next sync); **idle auto-lock** (default 15 minutes) and **attention** (window title count, taskbar flash); and a **per-user NSIS installer** configured in `tauri.conf.json`.

Create identity and unlock (Argon2id vault), lock, conversation list with unread counts, conversation view, send, receive, add a contact from a pasted card, show your own card and fingerprint, compare and mark a contact verified, relay setting, and the agent panel: grants, waiting requests rendered from the signed typed fields (ADR 0017), approve or deny (ADR 0014, 0016).

## Verified
`cargo test` in `desktop/src-tauri` (15 API tests and 2 unit tests, real relay, no window): create, unlock and passphrase rules, chat with unread and verification, typed agent requests shown by their fields and decided, free text marked as such, an unreachable relay, grants created, validated and revoked, files sent and saved byte for byte, a full device link (matching safety codes, history copied, one identity on two devices, a later message reaching both), a self-signed TLS relay refused until its certificate is trusted through the app (and still trusted after a restart), plain `ws://` off loopback refused without an explicit opt-in, edits and deletes reaching the other person, a removed device being told, and the live-connection rules of ADR 0020. The real app was also driven end to end through the webview's debug port: two instances exchanged messages, a typed request was approved in the window, the agent received the signed receipt, and a service refused a wrong payee and accepted the right one. A third window was linked to an existing identity through the UI, then received the same message as the first. A 300 KB file with a Thai name was sent in one window and saved identically from another. Instant delivery was measured in the real app at 113 ms to both devices; a self-signed `wss://` relay was refused, then trusted by choosing its certificate file in Settings; edit, delete, device removal and auto-lock were each driven through the real UI.

## Not in this slice (shown in the prototype, not built)
Message requests (they need a security design first: today a first message from an unknown key cannot even be decrypted), groups, calls (ADR 0018), message requests, search, Windows Hello, idle auto-lock, system tray and notifications, installer and updater, code signing.

## Limits (honest list)
- Unreviewed code, like everything else here; the webview is a large attack surface even with a strict CSP and no remote content. Never load remote pages in it.
- No auto-update and no signed builds yet. Release integrity (reproducible builds, signing) is a pre-launch gate in the roadmap.
- Linux needs WebKitGTK; macOS needs a signed and notarised build to run for others. Only Windows is exercised so far.
- Instant delivery has no keepalive: a connection dropped silently by a router is noticed only at the 60-second safety check (ADR 0020).
- The installer is configured but unsigned, so Windows SmartScreen will warn, and there is no auto-update.
