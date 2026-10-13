# ADR 0006: Fallback prekey

Status: **Proposed** (implemented)
Date: 2026-10-13

## Problem
Sessions start from single-use prekeys held by the relay (ADR 0002). Anyone can claim a device's prekeys, so an abuser, or a malicious relay, could drain them and stop new contacts from reaching that device until it reconnects and replenishes.

## Decision
Each device also publishes one **fallback prekey**: a reusable last resort that the relay hands out only when no one-time prekeys remain. It is signed by the device key like any other prekey, checked by the relay on upload and by the sender before use.
- **Rotation:** the client replaces it when the relay reports it is missing or older than 7 days (`Ready.fallback_age_secs`; the client keeps no timer of its own). The account keeps the current and previous private halves, so messages already in flight to the old key still open. The one before that is dropped, so a fallback private key lives at most about two rotation periods.
- **Replay:** because the fallback key is reusable, a replayed first message could try to create a duplicate session. The receiver matches the message's session id against its existing sessions first, so replays are refused (tested).
- **Preference:** one-time prekeys are always handed out first; the claim reply says whether the key was the fallback.

## Trade-off accepted
A session started with the fallback key has weaker forward secrecy for its **first message** than one started with a one-time key: if the device's account is stolen while that fallback private key is still held (up to about two weeks), the first message of sessions started through it can be decrypted. Later messages are protected by the ratchet as usual. This is the standard "last resort key" compromise; it is what keeps the device reachable, and it only applies when one-time keys ran out.

## Remaining gaps
- Drained one-time prekeys still degrade first-message forward secrecy for new contacts until the device reconnects. The per-connection claim cap slows this but a reconnecting attacker resets it; a per-recipient or per-IP rate limit on the relay is future work.
- A malicious relay could withhold the fallback (then no sessions can start) or serve a stale one; neither reveals content.
