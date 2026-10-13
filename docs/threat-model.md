# Threat model (Phase 0 draft, updated for ADR 0002 and 0003)

Scope: the code in this repository today (`core`, `relay`, `cli`). It will grow with the product. Each item is marked **mitigated**, **partial**, or **open**.

## What an observer or attacker can see
| Party | Can see | Cannot see |
|---|---|---|
| Network observer on a device ↔ relay link | IP addresses, timing, sizes (**the link is plain `ws://`; put TLS in front of the relay**) | Message content (end-to-end encrypted) |
| Relay operator | Recipient device ID, the authenticated uploader's device ID, blob size, timing, IPs, who claims whose prekeys (so it can infer who is starting conversations) | Message plaintext, session keys |
| Recipient | Nothing about the sender from the relay (sender IDs are not forwarded); sender identity comes from the cryptographic session | |
| Thief of an *unlocked* device's current state | Future messages until the next honest ratchet step; impersonation of that device | Earlier captured traffic (forward secrecy) |
| Thief of a *locked* device or backup | Vault salt, KDF parameters, file sizes and times | Contents, contact names, keys (without the passphrase) |

No anonymity and no metadata protection is claimed.

## Threats
| Threat | Status | Notes |
|---|---|---|
| Relay reads messages | Mitigated | Olm encryption; tested that plaintext is absent and third parties cannot decrypt |
| Relay or attacker modifies a message | Mitigated | Authenticated encryption; tamper test flips bytes at several positions. MAC is truncated to 8 bytes (session config v1, see ADR 0002) |
| Relay replays or reorders messages | Mitigated for content | Replays are rejected by the protocol; out-of-order works. A relay can still **delay or drop** messages undetectably (no delivery receipts yet) |
| Impersonation of a sender | Mitigated for known contacts | First messages from unknown keys are dropped and create no session |
| Impersonation of a device to the relay | Mitigated | Ed25519 challenge-response; wrong-key test |
| Relay forges or swaps a recipient's prekey | Mitigated | Prekeys are signed by the recipient's key and verified by sender and relay; forged-prekey tests |
| Key substitution during pairing | Partial | Cards are self-signed (keys can't be mixed). A substituted *whole* card is only caught if users compare the fingerprint out of band; contacts start **UNVERIFIED** and the CLI warns |
| Key change of an existing contact | Mitigated | `add` refuses a different key for an existing name unless `--force`, which resets verification |
| Compromise of a device's keys exposes history | Mitigated for earlier traffic | Forward secrecy tested. Anything still stored on the device, and sessions' future messages until recovery, are exposed |
| Prekey exhaustion / contact-start denial of service | Partial | Single-use prekeys, per-connection claim cap (resettable by reconnecting); no fallback key; no per-account rate limit |
| Stolen disk or leaked backup | Mitigated | All state is encrypted under an Argon2id-derived key (ADR 0003); tests check nothing identifying is stored in the clear and that tampering, truncation and file swaps are rejected. Strength depends on the passphrase |
| Rollback to an older valid state file | **Open** | An attacker with write access could restore an old ratchet state; needs a monotonic counter / hardware keystore |
| Compromised running endpoint (malware, debugger) | **Open** | Keys are in memory while unlocked; out of scope for Phase 0 |
| Relay resource abuse / spam | Partial | Size, per-recipient queue cap and TTL enforced; no per-sender rate limit or abuse controls |
| Relay restart loses queues and prekeys | Known gap | In memory; recipients re-publish prekeys on next connect |
| Message lost on crash between persist and display | Known gap | At-most-once display (ADR 0002) |
| Metadata inference | **Open** | Needs padding/batching research; documented, not solved |
| Abusive agents | **Open** | Agent permissions and approval flow are Phase 2 |

## Before any real use
Independent security review, TLS, persistent relay queues and prekeys, per-sender and per-account rate limiting, a fallback prekey, delivery receipts.
