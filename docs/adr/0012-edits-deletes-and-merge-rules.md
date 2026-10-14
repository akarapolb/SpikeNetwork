# ADR 0012: Edits, deletes and merge rules (multi-device milestone M5, first slice)

Status: **Proposed** (implemented)
Date: 2026-10-14

## Decision
- **Envelope kinds** `edit` and `delete` (plus `text`), each naming a `target` message id. They travel exactly like messages: encrypted per device (ADR 0009), to every device of the recipient and to the sender's own other devices. Envelopes carry a `lamport` logical clock; every device keeps a persisted counter (`lamport.enc`), advanced on send and raised on receipt, so concurrent changes from different devices are ordered without trusting wall clocks.
- **Merge rules** (`core/src/merge.rs`, pure and unit-tested, identical on every device):
  - A message's stamp is `(lamport, sender device id)`; an edit applies only if its stamp is strictly greater than the stored one (device id breaks ties), so all devices converge to the same text whatever the arrival order. Replays of an edit change nothing.
  - **A delete is a tombstone and always wins** over any edit, in either order. The text and any attachment (stored bytes included) are removed; the row stays so a late or replayed copy cannot bring the message back.
  - **Only the author may change a message:** the change must come from the same person (any of their devices) and from the same conversation. A change from anyone else is refused and reported (`Outcome::Rejected`), never applied.
  - A **delete for an unknown message** leaves a tombstone, so it never appears if it arrives later; an **edit for an unknown message** is dropped (nothing is invented).
- **Linking** carries edited text and tombstones in the bundle (they are ordinary rows), so a new device never shows a deleted message.
- CLI: `spike edit <id> "<text>"`, `spike delete <id>`; `spike history` shows message ids (first 8 hex characters), `(edited)` and `[message deleted]`.

## Tested
Core: 10 unit tests for the merge decision table and for all arrival orders of concurrent edits converging. End to end (`cli/tests/sync.rs`): edit from the other device and delete reach all four devices; concurrent edits from two devices converge on all four; delete beats edit in both arrival orders; only the author can change a message (through the app and through crafted changes, including wrong conversation); stale edits and replays are no-ops; a delete arriving first leaves a tombstone; a new device linked later receives edits and tombstones and follows later changes; deleting also removes stored attachment files on both sides. The demo shows an edit and a delete on a contact's device and on the sender's own laptop.

## Limits and honest notes
- **Deletes are not guaranteed erasure.** A contact's device that is offline, modified, or malicious may keep the text; screenshots exist. A deleted message is removed from honest devices that receive the delete.
- **No edit history is kept**; the latest accepted text replaces the old one.
- **Sender device ids** are known only when the sender is in the signed device list we hold; a change from an unlisted device is not accepted (it fails to decrypt as a known session).
- **Not yet in M5:** read state sync, catch-up of a long-offline device from another own device (it still relies on the relay queue until it expires), and device-to-device fetch of expired attachments. These need the own-device sync channel (a `sync-control` envelope kind), the next slice.
