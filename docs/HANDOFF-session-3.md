# Handoff: Spike Connect, end of session 3

Written 2026-10-19. Branch `claude/charming-ramanujan-5cl13k`. Read this, then `HANDOFF-session-2.md` (agent loop), `HANDOFF.md` (project background), `README.md`, `desktop/README.md` and ADRs 0017 to 0019.
**Nothing has been pushed and no PR exists** (the user has not asked for one).

## 1. What session 3 did
| Area | What | Where |
|---|---|---|
| Typed actions | Signed fields the human sees; services must match them exactly; `--typed-only` grants | ADR 0017, `core/src/action.rs`, commit `b4717cd` |
| Design | Desktop prototype (aligned with the mobile prototype, quieter colour rule, icon-only rail, small groups, planned call screen); mobile prototype recoloured and updated to typed approvals; live mobile canvas republished (version 14) | `docs/desktop-prototype/`, `docs/mobile-prototype/`, commit `e094caa` |
| Calls | Draft design: Spike signals, WebRTC carries media, direct first, relay fallback | ADR 0018 (draft) |
| **Desktop app** | **Tauri 2 first slice, working end to end on Windows** | ADR 0019, `desktop/` |

## 2. The desktop app, in one paragraph
`desktop/` is its own Cargo workspace (excluded from the main one). `src-tauri/src/api.rs` holds all logic as plain functions over a `Session` (the unlocked `Home` plus the relay); `lib.rs` wraps them as Tauri commands. The UI is plain HTML/CSS/JS in `ui/` with a strict CSP, no npm, no remote content. Keys never enter the webview. All network operations are serialized by one lock because `send` and `recv` each load and save ratchet state. Receiving polls every 15 s. Built: create/unlock/lock, chats, send, receive, add contact by pasted card, verify, relay setting, agent panel (typed approvals, approve/deny). Not built: devices and linking, attachments, edit/delete, grant creation, groups, calls, message requests, search, Windows Hello, auto-lock, tray, installer, signing.

## 3. State
- `cd desktop/src-tauri && cargo test`: 5 passed (real relay, no window). The main workspace's `cargo test --all` was 150 passed before the desktop work and is unaffected by it (the desktop crate is excluded).
- Verified by driving the real app: two instances (`SPIKE_DESKTOP_HOME`) exchanged messages including Thai text; wrong and right passphrase after a restart; a typed request approved in the window; the agent received the receipt; `verify-receipt` refused payee `mallory` and accepted `acme-gpu`.
- First `cargo build` of the desktop crate takes about 10 minutes (Tauri with optimised dependencies); later builds 10 to 20 s.

## 4. How the real app was driven (reusable)
WebView2 exposes a debug port: start the exe with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222` and a separate `WEBVIEW2_USER_DATA_FOLDER`, then connect over CDP. A 15-line Node script (built-in `WebSocket`, `Runtime.evaluate`, `Page.captureScreenshot`) was enough; it is not in the repo. Use it for UI checks instead of screen control.

## 5. Next steps (recommended order)
1. **Decide on pushing and a PR.** Ask first.
2. **CI** (fmt, clippy, tests for both workspaces, `cargo audit`, license check). None configured; the desktop crate needs a Windows runner at least.
3. **Desktop, next slice:** devices and linking screens (the code exists in `cli/src/link.rs`), attachments (send, open), creating agent grants from the UI, auto-lock, notifications, then an installer with `tauri-cli`.
4. **Mobile and desktop parity** for groups: groups are design only. The proposed small-group approach (pairwise fan-out, up to 20) needs an ADR and a group membership model before code.
5. **Spike OS integration and telemetry** still need the user's definitions (see session 2 handoff).
6. Earlier open items: "resolved" notice for forwarded agent requests (ADR 0016), relay hardening, payee/path allowlists in grants, `docs/architecture.html` is stale.

## 6. Gotchas found this session
- **Windows blocks rebuilding a running exe.** Kill `spike-desktop.exe` before `cargo build` in `desktop/src-tauri`.
- **Tauri embeds `ui/` at compile time** in debug builds: after editing the UI, rebuild (it picks the change up in seconds).
- **`cargo clippy` in the desktop crate recompiles Tauri** with different flags (long first run).
- The Bash tool still mishandles long heredocs sometimes; writing scripts to the scratchpad with the Write tool and running them was reliable again.
- The Artifact canvas read returns files into the scratchpad; `\r` line endings in the repo copies differ from the canvas copies, so strip `\r` before publishing or comparing.
- `Home` is not `Debug`, and tests use `.err().unwrap()`; the desktop `Session` follows the same habit (no `Debug`).

## 7. Decisions and approvals
- The user chose **Tauri desktop** and asked for the design prototype first, aligned with mobile, in a quieter colour tone and icon-only left nav. Groups and calls: design only so far; the user asked for a group screen and a calls draft, not for them to be built.
- Commit trailer: follow the session's system reminder (it has changed between Sonnet and Opus). Commits were made only when the user asked. **Do not push or open a PR without asking.**
- Open stack questions still unanswered: libp2p timing, license, monorepo. Tauri vs Electron is now answered. No LICENSE file exists on purpose.

## 8. Honest status
Unreviewed prototype code (README: "Do not use it for real secrets"). The webview is a large attack surface even with a strict CSP; the passphrase crosses the bridge as a JavaScript string; only Windows has been run; no signed builds, no auto-update.
