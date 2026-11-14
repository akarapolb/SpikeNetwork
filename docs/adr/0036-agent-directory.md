# ADR 0036: Agent directory

Status: **Accepted** (owner chose option 1, 2026-11-14). Built on the phone; desktop to follow.
Date: 2026-11-14

## Problem
Today an agent becomes a contact the way a person does: someone hands you its identity card, you add it, and then you give it a grant (ADR 0014). The UI prototype adds "Find an agent: verified agents you can allow", with each entry marked *identity verified* or *identity not verified*. Two things are undefined: **where the list comes from**, and **who says an agent is "verified"**, and what that word promises.

## What "verified" can honestly mean
Spike already has one meaning of verified: *you compared the safety number with the person*, so you know this owner key is the one you meant. For an agent there is nobody to compare numbers with, so "verified" must be **someone else's signed statement**: "the owner key `K` is operated by maker `M` (a legal entity we checked)". It must never mean "this agent is safe" or "this agent behaves well": a grant's limits, typed actions and receipts are what protect you, and the directory does not change them. The entry's scopes are what the maker *says* it needs; the grant is still the person's choice and starts empty.

## Decision (proposed)

### Listings are signed files, not a server
A **listing** is a small signed object: `{agent owner key, identity card, name, maker, one-line description, scopes it asks for, issued, expires}` signed by a **lister key** (Ed25519, domain-separated `spike-agent-listing-v1`, same primitives as grants). A **directory** is a set of listings fetched over HTTPS or handed over as a file; transport is untrusted, the signatures are what count. Bad signature, expired, or a card that does not match the owner key: the listing is dropped.

### Who is trusted to vouch (the owner decision)
The app shows **"Identity verified"** only when a listing is signed by a lister key **the person (or their organisation, ADR 0023) chose to trust**. A listing signed by the agent's own key is shown as **"Identity not verified · listed by its maker only"**. Options for which listers exist by default:

1. **No built-in lister (chosen).** The app ships trusting nobody. A person adds a lister by its key (or an organisation's enrollment brings its admin key as the lister: "Agents: verified only"). Spike runs no vetting service, takes on no liability for vetting makers, and nothing about agents goes through a central party. The directory is empty until a lister is added; its empty state explains this.
2. **Spike runs a lister**, and the app trusts it by default (removable). Gives a useful list on day one, but makes Spike a certificate authority for agents: someone must check makers' identities, handle disputes and revocation, and the key becomes a high-value target.
3. **No vouching at all**: the directory only shows agents your verified contacts shared with you. Simplest, but "identity verified" then means only "a friend sent it".

### Using an entry
"Set limits" adds the agent as a contact from the listing's identity card (marked as an agent) and opens the grant form, empty. An agent already allowed shows "Allowed". An unverified entry shows "Review first", which opens the same screen with the warning above the form. Nothing is granted by the directory itself.

### Revocation
Listings expire (at most 90 days) and are re-fetched; a lister can publish a signed revocation of an owner key. Revoking a listing does **not** revoke a grant the person already gave; the app marks that agent "no longer listed" and the person decides.

## Built (2026-11-14)
- `core/src/listing.rs`: `Listing` sign/verify (`spike-agent-listing-v1`), card must carry the agent's key, at most 90 days, not dated in the future; tests for tampering, expiry, wrong card, wrong signer, self-listing.
- `cli/src/directory.rs`: trusted listers and imported listings, both in the sealed state (`agent-listers.enc`, `agent-listings.enc`); import checks each listing on its own and keeps the newest per lister and agent; at most 500 listings and 32 listers. An agent counts as verified when any in-date listing for it is signed by a trusted lister that is not the agent itself.
- `spike agent-listing --name --maker [--about] [--scope ...] [--card <agent card>] [--days]` signs a listing (for this identity, or as a lister for another agent) and prints it as JSON.
- Bridge: `directory`, `add_lister`, `remove_lister`, `import_listings`, `add_listed` (adds the agent as a contact from its listing; grants nothing).
- Phone: Agents tab, "Give an agent a grant", "Find an agent" (search, cards with Set limits / Allowed / Review first), the review screen for an unverified agent, and "Agent directory" (listers you trust, paste listings). Driven by `android/drive_agents.mjs`.

Not built yet: fetching a directory over HTTPS (today listings are pasted or imported), lister revocations, the "no longer listed" mark, an organisation's enrollment bringing its lister key, and the desktop screen.

## Not decided here
Search ranking, categories, paid placement (none), and how a lister checks a maker's identity (that is the lister's own published policy).
