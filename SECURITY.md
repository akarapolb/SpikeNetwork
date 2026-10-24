# Security policy

**Status: unreviewed prototype.** Spike Connect has had no independent security review. Do not use it for
messages whose exposure would harm anyone. [`docs/SECURITY-STATUS.md`](docs/SECURITY-STATUS.md) says plainly
what has been tested, what has not, and the gates that must pass before real use.

## Reporting a vulnerability

- **Contact: not yet set up.** The project owner must publish a monitored address (and ideally a PGP key or
  GitHub private vulnerability reporting) here before anyone is asked to rely on this policy. Until then,
  report privately to the project owner directly; do not open a public issue for a vulnerability.
- Include: what you found, the version or commit, steps or a proof of concept, and what you think the impact
  is. A failing test is the best report.
- Please do not access other people's data, run load against relays you do not operate, or test social
  engineering against the project's users.

## What to expect (targets, not guarantees, until there is a team)

| Step | Target |
|---|---|
| Acknowledge your report | 3 working days |
| First assessment (accepted / need more / not a vulnerability) | 10 working days |
| Fix or mitigation for a high-severity issue | 30 days |
| Public advisory | After a fix ships, agreed with the reporter; 90 days at the latest unless a fix is genuinely in progress |

Reporters acting in good faith under this policy will not be pursued for the research itself. Credit is given
in the advisory if wanted.

## In scope

The code in this repository: `core/` (cryptography glue, vault, wire formats), `relay/`, `cli/`, `app/`, `mls/`,
`mobile/` (JNI), `android/` (Kotlin app), `desktop/` (Tauri app), and the build and release configuration.

Most useful to us: anything that lets a relay or a contact read, forge, replay or silently drop messages
beyond what [`docs/threat-model.md`](docs/threat-model.md) already admits; anything that defeats the vault, the
Android Keystore-sealed unlock, or device linking; remotely triggerable crashes or unbounded resource use in
the relay or engine; and mistakes in the MLS large-group rules
([`docs/review/large-groups-review-brief.md`](docs/review/large-groups-review-brief.md)).

## Out of scope

- Flaws inside third-party libraries (`vodozemac`, `openmls`, RustCrypto, rustls, SQLite): report them
  upstream; tell us too if we use them wrongly or should upgrade.
- An attacker who already controls an unlocked device or its operating system.
- Traffic analysis the documents already say is open (see [`docs/review/metadata-review.md`](docs/review/metadata-review.md)).
- Denial of service by a well-resourced network attacker against a relay that is not behind a front-line service.

## Supported versions

Only the latest commit on the main branch. There are no releases yet.

## Dependencies and advisories

`cargo deny check` (see `deny.toml`) runs in CI and in a scheduled watch workflow
(`.github/workflows/watch.yml`) so a new RustSec advisory is noticed without a code change. Every accepted
advisory in `deny.toml` carries a reason and a date.
