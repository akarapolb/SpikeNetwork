# ADR 0028: Voice and video calls

Status: **decided, not built**, 2026-10-24. Turns the draft ADR 0018 into decisions and a build order. Phase 1 (the one-to-one voice slice on the same Wi-Fi) is next; nothing here is reviewed.

## Decision

**Spike carries the call setup; WebRTC carries the media.** Each device keeps its own WebRTC stack. The Rust engine never touches audio or video: it only sends, receives and validates *signalling*, as end-to-end messages, and keeps the call log.

1. **Media is WebRTC** (DTLS-SRTP, Opus, VP8/H.264). Android uses the native WebRTC library (`org.webrtc`, a prebuilt AAR from a maintained distribution; pinned and checksum-verified like other dependencies). The desktop uses the webview's own `RTCPeerConnection` (WebView2 on Windows, WKWebView on macOS); Linux WebKitGTK is not a target for calls until it proves adequate.
2. **Signalling is ordinary Spike envelopes.** Four new envelope kinds, sealed by the per-device session like any message, so the relay sees opaque blobs and never learns that a call exists: `call_invite` (call id, media kinds, SDP offer), `call_answer` (call id, SDP answer), `call_ice` (call id, one or more candidates), `call_end` (call id, reason: `hangup`, `declined`, `busy`, `timeout`, `answered_elsewhere`, `failed`). They travel through `send_to_device`, so when ADR 0027's direct connections are on and the peer is reachable, the call setup does not touch the relay either; otherwise it goes through the relay queue (slower, but it works). Trickle ICE is used, with candidates batched for about 50 ms.
3. **Calls are not chat messages.** Signalling envelopes are never stored as conversation messages and never ack'd as unread. The engine writes one *call log* entry per call (who, when, direction, duration, outcome: answered / missed / declined) which the screens show as a system line in the conversation.

### Authenticating the peer

- A signalling envelope arrives through an Olm session whose far end is a device in the contact's signed device list (ADR 0008). That already authenticates the sender: nobody else can produce a valid `call_invite` from that device.
- The SDP carries the DTLS certificate fingerprint. Because the SDP travels inside the authenticated envelope, **the DTLS handshake's fingerprint is checked against the one the person signed into the offer/answer**: a man in the middle of the media path cannot substitute a certificate. A mismatch ends the call (`failed`), it is never offered as a warning to click through.
- The call screen shows the contact's name and **whether the person is verified** (ADR 0008 safety number), the same wording as the chat header. For an unverified contact it also shows a short **call code** (4 digits derived with HKDF from the two DTLS fingerprints, sorted), which both people can read to each other: it is optional and catches an attack on the Olm session itself, which is not expected, but costs nothing.
- Calls are accepted **only from contacts** (a device in a known, verified device list) or from our own devices. A stranger's `call_invite` is dropped silently; strangers use the message-request path (ADR 0021) first. This also bounds spam ringing.

### Ringing across a person's devices

- The caller sends `call_invite` to **every device** of the callee (the list the engine already fans messages out to). It also sends it to its *own other* devices so they show "calling from your other device" and do not start a second call.
- Every callee device rings. The first to accept sends `call_answer` to the caller, and `call_end(answered_elsewhere)` to the callee's *other* devices; they stop ringing and log "answered on <device>". The caller attaches to the first valid `call_answer` and ignores later ones for that call id (answers from other devices get `call_end(answered_elsewhere)`).
- **Decline** on a device only stops that device ringing and sends `call_end(declined)` to the caller; the caller shows "declined" once every device has declined, or `timeout` after 45 s of ringing. A person declines everywhere by declining on the device that is ringing and choosing "decline on all my devices" (sends `answered_elsewhere`-style end to siblings with reason `declined`). The default is the quieter one.
- **Busy:** a device already in a call replies `call_end(busy)` to a new invite for itself only; its siblings still ring.
- **Glare** (both call each other at once): the call whose id is lexicographically smaller wins; the other side drops its own invite and treats the winner as an incoming call, auto-accepting if the person was already calling that contact.
- One active call per device. Ringing and call state are in memory: after a crash a call is over.

### Incoming call with the app closed (Android)

There is no Google push in this product (no third-party that sees timing), so a closed app cannot be woken from outside. The honest design:

- A **foreground service** (`specialUse`/`dataSync`, with a persistent low-priority notification the person can switch off) keeps the relay wait loop and, when direct connections are on, the listener alive. This is opt-in as "Receive calls when Spike is closed". It costs battery; the setting says so.
- An invite arriving there raises a **full-screen intent** notification (`USE_FULL_SCREEN_INTENT`, a high-importance "Incoming call" channel with a ringtone and vibration) showing the caller, **Answer** and **Decline**. Android 14+ requires the person to grant full-screen intents; if they have not, it degrades to a heads-up notification with the same buttons.
- On answering, the activity starts a second foreground service of type `microphone` (and `camera` for video) for the duration of the call, as Android requires to keep capturing in the background.
- With the setting off, a closed app learns of a call when it next opens: it logs a **missed call** from the signed timestamp of the (expired) invite, and tells the person so.
- Desktop: while the app runs (including tray), an invite raises an operating-system notification and a ringing window; the call window stays on top until answered or declined. Closed means missed.

### Permissions

Microphone (`RECORD_AUDIO`) is requested when the person first starts or answers a call; camera (`CAMERA`, already declared for scanning) when video is first turned on. Never at launch. A refusal means a voice call without sending audio is impossible, so the call is declined with a clear message; refusing the camera still allows voice. Video can be turned on or off during a call, and the camera and microphone are released the moment the call ends. The desktop grants the webview's media permission only to the app's own origin and only while a call screen is up.

### Where the code goes

- `core`: the envelope kinds and their validation (call id 16 random bytes; SDP at most 16 KiB; at most 16 candidates per `call_ice`; known reason strings), the replay rules below, and a pure **call state machine** (idle, calling, ringing, connecting, active, ended) that takes events and emits actions (send this signal, start/stop ringing, attach). It is sans-I/O and unit tested with no WebRTC at all: ringing across devices, first answer wins, glare, busy, timeout, replayed and stale invites, out-of-order ICE.
- `app`: bridge commands `call_start`, `call_accept`, `call_decline`, `call_end`, `call_signal` (the front end hands up its SDP/ICE) and a `call_events` wait, so both front ends use the same state machine and only differ in the media API they call.
- Android: a `CallManager` around `PeerConnectionFactory`; desktop: the same logic in `app.js` around `RTCPeerConnection`.

## Phases

1. **One-to-one voice on the same Wi-Fi** (this ADR's first build). Only host candidates are gathered (no STUN, no TURN), so the call cannot leak to a STUN server and needs no Internet. Needs ADR 0027's direct transport to be useful without a relay, but signalling works over the relay too. Includes the engine pieces above, the call screen, ringing across devices, and the full-screen alert.
2. **Video**, same path: camera capture, a switch-camera and a camera-off button, a picture-in-picture self view, resolution capped at 720p and bitrate by what the link reports. No new security design.
3. **Calls across networks (TURN).** A **coturn** server the relay operator self-hosts (the same box as the relay; documented in the deployment guide). It forwards DTLS-SRTP packets it cannot read. Credentials are short-lived (the TURN REST scheme, HMAC of an expiry), issued by the relay to an authenticated device and carried to the peer in the encrypted `call_invite`/`call_answer` so a stranger cannot use the server; each credential is per call, rate-limited per device and capped in bandwidth and duration. STUN comes from the same server (we do not use a public STUN, so no third party sees our IPs). The call screen says **Direct** or **Through relay**. A "Always use relay" setting (relay-only ICE policy) hides IPs from the peer at the cost of bandwidth and the operator seeing the pair of IPs and byte counts.
4. **Group calls, last.** An SFU (a self-hosted open-source one, in the same deployment) forwards streams; to keep the SFU blind, media is also encrypted end to end with SFrame (WebRTC insertable streams) under keys derived from the group's keys: the MLS epoch secret for large groups (ADR 0025), a per-call key sent in sealed envelopes to each member for small groups (ADR 0024), rotated whenever a member joins or leaves. Needs its own ADR before building.

## Threat model

| Attacker / concern | What they can do | Outcome |
|---|---|---|
| The relay (and an operator) | See opaque blobs and their timing, as for chat; for TURN calls, two IPs, durations and byte counts | Cannot read signalling or media, cannot place or answer a call, cannot swap a DTLS fingerprint (it is inside the sealed offer). **Residual:** a burst of small blobs both ways looks like a call setup; with direct signalling (ADR 0027) it does not reach the relay at all. |
| The callee / any peer | Learn the caller's **IP address** (ICE candidates are addresses) | Inherent to a direct call. On the LAN (phase 1) it is an address the peer could already see. For phase 3, calls are accepted from contacts only, and "Always use relay" removes it. See the note under the table: in phase 3 candidates are gathered only after the callee answers. |
| A stranger | Ring, spam, probe | Invites from non-contacts are dropped before any parsing beyond the envelope; ringing is rate-limited (3 unanswered invites per contact per hour, then silent). |
| Replay of a recorded `call_invite` | Make the phone ring later, or connect a stale offer | A blob cannot be replayed (Olm message keys are used once). If a hostile relay *withholds* a genuine invite and delivers it later, the invite carries the sender's signed timestamp and a call id: invites older than 60 s, or whose call id was seen in the last 24 h, are discarded and logged as missed. Answers and ICE for an unknown or ended call id are ignored. |
| Man in the middle of media | Substitute a certificate | Fails: the DTLS fingerprint is checked against the sealed SDP. |
| Compromised device | Place calls as that person | As for any message; removing it from the signed device list stops others accepting it. A device that silently auto-answers is **not** a feature: there is no auto-answer, and the microphone indicator is the operating system's. |
| Eavesdropping through a call | Hot microphone after hang-up | Capture is released on end and on any error; the foreground-service notification and the OS indicator are visible while it is on. |
| Denial of service | Many invites / ICE spam | Caps per contact and per call (above), state machine ignores events not valid in the current state. |

One honest design point on candidates: the caller must include its candidates in the `call_invite`'s offer for fast setup, so the callee learns the caller's addresses when the *invite* arrives, even if they never answer, and a contact can therefore see a LAN address by ringing. In phase 1 this is a LAN address of a contact. For phase 3, the offer is sent with no candidates and the caller starts gathering when the callee's `call_answer` arrives (one extra round trip, about 100 ms on the relay), so a contact who rings and is not answered learns nothing, and the relay-only setting sends relay candidates only.

## Not decided here / limits

- Audio processing (echo cancellation, noise suppression) is the platform's WebRTC defaults. Bluetooth headsets and the speaker/earpiece switch are Android audio-routing work in phase 1.
- Screen sharing, call recording (never built: Spike would not record), voicemail, and calls with agents are out of scope.
- Quality adaptation relies on WebRTC's built-in congestion control; no custom code.
- iOS does not exist yet.
- WebRTC configuration and the fingerprint binding need the independent security review, as ADR 0018 said.

## Testing plan

- Unit: the call state machine (every transition above, plus invalid events), envelope validation (caps, bad reasons, stale timestamp, duplicate call id), call code derivation (same on both sides, differs if a fingerprint differs).
- Integration (Rust, real relay and direct transport, no media): a caller and two callee devices; first answer wins; decline; busy; glare; timeout; signalling with the relay dead (needs 0027).
- End to end per phase: a driver that places a call between the emulator and the desktop and proves ICE reached `connected` and audio packets flowed in both directions (WebRTC stats `packetsReceived > 0` and `audioLevel` above zero against the emulator's virtual microphone), as `android/drive_direct.mjs` does for messages.
