# Relay security audit (read-only)

Scope: relay/src/{lib,store,limits,metrics,main,tls}.rs, core/src/{wire,mailbox}.rs, plus verify_auth/is_valid_device_id in core/src/identity.rs. ADRs 0022/0023 read first. No builds run; every finding below was confirmed by reading the code.

Key context: ADR 0023 (tenants, enrollment tokens, admin/operator keys) is "not built". `grep tenant|enroll` in relay/src finds nothing. The relay is therefore always in "open" mode: any fresh Ed25519 key is a valid device. Almost every resource finding below follows from that: per-device caps are not per-anyone caps.

Paths are relative to the repo root. Line numbers are from the working tree as audited.

## Findings

### R-01 (HIGH) Unbounded outbound channel + no write timeout: remote memory exhaustion
- relay/src/lib.rs:371 (`mpsc::unbounded_channel`), :133/:137 (State holds unbounded senders), :418 (`send_json(...).await?` with no timeout), :978 (group push), :662 (live push).
- Attack: attacker (any fresh key) creates its own group mailbox (legit, quota 200), opens ~60 connections from one IP, subscribes each to the mailbox, and then never reads from the socket (zero TCP window). A 61st connection appends 256 KB entries at the bucket rate (50/s). Each append clones a ~512 KB hex `WireEntry` per subscriber into that connection's unbounded channel (about 1.5 GB/s of heap growth), and the stalled `send_json` await also means the idle timeout branch is never polled, so the connection is never reaped. Relay is OOM-killed in seconds. The same works with `Send` to self (queue_cap bounds the DB, not the channel, if the attacker acks from a second connection).
- Fix: bounded channel (`mpsc::channel(N)` ~64) with `try_send`; on Full, drop that subscriber/close the connection. Wrap every `tx.send` in `tokio::time::timeout` (e.g. 10-30 s) so a non-reading peer is disconnected.

### R-02 (HIGH) Unbounded, never-expiring storage: free identities bypass every per-device cap
Every cap is keyed by a device id the attacker mints for free (open mode, no enrollment, ADR 0023 unbuilt). No global byte/row cap exists and several tables have no TTL at all.
- a) `device_lists` (store.rs:503 `put_device_list`, lib.rs:719 `on_publish_list`): attacker generates owner key + device, signs a list (<= 64 KB), publishes; repeat with new owner keys. Never purged (`purge_expired` store.rs:331 ignores it). 50 req/s x 64 KB = 3.2 MB/s per connection, forever.
- b) `prekeys` (store.rs:406), `fallbacks`, `key_packages` (:601, 200 x 16 KB = 3.2 MB per identity), `key_package_last_resort`: no TTL, no purge, per-device cap only.
- c) `messages` (store.rs:342): `Send` to any syntactically valid 32-hex id (no check the recipient exists). queue_cap 1000 x 64 KB = 64 MB per recipient id, 7-day TTL; attacker iterates over random ids. 1000 sends/recipient so ~20 s of one connection fills 64 MB.
- d) `blob_chunks` (store.rs:514): 1 GB quota per uploader, 30 days; new identity = new 1 GB.
- e) `group_mailboxes/entries`: 64 MB per mailbox x 200 per device (12.8 GB per identity); commits are never evicted.
- Fix (minimal): (1) global caps (DB size / row counts per table) that make writes return `quota_exceeded` when exceeded; (2) add `expires` to prekeys/key_packages/fallbacks/device_lists (e.g. 90 days refreshed on publish) and purge them; (3) require recipient to be "known" (has signed in within N days) before queueing more than a small number of messages; (4) real fix is ADR 0023 enrollment, since this is what it is for. Until then document that open mode must not face the internet.

### R-03 (MEDIUM-HIGH) Targeted queue-fill DoS on any victim
- relay/src/lib.rs:641 `on_send`, store.rs:342 `enqueue`.
- Attack: device ids are public (device lists, cards). Any authenticated key sends 1000 small messages to the victim id (~20 s at 50 req/s, possibly less with several connections). The victim's queue is full for up to the 7-day TTL (garbage can only be removed by the victim acking it), so every legitimate sender gets `queue_full`. The relay cannot tell senders apart; ADR 0022 relies on the cap as the spam defence, but the cap is itself the weapon.
- Fix: do not let one source consume the whole queue: per-source-IP (or per-authenticated-key) share of a recipient's queue, a reserved slice for senders that hold a recipient-issued capability/token (delivery tokens), and shorter TTL for unknown senders.

### R-04 (MEDIUM) Reconnect backlog: whole queue loaded + hex-encoded in memory under the global lock
- lib.rs:494-512 `register` holds `state.state` (the global Mutex) while `store.backlog()` loads all rows (up to 1000 x 64 KB = 64 MB), then `hex::encode`s each into `ServerMsg::Deliver` (another 128 MB), up to 4 live connections per device (MAX_LIVE_PER_DEVICE).
- Attack: attacker fills its own queue (R-02c/R-03 style, sends to itself), then reconnects 64 connections per IP (conn rate 300/min). Roughly 190 MB per connection, ~12 GB of RAM per IP; all other sends/registrations/disconnects block on the global state mutex during every backlog read.
- Fix: page the backlog (LIMIT 50 per read, stream as acks arrive), and read it without holding the global mutex (e.g. register first, take a sequence cursor, read backlog, then dedupe by id).

### R-05 (MEDIUM) Accept error terminates the whole relay
- lib.rs:190 `let (stream, peer) = listener.accept().await?;` inside `loop`; main.rs:134 returns that error and the process exits.
- Attack: any transient accept error (EMFILE when fds are exhausted by many idle connections from many IPs, ECONNABORTED when a client RSTs while in the accept queue on Linux, ENOBUFS) ends `serve_with_metrics`, killing the relay (until a supervisor restarts it). Cheap for an attacker with a few IPs to trigger via fd exhaustion; sometimes triggerable by a single client RST race.
- Fix: log-free handle: on Err, `continue` (sleep ~100 ms for EMFILE) instead of `?`.

### R-06 (MEDIUM) No timeout on the WebSocket handshake (slowloris); trusted-proxy path has no admission limit at all
- lib.rs:315 `accept_hdr_async_with_config(...).await` has no timeout (the `auth_timeout` only starts after the handshake; the TLS accept at :213 does have one). Plain-ws and behind-proxy deployments (the documented default: relay behind a TLS proxy, `--insecure-no-tls`) hit this.
- Attack: open connections and send nothing (or one byte per minute). Each holds an fd and a task. Per-IP cap 64 applies for direct peers, but for connections from a trusted proxy the admission is skipped until after the handshake (lib.rs:197, :318), so the connections of all clients of the proxy are unlimited and unreaped before the handshake finishes. fd exhaustion then also triggers R-05.
- Fix: wrap the handshake in `tokio::time::timeout(cfg.auth_timeout, ...)`; for proxied connections apply a global cap on pending handshakes.

### R-07 (MEDIUM) CPU amplification: signatures verified before the count cap
- lib.rs:682 `on_publish`: loops over *all* `keys` calling `SignedPrekey::from_wire` + `verify_prekey` (Ed25519), and only `publish_prekeys` applies `MAX_PREKEYS` (50) afterwards. A 528 KB frame (frame_limit, lib.rs:760) holds ~2,400 keys, i.e. >100 ms of CPU per request (the bad-signature path breaks early, but a batch of valid ones signed by the attacker's own key does not). 50 req/s per connection x 64 connections x blocking pool (default 512 threads) = full CPU starvation. Likewise `on_publish_key_packages` (lib.rs:801) decodes all packages (up to ~1 MB hex) before the 200 cap.
- Fix: `if keys.len() > MAX_PREKEYS { return err("too_large") }` (and packages.len() > MAX_KEY_PACKAGES) before any decode/verify.

### R-08 (MEDIUM) Auth signature is not bound to the relay: a malicious relay or MITM can impersonate the device on another relay
- core/src/identity.rs:153 `verify_auth` signs `"spike-relay-auth-v1" || nonce` only; lib.rs:355. No relay identity/URL/TLS-exporter in the signed bytes.
- Attack: federation means a client connects to relays it does not control (a contact's home relay). A malicious relay A, when the client connects to it, opens a connection to relay B as that client, forwards B's nonce as its own challenge, and relays back the client's signature to B. A is now signed in as the victim device on B: it can read the victim's queue ciphertext, `Ack` (silently delete) messages, drain prekeys/key packages, publish prekeys signed by that same device key (valid), and subscribe/read groups the device is in only if it holds the mailbox key (it does not). Same on plaintext links (insecure_no_tls behind a proxy is TLS-terminated by operator, so mostly the first case).
- Fix: sign `ctx || nonce || relay_id` where relay_id is the relay's configured name or its TLS certificate hash/exporter value; send `relay_id` in the Challenge.

### R-09 (MEDIUM) One-time prekey / key package drain: claim limit is per connection only
- lib.rs:576-632 (`claims` is a local counter in the connection, max 20); the DB has no per-target limit.
- Attack: attacker reconnects (300 connections a minute per IP, 20 claims each, i.e. 6000 claims a minute) and claims every one-time prekey and key package of a victim, forcing all future first contacts onto the reusable fallback prekey / last-resort key package (no one-time forward-secrecy property, reusable material), and it can repeat each time the victim replenishes. It also lets an attacker probe whether a device id exists (`prekey: None` vs `Some`).
- Fix: per-target-device rate limit on the relay (e.g. N claims per target per hour across all connections), not just per connection.

### R-10 (LOW-MEDIUM) Ping and WebSocket control/binary frames are free: unmetered blocking-pool work
- lib.rs:263 `cost(Ping) = 0.0`; each Ping still goes through `spawn_blocking` (lib.rs:449). Binary frames are `continue`d without charge (lib.rs:426-429).
- Attack: a client sending Pings at line rate (about 100k/s on loopback-class links) burns blocking-thread scheduling and CPU and never hits `rate_limited` or the 200-refusal hang-up; 64 connections per IP. Also resets the idle timer so the connection lives.
- Fix: charge Ping a small cost (0.05-0.1) and answer Ping inline without `spawn_blocking`; count non-text frames against the bucket.

### R-11 (LOW-MEDIUM) Blocking std::sync::Mutex taken on async worker threads; mutex poisoning turns one panic into a relay-wide outage
- lib.rs:467, :1282 (`state.state.lock().unwrap()` in async fns, and in the connection-teardown path) wait on a lock that blocking threads hold across fsync'd SQLite writes (`on_send` :651 holds `state` through `enqueue`; `synchronous=FULL`, store.rs:290). Under write load tokio worker threads sit blocked, stalling all I/O (contradicts ADR 0022's "off the async threads").
- Every `.lock().unwrap()` (about 30 in lib.rs/store.rs/limits.rs) panics if any previous holder panicked. I found no remote-reachable panic in the relay code (all slicing/casts/parses on attacker data are checked), but any future one would permanently brick the relay.
- Fix: use `parking_lot::Mutex` (no poisoning) or `lock().unwrap_or_else(|e| e.into_inner())`; do the teardown work in `spawn_blocking` or use `tokio::sync::Mutex` for `State`; do the DB write outside the `state` lock where ordering allows.

### R-12 (LOW) Purge and group-create do global work under the single DB lock
- store.rs:331 `purge_expired` runs unbounded `DELETE`s, then `purge_groups` (:574) recounts bytes per touched mailbox with `SUM(LENGTH(blob))`; `create_group_mailbox` (:703) and `append_group` (:767, :786) call/duplicate this on every request, inside the one SQLite connection mutex. An attacker with many entries expiring at once (or just spamming `GroupCreate`, which does `purge_groups` first even to return `Exists`) makes every other request wait. Also `/metrics` `stats()` (store.rs:309) does 4 full-table scans/SUMs per scrape.
- Fix: batch deletes with LIMIT; do `purge_groups` only on the timer, not per create; cache stats on a timer.

### R-13 (LOW) Metrics HTTP listener has no connection cap
- lib.rs:1220-1233 spawns a task per accepted connection with a 5 s timeout; no cap or per-IP limit. `/healthz` is unauthenticated by design. Default config is safe (metrics off; non-loopback requires token, main.rs:103), but when exposed on a private network a flood can exhaust fds (then R-05). Fix: semaphore of ~32 concurrent metric connections.

### R-14 (LOW) Group mailbox key is a shared bearer secret: removed member can DoS, existence oracle
- store.rs:741-: any holder of the mailbox private key can append commits/seal; the relay cannot tell members from ex-members. An ex-member who still holds the old key can race a commit/seal (first commit at `expected_seq` wins) or flood the old mailbox until rotation. By design in ADR 0025 (rotated on removal), so low; the sealed successor depends on the removing commit winning the race. Existence is also visible: `unknown_mailbox` vs `bad_signature` (lib.rs:889-893), but ids are 128-bit random so this is not exploitable. Fix (optional): none required; note in the threat model.

### R-15 (LOW / INFO) Backup files and operator hygiene
- store.rs:241-248: `VACUUM INTO` creates the `.before-v<N>` file with the default umask and only afterwards chmod 0600 (brief world-readable window on unix; none on Windows, which has no permission handling at all, store.rs:277 is `cfg(unix)` only). The backups of the metadata store are never deleted or rotated. WAL/-shm files inherit mode from the main db (fine).
- Fix: create the parent dir 0700 / set umask before VACUUM, or write to a temp name in a 0700 dir and rename.

### R-16 (INFO) Metadata exposure
- `FetchDeviceList`, `ClaimPrekey`, `ClaimKeyPackage`, `GetBlobChunk` are open to any authenticated key: anyone can learn which device ids/owners exist, device-list contents (relay URLs in `DeviceEntry.relay`), prekey counts (`Ready.prekeys`, claim results), blob existence. Needed by the protocol, but together with R-02 (free identities) it lets a crawler enumerate. `/metrics` aggregates are content-free (checked: fixed label set, `ERROR_CODES`, no ids).

## Checked and found fine
- SQL injection: every query uses `params!`/`?N` binding; no string-built SQL (only the static `format!` for the `.before-vN` file name, passed as a bound parameter to VACUUM INTO).
- Authorization of the core queues: `ack` deletes `WHERE id AND recipient = me` (store.rs:387); a device cannot ack or read another's queue. Backlog is keyed to the authenticated id (id = SHA-256(ed25519 pub)[..16], lib.rs:359). Prekey/fallback publishes require a signature by the authenticated device key (lib.rs:682-713). Blob chunk upload ownership (`NotOwner`), `total` consistency and per-uploader quota are checked inside one SQLite transaction on the one connection (no TOCTOU).
- Group mailboxes: create proves possession of the new key; every append/read/subscribe signature is domain-separated, length-prefixed and binds device id + mailbox + all fields (core/src/mailbox.rs); commits are compare-and-append in one transaction; seal+successor is one transaction; subscription is authenticated before registering the weak sender. Lock order (state -> store, subs -> state) is consistent; no deadlock found.
- Panic review: all hex/serde/parse of attacker data returns errors; casts are bounded (`index<total`, `limit.clamp`, `after_seq as i64` only widens the range); `blob_hex.len()` caps are applied before `hex::decode` (on_send, group append, chunks, key packages, device list) so no huge-string decode. Frame size is capped by `max_message_size`/`max_frame_size` pre-auth (~528 KB). No `unwrap` on attacker data in relay or the called core fns I read (identity/owner/mailbox/wire).
- Metrics token compare: `same_secret` is constant-time over max length (only length leaks); header parse is bounded to 8 KB. Metrics default off; loopback default `127.0.0.1:8787`; plaintext on non-loopback is refused unless `--insecure-no-tls` (main.rs:79) and `/metrics` on non-loopback requires a token (main.rs:103). Note: `--metrics-token` on the command line is visible in the process list (the `env = SPIKE_RELAY_METRICS_TOKEN` form avoids that).
- TLS: rustls with ring, `with_safe_default_protocol_versions` (TLS 1.2 + 1.3), no client auth (appropriate), cert chain must be non-empty, handshake has `auth_timeout`. Cert is loaded once (renewal needs restart; operational only).
- Logging: only startup `eprintln!` lines (listen addr, db path); no connection, id, address or content logging anywhere in the relay crate.
- X-Forwarded-For: rightmost-untrusted entry logic is correct and only used when the TCP peer is a trusted proxy; IPv4-mapped IPv6 canonicalised. (Note: IPv6 is limited per full /128, so one /64 gives an attacker 2^64 addresses; add a /64 aggregate for v6 in `IpLimits::admit`, limits.rs:131.)
- Tenants / enrollment tokens / operator key (ADR 0023): not implemented, so there is no cross-tenant isolation to audit; the open-mode findings above (R-02/R-03) are the consequence. No admin or operator surface exists on the WebSocket protocol.
