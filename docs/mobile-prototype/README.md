# Mobile client prototype

Clickable design prototype of the Spike Connect mobile app: eleven phone screens (390×844).
Live canvas (private, owner-only until shared): https://claude.ai/artifact/MvZutgjnWvxnDPEN3BTRJi

**Status: design only.** No app code exists yet. Multi-device and agent features shown here are proposed, not built (see `../multi-device-design.html` and `../engineering-plan.md`).

## Screens
| File | Screen | Notes |
|---|---|---|
| `Onboarding.dc.html` | 1 · Create identity | No phone number or email; passphrase protects keys on the phone; biometric unlock option |
| `Main.dc.html` | 2 · Chats | Add-contact button, message-requests row, sync and relay status chips; contacts marked verified, unverified, on-device assistant, business agent |
| `Conversation.dc.html` | 3 · Conversation | "Contact added a device" notice; per-device delivery state; old attachment fetched from your laptop on demand |
| `Verify.dc.html` | 4 · Verify a contact | QR and safety number; contact's signed device list |
| `Devices.dc.html` | 5 · Your devices | Device states, identity key, removal, "backup not available yet" |
| `LinkDevice.dc.html` | 6 · Link a new device | Interactive history choice: all text + recent attachments (default), everything, last 30 days, start fresh |
| `Network.dc.html` | 7 · Network and privacy | Home relay (e.g. Spike Box), what a relay can and cannot see, direct connection, prekey readiness |
| `AgentRequest.dc.html` | 8 · Agent approval | Interactive approve/deny for a payment over the agent's limit; scopes; signed request; expiry |
| `AgentLog.dc.html` | 9 · Agent activity log | Interactive filters; hash-chained log, no message content |
| `AddContact.dc.html` | 10 · Add a contact | Interactive tabs: show my code (in-person scan verifies both), scan, invite link (one-time, 7 days, arrives unverified); optional username on your home relay |
| `Requests.dc.html` | 11 · Message requests | Interactive accept / decline / block; unknown senders' messages and links stay hidden until accepted |

## Colour and typed actions (2026-10-19)
The files here now follow the same quieter colour rule as `../desktop-prototype/`: colour means status only (teal for actions and selection, amber only for "waiting for you", red only for danger, green only as a small verified or connected mark); avatars and tags are neutral; scope chips use ✓ / ✕. The agent approval screen (`AgentRequest`) now headlines the signed typed fields ("Pay €286.00 to harbor-travel", ADR 0017) and shows the agent's own words as "Agent says (not verified)".

The live canvas linked above was updated with these files on 2026-10-19 (version 14).

## Message requests and blocking (2026-10-20)
The message-requests screen is now implemented in the desktop app (ADR 0021). One claim in the first version of this design was wrong and has been corrected: **blocking cannot happen at the relay**, because a relay only sees the recipient of a message, not who sent it. Blocking drops a sender's messages on your devices as they arrive. Declining or blocking also deletes that conversation attempt's session, so the sender's later messages in it cannot be read. The live canvas still carries the old wording until the next republish.

## Format
The `.dc.html` files are Design Component pages for the claude.ai Design canvas; `canvas.json` lays them out. They need the canvas runtime (`support.js`), so they do not render correctly when opened directly in a browser. To view or edit, use the canvas link above. To change the design, edit the canvas there (or edit these files and republish them to the canvas), then copy the updated files back here.

## Decisions these screens assume
- Platform-neutral design (not iOS- or Android-specific yet).
- No phone-number or email contact discovery; people are added by QR code, invite link, or an optional username.
- Messages from unknown senders go to a **requests** inbox (the current code drops them; needs relay-side limits too).
- Contacts are **notified** of a new device, not asked to approve it (`Verify`, `Conversation`).
- **No backup** in the first multi-device release (`Devices`).
- Agent approval threshold, scopes and expiry come from Phase 2 of the roadmap.
Sample names, the agent and the hotel are fictional.
