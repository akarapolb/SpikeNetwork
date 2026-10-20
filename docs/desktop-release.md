# Releasing the desktop app: signing and updates

**Status: scaffolding. Nothing here has been run with real keys, a real certificate or a real update server**, because those are yours to create. What is tested: the app builds with and without the updater, and the Settings screen and commands answer correctly in each case. What is not: a signed installer, an update download, and anything on macOS.

## What exists
- `.github/workflows/release.yml`: manual builds for Windows (NSIS), Linux (deb, AppImage) and macOS (dmg). Windows signs if you add the secrets below; the updater is an option. Linux and macOS jobs may fail without being treated as a failure of the run.
- A Cargo feature `updater` in `desktop/src-tauri` that adds Tauri's updater plugin, and `tauri.updater.conf.json` (a template you fill in).
- A **Check for updates** button in Settings. **The app never checks by itself**: a check tells the update server that this computer runs Spike and when, which is metadata you chose not to collect by default. A build without the feature says it cannot update itself.

## Two different signatures (do not mix them up)
1. **Code signing** (Windows Authenticode, Apple Developer ID). The operating system checks it when someone runs the installer; without it Windows SmartScreen warns and macOS refuses to open the app. Needs a certificate you buy from a certificate authority (Windows) or an Apple Developer account (macOS).
2. **Update signing** (a minisign-style key made by Tauri). The *app* checks it before installing an update, using the public key compiled into it. It is free and yours alone. If you lose the private key you can never ship an update to existing installs; if it leaks, anyone who also controls your update server could ship one.

## Steps to turn the update channel on
1. Make the key pair on a trusted computer: `cargo tauri signer generate -w spike-update.key`. Keep the private file and its password in a password manager and a second safe place. The command prints the public key.
2. Put the public key in `desktop/src-tauri/tauri.updater.conf.json` (`plugins.updater.pubkey`) and the address of your update endpoint in `endpoints`. Use `https` only.
3. Add repository secrets `TAURI_SIGNING_PRIVATE_KEY` (the file's content) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
4. Run the **Desktop release build** workflow with *updater* ticked. Alongside each installer it produces a `.sig` file and the updater artifacts.
5. Host what the endpoint must return. Tauri asks `GET <endpoint>` (with `{{target}}`, `{{arch}}` and `{{current_version}}` filled in) and expects either `204 No Content` (up to date) or JSON: `{"version":"0.0.2","notes":"…","pub_date":"…","url":"https://…/installer","signature":"<contents of the .sig file>"}`. A static file per platform on any HTTPS host works; no server code is needed. Do not log client addresses on it.
6. Versioning: raise `version` in `tauri.conf.json` for every release; an update is offered only if the endpoint's version is higher than the installed one.

## Steps to sign on Windows
1. Buy a code-signing certificate (an EV certificate gets SmartScreen's trust fastest; a standard one builds trust with downloads). Export it as a `.pfx` with a password.
2. Add secrets `WINDOWS_CERTIFICATE` (the `.pfx` encoded as base64: `base64 -w0 cert.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD`. The workflow imports it for the build and removes the file. If your certificate lives on a hardware token or a cloud signing service, use Tauri's `signCommand` instead; that needs a runner you control or the vendor's action, and is not set up here.
3. Check on a clean computer: right-click the installer, Properties, Digital Signatures.

## macOS (not run)
Needs an Apple Developer account, a Developer ID Application certificate and notarisation (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` are the names Tauri reads). They are not wired into the workflow because nothing here can be tested without them. Unsigned, the dmg builds but macOS will not open it by double-click.

## Linux
The deb and AppImage need WebKitGTK 4.1. The app builds and its tests pass on Ubuntu under WSL; the window itself has not been seen on a Linux desktop. AppImages can self-update through the same updater; a deb is updated by the user's package manager.

## What an update cannot do
- It cannot read or change your identity: keys and history live in the profile folder, not next to the program.
- It runs with the same permissions as the app. A signed update is as trusted as the signing key, so protect that key like the identity of the product. The roadmap's reproducible builds and binary transparency are the longer answer.
