# Group calls: build plan

Written 2026-11-14 for ADR 0032 (proposed; the owner took the recommendations on shape, non-contacts, money and
operators on 2026-11-13). This plan covers only what needs no further owner decision: phases B0, B1, B2, C and D1. Phases
D2 to F are listed at the end with what they wait for. Sizes are relative (S, M, L), not dates.

Existing code this builds on (line counts at 2026-11-14):

| Area | File | Lines |
|---|---|---|
| One-to-one call signals and state machine | `core/src/call.rs` | 1738 |
| Randomized simulation of that machine | `core/tests/call_sim.rs` | 511 |
| Envelope kinds and per-kind rules | `core/src/envelope.rs` (`Kind::Call` at the lines that list kinds), `core/src/merge.rs` | |
| Receive path that handles `Kind::Call` | `cli/src/lib.rs` (two places), `cli/src/ops.rs` (`send_call`, `Outcome::Call`) | |
| Bridge commands for calls | `app/src/calls.rs` | 511 |
| Desktop call screen and WebRTC | `desktop/ui/call.js`, `desktop/src-tauri/src/api.rs` | 397 |
| Android call manager, audio, Telecom | `Calls.kt` (1057), `CallAudio.kt` (190), `Telecom.kt` (120) | |
| Drivers | `desktop/drive_call.mjs`, `android/drive_call.mjs` | 85, 223 |

## Order and parallelism

```
B0 core ──► B1 mesh voice ──► B2 mesh video
   │
   └─(independent)─► C  Android SFrame spike      (start with B1, own session)
   └─(independent)─► D1 SFU spike with str0m      (start with B1, own session)
```

B0 comes first because B1 builds on it. C and D1 touch no shared files with B, so they can run as separate sessions
in parallel with B1. B1 and B2 both edit the call screens, so they run one after the other.

## B0: core (M)

Goal: a member of a group who is **not a contact** can start, join and leave a group call at the message level, with
every rule of ADR 0032 section 3 enforced and tested, and no WebRTC anywhere.

1. **`core/src/envelope.rs`: add `Kind::GroupCall`.** Mirror how `Kind::Call` is listed at its three places (never
   stored as a conversation message, no ack as unread, refused in own-device sync). Rules: requires `group` set;
   refused when `group` is absent; body size cap.
2. **`core/src/group_call.rs` (new):** `Signal` enum (`Start`, `Join`, `Leave`, `Key`, `KeyAck`, `End`, plus the
   mesh pair signals `Invite`, `Answer`, `Ice`, `PairEnd`), validation (call id 16 random bytes, SDP at most
   `MAX_SDP`, at most `MAX_CANDIDATES` per `Ice`, known shapes, a signed timestamp), and the freshness rule (60 s,
   24 h call-id memory). Reuse the constants and helpers from `call.rs` rather than copying them.
3. **Sans-I/O state machine in the same file:** events in (signals, timers, user actions), actions out (send this
   signal to these devices, start or stop ringing, join, leave, move to the SFU). Covers: one device per person,
   "in this call on another device", first-join-silences-ringing, host handover (longest in call, lowest device id),
   ring policy (8 members, 3 unanswered per member per hour, one ring per group per 10 minutes, mute), the
   mesh-to-SFU move at 5, and "full" when no SFU is offered.
4. **Acceptance rule in `cli/src/lib.rs` and `cli/src/ops.rs`:** a `GroupCall` is accepted under ADR 0024 section 4
   (we are a member of the named group, the sending device belongs to a member of our current state), not under the
   contacts-only rule of `Kind::Call`. Add `Outcome::GroupCall` and `ops::send_group_call` (fan out to every device
   of every member, plus own other devices, like `groups::send`).
5. **`core/tests/group_call_sim.rs` (new):** copy the shape of `call_sim.rs`. Several people, several devices each,
   start/join/leave/decline at random, signals late, out of order and duplicated. Invariants: nobody is left ringing,
   nothing rings twice, everyone converges on the same participant list, no person is in a call on two devices.
6. **Integration test `cli/tests/group_call.rs` (new):** three people through a real relay, two of them not contacts
   of each other; start, join, leave, a removed member's signals refused, a stale signal dropped.

Done when: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` are green, the
simulation runs at least the 20,000 histories `call_sim.rs` runs, and `app`, `desktop` and `mobile` compile unchanged.
No change to the wire format of existing kinds.

## B1: mesh voice, up to 4 people (L)

Goal: a three-person voice call in a small group between two desktop windows and the emulator, with a non-contact in it.

1. **`app/src/group_calls.rs` (new), bridge commands:** `group_call_start`, `group_call_join`, `group_call_leave`,
   `group_call_signal` (the front end hands up SDP and candidates for one pair), `group_call_events` (wait). Same
   pattern as `app/src/calls.rs`, so both front ends share one state machine.
2. **Pair connections and relay-only for non-contacts.** The front end opens one `RTCPeerConnection` per other member.
   For a pair where either side is not the other's contact, the app tells the front end to use relay candidates only
   and to fetch a TURN login (`ClientMsg::TurnCredentials`, already built). Add the "Direct" or "Through relay" note
   per pair.
3. **Desktop:** `desktop/ui/group-call.js` (new) for the grid, the call bar in the chat ("Call in progress, 3 people,
   Join"), and the join and leave buttons; Tauri commands in `desktop/src-tauri/src/api.rs`. Reuse the audio code in
   `call.js`; do not fork it.
4. **Android:** `GroupCalls.kt` (new) next to `Calls.kt`, using the same `PeerConnectionFactory`; the call bar in the
   group chat; the ringing path through `Telecom.kt` for small groups; `CallAudio.kt` unchanged.
5. **Drivers:** `desktop/drive_group_call.mjs` (three windows, fake microphones, audio energy checked on every pair,
   one member not a contact, leave and rejoin) and `android/drive_group_call.mjs` (emulator joins two desktops).
   Model them on `drive_call.mjs`; keep the `--disable-gpu` flag the existing drivers use.
6. **Measurement for the "6 people voice" question:** a script that records CPU, battery drain and bitrate on the
   emulator and, by hand, on the real phone at 4 and 6 people. The result goes into ADR 0032 section 8.

Done when: both drivers pass; a member who is not a contact never receives a direct candidate from you (checked in the
driver by reading the connection stats); the call log shows "Group call, 3:12, 3 people"; all existing call drivers
still pass.

## B2: mesh video (M)

360p cap per stream, camera on and off, self view, grid of up to 4 tiles. Same files as B1; no new security design.
Done when the drivers also check video frames on every pair and that the cap holds.

## C: Android SFrame spike (M), independent

Goal: a go or no-go for frame encryption on the Android WebRTC library, before anything depends on it. This is a spike:
no UI, no production wiring, and its branch is thrown away if the answer is no.

1. Find out, before writing C++, whether `io.getstream:stream-webrtc-android:1.3.10` exposes `RtpSender.setFrameEncryptor`
   and `RtpReceiver.setFrameDecryptor`, and whether the matching C++ headers can be obtained for the same libwebrtc
   revision. Record the answer in ADR 0032 section 5 whatever it is.
2. If yes: a minimal native shim that implements a frame encryptor and decryptor with a **fixed test key and plain
   AES-GCM first**, wired into one call on the emulator against the desktop, with a matching worker on the desktop
   (insertable streams). Then switch to real SFrame framing.
3. Pass criteria are the four in ADR 0032 section 5: audio energy both ways, a wrong key gives silence, a reproducible
   pinned build, and a real phone.
4. If no: try fallback (a) of the ADR (the group-call screen in a WebView) as a second spike, and write down the result.

Output: a short note in ADR 0032 with go, no-go and the chosen fallback.

## D1: SFU spike with str0m (M), independent

Goal: decide `str0m` or LiveKit with evidence. Throwaway code in `spikes/sfu/` (the repo already has a `spikes` folder).

1. Check `str0m` first: licence, last release, simulcast and RTCP feedback support, ICE over TCP. Record the answer.
2. A forwarding-only server: one room, 5 desktop clients (headless Chromium or the webview), audio plus video,
   simulcast with three layers, **no encryption yet**.
3. The gate from ADR 0032 section 2: with test frames encrypted by an insertable-streams worker, the SFU must still
   switch layers and ask for keyframes using only RTP header extensions and RTCP. Measure how long a layer switch takes.
4. UDP blocked: confirm a client reaches the SFU through coturn over `turns:` on 443.
5. A room-grant check with a fake MAC, to prove the join path of ADR 0032 section 6 works with a bearer token.

Output: a one-page result in ADR 0032 and a go or no-go on `str0m`. No production code is merged from the spike.

## What waits, and for what

| Phase | Waits for |
|---|---|
| D2 SFrame on the desktop with key rotation (M) | D1 go, and the owner's answer on risk acceptance |
| D3 relay integration: `SfuCredentials`, room grants, tenant limits, metering (M) | ADR 0023 slice 1 (tenants, L, not built) and the owner's pricing |
| D4 abuse limits and the "no call server" line (S) | D3 |
| E large-group calls via the MLS exporter (M) | D2 and D3 |
| F Android on the SFU (M) | C go, or the chosen fallback |

## Risks to watch while building

- **B1 is the large piece.** Group state, device fan-out, and two UIs. If it slips, ship desktop first and Android after;
  the driver design allows it.
- **Android calls are fragile across makers** (ADR 0028 notes only Samsung and the emulator were tried). The ring path
  for groups reuses `Telecom.kt`; test it on the real phone before calling B1 done.
- **TURN carries every non-contact pair.** With the coturn quotas in `docs/turn.md` (4 allocations per user) a
  4-person call with three non-contacts could hit the per-user quota. Check this in B1 and raise the quota advice if so.
- **One test machine shares ports.** The existing drivers kill `spike-relay.exe` by name; group drivers must use the
  same file-name convention as the Wi-Fi relay so they do not stop each other.

## Suggested split into sessions

1. **B0 then B1 then B2**, in one session, in that order (they share files).
2. **C**, its own session, started together with B1.
3. **D1**, its own session, started together with B1.

Each commits on its own branch and is reviewed before merging; no phase here changes the wire format of an existing
message kind or the relay.
