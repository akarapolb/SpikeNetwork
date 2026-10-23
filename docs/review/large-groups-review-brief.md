# Large groups: brief for an independent reviewer

Status: prepared 2026-10-23 (ADR 0025, slice 4). The project owner decided that an independent review is a
**gate before any real use** of large groups. This brief is what a reviewer needs to start; it was written by
the author of the code, so treat its claims as claims.

## What is being asked

Review the design and implementation of large groups (up to 500 people) built on MLS (RFC 9420) with the
`openmls` crate, bound to this project's identity model. The questions that matter most:

1. **Is a leaf really bound to a person, and does revocation really work?** (credential design, below)
2. **Can any member, or the relay, make members disagree about who is in the group, or take rights they
   should not have?** (commit-authority rules)
3. **Does removing someone really shut them out, including from the relay mailbox?** (rotation design)
4. **Is anything hostile-input-reachable that can crash, hang or corrupt a member?** (parsing boundary)
5. **Is the use of `openmls` itself sound?** (we use a narrow slice of its API; we do not review `openmls`)

Out of scope: the cryptography inside `openmls`, RustCrypto and `vodozemac`; the one-to-one (Olm) messaging;
the small-group design (ADR 0024); the desktop UI beyond what it exposes.

## Where the code is

| Area | Files | Lines |
|---|---|---|
| Credential, directory, freshness | `mls/src/credential.rs` | ~170 |
| Policy and commit-authority rules (pure) | `mls/src/policy.rs` | ~210 |
| Group wrapper: pending commits, validation before merge, rotation, join bundle, transactional processing | `mls/src/group.rs` | ~790 |
| Mailbox key material and the outer AEAD layer | `mls/src/outer.rs` | ~130 |
| State sealed on the vault | `mls/src/state.rs` | ~110 |
| Relay group mailbox (ordering, seal, signatures, retention) | `relay/src/lib.rs`, `relay/src/store.rs` (group parts), `core/src/mailbox.rs`, `core/src/wire.rs` | ~600 of ~2,700 |
| Client glue: keys, invitations, sync, removal | `cli/src/large.rs` | ~980 |
| Test hooks (never in a release build) | `mls/src/testing.rs`, feature `test-hooks` | ~110 |

Design: `docs/adr/0025-large-groups-with-mls.md` (sections 3 to 8 matter most, then the "built" sections for
what was learned). Threat model of the product: `docs/threat-model.md`. The openmls feasibility report:
`spikes/openmls/REPORT.md`.

## The design in one page

- **One MLS leaf per device.** The leaf's credential identity is
  `format | owner_pub(32) | device_id(16) | device_list_version(8) | owner_signature(64)`; the owner key (the
  person's long-term Ed25519 key) signs the leaf's own signature key, the device, and the list version.
- **Freshness.** A leaf is accepted only if the owner's signed device list, *as the group knows it*, names
  the device. The group's lists come only from its own history: those in the Welcome that brought a member
  in, and `LargeLists` messages announcing a new list before any commit that depends on it. Every member
  replaying the log therefore reaches the same verdicts (a list fetched from elsewhere could make a lagging
  member refuse a commit that was legitimate when made, and fall out of step; this was found and fixed in
  slice 5). A credential from a list newer than the group has seen is refused as stale.
- **Policy** lives in a GroupContext extension (so joiners get it in the Welcome): admins, who may add and
  remove, caps (500 people / 1000 devices).
- **Every commit is validated before it is merged**: sender's own credential, every leaf it introduces, and
  the authority rules (`policy::authorize`). A commit that fails is refused identically by every member.
  External, ReInit and PSK commits are refused. Standalone proposals are refused.
- **Merge-after-accept.** A commit we make is *pending*; the epoch moves only after the relay accepts it
  (compare-and-append on a position). A commit that wins first replaces ours; an invalid commit does not.
- **Mailbox.** One append-only log per group generation on the relay, ordered by the relay, every entry
  sealed in an outer XChaCha20-Poly1305 layer so the relay sees no MLS framing. The mailbox key, id and outer
  key are derived (HKDF) from a per-generation secret.
- **Removal rotates the mailbox.** The next generation's secret is derived from the **next epoch's MLS
  exporter**, which the removed member cannot compute (she can read the removal commit itself, so nothing
  secret rides in it). The committer seals the old mailbox and opens the new one in one relay operation.
  Newcomers get the current generation in a *join bundle* sealed under the epoch they join in.
- **Consent.** A Welcome is delivered as an encrypted file plus a small message; the invitee joins only on
  accept.

## Invariants and the tests that pin them

Each was broken on purpose (`mls/mutcheck.py`, `cli/mutcheck_large.py`) and a test failed; a control run
before and after was clean. "Mutation-checked" means exactly that and no more.

| Invariant | Where | Test |
|---|---|---|
| A credential signature covers the leaf key, device, owner and list version | `credential.rs` | `mls/tests/credential.rs` |
| A device not in the owner's current list is refused; a newer list is "stale", not accepted | `credential.rs` | same |
| Every incoming commit is validated before merge | `group.rs::process`, `validate` | `mls/tests/group.rs` (receivers_refuse_*) |
| The committer's *own* credential is checked (a path-less add from a revoked device) | `group.rs::validate` | `a_commit_from_a_revoked_device_is_refused_...` |
| Authority: add / remove / policy / identity change / duplicate device / caps | `policy.rs` | `mls/tests/policy.rs` |
| A refused commit or Welcome leaves state unchanged and can be offered again | `group.rs` snapshot/restore | `receivers_refuse_a_revoked_device_...`, `a_welcome_with_a_member_nobody_vouches_for_...` |
| Removal rotates the mailbox at committer and receiver; the removed member cannot follow | `group.rs`, `outer.rs` | `removing_someone_rotates_...`, `mls/tests/relay.rs`, `cli/tests/large.rs` |
| Newcomers get the current generation, and a wrong or altered bundle is refused | `group.rs` | `a_new_member_is_handed_...`, `a_join_bundle_that_is_wrong_...` |
| Every member in a Welcome is verified before the group is kept | `group.rs::join` | `a_welcome_with_a_member_nobody_vouches_for_...` |
| A pending commit is saved before it is sent; a message ratchet before the message | `cli/src/large.rs` | not directly testable (a crash); reviewed by reading |
| A member offline across a revocation still follows the group (lists come from the log, announced before use) | `cli/src/large.rs` (`GroupDir`, `commit_with`) | `a_revoked_device_is_taken_out_of_the_groups` |
| A rule-breaking commit is skipped, recorded, and the group carries on | `cli/src/large.rs` | `a_commit_every_member_refuses_is_skipped_...` |
| A person's own devices are added and removed to match their signed list | `cli/src/large.rs` | `a_device_linked_later_...`, `a_revoked_device_is_...` |
| Invitations do nothing until accepted | `cli/src/large.rs` | `an_invitation_does_nothing_until_it_is_accepted` |

## What we already know is weak (so you do not have to find it)

- **No independent review of anything yet**, including `openmls` (unaudited; seven GitHub security advisories
  since 2025-09, two High; all fixed in the version we pin: `mls/advisories-reviewed.txt`).
- **The relay orders the log.** A dishonest relay can show different logs to different members; nothing
  compares epoch authenticators yet. It can also drop or delay entries, and learns sizes, timing and which
  devices read which mailbox.
- **Any current member can fill a mailbox** with garbage up to its cap (never evicting commits) until removed.
- **A removed member can append to the old mailbox** until the removal commit lands and seals it.
- **A refused entry is skipped** (reported, cursor advances). A hostile or buggy commit that every member
  refuses survives silently; whether to mark the group broken is undecided.
- **Messages from the last three epochs are accepted** (a message sent just before a commit can arrive just
  after), so three old epochs' secrets live on.
- **The Welcome file expires with the relay's blob retention**; an old invitation cannot be accepted.
- **Directory trust.** Lists are verified (signed by the owner key) but the *first* list for an owner a
  member has never met comes from whoever added them, and is trusted on first sight (the owner key is not
  otherwise verified for non-contacts; the UI shows them as "not verified"). The announcement of a
  revocation is made by the owner's own remaining device; between a revocation and that announcement the
  revoked device is still accepted.
- **Leaving is a request.** A leaver is a member until an admin removes them.
- **A member whose own commit every other member refuses forks off alone** and needs to be removed and
  re-added.
- **Not measured:** phones, a slow disk or network, trees with many blanked nodes, devices with many leaves
  per person. Scale numbers (to 500 members) are in the ADR.

## Evidence so far (and its limits)

- 59 tests in `spike-mls`, 16 in the CLI for large groups, 5 in the desktop API tests for them, core envelope tests;
  `cargo test --all` passes. 13 protections mutation-checked in `spike-mls`, 4 in the CLI glue.
- The real binaries were driven end to end: three CLI users (`mls/demo_large.sh`) and three desktop windows
  (`desktop/drive_large.mjs`).
- **Fuzzing:** five libFuzzer targets in `mls/fuzz` (message processing, Welcome and bundle, key package,
  policy decoding, credential parsing), each seeded from edits of *valid* messages. Results are recorded in
  the ADR's slice 4 section with the time and number of runs; they show the absence of crashes found in that
  time, not absence of bugs.
- Dependency advisories: `cargo deny` (RustSec) on both lockfiles, and `scripts/openmls_advisories.py` for the
  openmls GitHub advisories RustSec does not carry; a weekly workflow runs both.

## How to run things

```
cargo test --all                                   # everything but the desktop (its own workspace)
cd desktop/src-tauri && cargo test                 # desktop API tests
python mls/mutcheck.py                             # break each protection in turn; run from the repo root
python cli/mutcheck_large.py
bash mls/demo_large.sh                             # real binaries; after cargo build -p spike-cli -p spike-relay
cargo test -p spike-mls --release --test scale -- --ignored --nocapture   # cost by group size
cd mls/fuzz && cargo +nightly fuzz run process     # needs nightly and cargo-fuzz (see run_parallel.sh)
```

## What we would like back

A list of findings ranked by severity, with for each: what an attacker (a malicious member, a removed member,
the relay, a network observer) can do, how, and what would fix it. Also useful: anything in the invariants
table you think is *missing*, and a view on whether "one leaf per device, up to 500 people / 1000 devices,
no external commits" is a sound first-release scope.
