# Security audit: core, cli, app bridge, mls (read-only)

Repo: D:\Claude\Projects\Spike-Network-sec (HEAD 1f4e0db plus uncommitted edits that were landing while I read; the relay-auth audience binding in core/src/identity.rs and cli/src/client.rs is one of them). Line numbers are from the files as read and may drift by a few lines.
Method: read the listed ADRs/threat model/brief first, then read the code and confirmed each finding by tracing the path. Nothing was built or run. Speculative items were dropped; the "checked and fine" list is at the end.

Release profile has `overflow-checks = true` (Cargo.toml:13), so any u64 `+` on a peer-supplied value is a remote panic, not a wrap. That drives findings H1 and H2 and M6/M7. `mobile` is in the workspace (same profile); `desktop` is a separate workspace and may wrap instead of panic.

Summary table

| id | sev | one line |
|---|---|---|
| H1 | high | A peer-supplied Lamport clock of u64::MAX permanently breaks sending (and recv replies) |
| H2 | high | A contact can auto-join you to a "group" whose version is u64::MAX, then one more state panics `recv` before ack: permanent receive poison pill |
| M1 | medium | No inter-process lock on the state dir: concurrent CLI commands roll back ratchet state, causing Olm message-key reuse |
| M2 | medium | Removing a device does not remove its power: it still holds the owner key and can re-add itself (threat model says "Mitigated") |
| M3 | medium | Contact/group-controlled relay URLs make the client connect (and authenticate) to arbitrary hosts, including loopback |
| M4 | medium | A device card carries no owner consent: one device can sit in several people's lists; first contact wins attribution |
| M5 | medium | Large-group sync loop trusts relay `seq`: hostile relay can loop/OOM the client or panic it via `cursor + 1` |
| M6 | medium | Large-group mailbox generation from the join bundle is attacker-chosen and later `+ 1` |
| M7 | medium | Unknown-sender blobs trigger N relay fetches and 3x full contact decoding per blob (unauthenticated amplification) |
| L1 | low | Tombstone/ID squatting bypass: a 1:1 Delete of a group message id suppresses the real message |
| L2 | low | Agent inbox and audit log grow without bound for any contact; each request also buys a reply |
| L3 | low | Large-group `LargeLists` lets any member bloat every member's directory file |
| L4 | low | Link hello is unauthenticated and first-wins: anyone knowing the new device id can force a code mismatch |
| L5 | low | `write_private` creates the temp file before chmod, uses a non-unique `.tmp` name, no directory fsync |
| L6 | low | Passphrase rules live only in `app::check_passphrase` and the CLI; `Home::create`/`Vault::create` accept anything; secrets sit in non-zeroized buffers |
| L7 | low | Attachment/group/policy names are not checked for control or bidi characters; Windows names not handled by `safe_file_name` |
| L8 | low | Legacy-contact "Upgrade" accepts any owner key whose list contains the old device |
| L9 | low | Replayed fallback-key pre-key messages can create new sessions and evict real ones |
| I1.. | info | see end |

---

## H1. Lamport clock poisoning (one message from any contact or co-member)

- Where: cli/src/lib.rs:292 `home.observe_lamport(env.lamport)?` (runs for every authenticated envelope, before any kind or sender check, including `From::Member`, i.e. people you only share a small group with); cli/src/state.rs:382-394 (`next_lamport` does `self.lamport() + 1`, `observe_lamport` stores any larger value); core/src/envelope.rs:61 (`lamport: u64`, never bounded in `from_bytes`). Also cli/src/requests.rs:393 (held stranger messages, applied on accept) and cli/src/lib.rs:507 (sibling catch-up).
- Attack: a contact (or a member of a small group you are in) sends a Text envelope with `"lamport": 18446744073709551615`. `lamport.enc` now holds u64::MAX. Every later `deliver`, `edit`, `delete`, group send, receipt, `flush_outbox` reply and sync push calls `next_lamport`, which panics (overflow-checks). In the app bridge `catch_unwind` turns it into "internal error" forever; in the CLI the process aborts, and `recv` aborts mid-loop. The value is persisted, so a restart does not help; only deleting `lamport.enc` (not possible from the UI) does.
- Fix: clamp on receipt (`seen.min(1 << 53)` or reject envelopes with lamport above a sane bound in `Envelope::from_bytes`), and use `saturating_add`/`checked_add` in `next_lamport`. Add a test with `lamport = u64::MAX`.

## H2. Group-state version overflow: receive-path poison pill from a contact

- Where: core/src/group.rs:317 (`self.version != prev.version + 1`), :546 (`v + 1` in `highest_contiguous`), :147 (`self.version + 1` in `successor`); `Chain::from_checkpoint` :403-417 accepts any version; cli/src/groups.rs `on_states` (status becomes `Joined` immediately when the sender is a contact, no consent).
- Attack: contact Mallory sends a `GroupState` for a new group id with `version = u64::MAX`, a dummy `prev`, signed by her own owner key, listing the victim as member. `from_checkpoint` only checks signature, shape and that the author is in it, so the victim is silently "Joined". Mallory then sends a second state with `prev = hash(first)`. `Chain::add` finds `prev`, calls `verify_transition`, and `prev.version + 1` panics. The panic happens inside `process()` before `relay.ack`, so the relay redelivers the same blob on every `recv` and every sync dies at that message. All messages queued behind it never get processed. In the app it shows as permanent "internal error" on sync.
- Related root cause: `ops::recv` uses `process(...)?` (cli/src/ops.rs ~521). Any `Err` from `process` aborts the whole loop without acking the delivery, so any future error reachable from remote input becomes a permanent poison pill too.
- Fix: (1) `checked_add` everywhere versions are incremented, and reject checkpoint versions above a bound (say 1_000_000) in `from_checkpoint`; (2) in `recv`, catch `Err` (and wrap `process` in `catch_unwind`) per delivery, log it, ack or quarantine the blob and continue; (3) add a fuzz target for `Chain::add`/`from_checkpoint` (see I6).

## M1. No locking of the state directory: lost updates roll back ratchet state

- Where: cli/src/state.rs `write`/`write_private` (whole-file replace), cli/src/ops.rs `deliver` (loads `sessions`/`me` at start, saves after each encrypt), `recv` (loads at start, saves per message), cli/src/main.rs `--follow` mode (line ~81, ~1594). No lock file anywhere (grep for flock/fs2/lock found only Mutexes).
- Attack (accidental or induced): `spike recv --follow` in one terminal and `spike send` in another (or the desktop app plus the CLI on one home). Both hold an in-memory copy of `sessions.enc`/`identity.enc`; the later writer overwrites the other's ratchet advance. The sender that lost an update re-derives the same chain key and encrypts a different plaintext with the same Olm message key (AES-CBC key/IV derived from it), which leaks plaintext relationships; the receiver that lost an update fails to decrypt later messages. A message-triggered variant: `recv` reacts to a request by calling `deliver` (flush_outbox, flush_receipts) while its own `sessions` copy is stale (the code reloads after, but the window exists; the in-app `net` mutex only covers one process).
- Fix: take an exclusive advisory lock (`fd-lock`/`fs2`) on `<home>/.lock` in `Home::unlock` and hold it for the life of the `Home`, failing fast if taken. Alternatively read-modify-write under the lock per operation.

## M2. Device removal does not stop the removed device (owner key travels with every device)

- Where: ADR 0010 option (a); cli/src/link.rs `finish` copies `owner_key` into the bundle; app/src/lib.rs:942 `remove_device`; core/src/owner.rs `DeviceList::with_device`; threat-model row "Removed or stolen device keeps receiving: Mitigated for new messages".
- Attack: an attacker holding a removed or stolen device (unlocked, or with the passphrase) has the owner key. They sign a list at version N+2 that includes themselves and publish it. The relay accepts it (signed, version higher, publisher is in the submitted list); contacts accept it as "Newer" and verification is kept (the fingerprint covers the owner key only). They can also publish a same-version competing list first, which makes honest contacts refuse the legitimate list as equivocation until the owner jumps versions.
- Impact: "removal" only protects against a device that cooperates. The doc says Mitigated; it is really "partial, requires the removed device to be honest".
- Fix: at minimum reword the threat model row. Real fix: rotate the owner key on removal (new owner key, cross-signed by the old one, contacts re-verify), or countersign list entries per device so the owner key alone cannot add a device (see M4), or keep the owner key off phones.

## M3. Peer-controlled relay URLs make the client dial arbitrary hosts

- Where: core/src/owner.rs `DeviceEntry.relay` (signed by the contact, never validated: `check_shape` ignores it); cli/src/ops.rs:441 (`url = relay.unwrap_or(base.url)` in `deliver`) and ~680 (`deliver_to_own`); cli/src/ops.rs:268 `download_file` uses `att.relay` from the descriptor (core/src/attachment.rs `validate` ignores `relay`); cli/src/large.rs `WelcomeBody.relay`; core/src/group.rs `Member.relay` (<=512 bytes, unvalidated scheme); cli/src/link.rs `begin` uses `offer.relay` from the QR.
- Attack: Mallory is a contact. She publishes a device list whose entry says `relay: "wss://collector.mallory.example"` (or `ws://127.0.0.1:PORT`). The next time the victim sends her a message, `Pool::get` opens a TLS or WebSocket connection to that host from the victim's device with no click: IP and timing disclosed, device-key authentication performed (audience-bound after the in-progress fix, so not replayable elsewhere), a prekey claim for the victim's chosen peer sent. With `ws://127.0.0.1:N` she can probe local ports of the phone/desktop (connect success/timeout distinguishes open ports). Same for an attachment descriptor (on fetch) and a Welcome (on accept). If the user turned on `allow_insecure`, plaintext `ws://` to any host is also reachable this way.
- Fix: a `valid_relay_url` check at the parse boundary (DeviceList shape, Attachment::validate, WelcomeBody, group Member): scheme `wss` only (plus `ws` only to loopback when the *user's own* relay is loopback), reject IP literals in private/loopback/link-local ranges and loopback hostnames for foreign relays, cap length, no userinfo. Consider a per-session allow-list so a contact's relay needs one user confirmation.

## M4. A device is not bound to its owner by the device: shared/stolen devices in other people's lists

- Where: core/src/owner.rs `DeviceEntry`/`check_shape` (any valid self-signed card can be listed by any owner key); cli/src/requests.rs `accept` and cli/src/state.rs `add_contact` (only compare name and owner key); cli/src/lib.rs:249-276 and core/src/session.rs:198 (`contacts.iter().position(|c| c.x25519 == sender)`: first contact in file order wins).
- Attack: Mallory generates a fresh owner key M and signs a list `[Mallory's device, Alice's device card]` (Alice's card is public). She messages the victim as a stranger; the request check only requires that the *sending* device is in the card, so it passes. The victim accepts (contact "Mallory"). Later the victim adds Alice. Alice's messages now match Mallory's contact first and are filed and shown as from "Mallory"; the victim's reply in that thread is encrypted to every device in Mallory's list, so it reaches Mallory's device (and Alice's). Mallory reads the reply that was meant for Alice. A related effect: blocking Mallory (`add_block`) puts Alice's device key on the block list.
- Fix: refuse to add or update a contact whose list contains a device id already attributed to another owner (in `add_contact`, `accept`, `refresh_contacts` and when processing group member cards); and, properly, have each device countersign "I belong to owner O" and carry that signature in the list entry (core change in `DeviceEntry`).

## M5. Large-group mailbox sync trusts relay positions: hang, OOM, panic

- Where: cli/src/large.rs `sync_group` (~1467-1609): `rec.cursor = e.seq` with no monotonic check; loops until a page is empty; `events` is an unbounded Vec; cli/src/large.rs:612 `Some(rec.cursor + 1)`.
- Attack (hostile relay, not in the "relay only delays or drops" model): answer `GroupRead` with the same non-empty page regardless of `after_seq`. The loop never exits, each pass records a refusal/event (`events` grows, `refused_total` increments, the record is rewritten), so the client spins and eventually runs out of memory. Or return one entry with `seq = u64::MAX`: the cursor is saved, and the next commit by this member panics at `cursor + 1`; the group is then unusable for that member. A relay can also move the cursor forward past commits so the member silently forks.
- Fix: require `e.seq > rec.cursor` and strictly increasing within a page (else treat the relay as faulty and return an error), bound `events`, use `checked_add`, and bound pages per sync call. Compare `next_seq` from the page with the sequence of the entries.

## M6. Mailbox generation chosen by the inviter

- Where: mls/src/group.rs:799 (`generation: u64::from_be_bytes(pt[..8])` taken from the join bundle), :374 and :566 (`self.mailbox.generation + 1`), and the outer AEAD AAD (`outer.rs aad`) which includes it.
- Attack: any member who adds someone controls the bundle plaintext (generation and secret). `generation = u64::MAX` makes the new member panic the first time any removal commit is processed (`after_merge`) or created (`outgoing`), so every later sync of that group aborts for that member. A different generation or secret also isolates the invitee in a mailbox the others do not read (they cannot open each other's entries) while the UI says "joined".
- Fix: `checked_add`; bound generation (the joiner can learn the expected generation from the Welcome's group context if it is added to GroupContext or checked against the inviter's announcement); reject absurd values.

## M7. Unauthenticated amplification on unknown-sender blobs

- Where: cli/src/ops.rs `recv` (~521-545): for each `UnknownSender` outcome it runs `refresh_contacts` (one relay fetch per contact), `sync_own_list`, `refresh_member_cards`, and calls `process` up to three times; each `process` re-decodes every contact card (core/src/owner.rs `IdentityCard::decode` verifies the list signature plus every device card signature, up to 17 Ed25519 verifications per contact) and rebuilds the cards table.
- Attack: anyone who knows your device id (it is in your card) queues junk blobs that look like pre-key messages from random keys (the relay cannot tell). Each blob costs the victim N network round trips and roughly 3 x 17 x N signature checks. With 200 contacts that is hundreds of fetches per junk blob; battery and relay-quota drain, and recv latency for real messages.
- Fix: cache the parsed cards in memory per recv session; only refresh when the unknown key has not been seen in the last N minutes (rate-limit per recv, e.g. once per 5 minutes); skip refresh for blobs that are not valid pre-key messages (parse `OlmMessage::from_parts` first); apply the existing 50-request cap before any refresh.

## L1. ID squatting and tombstone bypass across conversations

- Where: cli/src/messages.rs `insert` (`INSERT OR IGNORE`, UNIQUE on id tag alone, ignoring conversation); cli/src/lib.rs `apply_change` (Tombstone inserted under the *sender's* conversation); cli/src/groups.rs `store_message` (only overrides a tombstone when `t.peer == gid`).
- Attack: small-group member Mallory sees Bob's group message id X, and as a contact of Carol sends a 1:1 `Delete{target: X}` that reaches Carol before Bob's copy does (Bob's delivery to Carol is retried later, or a hostile relay reorders). Carol stores a tombstone for X under Mallory's conversation. Bob's real message then goes through `store_message` -> `insert` -> ignored -> `Duplicate`, so it is silently dropped at Carol. The same race works with a 1:1 Text message that reuses X.
- Fix: derive the id tag from (conversation, id) rather than id alone, or in `store_message`/`insert` treat any tombstone with `author != sender` or a different peer as non-blocking and overwrite.

## L2. Agent inbox and audit log: unbounded growth and reply amplification

- Where: cli/src/agent.rs `on_request` (~247-345): every request, including those from contacts with no grant or over the hourly limit, is appended to `agent-inbox.enc` and `agent-audit.enc` (whole file rewritten each time), and a Deny receipt is queued and sent back.
- Attack: any contact floods signed requests (valid signature required, but they sign with their own key). Disk grows without bound, each request costs O(size) to rewrite and verify, and each costs the victim a reply message.
- Fix: for contacts with no grant, drop silently (or deny without storing) after a small cap; prune decided entries older than N days; cap total inbox rows; do not reply to over-limit requests.

## L3. `LargeLists` lets any member bloat every member's state

- Where: cli/src/large.rs `sync_group` (`dir.apply(list)` for up to 64 lists per message, any owner), `GroupDir::apply`/`save` (whole overlay file rewritten when dirty).
- Attack: a member generates owner keys at will and posts self-signed lists (valid signatures, any owner). Every member stores them forever in `large-lists-<id>.enc` and rewrites the file each time. Bounded only by mailbox caps.
- Fix: accept lists only for owners that are members, or are referenced by a credential in a commit the group has seen, and cap total entries.

## L4. Link hello: first frame wins, no authentication

- Where: core/src/link.rs:152 `on_hello`, cli/src/link.rs:81-93 `await_hello` (returns on the first well-formed 37-byte hello).
- Attack: anyone who can queue to the new device's address (device id is in the QR; the relay sees it) sends a hello before the real device. The new device binds to that session; the real device's code differs; the person correctly aborts. This is a denial of service on linking, not a bypass: without the QR secret the attacker cannot compute a matching 8-digit code (single online guess, 1 in 10^8).
- Fix: ignore hellos that do not carry a MAC under the QR secret (HMAC(secret, hello)), so only someone who saw the QR can interfere; keep waiting until the timeout.

## L5. State-file writes

- Where: cli/src/state.rs:444-457 `write_private`, :433 `make_private_dir`; app/src/lib.rs `write_ca` uses plain `std::fs::write`.
- Issues: `File::create` runs before `set_permissions(0o600)` (file briefly follows the umask; content is sealed except `vault.json`, so impact is small); temp name is `path.with_extension("tmp")` so concurrent writers collide on `identity.tmp`; no fsync of the directory after rename; on Windows no ACL is set (relies on profile defaults); `fetch` in cli/src/main.rs:1525-1535 writes plaintext files with default permissions and follows symlinks (`exists()` then `write`).
- Fix: `OpenOptions::mode(0o600).create_new(true)` with a random temp name, fsync file then directory; `create_new` for downloads.

## L6. Passphrase and secret handling

- Min length (10 chars) is enforced in `app::check_passphrase` (app/src/lib.rs:345, used by `Session::create`, so the bridge is covered) and in cli/src/main.rs:667, but not in `Home::create` or `Vault::create`. Any other caller (desktop `api.rs`, future restore/backup code) must remember its own check. Move the check into `Home::create`.
- No unlock throttling: offline guessing is bounded only by Argon2id (64 MiB, 3 passes, 1 lane). That is the OWASP floor; consider 19 MiB x 2 minimum is lower, so this is acceptable, but a phone can afford more (e.g. 128 MiB, t=3) and a rate limit on repeated `unlock` calls in the bridge would limit shoulder/biometric-bypass guessing.
- Secrets in non-zeroized memory: bridge `unlock`/`create_identity` pass the passphrase through `args_json` (&str) and `serde_json::Value` before it reaches the `Zeroizing` wrapper (bridge.rs:90-94, 115-124); `OwnerKey::to_hex()` returns a plain `String` (state.rs:118); `Identity::to_json` (account pickle) is a plain `String` and is produced after every message; `Home::read` returns a plain `Vec<u8>` of identity/session JSON; `mls_state::export` returns a plain `Vec<u8>` holding the mailbox secrets; the link bundle JSON holds the owner key. `Vault` itself is `ZeroizeOnDrop`. Wrap these in `Zeroizing` where cheap; accept the rest as residual (Android heap dumps need root).
- Plain-text exposure checks: no secret is logged or printed (no `println!/eprintln!` of keys found in core, cli/src/ops, app); the bridge DTOs contain only public data (fingerprint, public card, names).

## L7. Display and file-name hygiene

- `Attachment::validate` (core/src/attachment.rs:65) allows any bytes in `name` up to 255, including newlines, ANSI escapes and bidi overrides. The name is shown raw in requests.rs `Held.file` text, in the "[A file named ...]" lines (requests.rs `accept`, groups.rs history), and is only sanitised at save time. In the CLI that is terminal-escape injection; in UIs it is spoofing.
- Group names (`parse_welcome` in large.rs, policy `name`, core/src/group.rs `name`) reject only control characters in the policy case; bidi/zero-width are allowed (core/src/action.rs `invisible()` shows the project already knows the list).
- `Request.action` (core/src/agent.rs `validate`) rejects `char::is_control` only; U+202E and other format characters pass in free-text requests (the test at agent.rs:~590 passes because of the `\n`, not the bidi char).
- `safe_file_name` (attachment.rs:158) does not handle Windows reserved device names (`CON`, `NUL`, `COM1`, `aux.txt`), trailing dots or spaces; the desktop build runs on Windows.
- Fix: one shared `displayable()` check (the `action.rs` one) applied in `Attachment::validate`, group names, policy names, `Request.action`; reserved-name and trailing-dot handling in `safe_file_name`.

## L8. Legacy "Upgrade" path

- Where: core/src/owner.rs:291-297, cli/src/state.rs:265. A signed card for a legacy contact is accepted if it merely lists the old device card; the owner key can be anyone's (the legacy device card is public). The contact is marked unverified, but an unverified legacy contact is the common case.
- Reachability: `refresh_contacts` skips legacy contacts, so a relay cannot trigger it; only a manually imported card (`spike add`, `add_contact`) can. Low.
- Fix: require explicit user confirmation showing the new fingerprint, or drop legacy cards.

## L9. Fallback-key pre-key replay evicts sessions

- Where: core/src/session.rs:196-221 and `insert` (`truncate(MAX_SESSIONS_PER_PEER = 4)`). A pre-key message built on the reusable fallback key can be replayed after its session has been evicted; vodozemac does not delete fallback keys on use, so `create_inbound_session` succeeds again, the (already stored) envelope is deduplicated by id, but a new session sits at the front of the list and pushes real sessions out.
- Attack: relay (or anyone who recorded old traffic) replays up to four old fallback pre-key messages from a contact; later normal messages in the live session can no longer be decrypted ("no session could decrypt").
- Fix: remember session ids that were evicted (or processed pre-key message ids) for the fallback-key lifetime and refuse them; or do not evict the most recently used session.

---

## Info / design notes

- I1. Relay-auth channel binding (sign_auth over audience) is being added in the working tree; reviewed the diff: length-prefixed audience, normalised host:port, new context string `spike-relay-auth-v2`. Good. Make sure the relay side compares against the `Host` header it actually received and that old v1 signatures are refused.
- I2. Olm session config v1 truncates the MAC to 8 bytes. Forgery needs an online oracle at 2^-64 per attempt; acceptable, but keep it in the review notes. Combined with M1 (state rollback) the AES-CBC key reuse is the real exposure, not the MAC.
- I3. Group history import (cli/src/groups.rs `import_history`) lets the person who added you fabricate "last 7 days" messages attributed to any member's owner key; they are tagged `shared_by`, make sure every UI shows that tag next to the author name.
- I4. Mailbox secret and outer key stay constant for a generation across add/self-update commits (mls/src/group.rs `after_merge` rotates only on removal), so a compromised member device exposes the outer layer and mailbox signing key until the next removal. The MLS layer still protects content; this is a metadata/availability exposure and is mostly in the brief already.
- I5. Linking: `LinkOffer.relay` from a QR is trusted (connects there, see M3); a QR produced by an attacker's device gives a valid matching code by design (the person confirms *that the phone in their hand* shows the same code). The UI should name the new device id/fingerprint on the existing device.
- I6. Fuzz coverage (mls/fuzz): five MLS targets only. Missing and worth adding: `Envelope::from_bytes`, `IdentityCard::decode`/`DeviceList::verify`, `GroupState` + `Chain::add`/`from_checkpoint` (would have found H2), `LinkOffer::decode` and `Assembler::add`, `SyncMsg::from_envelope`, `Attachment::validate`, wire `ServerMsg` deserialisation, and the `process` target should also vary the Directory (revoked/stale owners) and compare openmls storage before/after a refusal, not only epoch and member count. The existing `process` target rebuilds the whole world per input, which limits exec/s.
- I7. Bridge command surface: `send_file`/`group_send_file` read any path the caller names (bridge.rs:221-235); acceptable for the native Android UI, but the desktop WebView must not forward a path that came from message content. `set_allow_insecure` can be flipped by one JSON call with no confirmation; gate it in the UI.
- I8. `unique_path` (bridge.rs:268) is check-then-write (TOCTOU) and `std::fs::write` follows symlinks; use `create_new`.
- I9. `SPIKE_PASSPHRASE` and `--passphrase-file` are documented; file permissions of the passphrase file are not checked.
- I10. Recv robustness (generalising H2): `process`, `groups::on_envelope`, `large::on_welcome` and `agent::*` all use `?` on local I/O; any persistent I/O error also freezes the queue. Treat per-message errors as non-fatal.

## What I checked and found fine

- Vault (core/src/vault.rs): Argon2id parameters bounded on read (1 GiB, 20 passes, salt >= 8 bytes), fresh random 24-byte XChaCha20 nonce per seal (192-bit, no practical reuse risk), file name as AAD, wrong-passphrase check does not leak, `index_tag` uses HKDF-style subkey derived with a label and unambiguous separator, `Vault` zeroized on drop. Header tampering only causes unlock failure.
- Signature contexts: card, prekey, device list, group state, agent request/grant/receipt/audit, presentation, mailbox create/append/read/subscribe all have distinct context strings; mailbox requests are length-prefixed and bound to device id, mailbox id, kind, expected seq, blob digest and seal; MLS leaf credential signs owner, leaf key (length-prefixed), device and list version under `spike-mls-leaf-v2`.
- Attachments: per-file random key, AAD binds blob id, index and total, size and hash re-verified, chunk size bounds checked before decrypt, `chunks == chunks_for(size)`; `safe_file_name` strips separators, `..`, control chars and leading dots; bridge `save_attachment` and CLI `fetch` use the sanitised name; the bridge never joins an unsanitised name.
- Link crypto: HKDF salt = QR secret, info binds both ephemerals and the card, fresh random 24-byte nonces per frame, header (seq, total) authenticated, `MAX_FRAMES` bound before allocation, hello length/magic checked, safety code from HKDF output (no offline test for an outsider), bundle installed only after `install_identity` validates owner signature and that this device is listed.
- Own-device sync accepted only from devices in your own signed list; contacts' sync messages are rejected before any action (cli/src/lib.rs:397); file offers must match hash, size and name; catch-up records validated and merged under the author rules.
- Device-list rules (rollback, equivocation, key change) in `check_update`; envelope shape/size validation (id, to, group, target, kind-specific fields, 50 KB body); intro only on Text/GroupState and bounded (16 KB, no control chars).
- Message requests: sessions kept in a separate quarantine store, cap of 50, TTL 30 days, per-request held-message cap (5 x 4 KB), attachments dropped, intro card must list the sending device, blocked keys dropped before a session is created, session forgotten on refusal.
- Agent flow: request signed by the contact's own owner key and bound to the principal, receipt verified against the exact request hash, grant re-checked at decision time, daily cap uses `saturating_add`, verifier checks audience, expiry, scope, amount, typed fields and single use (SQLite primary key, atomic); overflow in `check`/`redeem` is unreachable because the earlier bounds checks run first (traced `presented`, `created`, `expires`).
- MLS wrapper: whitelist of proposal types (everything else `Forbidden`), standalone proposals refused, external/ReInit/PSK refused, committer's own credential re-verified, commit authority via `policy::authorize` (add/remove/policy/duplicate device/caps/identity change) before merge, state snapshot restored on refusal, join verifies every member before keeping state, bundle sealed under an exporter of the joining epoch, removal rotates via next-epoch exporter, outer AEAD binds group, generation and kind with a random 24-byte nonce.
- Transport: `wss` with webpki roots plus optional extra CA, plain `ws` only to loopback unless explicitly allowed, `localhost.evil.com` style hosts rejected, 10 s connect timeout.
- Bridge: every command is wrapped in `catch_unwind`; DTOs contain no keys, cards are public data; `qr_svg` capped at 1200 bytes; `wait_for_push` capped at 120 s; JNI wrapper only passes strings.
- No secret printed or logged in core, cli ops, or app (grepped for println/eprintln/dbg/log).
