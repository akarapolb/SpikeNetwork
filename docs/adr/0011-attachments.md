# ADR 0011: Encrypted attachments and the relay blob store

Status: **Proposed** (implemented). Completes the last Phase 1 roadmap item (attachment prototype).
Date: 2026-10-14

## Decision
- **Client-side encryption** (`core/src/attachment.rs`): a fresh random 256-bit key per file; the file is split into 256 KiB chunks, each sealed with XChaCha20-Poly1305. Associated data binds each chunk to the blob id, its index and the chunk count, so chunks cannot be swapped between files, reordered or dropped unnoticed. A SHA-256 of the whole plaintext is checked after reassembly. Files up to 100 MiB.
- **Descriptor** `{name, size, sha256, blob, key, chunks, relay}` travels only inside the end-to-end encrypted message (envelope `attachment` field, validated on receipt). The body is the caption.
- **Relay blob store**, separate from the message queue (answers open question 6 of the multi-device design): chunks stored under a random 128-bit blob id; the first uploader of a blob owns it (others cannot add chunks); chunk count fixed by the first chunk; size and shape checked; per-device quota (default 1 GiB); expiry (default 30 days), purged with the message queue. Anyone who knows the random blob id may download chunks: the id is a capability, and the chunks are useless without the key.
- **Receiving**: the message is stored with its descriptor; `spike fetch` downloads, decrypts, verifies and keeps the file **sealed in the local vault** (`attachments/<keyed tag>.enc`). The sender keeps its own copy too. A failed integrity check keeps nothing.
- **Linking** (ADR 0010): attachments younger than 30 days that the existing device holds travel inside the encrypted bundle (bytes checked against their hash on arrival); older ones are fetched on demand from the relay while it still has them. This implements the multi-device design's default.
- **File names** are display only; `safe_file_name` strips paths, parent-directory tricks, control characters and leading dots before anything is written, and `spike fetch` refuses to overwrite without `--force`.

## What the relay learns
Blob ids, chunk counts and sizes (so approximate file sizes), which device uploaded, and when. Not names, content, keys, or which message or conversation a blob belongs to.

## Tested
Core: multi-chunk and empty round trips; plaintext and key absent from chunks; swapped, reordered, foreign and tampered chunks rejected; whole-file hash checked; descriptor validation; file-name sanitising. End to end (`cli/tests/attachments.rs`): round trip with local copies on both sides; the sender's other device can open its copy; relay disk holds no content or names; a tampered stored chunk is refused and nothing is kept; expired files give a clear message; relay ownership, shape, size and quota rules; linking carries recent files and older ones are fetched on demand. `scripts/demo.sh` sends a 44 KB file with the real binaries and checks the relay's storage.

## Limits
- Once the relay has expired a file, a device that never fetched it cannot get it; **device-to-device fetch** ("available on your laptop", multi-device design) is not built (needs M5's own-device sync channel).
- Whole files are read into memory (fine up to 100 MiB; streaming later).
- No thumbnails or previews; no resumable uploads beyond re-sending chunks; hex-in-JSON transport doubles bytes on the wire.
- File size leaks to the relay; padding to size classes is possible later.
