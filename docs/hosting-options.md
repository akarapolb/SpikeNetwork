# Hosting relays: options and a decision for you (roadmap Phase 4)

> You chose the multi-tenant hosted service (option C). The scope, slices and the decisions it needs are in [`hosted-relay-plan.md`](hosted-relay-plan.md).

**Status: a brief for a decision, not a plan that has been agreed.** Nothing here is built beyond the relay itself (`docs/deploy-relay.md` explains how anyone can run one today). The roadmap puts "hosted relays, billing, org accounts, self-host docs" in months 7 to 9; this note says what that choice involves so it can be made deliberately and not by default.

## What "hosted" would have to mean
A relay stores only ciphertext and metadata, so hosting is mostly about **who operates it, who can reach it, who pays, and what happens when it is abused or compelled**. The product promise is that the operator cannot read content, so the hosted offer is "we run the pipe", never "we keep your messages".

## Options
| | What it is | Good for | Costs and risks |
|---|---|---|---|
| **A. Self-host only** (today) | Each group runs its own relay from `docs/deploy-relay.md`: a small VPS, a home server, or a Spike Box | The privacy-first story; design partners with IT; zero operating liability for Spike | Every partner needs someone who can run a server and renew certificates. Slow onboarding, and Spike sees no usage |
| **B. One relay Spike operates, for pilots** | A single small VPS run by Spike for design partners, with an allow-list or invite-only access | The 5 to 10 pilot groups; fastest way to get real use and the delivery numbers the release gates need | Spike becomes an operator: metadata exposure, availability duty, abuse handling, and a legal-requests process (business plan 8a). The relay has no per-user limits yet, so access must be restricted |
| **C. Multi-tenant hosted relays with accounts** | Per-organisation relays or namespaces, sign-up, billing, quotas, admin | The Phase 4 commercial product | A real build: accounts or tokens, per-tenant quotas and rate limits, metrics, backups, replication, abuse tooling, billing. Months of work and an ongoing operations load |
| **D. Bundled appliance ("Spike Box")** | The relay shipped as a small box or image that a customer plugs in | Customers who want no cloud at all; the strongest privacy position | Hardware or image support, updates, and signing; much later |

## Recommendation
**B for the pilot, A for anyone who asks, C only after the pilot says people will pay.** Concretely: stand up one small relay with TLS and access limited to pilot users, publish the self-host guide for the rest, and do not build accounts and billing until the pilot shows demand (kill/pivot review at day 90). This keeps the commitment small and still produces the measurements the gates need.

## Decisions I need from you
1. **Who operates a pilot relay, and under what legal entity?** Operating one makes Spike a service provider that can be asked for metadata. The business plan's stance (cooperative and transparent, little to hand over) assumes a legal-requests policy exists first; it does not yet.
2. **Where should it run?** A small VPS at a mainstream provider in a jurisdiction you are comfortable with, versus Spike's own hardware. The choice affects which authorities can compel what.
3. **Access control:** invite-only by device list, a shared token, or an IP/VPN allow-list. The relay today lets any device register and use its quota, so something must restrict a public one.
4. **What do we promise?** Retention (messages are kept up to 7 days if undelivered), uptime (a single node has no failover), and what we log (today: no content, connection errors only).
5. **Budget and timing:** a pilot relay is a small recurring cost plus someone on call; multi-tenant hosting is a project. I have not estimated prices; get quotes for your chosen provider and region.

## Engineering that any hosted option needs first
- Per-IP and per-sender rate limits, `spawn_blocking` (or a pool) for SQLite, schema migrations, and basic metrics that never include content.
- A published legal-requests policy and a decided response to an order to weaken the product (business plan 8a).
- The independent security review before public use, as the roadmap's release gates require.
