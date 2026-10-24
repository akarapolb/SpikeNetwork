#!/usr/bin/env bash
# Build the Rust library for Android and put it where Gradle packages it.
# Needs: rustup targets x86_64-linux-android and aarch64-linux-android, cargo-ndk, and the Android NDK
# (ANDROID_NDK_HOME). From the repository root: bash android/build-native.sh [--release]
set -euo pipefail
PROFILE=${1:-}
cargo ndk -t x86_64 -t arm64-v8a -o android/app/src/main/jniLibs build -p spike-mobile $PROFILE
