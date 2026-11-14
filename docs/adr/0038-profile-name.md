# ADR 0038: A name you offer people

Status: **Proposed** (implemented in the same change)
Date: 2026-11-14

## Problem
Spike Connect has no profile name: whoever adds you types a name for you. The design's Settings > Identity has a
**Name** row: "Shown to people you add. Each contact can rename you." It should be the suggested name when someone
accepts your first message. A name is personal data, and the relay must never learn it (ADR 0021: a relay sees which
device a message is for, when and how large, nothing else).

## Options
1. **In the identity card.** Anyone who adds you from a pasted card or a QR code would get the suggestion. But a card
   is made to be passed around: posted in a chat, printed, left in an invite link. Whoever holds it would read your
   name with it, and an old card would keep an old name forever. The card would also have to carry it unsigned (the
   signed device list is published to relays, ADR 0009) or the relay would store it.
2. **In the encrypted introduction (chosen).** When you write first to someone (ADR 0021), the envelope already
   carries `intro`, your card. It now also carries `intro_name`, the name you chose. The envelope is end-to-end
   encrypted, so only the person you wrote to reads it. That is exactly "shown to people you add".
3. **A profile message to every contact** whenever the name changes. More reach, but it tells every contact each
   rename, and it needs a new message kind and merge rules. Not needed for a suggestion.

## Decision
- The name is kept sealed on the device (`profile.enc`), set in Settings > Identity. Trimmed, at most 64 characters,
  no control characters; empty means no name.
- `Envelope.intro_name` is sent only together with `intro` (a new session with someone, or a group invite), and is
  refused on any envelope without `intro` or when it is not a valid name. Older versions ignore the field.
- A message request keeps the offered name. Accepting suggests it in the name field; the person accepting can change
  it, and contacts are still named by the person who has them. The request screen says "They call themselves …. Not
  checked." It is not verification: anyone can call themselves anything.
- The card is unchanged and carries no name. Adding someone from their pasted or scanned card gets no suggestion: you
  type the name, as before.

## What this does not do
- Contacts who already have you get no update when you rename yourself (they named you themselves).
- The name is per device for now: your other devices do not learn it, and a linked or restored device starts without
  one. Syncing it to your own devices (ADR 0013) is a follow-up.
- No name reaches a relay, a backup header, or a push notification.

## Tested
`core/src/envelope.rs` (a name only with an introduction, limits counted in characters), `cli/tests/requests.rs`
(the name a sender chose arrives with the request; invalid names are refused), `desktop/drive_identity.mjs` and
`android/drive_identity.mjs` (set the name in Settings, the other side's Accept suggests it, their own name wins).
