# Running a Spike relay

A relay is the always-on server that holds encrypted messages for devices that are offline. It never sees message text, files or contacts, but it does see **metadata**: which devices connect, when, and how much they send (see `docs/threat-model.md`). You can run one for your own group; nobody has to trust a relay operator with content.

**What has and has not been tested.** Tested: the relay with a TLS certificate and the desktop app trusting a self-signed certificate (`desktop/src-tauri/tests/api.rs`, `cli/tests/tls.rs`, and a hand run with two app windows); `scripts/demo.sh`. **Not tested here:** the systemd unit, certbot renewal and the reverse-proxy recipes below, because they need a real host. Treat them as a starting point and report what breaks. The relay is unreviewed prototype software: a pilot relay, not a production service.

## 1. Build it
On the server (or any machine with the same CPU architecture and libc):
```sh
git clone https://github.com/akarapolb/SpikeNetwork.git && cd SpikeNetwork
cargo build --release -p spike-relay
sudo install -m 0755 target/release/spike-relay /usr/local/bin/spike-relay
```

## 2. Flags
| Flag | Meaning | Default |
|---|---|---|
| `--listen ADDR` | Address and port | `127.0.0.1:8787` |
| `--db FILE` (or `SPIKE_RELAY_DB`) | SQLite file for queued messages and prekeys. **Without it everything is lost on restart.** | none (memory) |
| `--tls-cert FILE` `--tls-key FILE` | PEM certificate chain and private key: the relay serves `wss://` itself | none |
| `--insecure-no-tls` | Allow plain WebSocket on a non-loopback address (only behind a TLS-terminating proxy) | off |
| `--ttl-secs N` | How long an unacknowledged message is kept | 604800 (7 days) |
| `--queue-cap N` | Most queued messages per recipient device | 1000 |
| `--group-max-entry N` | Largest entry in a group mailbox, in bytes (large groups, ADR 0025) | 262144 (256 KiB) |
| `--group-message-days N` `--group-handshake-days N` | Days a group message is kept, and a group commit (and a mailbox after its last append). Commits are kept far longer on purpose: a device that misses one cannot read anything afterwards | 30 / 180 |
| `--group-max-bytes N` | Bytes one group mailbox may hold; past it the oldest messages (never commits) are dropped | 67108864 (64 MiB) |
| `--group-max-mailboxes-per-device N` | Group mailboxes one device may create | 200 |
| `--max-group-subs-per-conn N` | Group mailboxes one connection may listen to | 256 |
| `--max-conns-per-ip N` | Connections one client address may hold open (0 = unlimited) | 64 |
| `--conn-rate-per-ip N` | New connections one address may open per minute (0 = unlimited) | 300 |
| `--req-rate R` `--req-burst N` | Requests per second one connection may sustain, and the burst allowed (0 rate = unlimited) | 50 / 200 |
| `--trusted-proxy CIDR` | Reverse proxy (address or network, repeat or comma-separate) whose `X-Forwarded-For` is believed. **Set this when behind a proxy**, or every client looks like the proxy and shares one limit | none |
| `--metrics-listen ADDR` | Serve `/healthz` and `/metrics` (plain HTTP, numbers only) on this address | off |
| `--metrics-token TOKEN` (or `SPIKE_RELAY_METRICS_TOKEN`) | Require `Authorization: Bearer TOKEN` for `/metrics`. Mandatory on a non-loopback metrics address | none |

**Group mailboxes (large groups, ADR 0025; no client uses them yet).** For each large group the relay holds one or two random mailbox ids, the mailbox's *public* key, and an append-only log of ciphertext with its sizes and times. It holds no group id, no name, no member list and no content; it does see which authenticated devices create, read, append to and listen to a mailbox, as it sees which devices exchange messages today. Plan disk for the longer retention: a busy mailbox can hold up to `--group-max-bytes`, and commits (small) stay for `--group-handshake-days` even after the group moves on. `/metrics` gains `group_mailboxes`, `group_entries`, `group_bytes` and counters for appends, bytes, reads and pushes; none carries an id. The relay also stores opaque MLS key packages a device publishes (up to 200 one-time ones of at most 16 KiB each, plus one reusable last-resort package) and hands each one-time package out once; `/metrics` shows only a count. The database schema moves to version 3 the first time this relay version opens it; an older database is copied to `<file>.before-v2` first, and a database written by a newer relay is refused.

Fixed in code for now (not flags): messages up to 64 KiB, attachments kept 30 days with 1 GiB per device, **at most 4 live connections per device** (the oldest beyond that is closed), and a connection that has sent nothing for **120 seconds** is closed (waiting clients ping every 25 s, so only dead or abandoned connections reach that). The relay refuses plain `ws://` on a non-loopback address unless you pass `--insecure-no-tls`.

## 3. Pick how clients will trust it
Clients (the app and the CLI) only connect with TLS unless the relay is on the same computer. Choose one:

**A. A real hostname and a public certificate (recommended for anything beyond your own LAN).** You need a DNS name pointing at the server and port 80 open once for the challenge:
```sh
sudo certbot certonly --standalone -d relay.example.org \
  --deploy-hook /usr/local/bin/spike-relay-renew.sh
```
The relay reads its certificate **only at startup**, so renewal must restart it. `/usr/local/bin/spike-relay-renew.sh` (mode 0755):
```sh
#!/bin/sh
set -e
install -d -m 0750 -o root -g spike-relay /etc/spike-relay
install -m 0640 -o root -g spike-relay "$RENEWED_LINEAGE/fullchain.pem" /etc/spike-relay/fullchain.pem
install -m 0640 -o root -g spike-relay "$RENEWED_LINEAGE/privkey.pem"   /etc/spike-relay/privkey.pem
systemctl restart spike-relay
```
Run it once by hand after the first `certbot` run. A restart drops live connections; apps reconnect by themselves, and nothing is lost when the relay uses `--db`. Clients need no extra setup: public certificates are trusted already.

**B. A private or self-signed certificate (a home or office network without a domain).**
```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 \
  -subj "/CN=spike-relay" \
  -addext "subjectAltName=DNS:relay.lan,IP:192.168.1.10" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -keyout relay.key -out relay.crt
```
Put every name or IP clients will use in `subjectAltName`. Keep `CA:FALSE` (a certificate marked as a CA is rejected as a server certificate). Copy only `relay.crt` (never the key) to each computer. In the desktop app: **Settings → Trust for a relay you run yourself → Choose certificate file**. For the CLI: `--relay-ca relay.crt`. The certificate expires: put the date in a calendar, and every client must trust the new file when you rotate.

**C. Behind a reverse proxy that terminates TLS** (Caddy, nginx). Run the relay on loopback and let the proxy hold the certificate:
```sh
spike-relay --listen 127.0.0.1:8787 --db /var/lib/spike-relay/relay.db
```
Caddy (`Caddyfile`): `relay.example.org { reverse_proxy 127.0.0.1:8787 }`. nginx needs the WebSocket headers and a long read timeout: `proxy_http_version 1.1; proxy_set_header Upgrade $http_upgrade; proxy_set_header Connection "upgrade"; proxy_read_timeout 3600s;`. The app keeps one connection open so messages arrive instantly. While it waits it sends a small ping every 25 seconds, which keeps typical proxy idle timeouts (nginx's default is 60 seconds) from closing it; if a proxy has a shorter timeout the app reconnects within a second and loses nothing, but it is wasteful, so keep the idle timeout above 30 seconds (an hour is fine). If the proxy is on another machine, you need `--insecure-no-tls` and a private network between them.

## 4. Run it as a service (systemd)
```sh
sudo useradd --system --home-dir /var/lib/spike-relay --shell /usr/sbin/nologin spike-relay
```
`/etc/systemd/system/spike-relay.service`:
```ini
[Unit]
Description=Spike Network relay
After=network-online.target
Wants=network-online.target

[Service]
User=spike-relay
Group=spike-relay
ExecStart=/usr/local/bin/spike-relay --listen 0.0.0.0:443 \
  --db /var/lib/spike-relay/relay.db \
  --tls-cert /etc/spike-relay/fullchain.pem --tls-key /etc/spike-relay/privkey.pem
AmbientCapabilities=CAP_NET_BIND_SERVICE
StateDirectory=spike-relay
StateDirectoryMode=0700
ReadOnlyPaths=/etc/spike-relay
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
Restart=on-failure
RestartSec=2
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```
```sh
sudo systemctl daemon-reload && sudo systemctl enable --now spike-relay
sudo journalctl -u spike-relay -f
```
The log has start-up lines and connection errors, never content. Open only the relay's port in the firewall (443, or whatever you chose).

## 5. Point clients at it
Desktop app: **Settings → Home relay** → `wss://relay.example.org` → **Test connection** (it says "certificate verified" on success). CLI: `export SPIKE_RELAY=wss://relay.example.org`. Everyone who talks to each other can use different relays; each device's home relay is published in its signed device list.

## 6. Operating it
- **Disk:** put the database on an encrypted volume. The file holds ciphertext and metadata (recipient device ids, sizes, times) until messages are acknowledged or expire; the relay does not encrypt it. It is created with mode 0600.
- **Backups:** the queue is transient. If you lose the file, undelivered messages are lost; clients publish fresh prekeys the next time they connect. A nightly copy of the file while the service runs is safe (SQLite WAL), but you only need it to preserve messages that are still waiting.
- **Upgrades:** build the new binary, `sudo install` it, `sudo systemctl restart spike-relay`. The database carries a schema version and is upgraded on start. Before changing a file that holds data, the relay copies it to `<db>.before-v<N>` (delete the copy once you are satisfied). A database written by a *newer* relay is refused, not downgraded: the relay exits with a message.
- **Capacity:** a small VPS is enough for a pilot. Attachments are the heavy part: up to 1 GiB per device for 30 days.
- **Monitoring:** with `--metrics-listen 127.0.0.1:9100`, `GET /healthz` answers 200 when the database responds (use it for a load balancer or uptime check), and `GET /metrics` returns Prometheus-format counters: connections, sign-ins, refusals by reason, messages and bytes accepted, error replies by code, queue depth and stored bytes. They are numbers only: no device id, address or content appears in any name or label (tested). Also watch disk space and the certificate's expiry date.

## 7. Limits to know before exposing it
- **Anyone can register a device and use quota.** There is no allow-list. Abuse controls are the per-recipient queue cap, the per-device attachment quota, the per-address connection and rate limits, and the per-connection request rate (ADR 0022). They slow abuse from one address; they do not stop a determined attacker with many addresses, and the relay cannot see who sends a message, so there is no per-sender limit. Enrolling devices per organisation is designed but not built (ADR 0023). For a pilot, run the relay for your own group and put it behind a VPN or an IP allow-list.
- **Dead connections are noticed within about 40 seconds** (a ping goes unanswered for 15 s), and the app reconnects; a router that silently drops an idle connection therefore delays delivery by at most that long.
- **One node, one file:** no replication or high availability. Database calls run on blocking threads, so a slow disk delays only the connection that asked, but all requests still share one SQLite connection.
- **Metadata is visible** to the operator and, partly, to network observers: device ids, timing and sizes. Plain `ws://` (allowed only by explicit opt-in) additionally exposes the connection itself.
- **Unreviewed code.** Nothing here has had an independent security review.

## 8. Troubleshooting
| Symptom | Cause and fix |
|---|---|
| `invalid peer certificate: UnknownIssuer` | The relay's certificate is private or self-signed: trust it (section 3B) |
| `refusing plain ws:// to <host>` | Use `wss://`, or allow plain connections on a network you control (Settings, and only there) |
| `TLS handshake ... failed` with a public certificate | The name in the address must match the certificate; check the date and clock |
| The log shows a reconnect every 30 seconds or so | A proxy or firewall is closing connections that look idle: raise its idle timeout above 30 s (section 3C). Nothing is lost, delivery is just wasteful |
| Relay exits at start with "file not found" | The certificate or key path is wrong or unreadable by the service user |
| Everything lost after restart | The relay was started without `--db` |
