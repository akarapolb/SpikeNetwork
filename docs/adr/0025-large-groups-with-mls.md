# ADR 0025: Large groups with MLS

Status: **Accepted 2026-10-23 for slices 0 and 1** (decisions recorded below). **Slice 0 (the relay mailbox) is built; slice 1 (`spike-mls`) is not started.** Nothing here changes existing groups or clients.
Date: 2026-10-23

Evidence: `spikes/openmls/REPORT.md` (a feasibility spike, committed with its code and raw data). Read its verdict and its list of what is unverified before this ADR: several choices below rest on design reasoning about the relay that nobody has measured.

## Problem
ADR 0024's groups send every message separately to every device of every member over one-to-one sessions. That is simple and needs no new cryptography, and it stops at 20 people for good reasons: the sender does one encryption and one relay upload per device (about 96 bytes each, so 48 KB per message at 500 devices), nothing proves two members hold the same group state, and group calls (ADR 0018) need one shared group key. Organisations that want 50, 200 or 500 people in a group have no option. ADR 0024 already decided the next step is MLS (RFC 9420), preceded by a spike. The spike is done.

## What the spike showed (short)
- `openmls` 0.9.2 builds on Windows with the pure-Rust provider and compiles for Android and iOS, with no C toolchain. A full add, send and remove lifecycle works.
- A send is **one 177-byte ciphertext at any size**. A member change costs about 1.5 to 2 KB (a few milliseconds) once members have each updated their key once; right after a bulk add it is large (42 KB at 500 devices), so every new device updates its key after joining.
- The ratchet tree is the heavy object: about 185 KB at 500 devices, 285 KB at 1000. It does not fit the relay's 64 KiB message limit above roughly 200 devices.
- Cost is not the obstacle. The obstacles are the relay (ordering, fan-out, retention, resync), identity (one leaf per device, credentials, linking), and **metadata** (MLS headers show the group id and epoch in the clear).
- `openmls` is unaudited, has had seven security advisories since September 2025 (two High) and changes its API in most minor releases.

## Decision (proposed)

### 1. Scope: two kinds of group, chosen at creation
Groups of up to 20 stay pairwise (ADR 0024, unchanged). A **large group** uses MLS, is chosen when the group is created ("Large group" switch), and cannot be converted in either direction in the first version: moving a pairwise group to MLS would need every member to rejoin, and nothing is gained by pretending otherwise. A conversion can be a new group with an invite. **Proposed cap for the first release: 500 people and 1000 devices**, to be revised after measuring on phones (not measured) and after the relay work below.

### 2. MLS parameters and dependency
Ciphersuite 1 (`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`: mandatory in the RFC, matches our Ed25519 and X25519 keys, hardware AES on phones). The RustCrypto provider. `openmls` **pinned to an exact version**, isolated in a new crate (`spike-mls`) so that `spike-core` stays free of it and the audit and build surface of the new dependency is one crate. Upgrades are deliberate chores with the advisory list read first.

### 3. Identity: one leaf per device, bound to the person's owner key
Each device is a leaf (the MLS norm; the 16-device cap per person makes 20 people at most 320 leaves). The leaf credential is the spike's design: the person's owner public key, the device id, and an owner-key signature over the leaf's signature key and device id (112 bytes). Our code checks it on **every** leaf that a commit or Welcome introduces, before the commit is merged (`openmls` has no callback for this; the spike shows where to check). Two additions the spike did not build:
- **Freshness:** the credential also carries the version of the owner's signed device list it was issued under, and a validator rejects a leaf whose device is absent from the owner's current signed list (fetched through the existing directory). This closes the "revoked device stays valid until someone commits a Remove" gap.
- **Who may commit what:** the group's policy (name, admins, who can add, what new members see) lives in an MLS group-context extension, updated only by admin commits. A commit that adds or removes members is valid only if its sender's person is an admin, or the policy allows any member to add (adds only, never removes). Members validate this on the staged commit and refuse a violating one, so a malicious member cannot rewrite the group. This replaces the signed state chain of ADR 0024 for large groups (the epoch order does its job).

Consequences for devices: linking a new device means an Add commit in every large group the person is in, sent by one of their other online devices (MLS secrets cannot be copied, which ADR 0024's link bundle does for pairwise groups); removing a device means a Remove commit per group. A person whose only device is offline cannot be added to a group.

### 4. The relay: a per-group mailbox (the critical path; needs a prototype)
The spike lists what MLS needs and today's relay lacks. Proposed design, to be proven in a first slice before any client work:
- **A group mailbox** is an append-only log with relay-assigned sequence numbers. Its id is random and chosen by the creator; the relay holds one **Ed25519 public key** per mailbox generation and accepts appends and reads only when signed by the matching private key, which every member holds (delivered in the Welcome, carried in the group-context extension). The relay never learns a member list or any MLS content.
- **Ordering:** a commit is appended with the sequence number the sender last saw (`expected_seq`); the relay accepts exactly one append per number. The loser discards its pending commit and redoes its change on the winner. This gives a total order without the relay reading the epoch.
- **Fan-out:** one append, stored once; members read from a cursor (`after_seq`) and are pushed a notice over their live connection (a subscription signed by the mailbox key). One upload replaces N x D uploads.
- **Rotation on removal (corrected in slice 1):** a removal commit moves the group to a new mailbox, but the new mailbox's key is **not carried in the commit**. The first draft said a removed member "cannot process the removal"; slice 1 tested that and it is false: the removal commit is encrypted under the epoch she is still in, so she can read everything inside it (`mls/tests/mls_facts.rs`). Instead the new generation's secret is **derived from the next epoch's MLS exporter** (label `spike-mailbox`, context = the generation number), which she cannot reach. The committer can compute it from its pending commit before the relay has accepted it, so the commit that closes the old mailbox can also open the new one (the seal needs the successor's public key and proof of possession). Members derive the same secret when they merge the commit; a member who was offline replays the old log, merges the removal, and derives the successor by herself. Adds do not rotate; a newcomer is given the *current* generation in a **join bundle** sealed under the new epoch's exporter and delivered with the Welcome (an old member's epoch-derived value would differ).
- **Retention:** handshake messages (commits, Welcomes) are kept much longer than application messages (proposed: 180 days against 30), because a device that misses one commit cannot read anything afterwards. A cap per mailbox protects the relay (and ties into the tenant quotas of ADR 0023).
- **Sizes:** the 64 KiB limit is wrong for trees and bulk commits. Welcomes carry the ratchet tree as an encrypted blob in the existing chunked blob store (pointer and key in the Welcome); commits above the message limit use the same route.
- **Key packages:** the existing prekey publish and claim extended with a kind, plus a "last resort" package per device for when the pool is empty.

### 5. Metadata: wrap every MLS message
MLS message headers expose the group id, epoch and message type. Every message is wrapped in an outer AEAD layer keyed from the mailbox generation's secret (which also gives the relay signing key and the mailbox id, by separate HKDF labels), so the relay sees only (mailbox id, opaque blob). **What the relay still learns, and says so:** that a mailbox exists, message sizes and times, a commit-versus-message pattern from sizes, and which authenticated devices read which mailbox (it authenticates devices on connection; pairwise queues expose the same kind of fact). It does not learn the group id, names, member list or content. This is a regression only if the wrapping is skipped.

### 6. Missed commits and resync
Replay of the mailbox log is the normal path. If the log was trimmed before a device caught up, the device is **dead in that group** until an online member re-adds it with a new Welcome; the app shows "needs to rejoin". **No external commits in the first version:** they would let anyone holding the group info join, and an advisory from October 2026 concerns removed leaves reappearing after an external commit. The cost is that an offline device may need a person to re-add it; that is acceptable for a first version and visible in the UI.

### 7. Carry-over from ADR 0024
Messages inside the group are the same envelopes (text, edit, delete, attachment): editing, deleting, files (one encrypted blob for all) and own-device catch-up work unchanged, and one's own other devices simply receive messages as members (no "sent copy" needed). New-member history ("the last 7 days") is the same request and answer, now over MLS application messages, with the same "shared by" marking: MLS gives a new member no past messages either. Members who are not contacts, invites, blocking and consent (ADR 0024 sections 5 and 6) are unchanged in meaning. Group calls (ADR 0018, item 5) become possible: the MLS exporter secret gives the shared group key that SFrame needs.

### 8. Storage and hygiene
`openmls` needs a `StorageProvider`; ours is built on the vault's sealed storage (an SQLite provider exists to learn from; the effort is not measured). The new crate gets a fuzz target for message and extension parsing (two of the advisories are parsing denial-of-service), a CI check that fails on any new `openmls` advisory (`cargo-deny` already runs), and the exact pin.

## What this does not give you (honest list)
- **No independent audit** of `openmls`. The README's "do not use for real secrets" stays; large groups add a dependency that is fast-moving and has had High advisories this year.
- **The relay can still reorder, withhold or split views** (show different logs to different members). MLS makes each epoch's membership cryptographic, but only members comparing epoch authenticators (a "group safety number", not built) can catch a relay that equivocates.
- **Removed members keep what they already received**, and any member can copy messages elsewhere.
- **Dead devices:** a device offline longer than the handshake retention needs a person to re-add it.
- **Linking and revoking a device is heavier:** a commit per large group by an online device.
- **Metadata** as in section 5. Group size, activity pattern and readers' device ids remain visible to the relay operator.
- **Phones are unmeasured**, as are the libcrux provider, a linked Android library, and a 1000-device tree where every member has updated.
- **Concurrent admin edits** are serialised by the epoch order, so one of two simultaneous changes is redone; no merge.

## Alternatives considered
- **Raise the pairwise cap.** Feasible to roughly 100 devices (about 9.5 KB per send), but still O(devices) per send, no shared group key and no calls. A reasonable stopgap if MLS slips; it does not reach 500.
- **Sender keys (Megolm via `vodozemac`).** Rejected in ADR 0024 for the same reasons: it needs the same relay fan-out, removal re-keying is quadratic, and every group would migrate twice.
- **One leaf per person with shared state.** Unsupported by `openmls`, and loses per-device forward secrecy. Rejected.
- **Virtual clients** (a draft, in `openmls` behind a feature flag): a person's devices act as one MLS client. Would remove the per-group commit on linking, at the cost of an extra emulation group per person. Unverified; worth a second spike if linking proves painful.
- **External commits for resync** and **an off-the-shelf delivery service**: the first is deferred for the reasons in section 6; the second would put another party's server in the trust path of a product whose point is the relay we operate.

## Build order (proposed)
0. **Relay slice:** group mailbox (create, append with `expected_seq`, read from a cursor, subscribe, seal), the larger and chunked sizes, key package kinds, retention classes, quotas. Integration tests with fake MLS payloads: ordering under concurrent appends, removed-member exclusion across rotation, a lagging reader replaying across a seal. No MLS code yet; this proves or kills section 4.
1. **`spike-mls` crate:** the spike's wrapper, the owner-bound credential with freshness, the policy extension and commit-authority checks, merge-after-accept, a `StorageProvider` on the vault, a fuzz target. Tests: lifecycle, credential splicing, policy violations, two concurrent commits, replay across rotation.
2. **CLI** (`spike group new --large` and the existing group commands), including device linking and "needs to rejoin".
3. **Desktop** screens (reusing the group screens; a "Large group" switch and a size note).
4. **Hardening:** phone benchmarks on real hardware, the advisory watch, fuzzing time, and an independent review of the credential design and the relay protocol before any real use.

## Decisions (2026-10-23)
Accepted as proposed, with the recommended defaults:
1. **MLS is the path above 20**, the kind chosen at creation, no conversion in the first version.
2. **First-release cap: 500 people and 1000 devices.**
3. **One leaf per device**, with linking and revoking a device needing an online device and a commit per large group.
4. **The relay changes and the metadata trade** in sections 4 and 5, including the longer handshake retention.
5. **No external commits in the first version.**
6. **An independent review is a gate before any real use.** Build now with the exact pin and the safeguards in section 8.
7. **Slices 0 and 1 only** (the relay mailbox with fake payloads, then the `spike-mls` crate): no client or UI work until they pass.

## Slice 0: what was built (2026-10-23)
The relay side of section 4, with fake payloads, before any MLS code. Everything below is in `relay/`, `core/src/mailbox.rs` and `core/src/wire.rs`, with client calls in `cli/src/client.rs` and tests in `cli/tests/mailbox.rs`.

**Protocol.**
- A mailbox is registered with its id and an Ed25519 *public* key (`GroupCreate`; the new key signs the request, so whoever registers holds the private key). Every later request (`GroupAppend`, `GroupRead`, `GroupSubscribe`) is signed with that key. Each signature covers the **requesting device's id** (the relay knows which device authenticated the connection), the mailbox, and every field of the request, with length-prefixed fields and a context string per request type: a signature captured on one connection cannot be replayed by another device, one request's signature is useless for another, and no two requests produce the same bytes.
- An entry is a `Message` or a `Commit`. A **commit** must say the position it expects and is accepted only if the log is exactly there (compare-and-append): of two concurrent commits one gets the position, the other is told `GroupConflict { next_seq }`. Messages have no such rule. Welcomes are not in the mailbox (they go to the new device's own queue, as today).
- A commit may **seal** the mailbox and name a successor (id, public key, and a signature by the successor key proving possession). Closing the old mailbox and opening the new one happen in one transaction: a bad proof, a stale position, a successor id that exists, or a device over its quota leaves everything unchanged. A sealed mailbox refuses appends but can still be read, so a member who was offline replays it up to the sealing commit and finds the handover there.
- **Fan-out:** `GroupSubscribe` registers the connection; each append is stored once and pushed (with its position) to every subscriber. Subscribing returns where the mailbox is, not a backlog: subscribe first, then read from your own cursor (`GroupRead`, capped at 200 entries), and use the positions to tell pushes from what you read.
- **Key packages** (the other half of the ADR's slice 0): `PublishKeyPackages`, `PublishLastResortKeyPackage`, `ClaimKeyPackage`. Opaque to the relay; one-time packages are handed out once each, in order, then the reusable last-resort package repeats; at most 200 per device and 16 KiB each; claims share the per-connection claim limit with prekeys so a stranger cannot drain a device. The relay cannot check a package, so whoever claims one checks it and the credential in it.

**Limits and retention** (all relay flags, see `docs/deploy-relay.md`): entries up to 256 KiB; messages kept 30 days, commits and a mailbox after its last append 180 days; 64 MiB per mailbox, past which the oldest *messages* (never commits) are dropped and a mailbox full of commits says `mailbox_full`; 200 mailboxes per creating device (a seal counts for the device that seals); 256 subscriptions per connection; positions are never reused. Storage is SQLite schema version 3, applied with the existing backup-before-upgrade and refuse-newer-database rules. Metrics add counts only (mailboxes, entries, bytes, appends, pushes, reads, key packages), with no id in any name or label.

**How it was tested.** 16 integration tests against a real relay (`cli/tests/mailbox.rs`), store unit tests including the upgrade from the previous schema, and core tests for the signing bytes. **Eighteen protections were each broken in turn and a test failed every time** (signatures, device binding of the position check, commits needing a position, only commits sealing, proof of the successor key, existing successor ids, sealed mailboxes, quotas, retention classes, never dropping commits, subscription cap, one-time package consumption and caps), with a clean control run before and after. Two things went wrong on the way and are worth knowing: the relay's connection eviction broke (an evicted connection's channel stayed open because the new subscription registry held a clone of its sender; fixed with weak senders, with a regression test that includes a subscribed connection), and the first mutation harness left a stale binary behind (restoring a file with an old timestamp makes cargo keep the mutated build), which invalidated its first results until it was rewritten and rerun with control runs.

**Differences from the proposal above.** Subscribing returns no backlog (see above). A seal needs the successor key's proof of possession, which the proposal did not say. The outer-layer secret (section 5) and the handover of the new mailbox key are entirely client matters: the relay never sees either, so slice 0 cannot test them; slice 1 must.

**What slice 0 does not show.**
- **Throughput is bounded by the disk flush, at about 270 appends a second on the development machine** (`cli/tests/mailbox_bench.rs`, release build, a real database file, loopback, 177-byte entries; run it by hand with `--ignored --nocapture`). The rate was the same, 264 to 277 a second, for 1, 8 and 32 concurrent senders and for one shared mailbox or one each: that flatness means the limit is the one flush per append, not the protocol or the number of mailboxes, and a faster disk or batching commits would raise it. One relay process therefore handles a few hundred appends a second across *all* its groups. For scale: a 500-person group where each person sends once every ten seconds needs about 50 a second; everyone sending in the same second takes about two seconds to land. The relay's existing per-message queue is also one transaction per message, so by the same reasoning the pairwise design costs a relay one flush per recipient device (500 flushes, about two seconds, for one message to 500 devices) against one for a mailbox. That comparison is inference from the same bound, not a measurement of the pairwise path. Not measured: other disks, a networked or busy relay, a database that has grown large.
- **Any current member can fill a mailbox** with garbage up to its cap, evicting older messages (never commits); the only brakes are the per-connection request rate and the mailbox size cap. A removed member can no longer append once the removal commit seals the old mailbox, but between a member turning hostile and being removed nothing stops this.
- **The relay still sees** which authenticated devices read and write which mailbox, sizes and times, and the number of entries, as section 5 says.
- **Ordering is by the relay.** A dishonest relay can still show different logs to different members; only members comparing epoch authenticators can notice (not built).
- **Nothing uses it yet.** No client creates a mailbox; the deployment guide says so.

**For slice 1:** wrap every message in the outer layer and treat `GroupConflict` as "discard my pending commit and redo it on the winner". (The first draft also said to put the new mailbox key inside the removal commit; that was wrong, see section 4. Slice 1 is below.)

## Slice 1: what was built (2026-10-23)

The `spike-mls` crate (`mls/`), openmls 0.9.2 and its companion crates pinned exactly. It is a library only; no client, relay or app uses it yet except its own tests.

- **`credential`**: the leaf's credential identity is `format | owner key | device id | device-list version | owner signature`, the signature covering the leaf's own signature key. `verify_leaf` checks the signature, then the owner's *current signed device list* from a `Directory`: a device not in it is `Revoked`; a credential from a newer list than the directory holds is `StaleDirectory` (refresh and retry, never accept); a list that is not signed by that owner is not trusted. This closes the spike's gap that a removed device's credential stays cryptographically valid.
- **`policy`**: the group's rules live in a GroupContext extension (so joiners get them in the Welcome). `authorize` is a pure function over policy, committer, roster and the commit's changes: admins may add and remove anyone; others may manage their own devices only (and add or remove others only if the policy says so); one leaf per device; the 500-person / 1000-device ceilings; an update may not change who a leaf is; only admins change the policy and it must stay valid; external, ReInit and PSK commits are refused.
- **`group`**: `Group` wraps an openmls group. A commit we make is **pending**: the epoch does not move until the relay accepts it and the caller says `confirm`; `discard` drops it; processing someone else's valid commit replaces ours, and an *invalid* commit from someone else does not cost us our pending one. Every incoming commit has all its credentials verified and its changes authorised **before** `merge_staged_commit`. Reading a commit uses up the secrets that opened it, so commits are processed transactionally: the openmls storage is copied first and put back on any refusal, which makes "stale directory, refresh, offer the same bytes again" work. A Welcome is treated the same way (it consumes the key package), and every member in it is verified before the group is kept. Our own commits and messages coming back from the relay are recognised and reported as `Own`.
- **`outer`**: one 32-byte secret per mailbox generation gives the relay signing key, the mailbox id and an XChaCha20-Poly1305 key (separate HKDF labels). Every MLS message is sealed with the group, generation and kind as associated data.
- **`state`**: the device's keys, openmls storage and each group's mailbox state, sealed in one vault blob bound to its vault and name.
- **`mls/fuzz`**: five cargo-fuzz targets (process, join, key package, policy, credential). **They compile but have not been run** (cargo-fuzz needs a nightly toolchain, which this machine does not have). `mls/tests/mutation.rs` is the stand-in: every single-byte change and truncation of a real commit, message, Welcome, bundle and key package is fed in; nothing panics, nothing mutated is accepted, and a refusal leaves the group unchanged.

**How it was tested.** 52 tests in the crate: credential 8, policy 10, outer 4, group lifecycle 16, vault state 3, mutation 4, openmls facts 4, and **3 against the real relay** (`mls/tests/relay.rs`: talking through sealed mailbox entries, two admins racing for one position with the loser redoing it on the winner, and a removal sealing the old mailbox while the removed member can neither read the new one nor append to the old). **Twelve protections were each broken in turn and a test failed every time** (`mls/mutcheck.py`: commit validation before merge, owner signature, freshness, revocation, authority rules, restore after a refused commit, restore after a refused Welcome, rotation on removal at both committer and receiver, Welcome member verification, one leaf per device, identity change), with clean control runs before and after. The Welcome member check was not covered at first; the harness found that and a test was added. (The harness has to touch restored files, or cargo keeps the mutated build.)

**Findings that change the design.**
- The rotation correction above.
- Welcomes and commits consume secrets, so a client that cannot retry them is fragile; the snapshot is the answer in the spike. At 500 members the snapshot is a copy of the whole openmls storage per *commit* (not per message); that cost is unmeasured and may need a smarter transaction.
- Application messages from up to three epochs back are accepted (a message sent just before a commit can arrive after it). That keeps three old epochs' secrets alive and weakens forward secrecy by the same amount. A choice, not a requirement.
- Standalone proposal messages are refused outright: every change travels inside a commit.

**What slice 1 does not show.**
- **Not independently reviewed.** The ADR decision stands: independent review before anything real depends on this. openmls itself is unaudited and has had High advisories.
- **Licence:** openmls pulls in `hpke-rs` (MPL-2.0). `deny.toml` carries three per-crate exceptions. **Accepted by the project owner, 2026-10-23.**
- **Rejected commits stall a device.** A commit every member refuses still occupies a relay position; what a client does then (skip it, mark the group broken) is slice 2's problem and is not decided.
- **Nothing here is used by a client** (slice 2 below does). "Needs to rejoin" and leaving are still open. The relay test makes `spike-mls` depend on the CLI in tests while the CLI depends on `spike-mls`; Cargo allows that cycle through a dev-dependency, and it builds.
- **Scale, measured once (below); other conditions are not.**

**Scale measurement (2026-10-23).** `mls/tests/scale.rs`, release build, one machine, no network, one device per person, a group grown by adding people and then one more person added and one removed at each size (run by hand with `--ignored --nocapture`):

| members | add one: build commit | commit size | Welcome size | observer applies it | newcomer joins | remove one: build commit / size | observer applies | sealed device state |
|---|---|---|---|---|---|---|---|---|
| 20 | 1.8 ms | 2.5 KB | 6.7 KB | 1.4 ms | 6.5 ms | 1.4 ms / 2.1 KB | 1.1 ms | 117 KB |
| 100 | 4.2 ms | 9.2 KB | 30 KB | 2.7 ms | 29 ms | 2.9 ms / 8.8 KB | 2.6 ms | 617 KB |
| 250 | 7.8 ms | 21.5 KB | 74 KB | 7.5 ms | 75 ms | 7.5 ms / 21 KB | 5.7 ms | 1.7 MB |
| 500 | 15 ms | 42 KB | 147 KB | 13 ms | 141 ms | 14 ms / 42 KB | 12 ms | 3.6 MB |

Everything grows roughly linearly and is cheap in time: a change at 500 members costs about 15 ms of work per device, so the snapshot taken before each commit (a copy of the same storage that is sealed to 3.6 MB, which takes 16 ms including encryption) is not a problem at this size. **Sizes are the finding.** A single-change commit at 500 members is 42 KB, under the relay's 64 KiB message limit, but a Welcome is 147 KB and a batch of adds would be larger, so the blob route in section 4 is needed (Welcome at about 250 members, commits earlier for batches), not optional. Still unmeasured: debug builds, a real network or a slow disk, devices with several leaves per person, a tree that has had many removals (blanked nodes), and phones.
- Fuzz targets unrun, as above.

## Slice 2: what was built (2026-10-23), the CLI core

`cli/src/large.rs` and `spike large ...`: `keys`, `new`, `list`, `add`, `remove`, `accept`, `decline`, `send`, `sync`, `history`. It is the glue around `spike-mls`; the cryptography and rules are unchanged.

- **Keys and state.** MLS key packages are published to the relay's key-package store (`large keys`); the device's MLS keys, openmls storage and each group's mailbox state are one sealed file in the home (`large-mls.enc`), records of groups another (`large-groups.enc`). A commit is **saved while pending, before it is sent**, and a message's ratchet is saved before the message is sent, so a crash can never reuse a message key and a commit that landed just before a crash is confirmed by the next sync when it sees its own entry.
- **Who is who.** Credentials are checked against the device lists of contacts, our own, and lists fetched from the relay. When a credential cannot be checked because a list is missing or old, the library reports which owner, the client fetches that signed list and offers the same entry again (this is what the transactional processing in slice 1 is for). That is how people who are not each other's contacts can be in one group.
- **Invitations need consent.** `add` claims one key package per device of the contact, commits, and sends the Welcome as an **uploaded encrypted file** (a new envelope kind, `large_welcome`; a Welcome at 500 members is 147 KB, far over the envelope limit, which settles the blob-route question for Welcomes) plus a small message. The invitee sees a pending invitation and **joins only on `accept`**. Nothing is read or sent for a group not joined.
- **Races.** `add` and `remove` sync first, build the commit, save, append at the next position, and on a lost race discard it and redo it on the winner (five tries).
- **Removal.** The group moves to the next mailbox in the same commit (slice 1); other members follow on their next `sync`; the removed person learns it from the old mailbox, keeps her history, and cannot send.

**How it was tested.** 5 integration tests on a real relay (`cli/tests/large.rs`): people who are not contacts talking, consent, removal and rotation (both directions after it), missing key packages, restart. 4 protections mutation-checked (`cli/mutcheck_large.py`: fetching unknown members' lists in sync and in accept, resetting the cursor after rotation, refusing an add with no key package). One core test for the new envelope kind. Plus a run of the real binaries (`mls/demo_large.sh`): a relay and three people, invite, accept, talk, remove, history. The mutation run first showed a gap (nobody read from the new mailbox after a rotation) and the test was strengthened.

**What it does not do.**
- **No instant delivery:** `spike large sync` polls; `recv --follow` does not know about large groups yet.
- **No leaving, no block, no admins beyond the creator, no change of policy, no new-member history, no files, no editing or deleting.** Policy is fixed at creation (admins add and remove).
- **Own other devices are not handled:** the invitation goes to the contact; a second device of yours does not join, and a person's second device is added to a group only when the person is added (a device linked later is not added to existing groups).
- **A rejected entry is skipped and reported** (`entry N refused`), and the cursor moves past it. That keeps a group usable but means a malicious or buggy commit is silently survived rather than escalated; whether to mark the group as broken is undecided.
- **The Welcome file expires with the relay's blob retention.** An invitation left unaccepted too long cannot be accepted; the inviter has to add again, and the first commit stays in the group (a stale member who never joins). No tooling for that yet.
- **A device in a group that never accepts** is a member who cannot read anything; removing it is manual.
- **Several relays:** all of a group's mailbox traffic is on the relay it was created on, and key packages are claimed from each device's own relay.
- Desktop screens are slice 3. Review, advisory watch and the independent review (slice 4) are still ahead; nothing here has been reviewed.

## Slice 3: what was built (2026-10-23), the desktop screens

Large groups sit in the same chat list and use the same screens as small ones, marked **Large**; the app decides by the group's id.

- **Create:** the New group screen has a Small / Large switch. A large group takes any number of your contacts, shows that people join by accepting and that it is experimental and not independently reviewed, and asks for confirmation above 20 people. The creator is the only admin and the policy is fixed (admins add and remove).
- **Invitations:** the same consent screen, with the member list hidden (you cannot see who is in it until you join) and no Block button. Declining affects this device only.
- **Thread and members:** one encrypted copy for everyone, with the same marks for people you have not verified and the same "~" names for people who are not contacts. Admins add people (up to 500) and remove them; removal moves the group to a new mailbox. The window title and unread counts include large groups and open invitations.
- **Not offered, and the app says so rather than pretending:** renaming, leaving, a second admin, "who can add", new-member history, files, blocking an inviter. The same commands refuse with "... is not available for a large group yet".
- **Sync:** the app's normal sync reads the one-to-one queue, then the large-group mailboxes (every 60 seconds and on wake-ups; there is no push for large groups yet). A failure reading them becomes a note and does not hide what the queue delivered. Once per run it tops up this device's key packages to 20 so people can add it.
- **Code:** `Session` (`desktop/src-tauri/src/api.rs`) dispatches each group call on whether the id is a large group; one new command, `group_create_large`; helpers in `cli/src/large.rs` (members, unread, mark read, remove by owner key, key package top-up).

**How it was tested.** 4 new desktop API tests against a real relay (create, invitation and consent, decline, messages with names and unread, removal and re-adding, refusals for what is not built), plus the CLI mutation checks again after the refactor. **The real app was driven:** `desktop/drive_large.mjs` starts a relay and three desktop windows (alice, bob, carol) and drives them over the Chrome DevTools protocol: Alice makes a large group from the screen, the others see and accept the invitation, messages go all ways (Carol reads Bob's under a "~" name, since she does not know him), Alice removes Carol, Carol is told and cannot write, Bob and Alice carry on. 18 checks, all pass, with screenshots of the new screens. Windows only.

**What it does not do.** Everything the CLI section lists as missing is missing here too, and: no desktop notification or instant delivery for large groups; no way to see that an invitation was never accepted; no per-person delivery status (everyone reads one copy); a person with several devices appears once and "Your devices" is not itemised; and the Large tag, wording and layout were looked at on one machine and one window size only.
