#!/usr/bin/env bash
# Run every fuzz target for N seconds each (default 180). Needs: rustup nightly, cargo-fuzz.
# From mls/fuzz: bash run_all.sh [seconds]
set -u
# On Windows the sanitizer runtime must be on PATH, e.g. (Visual Studio Build Tools):
#   export PATH="/c/Program Files (x86)/Microsoft Visual Studio/18/BuildTools/VC/Tools/MSVC/<version>/bin/Hostx64/x64:$PATH"
T=${1:-180}
for t in process join key_package policy credential; do
  echo "=== $t ($T s)"
  cargo +nightly fuzz run "$t" -- -max_total_time="$T" -max_len=70000 2>&1 | tail -6
done
echo "=== done"
