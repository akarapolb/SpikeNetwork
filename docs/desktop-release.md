# Releasing the desktop app: signing and automatic updates

**Status:** the update channel is set up for this repository's public GitHub releases. The signing key exists on the release maintainer's computer; the private key must be added as a repository secret before the release workflow can publish anything (see below). What has been run: the Windows installer and the Linux packages were built locally, and the updater signature check runs on every download. What has not: a real update from one installed version to the next through GitHub, and anything on macOS.

## What exists
- `.github/workflows/release.yml`: pushing a tag `v<version>` (for example `v0.1.0`) builds the Windows installer (NSIS) and the Linux packages (AppImage and deb), signs the updater packages, writes `latest.json` with `scripts/make-update-manifest.mjs`, and publishes a GitHub release with all of them. A manual run of the workflow builds and uploads the packages as run artifacts and publishes nothing.
- Automatic updates in the app (`desktop/src-tauri/src/updates.rs`, on by default). The app checks the newest release a minute after start-up and every six hours, downloads a newer version, checks its signature against the public key in `desktop/src-tauri/tauri.conf.json`, and keeps it. It installs only when the person presses **Restart to update** in the banner (or **Install and restart** in Settings), never during a call. **Settings → Updates → Check for updates automatically** turns the background checks off; a manual check still works.
- The update address is `https://github.com/akarapolb/SpikeNetwork/releases/latest/download/latest.json`. The release must be a published one (not a draft or pre-release) for installed copies to see it.

## Two different signatures (do not mix them up)
1. **Code signing** (Windows Authenticode, later Apple Developer ID). Windows checks it when someone runs the installer; without it SmartScreen warns. The release workflow signs with a certificate only if `WINDOWS_CERTIFICATE` and `WINDOWS_CERTIFICATE_PASSWORD` are set.
2. **Update signing** (a minisign key made by Tauri). The *app* checks each update against the public key compiled into it. The key pair is ours alone. Lose the private key and existing installs can never be updated again; leak it and anyone who can publish a release can push an update to every install.

## Where the update key is
- Private key: `C:\Users\Avalant\.tauri\spike-connect-updater.key` on the maintainer's computer, with **no password** (the workflow takes the file's content as `TAURI_SIGNING_PRIVATE_KEY`). Keep a second copy somewhere safe, offline.
- Public key: `C:\Users\Avalant\.tauri\spike-connect-updater.key.pub`, already in `tauri.conf.json` (`plugins.updater.pubkey`).
- Never commit the private key. If it is ever exposed, make a new pair, change `pubkey` in a new release, and tell users to install that release by hand: installs signed by the old key cannot be updated to it.

## Steps to publish a release
1. Add the repository secret `TAURI_SIGNING_PRIVATE_KEY` with the contents of the private key file. (Set `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` only if the key has a password.) GitHub → Settings → Secrets and variables → Actions.
2. Raise `version` in `desktop/src-tauri/tauri.conf.json` and `desktop/src-tauri/Cargo.toml` (both the same). An update is offered only when the release's version is higher than the installed one.
3. Commit, then tag and push: `git tag v0.1.0 && git push origin v0.1.0`. The workflow's `publish` job refuses a tag that does not match `tauri.conf.json`.
4. Check the release page: the Windows `.exe`, the Linux `.AppImage` and `.deb`, `latest.json`, and the `.sig` files.

## Building locally (for testing)
```bash
cd desktop/src-tauri
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/spike-connect-updater.key)"
cargo tauri build --bundles nsis --config tauri.updater.conf.json   # Windows
cargo tauri build --bundles deb,appimage --config tauri.updater.conf.json   # Linux
```
Without the key the build with `--config tauri.updater.conf.json` fails on purpose: an updater package that is not signed would be refused by every install.

## Steps to sign on Windows (optional, not yet done)
1. Buy a code-signing certificate (an EV certificate gets SmartScreen's trust fastest). Export it as a `.pfx` with a password.
2. Add secrets `WINDOWS_CERTIFICATE` (the `.pfx` as base64: `base64 -w0 cert.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD`. The workflow imports it for the build and removes the file.
3. Check on a clean computer: right-click the installer, Properties, Digital Signatures.

## macOS (not built)
Removed from the release workflow for now. It needs an Apple Developer account, a Developer ID certificate and notarisation; without them macOS will not open the app by double-click.

## Linux
The AppImage is the self-updating package. The deb is for installing by hand or from a package manager and is not updated by the app. Linux builds are made on Ubuntu 24.04 (the release runner); the window and calls have not been exercised on a Linux desktop.

## What an update cannot do
- It cannot read or change your identity: keys and history live in the profile folder, not next to the program.
- It runs with the same permissions as the app. A signed update is as trusted as the signing key, so protect that key like the identity of the product.
- It tells GitHub, on each check, this computer's address and that it runs Spike. Settings turns the background checks off.
