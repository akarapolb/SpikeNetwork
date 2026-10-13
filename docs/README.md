# Spike Connect — Planning Docs

Source: *Spike Connect: Product Vision, Roadmap, Business Model* (strategy working paper v1.0, 13 Oct 2026).

| Doc | Purpose |
|---|---|
| [product-roadmap.md](product-roadmap.md) | What we build, in what order, and the gates between phases |
| [business-plan.md](business-plan.md) | Segments, packaging, pricing hypotheses, go-to-market, validation |
| [engineering-plan.md](engineering-plan.md) | Architecture, work breakdown, security approach, team, risks |

Status legend used across docs: **[Paper]** = taken from the strategy paper; **[Proposed]** = new suggestion needing a decision; **[Open]** = unresolved question.

## Engineering artifacts
| Doc | Purpose |
|---|---|
| [threat-model.md](threat-model.md) | Phase 0 threat model with honest status per threat |
| [adr/0001-phase0-stack-and-sealed-messages.md](adr/0001-phase0-stack-and-sealed-messages.md) | Stack decisions (message pattern superseded by 0002) |
| [adr/0002-ratcheting-sessions-with-prekeys.md](adr/0002-ratcheting-sessions-with-prekeys.md) | Olm ratcheting sessions, prekeys, accepted limits |
| [adr/0003-encrypted-state-at-rest.md](adr/0003-encrypted-state-at-rest.md) | Passphrase-encrypted local state: what it covers and what it does not |
| [adr/0004-durable-relay-storage.md](adr/0004-durable-relay-storage.md) | SQLite-backed relay queues and prekeys; what the relay disk reveals |
| [adr/0005-tls-for-relay-connections.md](adr/0005-tls-for-relay-connections.md) | wss://, safe defaults, certificate handling |
| [architecture.html](architecture.html) | Architecture diagrams |
| [spike-connect-plans.html](spike-connect-plans.html) | All three plans in one page |
