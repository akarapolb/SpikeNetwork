# ADR 0025: Large groups with MLS

Status: **Proposed. For your review; not built.** Nothing here changes existing code or groups.
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
- **Rotation on removal:** a removal commit carries a new mailbox id and key. The committer appends it to the old mailbox with a *seal* (the relay then refuses further appends there) and the new mailbox starts. A removed member cannot process the removal, so she never learns the new mailbox; a member who was merely offline replays the old log, reaches the removal commit and finds the new one. Adds do not rotate.
- **Retention:** handshake messages (commits, Welcomes) are kept much longer than application messages (proposed: 180 days against 30), because a device that misses one commit cannot read anything afterwards. A cap per mailbox protects the relay (and ties into the tenant quotas of ADR 0023).
- **Sizes:** the 64 KiB limit is wrong for trees and bulk commits. Welcomes carry the ratchet tree as an encrypted blob in the existing chunked blob store (pointer and key in the Welcome); commits above the message limit use the same route.
- **Key packages:** the existing prekey publish and claim extended with a kind, plus a "last resort" package per device for when the pool is empty.

### 5. Metadata: wrap every MLS message
MLS message headers expose the group id, epoch and message type. Every message is wrapped in an outer AEAD layer keyed from a secret that members hold (generated with the mailbox generation, rotated with it), so the relay sees only (mailbox id, opaque blob). **What the relay still learns, and says so:** that a mailbox exists, message sizes and times, a commit-versus-message pattern from sizes, and which authenticated devices read which mailbox (it authenticates devices on connection; pairwise queues expose the same kind of fact). It does not learn the group id, names, member list or content. This is a regression only if the wrapping is skipped.

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

## Decisions I need from you
1. **MLS as the path above 20, with the kind chosen at creation and no conversion in the first version.**
2. **The first-release cap:** 500 people and 1000 devices as proposed, or lower (200?) until phones are measured.
3. **One leaf per device,** accepting that linking and revoking a device need an online device and a commit per large group.
4. **The relay changes and the metadata trade** in sections 4 and 5, including the longer handshake retention.
5. **No external commits in the first version:** a device offline too long is re-added by a person.
6. **Shipping policy for an unaudited dependency:** build now with the exact pin and the safeguards in section 8, but treat an independent review as a gate before any real use (recommended), or require it before the CLI slice.
7. **Go ahead with slices 0 and 1 only** (the relay mailbox with fake payloads, then the `spike-mls` crate), with no client or UI work until they pass.
