# Run a Spike relay in the cloud (relay + HTTPS + TURN for calls)

This puts three things on one small Linux server: the **relay** (holds encrypted messages for offline devices), **Caddy**
(HTTPS certificate, automatic) and **coturn** (a TURN server, so calls work between different networks).
It was written against the relay's own flags and tests, but **not run on a real server by its author**: expect to adjust
something, and report what.

You need: a small Linux VPS with a public IPv4 address (1 vCPU / 1 GB is enough for a pilot), a domain name you control,
and about 20 minutes.

## 1. Point a name at the server
Make a DNS **A record**, for example `relay.example.org` pointing at the server's public IP. Wait until
`ping relay.example.org` answers with that address (Caddy needs it to get the certificate).

## 2. Open the firewall (and the cloud provider's own firewall or security group)
| Port | Protocol | For |
|---|---|---|
| 80 | TCP | certificate challenge |
| 443 | TCP and UDP | the relay (HTTPS / WebSocket) |
| 3478 | UDP and TCP | TURN |
| 49160-49200 | UDP | TURN media |

## 3. Install Docker
```sh
curl -fsSL https://get.docker.com | sudo sh
```

## 4. Put the code on the server and start it
Copy the source archive to the server (`scp spike-relay-deploy.tar.gz user@server:`), then:
```sh
tar xzf spike-relay-deploy.tar.gz && cd deploy
cp .env.example .env
nano .env        # DOMAIN, PUBLIC_IP, and a secret from:  openssl rand -hex 32
sudo docker compose up -d --build
```
The first build takes a few minutes (it compiles the relay).

## 5. Check it
```sh
sudo docker compose ps                 # relay, caddy, coturn all "running"
sudo docker compose logs relay         # "spike-relay listening on 0.0.0.0:8787 ..."
sudo docker compose logs caddy | tail  # "certificate obtained successfully"
curl -i https://relay.example.org/     # any HTTP answer over a valid certificate is right
```

## 6. Use it
In each app: **Settings, Home relay**, enter `wss://relay.example.org`, then **Test connection** (it says
"certificate verified"). On the desktop, **Settings, Calls** should now say your relay offers a TURN server. A device
publishes its home relay in its signed device list, so contacts learn a new relay address when they next refresh. Then call
from a phone on mobile data to a computer on Wi-Fi: the call screen says **Through a relay** when TURN carried it.

## Notes
- **Keep the secret secret.** Anyone who has `TURN_SECRET` can use your TURN server.
- **Data:** queued messages are in the `relay-data` volume (`/data/relay.db`). Back it up if you care about messages that
  are still waiting; delivered messages are deleted.
- **Update:** put the new source in place, then `sudo docker compose up -d --build`.
- **Wake-up pushes (UnifiedPush):** the relay also sends a content-free wake-up to a push endpoint a device registered, when a
  message waits for it. Turn that off with `--no-push` in `docker-compose.yml`.
- **No Docker?** `docs/deploy-relay.md` builds and runs the relay directly (systemd), and `docs/turn.md` configures coturn
  by hand.
- **Costs and abuse:** TURN carries call traffic through your server: about 40 kbit/s for voice and 1 to 2.5 Mbit/s for video,
  per direction. The coturn quotas in the compose file (`--user-quota`, `--total-quota`, `--max-bps`) limit that; raise them
  if you have many users.
- This is unreviewed prototype software: a pilot relay, not a production service (see `SECURITY.md`).
