# ADR 0024: Small groups over pairwise sessions

Status: **Accepted 2026-10-23: pairwise first, MLS later.** Slice 1 (core, `core/src/group.rs`) built.
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
Being in the same group is a reason to accept a session: a first message (with the ADR 0021 introduction) from a device of a member of a group we are in goes **straight to that group**, not to message requests. The member is shown with their name from their own card, "not in your contacts" and **not verified**; the prototype's unverified banner and per-message tag apply. You can verify or add them as a contact from the member list. Leaving every shared group puts them back to "stranger" for future sessions.

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

## Decisions (2026-10-23)
1. **Pairwise first, MLS later.** Sender keys (Megolm via `vodozemac`) were considered as a middle step and rejected: they need the same relay fan-out work as MLS, removal re-keying grows with the square of the group size, and every group would migrate twice. Larger groups wait for an MLS ADR, preceded by a short `openmls` spike (Windows and Android builds, add/remove, what it needs from the relay).
2. **Cap of 20 people.**
3. **Adds by non-contacts go to message requests** (section 6).
4. **Admins are equal.**
5. **`week` history in a later slice.**
