# ADR 0035: Organisations: the console, enrolment and rules

Status: **Accepted by the owner, 2026-11-14** (all five decisions as recommended; see "Decisions").
Date: 2026-11-14

Builds on ADR 0023 (tenants and enrollment tokens, itself still proposed), ADR 0022 (content-free counters),
ADR 0009 (home relays), ADR 0032 (operator kinds) and ADR 0034 (licensing). It covers the four prototype screens
"Organisation console: overview", "Organisation console: devices and an enrolment token", "Joining an organisation's
relay" and "Organisation rules".

## Problem

An organisation wants its people's devices on its relay (hosted by us or run by itself), wants to admit and remove
devices, see that the service is healthy and within its limits, and set a few rules for the app on those devices.
The owner's line is fixed: a relay, its operator and the organisation **never see message content or who talks to
whom** beyond what the ADRs already allow. The prototype says it in its own words: "An admin sees devices and
counters, never people or content."

## Who is who

| Role | Holds | Example |
|---|---|---|
| **Relay operator** | The machine and the database; the operator key (ADR 0023) | Us for hosted relays; the organisation itself when it self-hosts |
| **Organisation admin** | The tenant's **admin key** (Ed25519), on their desktop, sealed like the other keys (ADR 0003) | "AM" at Northwind in the prototype |
| **Member device** | Its own device key; the admin's *public* key, pinned when it joined | A Northwind employee's phone |

The console is a screen of the **desktop app**, shown only when the app holds an admin key. It is not a web page and
not a separate product. The admin's own messaging identity has nothing to do with the admin key: being an admin
does not make you a member, and being a member does not let you see the console.

## What the organisation admin can see and do

**Can see** (all of it is already on the relay; the console only shows the tenant's own slice):

- The **device ids** enrolled in the tenant, each with the **day** it enrolled.
- An **activity bucket** per device: *active this week*, *idle 7 to 30 days*, *idle more than 30 days*. Not a
  "last seen" time (see "What changes from the prototype").
- **Tenant totals**: devices enrolled against the limit; bytes stored for offline devices against the limit;
  (later) group-call minutes against the plan, once the SFU exists (ADR 0032).
- **Relay health**: up or down, uptime, certificate expiry. Not other tenants, not per-connection data.
- The **rules** it published and the version each device last fetched (so the admin can tell a phone is on old rules).

**Can do**, each a request signed by the admin key:

- **Issue an enrolment token** (below). Issuing is local: the desktop signs it; the relay learns of a token only
  when a device redeems it.
- **Revoke a device.** The relay refuses it from then on and deletes its queue (ADR 0023 decision 2).
- **Publish rules** (below).
- **Read usage** for the tenant.

**Cannot see**, by construction rather than by policy:

- Message content, attachments, call media (end-to-end encryption, unchanged).
- Who a device talks to, who its contacts are, which groups it is in. The relay sees sealed envelopes addressed to
  device ids (ADR 0001); the console is given **no per-device traffic numbers** (no messages sent or received, no
  bytes, no queue depth per device), only tenant totals. Per-device counts would let an admin who knows whose phone
  is whose watch one person's activity.
- Names, phone numbers, email addresses. Nothing in enrolment carries them.
- IP addresses or exact connection times.

**Cannot do**: read or wipe a phone, force a backup to the organisation, add itself to a conversation, enrol a device
without a person on that device accepting, or change what the relay operator can see.

**Honest limit.** The admin hands each token to a person, so the admin can keep their own list "token 3 went to
Alice" and from it learn "this device id is Alice's phone". The relay never learns this; the admin does, by their
own bookkeeping. That is why per-device numbers stay out of the console: with that mapping, a device's activity is a
person's activity. The console keeps an optional **label** per token ("Alice's phone") **on the admin's desktop
only**, never sent to the relay, because admins will keep that list anyway and it is better that it is not on a
shared server.

## What the relay operator can see and do

Unchanged from ADR 0023, with the tenant now visible: the operator sees device id -> tenant, the tenant's totals
and the content-free metrics of ADR 0022. It can create, suspend, resume and delete tenants and set limits, and it
can re-register a tenant's admin public key (key rotation or a lost key). It sees the tenant's rules, because they
are not secret (they are shown to every member). It cannot forge rules or tokens (it lacks the admin key) and a
member device refuses rules not signed by the key it pinned. A malicious operator can still enrol its own devices
or drop a tenant: availability, never content (ADR 0023 accepts this).

For the first version the operator works with **commands run on the relay machine** (`spike-relay tenant create`,
`limits`, `suspend`, `resume`, `delete`, `rotate-admin`) that act on the database directly. ADR 0023's signed
operator requests over the network are for the hosted service at scale and wait for its key-custody decision.

## Enrolment tokens

### What changes from ADR 0023

ADR 0023 bound a token to a device id chosen in advance, valid 7 days. The prototype instead shows the admin making
a QR code first ("Valid for 24 hours", "Uses 1 device") that the person then scans. A device-bound token needs the
person to send their device id to the admin first, a second round trip that people will get wrong. **Proposed:
the prototype's flow**, a single-use token not bound to a device:

```
token = {
  v: 1, tenant, token_id (16 random bytes), not_after (unix seconds, default now + 24 h),
  relay (wss URL), org_name (display only), admin_pub
} signed by admin key under "spike-enroll-v2"
```

- **Single use.** The relay records `token_id -> device` on first redemption, in the same transaction that enrols the
  device. A second device presenting it gets `token_used`. The same device presenting it again is harmless.
- **Short.** 24 hours by default; the admin may choose 1 hour to 7 days.
- **What a leak costs.** Whoever redeems first is admitted to the tenant's *relay*, which reads no one's messages.
  The intended person sees "This token was already used", the admin sees an unexpected device and revokes it.
- **Withdraw before use.** The admin can cancel an unused token; the relay then keeps its id in `revocations` until
  its `not_after` passes.
- **Never in URLs or logs** (ADR 0023). The QR holds the token bytes, not a link. The relay logs neither.
- **The phone pins `admin_pub`** from the token it accepted and trusts rules only from that key. A token whose
  signature does not check against its own `admin_pub`, or whose `admin_pub` is not the tenant's key on that relay,
  is refused.

### Joining from the phone (and desktop)

1. Scan or paste the token (Settings, Home relay, "Join an organisation").
2. The app shows the prototype's sheet: organisation name and relay, "Enrols: this phone only", the expiry, "Northwind
   can see: that this device exists", "Northwind cannot see: your name, contacts, messages". Name and relay come from
   the signed token; the sheet says the name is the one the organisation chose.
3. **Use this relay** connects, signs in with the token (a new optional field on `Auth`, so redemption is one
   round trip), fetches the rules, and only then switches the home relay through the existing `set_relay` path,
   which republishes the device list (ADR 0009) so contacts deliver there.
4. Failures in words: expired, already used, withdrawn, organisation full (`tenant_over_limit: devices`),
   organisation suspended, relay unreachable.

**Leaving** ("Leave Northwind's relay"): the device sends a signed `LeaveTenant`, the relay removes the enrolment
and its queue, the app drops the rules and the pinned key and asks the person which relay to use next (nothing is
deleted from the phone). A revoked device gets the same screen, worded as "Northwind removed this device".

## Organisation rules

### What a rule is

A signed document per tenant:

```
rules = { v: 1, tenant, version (counter, only goes up), issued_at,
          lock_idle_minutes?, keep_messages_days?, backups?: "allowed" | "key_required" | "off",
          agents?: "any" | "verified_only" | "off" } signed by admin key under "spike-rules-v1"
```

A missing field means "no rule". These four are what the prototype shows; new kinds of rule need a new ADR, because
each is a power the organisation gets over a person's phone.

### How rules reach devices

- The admin's desktop signs the document and sends it in `PublishRules`; the relay checks the signature against the
  tenant's admin key, refuses a version that is not higher than the stored one, and stores it (one row per tenant).
- A member device fetches it after each sign-in (`FetchRules`) and is sent `RulesChanged` when the admin publishes
  while it is connected (the existing live-connection path, ADR 0020).
- The device **verifies the signature against its pinned admin key** and **refuses a lower version** than the one it
  holds, so neither the relay nor an attacker on the network can roll rules back or invent them. A relay can only
  withhold a new version, which the console makes visible ("devices on version 3 of 4").
- The relay reports to the admin, per device, only the rules version it last fetched. That is one more per-device
  number; it says nothing about traffic. **Open question 4.**

### Enforced by the app, and said so

Rules are carried out by the Spike app on the member's own device, as the prototype says. A person who installs a
modified app can ignore them; rules are a promise between an organisation and its people's honest apps, not a
security boundary against the person. What each rule does:

| Rule | On the device |
|---|---|
| Lock when idle | The app locks after N minutes (the existing lock and unlock, ADR 0003 and the Android biometric unlock). |
| Messages kept on this phone | The app deletes messages older than N days from the local store (ADR 0007), in **every** conversation on that device. See open question 3. |
| Backups | "Off" hides backup; "key required" allows only passphrase-protected backups (ADR 0029 already requires a key, so this rule is mostly a statement). The organisation never receives the backup. |
| Agents | "Verified only" allows only agents whose identity is verified (agent directory); "off" refuses agent requests (ADR 0014). |

The rules screen lists them, says they are enforced by the app, and says what the organisation cannot see.

## Storage and wire

- Relay tables (schema version 5): `tenants(id, name, admin_pub, max_devices, storage_bytes, state, created,
  rules_version, rules)`, `enrollments(device, tenant, token_id, enrolled_day, active_week, rules_version)`,
  `token_uses(token_id, tenant, device, not_after)` (device empty = withdrawn; rows go when the token expires) and
  `revoked_devices(device, tenant)`. `active_week` holds only the week a device last signed in, which is what the
  bucket needs and no more.
- Relay modes from ADR 0023: open (today) and enrolled (`--enrolled`).
- **Visitors (found while building, follows from decision 1).** A contact delivers to a member by signing in to the
  member's home relay (ADR 0009). If enrolled mode refused every unenrolled device, nobody outside the organisation
  could reach its members. So an enrolled relay lets an unenrolled device sign in as a **visitor** (`Ready.visitor`):
  it may send, claim prekeys and key packages, fetch device lists, read attachment chunks, and read and append to
  group mailboxes; it may not publish keys or device lists, upload attachments, create group mailboxes, register a
  push endpoint, ask for TURN credentials or receive anything (it is not registered, so nothing is delivered to it).
  An enrolled relay also refuses to *queue* a message for anyone who is not a member (`not_enrolled`), so visitors
  cannot use it as free storage for each other. A visitor costs the organisation only its members' own queues,
  which are capped as before. A revoked device becomes a visitor and learns from `FetchRules` that it was removed.
- Wire: `Auth` gets an optional `enrol` token; new `AdminAuth` (the admin key answers the challenge instead of a
  device key; such a connection may only make admin requests), `ListDevices`, `RevokeDevice`, `CancelToken`,
  `PublishRules`, `TenantUsage`, `FetchRules`, `LeaveTenant`; server `Devices`, `Rules`, `RulesChanged`, `Usage`,
  and errors `token_expired`, `token_used`, `token_withdrawn`, `not_enrolled`, `tenant_suspended`,
  `tenant_over_limit`.
- Core: token and rules formats, signing and verification, with domain separation, in `core/src/tenant.rs`.

## What changes from the prototype

The prototype is a direction, drawn before this design. Where it would show more than the line above allows, this
ADR shows less:

1. **"Last seen: now / 2 h ago / yesterday"** becomes the activity bucket ("active this week", "31 days idle" stays
   as "idle 30+ days"). Exact last-seen times are presence, and with the admin's own token list they are a
   person's presence.
2. **"Delivered within target 99.94%"** needs per-message delivery timing the relay does not keep. Left out until
   there is a content-free way to measure it (an aggregate histogram in the metrics of ADR 0022 is likely), and
   shown as "not measured yet" rather than a made-up number.
3. **"Group call minutes"** and **"Plan and usage"** depend on the SFU (ADR 0032) and on pricing (hosted plan
   decisions 3 and 4). The nav item is present but says what is not available yet.
4. **"Raise limit"** on a hosted relay is a request to the operator (us), not something the admin can do; on a
   self-hosted relay it is the operator command. The button opens that explanation.

## Alternatives considered

- **Device-bound tokens (ADR 0023 as written).** Leak-proof but needs the person to send a device id to the admin
  first. Kept as an option for admins who want it ("Enrol a known device id").
- **Rules carried inside messages** from an admin "account" to each member: hides rules from the relay, but rules are
  not secret, and it would make the organisation a contact of every member, which reveals more. Rejected.
- **Rules enforced by the relay** (for example the relay deleting old messages): the relay holds no messages beyond
  the offline queue; it cannot enforce anything on a phone. Rejected.
- **A web console run by the hosted service.** ADR 0034 keeps hosted operations unpublished, but the admin key must
  stay with the customer; a web page would mean either we hold it or the browser does. The desktop app keeps it in
  the sealed store. The *hosted* billing and plan pages may still be a web page later.

## Decisions for the owner

1. **Approve ADR 0023** with its four questions answered as: (a) enrolled devices may talk to anyone, (b) revocation
   deletes the queue, (c) the customer holds the admin key, (d) operator key: commands on the relay machine for now,
   signed operator requests later.
2. **Token flow:** single-use, unbound, 24 hours by default (the prototype), with device-bound as an option; or
   device-bound only (ADR 0023 as written).
3. **"Messages kept on this phone"** applies to all conversations on an enrolled device (simple, and what an
   organisation expects), or only to conversations with other members of the organisation (gentler, but the app
   then has to know who is a member, which the design otherwise avoids).
4. **Per-device data in the console:** the activity bucket and the rules version only (recommended), or also an
   exact last-seen time as drawn.
5. **Where this code lives** given ADR 0034: tenancy and enrolment in the open relay and clients (ADR 0023 slice 4,
   recommended), with the console and rules in the open desktop and Android apps too; or the console and rules as an
   unpublished "enterprise" layer.

## Decisions (2026-11-14)

The owner took every recommendation:

1. ADR 0023 approved: enrolled devices may talk to anyone; revocation deletes the queue; the customer holds the
   admin key; the operator uses commands on the relay machine for now.
2. Tokens are single-use, not bound to a device, 24 hours by default (the prototype's flow).
3. "Messages kept on this phone" applies to every conversation on an enrolled device.
4. The console shows per device only the activity bucket and the rules version fetched; no last-seen time.
5. Tenancy, enrolment, the console and rules are in the open relay and the open desktop and Android apps.

## Built (2026-11-14)

- **Engine** (`core/src/tenant.rs`): token and rules formats, signed over exact JSON bytes with their own contexts
  (`spike-enroll-v2`, `spike-rules-v1`), the admin sign-in (`spike-admin-auth-v1`, bound to tenant and relay name),
  version-only-goes-up acceptance; `DeviceList::with_relay` for moving a device's home relay. Wire messages in
  `core/src/wire.rs`.
- **Relay** (`relay/src/org.rs`, `relay/src/store_tenants.rs`): schema version 5, redemption in one transaction, the
  sign-in gate, visitors, per-tenant device and storage limits (`tenant_over_limit`), the admin connection,
  rules pushed to connected members (`RulesChanged`), leaving and revoking (queue dropped), and the operator's
  `spike-relay tenant create | list | limits | suspend | resume | delete | rotate-admin` acting on the database.
  Twelve new error codes are counted in the metrics; none carries an id.
- **Client** (`cli/src/org.rs`): join (enrol, take rules, re-sign the device list with the new home relay and publish
  it on both relays), refresh and carry out rules, leave, and the admin side (key, tokens with local labels, the
  admin connection, publish rules, withdraw tokens).
- **App** (`app/src/org.rs`): the same for both apps. Rules are fetched with every sync; "backups off" refuses an
  export; "agents off" refuses grants and approvals; "verified only" refuses a grant to an unverified contact;
  "messages kept" deletes older messages in every conversation (their attachment files are not deleted yet; see
  limits).
- **Desktop** (`desktop/ui/org.js`): the console as its own section of the rail, shown only with an admin key
  (Overview, Devices with the enrolment token and QR, Enrolment, Limits with the rules editor, Plan and usage,
  Admin keys); Settings, Organisation console to make the key; Settings, Home relay, Join (the sheet from the
  design) and the rules page with Leave. The lock rule caps auto-lock.
- **Android** (`Org.kt`): Home relay, Join (paste or scan), the join sheet, the rules page with Leave. The lock rule
  locks the app after it has been in the background for the rule's time or the app's own 2 minutes, whichever is
  shorter, **even when alerts are on** (the organisation chose it; alerts stop while locked).

**Tested:** `core` (9 unit tests: tokens, tampering, lifetimes, rules signatures, rollback, admin sign-in
binding), `relay` store (9: single use, wrong key, unknown and full tenant, withdrawn, suspended, revoke drops the
queue, rules only forward, storage limit, leave and delete, activity buckets), `cli/tests/orgs.rs` (7 end to end on
real relays: visitor and single-use, full and withdrawn and forged tokens, rules forward-only and forged rules
refused, retention, revoke in open and enrolled mode, a contact on another relay reaching a member and leaving
moving back, the admin interface closed to devices and to other keys), and `desktop/drive_org.mjs` (three real
windows: the console, a member joining by the sheet, a contact on another relay, rules, revoke, leave; all checks
pass). `android/drive_org.mjs` is written; on its one run (2026-11-14) it passed up to opening the join page, then
the emulator was needed by another session, so the phone's join, rules and leave steps are **not yet verified on the
emulator** (the Android app builds).

**Limits (honest list).**
- Rules are carried out by the app; a modified app ignores them.
- The relay can withhold new rules (it cannot forge or roll them back); the console shows which version each device
  fetched.
- "Messages kept" deletes message rows but not yet the attachment files those messages pointed to (sealed on disk,
  unreachable from the screen). Deleting them too is a follow-up.
- One admin key per tenant. A rotated key: devices keep trusting the key they pinned and refuse rules from the new
  one until they join again. Re-pinning is a later piece of work.
- Signed operator requests over the network, billing, call minutes and a delivery-time figure are not built.

## Not decided here

Billing, sign-up and the hosted plan pages; several admins per tenant (one admin key for now; rotation through the
operator); single sign-on; audit logs of admin actions beyond the relay's own; iOS.
