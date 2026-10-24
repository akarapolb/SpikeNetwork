# Calls between different networks: running a TURN server

Two devices on the same Wi-Fi call each other directly. Across networks (one on mobile data, one at home, or a
strict office network) they usually cannot reach each other, so the packets go through a **TURN server** that you
run next to your Spike relay. It only forwards encrypted packets; it cannot hear or see a call. See ADR 0028.

What it learns: that two addresses exchanged some amount of data for some minutes. Not who they are (logins carry
only a short label of the device, expire in an hour, and come from your relay), and not the content.

## 1. Install coturn

Debian/Ubuntu: `apt install coturn`. Any machine with a public address and UDP open will do, usually the one that
runs the relay.

## 2. Configure it (`/etc/turnserver.conf`)

```
# Logins are minted by the Spike relay from this shared secret (the "TURN REST API"): no user database.
use-auth-secret
static-auth-secret=CHANGE-THIS-LONG-RANDOM-SECRET
realm=turn.example.org
server-name=turn.example.org

listening-port=3478
tls-listening-port=5349
cert=/etc/letsencrypt/live/turn.example.org/fullchain.pem
pkey=/etc/letsencrypt/live/turn.example.org/privkey.pem
min-port=49160
max-port=49200

# Calls only: forward between the two devices, never into your own network.
no-multicast-peers
no-cli
no-tcp-relay
denied-peer-ip=10.0.0.0-10.255.255.255
denied-peer-ip=172.16.0.0-172.31.255.255
denied-peer-ip=192.168.0.0-192.168.255.255
denied-peer-ip=127.0.0.0-127.255.255.255
denied-peer-ip=169.254.0.0-169.254.255.255

# Keep one bad client from using all your bandwidth.
user-quota=4
total-quota=200
bps-capacity=0
max-bps=3000000
fingerprint
stale-nonce
```

Open UDP and TCP 3478, TCP 5349 and UDP 49160-49200 in the firewall. If the relay machine is behind NAT, add
`external-ip=PUBLIC/PRIVATE`.

## 3. Tell the relay about it

```
spike-relay ... \
  --turn-url turn:turn.example.org:3478?transport=udp \
  --turn-url turns:turn.example.org:5349?transport=tcp \
  --turn-secret CHANGE-THIS-LONG-RANDOM-SECRET        # or SPIKE_RELAY_TURN_SECRET in the environment
```

`--turn-ttl-secs` (default 3600) is how long a login lasts. Devices ask the relay for one when a call starts, so a
relay without these flags simply has no TURN and calls stay direct-only.

## 4. Check it

Open Settings on the desktop app: the Calls card says "Your relay offers a TURN server". Turn on "Always use the
relay for calls" on two devices and call: the call screen says "Through a relay". `node desktop/drive_turn.mjs`
does this with a stand-in server (`relay/examples/turn_test_server.rs`) if you want to see it work first.

## Notes

- "Always use the relay for calls" sends no direct address to the other person, so they cannot learn your IP
  address from the call; your relay's operator sees the two addresses and the data volume instead.
- Use `turns:` (TLS, port 5349) as well as plain `turn:`: strict networks often allow only TLS on port 443. You can
  run coturn's TLS listener on 443 if nothing else uses it.
- Chromium (and so WebView2 on the desktop) does not use a TURN server on 127.0.0.1 or a private test address that
  it treats as loopback; use the machine's real address when testing.
