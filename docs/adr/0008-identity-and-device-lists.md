# ADR 0008: Identity key and signed device lists (multi-device milestone M2)

Status: **Proposed** (implemented)
Date: 2026-10-14

## Problem
Until now one Olm account was both the person and the device. Multi-device needs a stable "person" that can add and remove devices without contacts re-verifying everything, and without letting a relay invent devices.

## Decision
- **Owner key** (`core/src/owner.rs`): an Ed25519 key per person, generated at `spike init`. It only ever signs device lists, never messages.
- **Device list**: `{owner, version, devices[{card, relay?}], sig}`. Signed over a fixed field order with context `spike-device-list-v1`. Version starts at 1 and only increases; 1 to 16 devices; no duplicates; each device card's own self-signature is checked too.
- **Identity card**: `spike2:` + the signed list (hex JSON). `spike card` now prints this. Old `spike1:` single-device cards are still accepted as **legacy** cards (owner = the device key, version 0), so nothing already exchanged breaks.
- **Fingerprint** for a signed card covers the owner key only, so it stays the same when devices change. Verifying a person once covers every device their owner key signs. Legacy fingerprints are unchanged.
- **Update rules** for a stored contact (`IdentityCard::check_update`, used by `spike add`):
  | Offered card | Result |
  |---|---|
  | identical | nothing changes |
  | same owner, higher version | accepted; verification kept |
  | same owner, lower version | refused (rollback), even with `--force` |
  | same owner, same version, different devices | refused (equivocation), even with `--force` |
  | different owner | refused as KEY CHANGE unless `--force`; verification reset |
  | signed card for a legacy contact that still lists the known device | accepted as an upgrade; must re-verify |
  | legacy card for a signed contact | refused (rollback) |
- **Conversations are keyed by the owner key**, not a device key, so history stays in one conversation as devices come and go.
- **Where the owner key lives**: sealed in the vault (`owner.enc`) on this device (option (a) of open question 3 in the multi-device design). With one device per person today, this is the only choice; it gets real meaning in M4 (linking).

## Scope limits (M2 only)
Messages still go to **one** device per contact (the first in the list); fan-out to all devices, publishing lists on the relay, and per-device relay addresses are M3. Linking new devices is M4. Cards are about 1,100 characters; a QR code can hold that, but a compact binary encoding may be worth it before the mobile app.

## Tested
Sign/verify round trip; tampered version, added device or swapped owner rejected; shape rules; add/remove bumps the version; rollback, equivocation and key change refused; legacy upgrade allowed only when it keeps the known device; fingerprint stable across device changes; legacy cards interoperate end to end; verification kept on a newer list and reset on an upgrade or forced replacement.
