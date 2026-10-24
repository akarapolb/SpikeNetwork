# ADR 0029: Backup and recovery of an identity

Status: built (the encrypted backup file, slice 1), 2026-10-24: core, CLI, app bridge, desktop and Android screens. Not reviewed. The recovery phrase and recovery
contacts below are designed, not built.

## Problem

An identity is keys on a device, sealed with a passphrase (`core/src/vault.rs`). There are no accounts and no
server copy. Linking a second device (ADR 0010) is the only backup today, and it only helps people who own two
devices. Lose the only device and the identity is gone: contacts must be re-added, re-verified and told the
new key, and nobody can prove the new person is the old one. This ADR adds a way back.

## What an identity is (this decides everything below)

Two kinds of key, and only one of them is "who you are":

| Key | Where | Is it you? | Can it be regenerated? |
|---|---|---|---|
| **Owner key** (Ed25519) | every device (ADR 0010) | **Yes.** Contacts verify it once, by fingerprint, and then trust any device it signs | No. Replacing it is a visible KEY CHANGE for every contact |
| **Device keys** (Olm account: signing key, Curve25519 key, one-time and fallback prekeys) | one device | No, a device you currently use | **Yes**, a new device simply gets new ones |
| Ratchets (`sessions.enc`), MLS state | one device | No | Never restore them (see below) |
| Contacts, groups, history | every device (link copies them) | Convenience | Not from anywhere but a copy |

So recovery does not need to bring back a device. It brings back the **owner key** (and, as a convenience, the
contact list), then makes a **brand-new device** and has the owner key sign it into the device list. That is
exactly what linking does, except that the "existing device" is a file.

## Options

### (a) Encrypted backup file (chosen, built)

A file holding the owner key, the signed contact cards, groups and (optionally) message history, sealed with a
**separate backup passphrase** using the existing vault format (Argon2id then XChaCha20-Poly1305, the file's
purpose as associated data). Saved wherever the person likes (cloud drive, USB stick, password manager).

- Restores everything the person would otherwise have to rebuild: contacts keep their names and verified state,
  groups continue, history returns.
- Weakest point: the file is a **bearer copy of the identity**. Anyone who has the file and guesses the
  passphrase can sign a device list that removes all your devices and adds theirs (section Threat model).
- Costs the person one deliberate act, and a reminder when it gets old.

### (b) Written recovery phrase (designed, not built)

The 32-byte owner key is shown as 24 words once; typing them back regenerates the owner key. Nothing to store
digitally, nothing to guess the passphrase of.

- Restores **only** the owner key. Contacts, groups and history are not in 32 bytes, so the person has a valid
  identity again but an empty address book. Contacts that still hold your card would reach you (they refresh your
  device list from the relay), but you cannot know who they were, and you would have to be messaged first.
- Device keys, prekeys, ratchets cannot come from a phrase, and must not: a new device is created and listed,
  exactly as in (a).
- Is a plaintext bearer secret on paper: a photo of it is the identity, with no passphrase in the way.
- Needs a 2048-word list in the app. It is a good complement to (a): the phrase could seal the backup key, or be
  the whole backup for people who keep no files. Left out of slice 1 to keep it small and because it alone gives
  a worse restore.

### (c) Recovery contact / social recovery (designed, not built)

Split the backup key (Shamir, 2 of 3) among people the person trusts, held in their apps. Attractive in
principle: nothing to lose, nothing to store. Not chosen now because: it needs a protocol (share delivery,
request, the holders' approval and a time delay so a stolen phone of a holder is not enough), the holders must
themselves be reachable and honest, and every holder learns that you hold shares of an identity with them.
Correlated failure is common: people choose the same few friends. It would be built on top of (a): shares of the
backup passphrase, not of the keys. Not in slice 1.

### (d) Do nothing beyond linking a second device

Costs nothing, adds no new secret anywhere, and is the safest. It leaves people with one device, or whose two
devices are lost together (a bag, a house fire), with no way back. Kept as the default: a backup is **optional**,
nothing is nagged at day one beyond a line in Settings.

### Decision

Build (a). Design the restore around "owner key in, new device out" so (b) and (c) can be added later as other
ways to get the same owner key, with no change to how a restored identity rejoins the network.

## What is in the backup, and what deliberately is not

In (sealed, then written to the file):

- the **owner key**,
- the last known signed device list (for its version number only),
- **contacts** (names, cards, verified flags), **groups** the person is in and **member cards** (as the link
  bundle carries them),
- relay settings (address, whether plain ws is allowed, a custom CA), so the restored app finds its relay,
- optionally **message history** (text; attachments are left out, see below),
- the creation time. A file can be told apart from one made later.

Out, on purpose:

- **The device's Olm account** (device keys, one-time prekeys, fallback key). Prekeys published to the relay were
  consumed by other people; restoring a copy of the account would hand out the same one-time keys twice and keep
  a second device that looks like the lost one.
- **Ratchets and session state (`sessions.enc`, the message-request quarantine, MLS state)**. Restoring a ratchet
  from an old moment re-uses message keys already used (breaking forward secrecy) and desyncs peers (they are
  ahead of it, so every message fails to decrypt on one side until a reset). A new device starts new sessions
  with the usual prekey exchange; nothing is lost that was not already unreadable to a new device.
- **Large (MLS) groups**. Their epoch secrets are per device. The restored identity is not a member any longer
  and must be added again by a member (it receives a new key package).
- **Attachment bytes**: size, and they may be re-requested from the sender (existing flow). Their descriptors
  stay in the history.
- the vault passphrase of the old device, the app lock, direct-connection settings, read markers.

## How a restored identity rejoins the network

1. Make a **fresh** state on the new device (new vault passphrase, new device keys). Nothing about the lost
   device's keys exists here.
2. Connect to the relay as this new device, and **fetch the newest device list the relay holds for the owner**.
   (Another device may have changed it since the backup.)
3. Sign a new device list with the restored owner key: version = max(backup, relay) + 1, containing **only the
   new device**. The lost device (and any other device that existed) is thereby **removed**: whoever holds a lost
   or stolen device stops receiving. This is the point of recovering after a loss. The person is told, before,
   that restoring signs out their other devices, and may link them again afterwards.
4. Publish the list and the new device's prekeys to the relay (`publish_keys`).
5. Refresh each contact's list from the relay so cards are current, and install contacts, groups, history.
6. **Contacts are not messaged by the app.** They find out the usual way: the next time they send, their app
   fetches the owner's newer list, sees the change as "Alice's devices changed" (same owner key, higher version,
   so no KEY CHANGE warning and the verified mark stays), and starts a new session to the new device. The
   restored person's first message to each contact does the same in the other direction.
7. **Small groups** continue (members are identified by owner key and each message goes to the member's current
   devices). The restored person re-sends nothing automatically; messages sent to the lost device while the
   person was away sat in a mailbox nobody could read and are gone with the device (they were encrypted to its
   keys; relays also drop mail after the retention period). Large groups: re-add, as above.

Known gap: between the loss and step 4, people sending to you still encrypt to the lost device. Those messages
are lost. Nothing can fix that without a server-side copy, and a server-side copy is a design decision this
product has avoided.

If the relay cannot be reached the restore fails and changes nothing (the new state is built in a staging
directory and moved into place only at the end). Restoring needs the network; this is a feature: a restore must
publish a list that is newer than whatever exists.

## Threat model

| Threat | Result | Mitigation / residue |
|---|---|---|
| Someone gets the file | They can try passphrases offline, without a rate limit, forever | Argon2id at **256 MiB x 3** (heavier than the vault's 64 MiB: the file leaves the device), minimum 14 characters, and the app offers a **generated key** (25 characters from a 32-letter alphabet, 125 bits) that cannot be guessed. A person-chosen passphrase is only as strong as the person; no length rule fixes that |
| Someone gets file **and** passphrase | They are the person: they can sign a list naming only their device and read new mail. They cannot read old mail (ratchets are not in the file), but they can read the history if it is in the file | Not preventable. Revocation exists only as a new list from the same key, so a thief and the owner race; a contact that sees the list change twice sees it in "devices changed". An owner-key rotation ("identity reset") is future work |
| A backup is a new place to attack | The identity is now only as safe as the file's weakest storage and passphrase | History is **opt-in** and off by default; the file says nothing in clear except its format and creation time |
| Tampering | Flipped bits, swapped header | AEAD with the file purpose as associated data; every field in the payload is validated again on restore (device list signatures, group chains, contact cards) |
| Hostile file | Giant KDF parameters, decompression-like growth | Parameters are bounded as in the vault (refuses > 1 GiB or > 20 passes); size cap on the whole file (256 MiB) and on the header |
| Old backup restored by mistake or by an attacker | Contacts and groups are older; the owner key is the same | The restore fetches the newest list from the relay and numbers above it; contacts are refreshed; the file shows when it was made |
| Server-side copy | None exists | The relay stores nothing of this. **Forgetting both the passphrase and the device loses the identity**: there is no reset, and support cannot help |
| Expiry / rotation | A file never expires cryptographically (the owner key does not change) | The app records when the last backup was made and, after **90 days**, or whenever contacts or groups have changed since, shows "your backup is out of date" in Settings. Making a new backup is the rotation: delete the old file |
| Passphrase typed on a malicious or logged device | Lost like any passphrase | Restore is done on the person's own device; the generated key is shown once and never stored in the app |
| Restore on a device that is already an identity | Overwrites nothing | The restore is only offered on the create screen; the engine refuses to restore into a directory that has an identity |

## Warning text shown to the person (verbatim in the apps)

On making a backup:

> **This file is a copy of your identity.** Anyone who has it and knows its passphrase can pretend to be you and
> will receive your new messages. Keep it somewhere only you can reach, and keep the passphrase separate from the
> file. There is no copy on any server: if you lose both this file and your devices, your identity cannot be
> recovered. The file does not contain your old messages' encryption keys, so a restored account starts new
> conversations with each contact; messages sent to you while you were locked out of every device may be lost.
> Make a new backup every few months, and after you add contacts.

On restoring:

> Restoring signs out every other device that uses this identity, including the one you lost. You can link them
> again afterwards. This needs the relay to be reachable.

## Implementation (slice 1)

- `core/src/backup.rs`: the container (magic, the vault header, sealed payload), the generated-key helper and the
  limits. Pure, no I/O. Tests: round trip, wrong passphrase, tamper at every region, hostile parameters,
  truncation, generated-key shape and normalisation.
- `cli/src/backup.rs`: `export` (payload from a `Home`), `restore` (staging directory, relay, new list, install),
  `last_backup`. CLI commands `spike backup` and `spike restore`.
- `app` (`Session::backup_export`, `Engine` commands `backup_export`, `backup_status`, `backup_new_key`,
  `backup_restore`) and the Tauri commands for the desktop, with Settings and create-screen UI on the desktop and
  on Android (system file picker and share sheet).
- Tests: `cli/tests/backup.rs` (a real relay: total loss of the only device, restore, contacts still reach the
  person, an old device is removed, ratchets do not come back, wrong passphrase and unreachable relay change
  nothing) and `app/tests/bridge.rs` (the same through the JSON bridge).

## How it was verified

- Rust: `core/src/backup.rs` (9 unit tests: round trip, wrong passphrase, tamper in every region, an edited date,
  hostile parameters, key shape and typing variants); `cli/tests/backup.rs` (6 tests against a real relay: total
  loss and restore with contacts, history and no key-change alarm; a lost device that turns up is removed; the
  new list is numbered above the relay's, not the file's; no history when not asked; wrong passphrase, damaged
  file, relay down and an existing identity all change nothing; the file holds no device secrets and the status
  goes stale when contacts change); `app/tests/bridge.rs` (the same through the JSON bridge, with a typed-in
  lower-case key). `cargo test --workspace`, `clippy -D warnings` and `fmt --check` pass.
- Desktop: `node desktop/drive_backup.mjs` drives two real windows: make a key and a backup in Settings, lose the
  window, restore on a fresh install through the file input, wrong passphrase first, then chat with Bob with no
  key-change warning (12 checks, all pass).
- Android: the screens (`Backup.kt`) compile and the app builds; the create screen with the restore button was seen
  on the emulator. **The end-to-end run on the emulator (`android/drive_backup.mjs`) has not passed**: it
  got as far as making the identity and the contact and opening Settings, and then another session was using the
  same emulator, and the second AVD needs a screen-lock PIN this session does not have. The system file picker
  steps (Save to... and Choose backup file) are therefore **unverified**.

## Not done

- (b) the written phrase, (c) recovery contacts, an owner-key reset.
- Attachment bytes in the backup, automatic scheduled backups, an in-app way to delete old backups.
- A review: this is prototype software. In particular the Argon2 cost for phones was chosen without measuring a
  low-end device.
