# ADR 0014: Agent requests with human approval (Phase 2, first slice)

Status: **Proposed** (implemented)
Date: 2026-10-14

## Decision
An **agent** is an ordinary Spike identity (its own owner key) that a person added as a contact. It asks its **principal** (a person) for permission to act. Four signed objects (`core/src/agent.rs`), all Ed25519 with domain-separation prefixes and no new cryptography:

- **Request** (signed by the agent): id, agent key, **principal key**, scope (`files.read`, `payments.send`, ...), a human-readable action, optional amount in cents, created and expiry (at most 24 h). Binding the principal stops a request being replayed to someone else; binding every field means an intermediary cannot change the amount.
- **Grant** (signed by the principal): the standing policy for one agent: allowed scopes, expiry, a hard cap per request (`max_cents`; none means no money at all), an auto-approve limit (`auto_below_cents`; none means always ask a human), and a per-hour request limit. **Anything outside a grant is denied; there is no implicit permission.**
- **Receipt** (signed by the principal): approved or denied, by *human* or *policy*, bound to the request's hash. The agent accepts it only if it answers a request it made, comes from the person it asked, and verifies. It also carries the position and hash of the audit entry that recorded the decision.
- **Audit entry** (signed by the principal's owner key): sequence, time, event (`grant`, `revoke`, `request`, `approved`, `denied`), agent, request hash, and **scope and amount only, never the action text or message content** (so E2EE is not undermined). Each entry's hash covers the previous entry's hash; `spike agent-audit` verifies the whole chain and every signature.

Transport: two new envelope kinds, `agent_request` and `agent_receipt`, over the normal end-to-end encrypted sessions. Relays see nothing new.

Flow: `agent-grant` (once) -> agent `agent-request` -> principal `recv` evaluates the grant: **deny** (no grant, expired, wrong scope, over cap, rate limit, stale or future-dated), **auto-approve** (within the limit), or **wait for a human** (`agent-pending`, `agent-approve`, `agent-deny`). Policy decisions send their receipt immediately; human decisions send it when made, and an undelivered receipt stays queued (`agent-flush`, and `recv` retries). Approving **re-checks the grant now**: a revoked or expired grant cannot approve (denying always works). At most 100 requests may wait; more are denied.

**Truncation:** a log alone cannot show that its newest entries were deleted. Every receipt carries the audit head it created, and the agent remembers the highest head it saw per principal; a later receipt with a lower or conflicting head is reported (`its audit log went backwards or forked`) and not accepted.

## Single approver device
A person's request goes to all their devices (normal fan-out), but only the **first device in the signed device list** acts on it, so a request is decided and logged exactly once and there is one audit chain. Other devices ignore agent requests. This is a deliberate limit of the first slice (approving from the phone is the natural case and needs the approval to move between devices; planned). If the first device is removed, the next one takes over with a fresh log.

## Tested (`cli/tests/agents.rs`, `core/src/agent.rs`)
Ask, approve, receipt, auto-approve, scope denial; audit log verifies and holds no action text; edit/remove/reorder/re-hash/forge of log entries detected (unit and on disk); revoked grants cannot approve; no grant means deny; a request signed by a key other than its sender's is refused; a tampered amount fails the signature; duplicates handled once; an agent cannot forge its own approval; per-hour limit; audit rollback seen by the agent; grants and receipts reject tampering.

## Limits (honest list)
- **The agent does not enforce anything.** A receipt is a signed fact. The service that performs the action (a payment API, a file server) must itself require and check a valid receipt (principal signature, request hash, expiry, single use). No such verifier exists yet; this slice builds the approval and evidence layer only.
- Receipts are not yet single-use tokens with their own expiry; a verifier must remember the request ids it has honoured.
- A compromised principal device holds the owner key and can sign anything, including audit entries. The log is evidence against outsiders and accidents, and against silent edits by someone without the owner key, not against the owner's own device.
- The approval prompt shows the action text the agent wrote. A lying agent can describe an action misleadingly; the scope and amount are shown beside it and are what policy checks. Structured, typed actions are future work.
- Single approver device, local log (not replicated), a cumulative 24-hour cap is available from ADR 0015's follow-up (`--daily-cents`, below), no agent-to-agent delegation, no revocation notice sent to the agent.
- Clock-based expiry trusts the principal's clock for its own decisions and allows 5 minutes of skew.

## Addendum 2026-10-15: cumulative daily cap
`GrantBody.daily_cents` (optional; `agent-grant --daily-cents`) caps money over any rolling 24 hours. It counts requests that were approved **and requests still waiting**, so many small or slow requests cannot exceed it, and auto-approval is denied once the cap would be passed. Approving re-checks it against everything else committed, so tightening a grant also stops waiting requests. The field is omitted from the signed body when unset, so grants signed before it existed verify unchanged. Limits: it is counted from the principal's local inbox on the approver device only; it limits what is *approved*, not what a service later spends (the service enforces the amount in the receipt, ADR 0015).
