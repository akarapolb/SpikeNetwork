#!/usr/bin/env bash
# Phase 0 demo: two devices and one relay. Bob is offline while Alice sends.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q
BIN=target/debug
D=.spike-demo; rm -rf "$D"; mkdir -p "$D"
A="$BIN/spike --home $D/alice"; B="$BIN/spike --home $D/bob"
export SPIKE_RELAY=ws://127.0.0.1:8799

$BIN/spike-relay --listen 127.0.0.1:8799 2>"$D/relay.log" & RELAY=$!
trap 'kill $RELAY 2>/dev/null' EXIT
sleep 0.5

echo "== identities"; $A init >/dev/null; $B init >/dev/null
ACARD=$($A card 2>/dev/null); BCARD=$($B card 2>/dev/null)
AFP=$($A card 2>&1 >/dev/null | sed -n 's/^fingerprint: //p')
BFP=$($B card 2>&1 >/dev/null | sed -n 's/^fingerprint: //p')

echo "== pairing (fingerprints compared out of band)"
$A add bob "$BCARD" >/dev/null;   $A verify bob "$BFP"
$B add alice "$ACARD" >/dev/null; $B verify alice "$AFP"

echo "== alice sends while bob is offline"
$A send bob "meet at the usual place, 3pm"

echo "== bob comes online later"
$B recv

echo "== bob reconnects: acked message must not come back"
$B recv
echo "== relay log (no content):"; cat "$D/relay.log"
