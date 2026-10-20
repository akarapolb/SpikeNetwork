# ADR 0021: Message requests

Status: **Proposed** (implemented in the same change)
Date: 2026-10-20

## Problem
A first message from someone who is not a contact is dropped before any session exists (`Opened::UnknownSender`), and even if it were kept, nothing in it says who sent it: a pre-key message carries only the sender's device key. So today two people can only talk after **both** have added the other's card by hand. The mobile and desktop designs show a "message requests" inbox (someone uses your invite and writes first; you accept, decline or block), and the code does not do it.

## Decision
**The first message of a new session carries a signed introduction, and a stranger's first messages wait in a quarantine inbox until you decide.**

1. **Introduction.** When a sender starts a new session with any of the recipient's devices, the message envelope carries `intro`: the sender's identity card (the owner-signed device list, as `spike card` prints it). It is the same information a contact already has, so nothing new is disclosed to someone you chose to message. Envelopes without `intro` are unchanged.
2. **Binding.** The receiver accepts an introduction only if the card is validly signed by its owner key **and** lists the device whose key actually encrypted the message. A stranger therefore cannot claim someone else's identity card: they cannot have a device in that signed list. Anything without a valid, bound introduction is dropped as before.
3. **Quarantine.** A valid first message creates its session in a **separate sealed store** (never the main session store) and the message is held in a sealed `requests` list: sender fingerprint, their card, and up to 5 text messages of at most 4 KiB each. **Attachments are stripped** (only the text is kept). Nothing from a request is shown as a conversation, nothing is acknowledged back to the sender (no read receipt), and the UI never turns text into links.
4. **Decide.**
   - **Accept (with a name you choose):** the card becomes a contact (**not verified**), the quarantined session moves into the main store, and the held messages enter that conversation's history.
   - **Decline:** the request and its session are deleted. The sender is not told. Deleting the session keys means the sender's later messages in that same session can no longer be read (they arrive as undecryptable and are dropped); they would have to start a new session for a new request.
   - **Block:** as decline, and the sender's device keys are added to a sealed block list. Later messages from those keys are dropped **before any session is created**, so a blocked sender costs nothing.
5. **Limits.** At most 50 pending requests, 5 held messages per request, and held requests expire after 30 days. Beyond the caps a first message is dropped (and acknowledged, so the relay does not redeliver it).
6. **Delivery.** Dropping or holding a message acknowledges it to the relay, like every other outcome.

## What this does not do (honest list)
- **Blocking is local.** The relay only sees the recipient device, not the sender, so it cannot refuse a blocked sender's messages. Your devices drop them on arrival; the relay still carries and queues them until then. (The mobile prototype claimed the relay drops them; that wording is corrected.)
- **A sender needs your card first.** Device ids are random 256-bit values, so strangers cannot find you: a request means someone was given your card or invite link. A leaked card means spam; block is the answer.
- **Spam still costs a one-time prekey each.** An attacker who knows your card can drain one-time prekeys faster than they are refilled; the fallback prekey keeps you reachable (ADR 0006), and the per-connection claim cap (20) slows it. No per-sender rate limit exists on the relay yet.
- **The introduction is not verification.** Accepting adds the person as *not verified*; compare safety numbers as for any contact.
- **Decline and block both end that session for good.** Neither keeps state for a declined stranger (that would be unbounded), so after either, the sender's old session is unreadable even if you later unblock them. The sender's app is not told and keeps sending messages that wait at the relay until they expire.
- **Existing sessions are not retro-fitted.** Contacts you already have work as before.

## Alternatives considered
- **Hold the raw ciphertext and decrypt on accept.** Avoids creating a session early, but the first messages cannot be shown (no preview of what they want) and the relay's copy would have to be acknowledged anyway, so the ciphertext must be kept either way. A quarantine session is simpler and equally contained.
- **Accept strangers straight into the main store.** Bounds nothing: any card holder could create unlimited sessions and conversations.
- **Require the sender to be in a directory.** There is no directory (by design).

## Tested
`cli/tests/requests.rs` and `desktop/src-tauri/tests/api.rs`: a stranger's introduced message becomes a request and creates no contact or history; accept, decline and block; an introduction signed by someone else or naming a different device is refused; attachments are stripped; the per-request cap and expiry hold; blocked senders are dropped before a session is created; follow-up messages from a pending sender are held in the same request; after a decline the old session is unreadable but a different stranger can still write.
