# Spike Connect for iPhone: the plan

Status: plan only. No iOS code exists yet, and none can be built on the Windows machine this repository is developed on.
This note says how an iPhone app would reuse the Rust engine, which SwiftUI screens it needs (from the prototype's
"iPhone: Chats", "iPhone: Conversation" and "iPhone: Face ID unlock"), how Face ID unlock and push would work, what needs a
Mac and an Apple developer account, and the order to build it in.

## 1. The engine: the same Rust, a different wrapper

The Android app does not talk to `spike-core` directly. It calls **one function** of `spike-app`'s bridge
(`app/src/bridge.rs`): `Bridge::call(command, args_json) -> String`, which dispatches about eighty JSON commands
(`overview`, `conversation`, `send`, `group_send`, `unlock`, `lock`, `call_start`, `push_register`, …) and answers
`{"ok": …}` or `{"err": "…"}`. `mobile/src/lib.rs` is only a ~60-line JNI wrapper (`open`, `call`, `close`) around it.
iOS gets the same shape:

| | Android today | iOS |
|---|---|---|
| Native crate | `mobile/` (`spike-mobile`, `cdylib`, JNI) | new `ios/` crate (`spike-ios`, `staticlib`), or a feature on `mobile/` |
| Boundary | `Java_network_spike_connect_Native_{open,call,close}` | three `extern "C"` functions, or UniFFI |
| Build | `cargo ndk` → `jniLibs/*.so` | `cargo build --target aarch64-apple-ios` (+ `aarch64-apple-ios-sim`) → `SpikeEngine.xcframework` |
| App side | `Bridge.kt` (`Bridge.call`, `Bridge.obj`, `Bridge.arr`) | `Bridge.swift`, same three helpers |

### Recommended: a plain C ABI first

The bridge already speaks JSON strings, so the C surface is tiny and stays stable as commands are added:

```c
// spike_engine.h (written by hand, or by cbindgen)
typedef struct SpikeEngine SpikeEngine;
SpikeEngine *spike_open(const char *data_dir);                 // NULL on error
char        *spike_call(SpikeEngine *e, const char *command, const char *args_json); // caller frees
void         spike_free_string(char *s);
void         spike_close(SpikeEngine *e);
```

```swift
enum Bridge {
    static func call(_ cmd: String, _ args: [String: Any] = [:]) throws -> Any  // decodes {"ok"} / throws on {"err"}
}
```

Each Swift call runs off the main actor (`Task.detached` or a dedicated serial executor), exactly as Android runs
`Bridge.call` on `Dispatchers.IO`; the bridge already owns its Tokio runtime. Commands return the same DTOs, so the Swift
models are `Codable` copies of `android/.../Model.kt`.

### UniFFI later, if typed calls are wanted

UniFFI (`uniffi::export` on a `SpikeApp` object) would generate Swift types and async functions directly from Rust, and
the same definitions could later replace the hand-written Kotlin `Model.kt`. It is more build machinery (a bindgen step,
generated sources to keep in sync) for a boundary that is already one string in, one string out. Start with the C ABI;
move to UniFFI only if both apps want typed DTOs, and then for Android and iOS together.

### Things in the engine that need checking on iOS
- **Data directory**: pass the app's `Application Support` directory (excluded from iCloud backup with
  `isExcludedFromBackup`), with file protection `completeUntilFirstUserAuthentication`, so a wake-up after a reboot and
  before unlock can still open the encrypted store files (they are encrypted at rest by the engine anyway, ADR 0003).
- **Networking**: the relay client uses Tokio TCP/TLS (rustls). That works on iOS, but sockets die when the app is
  suspended; the app must reconnect on `scenePhase == .active` and after each push wake-up, as Android does on `ON_START`.
- **Direct connections** (ADR 0027) listen on a port: allowed in the foreground only, and need the
  `NSLocalNetworkUsageDescription` prompt. Ship them off on iOS at first.
- **Calls**: the engine carries signalling only (`app/src/calls.rs`); the screen owns WebRTC. iOS uses the WebRTC
  framework (the Google `WebRTC.xcframework` build) the way `Calls.kt` uses `org.webrtc`.
- **Binary size and bitcode**: build with `lto = "thin"`, `panic = "abort"` for the iOS profile; no bitcode (Xcode 14+).

## 2. Screens: SwiftUI, mapped to the prototype

The prototype's rule: same content and rules as Android, only platform habits change: **a labelled back button, a centred
title, Face ID, and the home indicator kept clear.** Quiet ink, Geist (bundled, as Android bundles it), flat, no shadows
except the floating tab dock.

| Prototype | SwiftUI | Android counterpart |
|---|---|---|
| iPhone: Chats | `ChatsView`: large left-aligned "Chats" title with the compose button, a search field, `List` of rows (avatar, name with the teal verified mark, one-line preview, time, unread dot; "Needs your approval · €286.00" in amber; "Not verified" in red). The floating dock (Chats, Agents, Devices, Settings) sits above the home indicator. | `ChatsTab`, `TabBar` |
| iPhone: Conversation | `ConversationView` in a `NavigationStack`: a back button labelled "Chats", the centred name with the verified mark and "Verified · 2 devices" under it, video and phone on the right; the thread with day labels, runs of bubbles and "delivered" stamps; the composer bar (attach, text, green lock, send) lifted clear of the home indicator. | `ConversationScreen` |
| iPhone: Face ID unlock | `UnlockView`: the Spike mark, "Unlock Spike", "Look at your iPhone.", the Face ID button, and "Use passphrase instead". | `GateScreen` (locked state) |

Then, in the same pattern, the remaining screens already drawn for Android: create identity, Add a contact (my code,
scan with `AVCaptureSession`/`DataScannerViewController`, paste, share via `ShareLink`), message requests, verify
(safety number), group members and invite, Devices and linking (new device / existing device), Agents (list, approval,
limits, signed log), Settings, backup and restore (`fileExporter` / `fileImporter`), and the call screen.

An **iPad** build comes almost free: `NavigationSplitView` gives the two-pane "Tablet, landscape" layout (list beside the
conversation, a sidebar rail for the tabs) that the Android app now uses at 840dp and wider.

State lives in one `@Observable final class AppModel` mirroring `AppModel.kt` (`status`, `overview`, `tab`, `screen`,
`messages`, `notice`, `refresh()`, `open(_:)`), so the two apps stay easy to compare screen by screen.

## 3. Face ID unlock through the Keychain

Android keeps the passphrase encrypted under an Android Keystore key that needs a strong biometric (`BioLock` in
`Platform.kt`). iOS does the same with the Keychain, more simply:

1. After a successful passphrase unlock, if the person turns on Face ID, store the passphrase as a generic-password item
   with an access control of `kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly` + `.biometryCurrentSet`.
   - `ThisDeviceOnly`: never in iCloud Keychain or a backup.
   - `.biometryCurrentSet`: the item is invalidated if faces/fingers are added or removed, as Android's
     `setInvalidatedByBiometricEnrollment(true)`.
2. To unlock, read the item with an `LAContext` (`localizedReason: "Unlock Spike"`); iOS shows Face ID, then returns the
   passphrase, which goes straight to `Bridge.call("unlock", ["passphrase": …])` and is dropped.
3. "Use passphrase instead" always works; the passphrase alone still unlocks everything.
4. Turning Face ID off deletes the item. Auto-lock (`AUTO_LOCK_MINUTES` on Android) calls `lock` when the app has been in
   the background that long.
5. `NSFaceIDUsageDescription` in `Info.plist`. On devices with Touch ID the same code shows Touch ID.

Optionally the Keychain item can be a key in the Secure Enclave (`kSecAttrTokenIDSecureEnclave`) that wraps the
passphrase, but for a passphrase-sized secret the access-controlled Keychain item is the standard, audited path.

Screen privacy (Android's `FLAG_SECURE`): iOS cannot block screenshots; cover the window with the Spike mark on
`scenePhase == .inactive` so the app switcher thumbnail shows no messages.

## 4. Push: waking the app on iPhone

Android wakes through UnifiedPush (ADR 0028, `docs/unifiedpush.md`): the relay posts one content-free message to a URL the
phone registered with `push_register`. iOS has no UnifiedPush; only **APNs** can wake an app, and only a server holding
an Apple key can send to APNs. The same design carries over with one extra hop:

1. The app registers for remote notifications and gets an APNs device token.
2. It registers with the relay as today (`push_register`), with a URL on a small **push gateway** that holds the APNs
   key (`.p8`, team id, key id), e.g. `https://push.spike.network/apns/<opaque-token>`. The gateway maps the opaque
   token to the APNs token; the relay never sees an Apple identifier, and the gateway never sees message content or
   senders (it gets the same fixed word UnifiedPush gets).
3. **Messages**: the gateway sends an alert push with `mutable-content: 1` and a generic body ("New message"). A
   **Notification Service Extension** (its own target, sharing the data directory through an App Group and the Face ID
   Keychain item through a shared access group) opens the engine, runs `sync`, and rewrites the notification with the
   sender and preview, or leaves it generic if the person chose content-free notifications. Silent background pushes
   are throttled by iOS and must not be relied on.
4. **Calls**: a **PushKit VoIP** push, sent by the gateway with its own topic (`<bundle>.voip`). iOS requires every VoIP
   push to be reported to **CallKit** at once (`reportNewIncomingCall`), so the gateway sends VoIP only for a call
   offer, and the app shows the native incoming-call screen, then connects to the relay to fetch the offer.
5. The relay already rate-limits wake-ups (5 s per device) and only posts to public `https://` URLs.

Things to settle: who runs the gateway (the hosted relay operator, or a Spike-run service shared by all relays), and an
ADR for it, since it is the first piece that needs an Apple-held credential.

## 5. What needs a Mac and an Apple developer account

| Need | Why |
|---|---|
| A Mac with Xcode (current, e.g. 16+) | Only Xcode builds, signs and runs iOS apps; the Rust iOS targets link against the iOS SDK that ships with it. A cloud Mac (MacStadium, GitHub Actions `macos-latest`) works for CI, not for comfortable UI work. |
| Rust targets on the Mac | `rustup target add aarch64-apple-ios aarch64-apple-ios-sim` (and `x86_64-apple-ios` for Intel Macs). |
| An iPhone with Face ID | The simulator fakes Face ID (Features → Face ID → Matching Face) but not APNs VoIP, CallKit fully, the camera, or real network suspension. |
| Apple Developer Program membership (99 USD/year), as an organization for the store | Signing on a device beyond 7-day free profiles, the Push Notifications and PushKit capabilities, the APNs auth key for the gateway, App Groups for the notification extension, TestFlight, and App Store release. |
| App Store review notes | End-to-end encryption export compliance (`ITSAppUsesNonExemptEncryption`, and the annual self-classification report for mass-market encryption), privacy nutrition labels, and a demo account or video for review. |

## 6. Step by step

1. **Engine for iOS (Mac)**: add the `spike-ios` crate (or a feature on `mobile/`) with `spike_open`, `spike_call`,
   `spike_free_string`, `spike_close`; a script `ios/build-native.sh` that builds the device and simulator libraries
   and packs `SpikeEngine.xcframework`. Run `cargo test -p spike-app` on the Mac to check the engine on Apple targets.
2. **Xcode project**: `ios/SpikeConnect.xcodeproj`, SwiftUI app target, iOS 17+, the xcframework, bundled Geist and
   Geist Mono, the colour tokens from `Theme.kt` as an asset catalog with light and dark values. `Bridge.swift` and the
   `Codable` models. A smoke test (`XCTest`) that creates an identity in a temp directory and reads `overview`.
3. **Gate**: create identity, passphrase unlock, lock, auto-lock.
4. **Chats and Conversation** (the two prototype screens): list, search, open, send, replies, reactions, edits and
   deletes, files (`PhotosPicker`, `fileImporter`), read marks. Live updates: a foreground relay connection driven by
   `scenePhase`, polling `sync` as Android's `Listener` does.
5. **Face ID unlock** (the third prototype screen): the Keychain item, the offer after create/unlock, "Use passphrase
   instead", invalidation on biometry change, the app-switcher cover.
6. **Contacts and devices**: Add a contact (QR show/scan/paste/share), message requests, verify, device linking both ways.
7. **Groups**: small and large (MLS) groups, members, invites.
8. **Agents**: list, approval, limits, signed log (same commands as the Android Agents tab).
9. **Push**: the gateway and its ADR; APNs registration; the Notification Service Extension with an App Group;
   PushKit + CallKit for calls.
10. **Calls**: WebRTC audio/video, CallKit integration, audio session handling, the call screen.
11. **iPad**: `NavigationSplitView` two-pane layout, keyboard shortcuts.
12. **Drivers and release**: XCUITest drivers mirroring `android/drive_mobile.mjs` against a local relay (the simulator
    reaches the Mac's `127.0.0.1` directly); TestFlight; App Store submission.

Steps 1–5 give a usable one-to-one messenger matching the three iPhone screens in the prototype; 6–12 bring it to parity
with Android.
