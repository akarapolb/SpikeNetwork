# ADR 0032: Group calls

Status: **proposed, revised 2026-11-13.** Nothing here is built. First written 2026-10-26; this revision reviews it
against the code and ADRs 0018, 0022, 0023, 0024, 0025, 0028, 0030 and 0033, `docs/hosted-relay-plan.md`,
`docs/business-plan.md` and `docs/turn.md`, closes the gaps that review found, and answers the open questions
(who runs the call server, how it is paid for, the size limits, Android, and what the owner must decide). It is for
the owner to approve or change before any code is written.

Claims about the code were checked on 2026-11-13 and say so. Claims about libraries and platforms that were not
tried are marked **[unverified]**; each has a test in the build order that settles it before anything depends on it.

## Problem

People in a group want to talk together. One-to-one calls (ADR 0028) work: Spike envelopes carry the setup, WebRTC
carries the media, DTLS-SRTP encrypts it between the two devices, and TURN on the relay's box forwards packets it
cannot read when the two cannot reach each other directly.

A group call breaks three of those assumptions:

- **Bandwidth.** If every device sends its stream to every other device (a *mesh*), each one uploads n-1 copies.
  That works for 3 or 4 people on voice, poorly for video, and not at all for 10.
- **Who can read the media.** The usual answer to bandwidth is a server that receives each stream once and forwards
  it to the others (an *SFU*). DTLS-SRTP then ends at the server: it could listen to every call. That is not
  acceptable here; nothing else in Spike lets the operator read content.
- **Who may see whose address.** A direct call shows each side's IP address to the other (ADR 0028 accepts this
  between *contacts*). A group has members who are not contacts (ADR 0024 section 5) and large groups have up to
  500. A mesh would show everyone's address to everyone.

## What the review of the first draft found

| # | Gap in the first draft | Fix in this revision |
|---|---|---|
| 1 | ADR 0028 accepts call signals **only from contacts and own devices**, and drops anything else. Group members need not be contacts (ADR 0024 section 5), so a mesh invite from a non-contact would be dropped. | A new envelope kind `GroupCall`, accepted under the *group membership* rules of ADR 0024 section 4 (a member of our current state, sent by a device of that member). `Kind::Call` and its one-to-one state machine are not changed (section 3). |
| 2 | A mesh shows every member's IP address to every other member, including non-contacts. In a large group the pairwise offers would have to be broadcast inside the group, so all 500 members would see them. | Two rules: a pair connects directly only if both are contacts, otherwise through TURN with relay-only candidates; **large groups never use a mesh**, only the SFU, where only the server sees addresses (section 1). |
| 3 | The SFU's room admission used the call key itself as the only credential and said nothing about who may *create* a room. On a shared relay that is a free bandwidth proxy. | A room is created only with a **room grant** issued by a relay to an enrolled device, so the SFU has an accountable owner (section 6). This makes the SFU depend on ADR 0023 (tenants), which is **proposed and not built** (no tenant code exists in `relay/src`, checked 2026-11-13). |
| 4 | Signalling for a call is described once; nothing says how a **person with several devices** takes part. | One device per person is in the call; the person's other devices see "in this call on another device" (section 3). |
| 5 | In a large group, signals ride the MLS mailbox, which **keeps entries for 30 days** (ADR 0025). An old `group_call_start` would look like a live call. | Every group-call signal carries the sender's signed timestamp and call id; the 60 s staleness and 24 h call-id memory of ADR 0028 apply (section 3). |
| 6 | Key rotation by "the device that has been in the call longest" has no tie-break, and the room token and the SFrame keys came from the same key with unrelated labels. | Deterministic rotation with a generation number, and labelled HKDF derivations for each use (section 4). |
| 7 | The SFU must switch simulcast layers, which normally needs to see keyframes. With end-to-end encryption it cannot read the payload. | Layer switching must work from RTP headers and RTCP only; this is a go/no-go test for `str0m` before the server is built (section 2). **[unverified]** |
| 8 | "Moving the call to the relay's call server" assumed one exists. A self-hosted or community relay may have none. | The call says "full" with a reason, and the SFU is a property of the relay's offer (section 1). |
| 9 | The 8-person ring limit and "3 unanswered invites per hour" of ADR 0028 do not say what happens in a group. | A ring policy for groups (section 3). |
| 10 | No answer to who pays, who runs the SFU, or how a person on another relay joins. | Sections 6 and 7. |
| 11 | Android: the plan said "tried before anything depends on it" without a test or a fallback. | A go/no-go spike with pass criteria and a fallback ladder, and a rule that no client ever sends media to an SFU without frame encryption (section 5). |

## Decision

### 1. Two shapes, chosen by size and by who is in the call

- **Mesh, up to 4 people, only among contacts.** Each device opens one WebRTC connection per other device, as in a
  one-to-one call. No new server, no new cryptography: each pair's DTLS-SRTP is already end to end. Voice first;
  video is allowed but capped at 360p in a mesh. Where either end of a pair is not the other's contact, that pair
  uses TURN with relay candidates only (ADR 0028 "Always use the relay"), so a non-contact never learns an
  address. The call screen says which pairs are direct and which go through the relay.
- **SFU, 5 or more people, or any call in a large (MLS) group, or when the starter prefers it.** Each device sends
  its streams once to the SFU, which forwards them. Media is encrypted *before* it leaves the device with
  **SFrame** (RFC 9605), so the SFU forwards frames it cannot read. DTLS-SRTP still runs between device and SFU as
  the transport layer. Members' addresses are visible only to the SFU, not to each other.
- A mesh call that grows past 4 moves to the SFU (everyone reconnects once; the screen says "Moving the call to the
  relay's call server…"). A call never moves back. The **call host** (the device that started it; if it leaves, the
  device that has been in the call longest, ties broken by the lowest device id) decides the move and owns the
  room (section 6).
- **If the relay offers no SFU** (section 6), the fifth person is refused with "This call is full: your relay has no
  call server", and the group sees the same line. Small calls are unaffected.
- **Large groups never use a mesh.** A mesh needs pairwise setup messages; inside a large group those would be
  readable by every member, revealing addresses to up to 500 people.

### 2. The SFU: our own, built on `str0m`, next to the relay

- A small forwarding server in Rust, `spike-sfu`, built on **str0m** (MIT/Apache-2.0, a sans-I/O WebRTC library
  that is used for SFUs **[unverified: licence, maintenance state and feature set were not re-checked for this
  revision]**). It runs on the same box as the relay and coturn, like TURN in ADR 0028; the deployment guide gains
  one service.
- It only forwards: no mixing, no recording, no transcoding. It picks which video layers to forward (simulcast:
  each sender sends up to 3 sizes; the SFU sends each receiver the size that fits), and forwards audio from at
  most the 3 loudest speakers when a call is large (audio level is a header extension, readable without the media
  key).
- **Constraint that decides whether `str0m` is usable (gap 7):** layer switching and keyframe requests must work
  from RTP header extensions and RTCP feedback alone, because the payload is encrypted. If a receiver needs a
  keyframe to switch layers, the SFU asks the sender with a PLI/FIR. **[unverified]** Whether `str0m` plus
  Chromium and the Android WebRTC library do this with SFrame-encrypted frames is the first test of phase D1.
- **Speaking pattern leak.** The audio-level extension tells the server who is speaking. A setting "hide who is
  speaking from the server" turns the extension off; the SFU then forwards all audio streams up to a cap of 6
  senders per receiver and cannot pick loudest speakers. Default: off (extension on), because it gives better
  large calls; the cost is stated where the switch is.
- **ICE and transport.** The SFU has a public address and a UDP port range. Devices behind networks that block UDP
  need a fallback: TCP or TLS to the SFU, or TURN over TLS (`turns:` on 443, `docs/turn.md`) to reach it.
  Whether `str0m` supports ICE over TCP is **[unverified]**; if it does not, `turns:` through coturn is the
  fallback and the guide says so.
- **Why not an existing SFU.** LiveKit, Jitsi Videobridge, Janus and mediasoup are mature, but each brings its own
  signalling protocol and client SDK (or, for mediasoup, a Node application server). Our clients use plain WebRTC
  with Spike's own signalling, and the media keys must stay out of any server's hands. A small SFU that speaks our
  signalling is less code to trust than adapting a large one. **Gate:** phase D1 builds a forwarding-only
  prototype and tests it against the criteria above. If it fails them, the fallback is LiveKit (Apache-2.0) with
  its end-to-end encryption mode, accepting its signalling and its frame format for the clients that use it.

### 3. Signalling: group envelopes, not a new channel

New call signals use a **new envelope kind `GroupCall`** (body: a `group_call::Signal` as JSON, like `Kind::Call`),
not new variants of `Kind::Call`. `Kind::Call` stays what ADR 0028 built: contacts and own devices only, one
state machine. `GroupCall` is accepted under ADR 0024 section 4 (we are a member of the group named by `group`; the
sending device belongs to a member of our current state) and, in a large group, only as a message inside the group's
own mailbox (ADR 0025). It is never stored as a conversation message. The relay sees opaque blobs, as for chat.

Signals (every one carries the group id, the call id, and the sender's signed timestamp):

- `start` (media kinds, shape, host device): "Ann started a call". For the SFU shape it also carries the SFU's
  address and the room id.
- `join` / `leave`: who is in the call, so every device shows the same list. Sent to every member in a small group
  and also to the person's **own other devices**, which then show "in this call on another device" and do not join a
  second time (the same idea as `answered_elsewhere`). One device per person is in a call.
- `key` / `key_ack`: the call key and its generation (section 4).
- `end`: sent by the last person to leave, or by any member after the call has been empty for 60 s.
- In a mesh, the pairwise `call_invite` / `call_answer` / `call_ice` / `call_end` of ADR 0028 are exchanged between
  the two devices, carried as `GroupCall` envelopes tagged with the group and call ids so the contact-only rule of
  `Kind::Call` does not apply. As in ADR 0028 the offer carries no candidates; the sender starts gathering when
  the other side answers.
- With the SFU, each device talks to the SFU over a WebSocket (its own SDP offer/answer and ICE), authenticated
  as in section 6. The SFU never sees Spike envelopes.

**Freshness.** A signal older than 60 s, or for a call id seen in the last 24 h as ended, is dropped and logged
(a missed call from its timestamp), as in ADR 0028. This matters most in large groups, where the mailbox keeps
entries for 30 days.

**Ringing policy.**
- Small groups ring (like a one-to-one call) up to **8 members**; each member's devices all ring, and the first to
  join silences the rest. Groups of 9 to 20 get a notification and a "Join" bar in the chat, without ringing.
  Large groups never ring.
- Ringing is limited per member and per group: at most 3 unanswered group invites per member per hour (as in
  ADR 0028), and one *ring* per group per 10 minutes; a call started inside that window shows the join bar.
- A member can mute a group's calls; the invite then shows the bar without ringing.

**State machine.** The call logic goes into `core` as a sans-I/O state machine (like `core/src/call.rs`), unit
tested without WebRTC and covered by a randomized simulation of joins, leaves, late and duplicated signals, as
`core/tests/call_sim.rs` does for one-to-one calls. The SFU's room roster is the truth for who is in an SFU call;
the `join`/`leave` signals are for the chat's bar and the call log.

### 4. Keys

One **call key** (32 bytes) per call and **generation**. Every use is derived with HKDF and its own label, so no key
is used for two purposes:

- `room_token = HKDF(call key, "spike-call-room" || call id)`
- `sframe_base = HKDF(call key, "spike-call-sframe" || call id || generation)`
- SFrame (RFC 9605) derives each sender's key from the base secret and a key id. The key id is
  `generation (16 bits) || the first 48 bits of SHA-256(sender device id)`. A device that finds two senders with the
  same key id refuses the join (collision probability is negligible). Frames carry the key id and a counter in the
  SFrame header; the counter rejects replays.

**Small groups (ADR 0024).** The call host makes a random call key at generation 1 and sends it, sealed, to each
device that joins. When someone leaves, or is removed from the group during the call, the **rotator** makes a new
key at generation+1 and sends it to everyone still in the call. The rotator is the device with the lowest device id
among those in the call and in the group's current state; if two devices rotate at once (a view that has not
caught up), the higher generation wins and, at equal generations, the lower sender id. Old generations are
accepted for 2 s after a new one is seen, then dropped. A removed member's device keeps the old key and learns
nothing after the rotation.

**Large groups (ADR 0025).** The key is derived from the MLS group with
`export_secret("spike-group-call", call id || epoch, 32)` (the exporter exists in `mls/src/group.rs`; checked
2026-11-13; this label is new). Each commit that removes someone moves the epoch, so the key changes with membership;
the generation is the epoch's low 16 bits. A device that has not processed the commit cannot derive the new key,
which is right: it hears silence until it has. Because ADR 0025 accepts application messages from up to three
epochs back, receivers also keep the previous two epochs' keys for the 2 s overlap.

**Room token and room grant.** See section 6.

### 5. Media encryption on each platform

- **Desktop (WebView2, Chromium):** encoded insertable streams (`RTCRtpSender.createEncodedStreams`, or
  `RTCRtpScriptTransform` where available) in a worker that runs SFrame. The key never leaves the webview's memory
  for the call's lifetime. Browser support for the same API in a future web client (ADR 0033, stage 2) is
  **[unverified]** and not required here.
- **Android (`org.webrtc`):** Spike's calls use the `io.getstream:stream-webrtc-android` AAR (ADR 0028). The Java
  API takes a frame encryptor on `RtpSender.setFrameEncryptor` and a decryptor on `RtpReceiver.setFrameDecryptor`
  **[unverified for this AAR]**; implementing one needs a native object compiled against the *same* libwebrtc
  revision and C++ runtime as the AAR, which the AAR does not ship headers for. **This is the riskiest part.**

**Rule that holds on every platform: a device never sends media to an SFU unless SFrame is on.** If frame
encryption is unavailable, the device refuses to join an SFU call with a clear message; it never falls back to
DTLS-only. (A mesh call does not need SFrame, since DTLS-SRTP ends at the other member's device.)

**Android go/no-go (phase C, a spike, before anything depends on it).**

*Pass* means all of: (1) on the emulator, a one-to-one call where Android encrypts and the desktop decrypts with a
fixed test key and the other way round, audio packets with energy both ways as `android/drive_call.mjs` checks, and
a wrong key gives silence, not noise; (2) the shim builds reproducibly from pinned inputs, with the AAR and its headers
pinned and checksum-verified like other dependencies; (3) it also works on a real phone (a Samsung phone is the
only maker tried so far); (4) the APK grows by an amount the owner accepts.

*If it fails, in this order:* (a) **an Android group-call screen in a WebView** (Chromium on the phone supports
insertable streams, the same worker and key code as the desktop; cost: a heavier screen, microphone and camera
permissions through the WebView, background service work again) **[unverified]**; (b) a different WebRTC build that
exposes a frame cryptor (for example LiveKit's fork, whose frame format would then have to be matched on the desktop)
**[unverified]**; (c) Android takes part in **mesh calls only** (up to 4, contacts only) and says plainly why it cannot
join bigger ones. A call never depends on Android being able to join.

### 6. Who runs the SFU, and how a client finds it and gets in

**Recommendation: the SFU is a service of a relay, run by whoever runs the relay.** It uses the same binary and
deployment guide, and nobody else's server is needed. There are three kinds of operator, all with the same code:

| Operator | SFU policy | Who pays |
|---|---|---|
| **Hosted by us** (ADR 0023 tenants, `hosted-relay-plan.md`) | On for tenants whose plan includes it, with per-tenant limits | The tenant (section 7) |
| **Self-hosted by an organisation** | On or off by flag; the operator's own limits | The organisation's own bandwidth |
| **Community or open-mode relay** | **Off by default.** An open-mode relay would be a free bandwidth proxy for anyone. It may be turned on in *enrolled* mode only, or with a flat per-device cap | The operator, voluntarily |

**Discovery.** A relay that has an SFU says so the way it advertises TURN (`--turn-url`): new flags `--sfu-url` and
`--sfu-secret`, and a client message `SfuCredentials` next to `TurnCredentials` (`core/src/wire.rs`; the TURN message
exists, checked 2026-11-13). The call host's *own home relay* provides the SFU for the call: it is the relay that
already knows the host's device, so the hosted-plan limits and the billing attach to it.

**Authentication, in two parts.**

1. **Creating a room (the host).** The host asks its relay for `SfuCredentials` for a new room id. The relay checks
   the device's enrollment and the tenant's limits (section 7) and returns a **room grant**: the room id, the
   participant and bandwidth caps, an expiry, and a MAC under a key the relay shares with its SFU (the coturn REST
   scheme used for TURN in `core/src/turn.rs`; in v1 the SFU is on the relay's box, so a shared secret is
   enough; if the SFU is ever on another box, switch to an Ed25519 signature the SFU verifies with the relay's public
   key). The host registers the room at the SFU with the grant and the `room_token` of section 4.
2. **Joining (everyone else, from any relay).** A member presents the `room_token` for that room and call. The
   SFU admits a device that holds the token for a *registered* room, up to the room's caps. A guest needs no account
   or enrollment at the host's relay, and the host's tenant pays for the call. Because the token is derived from the
   call key, a removed member's old token stops working when the key rotates: the remaining devices re-register the
   room under the new token and the SFU drops connections that hold the old one.

The SFU learns a random room id, a token, and the participants' IP addresses and times. It never learns the key, the
group, or any name. The token proves that a device held the call key when it joined; it is a bearer credential,
so a malicious SFU could replay it to impersonate a participant *to itself*, which gives it nothing it did not
already have.

### 7. Metering and money

Principles from `docs/business-plan.md` apply unchanged: **never charge for encryption; usage billing reflects
traffic, never content.** The SFU can measure, without any content: room count, participants over time, bytes
forwarded in each direction, audio versus video sender-minutes (by the media kind negotiated), and room duration.

**Recommendation.**
- **Free for everyone:** mesh calls of up to 4 people (and one-to-one calls). They need no SFU, only TURN when two
  devices cannot connect, which is already capped per call (`docs/turn.md`).
- **Paid or metered:** calls on the SFU (5 or more, any call in a large group, or a call that moves there by
  choice). Hosted plans (Business, Agent Network, and Personal Plus if you want it) include a monthly allowance of
  SFU **participant-minutes**, with **video minutes weighted higher than voice minutes**, because video forwarding
  costs far more bandwidth. Beyond the allowance: an overage rate per participant-minute, or per GB forwarded.
  Actual prices are for the owner and need real cost data (below).
- **Where the limits live:** new tenant limits next to those in ADR 0023: `sfu_rooms` (concurrent), `sfu_participants`
  (per room), `sfu_bps` (per room) and `sfu_minutes` (per month). The relay refuses a room grant with a named
  error (`tenant_over_limit: sfu_minutes`) as for other limits, and the room's caps ride in the grant, so the SFU
  can cut a room off without asking the relay.
- **Guests do not need a plan.** A free-tier person can join a paying member's call (section 6, part 2). This also
  makes the call a reason for a group to have one paying member, and no free-tier user can start an SFU call without
  a plan that includes it. The message for a free user starting a fifth-person call is "This call needs a plan with
  call server minutes", with a one-tap choice to continue as a mesh without video.
- **Self-hosters** pay nothing to Spike and carry their own bandwidth, as the business plan expects. Open source
  caps what a hosted SFU can charge; the plan's note (section 7) to model the self-host fraction applies here.

**Rough cost shape (an estimate to be replaced by measurement; do not price from it).** Voice at roughly 30 to 40
kbit/s per stream: a 10-person voice call with the loudest-3 rule is about 3 streams x 10 receivers, about 1.2
Mbit/s out of the SFU, about 0.5 GB an hour. Video at roughly 250 kbit/s per visible tile with 9 tiles: about 2.4
Mbit/s per receiver, so a 10-person video call is about 24 Mbit/s out, about 11 GB an hour. Video is the cost;
voice is cheap. Phase D3 measures real numbers on the box the first customers use.

**Honest limit.** The operator learns that tenant T had a call of N people for M minutes with this much video.
That is metadata, the same class as the numbers the relay already counts (ADR 0022), and it identifies no person
and no group.

### 8. Size limits: are 4 and 8 right?

- **Mesh limit of 4: keep as the shipped default, and measure whether voice can go higher.** At 4 people each
  device sends 3 copies: about 100 to 120 kbit/s of voice, trivial; 360p video at roughly 400 to 500 kbit/s is about
  1.5 Mbit/s up, which is already hard on mobile data. Voice-only mesh could plausibly reach **6** (5 connections
  per device, about 200 kbit/s up) and keep more calls free and operator-free, but each device then runs five DTLS
  handshakes, five decoders and five relayed pairs where a non-contact is present. **[unverified on a phone]**
  Phase B measures CPU, battery and bitrate on the emulator and on the real phone; raise voice-only to 6 only if
  the real phone passes. Video stays at 4.
- **Ring limit of 8: keep.** It is a notification, not a media limit, and it bounds spam ringing (each ring is up to
  8 members x their devices of sealed signals). Groups of 9 to 20 get the join bar. The ring policy above adds the
  per-group rate limit the first draft lacked.
- **Group size for calls:** small groups are capped at 20 people (ADR 0024), large at 500 (ADR 0025). The SFU's
  participant cap is a room-grant parameter, not a protocol limit; a first default is 25, tuned in D3 against the
  box. A call in a 500-person group can have 25 people in it; the others see the join bar and get "full".

### 9. What people see

- The chat header gets a call button for groups (voice; video in the menu). A running call shows a bar at the top of
  the chat: "Call in progress · 3 people · Join".
- The call screen shows a grid (up to 9 tiles; the rest as a strip), who is talking, and each person's verified
  state. The 4-digit call code of ADR 0028 does not scale to a group and is not shown; verification is per person,
  as in the chat. Pairs that go through the relay are marked, and the screen says when a member's address is hidden
  because they are not your contact.
- A person can leave without ending the call for others. The call log line in the chat says who started it and how
  long it lasted.

## Threat model

| Attacker / concern | What they can do | Outcome |
|---|---|---|
| The relay and the SFU operator | See IPs, timing, packet sizes, the number of people in a room, who is speaking (audio level header) | Cannot read media (SFrame), cannot join (room token), cannot tell which group a room belongs to. **Residual:** the speaking pattern and video sizes leak activity, as with any SFU; the audio-level setting reduces it. The operator also sees that the room belongs to a tenant. |
| A non-contact group member | Learn your IP from a mesh | Prevented: such pairs go through TURN with relay candidates only; in an SFU call members never see each other's addresses. |
| A removed member | Keeps the old call key and room token | The key changes on removal (small: a new generation; large: a new epoch) and the SFU drops the old token. Frames from before the removal were readable to them, as they were in the call. |
| A member who should not hear part of a call | — | Not a goal: everyone in a call hears everything said while they are in it, as in any call. |
| A malicious SFU | Forwards streams to the wrong room, drops or replays frames, injects frames | Cannot decrypt another room's frames; SFrame authenticates frames and its counters reject replays and forgeries; dropped frames are visible as a broken call. |
| A member device | Sends frames under someone else's sender id | Each sender's SFrame key is bound to its key id (generation and device id); a forged sender id fails to authenticate for everyone else. A member can still send *its own* garbage or bad keys to a call it is in; that disrupts the call and is solved by leaving and removing the member. |
| A member who sends different keys to different people | Splits the call (some members cannot decrypt) | Visible as a broken call; the deterministic rotator and generation rule limit it to one key-holder at a time. Not prevented in small groups (ADR 0024 has the same split-view limit); in large groups the key comes from the MLS epoch, so it cannot happen. |
| Replay of a recorded group call start | Make phones ring later, or show a call that is over | Fresh timestamp and call id rules (section 3); mailbox entries in a large group are old after 60 s and dropped. |
| Ring spam by a member | Ring everyone repeatedly | Per member, per group rate limits; muting; large groups never ring. |
| Free bandwidth through an SFU | Use an operator's SFU for free | A room needs a grant from an enrolled device; open-mode relays have the SFU off by default; caps ride in the grant. |
| Android cannot do frame encryption | Join an SFU call without end-to-end media encryption | Refused by rule (section 5): no join without SFrame. |

## Phases and sizes

Sizes are relative (S, M, L), not dates, as in `hosted-relay-plan.md`.

A. **This ADR**, approved or changed by the owner. Nothing is built before that.

*Can start as soon as A is approved; none needs a commercial decision:*

B0. **Core:** `GroupCall` kind and its validation, the sans-I/O group call state machine, the non-contact and
    freshness rules, ring policy, the randomized simulation, and a test in `core` that a member who is not a contact
    can send and receive `GroupCall`. (**M**)
B1. **Mesh voice for small groups, up to 4 people, contacts direct and non-contacts through TURN.** `app` bridge
    commands, the call bar and screen on the desktop and Android, drivers with three desktop windows plus the
    emulator, and the measurement of voice at 4 versus 6 on a real phone. (**L**)
B2. **Mesh video (360p).** (**M**)
C.  **Android SFrame spike** (section 5). Independent of B; start with B1. Output: go, or no-go with the fallback
    chosen. (**M**)
D1. **SFU spike with `str0m`:** forwarding only, no encryption yet, desktop only; tests the simulcast and keyframe
    gate of section 2 against SFrame-encrypted test frames, UDP and TCP/TLS reachability, and the room grant check.
    Output: `str0m` or LiveKit. (**M**)

*Needs owner decisions (below) before it ships, but design and spikes need none:*

D2. **SFrame on the desktop** (worker, key derivation, generation rotation, 2 s overlap) with the call key
    distributed in small groups. (**M**)
D3. **Relay integration:** `SfuCredentials`, room grant, room token, tenant limits and metering counters, the
    deployment guide; measurement of real cost per participant-minute. Depends on **ADR 0023 slice 1** (tenants,
    L, proposed). (**M**, plus ADR 0023's **L**)
D4. **Abuse and limits:** room caps, ring limits tuned on real devices, a status line in the app when a relay has no
    SFU. (**S**)
E.  **Large (MLS) groups:** keys from the MLS exporter, rotation on commits, SFU only, no ringing. (**M**)
F.  **Android on the SFU,** only if C passed or with the chosen fallback. (**M**)

Each phase gets its own "built" note here, as ADR 0028 did. Group calls on a future browser client (ADR 0033, stage 2)
are out of scope here.

## What the owner must decide, and what can be built now

**Build now (after approving this ADR):** B0, B1, B2 (mesh voice then video), C (Android spike), D1 (SFU spike). None
needs a pricing, entity or hosting decision.

**Decisions only the owner can make:**

1. **The overall shape:** mesh up to 4 plus an SFU for 5 or more, with SFrame (recommended), or mesh only, which
   needs no server and no new cryptography but stops at 4. Also whether large-group calls are in scope at all.
2. **Non-contacts in a mesh:** relay-only candidates for any pair that is not two contacts (recommended; the operator
   then carries that traffic), or refuse mesh calls in groups that contain non-contacts and use the SFU only.
3. **The SFU as a paid feature:** free mesh up to 4, SFU on hosted plans with included participant-minutes and a
   higher weight for video (recommended); the allowances and prices; whether Personal Plus includes any.
4. **Who may run an SFU:** off on open-mode relays (recommended); whether self-hosters get the same binary, with the
   same licence as the relay (the licence choice in `hosted-relay-plan.md` decision 7 decides whether someone else may
   resell it).
5. **ADR 0023 (tenants) first:** the SFU's room grants and limits depend on it. Its own open questions (who holds the
   admin key, operator key custody, redeem window) block the hosted SFU, not the spikes.
6. **Risk acceptance for the engine:** `str0m` with LiveKit as the fallback; and the Android fallback ladder in
   section 5 (WebView screen, other WebRTC build, or mesh only) if C fails.
7. **Ring policy and limits:** ring up to 8 with the per-group rate limit; whether voice mesh may rise to 6 if the
   phone measurements pass.

## Decisions (2026-11-13)

The owner answered four of the seven questions above; each took the recommendation.

1. **Shape:** mesh up to 4, SFU with SFrame for 5 or more and for all large-group calls.
2. **Non-contacts in a mesh:** pairs that are not both contacts connect through TURN with relay candidates only.
3. **Money:** mesh up to 4 stays free; SFU calls are metered on hosted plans in participant-minutes, video weighted
   higher, billed to the host's tenant; guests need no plan. Prices and allowances are still open.
4. **Operators:** hosted and self-hosted relays only; the SFU is off by default on open-mode relays. The hosted SFU
   waits for ADR 0023 (tenants); mesh and the spikes do not.

**Still open:** the licence for the SFU (decision 4 of `hosted-relay-plan.md`), risk acceptance for `str0m` and the
Android fallback order (question 6), and the ring policy and whether voice mesh may rise to 6 (question 7).
Status stays **proposed** until those are answered.

## Not decided here

- Recording, transcripts, or any server-side processing: out of scope, and they would conflict with SFrame.
- Screen sharing in group calls (works in one-to-one desktop calls): later, after D.
- Exact limits (people per call, video tiles, simulcast layers) are tuned in D3 against real devices and the box the
  first customers use.
- High availability of the SFU (one box, no failover in the first version; a call on a failed SFU ends and can be
  restarted), and regions.
- iOS does not exist yet.
- The independent security review: group-call keys and the SFrame use need it, as ADR 0028 said of WebRTC.
