# ADR 0003: Encrypted state at rest

Status: **Proposed** (implemented)
Date: 2026-10-13

## Decision
All client state is encrypted on disk under a key derived from a user passphrase.
- **KDF:** Argon2id (`argon2` crate), 64 MiB, 3 passes, 1 lane, random 16-byte salt. Parameters are stored in `vault.json` and bounded on read so a hostile file cannot force huge memory use.
- **Cipher:** XChaCha20-Poly1305 (`chacha20poly1305` crate) with a fresh random 24-byte nonce per write.
- **Binding:** the file name is authenticated data, so `contacts.enc` cannot be replaced with another sealed file, including one from a different vault (different key anyway).
- **What is sealed:** account/identity keys and prekeys, ratchet sessions, and the contact list (names, cards, verified flags). Only `vault.json` (salt, KDF parameters, a sealed check value) is plaintext.
- **Passphrase input:** `--passphrase-file`, then `SPIKE_PASSPHRASE`, then an interactive prompt (twice on `init`, minimum 10 characters). Passphrase memory is zeroized on drop where the language allows.
- **Files:** state directory `0700`; files `0600`, written atomically (temp file, fsync, rename).
- **No custom primitives:** only the audited-lineage RustCrypto crates.

## Changing the passphrase (2026-11-14)
Settings > Identity > Passphrase (desktop and phone) re-seals **this device's** vault. The files are not
re-encrypted: the vault header becomes version 2 and holds the same vault key, sealed (XChaCha20-Poly1305, name
`vault-key`) under a key stretched from the new passphrase with a fresh salt and the same Argon2id parameters. The
old passphrase no longer opens the new header; everything sealed so far, and the keyed index tags, stay as they were.
The header is written atomically, so a crash leaves either the old or the new passphrase working. Version 1 headers
still unlock. Each device has its own passphrase; changing it on one does not touch the others. On the phone, a
passphrase kept for fingerprint unlock is replaced after one fingerprint check (or fingerprint unlock is turned off,
and the app says so), and one kept for the background is replaced at once.

Limit: the vault key itself does not change. Someone who has an old copy of `vault.json` **and** the old passphrase
can still derive the key and read the files, including newer ones. Changing the passphrase protects against a
passphrase that leaked, not against a disk image that was already taken with it. Re-keying every file is future work.

## What this protects against
Theft or backup leakage of the disk while the device is locked or off: an attacker needs to guess the passphrase offline (cost set by Argon2id), and cannot read or modify state without detection.

## What it does not protect against
- A running, unlocked process, malware, or a debugger on the live device. Keys are in memory while the CLI runs.
- A weak passphrase. Argon2id slows guessing; it cannot rescue "password123".
- Rollback: an attacker with write access could restore an older copy of a state file (e.g. an older ratchet state). Authenticated encryption does not detect replacement by an older valid file from the same vault. Mitigations (monotonic counter in a hardware keystore) are future work.
- Loss of the passphrase: the identity cannot be recovered. Backup and recovery design is a Phase 3 item.
- Metadata: file sizes and modification times are visible.

## Not yet done
OS keystore / hardware-backed keys, a `spike passwd` command in the CLI (the engine call exists: `Home::change_passphrase`), re-keying on passphrase change, separate key per file, secure memory pinning (mlock), and a "locked" session agent so the passphrase is not needed for every command.
