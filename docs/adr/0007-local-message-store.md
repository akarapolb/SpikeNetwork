# ADR 0007: Encrypted local message store (multi-device milestone M1)

Status: **Proposed** (implemented)
Date: 2026-10-14

## Problem
`spike recv` printed messages and forgot them; only ratchet state was kept. Multi-device fan-out, history transfer and attachments (see `docs/multi-device-design.html`) all need a durable, de-duplicating message store with stable message ids.

## Decision
1. **Envelope** (`core/src/envelope.rs`): every session message now carries a small JSON envelope `{v, id, ts, kind, body}`. `id` is 128 random bits (hex), identical on every device that stores the message. Unknown versions or kinds are rejected, never half-parsed. Relays never see envelopes.
2. **Store** (`cli/src/messages.rs`): SQLite file `messages.db` in the state directory, created `0600`, WAL, `synchronous=FULL`. One row per message:
   - Clear columns: `id_tag` and `conv_tag`, keyed HMAC-SHA256 tags from a vault **subkey** (`Vault::index_tag`), used for idempotent inserts and conversation lookup.
   - Everything else (id, peer key, direction, time, text) is sealed with the vault key (XChaCha20-Poly1305), with the row's two tags as associated data, so a row cannot be moved to another conversation or id without detection.
3. **Order of writes on receive:** store the message, *then* save ratchet state and identity, then ack. A crash in between leads to redelivery, the old ratchet state decrypts again, and the duplicate insert is ignored. Nothing is lost or doubled (tested). This replaces the "at-most-once display" limitation in ADR 0002.
4. **Sent messages** are stored once the relay accepts them; a failed send is not in history.
5. CLI: `spike history <name> [--limit N]` and `spike prune --older-than-days N`.

## Why per-row encryption, not SQLCipher (yet)
Open question 5 of the multi-device design (SQLCipher licence and build effort) is still the user's to answer. Per-row sealing reuses the audited-lineage primitives we already ship and needs no new native crypto dependency. Cost: no SQL search over content (search will have to decrypt, or use an encrypted index later) and prune must decrypt rows. Revisit when search or scale needs it; the `MessageStore` API hides the choice.

## What a thief of the file (without the passphrase) learns
How many messages are stored, roughly their sizes, and which rows belong to the same (unnamed) conversation. Not who the conversations are with, when messages were sent, which were sent or received, or any content. Timestamps are sealed for this reason.

## Not yet
Edits, deletes, read state (M5), attachments, per-conversation retention settings, search, multi-device ids for the sender device (M2/M3).
