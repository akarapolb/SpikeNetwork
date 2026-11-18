# ADR 0023: Tenants and enrollment tokens

Status: **Accepted** by the owner on 2026-11-15, as written. Built (see "Built" below).
Date: 2026-10-20

This is slice 1 of `docs/hosted-relay-plan.md`. It changes what the relay stores about devices, so it is written for a decision before any code.

## Problem
Today any device may register with a relay and use its quota. A relay run by someone else for several organisations needs: a way to say which devices belong to which organisation, separate limits for each, the power to switch an organisation off, and none of it requiring the relay to learn who people are.

## Decision (proposed)

### Tenants and the admin key
A **tenant** is an organisation. It has an **admin key**, an Ed25519 key held by the customer (same primitives as owner keys, ADR 0007). The operator registers the *public* half with the relay when provisioning, together with limits. The relay never holds the private half.

### Enrollment tokens
A device is admitted by a **token** the admin key signs: `{tenant, device_id, not_after, token_id}` under a domain-separated signature (`spike-enroll-v1`). Properties:
- Bound to one **device id**, so a leaked token is useless to anyone else. The device id is public (the relay already sees it); the token adds nothing identifying.
- Short-lived to *redeem* (default 7 days), but the enrollment it creates lasts until revoked, so an admin does not need to re-issue.
- **Redeemed once:** on first sign-in with the token the relay records `device_id -> tenant`. Redeeming again by the same device is harmless; by another device is impossible (bound).
- **Revoked** by the admin key signing `{tenant, device_id, revoke}`; the relay then refuses that device and may delete its queue.

The relay learns exactly: *this device id belongs to tenant T*. No names, emails or identity cards. This is a deliberate line: tokens are about devices, never people.

### Joining from the app
"Join this organisation's relay": paste or scan the token (the QR work in the desktop plan fits here). The app stores the relay address and token sealed with its other settings, presents the token in the sign-in, and shows the organisation's relay as the home relay. A device with no token on a relay that requires enrollment is refused with a clear message.

### Relay modes
- **Open** (today's behaviour, self-host default): anyone may register.
- **Enrolled:** only devices with an accepted token may sign in. Prekey claims and sends **to** an unenrolled device are still allowed so a member can reach a contact on another relay or a different tenant (federation between relays is unchanged: it follows the home relay in the signed device list).
Self-hosters can run enrolled mode with one tenant, so hosted and self-hosted share the code (plan slice 4).

### Per-tenant limits
Devices, queue depth per device, attachment bytes (total), concurrent connections, request rate. Counters are kept per tenant. Over a limit the relay answers a specific error (`tenant_over_limit` with the limit's name) and the app says so in words. Suspending a tenant refuses all its devices; deleting one removes their queued ciphertext and chunks.

### Operator interface
Signed requests, not a web page: create tenant, set limits, suspend, resume, delete, read usage numbers. The operator key is a separate Ed25519 key supplied to the relay at start (public half). Usage numbers are the content-free counters of ADR 0022, split by tenant.

## What it costs (storage and code)
New tables: `tenants(id, admin_pub, limits, state)`, `enrollments(device, tenant, token_id, enrolled_at)`, `revocations`. A schema migration (ADR 0022 makes that routine). Roughly: token format and verification in core, enrollment and counters in the relay, a join step in the CLI and desktop app, operator commands, tests. No change to message encryption, the wire format for messages, or the device list.

## Privacy and risk
- **The relay still learns who talks when**, now also grouped by tenant. Grouping by organisation is more sensitive than a bag of devices; retention of the mapping should be the life of the enrollment and no longer, and counters should be aggregates, not per-device histories.
- **The operator can suspend an organisation**, which is a power over availability (never content). Customers should be told so in the terms.
- **A stolen admin key** can enrol devices into the tenant. Mitigation: devices in a tenant are still only *admitted*, they cannot read anyone's messages; and the admin can rotate the key by the operator re-registering a new public key (an operator action, logged).
- **A malicious operator** can enrol its own devices or drop tenants. The design does not defend against the operator for availability; end-to-end encryption still protects content.
- Tokens must never go in URLs or logs.

## Alternatives considered
- **Accounts with passwords or email:** more to protect, more to leak, and it identifies people. Rejected.
- **A shared secret per tenant:** simple, but one leak admits anyone and cannot be revoked per device. Rejected.
- **Enrolment through the existing owner device list** (the owner key signs that a device belongs to a person): that proves device-to-person, which is the opposite of what a tenant should learn. Kept separate on purpose.

## Decisions I need from you
1. **Open question: should a tenant-enrolled device be able to talk to devices outside the tenant?** Proposed yes (messaging is not restricted to the organisation); a stricter "closed organisation" mode would be a later option.
2. **Redeem window and revocation behaviour:** is 7 days to redeem and "revocation deletes the queue" what you want?
3. **Who holds the admin key in the first customers:** the customer (proposed), or Spike on their behalf at first?
4. **Operator key custody:** a single key on the host is simple but weak for a service; a hardware key or a two-person rule is stronger and more work.
5. Plan decisions 1 to 7 (customers, legal entity, payments, pricing, hosting, service level, licence) still gate slice 2; this ADR does not depend on them.

## Built (2026-11-15)

The owner's answers to the questions above are the proposals as written: members may talk to anyone (1);
7 days to redeem and revocation deletes the device's queue (2); the customer holds the admin key (3); a
single operator key whose public half is given to the relay at start (4).

**Code.** Token, revocation, limits and operator-request formats: `core/src/enroll.rs` (`spike-enroll-v1`,
`spike-enroll-revoke-v1`, `spike-relay-admin-v1`, length-prefixed fields; a token prints as one
`spike-enroll1:` line and its `Debug` hides the signature and id). Relay: `relay/src/tenancy.rs`
(admission, limits, operator requests), schema version 5 in `relay/src/store.rs` (`tenants`,
`enrollments`, `revocations`; upgraded with the ADR 0022 backup), `relay/src/admin_client.rs` and the
`spike-relay-admin` binary. Client: `ClientMsg::Auth` carries an optional `enroll` token;
`TransportOpts::enroll` holds tokens by relay, so a token is only ever presented to the relay it was
stored for; `Home::save_enrollments` keeps them sealed (`enrollments.enc`); `spike join`, `leave`,
`enrollments`, and for the customer's admin `spike tenant-keygen`, `tenant-token`, `tenant-revoke`.
The desktop app joins in Settings, Home relay (after the design's "Joining an organisation's relay"
screen): it shows this computer's full device id to send to the admin, takes the token and relay
address, shows what joining means, then enrolls and makes that relay home; nothing changes if the relay
refuses (`Session::preview_join`, `join_relay`, `leave_relay` in `app`; `app/tests/join.rs`;
`desktop/drive_join.mjs` in a real window). The token does not carry the organisation's name or the
relay's address, so the screen shows the tenant id and asks for the address. Android has the same step
on its Home relay page (`android/.../Organisation.kt`, through the bridge commands `preview_join`,
`join_relay`, `leave_relay`; `android/drive_join.mjs` on the emulator), with a button to scan the token
from a QR code; nothing makes such a code yet, so scanning is untested.

**Relay flags.** `--require-enrollment` switches enrollment on (open mode stays the default, and tokens
are ignored there); `--enroll-no-visitors` makes a closed relay; `--enroll-max-token-days` (7);
`--operator-key <hex>` enables `POST /admin` on the `--metrics-listen` address.

**Refusals** a client can act on: `not_enrolled`, `enroll_invalid` (bad signature, another device's
token, unknown tenant, or valid for longer than the relay allows), `enroll_expired`, `enroll_revoked`,
`enroll_conflict` (already in another tenant), `tenant_suspended`, and `tenant_over_limit` whose message
starts with the limit's name (`devices`, `queue_depth`, `attachment_bytes`, `connections`, `req_rate`).
The CLI turns each into a sentence.

**Changes agreed while building, where this ADR was silent** (raised by the builder and **approved by
the owner on 2026-11-15**; they are part of this decision):
- **Visitors.** A sender delivers through the *recipient's* home relay, so "only enrolled devices may
  sign in" would stop anyone outside a tenant from messaging its members, against answer 1. On an
  enrolled relay an unenrolled device therefore signs in as a **visitor** that may only reach members:
  send to an enrolled device, claim its prekeys and key packages, fetch device lists, read attachment
  chunks, use existing group mailboxes it holds a key for, hand in a revocation, ping, and acknowledge
  anything already queued for it. It may not publish keys or lists, upload, create mailboxes, register
  a push endpoint or ask for TURN credentials, and it may not send to devices that are not enrolled.
  Visitors keep the per-address and per-connection limits of ADR 0022 (so a visitor can claim at most
  `max_claims_per_conn` prekeys or key packages per connection), and a visitor's send counts against the
  recipient tenant's queue-depth limit. Visitors are allowed by default; `--enroll-no-visitors` turns
  them off (strict mode).
- **Revocations** are a new frame (`Revoke`) that any signed-in connection may hand in; the admin key's
  signature is what authorises it. A revocation is kept, so the token that admitted the device cannot
  admit it again; re-admitting a revoked device id is not possible in this slice.
- **Operator requests** are `POST /admin` on the operator listener: JSON `{body, sig}` where `body` is
  the exact signed text, holding the operation, `issued_at` (within 5 minutes of the relay's clock) and
  a random nonce the relay refuses to see twice. Operations: create tenant, set limits, set admin key
  (rotation), suspend, resume, delete, usage. Usage is counts and bytes per tenant (devices, live
  connections, queued messages and bytes, attachment bytes, refusals since start), never device ids.
  `/metrics` stays tenant-free.
- **Already enrolled devices** may present their token again (ignored, even when expired); a token for
  another tenant is refused. Suspending, deleting or revoking closes the affected live connections.
- **Delete** removes the tenant, its enrollments and revocations, and for each of its devices: queued
  messages, attachment chunks, prekeys, key packages, push endpoints and the group mailboxes they
  created. Signed device lists stay (they are public and belong to owners, not devices).
- **Request rate** is one token bucket per tenant shared by all its connections (burst four seconds'
  worth, at least 50), on top of the per-connection bucket of ADR 0022. Pings are free.

**Tested** in `cli/tests/enrollment.rs`: enrolled device admitted and stays enrolled without the token;
unenrolled, expired, too-long, wrong-tenant, unknown-tenant, conflicting, borrowed, rewritten and garbage
tokens refused; each limit (devices, queue depth, attachment bytes across devices, connections and their
release, shared request rate with pings exempt); the visitor boundary (each storage action refused,
reaching actions not refused, a visitor's sends counted against the recipient tenant's queue depth,
strict mode); suspend closes connections and keeps data, resume
restores; delete removes queued ciphertext and chunks; revocation needs the admin key, closes the
connection and blocks the old token; visitors reach members but store nothing; open mode unchanged and
ignores tokens; operator requests need the operator key, cannot be replayed or be stale; the join step's
storage is sealed and per relay. Format tests in `core/src/enroll.rs`.

**Not done.** Nothing shows a token as a QR code (the admin's tools print text), so scanning on Android
is untested and the desktop has no scan. No independent review. The
operator listener is plain HTTP like `/metrics`: keep it on loopback or a private network. The operator
key is a file on the operator's machine (no hardware key, no two-person rule). Tenant usage numbers live
in memory and in the database, not in Prometheus. No per-tenant storage limit on group mailboxes beyond
the existing per-device ones.

## Not decided here
Billing and sign-up (slice 2), high availability (slice 3), and the licence of the relay (slice 4).
