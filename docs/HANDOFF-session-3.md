# Handoff: Spike Connect, end of session 3

Written 2026-10-19. Branch `claude/charming-ramanujan-5cl13k`. Read this, then `HANDOFF-session-2.md` (agent loop), `HANDOFF.md` (project background), `README.md`, `desktop/README.md` and ADRs 0017 to 0019.
**Nothing has been pushed and no PR exists** (the user has not asked for one).

## 1. What session 3 did
| Area | What | Where |
|---|---|---|
| Typed actions | Signed fields the human sees; services must match them exactly; `--typed-only` grants | ADR 0017, `core/src/action.rs`, commit `b4717cd` |
| Design | Desktop prototype (aligned with the mobile prototype, quieter colour rule, icon-only rail, small groups, planned call screen); mobile prototype recoloured and updated to typed approvals; live mobile canvas republished (version 14) | `docs/desktop-prototype/`, `docs/mobile-prototype/`, commit `e094caa` |
| Calls | Draft design: Spike signals, WebRTC carries media, direct first, relay fallback | ADR 0018 (draft) |
| **Desktop app** | **Tauri 2, working end to end on Windows**: first slice, then attachments, grants and linking, then relay trust, instant delivery, edit/delete, remove a device, auto-lock, installer config | ADR 0019, 0020, `desktop/` |
| Relay | A device may hold up to 4 live connections and a message is pushed to all (the old "replace silently" rule left a waiting connection deaf); application-level ping/pong keepalive, and the relay closes connections silent for 120 s | ADR 0020, `relay/src/lib.rs`, `cli/src/client.rs`, `cli/tests/keepalive.rs` |
| Message requests | Strangers who introduce themselves (signed card bound to the sending device) wait in a quarantine inbox: accept, decline, block; files never kept; capped and expired | ADR 0021, `cli/src/requests.rs`, `core/src/session.rs` (`decrypt_any`), `cli/tests/requests.rs` |
| Plan | Multi-tenant hosted relay: slices, decisions needed (you chose option C) | `docs/hosted-relay-plan.md` |
| Docs | Relay deployment guide and a hosting-options brief for a decision | `docs/deploy-relay.md`, `docs/hosting-options.md` |
| CI | GitHub Actions, cargo-deny, Dependabot (PR #1; blocked by an account billing problem, so unproven on GitHub) | `.github/`, `deny.toml` |

## 2. The desktop app, in one paragraph
`desktop/` is its own Cargo workspace (excluded from the main one). `src-tauri/src/api.rs` holds all logic as plain functions over a `Session` (the unlocked `Home` plus the relay); `lib.rs` wraps them as Tauri commands. The UI is plain HTML/CSS/JS in `ui/` with a strict CSP, no npm, no remote content. Keys never enter the webview. All network operations are serialized by one lock because `send` and `recv` each load and save ratchet state. Receiving polls every 15 s. Built: create/unlock/lock, chats with instant delivery, send, receive, edit, delete, add contact by pasted card, verify, relay settings (address, custom CA, opt-in plain ws, test button), agent panel (typed approvals, approve/deny, create/change/revoke grants), attachments (send, save), devices (list, link both sides, remove), idle auto-lock, attention (title count, taskbar flash), installer config. Message requests with a block list are built. Not built: groups, calls, search, Windows Hello, tray, signing, auto-update, a QR code for linking.

## 3. State
- `cd desktop/src-tauri && cargo test`: 19 passed (17 API against a real relay, 2 unit). Root workspace: 166 passed on Windows and on Ubuntu (WSL). The main workspace's `cargo test --all` was 150 passed before the desktop work and is unaffected by it (the desktop crate is excluded).
- Verified by driving the real app: two instances (`SPIKE_DESKTOP_HOME`) exchanged messages including Thai text; wrong and right passphrase after a restart; a typed request approved in the window; the agent received the receipt; `verify-receipt` refused payee `mallory` and accepted `acme-gpu`.
- First `cargo build` of the desktop crate takes about 10 minutes (Tauri with optimised dependencies); later builds 10 to 20 s.

## 4. How the real app was driven (reusable)
WebView2 exposes a debug port: start the exe with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222` and a separate `WEBVIEW2_USER_DATA_FOLDER`, then connect over CDP. A 15-line Node script (built-in `WebSocket`, `Runtime.evaluate`, `Page.captureScreenshot`) was enough; it is not in the repo. Use it for UI checks instead of screen control.

## 5. Next steps (recommended order)
1. **Decide on pushing and a PR.** Ask first.
2. **CI** (fmt, clippy, tests for both workspaces, `cargo audit`, license check). None configured; the desktop crate needs a Windows runner at least.
3. **Desktop, next slice:** a QR code for linking, signing and auto-update, then a Linux and macOS pass; sync the invite-link flow (the mobile design) with message requests.
4. **Mobile and desktop parity** for groups: groups are design only. The proposed small-group approach (pairwise fan-out, up to 20) needs an ADR and a group membership model before code.
5. **Hosting decision** (`docs/hosting-options.md`) and a published legal-requests policy before Spike operates any relay.
6. **Spike OS integration and telemetry** still need the user's definitions (see session 2 handoff).
6. Earlier open items: "resolved" notice for forwarded agent requests (ADR 0016), relay hardening, payee/path allowlists in grants, `docs/architecture.html` is stale.

## 6. Gotchas found this session
- **Windows blocks rebuilding a running exe.** Kill `spike-desktop.exe` before `cargo build` in `desktop/src-tauri`.
- **Tauri embeds `ui/` at compile time** in debug builds: after editing the UI, rebuild (it picks the change up in seconds).
- **`cargo clippy` in the desktop crate recompiles Tauri** with different flags (long first run).
- The Bash tool still mishandles long heredocs sometimes; writing scripts to the scratchpad with the Write tool and running them was reliable again.
- The Artifact canvas read returns files into the scratchpad; `\r` line endings in the repo copies differ from the canvas copies, so strip `\r` before publishing or comparing.
- **The UI's `invoke` wrapper dropped Tauri's third argument** (the headers for raw-body calls), so `send_file` got an empty contact. Calls made straight through `window.__TAURI__` worked, which hid it: when something works directly but not in the app, check the wrapper.
- **Polling during linking would swallow the link frames** (`recv` acks what it cannot decrypt), so the UI stops polling while the new device waits.
- **git-bash `/tmp` is not Windows Python's `/tmp`:** pass real Windows paths to Python scripts.
- **A Tauri 2 window cannot listen to events without a capability** (`capabilities/default.json`). `listen` fails with a promise rejection that nothing handles, so the backend worked and the screen did not refresh. I had measured the store (113 ms) and called it the screen. Measure the DOM. Also: a keepalive deadline must run from the first unanswered ping, not be pushed back by each new ping (the test with a short interval found that).
- **Declining or blocking a message request deletes its session**, so the sender's next message in it is undecryptable (`Invalid`), not a new request. That is inherent to deleting the keys; see ADR 0021.
- **A relay that "replaces" a connection silently leaves the old one deaf.** The first listener never woke in the real app although a library test passed, because that test accepted a quiet timeout. When a test can pass on either outcome, tighten it. See ADR 0020.
- **Git-Bash rewrites `/CN=...` in `openssl -subj`** as a path (use `//CN=...`), and the failed command still left a half-written directory.
- **The release installer needs `tauri-cli`** (`cargo install tauri-cli --version "^2" --locked`, about 10 minutes); the NSIS toolset is downloaded during the bundle step.
- `Home` is not `Debug`, and tests use `.err().unwrap()`; the desktop `Session` follows the same habit (no `Debug`).

## 7. Decisions and approvals
- The user chose **Tauri desktop** and asked for the design prototype first, aligned with mobile, in a quieter colour tone and icon-only left nav. Groups and calls: design only so far; the user asked for a group screen and a calls draft, not for them to be built.
- Commit trailer: follow the session's system reminder (it has changed between Sonnet and Opus). Commits were made only when the user asked. **Do not push or open a PR without asking.**
- Open stack questions still unanswered: libp2p timing, license, monorepo. Tauri vs Electron is now answered. No LICENSE file exists on purpose.

## 8. Honest status
Unreviewed prototype code (README: "Do not use it for real secrets"). The webview is a large attack surface even with a strict CSP; the passphrase crosses the bridge as a JavaScript string; only Windows has been run; no signed builds, no auto-update.

## Update: hosted relay slice 0 (ADR 0022) and the tenant design (ADR 0023)
- Built: per-address and per-connection limits, trusted-proxy address handling, SQLite calls on blocking threads, schema migrations (`user_version`, backup before upgrade, newer database refused), `/healthz` and content-free `/metrics`, new relay flags (see `docs/deploy-relay.md`).
- Tests: root workspace 184 pass on Windows and Ubuntu (WSL); `cli/tests/hardening.rs` is new. Two `ownsync` tests with 400 ms and 2 s expiries were flaky under load (also before this change); their times were widened.
- ADR 0023 (tenants and enrollment tokens) is a proposal for your review. Nothing of it is built. Its "Decisions I need from you" section lists what to answer.
- Still to do from the same request: desktop polish (QR for linking, signing and auto-update, Linux/macOS pass, faster on-screen delivery) and small groups (ADR first).

## Update: desktop polish
- Screen updates as soon as each message is stored (`Session::sync_with`), not after the 0.7 s idle wait. Not re-measured in the real app.
- Link code is now `spikelink2:` (about 330 characters, old form still accepted) and the new device shows a QR (generated in the backend as SVG). Nothing scans it yet.
- Opt-in updater (`--features updater`, Settings button, never automatic), `release.yml` (Windows/Linux/macOS, optional Windows signing) and `docs/desktop-release.md`. Needs your certificate, update key and update server; none of it has been run with real keys. macOS never run.
- Linux: installed WebKitGTK dev libraries in WSL Ubuntu; the desktop crate builds, clippy is clean with and without `updater`, and all 19 desktop tests pass. The window itself has not been seen on Linux.
- Found by the Linux run: a relay that silently drops packets made connecting hang for about two minutes. Connecting now times out after 10 s.
- Shortcuts use Cmd on a Mac, Ctrl elsewhere.

## Update (2026-10-23): small groups ADR
- ADR 0024 (small groups over pairwise sessions) is a proposal for your review. Nothing of it is built. It covers a signed group state chain with a deterministic winner rule for concurrent admin changes, group envelopes, non-contact members, consent to being added (adds by non-contacts go to message requests), history for new members, and an honest list of what pairwise groups cannot give. Its "Decisions I need from you" section lists four questions.
- Next after your answers: build slice 1 (core state and validation), then the CLI, then the desktop screens.
- Later the same day: you chose **pairwise first, MLS later** (sender keys considered and rejected; reasons in the ADR) and accepted the four defaults. ADR 0024 is now Accepted.
- **Slice 1 built:** `core/src/group.rs`: signed `GroupState`, the change rules (admins anything; members leave, or add when `add_by = any`; always an admin), `Chain` with the winner rule (highest version, then lowest hash), missing-state handling, `check_message` (a removed member cannot post by naming an old state). 14 unit tests; root workspace 199 pass, clippy clean. The desktop crate was not rebuilt (no API it uses changed).
- **Slice 2 built (CLI):** `cli/src/groups.rs`, envelope fields `group`/`gstate` and kinds `group_state`/`group_state_request`, `StoredMessage.author`, member cards for people who are not contacts (`members.enc`), a reply outbox flushed by `recv`, `spike group ...` commands. The ADR's "Changes while building" section lists what changed from the plan (new members start from a checkpoint; stranger invites live with the group, not the request list). 8 integration tests in `cli/tests/groups.rs`; root workspace 210 pass, clippy clean; the desktop crate builds against it.
- **Next: slice 3 (desktop).** The prototype's group screens over `groups::*`; then the gaps in the ADR's list (invite decisions synced to own devices, attachments and read state in groups).
