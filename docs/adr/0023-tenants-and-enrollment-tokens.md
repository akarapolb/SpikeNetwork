# ADR 0023: Tenants and enrollment tokens

Status: **Proposed. For your review; not built.**
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

## Not decided here
Billing and sign-up (slice 2), high availability (slice 3), and the licence of the relay (slice 4).
