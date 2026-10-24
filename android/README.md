# Spike Connect for Android

A native Android app (Kotlin, Jetpack Compose) whose screens follow the mobile prototype
(`docs/mobile-prototype/`) and whose engine is the same Rust code as the desktop app.

```
Compose screens (Kotlin)  --JSON-->  libspike_mobile.so (mobile/, JNI)  -->  spike-app (app/)  -->  spike-cli, spike-core, spike-mls
```

- `mobile/`: the native library. Three JNI functions (`open`, `call`, `close`), all strings.
- `app/src/bridge.rs`: the JSON command bridge, the same commands the desktop app has (`overview`, `send`,
  `sync`, `group_create_large`, ..., plus `wait_for_push` for instant delivery and `qr_matrix` for drawing a
  QR code). Keys never cross it.
- `android/app/src/main/java/network/spike/connect/`: `Bridge.kt` (the door), `Model.kt` (parsing),
  `AppModel.kt` (state, and the loop that waits for the relay and syncs), `Theme.kt` (the prototype's palette,
  fonts, components), `Icons.kt` (the prototype's line icons, generated from its SVGs), `Screens.kt` (the
  screens), `MainActivity.kt`.
- Fonts (IBM Plex Sans, Schibsted Grotesk, IBM Plex Mono, SIL OFL 1.1) are bundled in `res/font`; see
  `FONTS-LICENSE.txt`.

## What follows the prototype, and what does not

Built to match: 1 Create identity (and unlock), 2 Chats (status chips, search, message-requests row, rows
with avatars, tags and unread badges, the four-tab bar), 3 Conversation, 4 Verify, 5 Your devices, 7 Network
and privacy (as Settings), 8 Agent approval and 9 Agents (as the Agents tab: waiting requests with
approve and deny, allowed agents), 10 Add a contact, 11 Message requests. Large and small groups use the same
style (not in the prototype).

Deliberately different, because the feature does not exist yet and a control that does nothing would mislead:
- "Your name" and "unlock with fingerprint or face" on the create screen are left out.
- Add a contact has **My code** (a real QR of your card), **Scan** (camera), **Paste a card** and **Share**.
- 6 Link a new device is built (code or scan, safety code, history choice) and so is the new-phone side ("I
  already use Spike on another device"); the "contact added a device" notice is not.
- Files work one-to-one and in small groups (not large groups). Voice and the on-device assistant are not built.
- The chat list searches names, not message history.
- A one-to-one chat shows no per-device delivery line (the engine does not record it).

## Direct connections (ADR 0027)

Settings has **Direct connections** (off by default): with it on, the phone listens on a port and hands messages straight to a contact's device on the same Wi-Fi without the relay; the relay stays the fallback (and is still needed for the very first message to a contact, which starts the session). It shows the address to give the other device, an address field per contact, and counters. The other device's address is typed once; the reverse is learned. Driver: `node android/drive_direct.mjs` (uses its own relay port, 8797, kills only that relay, and needs `SPIKE_DESKTOP_EXE=<path>` if the usual desktop build is locked by a running app). The emulator needs `svc power stayon true` or the app gets locked when the screen goes off; the driver sets it.

## Build

Needs the JDK 17, the Android SDK (platform 35, build-tools 35, NDK 27), `cargo-ndk` and the Rust targets
`x86_64-linux-android` and `aarch64-linux-android`.

```
export ANDROID_NDK_HOME=<sdk>/ndk/27.2.12479018 JAVA_HOME=<jdk17> ANDROID_HOME=<sdk>
bash android/build-native.sh --release            # builds libspike_mobile.so into jniLibs/
cd android && ./gradlew assembleDebug             # app/build/outputs/apk/debug/app-debug.apk
```

`local.properties` (`sdk.dir=...`) is not committed.

## Try it against a real relay on this PC

```
target/debug/spike-relay.exe --listen 127.0.0.1:8787
adb reverse tcp:8787 tcp:8787      # the phone reaches the PC's relay as its own localhost (no plain ws:// opt-in)
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
node android/drive_mobile.mjs [screenshot-dir]
```

`drive_mobile.mjs` drives the real phone app (through uiautomator and adb) and the real desktop app (through
the DevTools protocol) against that relay: create the identity on the phone, add each other, a one-to-one chat
both ways, a large group both ways, with nobody syncing by hand. Debug builds also have a `DebugReceiver` that
runs a bridge command from `adb shell am broadcast` and logs the answer under the tag `SpikeDebug`; it is not in
release builds. More drivers: `drive_link_new.mjs` (the phone as the new device), `drive_camera.mjs` (scanning a
QR code with the emulator's virtual camera; first `node android/make_camera_poster.mjs` and start the emulator
with `hw.camera.back = virtualscene`). The fingerprint steps of `drive_mobile.mjs` run when a fingerprint is
enrolled in the emulator (set a PIN, enrol with `adb emu finger touch 1`); `NO_BIOMETRIC=1` skips them.

## Known limits

- The app listens to the relay while it is on screen, and with "Alerts while the app is closed" on, through a
  foreground service. After a reboot or if the system kills the app, the passphrase (or fingerprint) is needed
  again before anything is read.
- Only the emulator (x86_64) has been run. The arm64 library builds but has not been run on a phone.
- Debug builds only; no signing, no Play Store packaging.
- Nothing here has been independently reviewed.
