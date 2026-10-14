# Threat model (Phase 0 draft, updated through ADR 0012)

Scope: the code in this repository today (`core`, `relay`, `cli`). It will grow with the product. Each item is marked **mitigated**, **partial**, or **open**.

## What an observer or attacker can see
| Party | Can see | Cannot see |
|---|---|---|
| Network observer on a device ↔ relay link | IP addresses, the relay's host name (TLS SNI), connection timing, approximate sizes | Message content (end-to-end encrypted); device ids and protocol messages (TLS). *If an operator opts into plain `ws://`, they also see device ids and timing.* |
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
| On-path attacker tampers with or reads the relay connection | Mitigated with `wss://` | rustls, hostname + chain verification, plain `ws://` refused off loopback by default (ADR 0005). Content is protected by E2EE regardless |
| Impersonation of a device to the relay | Mitigated | Ed25519 challenge-response; wrong-key test |
| Relay forges or swaps a recipient's prekey | Mitigated | Prekeys are signed by the recipient's key and verified by sender and relay; forged-prekey tests |
| Key substitution during pairing | Partial | Cards are self-signed (keys can't be mixed). A substituted *whole* card is only caught if users compare the fingerprint out of band; contacts start **UNVERIFIED** and the CLI warns |
| Device-list rollback, equivocation, or a device added without the owner key | Mitigated | Lists are signed by the person's owner key; lower versions and two different lists with one version are refused even with `--force` (ADR 0008). Lists are published on relays (ADR 0009); relays enforce the same rules and only accept a list from a device in it; clients re-check everything they fetch |
| Relay withholds a newer device list (sender misses a new device) | Partial | Senders refresh before every send and on any unknown sender; key transparency planned (engineering plan 7a) |
| Someone forges an edit or delete of another person's message | Mitigated | Only the author (same person, same conversation) may change a message; other changes are refused and reported (ADR 0012) |
| A deleted message is kept by a recipient's device or screenshot | Open, inherent | Deletion is honoured by honest devices only; the product must not promise erasure |
| Relay or attacker reads or alters attachments | Mitigated | Per-file random key, authenticated chunks bound to blob id/index/count, whole-file hash; tampered chunks refused, nothing kept (ADR 0011). File sizes are visible to the relay |
| Malicious file name writes outside the chosen folder | Mitigated | Names sanitised before any write; no overwrite without `--force` (ADR 0011) |
| Someone intercepts or hijacks device linking | Mitigated | Link code travels out of band (QR); ephemeral key agreement salted with a secret from the code; 8-digit safety code confirmed on both screens before anything is shared; bundle frames authenticated and unreadable to the relay (ADR 0010) |
| A compromised device adds a rogue device | Partial | Every device holds the owner key (ADR 0010 option a). Contacts are notified of device-list changes; any device can remove others. An offline owner key is a possible later option |
| Removed or stolen device keeps receiving | Mitigated for new messages | Removal publishes a newer signed list; senders stop encrypting to it and the device refuses to send (ADR 0009). It keeps what it already holds |
| Key change of an existing contact | Mitigated | `add` refuses a different key for an existing name unless `--force`, which resets verification |
| Compromise of a device's keys exposes history | Mitigated for earlier traffic | Forward secrecy tested. Anything still stored on the device, and sessions' future messages until recovery, are exposed |
| Prekey exhaustion / contact-start denial of service | Mitigated | A reusable, signed, weekly-rotated fallback prekey keeps a drained device reachable (ADR 0006). Cost: first messages of sessions started via the fallback have weaker forward secrecy while that key is held. Claim cap is per connection only (resettable); no per-IP/recipient limit yet |
| Stolen disk or leaked backup | Mitigated | All state is encrypted under an Argon2id-derived key (ADR 0003); tests check nothing identifying is stored in the clear and that tampering, truncation and file swaps are rejected. Strength depends on the passphrase |
| Rollback to an older valid state file | **Open** | An attacker with write access could restore an old ratchet state; needs a monotonic counter / hardware keystore |
| Compromised running endpoint (malware, debugger) | **Open** | Keys are in memory while unlocked; out of scope for Phase 0 |
| Relay resource abuse / spam | Partial | Size, per-recipient queue cap and TTL enforced; no per-sender rate limit or abuse controls |
| Relay crash or restart loses queues and prekeys | Mitigated with `--db` | Durable SQLite storage (ADR 0004); verified by tests and a real `kill -9`. Without `--db` the relay is in-memory and says so. Single disk, no replication: a lost disk loses undelivered messages |
| Theft of the relay database | Partial | Contains ciphertext and routing metadata only (recipient IDs, sizes, timing), `0600`; not encrypted by the relay itself, so use an encrypted volume |
| Message lost or duplicated on crash during receive | Mitigated | Stored before ratchet state is saved; idempotent on message id; tested (ADR 0007) |
| Stolen disk reveals message history | Mitigated | Every history row sealed under the vault key; only keyed tags in clear; tampering and moved rows detected (ADR 0007). Message count and sizes still visible |
| Metadata inference | **Open** | Needs padding/batching research; documented, not solved |
| Abusive agents | **Open** | Agent permissions and approval flow are Phase 2 |

## Powerful adversaries and compelled access

**Stance.** Spike Connect is not built against any government. It obeys the law where it operates, says plainly what it can and cannot technically provide, and is designed so that what it *can* provide is very little. Strong security protects citizens, businesses and public institutions from criminals, foreign intelligence services and insiders; the same properties also limit what any single operator can be compelled to hand over or change. This section treats "powerful adversary" as a **capability class**: well-resourced criminal groups, foreign state actors, rogue insiders, and legal orders that compel the operator to act.

| Threat | Status today | Planned mitigation | Residual risk |
|---|---|---|---|
| Operator ordered to disclose message content | Mitigated | Relays hold only ciphertext; no accounts, contact lists or history on servers; local state encrypted under the user's passphrase | None for content the operator never had |
| Operator ordered to disclose metadata | Partial | No relay logging and short retention on hosted relays; sealed sender (relay does not learn who sent); size padding; encouraging self-hosted relays | A relay still sees which device receives traffic and when |
| Operator compelled to ship a modified (backdoored) client, for everyone or one target | **Open** | Open source; reproducible builds; binary transparency (every release in a public append-only log, clients refuse unlogged updates); release signing split across several people in different jurisdictions using hardware security modules; published independent audits | Users who skip verification or install from untrusted sources |
| Server-side key substitution to intercept a contact | Partial | Device lists and prekeys signed by the person's keys (ADR 0002, 0008); rollback and equivocation refused; safety numbers; **key transparency** log so a server cannot show different keys to different people undetected | First contact via an unverified invite link is trust-on-first-use until safety numbers are compared |
| Service blocked in a country | **Open** | Many independent and self-hosted relays; direct device-to-device paths; proxy and circumvention transports; distribution outside app stores | Determined network blocking can still degrade service |
| Seized or confiscated device | Partial | Encrypted at rest (ADR 0003); remote device removal and disappearing messages (multi-device design) | An unlocked device, or a coerced passphrase |
| Targeted malware on a user's phone | Out of scope | Platform security, prompt updates, audits | Any end-to-end system is defeated by a fully compromised endpoint |

**Legal and organisational measures** (business plan, section 8): jurisdiction choice for the company and hosted relays, a lawful-access exposure review per country before launch, a published policy for handling legal requests, and transparency reports where the law allows.

## Before any real use
Independent security review, reproducible builds with binary transparency (before public launch), per-sender and per-account rate limiting, delivery receipts, relay-side rate limiting.
