# ADR 0015: Receipt verifier (Phase 2, second slice)

Status: **Proposed** (implemented)
Date: 2026-10-15

## Problem
ADR 0014 produces signed receipts, but "the agent does not enforce anything": nothing checked a receipt before an action happened, and nothing stopped one approval being used twice. This slice adds the service side.

## Decision
`core/src/verifier.rs` (pure logic) and `cli/src/verifier.rs` (storage). No new cryptography: Ed25519 signatures with a new domain-separation prefix.

- **Presentation**: what an agent hands the service: its signed request, the principal's receipt, an **audience** (the service's name), a timestamp, and a **proof** (the agent's signature over audience, request hash and time). The proof means a thief who captured the request and receipt, or a service that received a presentation, cannot replay it elsewhere without the agent key.
- **`check`** (no side effects), in order: shape, agent signature, **principal is one the service trusts**, receipt answers exactly this request, decision is *approved*, audience matches, proof valid, presentation fresh (5 min skew), request not expired or future-dated, approval not signed long after the request lapsed, **scope equals** the action's scope, **amount at or below** the approved amount (no money approved means none may move).
- **`redeem`** = `check`, then record `principal/agent/request-id` in a `SpentStore`. A second redeem of the same approval, even in a fresh presentation, is `AlreadyUsed`. Records are kept until the request's expiry plus skew, after which the request would fail `check` anyway.
- `SqliteSpent` is the durable store (primary key makes it atomic across processes sharing the file); `MemorySpent` is for tests.
- CLI: `spike agent-present <id> --audience <svc>` (agent) and `spike verify-receipt --audience --principal --scope [--amount-cents] --spent-db` (service; needs no identity or passphrase; exit 0 + JSON, exit 2 refused). `--principal` takes an owner key or an identity card.

## Also in this change
- `spike` runs on an 8 MiB thread: with this many subcommands, clap's parser overflowed Windows' 1 MiB main stack in debug builds (`--help` crashed).
- A closed pipe (`spike history bob | head`) exits quietly instead of panicking.

## Limits (honest list)
- **Single use is only as good as the store.** Delete the spent database and approvals are usable again until they expire. Services must keep it and share it between instances.
- **Redeem-then-act is not atomic with the action.** If the service redeems and then crashes before acting, the approval is lost (fail closed); the agent must ask again. The reverse (act, crash, not recorded) cannot happen if the service redeems first, as documented.
- **The service chooses which principals to trust.** There is no directory; the operator configures owner keys. Revoking a grant after approval does not un-approve: receipts live until the request's expiry (at most 24 h). Use short expiries for risky actions.
- **Amounts are cumulative nowhere**: each request is capped alone (unchanged from ADR 0014). Typed actions and cumulative spending limits are still future work.
- The service trusts its own clock (5 minutes of skew allowed).
- Unreviewed code, like everything else here.
