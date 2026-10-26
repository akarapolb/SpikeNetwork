# ADR 0031: Replies and reactions

Status: built, 2026-10-26: core, CLI, app bridge, desktop and Android screens, for one-to-one chats, small groups
(ADR 0024) and large groups (ADR 0025). Not reviewed.

## Problem

People answer a particular message ("which one?") and acknowledge things without writing a reply. Both are
expected in a messenger, and both have to work across every device of every person, like edits and deletes
(multi-device design, "Keeping devices in step").

## Decision

**A reply is a normal text message with one more field.** `Envelope.reply_to` (optional, `core/src/envelope.rs`)
holds the envelope id of the message it answers. Only a `Text` may carry it, and it must be an id (32 hex).
Older versions ignore unknown fields, so they show the text without the quote: nothing breaks. The id is stored
on the message (`StoredMessage.reply_to`) and travels with it to your other devices (catch-up), to new members
(group history, `HistoryMsg.reply_to`) and through MLS. The app turns it into a short quote when it lists a
conversation (`MessageDto.reply`: who wrote the original, its first 80 characters, or that it was deleted).
A reply can only point at a message of the same conversation that is still there (`Session::reply_target`).

**A reaction is a new kind, `React`.** `target` is the message, `body` is one emoji (at most 32 bytes, no spaces
or control characters), or empty to take the reaction back. Anyone in the conversation may react to any message,
including their own. Each person has at most one reaction per message: it is stored on the message
(`StoredMessage.reactions`, keyed by the person's owner key) with its `(lamport, device)` stamp, and a reaction
only replaces that person's earlier one when its stamp is newer (`spike_core::merge::reaction_wins`). A
taken-back reaction stays as an empty entry, so an older reaction arriving late cannot bring it back. Every
device therefore ends with the same reactions, whatever order they arrive in. A reaction to a message that is
not there, is in another conversation, or was deleted changes nothing.

Reactions are not messages: they never appear in a history list, do not count as unread, and raise no
notification banner. The app tells the screen that something changed (`SyncDto.changed`, also counted for
edits and deletes) so it redraws.

## Alternatives considered

- *A reaction as a text message with a `reaction_to` field.* Older versions would show a stray emoji message
  in the chat. A new kind is silently dropped by them instead (`Outcome::Invalid`), which is the better failure.
- *Reactions as a list that only grows.* Taking one back, or changing it, would then need a delete of each
  earlier one. One entry per person with a stamp is smaller and converges by the same rule as edits.
- *Replies by quoting the text into the body.* Simple, but the quote cannot follow a later edit or delete of
  the original, and it costs bytes in every copy.

## Known gaps

- In a large group, a version without this change drops a reaction as "a message that is not one of ours" and
  counts it as refused (`large.rs` `refuse`). Groups should upgrade together. From this version on, an
  envelope of a kind the build does not know (`Envelope::is_newer_kind`) is skipped quietly instead, so the
  next new kind will not repeat this.
- Reactions on files work like on text; reacting from the notification is not built.

## Later changes

- 2026-10-26: a reaction that arrives before its message (possible in a small group when the reaction
  overtakes the message on another member's path) waits for it, sealed in the history database
  (`early_reactions`, at most 500, oldest dropped first), and joins the message when it is stored, if that
  message is in the same conversation and not deleted. Catching up from another of your devices
  (`merge_record`) now also merges reactions on messages both devices already have.
