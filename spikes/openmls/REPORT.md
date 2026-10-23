# Spike: can Spike Connect use MLS (`openmls`) for groups larger than 20?

Date: 2026-10-23. Machine: Windows 11, Intel i5-11400, 40 GB RAM, rustc/cargo 1.95.0. Single-threaded, in-memory, no network. One run per number: treat timings as roughly ±30%; sizes are exact.

**Provenance.** The spike was run by a delegated agent; this file is the main session's write-up of its report. Re-checked by the main session: the two tests pass (`cargo test`), the CSVs match the tables below, the dependency pins are as stated, and nothing outside `spikes/openmls` changed. Everything else (Android/iOS checks, binary sizes, the advisory and maturity research, the relay analysis) is the agent's word and was not independently re-run. Items marked UNVERIFIED were not tested.

## Verdict

**Go with changes, as the path for groups above 20, but not as a drop-in swap of the transport.** Keep pairwise (ADR 0024) for 20 or fewer.

- The crate works: `openmls` 0.9.2 builds on Windows with the pure-Rust provider and no C toolchain, passes `cargo check` for aarch64-linux-android and aarch64-apple-ios, and a full add/send/remove lifecycle runs and passes.
- The numbers are good: one send is one 177-byte ciphertext at any group size (pairwise needs about 48 KB of Olm messages at 500 devices); an add or remove commit is about 1.5 to 2 KB once members have each committed once.
- The hard part is not the crate: (1) the relay needs a per-group ordered mailbox, bigger blobs and a resync story (today's per-device queue with a 7-day TTL would permanently kill any member who misses one commit); (2) identity: MLS is one leaf per device and `openmls` has no credential-validation callback (a working sketch is in `src/lib.rs`), and linking a device becomes a commit in every group; (3) metadata: MLS headers carry group id, epoch and message type in the clear, so messages need an outer layer to keep ADR 0024's promise that the relay does not see the group id or member list.
- Maturity is acceptable but not comfortable: MIT, actively maintained, but no independent audit of the crate was found, seven security advisories since September 2025 (two High in 2026), and an API that breaks in most minor releases.

## Evidence: cost at scale

Ciphersuite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`, RustCrypto provider, release build with LTO, credential identity 112 bytes. "Leaves" means devices. Measured on the creator (leaf 0) and one other member (leaf 1). `fresh`: the creator added everyone in one commit and nobody has committed since (worst case for the next commit). `populated`: every member then did one `self_update`. Sizes are TLS-serialized MLS bytes before transport encoding. Raw data: `bench_fresh.csv`, `bench_populated.csv`.

| Leaves | State | Add 1: commit B | Remove 1: commit B | Welcome (1 joiner) B | Ratchet tree B | Add: build ms | Remove: build ms | Member processes add ms | Member processes remove ms | Join (Welcome + verify all credentials) ms |
|---|---|---|---|---|---|---|---|---|---|---|
| 20 | fresh | 2,601 | 2,219 | 378 | 6,290 | 2.6 | 1.2 | 0.8 | 0.9 | 1.9 |
| 20 | populated | 1,453 | 1,153 | 378 | 7,565 | 1.5 | 0.9 | 0.8 | 0.6 | 1.9 |
| 100 | fresh | 9,231 | 8,849 | 378 | 29,148 | 3.1 | 2.6 | 1.9 | 1.6 | 8.7 |
| 100 | populated | 1,687 | 1,387 | 378 | 37,091 | 2.3 | 1.9 | 2.9 | 1.3 | 9.2 |
| 320 (20 people x 16 devices) | fresh | 27,345 | 26,963 | 378 | 91,764 | 6.8 | 5.7 | 3.5 | 3.5 | 34.0 |
| 320 | populated | 1,921 | 1,621 | 378 | 118,267 | 3.8 | 4.1 | 3.7 | 4.6 | 28.1 |
| 500 | fresh | 42,107 | 41,725 | 378 | 142,884 | 10.7 | 10.7 | 5.0 | 3.9 | 41.0 |
| 500 | populated | 1,921 | 1,621 | 378 | 184,703 | 6.4 | 8.6 | 5.2 | 6.0 | 46.3 |
| 1000 | fresh | 83,144 | 82,762 | 378 | 284,952 | 19.6 | 18.3 | 7.8 | 9.1 | 87.9 |
| 1000 | populated | UNVERIFIED | | | | | | | | |

The 1000 populated row was not run: setup is quadratic (the 500 run took about 17 minutes and 2.3 GB), so over 30 minutes and over 8 GB is expected.

Constant in every row: key package 385 B; application message with a 32-byte body 177 B; group creation 0.1 to 0.4 ms; key package generation 0.11 to 0.14 ms. A one-shot add of n devices in a single commit (commit B / Welcome B): 20: 8,022 / 3,114; 100: 39,134 / 15,274; 320: 124,562 / 48,716; 500: 194,402 / 76,076; 1000: 388,436 / 152,076. The Welcome grows about 150 B per joiner; the ratchet tree is about 285 to 370 B per leaf.

### Per send, MLS against today's pairwise design

Olm sizes measured with vodozemac 0.11.1 (the version `core` uses): a normal message with a 32-byte body is 95 B (plus 1 kind byte), 1,056 B with a 1,000-byte body; a first (pre-key) message is 200 B. Pairwise upload per send is (devices - 1) x 96 B, excluding `core` envelope padding and transport encoding (not measured).

| Devices | Pairwise upload/send (B) | MLS upload/send (B) | MLS download per recipient (B) | Pairwise download per recipient (B) |
|---|---|---|---|---|
| 20 | 1,824 | 177 | 177 | 96 |
| 100 | 9,504 | 177 | 177 | 96 |
| 320 | 30,624 | 177 | 177 | 96 |
| 500 | 47,904 | 177 | 177 | 96 |
| 1000 | 95,904 | 177 | 177 | 96 |

For a short text MLS is bigger per recipient (177 against 96 B) and much smaller for the sender, but only if the relay fans one upload out to many mailboxes. If the client still makes N x D `Send` calls, upload is N x D x 177 B, about twice the pairwise bytes: the gain is then CPU and sound group keys, not bandwidth. Olm CPU was not measured.

## Findings by question

**1. Builds on Windows.** Pins: openmls =0.9.2 (default features off), openmls_rust_crypto =0.6.0, openmls_basic_credential =0.6.0, openmls_traits =0.6.0, tls_codec =0.5.0 (0.4 gave a trait mismatch), ed25519-dalek =2.2.0, vodozemac =0.11.1 (baseline only); 166 unique crates. Clean build: debug 42.9 s, release with LTO 95.5 s. No C/C++ toolchain is invoked (`cargo tree -i cc` empty for Windows, Android, iOS and Linux; with `--target all`, `cc` appears only under a `valgrind_ct_test` cfg that is never active). The MSVC linker is still needed to link, as for any Rust program. Provider: RustCrypto is the default recommendation (what openmls tests and most users run, no platform intrinsics); libcrux (verified primitives, post-quantum suites) is UNVERIFIED. Suite 1 is mandatory-to-implement in RFC 9420 and matches our Ed25519/X25519 keys.

**2. Android and other targets.** `rustup target add aarch64-linux-android aarch64-apple-ios` worked; `cargo check --lib` passed for both (29 s, 26 s); a release build of the library for Android completed in 55 s with no NDK. Linking a final Android `.so` needs the NDK linker and was not done: a working library is UNVERIFIED. wasm32-unknown-unknown fails at `getrandom` (the `js` feature was not tried). openmls adds about 1.4 MB to a small Windows release binary (1,713,152 B against 276,480 B, both linking vodozemac and ed25519-dalek); mobile sizes are unmeasured. MSRV of all the crates is Rust 1.91.

**3. Group lifecycle** (`tests/lifecycle.rs`, passes; re-checked). Five devices (Alice's two, Bob, Carol, Dave): create group; add three in one commit with one Welcome; Bob's message decrypts for the others; add Dave (epoch 2); remove Carol, whose `process_message` then returns an error while everyone else decrypts Dave's later message (epoch 3). Carol keeps what she received before removal, the same limit as ADR 0024. The wrapper merges its own commit immediately; a real client must merge only after the relay accepts the commit.

**4. Cost.** Commit size is linear in group size right after a bulk add and logarithmic once members have committed (1.9 KB at 500 leaves), so every new device should `self_update` after joining. Single operations take milliseconds even at 1000 leaves on a desktop CPU; phones are unmeasured. The ratchet tree is the costly object (143 to 185 KB at 500 leaves, 285 KB at 1000). The relay's 64 KiB blob limit means a tree stops fitting in one blob from roughly 170 to 220 leaves, and the 500-leaf bulk-add commit (194 KB) does not fit either: chunking (`PutBlobChunk`) or a larger limit is required. Join time is dominated by one Ed25519 owner-signature check per leaf (43 ms at 500).

**5. Multi-device and identity.** 20 people x 16 devices is 320 leaves: commits up to 27 KB fresh and 1.9 KB populated, tree 92 to 118 KB, join 28 to 34 ms, processing about 4 ms: the 16-device cap is affordable. Options: (A) one leaf per device, the MLS norm and what the code does; linking a device means an Add commit in every group sent by an online device (ADR 0024's copy of group state to a new device is impossible in MLS, because private tree keys must never be cloned), and revoking a device means a Remove commit per group; (B) one leaf per person with shared state is unsupported and loses per-device forward secrecy, not recommended; (C) virtual clients (a draft, behind openmls's `virtual-clients-draft` feature), UNVERIFIED. A credential can say "this leaf belongs to owner X": identity `owner_pub(32) | device_id(16) | Ed25519_owner_sig(64)` (112 B) in a `BasicCredential`, the signature over `"spike-mls-leaf-v1" | leaf_signature_pub | device_id`; `verify_leaf()` is called for every leaf a commit or Welcome introduces; the test `credential_binding_is_enforced` shows an unknown owner and a spliced credential both rejected (openmls itself accepts the spliced key package, because its signature is self-consistent; only our check catches it). openmls has no authentication-service callback: the sketch inspects the staged commit before merging it, and `StagedWelcome::members()` before the group is stored. Not in the sketch: freshness and revocation (the credential does not carry the device-list version, so a revoked device stays valid until someone commits a Remove). Key package lifetime defaults to 84 days.

**6. What MLS needs from the relay** (design list from RFC 9420/9750, the openmls API and the relay source; nothing here was measured). Today: per-device queues (cap 1000, 7-day TTL), one-time prekey publish and claim, signed device lists, chunked blobs (30-day TTL), 64 KiB message limit.
- *Key packages:* partly there (`PublishPrekeys` / `ClaimPrekey` work this way); packages are 385 B; a "last resort" package must be reused when the pool is empty.
- *Total order of commits:* nothing today. RFC 9750 says the first commit for an epoch wins and concurrent commits are not merged. Either the relay rejects the loser (compare-and-append on a per-group sequence number, which gives an order without reading the epoch) or every client discards it by a deterministic rule and the loser redoes its change. The wrapper's eager merge must become merge-after-accept. openmls's `fork-resolution` feature is UNVERIFIED.
- *Fan-out of one ciphertext:* not done today. Uploading the same blob N x D times needs no relay change but gives no bandwidth win and shows the relay the exact device set (worse than now). A group mailbox with one append and per-member cursors can work without a member list if its id is unguessable (for example derived from a secret held by members); rotating access per epoch cuts off removed members but also strands a member who missed the commit: unsolved here.
- *Welcome delivery:* works today as a `Send`; adds about 150 B per joiner; the tree needs chunking above a few hundred leaves.
- *Offline and removed members:* the 7-day TTL and 1000-message cap are the wrong shape. A device that misses one commit cannot decrypt anything afterwards. Handshake messages must be kept longer than any plausible offline period, or the device resyncs by external commit from a published GroupInfo (`join_by_external_commit` exists; restrict who can fetch GroupInfo) or is re-added by an online member. A phone off for 8 days would be silently dead in every group until re-added. A removed member keeps receiving ciphertext she cannot read until the relay stops serving her.
- *Metadata:* message headers expose `group_id`, `epoch` and commit-versus-application in the clear; welcomes and key packages carry hashes linking to served packages. A naive MLS relay would see group ids, membership churn and, with N x D uploads, the recipient set. ADR 0024's "group id and member list stay encrypted" survives only if every message gets an outer AEAD layer addressed to the group mailbox (tens of bytes). Even then the relay learns that the mailbox exists, sizes and times, a commit-versus-message pattern from sizes, and the number of distinct readers.

**7. Maturity and risk** (GitHub pages and API read on 2026-10-23; counts approximate). MIT, created 2020, last push 2026-10-22, about 1,050 stars, about 197 open issues, not archived; maintained by Phoenix R&D and Cryspen. 0.9.0 landed 2026-08-25 with breaking changes (storage formats, `max_past_epochs()` signature, an error variant, a feature flag, MSRV 1.91): pin exactly and budget an upgrade chore per release. Audit: none found for the openmls code (an NCC Group review in October 2024 covered libxmtp, which builds on openmls, and was not read; a Security Research Labs review of the Phoenix architecture was a design review; Cryspen states a verification goal, not a result): treat as unaudited. Advisories as titled on the repo: improper persistence of the secret tree (Moderate, 2025-09); improper tag validation (High, 2026-02); out-of-bounds panic in manual deserializers, remotely triggerable denial of service (Moderate, 2026-08); quadratic pre-auth extension decoding (Moderate, 2026-08); unbounded recursion on nested extensions can abort the process (High, 2026-10-08); rejected private messages can advance the sender ratchet (Low, 2026-10-22); removed leaves reappearing after an external commit (Moderate, 2026-10-22; matters if external commits are used for resync). Open issues relevant to us: storage interface misuse-resistance (#1689, #1931, #2264); async (#1977, #1960: the core API is synchronous); post-quantum RustCrypto support (#1940); joining a large group is quadratic in credential-capability validation (#2224); loading a `PublicGroup` without re-verifying the tree (#2272). Persistence: the spike used `openmls_memory_storage`; production needs a `StorageProvider` on our encrypted store (an `openmls_sqlite_storage` crate exists; the effort is UNVERIFIED). Proposals by reference are supported but unused.

## Open questions and risks

1. The relay design is the critical path: group mailbox, ordering rule, handshake retention beyond 7 days or a defined resync, blob sizes for trees and bulk commits.
2. Metadata regression against pairwise unless messages are wrapped; how wrapping interacts with per-epoch access is unsolved.
3. The dead-member problem: any gap in commit delivery silently kills a device in that group; needs resync (external commit or re-add) and a "needs rejoin" state. Check the external-commit advisory first.
4. Device linking and revocation become per-group commits that need an online device.
5. Credential freshness (device-list version, revocation).
6. Storage: a `StorageProvider` on our encrypted store; migration across openmls storage-format changes.
7. An unaudited, fast-moving crate with recent High advisories: pin it, fuzz our parsing boundary, track advisories.
8. UNVERIFIED: phone performance, the libcrux provider, wasm, linking on Android and iOS, the 1000-leaf populated tree.
9. Not covered: pre-shared keys, external senders, GroupContext extensions for ADR 0024's admin and settings record, group calls (ADR 0018), concurrent admin edits.
10. Pairwise sizes exclude `core` envelope padding and relay transport encoding; MLS sizes are raw TLS bytes.
11. Benchmarks ran once; they were taken when `join` verified credentials after `into_group`. The committed code verifies before, using `StagedWelcome::members`, at the same cost.

## Reproduce

From `spikes/openmls` in PowerShell (delete `target` first to time a clean build; write the CSVs with `Out-File -Encoding utf8` so they are not UTF-16):

```
cargo build
cargo build --release
cargo test -- --nocapture
rustup target add aarch64-linux-android aarch64-apple-ios
cargo check --lib --target aarch64-linux-android
cargo check --lib --target aarch64-apple-ios
cargo build --release --lib --target aarch64-linux-android
cargo tree -i cc --target all
cargo build --release --bin with_mls --bin baseline
Get-Item target\release\with_mls.exe, target\release\baseline.exe
.\target\release\openmls-spike.exe bench fresh 20 100 320 500 1000 | Out-File -Encoding utf8 bench_fresh.csv
.\target\release\openmls-spike.exe bench populated 20 100 320 500 | Out-File -Encoding utf8 bench_populated.csv   # about 17 min, 2.3 GB RAM at 500
.\target\release\openmls-spike.exe olm
```
