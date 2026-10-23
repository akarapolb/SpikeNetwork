# Desktop client prototype

One self-contained file, `index.html` (vanilla JS, no build step; fonts from Google Fonts). Open it in a browser and unlock with any passphrase of 10+ characters.

**Status: design only.** Invented people, no cryptography, nothing is sent. It exists to agree on the screens before the Tauri app (planned ADR 0019) is wired to `spike-core`; the markup and styles are meant to become that app's frontend.

## Aligned with the mobile prototype
Same design system as `../mobile-prototype/`: IBM Plex Sans and Mono, Schibsted Grotesk headings, accent `#0B6B73` with the same neutrals and status colours, the same four sections (Chats, Agents, Devices, Settings), the same people and copy (Alice Moreau, Bob Chen, Harbor Travel, Spike Assistant, Carol Reyes, Dana Kim, Evan Brooks), and the same decisions (no phone-number lookup, message requests inbox, contacts notified of new devices, no backup yet). A dark theme is added (derived values; mobile has none yet).

**Colour is quieter than mobile, on purpose.** Colour means status only: teal for actions and selection, amber only for "waiting for you" (the approval card border, the Agents badge), red only for danger ("Not verified", Remove, a mismatch), and green only as a small verified or connected mark. Avatars are neutral; people are round, agents and the assistant are rounded squares. Scope chips use ✓ / ✕ instead of green and red fills. **The mobile prototype should get the same pass.**

Desktop differences: an icon-only left rail (tooltips, Ctrl+1-4, Ctrl+L to lock) instead of a bottom tab bar, a list column plus a detail pane, "this laptop" instead of "this phone", Windows Hello instead of fingerprint, and an idle auto-lock setting.

## Screens (mobile screen number in brackets)
| Screen | Where |
|---|---|
| Create identity [1] and unlock | First screen; "First time here?" switches between them |
| Chats [2] | Rail: Chats. Sync and relay chips, search, message-requests row, conversation list with tags |
| Conversation [3] | Bob Chen: new-device notice, attachments stored here or on another device, per-device delivery |
| Verify [4] | "Review" or "Safety number" in a conversation; Dana Kim shows the unverified case |
| Devices [5] and link a device [6] | Rail: Devices. Three-step link wizard with history choices and size estimates |
| Network and privacy [7] | Rail: Settings |
| Agent approval [8] | Rail: Agents > Harbor Travel, or the card inside the Harbor Travel chat |
| Agent activity log [9] | Rail: Agents > Activity log, with the same four filters |
| Add a contact [10] | The add-person button above the chat list: My code, Scan, Invite link |
| Message requests [11] | The dashed row in the chat list |
| Group conversation (new) | "Pilot team" in the chat list: sender names, system lines (who added whom), an unverified-member banner, per-group delivery counts |
| Group members and settings (new) | "Members" in a group: rename, add (from contacts), remove, make admin, verify a member, who can add people, what new members see, leave |
| Call (planned) | Phone and video buttons in a one-to-one conversation. Marked "planned, not built" (ADR 0018 draft): ringing, the identity check on call keys, direct vs relayed path, relay-only setting, mute, camera, hang up. Group conversations show the buttons disabled |
| New group (new) | The new-group button above the chat list: name, pick members, settings |

## Groups: decisions this design assumes (agreed and built in the desktop app: ADR 0024; the 7-day history option and the agents card are not in the app yet)
- **Small groups first, up to 20 people,** built on today's code: each message is encrypted separately for every member's devices over the existing one-to-one sessions. No new cryptography. Membership is kept by each member's app and is not yet one signed list; the screen says so. MLS (large groups, signed membership) stays in Phase 3.
- **Admins add people by default;** a group can switch to "any member".
- **New members see messages from when they join by default;** optionally the last 7 days, sent encrypted by the device that adds them.
- **Unverified members are visible to everyone:** a banner in the conversation, a tag on their messages, and a count in the header.
- **Agents can be members and post,** but their approval requests go privately to the person they work for, never to the group.
- Removing someone stops new messages reaching their devices; they keep what they already received.

## Changed from the mobile approval screen (ADR 0017)
The mobile screen headlines the agent's own words ("Book Hotel Lumen, Lisbon"). Since typed actions exist, the desktop headline is rendered from the signed fields ("Pay 286.00 EUR to harbor-travel"), and the agent's words move to an "Agent says (not verified)" line. Free-text requests show a warning that only the scope and amount were checked. The grant card adds the per-request, 24-hour and rate limits and the "Accept typed actions only" switch. **The mobile prototype should get the same change.**

## Mapping to the code (for the Tauri step)
Unlock -> `Home::unlock`; Chats -> `Home::messages().conversation`, `ops::send`, `ops::recv`; Agents -> `agent::list_inbox / list_remote / list_grants`, `ops::agent_decide`, `Request::summary()`; Log -> `agent::verified_audit`; Devices -> `link` module.

## Not built in the code yet (shown anyway, as in mobile)
Message requests inbox (the code drops unknown senders), invite links and usernames, direct connections, Windows Hello, idle auto-lock, the on-device assistant, the Box relay naming.
