# ADR 0001: Phase 0 stack and sealed-message protocol

Status: **Partly superseded by [ADR 0002](0002-ratcheting-sessions-with-prekeys.md)**: the stack, identity, addressing and relay-transport decisions stand; the Noise_X message pattern and its limitations were replaced.
Date: 2026-10-13

## Context
Phase 0 needs two devices exchanging verified encrypted messages, with a relay for offline delivery. The strategy paper requires established primitives and no custom cryptography. The open decisions (language, shell, libp2p timing, license) were not yet answered, so this ADR records the defaults used to start.

## Decisions
| Area | Choice | Why | Revisit when |
|---|---|---|---|
| Language | Rust workspace (`core`, `relay`, `cli`) | Memory safety; reusable from desktop, mobile, WASM | Before Phase 3 clients |
| Crypto | `snow` (Noise), `ed25519-dalek`; no hand-rolled primitives | Established libraries | Independent review |
| Message pattern | `Noise_X_25519_ChaChaPoly_BLAKE2s` | One message, sender authenticated, recipient can be offline | Phase 1 (see below) |
| Identity | Ed25519 key (signing, relay auth) + X25519 key (Noise), bound by a self-signature in a *device card* | Keys cannot be split or swapped | |
| Addressing | `device_id` = first 16 bytes of SHA-256(Ed25519 public key), hex | Derivable from the card, no registry | |
| Relay transport | WebSocket, JSON frames, hex blobs | Simple to debug; libp2p deferred until the protocol is stable | Phase 2 |
| Relay auth | Challenge-response with the device Ed25519 key | No passwords or accounts | |
| License | Not yet chosen; no `LICENSE` file added | Legal decision | Before any public release |

## Known limitations of Noise_X (accepted for Phase 0)
1. **No forward secrecy against recipient key compromise.** Anyone who later steals a recipient's static X25519 key can decrypt every message ever sent to it that they also captured. There is no ratchet.
2. **No replay protection in the protocol.** A malicious relay can resend a ciphertext. Receivers de-duplicate on a random per-message id stored locally (`seen.json`).
3. **No post-compromise security** and **no deniability guarantees** have been analysed.
4. Messages are limited to 60,000 bytes; attachments are not implemented.

## Replacement path
Phase 1 should replace one-shot Noise_X with an asynchronous ratcheting session (X3DH/PQXDH-style prekeys plus Double Ratchet, using an audited implementation) for 1:1, and MLS (RFC 9420) for groups in Phase 3. The wire envelope (`seal`/`open`) is the only API callers use, so this swap is local to `core/src/session.rs`.

## Consequences
Phase 0 messages are suitable for demonstrating the architecture, not for real secrets. Every release before an independent review must say so.
