# Reproducible builds: where we are

Status 2026-10-24: **not reproducible, not verified.** Nobody has built the same release twice and compared the
bytes. This note says what is pinned, what is not, and what to do so that a reviewer or a user can check that a
binary matches the source. Do not claim "reproducible" until the check at the bottom has actually passed on two
machines.

## What is pinned

| Thing | Pinned by | Gap |
|---|---|---|
| Rust crate versions (both workspaces) | `Cargo.lock`, `desktop/src-tauri/Cargo.lock` committed; CI uses the default (unlocked) `cargo` commands | Build with `--locked` so a stale lockfile is an error. CI does not yet |
| Registry sources | `deny.toml` allows only crates.io, no git sources | |
| Rust toolchain | **not pinned** (`dtolnay/rust-toolchain` "stable" in CI; no `rust-toolchain.toml`) | Pin a version in `rust-toolchain.toml`; compiler version changes the binary |
| GitHub Actions | pinned to commit SHAs in `ci.yml` / `watch.yml` | |
| Android Gradle plugin, Kotlin, Compose BOM | exact versions in `android/build.gradle.kts`, `app/build.gradle.kts` | Transitive Gradle dependencies float; no `gradle/verification-metadata.xml`, no dependency locking |
| Android NDK | README says 27.2.12479018; not enforced | Set `ndkVersion` in `app/build.gradle.kts` |
| Native build for Android | `cargo-ndk` (version not pinned) | Record the version |
| Embedded paths / timestamps | Rust embeds source paths unless remapped; Gradle APKs carry timestamps in some files | See below |

## What to do (each is cheap; none done yet)

1. `rust-toolchain.toml` with an exact version; build with `cargo build --release --locked`.
2. `RUSTFLAGS="--remap-path-prefix=$PWD=/src --remap-path-prefix=$HOME/.cargo=/cargo"` and `-C strip=symbols` for
   release builds, `CARGO_INCREMENTAL=0`.
3. Android: `ndkVersion`, Gradle dependency verification (`./gradlew --write-verification-metadata sha256`), a
   pinned `cargo-ndk`, and build the `.so` in a container image whose digest is recorded. The APK signature
   differs per signer; compare the **unsigned** APK, or compare contents after stripping signatures.
4. Desktop: the Tauri/NSIS installer embeds timestamps; check whether `SOURCE_DATE_EPOCH` is honoured and remove
   what is not.
5. Publish, per release: source commit, toolchain versions, the build container digest, SHA-256 of every artefact,
   and a signed statement of the hashes. Binary transparency (the threat model's "operator compelled to ship a
   backdoored client" row) is **not** built; this is only its first step.

## The check

On two machines with different usernames and paths, from a clean checkout of the same commit and with the same
pinned toolchain and container:

```
cargo build --release --locked -p spike-mobile --target aarch64-linux-android   # via cargo-ndk, see android/build-native.sh
sha256sum target/aarch64-linux-android/release/libspike_mobile.so
```

If the hashes differ, `diffoscope` on the two files shows why. Until the hashes match, say "not reproducible".

## Related

- Release signing for Android: `android/app/build.gradle.kts` reads `android/keystore.properties` or `SPIKE_STOREFILE`,
  `SPIKE_STOREPASSWORD`, `SPIKE_KEYALIAS`, `SPIKE_KEYPASSWORD`. Keep the keystore offline and backed up; losing it
  means users cannot update in place.
- Desktop release and updater: [`../desktop-release.md`](../desktop-release.md).
