# ADR 0018: Voice and video calls (draft)

Status: **Draft** (not accepted, not implemented)
Date: 2026-10-19

## Context
People asked whether Spike can carry video calls. The roadmap defers video from the first pilot ("Deferred: video/livestream"), and nothing in the code supports real-time media:
- The relay is **store-and-forward** (a queue in SQLite). Calls need a live stream with under ~200 ms of delay.
- There is no NAT traversal: direct device-to-device connections are deferred (ADR 0001), so there is no STUN/TURN.
- Groups use pairwise fan-out for now (desktop prototype) and MLS later; group calls need group keys.

This draft records how calls should fit the architecture so nothing built before then blocks it.

## Proposed design
**Spike is the signalling and identity layer; WebRTC carries the media.**

1. **Signalling over Spike.** Call offer, answer, ICE candidates, ringing, decline and hang-up are new end-to-end encrypted envelope kinds (`call_offer`, `call_answer`, `call_ice`, `call_end`), sent to every device of the callee like any message. The relay sees ordinary opaque blobs. The first device to answer wins; the others get `call_end` with "answered elsewhere".
2. **Identity-bound media keys.** WebRTC's DTLS certificate fingerprint for the call is placed in the encrypted offer/answer and the envelope is signed by the device, whose key is in the owner-signed device list. The callee's app checks the fingerprint against the one in the DTLS handshake. A man in the middle would have to break the existing safety-number check, which is stronger than most calling apps (where the signalling server could swap fingerprints). The call screen shows "keys checked against Bob's identity" and the contact's verified state.
3. **Media: WebRTC** (DTLS-SRTP, Opus, VP8/VP9/AV1). In the Tauri app, WebRTC comes from the webview (WebView2 on Windows, WKWebView on macOS). Linux WebKitGTK support is weak: Linux may need a native WebRTC stack or no calls at first.
4. **Connectivity.** STUN to find addresses; **TURN on the home relay / Spike Box** for the roughly 10–20 % of calls that cannot connect directly. TURN only forwards encrypted packets. TURN credentials are short-lived and issued to authenticated devices, so the relay cannot be used as an open proxy. The UI says whether a call is direct or relayed.
5. **1:1 first** (voice, then video). **Group calls later**: an SFU forwards streams, with an extra end-to-end layer (SFrame / insertable streams) keyed from the group's keys, so the SFU cannot see media. This needs MLS (Phase 3) for sound group keys.
6. **Agents** do not join calls in this design.

## What the relay operator learns
That two devices exchanged a few signalling messages (as with chat), and, for relayed calls, that a TURN allocation carried N bytes between two IP addresses for T minutes. Direct calls reveal IP addresses to the other party; offer a "relay only" setting to hide them, at the cost of bandwidth.

## Costs and risks
- **Bandwidth:** relayed video is about 1–2.5 Mbit/s per direction. Hosted relays need TURN capacity and pricing that covers it; self-hosted Spike Boxes need upstream bandwidth.
- **Abuse:** TURN must be rate-limited per device and capped per call.
- **Platform gaps:** Linux webview, mobile background ringing (needs push, which does not exist yet).
- **Review:** WebRTC configuration and the fingerprint binding need the independent security review like everything else.

## Not before
A desktop app (Tauri, separate ADR), direct connections or at least STUN/TURN, push for ringing on mobile, and pilot evidence that people want calls inside Spike rather than a link to an existing tool. Until then, the honest stopgap is a "start a call in another app" link, labelled as outside Spike's guarantees.

## Prototype
The desktop prototype shows voice and video buttons in one-to-one conversations and a call screen marked **planned**, with the identity check and direct/relayed path visible. Group conversations show the call buttons disabled ("Group calls come later").
