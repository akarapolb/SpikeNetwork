# Mobile client prototype

Clickable design prototype of the Spike Connect mobile app: nine phone screens (390×844).
Live canvas (private, owner-only until shared): https://claude.ai/artifact/MvZutgjnWvxnDPEN3BTRJi

**Status: design only.** No app code exists yet. Multi-device and agent features shown here are proposed, not built (see `../multi-device-design.html` and `../engineering-plan.md`).

## Screens
| File | Screen | Notes |
|---|---|---|
| `Onboarding.dc.html` | 1 · Create identity | No phone number or email; passphrase protects keys on the phone; biometric unlock option |
| `Main.dc.html` | 2 · Chats | Sync and relay status chips; contacts marked verified, unverified, on-device assistant, business agent |
| `Conversation.dc.html` | 3 · Conversation | "Contact added a device" notice; per-device delivery state; old attachment fetched from your laptop on demand |
| `Verify.dc.html` | 4 · Verify a contact | QR and safety number; contact's signed device list |
| `Devices.dc.html` | 5 · Your devices | Device states, identity key, removal, "backup not available yet" |
| `LinkDevice.dc.html` | 6 · Link a new device | Interactive history choice: all text + recent attachments (default), everything, last 30 days, start fresh |
| `Network.dc.html` | 7 · Network and privacy | Home relay (e.g. Spike Box), what a relay can and cannot see, direct connection, prekey readiness |
| `AgentRequest.dc.html` | 8 · Agent approval | Interactive approve/deny for a payment over the agent's limit; scopes; signed request; expiry |
| `AgentLog.dc.html` | 9 · Agent activity log | Interactive filters; hash-chained log, no message content |

## Format
The `.dc.html` files are Design Component pages for the claude.ai Design canvas; `canvas.json` lays them out. They need the canvas runtime (`support.js`), so they do not render correctly when opened directly in a browser. To view or edit, use the canvas link above. To change the design, edit the canvas there (or edit these files and republish them to the canvas), then copy the updated files back here.

## Decisions these screens assume
- Platform-neutral design (not iOS- or Android-specific yet).
- Contacts are **notified** of a new device, not asked to approve it (`Verify`, `Conversation`).
- **No backup** in the first multi-device release (`Devices`).
- Agent approval threshold, scopes and expiry come from Phase 2 of the roadmap.
Sample names, the agent and the hotel are fictional.
