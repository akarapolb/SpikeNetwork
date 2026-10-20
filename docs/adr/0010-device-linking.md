# ADR 0010: Device linking (multi-device milestone M4)

Status: **Proposed** (implemented)
Date: 2026-10-14

## Flow
1. **New device** (`spike link-new`): shows a **link code** (the QR code in the app): its device card, a fresh ephemeral X25519 public key, a random 16-byte secret, and its home relay. The code is never sent through the relay. The ephemeral secret key exists only in memory.
2. **Existing device** (`spike link-device <code>`): checks the device is not already linked, makes its own ephemeral key and sends a plain `hello` frame (its ephemeral public key) to the new device via the relay.
3. **Both derive** a frame key and an 8-digit **safety code** with HKDF-SHA256 over X25519(eph, eph), salted with the secret and bound to the new device's card and both ephemeral keys (`core/src/link.rs`). Both screens show the code; the person confirms on **both** devices. A substituted hello (relay or anyone else who saw the code) gives different codes.
4. **Only after confirmation**, the existing device signs device list v+1 including the new device, publishes it, and sends the **bundle** in numbered XChaCha20-Poly1305 frames (header authenticated: frames cannot be reordered, renumbered or the bundle truncated).
5. **New device** assembles the frames that open under its session (others are skipped), installs the owner key and signed list (`install_identity` checks the list is signed by that owner and includes this device), merges contacts (with their verified flags) and inserts history idempotently, then publishes its prekeys and joins fan-out (ADR 0009).

## Bundle
`{ owner_key, device_list, contacts, messages }`. History choice: `all` (default, per the multi-device design: all text), `recent --recent-days N`, or `none`. Attachments do not exist yet, so the "recent attachments, older on demand" rule is not implemented.

## The owner key travels to the new device
This implements option (a) of the multi-device design's open question 3: every device holds the owner key (sealed in its vault), so any device can add or remove devices. Consequence: a compromised device can add a rogue device. Contacts are told when a person's device list changes, and the owner can remove devices from any other device. Option (b), an offline owner key, remains possible later.

## Tested
Core: matching codes and keys; substituted hello gives a different code and frames don't open; tampered, renumbered, truncated and foreign frames rejected; link-code validation. End to end (`cli/tests/linking.rs`): linked device shares the owner key and fingerprint, receives contacts (still verified) and history, receives fan-out and can send (sent copy reaches the original device); history choices all / recent / none; man-in-the-middle hello produces different codes and the real bundle cannot be opened; re-linking refused; the relay's disk never contains the owner key, contact names or message text. The demo (`scripts/demo.sh`) links a laptop with the real binaries.

## Limits
- Transfer is not resumable: the new device acks frames as they arrive, so if it dies mid-transfer, linking must be restarted (the existing device's list already includes it; remove it or link again after removing).
- Both CLI sides need `--yes` in scripts; the app will show the code instead.
- No direct LAN or peer-to-peer transfer yet; the bundle goes through the relay (encrypted).
- Large histories are bounded at about 400 MB per bundle.

## Update (2026-10-20): compact code
The code is now `spikelink2:` plus the same four fields as raw bytes (card 128, ephemeral key 32, secret 16, relay URL) in base64url: about 330 characters instead of about 850, small enough for a QR code. `spikelink1:` (hex of JSON) is still accepted. The security properties are unchanged: the card's key-binding signature is still checked and the safety code still comes from the ephemeral exchange.
