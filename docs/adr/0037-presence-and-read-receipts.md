# ADR 0037: Read receipts, online now and typing

Status: **Accepted** (approved by the owner 2026-11-14)
Date: 2026-11-14

## Context
The roadmap lists "presence; read receipts opt-in" for Connect Chat, and the UI prototype has a "What others
see" screen with three switches: **Read receipts**, **Online now** and **Typing**. All three tell a contact
something about *when* you use the app. That is metadata, and this project exists to leak as little of it
as possible, so each must be the owner's choice and they start off.

## Decision
One new envelope kind, `status` (`spike_core::envelope::Status`), sent one to one (never to groups in this
version) inside the normal end-to-end encrypted per-device sessions (ADR 0002, 0009). Its body is one of:

| Body | Meaning | Sent when | Sent to |
|---|---|---|---|
| `{"read":{"upto":ms}}` | the newest of their messages you have read (their clock) | you open the conversation and it holds something newer than the last receipt you sent | all of that contact's devices |
| `"online"` | you have the conversation open | you open the conversation, then once every 60 s while it stays on screen | all of that contact's devices, **only if you verified them** |
| `{"typing":{"on":bool}}` | typing, or stopped | you start typing (at most once every 5 s), and `false` when you send | all of that contact's devices |

The envelope check refuses a status with a group, a target, an attachment, an introduction or a body that is
not one of these. A stranger's status (ADR 0021) is refused like any non-text envelope from a stranger.

- **Defaults: all three off**, on every new and existing device.
- **Reciprocal.** You see a contact's receipts, online state or typing only while you share the same thing
  yourself. When your switch is off, the app drops what arrives without storing it. This rule is enforced by your
  own app, not by the contact's: a modified app could still send them, and a modified app on the other side
  could keep yours.
- **Nothing is broadcast.** Presence goes only to the person whose conversation you have open, not to every
  contact. The relay has no online list to collect, and an unverified contact (or someone who only sent a
  message request, ADR 0021) never learns when you are around.
- **Never acknowledged automatically.** A receipt follows a human opening the conversation, never delivery.
  Message requests (ADR 0021) never get receipts.
- **Own devices.** The switches are per device in this version. Statuses go to the contact's devices only, never
  to your own (your read state already travels between them, ADR 0013); one arriving from your own device is
  ignored.
- **Storage.** Only the latest value per contact is kept, sealed (`presence.enc`, ADR 0003): their read point
  and when it came, when they were last seen online, typing until (arrival + 8 s), and the last read point you
  told them. There is no history. Turning a switch off forgets what was kept under it. Online and typing
  signals more than 60 s old when they arrive (a phone collecting its queue) are dropped. A message from them
  ends their typing.
- **Display.** Under your newest message that they have read: "Read 14:02" (when the receipt came). In the
  header: "typing…" while typing is fresh, otherwise "Online" if seen in the last 2 minutes. Nothing appears
  while your switch is off.

## What the relay sees
The contents never: the relay cannot tell a status from a short text message by what it carries. It **does**
see that an envelope went from one of your devices to the contact's devices at that moment, and its size
(envelopes are not padded). With receipts or presence on, someone running the relay can tell that you had that
conversation open at about that time, because a message follows the moment you open it. With all three off
nothing extra is sent. The prototype's line "A relay never learns any of these" is therefore changed to:
"They travel inside encrypted messages. A relay sees only that a message went."

## Not in this version
- Groups. Receipts in a group tell every member when each member reads, and fan out to every device.
- Padding envelopes to fixed sizes, which would hide statuses among texts by size.
- Syncing the switches between your own devices.
- "Last seen at" for contacts who are not open on screen.
- The desktop's own settings screen for the switches (the engine and desktop commands exist; the screen does not).

## Tested
- Engine (`cli/tests/presence.rs`): off by default; reciprocal; online only to a verified contact; a read point
  is told once and never moves back; typing goes stale and a message ends it; old signals dropped; turning a
  switch off forgets; your own other device receives nothing; group and malformed statuses refused.
- Phone driver (`android/drive_phone_extras.mjs`): the switches are off on a new install; with both sides on,
  the contact opening the conversation shows "Read" on the phone, and their typing and online show in the
  header; turning the phone's switch off hides them again.
