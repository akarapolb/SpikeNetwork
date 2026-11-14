# ADR 0035: Read receipts, online now and typing

Status: **Proposed**
Date: 2026-11-14

## Context
The roadmap lists "presence; read receipts opt-in" for Connect Chat, and the UI prototype has a "What others
see" screen with three switches: **Read receipts**, **Online now** and **Typing**. All three tell a contact
something about *when* you use the app. That is metadata, and this project exists to leak as little of it
as possible, so each must be the owner's choice and they start off.

## Decision
Three new envelope kinds, sent one to one (never to groups in this version) inside the normal end-to-end
encrypted per-device sessions (ADR 0002, 0009), like any message:

| Kind | Body | Sent when | Sent to |
|---|---|---|---|
| `receipt` | `{"upto": ms}`: the newest of their messages you have read (their clock) | you open the conversation and it had unread messages from them | all of that contact's devices |
| `presence` | `{"online": true}` | you open the conversation, then at most once every 60 s while it stays on screen | all of that contact's devices, **only if you verified them** |
| `typing` | `{"typing": true\|false}` | you start typing (at most once every 5 s), and `false` when you send or clear the box | all of that contact's devices |

- **Defaults: all three off**, on every new and existing device.
- **Reciprocal.** You see a contact's receipts, online state or typing only while you share the same thing
  yourself. When your switch is off, the app drops what arrives without storing it. This rule is enforced by your own app, not by the
  contact's: a modified app could still send them, and a modified app on the other side could keep yours.
- **Nothing is broadcast.** Presence goes only to the person whose conversation you have open, not to every contact.
  The relay has no online list to collect, and an unverified contact (or someone who only sent a message request,
  ADR 0021) never learns when you are around.
- **Never acknowledged automatically.** A receipt follows a human opening the conversation, never delivery. Message
  requests (ADR 0021) never get receipts.
- **Own devices.** The switches are per device in this version. Your other devices receive copies of receipts you
  send (like any message) so they also show "Read" on the contact's messages. Incoming receipts and presence are
  not forwarded between your devices.
- **Storage.** Only the latest value per contact is kept, in the sealed state (ADR 0003): their read point
  (ms), when they were last seen online, and typing until (now + 8 s). There is no history.
- **Display.** Under your newest message that they have read: "Read 14:02". In the header: "Online" if seen in
  the last 2 minutes, "typing…" while typing is fresh. Nothing appears when you have the switch off.

## What the relay sees
The contents never: the relay cannot tell a receipt, presence or typing envelope from a short text message.
It **does** see that an envelope went from one of your devices to the contact's devices at that moment, and
its size (envelopes are not padded). With receipts or presence on, someone running the relay can tell that you
had that conversation open at about that time, because a message follows the moment you open it. With all
three off nothing extra is sent. The prototype's line "A relay never learns any of these" is therefore
changed to: "They travel inside encrypted messages. A relay sees only that a message went."

## Not in this version
- Groups. Receipts in a group tell every member when each member reads, and fan out to every device.
- Padding envelopes to fixed sizes, which would hide receipts among texts by size.
- Syncing the switches between your own devices.
- "Last seen at" for contacts who are not open on screen.

## Tested
- Engine: each kind is accepted only from a contact's device, from own devices only as copies, and only while the
  matching switch is on; receipts never move a read point backwards; malformed bodies are refused.
- Phone driver: the switches are off on a new install; with both sides on, opening the conversation shows "Read"
  on the sender's phone; turning one side off stops it.
