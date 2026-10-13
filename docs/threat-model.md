# Threat model (Phase 0 draft)

Scope: the code in this repository today (`core`, `relay`, `cli`). It will grow with the product. Status of each item is stated honestly: **mitigated**, **partial**, or **open**.

## What an observer or attacker can see
| Party | Can see | Cannot see |
|---|---|---|
| Network observer on a device ↔ relay link | IP addresses, timing, sizes (**the link is plain `ws://` in Phase 0; use TLS in front of the relay**) | n/a once TLS is added: content is already end-to-end encrypted |
| Relay operator | Recipient device ID, the authenticated sender's device ID at upload time, blob size, timing, IPs | Message plaintext, sender identity inside the message |
| Recipient's relay peer | Nothing about who sent a message (the relay does not forward sender IDs) | |
| Thief of a device's key file | Everything sent to that device that they also captured; ability to impersonate it | |

No anonymity and no metadata protection is claimed.

## Threats
| Threat | Status | Notes |
|---|---|---|
| Relay reads messages | Mitigated | Noise_X encryption to the recipient's static key; tested that plaintext is absent and third parties cannot open |
| Relay or attacker modifies a message | Mitigated | AEAD authentication; tamper test flips bytes at several positions |
| Impersonation of a sender | Mitigated for known contacts | Sender key is authenticated by Noise; unknown keys are dropped |
| Impersonation of a device to the relay | Mitigated | Ed25519 challenge-response; wrong-key test |
| Key substitution during pairing | Partial | Cards are self-signed (keys can't be mixed). A substituted *whole* card is only caught if users compare the fingerprint out of band; contacts start **UNVERIFIED** and the CLI warns |
| Key change of an existing contact | Mitigated | `add` refuses a different key for an existing name unless `--force`, which resets verification |
| Replay of old ciphertext by relay | Partial | Receiver de-duplicates by message id. A relay can still *delay* or *drop* messages; no delivery receipts yet |
| Recipient key compromise exposes history | **Open** | No forward secrecy (ADR 0001) |
| Compromised endpoint | **Open** | Out of scope; keys are in a 0600 plaintext file in Phase 0 (encrypted storage is Phase 1) |
| Relay resource abuse / spam | Partial | Size, per-recipient queue cap and TTL enforced; no per-sender rate limit or abuse controls yet |
| Relay restart loses queue | Known gap | Queues are in memory |
| Metadata inference | **Open** | Needs padding/batching research; documented, not solved |
| Abusive agents | **Open** | Agent permissions and approval flow are Phase 2 |

## Before any real use
Independent security review, TLS, encrypted key storage, ratcheting sessions, persistent relay queues, per-sender rate limiting.
