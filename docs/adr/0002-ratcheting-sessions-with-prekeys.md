# ADR 0002: Ratcheting sessions with relay-served prekeys

Status: **Proposed** (implemented; reversible behind `core/src/session.rs`)
Date: 2026-10-13
Supersedes: the message pattern in ADR 0001 (Noise_X).

## Problem
ADR 0001 accepted two limitations of one-shot Noise_X: no forward secrecy against later theft of the recipient's key, and no replay protection in the protocol.

## Decision
Use **Olm** (X3DH-style asynchronous key agreement + Double Ratchet) through the `vodozemac` crate, instead of writing a ratchet ourselves.
- `vodozemac` is the Rust implementation used by Matrix, is Apache-2.0, and has had an external security audit (Least Authority, 2022). Matrix is one of the reference projects in the strategy paper. *Confirm the audit scope and any later changes before relying on this claim.*
- Identity is now an Olm account: Ed25519 signing key + Curve25519 identity key + one-time prekeys. The device card format and `device_id` derivation are unchanged.
- A sender starts a session with one of the recipient's **signed one-time prekeys**, handed out by the relay (at most once each, 30 target / 10 low-water, 50 cap). Prekeys are signed by the recipient's Ed25519 key and verified by both relay (on upload) and sender (before use), so the relay cannot substitute its own.
- The first message of a session is a pre-key message the recipient can process while the sender is offline. Later messages ratchet; replies reuse the session without needing a prekey.

## What this fixes
| Property | Noise_X (ADR 0001) | Olm (this ADR) |
|---|---|---|
| Forward secrecy | No | Yes: tested that stolen current state cannot decrypt earlier captured traffic |
| Replay by relay | Receiver-side id list | Rejected by the protocol (message keys are single-use) |
| Out-of-order delivery | n/a | Supported (skipped keys retained) |
| Post-compromise recovery | No | Yes, after the next ratchet step with an honest exchange |

## Accepted limits and trade-offs
1. **Session config version 1.** It truncates the HMAC to 8 bytes; it is what Matrix deploys. Version 2 (full MAC) exists in the crate behind an `experimental-session-config` feature and is not used.
2. **Prekey exhaustion.** The relay serves each prekey once, and claims are capped per connection (a reconnecting attacker resets the cap). A malicious relay or abuser can drain a device's prekeys so that new contacts cannot start sessions until it reconnects and replenishes. No fallback key is implemented yet.
3. **No fallback to one-time-key-less sessions.** If no prekey is available the send fails with an explanatory error rather than weakening security.
4. **Sessions per peer capped at 4;** simultaneous first messages from both sides create two sessions and both remain usable.
5. **At-most-once display.** State is persisted before the ack, so a crash after persisting but before printing loses that message from the UI (it is not redelivered).
6. State at rest is encrypted by [ADR 0003](0003-encrypted-state-at-rest.md).
7. Groups remain out of scope (MLS, Phase 3).

## Consequences
Phase 0 messages now have forward secrecy and replay protection by design, but the code is still unreviewed by an independent party. The "do not use for real secrets" statement stays until independent review is done.
