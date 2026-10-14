# Spike Connect — Planning Docs

Source: *Spike Connect: Product Vision, Roadmap, Business Model* (strategy working paper v1.0, 13 Oct 2026).

| Doc | Purpose |
|---|---|
| [HANDOFF.md](HANDOFF.md) | **Start here in a new session:** status, code map, gaps, next steps |
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
| [adr/0006-fallback-prekey.md](adr/0006-fallback-prekey.md) | Last-resort prekey: reachability vs first-message forward secrecy |
| [adr/0007-local-message-store.md](adr/0007-local-message-store.md) | Encrypted message history (multi-device M1): envelope ids, sealed rows, crash-safe receive |
| [adr/0008-identity-and-device-lists.md](adr/0008-identity-and-device-lists.md) | Owner key and signed device lists (multi-device M2): card v2, update rules, legacy cards |
| [adr/0009-fan-out-and-relay-directory.md](adr/0009-fan-out-and-relay-directory.md) | Fan-out to every device, sent copies, home relays, relay device-list directory (multi-device M3) |
| [adr/0010-device-linking.md](adr/0010-device-linking.md) | Linking a new device: link code, safety code, encrypted history bundle (multi-device M4) |
| [adr/0011-attachments.md](adr/0011-attachments.md) | Encrypted attachments and the relay blob store (Phase 1 attachment prototype) |
| [architecture.html](architecture.html) | Architecture diagrams |
| [multi-device-design.html](multi-device-design.html) | **Proposed** design: message history across multiple devices, fan-out, device linking, and which servers are still needed |
| [multi-device-demo.html](multi-device-demo.html) | **Interactive simulation** of that design (no real crypto): fan-out, offline catch-up, device linking, edits, relay outage, attacks. Open in a browser |
| [mobile-prototype/](mobile-prototype/README.md) | **Design prototype** of the mobile app: 11 clickable screens (identity, chats, verification, devices, linking, network, agent approval and log, adding contacts, message requests) |
| [spike-connect-plans.html](spike-connect-plans.html) | All three plans in one page |
