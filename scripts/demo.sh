#!/usr/bin/env bash
# Phase 0 demo: two devices and one relay. Bob is offline while Alice sends.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q
BIN=target/debug
D=.spike-demo; rm -rf "$D"; mkdir -p "$D"
# Each device has its own passphrase file (state is encrypted at rest).
echo "alice demo passphrase" > "$D/alice.pass"; echo "bob demo passphrase" > "$D/bob.pass"
A="$BIN/spike --home $D/alice/state --passphrase-file $D/alice.pass"
B="$BIN/spike --home $D/bob/state --passphrase-file $D/bob.pass"
export SPIKE_RELAY=ws://127.0.0.1:8799

start_relay() { $BIN/spike-relay --listen 127.0.0.1:8799 --db "$D/relay.db" 2>>"$D/relay.log" & RELAY=$!; sleep 0.5; }
start_relay
trap 'kill $RELAY 2>/dev/null' EXIT

echo "== identities (encrypted state, Argon2id)"; $A init >/dev/null; $B init >/dev/null
echo "files on disk:"; ls "$D/alice/state"; echo -n "first bytes of identity.enc: "; head -c 4 "$D/alice/state/identity.enc"; echo
echo -n "wrong passphrase: "; echo "nope nope nope" > "$D/bad.pass"
$BIN/spike --home "$D/alice/state" --passphrase-file "$D/bad.pass" contacts 2>&1 || true
ACARD=$($A card 2>/dev/null); BCARD=$($B card 2>/dev/null)
AFP=$($A card 2>&1 >/dev/null | sed -n 's/^fingerprint: //p')
BFP=$($B card 2>&1 >/dev/null | sed -n 's/^fingerprint: //p')

echo "== pairing (fingerprints compared out of band)"
$A add bob "$BCARD" >/dev/null;   $A verify bob "$BFP"
$B add alice "$ACARD" >/dev/null; $B verify alice "$AFP"

echo "== bob registers one-time prekeys with the relay, then goes offline"
$B keys

echo "== alice sends while bob is offline (first message starts a ratcheting session)"
$A send bob "meet at the usual place, 3pm"
$A send bob "bring the documents"

echo "== relay is killed with SIGKILL and restarted (queue is on disk)"
kill -9 $RELAY; wait $RELAY 2>/dev/null || true
start_relay

echo "== bob comes online later"
$B recv

echo "== bob replies over the same session; alice receives"
$B send alice "on my way"
$A recv

echo "== bob reconnects: acked messages must not come back"
$B recv
echo "== relay log (no content):"; cat "$D/relay.log"
