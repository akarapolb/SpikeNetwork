# ADR 0004: Durable relay storage

Status: **Proposed** (implemented)
Date: 2026-10-13

## Decision
The relay stores queued ciphertext and prekeys in SQLite (`rusqlite`, bundled), enabled with `spike-relay --db <path>` or `SPIKE_RELAY_DB`. Without `--db` it runs in memory and says so at startup.

- `synchronous=FULL` (and WAL): the relay answers "accepted" only after the message is committed, so an accepted message survives a crash or power loss.
- Acknowledgements delete rows durably; prekeys are removed in the same statement that returns them (`DELETE … RETURNING`), so each is handed out once even across crashes.
- Per-recipient queue cap, TTL and expiry purge (lazy plus every 60 s) still apply; expiry uses wall-clock time so it holds across restarts.
- The database file is created `0600`.
- A message that arrives while its recipient is connecting is delivered either from the backlog or live, never both (state lock held across backlog read and registration).

## What the relay's disk reveals
Ciphertext (opaque), recipient device IDs, message sizes, and timing. Not plaintext, sender IDs or keys that can read anything. This is still metadata: an operator or thief of the file learns who receives how much and when, until messages are acked or expire. Operators should put the file on an encrypted volume; the relay does not encrypt the database itself.

## Limits
- Single process, single file: no replication or high availability yet. A lost disk loses queued messages and prekeys (clients re-publish prekeys on next connect; undelivered messages are lost).
- Blocking SQLite calls run on the async runtime threads. Fine at pilot scale; move to `spawn_blocking` or a pool before load testing.
- No schema migrations yet (`CREATE TABLE IF NOT EXISTS` only).
- No backups or retention policy beyond TTL.
