# Multi-tenant hosted relay: scope and plan

**Status: a draft plan for your decisions, not agreed and not built.** You chose "multi-tenant hosted service" from `docs/hosting-options.md` (roadmap Phase 4: hosted relays, billing, org accounts). That is a product build, not a feature, and most of it depends on choices that are yours (legal entity, payment provider, pricing, who the customers are). This note breaks it into slices, says what each needs, and marks what is decided and what is not. Sizes are relative (S, M, L), not dates.

## What stays true
A hosted relay is still a **content-blind pipe**: it never sees message text, files or contacts, only metadata. Nothing below changes the end-to-end cryptography. The design goal for accounts is to give the relay the **least identifying information that still lets it enforce limits and bill**: it should know "this device belongs to tenant T", not who anyone is.

## Slices, in order

### Slice 0: make one relay safe to share (M). Needed before any tenant
**Built (ADR 0022): per-address limits, per-connection request rate, trusted-proxy address handling, database work off the async threads, schema migrations with backup, `/healthz` and content-free `/metrics`.** The list below is what it was asked to cover. Slice 1 is designed in ADR 0023 for your review.
Engineering the single-node relay already needs (the session 1 and ADR 0004 gap lists):
- Per-IP connection and rate limits, plus per-device rate limits (the relay cannot see senders, so limits attach to the connection and to the recipient queue).
- Move SQLite calls off the async threads (`spawn_blocking` or a pool); schema migrations; startup checks.
- Metrics that never include content: connections, queue depth, bytes stored, rejects by reason. A health endpoint.
- Done already: keepalive and dead-connection cleanup (ADR 0020), a cap on live connections per device, TLS, durable storage.

### Slice 1: tenants without accounts (M). The core of "hosted"
A **tenant** is an organisation with its own limits. Use the keys the product already has instead of passwords:
- A tenant has an **admin key** (an owner-style Ed25519 key, held by the customer). At provisioning the relay operator registers its public half with the relay.
- A device is **enrolled** with a short-lived, signed **enrollment token**: the admin key signs "device X may use this relay until T". The relay verifies the signature, stores the device-to-tenant mapping, and refuses unenrolled devices (this also closes the "anyone can use quota" gap for hosted use).
- **Per-tenant limits:** devices, queue depth per device, attachment bytes, connections, request rate. Counters per tenant; over-limit is refused with a clear error.
- **Suspend and delete a tenant** (the operator can switch one off for non-payment or abuse; its queued ciphertext is deleted).
- A small **operator admin interface** (signed requests, not a public web page): create tenant, set limits, suspend, read usage numbers. No message content exists to read.
- The app gets a "join this organisation's relay" step that takes an enrollment token (QR or paste) and stores it sealed with the other settings.

Privacy rule: the relay learns *device id to tenant*. It must not learn names, emails or identity cards. Enrollment tokens are about devices, never people.

### Slice 2: accounts and billing (L). Needs your decisions first
- Customer sign-up and a place to buy: a payment provider (checkout, subscriptions, invoices, tax), which creates a tenant and shows the admin key flow. **Never hold card data ourselves.**
- Metering from slice 1's counters (devices, stored bytes) feeding the provider.
- A customer-facing admin console (add devices, issue and revoke enrollment tokens, see usage).
- Terms of service, privacy policy, a data-processing agreement, and a support channel. These are legal documents, not code.

### Slice 3: operate it (L)
- High availability: today one node and one SQLite file. Options are a managed database, replicated SQLite, or regional single nodes with clear failover; each has a different cost and failure story.
- Backups, status page, alerting, on-call, incident response (the roadmap asks for tested runbooks), and a tested key-recovery and certificate-renewal routine.
- Abuse handling and a **legal-requests process** (business plan 8a), decided before the first customer, not after.
- The independent security review (roadmap gate) before public use.

### Slice 4: self-host parity and the SDK (M)
Self-hosters run the same relay binary with tenancy off (or one tenant). The enrollment and limits code ships in the open relay, so hosted and self-hosted behave the same. This is also where licensing matters: the relay's licence decides whether others can resell it.

## Decisions I need from you (in the order they unblock work)
1. **Who are the customers?** Organisations only (recommended), or individuals too. Individuals need self-serve support and abuse handling at a very different scale.
2. **Legal entity and jurisdiction** that will operate the service, and your position on lawful-access requests (business plan 8a). This must exist before slice 2.
3. **Payment provider and tax setup** (who is the seller of record, which countries).
4. **Pricing and quotas.** The paper's price points are hypotheses; the quotas in slice 1 (devices, queue, storage) are what the price pays for. Attachments are the real cost driver (bandwidth and storage), so decide the storage and retention you will sell.
5. **Hosting provider and regions** (affects data-residency promises and which authorities can compel what).
6. **Service level:** is a single node acceptable for the first design partners (recommended), and what do you promise about uptime and message retention (today: undelivered messages are kept up to 7 days).
7. **Licence of the relay** (open or not, and which licence): this decides slice 4.

## What I would do next, without waiting for slice 2's decisions
Slice 0 and slice 1 are mostly engineering on code that already exists and need none of the decisions above. If you agree, I would start with slice 0 (rate limits, `spawn_blocking`, migrations, metrics), then the tenant and enrollment design as an ADR for your review before building it.

## Risks to keep in view
- **Metadata liability grows with every tenant.** The operator becomes a holder of who-talks-when at scale. Minimise what is logged and kept; write the retention rules down.
- **Enrollment must not become tracking.** Tokens identify devices to a tenant and nothing more; expiry and revocation should be routine.
- **Cost surprises come from attachments**, not messages. Quotas and expiry are product features.
- **A single operator is a single point of failure and compulsion.** The self-host path (slice 4) is the answer to customers who cannot accept that.
