# ADR 0040: An outbox for one-to-one messages

Status: **Proposed** (implemented in the same change)
Date: 2026-11-15

## Problem
A one-to-one message is encrypted separately for each of the recipient's devices and for the sender's own other
devices, and each copy goes to the relay that device reads from (ADR 0009). People on different relays can talk. But
when a copy's relay could not be reached, the copy was simply lost: `ops::send` reported the error per device and
nothing sent it again. If none of the recipient's devices could be reached, the whole send failed and the message was
not even kept. Only group replies had an outbox (`group-outbox.enc`, ADR 0024).

The desktop also said "Saved, but no device could be reached yet. It will be delivered when they connect." in a case
where nothing would ever deliver it.

## Options
1. **Keep the sealed blob and resend it as is.** No new encryption, and no gap in the ratchet. But a blob sealed while
   a relay was down for days may belong to a session or receiving chain the recipient has since dropped (Olm keeps a
   few sessions per peer and a few receiving chains, ADR 0002), so it can arrive and fail to open.
2. **Keep the envelope's plaintext and encrypt it again at retry time (chosen).** The copy is sealed with whatever
   session is current when it finally goes. The first attempt may have advanced the sending chain without the
   message arriving; the recipient skips that key, which Olm allows (message gaps of up to 2000 per chain). The
   plaintext is no more exposed than the message history, which holds the same text: the outbox file is sealed
   with the state directory's key like every other home file (ADR 0003).
3. **Resend from the message history.** Works for text, but not for edits, deletions and reactions, which are not
   messages of their own, and it would need the exact envelope (id, Lamport time, introduction) rebuilt.

## Decision
- `outbox.enc` (sealed, `cli/src/outbox.rs`) holds one entry per undelivered copy: message id, conversation, the
  owner key and device id it is for, whether it is one of our own devices, whether it *counts* (a recipient's device;
  for a note to yourself, your own), the envelope plaintext, when it was queued, attempts, next try and last error.
- What is kept: text messages and files, edits, deletions and reactions (`Kind::Text | Edit | Delete | React`) to a
  contact, their sent copies to our own devices, and notes to your own devices. Not calls, presence, live app data,
  sync or group control: those are worthless late or have their own retry.
- A copy is kept when the relay could not be reached (connection failed or broke: `outbox::Unreachable`) or turned it
  away for a reason that passes (`rate_limited`, `queue_full`, `mailbox_full`, `quota_exceeded`, `internal`, or the
  device has no prekeys there yet). Anything else (too large, malformed) fails as before.
- A send whose copies all wait no longer fails: the message is stored and shown, and `SendReport.waiting()` says no
  recipient device has it yet. Housekeeping on our own relay (prekeys, device lists) becomes a note when that relay is
  down, so a message can be written with no network at all.
- **Retry.** After every sync (`ops::recv`: after the queue is read and device lists are current, and also when our
  own relay cannot be reached, since a contact's may be), when listening starts, before the next message to the same
  person (that person's copies first, whatever their backoff, so older messages go first), and in the desktop app as
  soon as the listener's connection comes back after being offline. The phone syncs after each listen cycle, so it
  retries at least every 20 s while open. A copy whose retry fails waits 15 s, then doubling, at most 5 minutes; a
  relay found out of reach is not tried again in the same pass.
- **Where it goes.** The device and its relay are looked up again at retry time from the current signed lists, so a
  contact who moved relays is reached on the new one. A copy for a device that left its owner's list, or a contact
  who was removed, is dropped.
- **Duplicates.** A copy is removed once a relay accepts it. If the acceptance was lost on the way back, the copy is
  sent again; receivers keep a message id once and apply edits, deletions and reactions by Lamport time, so a second
  copy changes nothing.
- **Expiry.** After 30 days a copy is given up on. A message none of whose counted copies reached anyone is recorded as
  given up (`gave_up`, the last 1000 ids, with the time), and the apps show it as "Not sent" in red.
- **State shown.** `MessageDto.waiting` (yours, and no counted copy accepted yet) and `MessageDto.not_sent`;
  `ContactDto.unsent` counts a conversation's waiting messages. A message one of whose recipient's devices has it is
  not "waiting", even while copies for their other devices still are. Both apps show "Waiting to send" in amber on
  the message (colour means status: amber is waiting, red is danger) and in the chat list.
- **Notices.** `send` and `send_file` now return `SentDto { delivered, waiting, relay_unreachable }`. The desktop says
  "will be delivered when their device connects" only when a relay accepted the copy or the device simply has not
  connected yet, and "their relay can't be reached right now. It will be sent automatically when it can." when the
  relay itself could not be reached.

## What this does not do
- No outbox for files' uploads: a file whose upload to our own relay fails still fails to send.
- Group messages keep their own path (small groups fan out per member, ADR 0024; large groups post to a mailbox,
  ADR 0025). Their sends report per-device errors as before.
- The outbox is per device. If this device is lost before a waiting copy goes, the copy is lost; the sent copy to our
  other devices (also waiting, if their relay was the one down) is what keeps it in our history elsewhere.
- No manual "retry now" or "delete" for a waiting message yet.

## Tested
- `cli/tests/outbox.rs`: the relay is down at send and comes back, and the message (and the sent copy to the sender's
  other device) arrives exactly once; the very first message to someone, whose session cannot start without the relay,
  goes on the next ordinary sync; a contact on another relay that is down gets it in order before the next message;
  a copy is given up on after the expiry and the message is recorded as not sent; a note to your own devices waits
  too.
- `desktop/drive_outbox.mjs` and `android/drive_outbox.mjs`: a message sent while the recipient's relay is stopped
  shows "Waiting to send" in amber, and the mark goes once the relay is back and the message has arrived.
