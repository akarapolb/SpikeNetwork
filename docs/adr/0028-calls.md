# ADR 0028: Voice and video calls

Status: **decided; the Rust signalling side is built (2026-10-24); no screen or media yet**. Turns the draft ADR 0018 into decisions and a build order. Phase 1 (the one-to-one voice slice on the same Wi-Fi) is next; nothing here is reviewed.

## Decision

**Spike carries the call setup; WebRTC carries the media.** Each device keeps its own WebRTC stack. The Rust engine never touches audio or video: it only sends, receives and validates *signalling*, as end-to-end messages, and keeps the call log.

1. **Media is WebRTC** (DTLS-SRTP, Opus, VP8/H.264). Android uses the native WebRTC library (`org.webrtc`, a prebuilt AAR from a maintained distribution; pinned and checksum-verified like other dependencies). The desktop uses the webview's own `RTCPeerConnection` (WebView2 on Windows, WKWebView on macOS); Linux WebKitGTK is not a target for calls until it proves adequate.
2. **Signalling is ordinary Spike envelopes.** One new envelope kind, `call`, whose body is one of four signals (as built; the first draft said four kinds). Sealed by the per-device session like any message, so the relay sees opaque blobs and never learns that a call exists: `call_invite` (call id, media kinds, SDP offer), `call_answer` (call id, SDP answer), `call_ice` (call id, one or more candidates), `call_end` (call id, reason: `hangup`, `declined`, `busy`, `timeout`, `answered_elsewhere`, `failed`). They travel through `send_to_device`, so when ADR 0027's direct connections are on and the peer is reachable, the call setup does not touch the relay either; otherwise it goes through the relay queue (slower, but it works). Trickle ICE is used, with candidates batched for about 50 ms.
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

## Built so far (slice 1: signalling in Rust)

- `core/src/call.rs`: the signals, their validation and the sans-I/O call state machine; 16 unit tests (ringing across devices, first answer wins, decline, busy, glare, timeout, stale/duplicate/replayed invites, per-contact rate limit, signals from the wrong device or contact ignored).
- `Kind::Call` envelope; `cli`: `ops::send_call` (to all of a contact's devices, one, or our own others; no relay housekeeping) and `Outcome::Call` (contacts and our own devices only).
- `app`: `calls.rs` (`call_start/accept/decline/hangup/fail/ice/wait`), the bridge commands, and `app/tests/calls.rs` (two phones through a real relay: ring, answer, candidates held until answered, hang up, decline, caller gives up, glare, media failure).
- Desktop: `desktop/ui/call.js` (call buttons, ringing, WebRTC, call code, direct/relayed note, mute, camera) and Tauri commands. `desktop/drive_call.mjs` drives two real desktop windows with fake microphones: ring, answer, audio packets with energy both ways, same call code on both, hang up, decline, caller gives up (all pass).
- Android: `Calls.kt` (`CallManager` over the WebRTC AAR `io.getstream:stream-webrtc-android:1.3.10`, the call screen, microphone permission asked at first call/answer, a high-importance incoming-call notification with a full-screen intent). `android/drive_call.mjs` runs the emulator against the real desktop app: the desktop calls and the phone answers, the phone calls and the desktop answers, a decline. Audio packets flow both ways (the phone receives the desktop's beeping fake microphone with non-zero energy), both screens show the same call code. All pass.
- Added later the same day: Android video (front/any camera 720p, remote and self views, camera off, switch camera); call log lines in both conversations ("Voice call, 3:12", "Missed video call", ...; local to the device, not synced); a foreground service while a call is on (microphone, camera types); the incoming-call alert opens the app over the lock screen and turns the screen on, and the app closes again when the call ends (Android 14+ also needs the person to allow full-screen notifications: Settings has a button for it); mic, speaker and camera icons. The driver now also checks video both ways, the call log, and a call to a locked phone with its screen off.
- Found by the driver and fixed: stopping the camera on the main thread froze the app (ANR); a send that raced the listener's relay connection was lost silently (now retried).
- Test-harness note: this WebView2 ends the fake camera at once unless it runs with `--disable-gpu`; the drivers pass that flag.
- Ringing a locked phone, learned on a real Samsung phone: the connection and the alert were fine from the start (the in-app Call diagnostics showed every call arriving); the screen did not wake until the call was shown by its own small `CallActivity` (declared show-when-locked and turn-screen-on, task separate from the app so only the call is ever visible) opened from a call-style notification with Answer and Decline, plus a wake lock and vibration. Settings has a "Calls on this phone" checklist (notifications, urgent call channel, lock-screen calls, battery, display over other apps, and each maker's autostart list) so other makers' switches are one tap away; with "display over other apps" on, the call window is opened directly and no longer depends on a maker's full-screen-alert rules. Not tested on makers other than Samsung and the emulator. A self-managed Telecom `ConnectionService` would be the next step for the most consistent behaviour across makers.
- **Not built:** TURN (cross-network calls) and group calls; Android video is tried only with the emulator's virtual camera; the call log entry in the conversation, any screen, WebRTC on either platform, the Android alert/foreground service, the emulator-to-desktop audio driver, a test with two devices on the callee side (the state machine covers it; the end-to-end test does not).

## Update: phase 3 built, 2026-10-24

- **TURN (calls across networks).** The relay (`--turn-url`, `--turn-secret`, `--turn-ttl-secs`) hands a signed-in device a
  short-lived login for the operator's TURN server (`ClientMsg::TurnCredentials`; coturn's `use-auth-secret` scheme,
  `core/src/turn.rs`, verified against the RFC 2202 vector). Both apps fetch one when a call starts and pass it to WebRTC;
  Settings has "Always use the relay for calls" (relay candidates only: the other person never sees this device's address).
  `docs/turn.md` is the coturn guide. `relay/examples/turn_test_server.rs` is a stand-in that accepts the same logins;
  `desktop/drive_turn.mjs` proves a call whose both ends use relay candidates only connects and carries audio, and that
  with the setting off the same call goes direct. Not tried: real coturn, TLS TURN (`turns:`), a phone on mobile data.
- **Network changes mid-call.** Three new signals (`Restart`, `RestartAnswer`, `RestartAsk`), valid only inside the
  active call, from the device on the other end, in the right direction (the caller offers, the callee answers or asks).
  A dropped connection (3 s) or a network change restarts ICE on the same call with a fresh TURN login; if it does not
  recover in 20 s the call ends. `desktop/drive_restart.mjs` (two desktops) and `android/drive_net.mjs` (the emulator's
  Wi-Fi switched off for 12 s and on) show the call carrying on; the phone logs "network changed during a call".
- **Closed app.** Spike calls are registered with Android's calling framework (a self-managed `ConnectionService`,
  `Telecom.kt`): the system accepts the call, asks the app for its UI, and treats it as a call. If it refuses, the old alert
  is used. "Keep receiving after a restart" (off by default) seals the passphrase under a hardware key that needs no
  fingerprint, so the listener service, now sticky, reopens the engine itself after the system kills the app
  (`android/drive_bg.mjs`: process killed with `kill -9`, the service came back, a call rang with nobody opening the app).
  A boot receiver starts it after a restart, but that path is not driven here (the system refuses a boot broadcast sent
  from `adb`). The trade-off is stated where the switch is: anyone who can unlock the phone and open the app could read messages.
- **Wake-up without Google push (UnifiedPush): built.** Relay: `RegisterPush` / `UnregisterPush` (schema v4), and when a
  message is queued for a device with no live connection the relay sends one empty POST ("spike") to its endpoint
  (`relay/src/push.rs`): https only, no credentials, a resolved address that must be public (checked before connecting, to that
  address), no redirects, short timeouts, 16 at a time, one per device per 5 s, a 404/410 forgets the endpoint, `--no-push`
  to switch off. Android: the connector library (3.0.10, Kotlin-2.0 compatible) with a receiver; Settings has the switch; the
  app leaves the endpoint with the relay once the engine is open; a wake-up opens the engine (needs "Keep receiving after a
  restart"), reads, and lets the call ring. Tested: five relay tests against a local endpoint (a push for an offline device,
  none for an online or unregistered one, coalescing, 410 forgets, private/odd endpoints refused) and
  `android/drive_push.mjs` (process killed; the relay posts the wake-up; the wake-up delivered by `adb` to the killed app;
  the phone rings). Not tested: a real push app or server, the push app's own battery rules. `docs/unifiedpush.md`.
- **Polish and checks (after UnifiedPush).**
  - *Audio on Android* (`CallAudio.kt`): one button steps through earpiece, speaker, Bluetooth headset and wired headset as they
    are available; plugging one in moves the sound there unless the person chose; the screen switches off against the ear
    on a voice call on the earpiece (proximity sensor); another call or app taking the audio puts the call on hold (the
    system's hold, and audio focus as a fallback) and closes the microphone until it ends. Driven on the emulator, which has
    only a speaker and no proximity sensor: the earpiece, Bluetooth and the sensor are built but not seen working.
  - *Screen sharing on the desktop*, in video calls only: the picture of the screen takes the camera's place on the same
    connection (`replaceTrack`), nothing is negotiated again; stopping puts the camera back (`desktop/drive_share.mjs`, two
    windows: the other side keeps receiving frames at screen size and again from the camera after). Voice calls offer no share
    button. Android shows the other person's picture whole instead of cropped, so a shared screen reads; it cannot share its own.
  - *Call screens*: "Reconnecting…" when sound stops or the connection drops, a word for the connection (good, fair, poor, from
    packet loss and round trip; the numbers are in the tooltip on the desktop), and "Call back" on a call line in the conversation.
  - *Randomized tests of the state machine* (`core/tests/call_sim.rs`): two people dial, answer, decline and hang up at random
    while signals arrive late, out of order and twice; no machine goes quiet without telling the front end, nothing rings
    twice, nobody is left ringing, and the two sides agree at the end. 20,000 histories pass. They found two real bugs, now fixed
    and covered by unit tests: an invite that arrived after the caller's hang-up rang a call nobody was making, and the same
    answer delivered twice told the callee the call was over.
  - *`cli/examples/relay_check.rs`*: one command to test a deployed relay from outside (name, certificate, sign-in, TURN
    logins, TURN allocation over UDP, TCP reachability); `deploy/README.md` shows it. Run against a local relay and the
    stand-in TURN server; not yet against a real server.

## Update: 2026-10-26

- *Audio focus as a fallback only.* On Android a call the system's calling framework manages (self-managed Telecom)
  is held and resumed by the system (`SpikeConnection.onHold` / `onUnhold`). The app also asked for audio focus, and
  the system then takes focus *for that same call*; when its request came second, the app read the loss as "another
  call has the audio" and put its own new call on hold, microphone closed. A call the phone placed then sent no sound
  about half the time (`android/drive_call.mjs`, phone as caller). Audio focus changes now only hold the call when the
  system is not managing it.
