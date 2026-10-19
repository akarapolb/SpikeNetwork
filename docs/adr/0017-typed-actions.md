# ADR 0017: Typed actions

Status: **Proposed** (implemented)
Date: 2026-10-19

## Problem
Until now the approval prompt showed the action text the agent wrote ("Buy 3 GPU hours"). Policy checked only the scope string and the amount, and so did the verifier. A lying or compromised agent could describe a payment to itself as a payment to a supplier, and nothing would catch it: the human approved words, and the service checked numbers (ADR 0014 limits, threat model).

## Decision
A request may carry **typed fields** for a known action type (`core/src/action.rs`). The type name is the request's scope, so grants keep naming scopes.

- **Registry, fixed in code.** `payments.send {payee, currency}` plus the amount, `files.read|write|delete {path}`, `messages.send {to, subject}`. Each type lists its fields, whether it moves money (required or forbidden), and a one-line template for the human. A new type is a reviewed code change; an agent can never define one. `spike agent-types` lists them.
- **Strict values.** Ids (payees, recipients) are printable ASCII without spaces, so look-alike letters from other scripts cannot impersonate a known payee. Currencies are three upper-case letters. Paths and text may be non-ASCII but must not contain control characters or invisible formatting characters (bidirectional overrides, zero-width characters, BOM, soft hyphen); paths must not contain a `..` component. Missing, extra or invalid fields fail validation, and a request that fails validation cannot be signed or verified.
- **Signed with the request.** `Request.typed` is a `BTreeMap`, so it serializes in key order and the signed bytes are canonical. It is omitted when absent, so free-text requests (and every request signed before this change) verify unchanged.
- **Shown by type.** The approval prompt is rendered from the fields in one pass over the template (so a value that looks like `{currency}` stays literal), with money in the currency's minor unit (`1500` USD is `15.00 USD`, JPY has no decimals, KWD three). The agent's words are still shown, labelled as the agent's note. A free-text request is labelled "free text in the agent's own words, not checked".
- **Grants can require it.** `GrantBody.typed_only` (`agent-grant --typed-only`, omitted from the signed body when false): free-text requests are denied by policy, and every scope in the grant must be a known type.
- **Services check it.** `verifier::Action.fields` (`verify-receipt --field k=v`): the service states the fields of what it is about to do. A typed approval is refused unless the service states fields, and they must equal the approved ones exactly (`WrongField`). A service that states fields refuses a free-text approval (`Untyped`). So a service that knows the type can never be satisfied by an approval whose fields it did not see, and the scope and amount checks are unchanged. `Authorized` now includes the fields and the line the human saw.
- **The audit log is unchanged**: scope and amount only. Payees and paths are content in the ADR 0014 sense and stay out of the log; the request hash in each entry still commits to them.

CLI: `agent-request ... --field payee=acme-gpu --field currency=USD`, `agent-grant --typed-only`, `verify-receipt --field ...`, `agent-types`.

## Alternatives considered
- **Free-form JSON schema per grant.** More flexible, but the human would then approve whatever schema the agent's operator wrote, and the display could not be audited once. A small fixed registry is reviewable.
- **A separate `type` field next to `scope`.** It would let one scope cover several types, but adds a second name for the same permission and a way for the two to disagree.
- **Optional field checks in the verifier** (accept a typed approval when the service gives no fields). Rejected: it would let a service that only checks the amount honour an approval for a different payee. Failing closed is the point.

## Limits (honest list)
- **Only as good as the registry and the service.** A service that does not pass `--field` gets the old checks (scope and amount) for free-text approvals and refuses typed ones; it must state what it is really about to do, not copy the fields from the presentation.
- **Ids are not identities.** `payee=acme-gpu` is whatever string the service maps to an account. Typed fields stop the agent from changing the payee after approval and make it visible to the human; they do not prove that `acme-gpu` is who the human thinks it is. Allowlists of payees or paths in the grant are the natural next step.
- Fields are exact-match only: no ranges, prefixes ("anything under /reports") or wildcards.
- The minor-unit table covers common currencies; any other is shown with two decimals.
- The agent's note is still shown and still unverified; the typed line above it is what was checked.
- Unreviewed code, like everything else here.
