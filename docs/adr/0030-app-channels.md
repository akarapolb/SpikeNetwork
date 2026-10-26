# ADR 0030: App channels (data for other applications over Spike)

Status: **built in part, 2026-10-26**: the `App` kind with stored and live data, full state for late joiners,
the library API, CLI commands, and a local API on the desktop with an approval prompt. One to one and small
groups (ADR 0024), and (2026-10-26, later) a fast path in the desktop listener and in the phone's. **Not
built:** the relay-side `fast` flag and live data that the relay never queues, large groups, a local API on
Android. Not reviewed.
Proposed: 2026-10-25.

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

## Decision

Spike stays an application-neutral network. It learns nothing about spreadsheets or documents; it only carries
bytes for an application and tells it who sent them.

### 1. A new envelope kind: `App` (built)

The body of a `Kind::App` envelope is JSON, `spike_core::app::AppData`:

| Field | Meaning |
|---|---|
| `app` | the application's id, reverse-DNS style (`work.center`): lowercase letters, digits, `-`, at least one dot, at most 64 bytes |
| `channel` | the application's own name for the stream (a file's id), 1 to 64 bytes, no control characters |
| `mode` | `stored` or `live` |
| `data` | the application's bytes, base64, at most 32 KiB before encoding |

`group`/`gstate` are set as for a group message, or `to` as for a one-to-one message. Everything above is
inside the encrypted envelope, so the relay never sees which application or channel a delivery is for.
(The proposal listed `app`, `channel` and `mode` as envelope fields; putting them in the body keeps the
envelope's shape and changes nothing about what the relay sees.) `Envelope::from_bytes` refuses an `App`
envelope whose body does not parse, whose names are invalid, or that carries a target, an attachment or an
introduction.

- **`stored`**: delivered like a message: queued at the relay for offline devices and copied to the sender's
  own other devices. On arrival it goes into a sealed inbox on that device (`app-inbox.json`, in the vault),
  never into the conversation history. The application takes events with a position (`seq`): asking for
  events after `n` returns the newer ones and forgets those at or before `n`, so passing the last one handled
  is the acknowledgement. A redelivered envelope is recognised by its id and handed over once. The inbox
  keeps at most 5,000 events; the oldest go first.
- **`live`**: never written to disk on the receiving side. It is handed to the application from memory, at
  most 200 per app for up to 30 s, and an envelope that arrives more than 30 s after it was sent
  (`LIVE_MAX_AGE_MS`, by the sender's clock) is dropped. A live send skips the relay housekeeping that a
  stored send does first, as calls do.

  *Different from the proposal:* the relay still queues a live envelope for a device that is offline, like
  any delivery; the receiver then drops it as late. A relay mode that never queues needs a header flag and
  was not built (see section 2).

**Registration.** An application is registered on a device by its id and a display name
(`spike_cli::apps::register`). An envelope for an id this device has not registered is acknowledged and
dropped, so it never blocks the queue, and counted: Settings lists the last 20 such app ids and how many
envelopes each sent ("Dropped: data arrived for apps that are not set up here: work.center (3)").
Removing an app forgets what waited for it and revokes a program's token (below). Sending also needs the
app registered on the sending device.

**Who may send:** the same rules as messages. In a group, only current members, by the group state the
receiver holds (an envelope naming an older member list is refused, as for text). One to one, only contacts.
App data from a stranger is dropped and never becomes a message request (only a text with an introduction
does, ADR 0021). Large (MLS) groups skip app envelopes quietly.

The receiving code gets `Outcome::App(AppEvent)`; the event says what it is (`data`, `state_request` or
`state`), the app, the channel, the mode, who sent it (name as this device knows them, owner key, device,
whether it is one of your own devices), the group if any, the envelope id and the sender's time.

### 2. Fast path on the live connection (built for the desktop listener, without the relay flag)

The proposal: process app envelopes on the listener's open connection instead of waking a full sync. Built as
follows. When the desktop listener receives a one-to-one delivery it no longer drops its connection and runs a
sync: it takes the session lock, opens and stores the delivery, acknowledges it **on the same connection**,
does what is owed because of it (`ops::handle_delivery`, `Session::listener_receive`), tells the screen, and
goes on listening. Any failure (another operation displaced the connection, an odd delivery) falls back to the
old path: the delivery was not acknowledged, so the sync is offered it again. The rule of ADR 0020 holds: only
one operation touches the ratchet at a time, because the fast path runs under the same lock as sends and syncs.

This applies to every one-to-one delivery, not only app data (the listener cannot tell before opening it),
so chat gets the same speedup. Group mailbox entries (large groups) still take the sync.

*Different from the proposal:* no `fast` header bit. The listener does not need to know before decrypting, and
the relay therefore learns nothing new (the proposal's metadata cost is not paid).

Measured in the real desktop app (`desktop/drive_apps.mjs`, three runs, local relay, one machine): a live update
sent by a program through Bob's desktop reached a program waiting on Alice's desktop in **77, 77 and 113 ms**
(from the send request to the answer of the waiting request), against about 470 ms before. That meets the
200 ms target. Not measured: a relay across the internet, a busy machine, the phone (it uses the same path, `listen_once`, but only the
Android app-channel driver was run).

### 3. Batching (as proposed)

Spike does not batch for the application. An application should send at most one `stored` envelope per
channel every 200 ms and coalesce its changes. The relay's rate limits (ADR 0022) apply as they are. Not
enforced by Spike.

### 4. Full state for a late joiner (built)

Two more kinds, between people who share a conversation (a small group, or two contacts):

- `AppStateRequest` (body: `{app, channel}`): "send me this channel's whole state". Sent to the group (or the
  contact). Your own other devices get a copy and ignore it.
- `AppState` (body: `{app, channel}`, attachment: the snapshot): the answer, uploaded as an encrypted file
  (ADR 0011, up to 100 MiB) and sent to the one member who asked (`groups::send_app` with a member, which
  encrypts for that member's devices only) or to the contact.

The requesting device remembers what it asked (app, channel, conversation) for 10 minutes. A state is only
accepted if it matches such a request from that conversation, from someone else, and the first one accepted
closes the request: a second answer, or state nobody asked for, is refused ("sent app state this device did
not ask for"). The receiving application gets a `state` event and reads the snapshot once
(`apps::snapshot`); a newer snapshot of the same channel replaces one not read yet. The file's key stays in
the sealed inbox and is never handed to the application. Applications apply the snapshot, then the stored
updates that follow, and must make updates safe to apply twice (a CRDT does).

### 5. Library API (built)

In the engine (`cli/src/apps.rs`), used by the CLI and the app crate:

```rust
apps::register(home, "work.center", "Center Work")?;
apps::send(home, relay, &To::Group(id), "work.center", channel, Mode::Stored, bytes).await?;
apps::request_state(home, relay, &To::Group(id), "work.center", channel).await?;
apps::send_state(home, relay, &event.reply_to(home)?, "work.center", channel, snapshot).await?;
apps::take(home, "work.center", after, max)?;      // events after a position; acknowledges up to it
apps::snapshot(home, relay, "work.center", seq).await?;
```

`To` is a contact by name, a small group by id or name, or one member of a group (to answer a request).
The app crate (`app/src/apps.rs`) wraps these on `Session` (`app_register`, `app_send`, `app_request_state`,
`app_send_state`, `app_snapshot`, `app_events(app, after, wait)`), adds live events from memory, and lets
`app_events` wait until something arrives (it wakes as soon as a sync or a direct connection delivers one).
The Android bridge has `apps`, `app_register` and `app_remove`.

CLI: `spike app register | list | remove | send | events | request-state | send-state | snapshot`.

### 6. Local API on the desktop (built; added to the proposal)

A program on the same computer uses app channels as the person, through the running desktop app, after the
person allows it once. Off until turned on in Settings → Apps ("Let programs on this computer connect"); it
runs only while Spike is unlocked.

- Plain HTTP on `127.0.0.1:8797` (`SPIKE_LOCAL_API_PORT` moves it), one request per connection, JSON.
- `POST /v1/connect {"app", "name"}` asks; the window shows a banner, "Center Work asks to use Spike as you",
  saying what it could do (send and receive its own data with your contacts and groups, see their names) and
  what not (read chats or files), with Allow and Don't allow. The program polls `GET /v1/connect/<id>`; once
  allowed it receives a random 256-bit token, once. Only the token's SHA-256 is kept, sealed with the app's
  registration. At most 4 requests wait at a time; one not answered is forgotten after 10 minutes.
- With `Authorization: Bearer <token>`, for that app only: `GET /v1/targets` (contact names and small groups),
  `POST /v1/send {to, channel, mode, data}`, `GET /v1/events?after=n&wait=s` (waits up to 25 s),
  `POST /v1/state/request {to, channel}`, `POST /v1/state/send {to, channel, data}` (up to 16 MiB through this
  API), `GET /v1/state/<seq>`. `to` is `{"contact": name}`, `{"group": id}`, or `{"group": id, "member": owner}`.
- Kept out: connections not from loopback; a `Host` other than `127.0.0.1:<port>` or `localhost:<port>` (DNS
  rebinding); any request with `Origin` or `Sec-Fetch-Site`, which browsers add to what a page sends (a page
  could not read the answers anyway, as no CORS headers are sent). Ordinary programs (Node's `fetch`, curl,
  Python) send neither.
- Removing the app in Settings revokes its token at once.

What this does **not** protect against: another program running as the same user on the same computer can
ask too, and can read the token from the program that holds it. The person's "Allow" is the gate, so the
banner tells them to allow a program only if they just started it.

## What it cost

- Core: three kinds (`App`, `AppStateRequest`, `AppState`) and their checks (`core/src/app.rs`). No new
  cryptography, no relay change.
- Engine: `cli/src/apps.rs` (registry, inbox, senders), hooks in `process` (contacts, own devices) and in the
  small-group receive path, `groups::send_app`, and a skip in large groups.
- App crate: `app/src/apps.rs` (DTOs, live events, waiting), `app/src/local_api.rs` (the HTTP API, on
  `httparse` and tokio, about 450 lines).
- Desktop: Settings → Apps (turn the local API on, list apps with what they sent and received, remove, the
  dropped list) and the approval banner. Android: Settings → Apps (list, remove, dropped list).

## Tested

- Core (`core/src/app.rs`, `core/src/envelope.rs`): app ids and channels, size limits, base64; envelopes
  round-trip and are refused with a bad body, a target, an introduction, a missing or extra attachment, or a
  group without its member list.
- Engine, real relay (`cli/tests/apps.rs`, 7 tests; the last handles a delivery on an open connection, acknowledges it, and the connection stays usable): stored data reaches both of Bob's devices after they were
  offline, each exactly once, and Alice's other device as her own copy, never in the history; data for an app
  not set up is dropped, acknowledged and counted, and removing an app forgets what waited; live data is never
  kept and a late live envelope is dropped; a stranger's app data is dropped and is not a message request; a
  late joiner asks the group, one member answers with a 200 KB snapshot, the joiner reads it once, a second
  answer and unasked state are refused, and newer updates follow; a removed member's app data naming the old
  member list is refused.
- Local API, real HTTP and relay (`app/tests/local_api.rs`, 3 tests): off until turned on; ask, wait, allow,
  token handed over once; send to a contact whose app linked the library; a long wait wakes on a live update;
  removing the app revokes the token; a snapshot through the API; `Origin`, `Sec-Fetch-Site`, a wrong `Host`,
  no token and a wrong token refused; a refused program gets nothing.
- Desktop, real windows (`node desktop/drive_apps.mjs`): two windows, two "programs" (the script); the banner
  and Allow; Settings lists the app as connected and counts what it sent and received; data between the
  programs never shows as chat; a live update wakes the other program (under 120 ms); remove revokes; data
  that still arrives is shown as dropped; turning the API off closes the port.
- Android, emulator (`ANDROID_SERIAL=emulator-5554 node android/drive_apps.mjs`): data for an app not set up
  is shown as dropped; once set up, Settings counts what arrived and nothing shows in the chats; Remove asks
  first and takes the app off.

## Limits and honest notes

- **Metadata.** The relay can see how often a conversation's devices receive deliveries, as for chat; it
  cannot tell app data from messages, nor which application or channel.
- **Ordering.** Spike does not order app traffic. Applications must merge in any order (a CRDT does).
- **Speed.** Under 120 ms program to program on a local relay with the desktop fast path (above); the phone's
  listener uses the same path (bridge command `listen_once`: a one-to-one delivery is stored and acknowledged
  on its open connection; anything else, or a failure, falls back to `sync`). Its speed is not measured.
- **Live data is queued by the relay** for offline devices (then dropped by them as late): it costs relay
  storage it need not.
- **Large groups** (ADR 0025) are not supported. **Android** has no local API: apps on the phone would link the
  library in-process; nothing does yet.
- **One device's inbox.** Each device keeps its own inbox; a program on the desktop does not see what an app
  on the phone already took.
- **No browser client.** An application in a browser cannot use this until Spike's client builds for
  WebAssembly, and the local API refuses browsers on purpose.
- Unreviewed, like the rest of Spike.
