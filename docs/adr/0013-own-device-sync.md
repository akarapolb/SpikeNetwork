# ADR 0013: Own-device sync channel (multi-device milestone M5, completed)

Status: **Proposed** (implemented)
Date: 2026-10-14

## Decision
A new envelope kind `sync` carries housekeeping between a person's **own** devices. It is end-to-end encrypted over the same per-device sessions as messages (ADR 0009). **A device accepts it only from a device in its own signed device list**; the same message from a contact is refused and reported (`Rejected`), never acted on and never answered (tested).

Messages (`cli/src/sync.rs`, validated on receipt):
- **Read state** `read {peer, upto}`: `spike read <name>` moves this device's read marker for a conversation to its newest incoming message and tells the other devices. Markers merge by max and never move backwards. `spike unread` shows counts. (The CLI marks read explicitly; the app will do it when a conversation is opened.)
- **Catch-up** `catch_up_request {since}` and `catch_up_batch {messages, last}`: a device asks its other devices for everything they learned or changed since a time. Cursor = "when I last finished collecting my relay queue" (`lastsync.enc`) minus a 24 h margin for clock differences; each stored message carries `changed_at` (local clock, set on every write), so answers include late-arriving messages, edits and tombstones. Answers are sliced into batches that each fit one envelope. Incoming records merge under the ADR 0012 rules (unknown messages inserted, tombstones included; a delete always wins; a newer edit wins; inconsistent records ignored); a deleted message's stored file is removed. Repeating the request changes nothing.
  Why not a "highest clock value" cursor: clocks are not delivery order, so a late message with a low clock would be skipped forever. The time-based cursor with duplicates tolerated is simpler and correct.
- **File requests** `file_request {msg_id}` and `file_offer {msg_id, attachment}`: when the relay has expired an attachment, a device asks its other devices. A device that holds the bytes (and whose message is not deleted) re-encrypts them under a fresh key, uploads a new blob to its relay and offers the new descriptor. The requester accepts it only if **hash, size and name equal** the message's existing descriptor (otherwise rejected, tested), then fetches normally and verifies again. Devices without the bytes stay silent.
- Requests are answered inside `spike recv` (after acknowledging the request), so an answer that fails can simply be asked for again.

CLI: `spike read`, `spike unread`, `spike sync [--wait-secs N]`, `spike request-file <id>`.

## Tested (`cli/tests/ownsync.rs`, `cli/src/sync.rs`, `core` envelope)
Read state clears on the other device and only moves forward; a contact's sync messages of every kind are refused and trigger nothing; a laptop that missed messages, an edit and a delete (relay copies expired) converges to the phone exactly, tombstone included, and repeating the request is a no-op; an expired attachment is re-supplied by another device; devices without the file stay silent, and a deleted message's file is never offered; a mismatching file offer is rejected and leaves the message unchanged; batches fit an envelope and malformed payloads are refused. The demo shows read state following the user and a tablet linked with **no history** filling itself in (0 to 8 messages) by catch-up.

## Limits
- The answering device must be online and run `recv`; there is no push. A device that nobody can answer stays behind until one can.
- A **compromised own device** can feed its siblings false history: it already holds the owner key (ADR 0010), so this is inside the trust boundary. Contacts' messages cannot be forged this way: merged records must match their conversation and author direction.
- `lastsync.enc` is set only when `recv` finishes (not in `--follow` mode); a follow-mode device can ask for a broader catch-up by being restarted.
- No resumable or paged catch-up beyond batches; very large histories send many envelopes.
- Delivery receipts between a person and contacts are still not implemented.
