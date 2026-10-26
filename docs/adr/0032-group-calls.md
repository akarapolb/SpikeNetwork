# ADR 0032: Group calls

Status: **proposed, 2026-10-26.** Nothing here is built. Turns phase 4 of ADR 0028 ("group calls, last; needs its
own ADR") into a design and a build order, for the user to approve before any code is written.

## Problem

People in a group want to talk together. One-to-one calls (ADR 0028) work: Spike envelopes carry the setup,
WebRTC carries the media, DTLS-SRTP encrypts it between the two devices, and TURN on the relay's box forwards
packets it cannot read when the two cannot reach each other directly.

A group call breaks two of those assumptions:

- **Bandwidth.** If every device sends its stream to every other device (a *mesh*), each one uploads n-1 copies.
  That works for 3 or 4 people on voice, poorly for video, and not at all for 10.
- **Who can read the media.** The usual answer to bandwidth is a server that receives each stream once and forwards
  it to the others (an *SFU*). DTLS-SRTP then ends at the server: it could listen to every call. That is not
  acceptable here; nothing else in Spike lets the operator read content.

## Decision

### 1. Two shapes, chosen by size

- **Up to 4 people: a mesh.** Each device opens one WebRTC connection per other device, exactly as in a one-to-one
  call, and TURN helps when needed. No new server, no new cryptography: each pair's DTLS-SRTP is already end to end.
  Voice first; video is allowed but capped at 360p in a mesh.
- **5 or more: an SFU with end-to-end media encryption.** Each device sends its streams once to the SFU, which
  forwards them. Media is encrypted *before* it leaves the device with **SFrame** (RFC 9605), so the SFU forwards
  frames it cannot read. DTLS-SRTP still runs between device and SFU, as the transport layer.

A call that grows past 4 moves from mesh to SFU (everyone reconnects once; the screen says "Moving the call to the
relay's call server…"). A call never moves back.

### 2. The SFU: our own, built on `str0m`, next to the relay

- A small forwarding server in Rust, `spike-sfu`, built on **str0m** (MIT/Apache-2.0, a sans-I/O WebRTC library used
  for SFUs). It runs on the same box as the relay and coturn, like TURN in ADR 0028; the deployment guide gains one
  service. The relay hands out its address the way it hands out TURN logins.
- It only forwards: no mixing, no recording, no transcoding. It picks which video layers to forward (simulcast:
  each sender sends 3 sizes; the SFU sends each receiver the size that fits), and forwards audio from at most the
  3 loudest speakers when a call is large (audio level is a header extension, readable without the media key).
- **Why not an existing SFU.** LiveKit, Jitsi Videobridge, Janus and mediasoup are mature, but each brings its own
  signalling protocol and client SDK (or, for mediasoup, a Node application server). Our clients use plain WebRTC
  with Spike's own signalling, and the media keys must stay out of any server's hands. A small SFU that speaks our
  signalling is less code to trust than adapting a large one. Revisit if `spike-sfu` turns out harder than expected:
  LiveKit (Apache-2.0) with its end-to-end encryption mode is the fallback.

### 3. Signalling: group envelopes, not a new channel

New call signals travel the way group messages do: sealed to each member device in a small group (ADR 0024), as an
MLS application message in a large group (ADR 0025). The relay sees opaque blobs, as for chat.

- `group_call_start` (call id, media kinds, shape): "Ann started a call". **Small groups ring** (like a one-to-one
  call, up to 8 members); larger groups show a notification and a "Join" bar in the chat, without ringing.
- `group_call_join` / `group_call_leave` (call id): who is in the call, so every device shows the same list.
- In a mesh, each pair then exchanges `call_invite` / `call_answer` / `call_ice` as in ADR 0028, tagged with the
  group call id, between the two devices only.
- With the SFU, each device talks to the SFU over a WebSocket to the relay's box (its own SDP offer/answer and ICE),
  authenticated with a **room token** (below). The SFU never sees Spike envelopes.
- `group_call_end`: sent by the last person to leave, or when the call is empty for 60 s.

### 4. Keys

- **Small groups (ADR 0024):** the person who starts the call makes a random 32-byte **call key** and sends it,
  sealed, to every member device that joins. When someone leaves (or is removed from the group during the call), the
  device that has been in the call longest makes a new key and sends it to everyone still in it. Each key has a
  number (`key_id`, carried in the SFrame header), so frames under the old key are dropped after a 2-second overlap.
- **Large groups (ADR 0025):** the call key is derived from the MLS group: `MLS-Exporter("spike group call",
  call_id, 32)` in the current epoch. Any commit (someone added or removed) moves the epoch, so the key changes with
  membership, as group messages do. A device that has not processed the commit cannot decrypt, which is right.
- **SFrame keys** per sender are derived from the call key with HKDF (`"sframe" || sender device id`), so one
  sender's key does not open another's frames.
- **Room token.** The SFU admits a device that presents `HMAC(call key, "room" || call id)` for that call's room. The
  first device to arrive registers the room with that token; a removed member's device keeps the old token, but
  when the key changes the remaining devices re-register the room under the new token and the SFU drops connections
  holding the old one. The SFU learns a random room id and a token, never the key or the group.

### 5. Media encryption on each platform

- **Desktop (WebView2, Chromium):** encoded insertable streams (`RTCRtpSender.createEncodedStreams`, or
  `RTCRtpScriptTransform` where available) in a worker that runs SFrame. The key never leaves the webview's memory
  for the call's lifetime.
- **Android (`org.webrtc`):** `RtpSender.setFrameEncryptor` / `RtpReceiver.setFrameDecryptor` take a native
  `FrameEncryptor`. Spike's native library (Rust, `mobile/`) would implement SFrame and expose it through a small
  C++ shim that implements WebRTC's `FrameEncryptorInterface` and `FrameDecryptorInterface`. **This is the riskiest
  part:** it ties our build to the WebRTC AAR's C++ ABI. It is phase C below and is tried before anything depends on
  it; if it cannot be done safely, Android joins large calls as voice-only through the mesh limit until it can.

### 6. What people see

- The chat header gets a call button for groups (voice; video in the menu). A running call shows a bar at the top of
  the chat: "Call in progress · 3 people · Join".
- The call screen shows a grid (up to 9 tiles; the rest as a strip), who is talking, and each person's verified
  state. The 4-digit call code of ADR 0028 does not scale to a group and is not shown; verification is per person,
  as in the chat.
- A person can leave without ending the call for others. The call log line in the chat says who started it and how
  long it lasted.

## Threat model

| Attacker / concern | What they can do | Outcome |
|---|---|---|
| The relay and the SFU operator | See IPs, timing, packet sizes, the number of people in a room, who is speaking (audio level header) | Cannot read media (SFrame), cannot join (room token), cannot tell which group a room belongs to. **Residual:** the speaking pattern and video sizes leak activity, as with any SFU. |
| A removed member | Keeps the old call key and room token | The key changes on removal (small: new key; large: new epoch) and the SFU drops the old token. Frames from before the removal were readable to them, as they were in the call. |
| A member who should not hear part of a call | — | Not a goal: everyone in a call hears everything said while they are in it, as in any call. |
| A malicious SFU | Forwards streams to the wrong room, drops or replays frames | Cannot decrypt another room's frames; SFrame counters reject replays; dropped frames are visible as a broken call. |
| A member device | Sends frames under someone else's sender id | Each sender's SFrame key is bound to its device id; a forged sender id fails to decrypt for everyone else. |

## Phases

A. **This ADR**, approved or changed by the user. Nothing is built before that.
B. **Mesh calls for small groups, up to 4 people, voice.** No new server or cryptography: group signals (3), the
   call bar and screen on both apps, and a driver with three desktop windows plus the emulator. Then video in the
   mesh (360p).
C. **Spike for Android SFrame, as a spike.** Prove the native `FrameEncryptor` shim on the emulator with a
   one-to-one call (both sides encrypting with a fixed test key). Go or no-go for D on Android.
D. **`spike-sfu` and SFrame on the desktop**, for calls of 5 or more: the server, room tokens, key rotation for
   small groups, simulcast. Then Android if C succeeded.
E. **Large (MLS) groups**: keys from the MLS exporter, rotation on commits, no ringing.

Each phase gets its own "built" note here, as ADR 0028 did.

## Not decided here

- Recording, transcripts, or any server-side processing: out of scope, and they would conflict with SFrame.
- Screen sharing in group calls (works in one-to-one desktop calls): later, after D.
- Exact limits (people per call, video tiles) are tuned in D against real devices.
