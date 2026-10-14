# ADR 0009: Fan-out to every device and the relay device-list directory (multi-device milestone M3)

Status: **Proposed** (implemented)
Date: 2026-10-14

## Decision
- **Fan-out.** `ops::send` encrypts the same envelope separately for every device in the recipient's signed list and for each of the sender's *own* other devices (a "sent copy"), each over its own Olm session (sessions are per device pair). The message is stored once, after at least one of the recipient's devices has it. The report lists every device and whether its relay accepted it.
- **Sent copies.** Envelopes now carry `to` (the conversation's other person, owner key). A copy arriving from one of our *own* devices (authenticated by its session and by our own signed list) is filed as an outgoing message in that conversation. `to` is ignored on messages from other people.
- **Home relays.** A device entry may name the relay it reads from (`DeviceEntry.relay`). Each copy is delivered to that relay (default: the sender's relay), and prekeys for that device are claimed there. Connections are opened on demand per send.
- **Directory on the relay.** Clients publish their person's signed device list and fetch contacts' lists (`PublishDeviceList`, `FetchDeviceList`). The relay accepts a list only if its signature and contents verify, the **publishing device is in it**, and the version is higher than the stored one (same version: identical content only). Clients apply the ADR 0008 update rules again on everything they fetch, so a lying relay can withhold or delay lists but not forge, roll back or fork them.
- **Learning new devices.** Before sending, the sender refreshes the recipient's list. On receive, a message from an unknown device triggers one refresh of contacts' lists and a retry, so a contact's new device is accepted automatically if, and only if, their owner key signed it. Verification of the person carries over.
- **Our own list.** On connect, a device fetches its own person's list; a newer valid list is adopted. If it no longer contains this device, the device is told it was removed and refuses to send.

## Tested (`cli/tests/multidevice.rs`)
Delivery to both recipient devices plus the sender's other device, with the same message id in every store; a reply from a device the recipient had never seen, accepted after refresh and still verified; a removed device receives nothing and cannot send; the relay refuses rollback, equivocation, forged signatures and publishing by non-members; a device on a different home relay receives there and only there; partial delivery reported with the reason and the message still stored.

## Limits
- No device linking yet (M4): tests create multi-device identities directly. History for new devices, QR pairing and the safety-code step come with M4.
- Edits, deletes, read state (M5); backup (M6).
- A relay can still **withhold** a newer list from a sender (the sender then misses a new device until it notices). Key transparency (engineering plan 7a) is the planned fix.
- Each send opens fresh connections; a long-running client would keep them.
- Fan-out cost grows with devices (bounded at 16 per person); groups still need MLS.
