# Engineering Plan

## 1. Principles [Paper]
Local-first, end-to-end encrypted, P2P when practical, store-and-forward relay when necessary. Use established, audited primitives; no custom cryptography. Don't promise anonymity, zero metadata or serverless delivery.

## 2. Reference architecture
```
Client (desktop / mobile / Spike Box)
  → identity + key management
  → encrypted messaging session
  → direct P2P transport when possible
  → encrypted relay / store-and-forward when necessary
Local encrypted DB = history. Cloud = discovery, optional push, availability — never plaintext.
```

## 3. Proposed stack [Proposed — decision pending]
| Area | Proposal | Notes |
|---|---|---|
| Core language | Rust | Memory safety; bindings to desktop, mobile, WASM; strong libp2p/MLS libraries |
| Layout | Monorepo: `core/`, `relay/`, `cli/`, `docs/`; later `sdk/`, `app/` | Core is a headless library; everything else consumes it |
| Direct 1:1 sessions | Audited protocol via an existing library (Noise-based or Double Ratchet) | Evaluate before choosing; ADR required |
| Groups | MLS (RFC 9420), e.g. `openmls` | Phase 3; keep data model ready |
| Transport | Phase 0–1: relay over QUIC/WebSocket. Phase 2+: add libp2p for NAT traversal / direct paths | Avoids taking on libp2p complexity before the protocol is stable |
| Local storage | SQLite with encryption (e.g. SQLCipher), keys from OS keystore | |
| Desktop shell | Tauri or Electron — **[Open]**; CLI first | |
| Mobile | Phase 3, native shells over Rust core | Push needs relay-side wake-up without content |

Alternative: TypeScript/Node core for faster prototyping and Electron/web fit, at the cost of a weaker security and mobile story. The Rust choice is a recommendation, not a decision.

## 4. Threat model (Phase 0 deliverable)
In scope per paper: compromised devices, malicious relays, impersonation, key substitution, spam, metadata inference, abusive agents.
Required mitigations: verified device pairing; key-change warnings; forward secrecy where supported; key rotation; recovery design; independent review.
Document explicitly what relays can see (sender/recipient IDs, timing, size) and what we do not protect.

## 5. Agent security model
- Agent identity bound to a keypair, issued scoped credentials with expiry and revocation.
- Every request signed; every consequential action requires explicit human approval above a configurable threshold.
- Rate limits per agent and per scope.
- Tamper-evident audit log (hash-chained, signed) of agent and admin actions. It records actions and metadata, not message content, so E2EE is not undermined.
- Out of MVP: autonomous payments, unbounded actions.

## 6. Work breakdown
### Phase 0 (days 0–30)
- ADRs: language, session protocol, transport, storage, license.
- `docs/threat-model.md`.
- Identity: keypair generation, device key, fingerprint display, safety-number verification.
- Session: handshake + encrypt/decrypt 1:1; test vectors; property tests.
- CLI: `init`, `pair`, `send`, `recv`.
- Demo: 2 devices exchange verified encrypted messages (direct or via dev relay).
### Phase 1 (31–60)
- Relay: accepts ciphertext only, per-recipient queue, TTL, auth by signed challenge.
- Offline queue + acks + retry; reconnect handling; fault-injection tests (drops, reorder, duplicate).
- Encrypted local DB; key storage.
- Pairing flow (QR / short code); attachment prototype (chunked, encrypted, hash-verified).
- CI: lint, tests, `cargo audit`/`deny`, fuzzing of parsers.
### Phase 2 (61–90)
- Agent message envelope, signing, scopes, approval flow, receipts.
- Spike OS integration; telemetry (opt-in, content-free); security testing pass; pilot onboarding tooling.
### Phases 3–5
Mobile, MLS groups, multi-device sync, backup/recovery, admin console, billing, hosted relays, SDK beta, self-host docs, external audit.

## 7. Quality and security process
- Threat-model review at every phase gate.
- No custom primitives; dependency review and pinning; SBOM.
- Fuzz and property tests on all wire parsers and state machines.
- Reproducible builds as a goal for the relay and clients.
- Independent security review scheduled before private beta; external audit before public launch (gate: no unresolved criticals).
- Incident response and key-recovery drills before Phase 4.

## 8. Operations
- Relay observability: counts, latency, queue depth, errors. No content, minimal logs, with retention stated.
- Abuse controls: rate limits, spam throttling, block lists, per-org policy.
- Regional relays and SLOs defined in Phase 4. Define the "within SLO" latency before measuring the 99.9% gate.

## 9. Team and capacity [Paper, not confirmed]
Product lead 1; protocol/security 2; desktop/mobile 2; backend/relay 1; QA/SRE 1; shared design and security review.

## 10. Engineering risks
| Risk | Mitigation |
|---|---|
| Crypto/protocol bugs | Established libraries, ADRs, fuzzing, independent audit |
| P2P fails (NAT, mobile) | Relay fallback is core design, not an afterthought |
| Scope creep into a full chat clone | MVP exclusion list enforced at each gate |
| Multi-device and recovery complexity | Model devices and keys properly in Phase 0 data model |
| Metadata leakage via relays | Minimize logs, document honestly, consider padding/batching later |
| Dependency licensing vs. Apache-2.0 | License review during Phase 0 |

## 11. Decisions needed before coding
1. Rust vs TypeScript core.
2. Desktop shell (Tauri / Electron / CLI only for now).
3. libp2p now or after the protocol stabilizes (recommend after).
4. Monorepo layout as above, or split repos.
5. License choice (Apache-2.0 is the paper's candidate).
