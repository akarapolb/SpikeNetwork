#!/usr/bin/env bash
# Drive the real binaries: a relay, and three people (alice, bob, carol) in a large group.
# Usage (from the repository root, after `cargo build -p spike-cli -p spike-relay`): bash mls/demo_large.sh
set -euo pipefail
BIN=target/debug
TMP=$(mktemp -d)
trap 'kill $RELAY 2>/dev/null || true; rm -rf "$TMP"' EXIT
export SPIKE_RELAY=ws://127.0.0.1:18787
echo "correct horse battery" > "$TMP/pass"
$BIN/spike-relay --listen 127.0.0.1:18787 >"$TMP/relay.log" 2>&1 &
RELAY=$!
sleep 1
s() { local who=$1; shift; SPIKE_HOME="$TMP/$who" $BIN/spike --passphrase-file "$TMP/pass" "$@"; }
for p in alice bob carol; do s $p init >/dev/null; s $p keys >/dev/null; done
card() { s "$1" card | grep -o 'spike2:[0-9a-f]*' | head -1; }
s alice add bob "$(card bob)" >/dev/null;   s bob add alice "$(card alice)" >/dev/null
s alice add carol "$(card carol)" >/dev/null; s carol add alice "$(card alice)" >/dev/null
for p in bob carol; do s $p large keys --count 3; done

echo "--- alice creates the group and invites bob and carol"
s alice large new Org
s alice large add Org bob
s alice large add Org carol
for p in bob carol; do echo "--- $p receives:"; s $p recv --idle-ms 600; s $p large list; s $p large accept Org; done

echo "--- alice speaks; bob and carol (who are not each other's contacts) read and answer"
s alice large send Org "welcome to the org"
for p in bob carol; do s $p large sync; done
s bob large send Org "thanks alice"
s carol large sync
s alice large sync

echo "--- alice removes carol; bob follows; carol is cut off"
s alice large remove Org carol
s bob large sync
s alice large send Org "just bob and me now"
s bob large sync
s carol large sync
s carol large list
echo "--- alice's history"
s alice large history Org
