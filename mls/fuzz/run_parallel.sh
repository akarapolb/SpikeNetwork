#!/usr/bin/env bash
# Run every fuzz target at once for N seconds (default 1500), one process each, logs in $LOGDIR.
# From mls/fuzz: LOGDIR=/tmp/fuzz bash run_parallel.sh [seconds]. Same Windows PATH note as run_all.sh.
set -u
T=${1:-1500}
LOGDIR=${LOGDIR:-./logs}
mkdir -p "$LOGDIR"
for t in process join key_package policy credential; do
  ( cargo +nightly fuzz run "$t" -- -max_total_time="$T" -max_len=70000 > "$LOGDIR/$t.log" 2>&1; echo "exit $?" >> "$LOGDIR/$t.log" ) &
done
wait
for t in process join key_package policy credential; do
  echo "=== $t: $(grep -E '^Done' "$LOGDIR/$t.log" | tail -1) $(grep -cE 'SUMMARY|panicked' "$LOGDIR/$t.log") crash lines; $(tail -1 "$LOGDIR/$t.log")"
done
