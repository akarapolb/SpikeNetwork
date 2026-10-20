# ADR 0022: Relay hardening for a shared relay

Status: **Proposed** (implemented)
Date: 2026-10-20

## Problem
A relay that strangers can reach needs four things the single-group relay did not: limits on how much one client can ask for, database work that cannot stall everyone, a database that can change shape safely, and numbers an operator can watch without being able to read anyone's messages. This is "slice 0" of `docs/hosted-relay-plan.md`; none of it needs a product decision.

## Decision

**Limits attach to what the relay can see.** It cannot see who sends a message, only the network address of a connection, and how fast that connection asks.
- *Per address:* at most `max_conns_per_ip` open connections (default 64) and `conn_rate_per_ip` new connections a minute (default 300). The check happens before the WebSocket handshake, so a refused connection costs one accept.
- *Per connection:* a token bucket (`req_rate` 50 a second, `req_burst` 200). A request costs 1, an acknowledgement 0.25, a ping nothing, so keepalive never starves. A refused request gets `rate_limited` and the connection stays usable; after 200 refusals in a row the relay hangs up.
- *Behind a proxy:* every connection comes from the proxy's address. `--trusted-proxy` lists the proxies whose `X-Forwarded-For` header is believed; the address used is the **rightmost entry that is not itself a trusted proxy**, because a proxy appends the address it saw and anything to the left may be client-written. A connection from anyone else has the header ignored. IPv4-mapped IPv6 addresses count as the IPv4 address. A connection from a trusted proxy is admitted after the handshake (when the header is known), so for those the refusal is a close right after the upgrade.
- Limits of 0 mean unlimited. The defaults are generous on purpose: a normal client uses a handful of requests a minute.

**SQLite off the async threads.** Every request after sign-in, registration and the periodic purge run in `spawn_blocking`. One slow query now delays its own connection, not the runtime. The store is still one SQLite connection behind a mutex, so heavy load still serialises on the database; a pool or a different engine is a hosting-scale decision (plan slice 3).

**Schema migrations.** The database stores `PRAGMA user_version`. Each migration runs in a transaction and sets the version with it. A database from before versioning (version 0 with tables) is treated as version 1. Version 2 adds indexes on `expires` so the purge stops scanning whole tables. Before changing a file that holds data the relay writes `<db>.before-v<N>` with `VACUUM INTO`. A database with a *newer* version than the relay understands is refused with a clear message and left untouched.

**Content-free operator endpoints.** `--metrics-listen ADDR` serves plain HTTP: `GET /healthz` (200 if the database answers, else 503) and `GET /metrics` (Prometheus text). Metric names are fixed; the only label is the error code from a fixed list. No device id, address or content can appear, and a test checks that the ids and payload used in it are absent from the output. `--metrics-token` requires a bearer token (compared in constant time); the relay refuses to expose `/metrics` on a non-loopback address without one. `/healthz` stays open for load balancers.

## Not done, on purpose
- **Per-sender limits.** Not possible: the relay does not know the sender. Spam is limited by the recipient's queue cap and by message requests on the client side (ADR 0021).
- **Defence against many addresses.** Per-address limits stop one noisy client or a small botnet; a large one needs a front-line service (CDN, firewall), and tenant enrollment (ADR 0023) which makes an unknown device unable to use quota at all.
- **TLS on the metrics port.** Keep it on loopback or a private network; it holds no content but is for the operator.
- **A connection pool, replication, backups on a schedule.** Plan slice 3.

## Tested
`relay/src/store.rs` (new database at the current version; an old database is upgraded, keeps its data, and is backed up first; reopening makes no second backup; a newer database is refused and not downgraded), `relay/src/limits.rs` (networks, the forwarded-header rules, per-address caps freed on drop, per-minute rate, bucket), `relay/src/metrics.rs`, and `cli/tests/hardening.rs` (connection cap and release, connection rate, request rate with ping exempt and recovery, the header honoured only from a trusted proxy including a forged left entry, health and metrics with a content-leak check, token required, twenty busy connections finishing promptly on a file database).
Also: `cli/tests/ownsync.rs` had two tests that depended on 400 ms and 2 s expiry times and failed intermittently on a loaded machine (they failed on the code before this change too). Their times were widened.

## Limits (honest list)
- Defaults are guesses until a pilot measures real use.
- The streak and bucket numbers are config, but the 200-refusal hang-up and the 64-connections-per-IP default are not tuned for large shared NATs: an office behind one address may need a higher `--max-conns-per-ip`.
- The metrics HTTP server is minimal (one request per connection, GET only, 5 s timeout). It is not an HTTP framework and should not be exposed to the internet.
- Unreviewed code, like everything else here.
