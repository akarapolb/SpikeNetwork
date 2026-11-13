# ADR 0034: Licensing

Status: **decided by the owner, 2026-11-13. Not yet applied to the repository.** No licence files have been added and
no crate's `license` field has been changed. Legal review is still to be done (see "Before publishing").

## Problem

The business plan (section 8) lists Apache-2.0 as the candidate licence and marks it an open legal decision. The
product earns from managed operations (hosted relays, support, the agent directory), so the licence must keep the
code open and trusted without letting a larger competitor host it and add a closed layer for free. It must also stay
easy for companies, and for Center Work, to embed the libraries.

## Decision

A split licence, by what each part is for:

| Part | Licence | Reason |
|---|---|---|
| Relay, SFU (`spike-sfu`), TURN setup | AGPL-3.0 | The hosted service is where revenue comes from. Anyone who modifies and runs these as a network service must publish their changes. |
| `spike-core`, protocol libraries, SDK, `spike-client` | Apache-2.0 | Others must be able to embed these without legal friction. Apache-2.0 also grants patent rights. |
| Clients (desktop, Android, CLI, browser) | Apache-2.0 | Maximum adoption, and no AGPL friction with app stores. |
| Specs, ADRs, threat model | CC BY 4.0 | Reusable with credit. |
| Name and logos | Trademark policy | The code licence does not cover the brand. |

Not published: the hosted operations (billing, tenant admin, dashboards, abuse tooling, runbooks), enterprise extras
(SSO, audit, policy), the hosted agent directory service (its protocol is open), and internal business documents.

### Why not one licence

- **All Apache-2.0:** easiest to adopt, but a competitor could host the relay and sell a closed enterprise layer.
- **All AGPL-3.0:** stronger protection, but many company legal teams ban AGPL dependencies, which would block the SDK
  and the Center Work integration.
- **BSL or another source-available licence:** best revenue protection, but not open source, and it weakens the
  open-protocol and trust position.

Signal uses AGPL-3.0 for its server and clients. It is a nonprofit that does not sell hosting, so its reasons differ
from ours; this is why the client and library side here is Apache-2.0.

## Before publishing

1. **Contributor licence agreement** in place before any outside contribution is accepted. Without it the project
   cannot later offer a commercial licence to companies that will not accept AGPL.
2. **Dependency licence check.** AGPL code links only with compatible licences. Check at least `str0m`, the MLS
   libraries and the WebRTC AAR, which has its own terms. Not yet done.
3. **Legal review of the split** by a lawyer. This is a strategy decision recorded here, not legal advice.
4. The remaining items in `docs/HANDOFF-2026-10-29.md` section 4: secrets scan of the code and git history, README and
   CONTRIBUTING, trademark and domain clearance, export and compliance review.

## Work this implies (not done)

- `LICENSE` files per crate or directory and a `license` field in each `Cargo.toml`, matching the table.
- A `LICENSING.md` at the repository root explaining the split.
- A trademark policy.
- A note in `docs/business-plan.md` section 8 pointing here, replacing the "Apache-2.0 candidate" wording.

## Consequences

- A rival can still host the relay but must publish its changes to it.
- The SDK and clients stay easy to embed.
- A contributor licence agreement becomes a prerequisite for outside contributions.
- The split adds bookkeeping: each crate needs the right licence, and code must not move from an Apache crate into an
  AGPL one, or the reverse, without checking.
