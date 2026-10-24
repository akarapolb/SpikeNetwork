# Security review brief: Spike Connect (whole system)

Status: prepared 2026-10-24 by the code's authors' tooling. **Treat every claim below as a claim.** This
extends [`large-groups-review-brief.md`](large-groups-review-brief.md) (MLS large groups, which stays the
detailed brief for that part) to the whole system. Companion documents:
[`../SECURITY-STATUS.md`](../SECURITY-STATUS.md) (what is verified, what is not, the gates),
[`audit-findings.md`](audit-findings.md) (our own audit and what we fixed), [`metadata-review.md`](metadata-review.md),
[`../threat-model.md`](../threat-model.md), [`reproducible-builds.md`](reproducible-builds.md).

## What is being asked

An independent review of design and implementation, in this priority order:

1. **End-to-end confidentiality and authenticity of 1:1 and small-group messages** (Olm via `vodozemac`,
   fan-out, device lists, owner key, linking). Does a relay, a contact or a removed device get more than the
   [threat model](../threat-model.md) admits?
2. **MLS large groups** (up to 500 people): the questions in the large-groups brief.
3. **The relay as an adversary and as a victim**: authentication, resource exhaustion, what it learns.
4. **State at rest and key handling** on desktop and Android (vault, Keystore-sealed unlock, backups, lock).
5. **Hostile-input robustness** of everything that parses peer or relay data (panics, hangs, unbounded
   allocation). Release builds have overflow checks **on**, so integer overflow on peer data is a crash.
6. **The newer features** once their code is committed: direct device-to-device connections (ADR 0027), calls
   (ADR 0028, design only so far), identity backup and recovery (ADR 0029). At the time of writing these were
   uncommitted work in another session; the reviewer should be given the commit that includes them and the
   statement of what was actually implemented.

Out of scope: the cryptography inside `vodozemac`, `openmls`, RustCrypto crates, rustls, SQLite, Android
Keystore/StrongBox, Tauri/WebView2 internals (we review how we **use** them).

## System in one page

```
Compose screens (Kotlin) --JSON--> libspike_mobile.so (mobile/, JNI) --> spike-app (app/) --> spike-cli (cli/) --> spike-core (core/), spike-mls (mls/)
Tauri desktop UI (JS)    --IPC---> desktop/src-tauri (Rust)         --> spike-app ...
clients <-- wss / ws(loopback) --> spike-relay (relay/): SQLite store-and-forward, untrusted
```

- **Identity:** a person has an *owner* Ed25519 key that signs a *device list*; each device has an Olm account
  (Ed25519 + Curve25519). `device_id` = hash of the device Ed25519 key. Contacts are added from a signed card
  (`spike2:`), start unverified; a fingerprint is compared out of band. Details: ADRs 0002, 0008, 0009, 0010.
- **1:1 and small groups:** one Olm session per device pair, one ciphertext per recipient device plus the
  sender's own other devices; signed group state chain (ADR 0024).
- **Large groups:** MLS (RFC 9420) with a credential binding each leaf to an owner key and device list (ADR 0025).
- **Relay:** Ed25519 challenge-response sign-in (v2, bound to the relay name), queues per device id, prekey and
  key-package directory, device-list directory, group mailboxes, attachment chunks, per-address limits. It
  never holds plaintext or keys; it sees what [`metadata-review.md`](metadata-review.md) lists.
- **Vault:** Argon2id (64 MiB, 3 passes) -> XChaCha20-Poly1305, file name as associated data (ADR 0003). The
  CLI, desktop and Android engines all use it. Android can additionally keep the passphrase sealed by an
  Android Keystore key that needs a strong biometric per use (ADR 0026).
- **Android specifics:** `FLAG_SECURE` in release, auto-lock after 2 minutes away (alerts off), no backup,
  content-free notifications, foreground service for alerts.

## Where the code is (size is approximate, 2026-10-24)

| Area | Path | Lines |
|---|---|---|
| Crypto glue, vault, wire, identity, owner/device lists, linking, attachments, groups | `core/src` | ~6,400 |
| Relay | `relay/src` | ~3,300 |
| Client engine (ops, groups, large groups, state) | `cli/src` | ~10,800 |
| Shared app layer + JSON bridge | `app/src` | ~2,300 |
| MLS binding | `mls/src` | ~1,500 |
| JNI | `mobile/src` | ~65 |
| Android app | `android/app/src/main` | ~2,700 Kotlin |
| Desktop shell + UI | `desktop/src-tauri/src`, `desktop/ui` | ~950 Rust, ~710 JS |

## What we claim (and the evidence)

| Claim | Evidence | Limits |
|---|---|---|
| Relay cannot read or undetectably alter message content | tests tamper with bytes at several positions; Olm/MLS authenticated encryption | the relay can delay, drop and reorder; MAC in Olm config v1 is truncated to 8 bytes |
| A relay cannot reuse a device's sign-in elsewhere | v2 sign-in covers the relay name; tests for wrong and right name | clients before this change are incompatible; proxies that rewrite `Host` need `--auth-audience` |
| Forward secrecy for 1:1 content | test with stolen current state | fallback prekey weakens the first message of sessions started with it |
| State at rest is encrypted and tamper-evident | tests for tamper/truncation/swap | rollback to an older valid file is not detected (open) |
| Removal from a large group shuts the member out of future messages and the relay mailbox | tests and mutation checks | see the large-groups brief |
| Hostile input does not crash the relay | length/count caps, `catch_unwind` in the bridge, 5 libFuzzer targets for MLS parsing | only MLS parsing is fuzzed; two remote-panic bugs (H1, H2) were found by reading in this pass, which means more may exist |
| Android release hides content from screenshots and recents | `FLAG_SECURE` set; code reviewed | **not tested on a real device** |

## What we do **not** claim

- That the system is secure. It is an unreviewed prototype.
- Metadata protection of any kind (a relay learns who talks to whom and when).
- Deniability, anonymity, or resistance to a compromised or malware-infected device.
- That removed or stolen devices cannot re-add themselves (they hold the owner key).
- That a relay operator cannot deny service, withhold or delay messages, or show different members different logs.
- That any build is reproducible, signed, or distributed through a trustworthy channel (none exists yet).
- That anything works at scale or on real phones: only an emulator and one real phone over Wi-Fi have run it.

## Known weaknesses (read these first)

[`audit-findings.md`](audit-findings.md) lists everything found, with status. The open items that matter most
to a reviewer: open-mode relay resource exhaustion (no enrollment), queue-fill DoS, owner-key re-add after
removal, contact-chosen relay URLs, no state-directory lock, no fuzzing outside MLS, no rollback detection,
and the poison-pill shape of `recv` (one failing message blocks the queue).

## How to run everything

Prerequisites: Rust stable (see CI), a C toolchain for bundled SQLite; Android work needs JDK 17, the Android
SDK (platform 35), NDK 27 and `cargo-ndk`; fuzzing needs nightly and `cargo-fuzz`.

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace                     # core, relay, cli, mls, app, mobile
cargo deny --manifest-path Cargo.toml check
cargo deny --manifest-path desktop/src-tauri/Cargo.toml check
(cd desktop/src-tauri && cargo test)       # desktop has its own workspace
bash scripts/demo.sh                       # real binaries, TLS relay killed mid-run
cd mls/fuzz && cargo +nightly fuzz run process -- -max_total_time=300   # also: join key_package policy credential
bash android/build-native.sh --release && (cd android && ./gradlew assembleRelease)
```

Run a relay and two clients by hand: `docs/deploy-relay.md`, `cli/README`-style usage in `scripts/demo.sh`.
Android emulator end-to-end scripts: `android/drive_mobile.mjs` and friends (see `android/README.md`).

## Suggested review method

1. Read the threat model and this brief's claims, then try to break the claims, not the code style.
2. Start from the parsing boundaries: `core/src/wire.rs`, `envelope.rs`, `owner.rs`, `link.rs`, `mls/src/group.rs`
   (`process`, `join`), `relay/src/lib.rs` (`handle`, `dispatch`).
3. Check every use of `+`, `as`, indexing and `unwrap`/`expect` on data that came from a peer or a relay.
4. Review key lifecycle: where secrets live in memory, what is zeroised, what is written to disk, what the
   Android UI layer can see (`app/src/bridge.rs` says keys never cross it).
5. Review the relay as a hostile party in the client (what a relay may lie about) and as a target (what a
   client may do to it).
