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
export SPIKE_RELAY=wss://127.0.0.1:8799

# Self-signed certificate for the demo relay; devices trust it explicitly.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 \
  -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" -addext "basicConstraints=critical,CA:FALSE" \
  -keyout "$D/relay.key" -out "$D/relay.crt" 2>/dev/null
export SPIKE_RELAY_CA="$D/relay.crt"

start_relay() { $BIN/spike-relay --listen 127.0.0.1:8799 --db "$D/relay.db" --tls-cert "$D/relay.crt" --tls-key "$D/relay.key" 2>>"$D/relay.log" & RELAY=$!; sleep 0.5; }
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

echo "== stored history on bob's device (encrypted at rest)"
$B history alice

echo "== bob reconnects: acked messages must not come back"
$B recv
echo "== alice links a laptop: the laptop shows a link code, both screens show the same safety code"
echo "alice laptop passphrase" > "$D/alice-laptop.pass"
L="$BIN/spike --home $D/alice-laptop/state --passphrase-file $D/alice-laptop.pass"
$L init >/dev/null
$L link-new --yes --timeout-secs 30 >"$D/link.out" 2>"$D/link.err" & LINKPID=$!
CODE=""
for _ in $(seq 100); do CODE=$(grep -m1 '^spikelink1:' "$D/link.out" || true); [ -n "$CODE" ] && break; sleep 0.1; done
echo "phone (existing device):"; $A link-device "$CODE" --yes | sed "s/^/  /"
wait $LINKPID
echo "laptop (new device):"; grep "^safety code" "$D/link.out" | sed "s/^/  /"
grep '^linked' "$D/link.out"
echo "laptop history with bob:"; $L history bob

echo "== bob writes once; both of alice's devices receive it"
$B send alice "welcome to your laptop"
echo -n "phone:  "; $A recv
echo -n "laptop: "; $L recv

echo "== bob sends a file; it is encrypted before upload and checked after download"
printf 'Floor 3 layout, rev C\n%.0s' $(seq 2000) > "$D/plan.txt"
$B send-file alice "$D/plan.txt" --caption "updated plan"
$L recv
FILEID=$($L history bob | sed -n 's/.*id \([0-9a-f]\{8\}\).*/\1/p' | tail -1)
$L fetch "$FILEID" --out "$D/plan-received.txt"
cmp "$D/plan.txt" "$D/plan-received.txt" && echo "received file is identical"
echo -n "file content visible in relay storage? "; if grep -qa "Floor 3 layout" "$D"/relay.db*; then echo YES; else echo no; fi

echo "== plain ws:// to a non-loopback host is refused by the client"
SPIKE_RELAY=ws://relay.example.com:8787 $B recv 2>&1 || true
echo "== relay log (no content):"; cat "$D/relay.log"
