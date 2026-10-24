# Security status

Written 2026-10-24. Updated whenever a gate below changes. **Spike Connect is an unreviewed prototype. It is
not secure; it is not known to be secure; do not use it for messages whose exposure would hurt anyone.** Only an
independent review, real-device testing and the other gates below can change that sentence, and none has
happened yet.

## What has been verified (by the project's own tests and reading)

| What | How | Result |
|---|---|---|
| Unit and integration tests across `core`, `relay`, `cli`, `mls`, `app`, `mobile`, and the desktop workspace | `cargo test --workspace` and `cd desktop/src-tauri && cargo test` | see "Last run" below |
| Lints and format | `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all --check` | see "Last run" |
| Dependency advisories, licences, sources | `cargo deny check` on both lockfiles, weekly in CI (`watch.yml`) | clean with 3 documented, dated advisory exceptions in `deny.toml` (all via `openmls`/Tauri, none reachable by our configuration according to the reasons recorded there; those reasons are ours, not independently checked) |
| MLS parsing boundary | 5 libFuzzer targets (`mls/fuzz`), run for minutes at a time in CI and longer by hand | no known crash. Minutes-to-hours of fuzzing is a smoke test, not assurance |
| Our own audit (code reading, two passes plus manual Android/desktop review) | [`review/audit-findings.md`](review/audit-findings.md) | 3 high and 6 medium findings fixed (most with regression tests, a few without: see the table), plus some low ones; the rest listed as open |
| End-to-end behaviour on an emulator | Android emulator (x86_64, Android 15) against desktop app and a local relay, scripted | passes, per ADR 0026 |
| One real phone | A user's phone over Wi-Fi against a relay on a PC, manual and in progress | informal; no results recorded here |

### Last run

Filled in at the end of the session that wrote this file; see the commit message of the branch
`claude/security-hardening` for the exact counts. (If this line still says that, the numbers were not recorded:
re-run the commands in the review brief.)

## What has NOT been verified

- **Anything by an independent party.** No one outside the project has read this code. This includes the use
  of `vodozemac` and `openmls`.
- **Real devices.** Not tested: arm64 on a variety of phones, real fingerprint/face sensors, real cameras, battery
  and doze behaviour of the foreground service, `FLAG_SECURE` and the recent-apps behaviour, the 2-minute
  auto-lock, the minified/signed release APK, notification behaviour across OEMs.
- **Load and soak.** Nothing has measured the relay under hostile or even realistic load, over days, with
  restarts and full disks. Hardening limits are guesses (ADR 0022 says so).
- **Key compromise and recovery.** No drill: a stolen unlocked phone, a stolen vault file with a weak
  passphrase, a removed device that keeps the owner key, losing the only device, restoring from a backup (ADR
  0029 is in progress elsewhere).
- **The newest features:** direct device-to-device connections (ADR 0027), identity backup and recovery
  (ADR 0029) and calls (ADR 0028, design only) are not covered by the audit in `review/audit-findings.md`,
  because they were uncommitted work in other sessions when it was done.
- **Reproducible or signed builds, a release channel, an updater key, binary transparency.** None exist.
- **Metadata resistance.** None claimed; see [`review/metadata-review.md`](review/metadata-review.md).
- **Fuzzing outside MLS**, and long-running fuzzing of anything.

## Gates before any real use

All must be met, in this order of importance. "Real use" means: other people's private conversations, or yours
that you could not afford to have exposed.

| # | Gate | Status |
|---|---|---|
| G1 | **Independent security review** of the system (brief: [`review/security-review-brief.md`](review/security-review-brief.md)), including the large-groups review the project owner already made a gate, with every high and medium finding fixed or explicitly accepted in writing | **not started** |
| G2 | **Real-device testing**: at least three physical Android models (one low-end, one Samsung/Xiaomi-class with aggressive battery management), real biometrics, camera, doze, a week of normal use, release (not debug) build | **not started** |
| G3 | **Load and soak test of the relay** with hostile clients (stalled readers, floods, many identities) and a multi-day run with restarts; then set defaults from measurements | **not started** |
| G4 | **Relay enrollment** (ADR 0023) or equivalent so an open-mode relay is not exposed to the internet; until then run a private relay for a known group only | **not built** |
| G5 | **Key-compromise and recovery drills**: lost device, stolen device, removed-device re-add attack (M2 in the audit: decide on an offline owner key), forgotten passphrase, backup restore | **not started** |
| G6 | **Release engineering**: signed Android release with an offline-held key, reproducible-build check passed on two machines, desktop installer signed, an update path with a real key | **not started** (config and notes only: [`review/reproducible-builds.md`](review/reproducible-builds.md)) |
| G7 | **A working disclosure channel**: a monitored address in [`../SECURITY.md`](../SECURITY.md) and someone responsible for it | **not set up** |
| G8 | **Fuzz and robustness**: add fuzz targets for `Chain::add`, `Envelope`, `DeviceList`/`IdentityCard`, link frames and `SyncMsg`; fix the "one failing message blocks `recv`" shape; a state-directory lock (audit M1) | **not started** |
| G9 | **Rollback detection** for state files (counter in the Android Keystore / OS keystore) | **not built** |
| G10 | **Review of direct connections, calls and recovery** once implemented | **not started** |

## If you use it anyway

- Use a relay you run yourself, over `wss://`, with a database on an encrypted volume. Keep it off the open internet.
- Treat what it protects as "better than nothing", not as private. Do not rely on deletion, on removing a
  device, or on a relay not keeping a record of who talked to whom and when.
- Compare fingerprints with every contact in person or over a channel you already trust.
- Keep the phone's OS current (minimum is Android 8; older releases lack many protections).
- Report problems as [`../SECURITY.md`](../SECURITY.md) describes.
