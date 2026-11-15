# ADR 0039: One-time invite links

Status: **Proposed** (implemented in the same change)
Date: 2026-11-15

## Problem
Today two people become contacts by scanning each other's code in person, or by pasting an identity card
(`spike2:...`) that one of them sent the other. A card is made to be passed around (ADR 0038): whoever holds it
can write to you for as long as it is valid, so posting it in another chat app or an email leaves a permanent
way in. The design shows "Share an invite link · One-time, valid 7 days. Arrives unverified." on Add a contact
and on the empty chat list: something you can send through any channel that works **once**, stops working after
a week, and leaves both people as contacts without either one pasting anything back.

## Decision
**An invite link is a key to a sealed card held by your relay. The relay hands it out once and deletes it; the
person who opens it introduces themselves with a one-time token, and your app adds them as a contact, not
verified.**

1. **Making a link.** The inviting device makes a random 16-byte invite id, a random 32-byte link key and a
   random 16-byte token. It seals `{card, offered name, token, expires}` with XChaCha20-Poly1305 under the link
   key (associated data: `spike-invite-v1` and the id) and stores the sealed blob on its home relay with
   `InvitePut { id, blob }`. It keeps `{id, token, expires}` in a sealed local list (`invites.enc`).
   The link is `spikeinvite1:<base64url(id ‖ key)>@<relay url>`. The link key never reaches the relay.
2. **The relay keeps it 7 days and hands it out once.** `InviteTake { id }` returns the blob and deletes it in
   the same transaction, so two people opening the same link at the same moment cannot both get it. After the
   take, or after 7 days, the relay holds nothing. A device may hold at most 20 open invites, of at most 16 KiB
   each.
3. **Opening a link.** The app connects to the relay named in the link **with a throwaway key made for this one
   request** (on an organisation's relay it signs in as a visitor, ADR 0035, which may take invites), takes the
   blob, opens it with the link key and checks it has not expired. It adds the card as a contact,
   **not verified**, under the name the inviter offers (or the one you type), then sends one `InviteHello`
   envelope carrying the token. Like any first message to someone new it carries your card and offered name
   (ADR 0021, 0038).
4. **Being found.** The inviter's device takes an `InviteHello` from a stranger only if the introduction is valid
   and bound to the sending device (ADR 0021) and the token matches one of its own invites that is unused and
   not expired. It then marks the invite used, adds the sender as a contact, **not verified**, under the name they
   offer (or "New contact", made unique), and moves the session out of quarantine. A hello with an unknown, used
   or expired token is dropped like an unintroduced stranger. From a contact it only uses the invite up.
5. **Second use fails.** The relay no longer has the blob, so a second person who opens the link is told "This
   invite link was already used or has expired" and learns nothing, not even your card.

## Security properties
- **Arrives unverified, on both sides.** The link travels through whatever channel you chose to send it through;
  whoever controls that channel could swap it for their own. The contact is trust-on-first-use until you compare
  safety numbers, and both apps show it as not verified.
- **One-time.** Enforced by the relay's atomic take. The inviter's token check is a second lock: a hello is
  accepted only once per invite, even if a link were somehow opened twice.
- **Expires in 7 days.** The relay deletes the blob at 7 days, the sealed payload carries its expiry and the
  opener checks it, and the inviter refuses a token after it.
- **A used link is harmless.** After the take it opens nothing: the relay has no blob, so a leaked old link
  reveals neither your card nor your name.

## What the relay learns
- Which of its devices made an invite, when, and the sealed blob's size. Not the card, name or token inside.
- When the invite was taken, and the network address that took it. Not who took it: the take uses a throwaway
  key, so the relay cannot link the taker to the device that then writes to the inviter.
- As for every first message, the hello's arrival at the inviter's mailbox (it never sees the sender, ADR 0001).

## What this does not do (honest list)
- **The link itself is a secret until it is used.** Anyone who reads it before your friend does gets in instead,
  as a contact who is not verified. If your friend's link fails, ask them to compare safety numbers with whoever
  did get in, and remove that contact.
- **Only the device that made the link knows the token.** Your other devices treat the newcomer as a message
  request the first time they write a text (ADR 0021), until contacts are synced between devices.
- **No cancel yet.** An unused link stays valid on the relay until it is used or expires.
- **The relay you share it from must be reachable by the person opening it.** A link names its relay.
- **Older apps** do not know `InviteHello` and drop it; a link from a newer app cannot be opened by an older one.

## Alternatives considered
- **Put the card in the link and enforce one-time use on the inviter's device only.** Needs no relay change, but
  the card leaks to everyone who ever sees the link, and a second opener only learns it failed after the inviter
  is online. The relay-held blob makes the second use fail at once and reveal nothing.
- **Take the invite on the opener's own signed-in connection.** Simpler, but tells the relay who accepted whose
  invite, a link the relay otherwise never sees.

## Tested
`relay` store tests (take once, expiry, per-device cap), `cli/tests/invites.rs` (a link opened once makes both
people contacts, not verified, with the offered names; a second opening fails; an expired invite fails; a hello
with a wrong token is dropped), and the drivers `desktop/drive_invite.mjs` and `android/drive_invite.mjs`.
