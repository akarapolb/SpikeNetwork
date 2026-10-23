# ADR 0024: Small groups over pairwise sessions

Status: **Accepted 2026-10-23: pairwise first, MLS later.** Slices 1 (core, `core/src/group.rs`), 2 (CLI, `cli/src/groups.rs`) and 3 (desktop screens) built; see "Changes while building" below.
Date: 2026-10-23

Groups exist only in the desktop and mobile prototypes. The desktop prototype assumes a design (`docs/desktop-prototype/README.md`, "Groups: decisions this design assumes"): up to 20 people, each message encrypted separately for every member's devices over the existing one-to-one sessions, membership kept by each member's app. This ADR turns that into something buildable and says where it is weaker than MLS. Nothing here changes the relay.

## Problem
1. **There is no group to address.** An envelope belongs to one conversation, named by `to` (the other person's owner key), and `StoredMessage.peer` is that person. A group needs its own id, and a stored message needs its author as well as its conversation.
2. **Membership must agree.** With pairwise fan-out each sender decides who receives a message. If members' apps disagree about who is in the group, they send to different people without noticing. The prototype says "if apps disagree you will see a warning"; something must make that true and make apps converge.
3. **Members who are not your contacts.** In a group of 20 you will meet people you never exchanged cards with. You need their devices to encrypt for them, and their first message must not land in message requests (ADR 0021) as if from a stranger.
4. **Consent.** Being put into a group must not be a way around message requests: a stranger must not be able to add you and start talking.

## Decision (proposed)

### 1. Group state: a small signed record, one chain per group
A group is a random 128-bit id plus a **state**:

| Field | Meaning |
|---|---|
| `group` | id (hex) |
| `version` | starts at 1, +1 per change |
| `prev` | SHA-256 of the previous state (none for version 1) |
| `name` | at most 100 characters |
| `members` | up to 20 entries: owner key (hex) and a relay hint (where their device list can be fetched) |
| `admins` | subset of `members`, never empty |
| `add_by` | `admins` (default) or `any` |
| `history` | `join` (default) or `week` |
| `author` | owner key of the member who made this change |
| `sig` | that member's owner signature over all of the above |

The state is signed with the **owner key** (devices hold it, ADR 0010), so it can be checked by anyone and forwarded by any member, the same way device lists are.

**A state is valid** if its signature verifies, its shape is within limits, its `prev` is a valid state we hold, and the change from `prev` is allowed for `author` in `prev`:
- an admin may change anything (add, remove, make or unmake admin, rename, settings);
- any member may add people if `add_by = any`, and may change nothing else;
- any member may **remove only themselves** (leave);
- a state always has at least one admin, so the last admin cannot leave without making someone else admin (the app picks the longest-standing member).

Version 1 is valid if its author is its only admin and is a member.

**Concurrent changes.** Two admins may both build on version *n*. Every app picks the same winner: the valid state with the **highest version**, ties broken by the **lowest hash**. The losing change is not applied; its author's app sees that and offers to redo it on top of the winner. Because the choice depends only on the set of valid states, members who have seen the same states agree, whatever order they arrived in.

**Missing states.** A member who receives a state whose `prev` it does not hold asks the sender for the chain (`group_state_request {group, from_version}`) and holds the message until it arrives (capped, see limits). Groups change rarely and are small, so each app keeps the whole chain.

### 2. Envelopes and storage
- `Envelope` gains `group: Option<String>` and `gstate: Option<String>` (hash of the state the sender used). With `group` set, `to` is ignored.
- New kind `group_state`: body is one signed state (or a short chain when answering a request). New kind `group_state_request`.
- `Text`, `Edit` and `Delete` work in groups unchanged; edits and deletes keep the ADR 0012 author rule and must target a message **in the same group**.
- `Sync`, `AgentRequest` and `AgentReceipt` are **refused with `group` set.** An agent may be a member and post text; its approval requests stay one-to-one with its principal (as the prototype says).
- `StoredMessage` gains `author: Option<String>` (sender's owner key, set for group messages). For a group message `peer` holds the group id, so existing per-conversation queries keep working. Own-device catch-up (ADR 0013) carries group messages and group states like any other record.

### 3. Sending
`send` to a group fans out (ADR 0009) to every device of every member in the **current** state except the sender's own person, plus sent copies to the sender's other devices. Each message carries `gstate`. Device lists of members who are not contacts are fetched from their relay hint and checked against their owner key exactly as for contacts. The delivery report counts people and devices, as the prototype shows ("Delivered to all 3 members (5 devices)").

### 4. Receiving
A group message is accepted only if:
1. we are a member of that group in our current state, and
2. the sending device belongs to the owner key of a member (checked against that member's signed device list), and
3. the sender is a member of our **current** state (otherwise a removed member could keep posting by naming an old state; a message sent just before a removal and delivered after it is dropped, which is the price), and
4. `gstate` is our current state, an earlier state on its chain in which the sender was a member, or a state we do not hold yet (then: request it, hold the message).

If `gstate` names a state that loses to ours (a fork), the message is shown with a **"sent with an out-of-date member list"** note and our current state is sent back to that sender. This is the warning the prototype promises, and it also repairs the fork.

### 5. Members who are not your contacts
Being in the same group is a reason to accept a session: a first message (with the ADR 0021 introduction) from a device of a member of a group we are in goes **straight to that group**, not to message requests. The member is shown with their name from their own card, "not in your contacts" and **not verified**; the prototype's unverified banner and per-message tag apply. You can verify or add them as a contact from the member list. *(Corrected while building, item 12: someone you have talked to in a group stays known after you leave it, so their private messages are refused with a hint rather than treated as a request.)*

### 6. Consent to being added
When a state adds you:
- **added by a contact** → the group appears with "Carol added you" and a Leave button;
- **added by someone who is not a contact** → it appears in **message requests** (as "Group invite from …"); nothing from the group is shown until you accept; decline or block works as in ADR 0021;
- **after you left a group** → a later state adding you back is always an invite, never automatic.

Your app sends nothing to a group until you are in it by these rules.

### 7. History for new members
`join` (default): new members see messages from when they join. `week`: the device that adds someone sends them the group's last 7 days in batches (the ADR 0013 batch format), encrypted for them only. Those messages are shown as **"shared by Carol when you joined"**: the newcomer cannot check that other authors really wrote them, because pairwise sessions do not let a third party verify a message. **Proposed: build `join` first and `week` in a later slice.**

## What this does not give you (honest list)
- **No proof everyone got the same message.** A sender chooses its recipients; a malicious member can send a message to some members and not others (a "split view"). `gstate` shows *which member list* the sender claims to have used, not that it used it. In MLS one ciphertext goes to the whole group, so a sender cannot quietly give different members different messages (a delivery service can still withhold). Here membership is fixed only by signatures and a convergence rule.
- **Removed members keep what they received**, and any member can always copy messages elsewhere. Removal stops *new* messages reaching their devices.
- **Cost grows with devices.** 20 members × up to 16 devices is 320 encryptions and relay messages per send in the worst case; a typical group is 20 × 2. That is why the cap is 20.
- **The relay learns more than for one-to-one.** It sees one device send N messages at the same moment, which hints at group size. The contents, the group id and the member list stay encrypted.
- **No group calls.** ADR 0018: group calls need group keys (MLS).
- **Admins are only as trustworthy as their devices.** A stolen admin device can rewrite membership; other members see every change as a system line with its author.

## Alternatives considered
- **MLS now (`openmls`).** Sound group keys and signed membership, the right end state, but a new protocol layer, a delivery service with ordering rules, and a new key-package flow, before any group exists. The roadmap keeps MLS for Phase 3; the state record here (members, admins, settings) maps onto MLS group context later, so the UI and storage do not change when the transport does.
- **Unsigned membership kept by each app** (the prototype's wording). Simplest, but nothing converges and any member could claim anything; the signed chain costs little more.
- **Lamport merge of individual membership operations.** Works for adds and removes, but admin rights depend on order, and a merge of "remove Carol" with "Carol removes Dana" has no good answer. A total order over whole states is easier to reason about.
- **A server-held member list.** The relay would learn group membership. Rejected.

## Build order (proposed)
1. **Core:** state type, signing, validation rules, winner rule, chain storage; unit tests for every rule above, including forks and missing states.
2. **CLI:** `spike group new|add|remove|admin|rename|leave|list|show`, sending and receiving group messages, invites through message requests; integration tests with three people and multiple devices, including a non-contact member, a fork between two admins, and a removed member who receives nothing new.
3. **Desktop:** the prototype's group screens on top of the API.
4. Later: `week` history, mobile.

## Changes while building (2026-10-23)
Building the CLI changed or settled these points. Where they differ from the sections above, this list wins.
1. **New members start from a checkpoint, not the whole chain.** A whole chain does not fit one envelope after a few dozen changes, and a newcomer cannot judge the history anyway: it has never seen the earlier members. The state that adds you is your starting point (`Chain::from_checkpoint`): its signature and shape are checked, and its author must be in it. Earlier states are ignored (`BeforeStart`). Members who were already there keep the chain from version 1, and answers to "send me the states after version n" are split into batches that fit an envelope. Cost: if a branch that started before your checkpoint later wins elsewhere, you cannot follow it. That needs two admins to keep editing on both sides of a fork, and it shows as a member-list warning.
2. **An invite from someone who is not a contact is kept with the group, not in the message-request list.** `spike group list` shows it as INVITED, and `spike group accept` / `decline` answer it. It is still introduced and bound exactly as a request (ADR 0021), its session waits in quarantine, nothing is shown before you accept, and at most 20 such invites wait at once. Accepting moves the inviter's session out of quarantine and keeps their card as a group member's, not as a contact.
3. **A member who is not a contact cannot write to you one to one.** Their private messages are refused ("add them to talk one to one"). Their group messages work.
4. **Members' devices are fetched from the relay named in the member list** when you send, and when a message arrives from a device you do not know. A message from a brand-new member that arrives *before* the state that added them is dropped: you do not know them yet. On one relay that order cannot happen (the state is sent first); across relays it can.
5. **Replies to other members wait in a sealed outbox** (`group-outbox.enc`) and are sent by `recv` after it has acknowledged what it received, so a failed reply is retried and never blocks receiving.
6. **A tombstone binds only its author.** In a group, a member sees a message before you might. If someone else's delete for that id arrives first, the real message replaces the tombstone instead of being suppressed by it.
7. **A delete for a message you never had leaves a hidden marker.** ADR 0012 keeps a tombstone when a delete arrives before its message, so a late copy cannot bring it back. In a group, people who joined later receive deletes for messages from before they joined, and the marker showed up as a stray "Message deleted" line from someone they do not know. The marker now has time 0 (`groups::is_ghost`): it still blocks a late copy, and every group listing skips it. A message you did have still shows as "Message deleted".
8. **Own-device sync and device linking carry groups.** Found by driving the desktop app, not by the CLI tests: own-device catch-up checked every record's conversation as a 64-hex owner key, so one group message made the whole batch invalid (for ordinary chats too); read markers had the same check. Both now accept a group id (32 hex) together with an author. Device linking copied group messages but not the groups, so a newly linked device had history and no membership. The link bundle now carries every group you are in or left, with its signed chain (checked again on the new device), and the cards of members who are not contacts. An invite that waits for you is not copied: its session lives on the device that received it.
9. **Desktop screens** (slice 3): groups share the chat list with contacts (newest first); a group thread shows each sender's name with a "Not verified" mark and a banner when members are unverified; a members screen (rename, add from contacts, remove, make admin, who can add people, verify a member, keep a member as a contact, leave); a new-group screen; an invite screen for adds by someone who is not a contact (nothing is shown until you join). Unread counts, the window title and the taskbar include groups and invites. Left groups with a history stay listed, read-only. Edit and delete work on your own group messages. The "last 7 days" option for new members is not shown, because it is not built.
10. **Invite decisions follow you, and an invite can be blocked.** Accepting, declining or blocking an invite on one device is sent to your other devices (`SyncMsg::GroupInvite`, over the existing own-device channel), which settle the same invite: accept moves the inviter's waiting session into the main store and joins, decline and block leave. Telling the others is best effort; offline, they decide for themselves. If a device hears a decision before its own copy of the invite arrives (a faster relay), it keeps the decision for 30 days and applies it the moment the invite lands. **Block** means: decline, add the inviter to the block list (the same list as message requests, shown in Settings), and decline every other invite still waiting from them. It exists only for an inviter who is not a contact, because a contact's messages are never dropped and a block would do nothing; for a contact's invite the app offers Decline only. From then on the blocked person cannot invite you, **even if you share a group with them** (their first messages are dropped before a session exists; an invite from a known member is dropped by the same rule). Messages in a group you already joined with them keep arriving: leave it if you want them gone.
11. **Flaws in slice 2 found by building this** (each now has a test that fails without the fix):
    - *Members of a group you were only invited to were treated as known.* Their cards were fetched, so the inviter's later messages went through your main session store, bypassing the quarantine that holds until you accept. Cards are now fetched only for groups you joined.
    - *A second invite from the same stranger was lost.* Olm keeps sending first-style messages until you reply, and they carry no introduction card. A later invite now reuses the card kept with the earlier one (`introduction_for`), and several invites wait side by side on one quarantined session. Declining one no longer deletes a session another waiting invite still needs.
    - *An accept applied from another device was overwritten.* Adopting the inviter's session wrote the main session store on disk, then the receive loop saved its older in-memory copy over it, and later messages failed to decrypt. Adoption now happens on the in-memory sessions while receiving (`accept_into`, `apply_invite_decision_in`) and the loop reloads them after a decision that settled on arrival.
12. **A former member stays known (corrects section 5).** I first made people you only know from groups you left strangers again. It cannot work: their old session lives in your main store, so a first message treated as a stranger's goes down the quarantine path and cannot be read. They stay known: a private message from them is refused with "add them to talk one to one", in the group and after you leave it, and works once you add them as a contact.
13. **Files in groups.** A file is encrypted once under one key and uploaded once to the sender's relay; the descriptor (key included) goes to every member inside the same end-to-end encrypted message that carries the caption. Each member fetches that one blob, so cost to the sender does not grow with the group. The checks are the one-to-one ones (per-chunk authentication, size and hash after reassembly). Limits: 100 MiB; the relay keeps a blob for its retention time (30 days by default), so a member who stays offline longer cannot fetch it, and there is no group-wide "ask the sender again" yet (your own devices can re-supply each other, ADR 0013). Deleting a message deletes the file on every device that kept it.
14. **The "last 7 days" for new members is built, as a request, not a push.** I first sketched the adder pushing history. Pulling is simpler and safer: when a new member joins (for an invite from someone who is not a contact, when they *accept*; nothing is stored before), their app asks the person who added them, if the group's setting is `week`. That person's app answers once per requester per day, only if the requester is a member and the group still offers it, with the messages it holds from the last 7 days in batches that fit an envelope. The receiving app takes history only from the person it asked, only for a week after asking, only while the group still offers it, and drops anything outside the last week, claiming to be from you, or already held. Deleted messages are not sent; a message that was itself only shared with the sender is not passed on (that would chain claims nobody can check); a file is re-uploaded under a fresh key when the sender still holds it (up to 20 files and 50 MiB in total), otherwise the message says the file was not included. Shared messages are stored with who shared them and shown as "shared by X when you joined". **They are only as reliable as that person:** pairwise sessions cannot prove who wrote what, so a malicious sharer could invent messages and attribute them to someone else; the marking and a tooltip say so. A real edit or delete from the claimed author still applies (shared messages carry clock 0). If the adder has several devices, each answers and the duplicates are ignored. Changing the setting only affects people who join afterwards.
15. **Not built yet:** group calls, asking the sender again for an expired file, and a group-wide read state.

## Tested (`cli/tests/groups.rs`, `core/src/group.rs`, `core/src/envelope.rs`)
Three people where two are not contacts talk through the group, each stored with its author and nothing going to message requests; a removed member gets nothing new, cannot send, and a hand-made message naming her old member list is refused; two admins change the group at once and all three apps converge on the same state, and a message from the losing side is shown marked while its sender catches up; an add by someone who is not a contact is an invite, shows nothing before you accept, and works after; leaving and being added back is an invite even from a contact, and declining keeps you out; only the author can edit or delete a group message; a message naming a state you missed waits, the state is fetched from its sender, and then the message is shown; a group follows you to your other devices (joined there, sent copies, sending from the phone, read state); catching up from another of your devices accepts a batch that holds group messages (it fails with the old check); a device linked later receives your groups, their history with authors and the cards of members who are not contacts, and can send and read; a delete for a message you never had leaves no visible trace while a known deleted message still shows. Files and history (`cli/tests/groups.rs`): a 300 KB file (more than one chunk) reaches every member, each fetches and checks it, a member who does not know the sender can fetch it, the sender keeps a copy, and a delete removes it; a new member of a `week` group receives exactly the messages from the last 7 days that were not deleted, with the sharer marked, the claimed authors kept, a file that still downloads, and nothing older; a `join` group shares nothing; history nobody asked for is refused (even when the group now offers it), a second request within a day is not answered, only an admin can change the setting; accepting an invite is what asks. Removing either the "asked" check or the daily limit makes a test fail. Invite decisions: accepting and declining on one device settles the other and the group works on both; a decision that arrives before its invite is remembered and applied on arrival; blocking an inviter blocks them on both devices, drops later invites, clears every other waiting invite from them, and still holds when you share a group with them; blocking a contact's invite is refused; several invites from one stranger arrive together. `desktop/src-tauri/tests/api.rs` repeats the main flows through the app's own functions (names, unread, attention, invites, removal, settings), and the real app was driven with four instances over its debug port: create, write, members and settings, add through the picker, invite and join, remove, edit, delete and leave.

## Decisions (2026-10-23)
1. **Pairwise first, MLS later.** Sender keys (Megolm via `vodozemac`) were considered as a middle step and rejected: they need the same relay fan-out work as MLS, removal re-keying grows with the square of the group size, and every group would migrate twice. Larger groups wait for an MLS ADR, preceded by a short `openmls` spike (Windows and Android builds, add/remove, what it needs from the relay).
2. **Cap of 20 people.**
3. **Adds by non-contacts go to message requests** (section 6).
4. **Admins are equal.**
5. **`week` history in a later slice.**
