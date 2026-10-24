# Security audit findings (phase 1)

Date: 2026-10-24. Commit audited: `1f4e0db` plus the working tree of that day. Method: **code reading only** by
the author's tooling (two read-only passes by separate agents, one for the relay, one for core/cli/app/mls),
plus a manual pass over the Android and desktop configuration. Nothing was fuzzed or load-tested in this pass.
Every finding was confirmed against code by the reporter; I re-checked the ones I fixed. This is **not** an
independent review: an author-side audit finds what the author's tools can see and is a preparation for one.

Full per-finding write-ups (file, attack, fix): [`audit-relay.md`](audit-relay.md) (R-xx) and
[`audit-core-cli-mls.md`](audit-core-cli-mls.md) (H/M/L). Line numbers there may have drifted.

Status words: **fixed** = code changed in this branch; "test" says what proves it. **open** = not fixed; the
gate list in [`../SECURITY-STATUS.md`](../SECURITY-STATUS.md) says whether it blocks real use.

## Relay

| ID | Sev | Finding | Status |
|---|---|---|---|
| R-01 | high | Unbounded per-connection outbound queue and no write timeout: memory exhaustion by a client that never reads | **fixed**: bounded queue (32), 30 s write timeout, a full queue skips a live push (the entry stays in the database / is read by sequence). No regression test (needs a stalled TCP peer); manual reasoning only |
| R-02 | high | Prekeys, key packages, device lists never expire; no global caps; free identities bypass per-device caps | **open**. Root cause is that enrollment (ADR 0023) is not built. **Gate: do not expose an open-mode relay to the internet** |
| R-03 | med-high | Anyone can fill a victim's 1000-message queue; legitimate senders get `queue_full` for up to 7 days | **open** (needs delivery tokens / sealed-sender design, see metadata review) |
| R-04 | med | Backlog of a reconnecting device is loaded and hex-encoded in memory under the global lock | **open** |
| R-05 | med | An `accept()` error ends the whole relay | **fixed** (back off and continue). No test |
| R-06 | med | No timeout on the WebSocket handshake (slowloris) | **fixed**; test `a_silent_connection_is_dropped_before_the_handshake_finishes`. The proxied-connection admission gap (connections from a trusted proxy are not counted until after the handshake) is **open** |
| R-07 | med | Signatures verified before the per-request count cap (CPU amplification) | **fixed**; test `too_many_prekeys_in_one_request_are_refused_before_any_signature_is_checked` (key packages are not signature-checked by the relay and are truncated at the cap, so they needed no guard) |
| R-08 | med | Sign-in signature not bound to the relay: a relay can forward another relay's challenge and sign in as the device there | **fixed**: v2 signature covers the relay's name (`Host` header, or `--auth-audience`); tests `sign_in_answer_is_bound_to_the_relay_it_was_made_for`, `relay_audience_can_be_set_for_proxies_that_rewrite_host`, core unit tests. **Wire change: old clients and relays no longer interoperate** |
| R-09 | med | One-time prekey claim limit is per connection only: reconnecting drains a victim's prekeys | **open** |
| R-10 | low-med | Pings and non-text frames are free | **fixed** for pings (small cost); test `a_ping_flood_is_rate_limited`. Non-text frames still free (**open**) |
| R-11 | low-med | `std` mutexes on async threads; poisoning turns one panic into an outage | **open** (no remotely reachable panic found) |
| R-12 to R-16 | low/info | Purge work under the DB lock; metrics listener has no connection cap; shared mailbox key lets an ex-member DoS until rotation; backup file permissions window; open metadata lookups | **open** |

## Core, CLI, app, MLS

| ID | Sev | Finding | Status |
|---|---|---|---|
| H1 | high | A peer-supplied Lamport clock of `u64::MAX` permanently breaks sending (overflow checks are on in release) | **fixed**: received clock values are capped at 2^48 and the increment saturates; test `a_hostile_clock_value_does_not_wedge_sending` |
| H2 | high | A contact can make you adopt a group state with version `u64::MAX`; the next state panics `recv` before ack, so the message is redelivered forever | **fixed** for the overflow: versions above 2^48 are refused, arithmetic is checked; test `a_huge_version_is_refused_not_a_crash`. The general poison-pill shape (any error inside `process` aborts `recv` and leaves the message unacknowledged) is **open** |
| M1 | med | No inter-process lock on the state directory: two commands at once can roll back ratchet state and reuse message keys | **open**. Do not run two clients on one state directory (the apps do not) |
| M2 | med | A removed device still holds the owner key and can re-add itself | **open** by design (ADR 0010 option a); threat model corrected to "Partial". Gate: decide on an offline/hardware owner key |
| M3 | med | Contact-chosen relay URLs make the client dial arbitrary hosts (loopback port probing; authenticates there, now bound by R-08) | **open** |
| M4 | med | A device card carries no owner consent; first contact wins attribution | **open** |
| M5 | med | Large-group sync trusts relay `seq` (loop, OOM, `cursor + 1` panic) | **fixed**: out-of-order entries are an error, the add saturates. No test (needs a hostile relay double) |
| M6 | med | Mailbox generation from a join bundle is attacker-chosen then `+ 1` | **fixed**: bundles above 2^32 refused. No test |
| M7 | med | Unknown-sender blobs trigger N relay fetches and repeated signature checks (amplification) | **open** |
| L1 to L6, L8, L9 | low | Tombstone squatting through 1:1 Delete; unbounded agent inbox/audit growth; `LargeLists` bloat; link hello first-wins; `write_private` temp-file permissions race; passphrase rules only in app/CLI, secrets in non-zeroized buffers; legacy-card upgrade; fallback-key pre-key replay | **open** |
| L7 | low | Names not checked for control/bidi characters; Windows device names | **fixed** for attachment file names (`safe_file_name`; unit tests). Group and policy names: **open** |
| Fuzzing gap | info | Nothing fuzzes `Chain::add`, `Envelope`, `DeviceList`/`IdentityCard`, link frames or `SyncMsg` (a `Chain::add` target would have caught H2) | **open** |

## Android app (manual pass)

| ID | Finding | Status |
|---|---|---|
| A-1 | No `FLAG_SECURE`: messages appear in screenshots, screen recordings and the recent-apps picture | **fixed**: on by default in release builds, a "Allow screenshots" switch in Settings; debug builds allow screenshots so the test scripts keep working. Verified to compile; **not tried on a device** |
| A-2 | The engine stays unlocked in memory indefinitely once opened | **fixed**: locks 2 minutes after leaving the app unless background alerts are on (alerts need the engine). Not tried on a device |
| A-3 | Release build: no minification, no signing config, no R8 rules, dependency metadata in the APK | **fixed**: R8 + resource shrinking, keep rules for the JNI class, signing read from `android/keystore.properties` or `SPIKE_*` environment variables, v2/v3 signing only, dependency info stripped. An unsigned release APK builds; the `DebugReceiver` class is absent from its dex; the signed build and the JNI keep rules at runtime are **not verified on a device** |
| A-4 | No network security config, `allowBackup=false` only | **fixed**: cleartext off, system CAs only, explicit backup/transfer exclusion rules for Android 11 and 12+ |
| A-5 | Biometric unlock | Reviewed, no change: key requires strong biometric per use, invalidated on enrolment change; the passphrase is held as an ordinary string while in use (not zeroised) |
| A-6 | Notifications | Reviewed: content-free ("New message"), lock-screen public version, service notification secret |
| A-7 | Manifest | Reviewed: only `MainActivity` is exported; service and FileProvider are not; FileProvider covers `files/received/` only; `DebugReceiver` lives in the debug source set only; permissions are INTERNET, CAMERA, USE_BIOMETRIC, POST_NOTIFICATIONS and the foreground-service pair |
| A-8 | Clipboard | The only copies are the person's own public contact card. Pasting reads the clipboard on a tap. No secret is copied |
| A-9 | minSdk 26 (Android 8) | **open**: old OS versions miss years of platform fixes; consider raising to 29-31 before wider use |
| A-10 | Native library: `catch_unwind` around every command; release profile has overflow checks on | Reviewed. A panic becomes an "internal error" answer, not a crash. The library is not stripped of symbols or hardened further |

## Desktop app (manual pass)

| ID | Finding | Status |
|---|---|---|
| D-1 | CSP `default-src 'self'; style-src 'self' 'unsafe-inline'; connect-src ipc:` | Reviewed: no inline or remote script. `unsafe-inline` styles are a minor weakness |
| D-2 | Capabilities: only event listen/unlisten; commands are the app's own | Reviewed. The IPC surface contains no generic file-read or shell command; `send_file` takes bytes, not a path |
| D-3 | UI builds the DOM with `textContent`; one `innerHTML` assigns static icon paths | Reviewed: no injection of message text found. `qr_svg` output is inserted by the app from its own generator |
| D-4 | Received files are saved to Downloads with a unique name, never overwriting | Reviewed; file name rules improved (see L7). Windows will still happily run `.exe` the user opens: unchanged |
| D-5 | Updater is off unless built with `--features updater` and a public key | Reviewed: update check is user-initiated, downloads verified against a compiled-in key. Key, server and signing process **do not exist**; unverifiable until they do |
| D-6 | No code signing for the installer | **open** (SmartScreen warnings, no publisher assurance) |
