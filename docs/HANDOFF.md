# Handoff: Spike Connect, end of session 1

Written 2026-10-13. Branch: `claude/charming-ramanujan-5cl13k` (all work committed and pushed; working tree clean at `12eb7f8`).
Start here, then read `README.md`, `docs/threat-model.md` and the ADRs in `docs/adr/` in that order.

## 1. What this project is
Spike Connect is a privacy-first communication network for people, AI agents and devices (application: **Spike Connect**; infrastructure: **Spike Network**). The source is the strategy paper *Spike Connect: Product Vision, Roadmap, Business Model* v1.0 (13 Oct 2026). Its plans are summarized in `docs/product-roadmap.md`, `docs/business-plan.md` and `docs/engineering-plan.md` (also as one page: `docs/spike-connect-plans.html`, and `docs/architecture.html`).

Launch thesis from the paper: agent coordination plus private messaging for Spike OS users and design partners, B2B/agent-first. Not a mass-market chat app.

## 2. Where we are on the roadmap
| Roadmap item | Status |
|---|---|
| Phase 0: threat model, protocol decisions, 2-device encrypted chat, identity prototype | **Done** (`docs/threat-model.md`, ADRs 0001–0006, `scripts/demo.sh`) |
| Phase 1: relay fallback, offline queue, local encrypted storage | **Done** |
| Phase 1: pairing | **Partial.** Device cards + fingerprint verification via CLI. No QR / short-code flow |
| Phase 1: attachment prototype | **Not started.** Next item (see section 8) |
| Phase 2: Spike OS integration, agent-to-agent requests with approvals, telemetry | Not started |
| Direct P2P (libp2p / NAT traversal) | Not started; deliberately deferred (see ADR 0001) |
| Groups (MLS), mobile, multi-device sync, admin | Not started (Phase 3) |

The product and business actions in the paper's 30-day plan (15 user interviews, trademark and domain clearance, licensing review, appointing a product owner, 90-day budget) are **not done and are not code tasks**; see section 9.

## 3. Code map
Rust workspace (`cargo build`, edition 2021). About 3,400 lines including tests.
```
core/    spike-core    identity.rs  Olm account, DeviceCard (self-signed), signed prekeys, fallback prekey
                       session.rs   Sessions: Double Ratchet via vodozemac (start_outbound / encrypt / decrypt)
                       vault.rs     Argon2id + XChaCha20-Poly1305 file sealing (file name is AAD)
                       wire.rs      client <-> relay JSON frames (ClientMsg / ServerMsg)
relay/   spike-relay   lib.rs       WebSocket server: challenge-response auth, queue, prekey store, TLS accept
                       store.rs     SQLite (messages, prekeys, fallbacks); in-memory when no --db
                       tls.rs       rustls acceptor from PEM files
                       main.rs      CLI flags: --listen --db --tls-cert --tls-key --ttl-secs --queue-cap --insecure-no-tls
cli/     spike-cli     state.rs     Home: sealed identity.enc / sessions.enc / contacts.enc + vault.json
                       transport.rs wss with webpki roots + --relay-ca; ws only to loopback
                       client.rs    Relay connection (auth, send, claim/publish prekeys, deliveries, ack)
                       ops.rs       send / recv / publish_keys / prekey top-up and fallback rotation
                       main.rs      `spike` binary: init card add verify contacts keys send recv
scripts/demo.sh        end-to-end demo with real binaries over wss, SIGKILLs the relay mid-run
```
Data flow in one line: `spike send` -> `ops::send` -> claim a prekey if no session -> `Sessions::encrypt` -> persist ratchet -> upload blob to relay -> relay queues ciphertext in SQLite -> recipient `ops::recv` -> `Sessions::decrypt` -> persist state -> ack.

## 4. How to run it
```sh
cargo test --all                 # 44 tests, ~10 s on a warm cache
cargo clippy --all-targets       # clean (0 warnings)
cargo fmt --all --check
./scripts/demo.sh                # needs openssl; builds, runs two devices + relay over wss
```
Test inventory: `core` 21 unit tests (ratchet, replay, tamper, forward secrecy, prekeys, fallback, vault); `cli/tests/e2e.rs` 16 (conversation, restart durability, expiry, size/queue/claim limits, fallback, encrypted state); `cli/tests/tls.rs` 4; `relay/tests/binary.rs` 2; `cli` unit 1.

State is encrypted; give the passphrase with `--passphrase-file`, `SPIKE_PASSPHRASE`, or the prompt (min 10 chars on `init`). Test code uses `KdfParams::insecure_for_tests()`; never use it elsewhere.

## 5. Decisions made (and by whom)
The user did **not** answer the open stack questions from the first discussion, so I proceeded with my recommended defaults. All are reversible but nobody has formally approved them:
- Rust workspace, CLI first, no desktop shell yet (Tauri vs Electron still open).
- Relay over WebSocket; libp2p deferred until the protocol is stable.
- Olm (vodozemac) for 1:1 sessions; MLS planned for groups in Phase 3.
- **No LICENSE file** exists on purpose. Apache-2.0 is the paper's candidate; legal decision.
Details and trade-offs: ADR 0001 (stack; message pattern superseded), 0002 (Olm + prekeys), 0003 (encrypted state), 0004 (durable relay), 0005 (TLS), 0006 (fallback prekey).

## 6. Known gaps, most important first
1. **No independent security review.** Everything is unreviewed code. Do not describe it as secure for real secrets. In ADR 0002 I wrote that vodozemac was externally audited (Least Authority, 2022) **from memory; verify the scope before repeating the claim.**
2. **Olm session config v1** truncates the MAC to 8 bytes (matches Matrix). v2 is behind an experimental feature in the crate. Revisit with the reviewer.
3. **Rollback of local state files is not detected** (older valid file can be restored). Needs a monotonic counter / hardware keystore.
4. **Keys are in memory while the CLI runs;** every command re-runs Argon2 (about 0.3-1 s). A background agent holding the unlocked key is not built. No `spike passwd` (change passphrase) and no backup/recovery.
5. **Relay abuse controls are thin:** size, per-recipient queue cap, TTL, per-connection prekey claim cap (resettable by reconnecting). No per-IP or per-sender rate limits, no spam controls.
6. **Relay is single-node:** one SQLite file, no replication, no schema migrations, blocking SQLite calls on async threads (move to `spawn_blocking` before load testing). Certificates load at startup only (renew by restart).
7. **First messages of sessions started through the fallback prekey have weaker forward secrecy** while that key is held (up to about two weeks). Accepted in ADR 0006.
8. **At-most-once display:** a crash between persisting state and printing loses that message from the UI.
9. **Delivery receipts do not exist;** a malicious relay can silently drop or delay messages.
10. Metadata (who, when, how much) is visible to the relay operator and, partly, to network observers. No padding or batching.
11. `docs/architecture.html` predates prekeys, fallback, TLS and durable storage; the diagram is still directionally right but incomplete. Update it.

## 7. Gotchas I hit (save yourself the time)
- `cargo fmt` reflows code, so scripted find-and-replace edits that ran **after** a format pass silently did not apply several times. After any scripted edit, grep to confirm it landed, or edit with exact current text.
- Building one crate (`-p spike-relay`) can fail where `--all` succeeds because Cargo unifies features across the workspace (this hid a missing `clap` `env` feature).
- A self-signed certificate made by `openssl req -x509` is marked as a CA by default and the client (correctly) rejects it as a server certificate. Use `-addext "basicConstraints=critical,CA:FALSE"` (the demo does).
- `RUST_BACKTRACE` may be set in the environment; the CLI prints one-line errors regardless.
- `Home` deliberately does not implement `Debug` (it holds the vault key). Tests use `.err().unwrap()`.
- The dev profile optimizes dependencies (`[profile.dev.package."*"] opt-level = 3`) so Argon2 and the ratchet are fast in tests.
- One test (`fallback_rotates...`) once passed for the wrong reason because refilled one-time keys masked a missing rotation. When testing fallback behavior, drain one-time keys again before asserting.

## 8. Recommended next steps
1. **Attachment prototype (last Phase 1 item).** Proposed design, not yet decided: chunk the file (e.g. 256 KiB), encrypt each chunk with a fresh random file key (XChaCha20-Poly1305, chunk index in the nonce/AAD to prevent reordering), send the file key, chunk count and per-chunk or whole-file hash inside the existing end-to-end message, and store the encrypted chunks on the relay under random blob ids with their own size cap, quota and TTL. Receiver downloads, verifies hashes, decrypts, and acks. Needs new relay storage/limits, new wire messages, a CLI `send-file` / `recv`, tests for tampering, truncation, reordering, and resumption, and an ADR. Open question to settle first: do chunks go through the relay queue or a separate blob store? (Recommend a separate blob store.)
2. **Pairing UX:** QR / short-code exchange of the device card plus fingerprint comparison, replacing copy-paste of the card string.
3. **Relay hardening:** per-IP and per-sender rate limits, `spawn_blocking` for SQLite, schema migrations, metrics (counts, latency, queue depth, never content), a deploy guide.
4. **Delivery receipts,** then a measurement plan for the paper's gates (define "accepted message", "within SLO", "weekly active" precisely before instrumenting).
5. **Phase 2:** agent message envelope (signed requests, scopes, expiry, human approval, tamper-evident audit log). The design is sketched in `docs/engineering-plan.md` section 5 and in the second diagram of `docs/architecture.html`. This is the product differentiator; consider prototyping it earlier than the plan's 61-90 day window.
6. **Direct P2P** (libp2p) only after the above; keep the relay path as the fallback.
7. Add **CI** (fmt, clippy, tests, `cargo audit` / `cargo deny`, license check) once a repo workflow location is chosen. No CI is configured today.

### Multi-device design (proposed after the first handoff)
`docs/multi-device-design.html` is a proposed design for message history across devices: identity key signing a device list, per-device fan-out, an encrypted local message store (does not exist yet), link-time history bundles, own-device sync, optional backup, and a clear statement of which servers are still needed (relay, directory, push; none holds messages or accounts). Milestones M1-M6 are in that doc; **M1 (message store) and M2 (owner key + signed device lists) are done** (ADR 0007, ADR 0008; `cli/src/messages.rs`, `core/src/envelope.rs`, `core/src/owner.rs`). Next is **M3: fan-out to every device** (per-device sessions, sent copies to own devices, device lists published on and fetched from the relay, per-device home relay). Messages currently go to the first device in a contact's list only. Seven open questions are listed at the end. Settled in review: a new device gets **all text history**, attachments from the last 30 days in the bundle, and older attachments on demand from another own device (a flat 30-day window was judged too short). The rest still need the user's answers.

`docs/multi-device-demo.html` is a self-contained interactive simulation of the same design (vanilla JS, no real cryptography, 4 devices + relay, a guided tour, a 4-step linking wizard with history choices, and four attacks). It exists to explain the design, not to test it. Its merge rules (idempotent insert by msg_id, edit last-writer-wins by (lamport, device), delete tombstone wins, list version only increases, unsigned lists rejected) are the ones the M1-M5 milestones should implement and test.

`docs/mobile-prototype/` holds the mobile client design prototype (nine Design-canvas screens plus `canvas.json`; live canvas link in its README). Design only, no app code. It assumes contacts are notified (not asked) about new devices and no backup in the first multi-device release; both are still open questions for the user.

## 8a. Before any public launch (added after the security discussion)
Release integrity and compelled-access resistance are now explicit pre-launch gates: reproducible builds, binary transparency, threshold release signing, key transparency, relay metadata minimisation, published audits (engineering plan section 7a; threat model "Powerful adversaries and compelled access"; roadmap release gates). Stance agreed with the user: cooperative and transparent with governments, not adversarial; obey the law where Spike operates; design so little can be handed over (business plan section 8a).

## 9. Not code, still outstanding (needs the user / the business)
- Answer the open decisions: Rust vs TypeScript (currently Rust by default), desktop shell, libp2p timing, monorepo vs split repos, license.
- A lawful-access exposure review per target country, a legal-requests policy, and a pre-decided response to an order to weaken the product (business plan 8a).
- 15 user interviews, trademark and domain clearance for "Spike Connect", patent/dependency/export review, a named product owner, and a 90-day budget figure (none appear in the paper).
- Commission the independent security review before any private beta (Phase 3 gate in the roadmap).
- Pricing, the year-two ARR scenario and all gates are hypotheses from the paper, not validated. The year-two total ($1,554,000) was re-checked; about 62% of it comes from Business and Personal Plus seats.

## 10. Artifacts outside the repo (private links; only the owner can open them)
- Plans page: https://claude.ai/artifact/JcsrNeu8EF1h7w8yp7wUmr (same content as `docs/spike-connect-plans.html`)
- Architecture diagrams: https://claude.ai/artifact/7ndbqSykCJv6PZP28PxCtZ (same as `docs/architecture.html`, stale as noted above)
No pull request has been opened; the user did not ask for one.

## 11. Working agreements observed so far
- Commit and push to `claude/charming-ramanujan-5cl13k` only; commits end with the Co-Authored-By and Claude-Session trailer lines.
- Be honest in docs about what is mitigated vs partial vs open; update `docs/threat-model.md` and add an ADR whenever a security-relevant decision changes.
- Prefer established crates over hand-written crypto; no custom primitives.
