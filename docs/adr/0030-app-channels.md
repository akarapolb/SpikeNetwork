# ADR 0030: App channels (data for other applications over Spike)

Status: **Proposed. For your review; not built.**
Date: 2026-10-25

## Problem

Spike carries chat: every envelope kind (`core/src/envelope.rs`) is a message, an edit, a group control or a call
signal. Other applications want Spike's identity, end-to-end encryption, groups, relay and direct connections for
their own data. The first is Center Work (an office suite), which wants people to edit a spreadsheet, document or
deck together: a shared file is a group, and every change is a small update that all members' devices must
receive and merge.

Today such an application would have to hide its data in `Text` messages. That shows its bytes as chat, stores
them in the conversation history, and gives it no way to send something short-lived such as "Alice's cursor is
now at B12" without storing it.

Three more gaps show up as soon as two people edit at once:

1. **Speed.** ADR 0020 measured about 0.85 s from a send to the screen, because the listener drops its connection
   and runs a full sync for each delivery. For chat that is fine; for typing together it feels slow.
2. **Late joiners.** A new group member gets the last 7 days of messages at most (ADR 0024, section 7). A shared
   file needs its whole state, not a week of changes.
3. **Volume.** Typing produces many small changes. One envelope per keystroke, encrypted for every device of
   every member, would be wasteful.

## Decision (proposed)

Spike stays an application-neutral network. It learns nothing about spreadsheets or documents; it only carries
bytes for an application and tells it who sent them.

### 1. A new envelope kind: `App`

| Field | Meaning |
|---|---|
| `kind` | `app` |
| `app` | the application's id, reverse-DNS style (`work.center`), at most 64 characters |
| `channel` | the application's own name for the stream (for example a file's id), at most 64 characters |
| `mode` | `stored` or `live` (below) |
| `body` | the application's bytes, base64, within the existing size limits |
| `group` / `to` | as for messages: a group (ADR 0024) or one person |

`app` and `channel` travel **inside** the encrypted envelope, so the relay never sees them.

- **`stored`**: delivered like a message. Queued at the relay for offline devices, acknowledged only after the
  application has taken it, and copied to the sender's own other devices. **Not** written to the conversation
  history and never shown as chat.
- **`live`**: delivered only to devices that are connected right now, over live connections or direct ones
  (ADR 0027). Never queued, never stored, dropped silently if nobody is there. The same handling as `Call`
  signals today.

The receiving app gets a new outcome, `Outcome::App { app, channel, mode, from_owner, from_device, group, body }`.
An application registers which `app` ids it accepts. An envelope for an app id nobody registered is acknowledged
and dropped, so it does not block the queue.

**Who may send:** the same rules as messages. In a group, only current members (by the group state the receiver
holds). One to one, only contacts; message requests (ADR 0021) do not admit app data from strangers.

### 2. Fast path on the live connection

App envelopes are processed **on the listener's open connection** instead of waking a full sync:

- The listener already holds the session lock while it connects (ADR 0020). For an envelope whose relay header
  marks it as `app` traffic, it decrypts, hands it to the application and acknowledges it, without dropping the
  connection.
- The relay header gets one bit, `fast`, set by the sender. It tells the listener which path to take. The relay
  sees that a delivery is fast-path traffic, which is a small, stated metadata cost (threat model to be updated).
- Target: under 200 ms from send to the application on a typical connection. To be measured like ADR 0020.

### 3. Batching

Spike does not batch for the application. The library documents the expectation: an application sends at most
one `stored` envelope per channel every 200 ms and coalesces its changes. The relay's existing rate limits
(ADR 0022) apply as they are.

### 4. Full state for a late joiner

A new kind pair, mirroring `request-file` (ADR 0013) but between group members:

- `AppStateRequest { app, channel }`: a new member asks any current member for the channel's state.
- The member's application decides whether to answer. If it does, it produces a snapshot; Spike sends it as an
  encrypted attachment (ADR 0011, up to 100 MiB) with an `AppState { app, channel, attachment }` envelope.
- The requester's application applies the snapshot, then the `stored` updates that arrive after it. Applications
  must make updates safe to apply twice; a CRDT does this naturally.

### 5. Library API (the `app/` crate)

```rust
session.app_register("work.center");
session.app_send(target, "work.center", channel, AppMode::Stored, bytes).await?;
session.app_request_state(group, "work.center", channel).await?;
session.app_send_state(target, "work.center", channel, snapshot_bytes).await?;
// Outcomes: Outcome::App { .. }, Outcome::AppStateRequest { .. }, Outcome::AppState { .. }
```

## What it costs

- Core: one envelope kind with two modes, two state kinds and their checks. No new cryptography.
- Relay: the `fast` header bit. No new tables.
- Client: the fast path in the listener (the delicate part: it must keep ADR 0020's rule that only one
  operation touches the ratchet at a time), app registration, and the new outcomes.

## Tested (planned)

- Core: `App` envelopes round-trip; a `live` one is never written to history or queued; an unregistered app id is
  dropped and acknowledged; app data from a non-member or a stranger is refused.
- End to end, with the real relay: two devices exchange `stored` updates while one is offline and catch up; a
  `live` envelope to an offline device is lost and nothing else breaks; a late joiner gets a snapshot and then the
  newer updates exactly once.
- Timing: send-to-application latency on the fast path against ADR 0020's 0.85 s.

## Limits and honest notes

- **Metadata.** The relay can see how often a group talks and, with the `fast` bit, that it is application
  traffic. It cannot see which application or what the bytes are.
- **Ordering.** Spike does not order app traffic. Applications must merge in any order (a CRDT does).
- **Large groups.** MLS groups (ADR 0025) have an ordered mailbox that suits this well, but MLS is experimental
  and unreviewed. The first version supports small groups and one-to-one only.
- **No browser client.** An application in a browser cannot use this until Spike's client builds for
  WebAssembly.
- Unreviewed, like the rest of Spike.
