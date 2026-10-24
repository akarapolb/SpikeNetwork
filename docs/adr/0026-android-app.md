# ADR 0026: The Android app

Status: built, 2026-10-24, in two slices (the app; then camera scanning, linking, alerts, fingerprint unlock and files). Debug builds on the emulator only. Not reviewed.

## Decision

A **native Android app in Kotlin (Jetpack Compose)** whose screens follow the mobile prototype
(`docs/mobile-prototype/`), over **the same Rust engine as the desktop app**, through a small JSON bridge.
The user chose native Kotlin over a Tauri mobile build or a web app.

Why the bridge is shaped this way: the desktop app already has everything the screens need as one Rust type
(`Session`) with plain data types, and its Tauri commands are one-line wrappers. Moving that type out of the
desktop crate into a shared crate (`app/`, `spike-app`) and putting one function in front of it,
`Engine::call(command, json_args) -> json`, gives a Kotlin front end the whole feature set with three JNI
functions (`mobile/`) and no per-feature binding code. The desktop crate re-exports the same module, so its
tests are unchanged. Keys, sessions and the ratchets never cross the bridge: only names, texts and the DTOs the
desktop screens use.

## What was built

- `app/` (`spike-app`): the former `desktop/src-tauri/src/api.rs`, plus `bridge.rs` (the JSON commands, with
  `wait_for_push`, which holds a relay connection subscribed to the person's groups and returns when anything
  arrives, and `qr_matrix`, which returns a QR code as rows of 0 and 1 for drawing natively).
- `mobile/` (`spike-mobile`): the JNI library (`open`, `call`, `close`). Built for `x86_64` and `arm64-v8a`
  with `cargo-ndk`.
- `android/`: the Gradle project and the Kotlin app. The prototype's palette, three typefaces and 28 line icons
  are reproduced (the icons are generated from its SVGs by script); screens and their exceptions are listed in
  `android/README.md`.
- Instant delivery on the phone: while the app is on screen, a loop waits on the relay and runs a sync when
  something arrives (the same idea as the desktop listener).
- A debug-only broadcast receiver that runs a bridge command and logs its answer, so a script can drive the
  app the way `desktop/drive_large.mjs` drives the desktop (DevTools protocol).

## How it was tested

- Rust: `app/tests/bridge.rs` (4 tests against a real relay: locked or confused engine, two phones chatting,
  a large group with a push wake-up, the QR and devices data); the desktop's API tests still pass through the
  re-export.
- **The real phone app against the real desktop app on a relay running on this PC**
  (`android/drive_mobile.mjs`): the Android emulator (x86_64, Android 15) reaches the relay through
  `adb reverse`; the script creates the identity through the phone's own screens, adds the desktop as a contact
  by pasting its card, exchanges one-to-one messages both ways with nobody syncing by hand, makes a large
  group on the desktop, joins it on the phone through the consent screen, sends in both directions, and visits
  the other screens. 15 checks, all pass; screenshots were read and compared with the prototype.

## Findings along the way

- `crypto-bigint 0.5.5` (via `p256`, via openmls's `hpke-rs`) got a RustSec advisory (RUSTSEC-2026-0387) while
  this was being built. It cannot be upgraded from here (the fix is on a different major line). It is reached
  only through the P-256 ciphersuites; large groups use X25519 (ADR 0025 section 2). `deny.toml` ignores it with
  that reason, to be re-checked when `hpke-rs` or `p256` move.
- The first test runs were thrown by the emulator's keyboard (Gboard's toolbar covered the app) and by a card
  longer than the screen pushing the Add button off it; the second is fixed in the app (the field now stops
  growing), the first is a test-harness matter.

## Second slice: scanning, linking, alerts, fingerprint, files

- **Camera scanning** (Add a contact, Scan; and Link a new device). CameraX plus ZXing's plain Java decoder, so no
  Google services and nothing leaves the phone. Only `spike2:` cards are accepted as contacts.
- **Linking a phone to an existing identity**, both ways, with the same engine calls the desktop uses
  (`link_offer`, `link_wait`, `link_receive`, `link_begin`, `link_finish`, `link_cancel`, now bridge commands):
  "I already use Spike on another device" on the create screen (the phone shows a code and QR, compares the safety
  code, receives identity, contacts and history), and Devices, Link a new device (enter or scan the code, compare,
  choose how much history). Only the main device may link, as on the desktop.
- **Alerts while the app is closed**: a foreground service (type `remoteMessaging`, which unlike `dataSync` has no
  time limit on Android 15) keeps the process and one shared listener alive. The notification says that something
  arrived, never what, and is private on the lock screen. It works while the process lives and the app is
  unlocked: after a reboot or a kill the passphrase is needed again, so nothing is read until the person opens the
  app. That is a consequence of the keys being sealed, not a missing feature.
- **Fingerprint or face unlock**: the passphrase is sealed under an Android Keystore AES key that needs a strong
  biometric for every use and is dropped if the enrolled fingerprints change. The passphrase still works. Trade-off,
  stated plainly: anyone who can make the owner touch the sensor can unlock; that is the usual meaning of biometric
  unlock, and it is a switch the person turns on.
- **Files** one-to-one and in small groups: picked through the system picker, copied to the app's cache, sent by
  path through the bridge (`send_file`, `group_send_file`), received files are fetched and checked by the engine
  (`save_attachment`) and opened through a FileProvider. Large groups refuse files (the engine says so), so the
  button is hidden there.

Found on the way: Compose permission requests crashed inside `FragmentActivity` because the biometric library
brings an old Fragment (an explicit `fragment-ktx` fixes it); and the system Back key left the app instead of
closing the keyboard on some keyboards (now handled in the app).

### Tested

`app/tests/bridge.rs` (6 tests: files by path and saving, linking a second phone through the bridge);
`android/drive_mobile.mjs` (one-to-one, large group, files both ways, the phone linking a second device, the
alerts service and a real notification arriving with the app closed, fingerprint unlock with the emulator's
fingerprint sensor including an unknown finger being refused); `android/drive_link_new.mjs` (the phone as the new
device: same safety code, same identity afterwards, history received); `android/drive_camera.mjs` with
`make_camera_poster.mjs` (the emulator's virtual camera looks at a QR code of a contact card; the app reads it and
adds the contact with the right fingerprint). The camera test needs the emulator started with
`hw.camera.back = virtualscene`; the poster script replaces the emulator's sample poster (the original is kept as
`poster.png.orig`).

## Not done

- Voice, the on-device assistant, editing and deleting messages, files in large groups.
- Only the emulator has run it; the arm64 build has not run on a phone, and no real camera, fingerprint sensor or
  battery behaviour has been seen. Notifications with the process killed are not possible without the passphrase.
- Debug builds only. No signing or store packaging.
- No independent review (ADR 0025 already makes review a gate before real use; this adds a second front end).
