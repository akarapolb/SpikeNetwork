# Handoff: Spike Connect, end of session 2

Written 2026-10-17. Branch `claude/charming-ramanujan-5cl13k`, working tree clean at `be5ba69`, **3 commits ahead of `origin` and not pushed**.
Read this, then `docs/HANDOFF.md` (session 1: project background, code map, known gaps, working agreements; still accurate except where this file updates it), `README.md`, `docs/threat-model.md`, and ADRs 0014 to 0016.

## 1. What session 2 did
Session 1 ended with Phase 2's first slice (ADR 0014: signed requests, grants, receipts, audit log). This session built the rest of the agent-approval loop. Three commits:

| Commit | What | ADR |
|---|---|---|
| `d637eba` | **Receipt verifier**: the service side. An agent builds a `Presentation` (request + receipt + proof of the agent key bound to an *audience* and a time). The service calls `check` / `redeem`: trusted principal, receipt answers this exact request, approved, audience, freshness, expiry, scope equal, amount at or below the approved amount, then marks it spent in a durable SQLite store (single use). CLI: `agent-present`, `verify-receipt` (needs no identity or passphrase; exit 0 + JSON = authorized, 2 = refused). | 0015 |
| `6a6d078` | **Cumulative daily cap**: `agent-grant --daily-cents`. Counts approved *and waiting* requests over a rolling 24 h; auto-approval stops at the cap; approving re-checks it. Field omitted from the signed grant when unset, so old grants still verify. `evaluate()` now takes `committed_24h`. | 0014 addendum |
| `be5ba69` | **Approve from any device**: the approver device forwards waiting requests to your other devices (`SyncMsg::AgentPending`); `agent-approve` / `agent-deny` on any device sends `AgentDecision` back; the approver applies it (grant re-checked, one audit chain, it signs the receipt). | 0016 |

Also fixed: `spike --help` overflowed the 1 MiB Windows main-thread stack (clap parser for ~40 subcommands in a debug build); the CLI now runs on an 8 MiB thread. A closed pipe (`spike history bob | head`) exits quietly instead of panicking.

## 2. State
- `cargo test --all`: **142 passed, 0 failed** (about 30 s warm). `cargo fmt --all --check` clean. `cargo clippy --all-targets`: one **pre-existing** warning, unused `existed` in `relay/src/store.rs:48` (not touched; the session 1 handoff claimed 0 warnings).
- Roughly 8,900 lines of non-test source in `core/src` and `cli/src`.
- New files: `core/src/verifier.rs`, `cli/src/verifier.rs`, `cli/tests/agents_remote.rs`, ADRs 0015 and 0016. Changed most: `cli/src/agent.rs`, `cli/src/ops.rs` (`recv`, `agent_decide`, `announce_pending`), `cli/src/sync.rs`, `cli/src/lib.rs` (`apply_sync`, new `Outcome` variants), `cli/src/main.rs`.
- The README, ADR 0014 limits list and `docs/HANDOFF.md` were updated to match. `docs/architecture.html` is still stale (predates prekeys, TLS, agents).

## 3. How the agent loop works now (one paragraph)
Agent signs a `Request` for a principal -> principal's approver device evaluates the signed `Grant` (deny / auto-approve / ask a human, including rate and 24 h money caps) -> if a human is needed, the approver forwards it to the person's other devices -> any device records the decision (non-approvers forward it) -> the approver re-checks the grant, appends to the single hash-chained audit log, signs the `Receipt`, sends it to the agent -> agent runs `agent-present --audience <svc>` -> the service runs `verify-receipt` (or `core::verifier::redeem`) before acting, which spends the approval. Nothing here is enforced unless the *service* calls the verifier; that is by design and stated in ADR 0014/0015.

## 4. Next steps (recommended order)
1. **Push and open a PR.** Nothing has been pushed this session and no PR exists. The user has not asked for one; ask first.
2. **CI** (fmt, clippy, tests, `cargo audit` / `cargo deny`, license check). None configured. Needs a decision on where workflows live.
3. **Typed actions.** *Done 2026-10-19 (session 3): ADR 0017, `core/src/action.rs`, `--field`, `--typed-only`, `agent-types`; 150 tests pass, clippy 0 warnings (the relay `existed` warning is fixed). Natural next step: payee/path allowlists in the grant.* Was: biggest remaining gap in the agent feature: the approval prompt shows free text the agent wrote, so a lying agent can describe an action misleadingly. Design sketch: a small registry of action types with structured fields (e.g. `payments.send {payee, amount_cents, currency}`), signed inside the `Request`, shown by type to the human, and checked by the verifier against what the service is actually about to do (the verifier today only checks scope string and amount).
4. **"Resolved" notice** (ADR 0016 limit): when the approver decides locally, other devices keep showing the request as waiting until expiry. Add an `AgentResolved { request_id }` sync message.
5. **Relay hardening** (session 1 gap list, items 5 and 6): per-IP and per-sender limits, `spawn_blocking` for SQLite, schema migrations, metrics. Also fix the `existed` warning.
6. **Spike OS integration and opt-in telemetry:** need user decisions first (what Spike OS exposes; what "accepted message / within SLO / weekly active" mean precisely). Do not instrument before the definitions exist.
7. Still deferred by design: direct P2P, groups/MLS, mobile app, backup (M6), the independent security review (a hard gate before any private beta).

## 5. Gotchas found this session (save yourself the time)
- **The Bash tool mangled long `python - <<'EOF'` heredocs** twice (a stray "Is a directory" error, once an "unexpected EOF"), sometimes after the script had half-run. Reliable pattern: write the edit script to a file in the scratchpad with the Write tool, run it with `python <file>`, make every replacement `assert s.count(old) == 1`, and preserve line endings. Run `git status` after a failure to see what landed.
- Files in the repo are LF in the index but Windows checkouts warn "LF will be replaced by CRLF". Harmless, but scripted edits must handle both (`newline=""` when reading and writing).
- `cargo fmt` reflows code after scripted edits; grep to confirm an edit still exists. (Same advice as session 1.)
- Clippy's `too_many_arguments` fired on `agent::grant` (8 args); it carries an `#[allow]` with a reason rather than a new struct.
- A validation I added (`--daily-cents` must not be below `--max-cents`) rejected my own first test grant; keep test policies self-consistent.
- When deciding what to assert in an agent test, check the request wasn't already denied *on arrival* by policy; then there is nothing pending to decide (this produced a test that passed for the wrong reason once; fixed).
- `ops::recv` must reload `me` and `sessions` from disk after anything that calls `deliver` / `broadcast_own` (they keep their own copy of the ratchet state). The new `announce_pending` follows the existing `flush_receipts` pattern for this. Forgetting the reload corrupts the ratchet.
- `Outcome` is matched exhaustively in `cli/src/main.rs` (`recv` printer), so a new variant must get an output arm.

## 6. Decisions and approvals
- The user asked for builds ("continue build", "continue") and for commits explicitly each time; commits were made only after the user confirmed in the box. **Do not push or open a PR without asking.**
- Commit trailer used: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` (follow whatever attribution the session's system reminder gives).
- The open stack questions from session 1 (Tauri vs Electron, libp2p timing, license, monorepo) are **still unanswered**. No LICENSE file exists on purpose.
- ADRs 0014 to 0016 are marked "Proposed (implemented)": nobody has formally accepted them.

## 7. Honest status
All of this is unreviewed prototype code (README: "Do not use it for real secrets"). The verifier's single-use guarantee is only as strong as the spent database the service keeps; the audit log protects against outsiders and silent edits, not a compromised owner device; the approval prompt is only as honest as the agent's text until typed actions exist. See each ADR's limits list.
