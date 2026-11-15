# Product Roadmap

## 1. Product definition [Paper]
**Spike Connect** is the application/service; **Spike Network** is the underlying interoperable infrastructure.
North star: every person, AI agent and trusted device can communicate and coordinate privately, across vendors, without surrendering message content to a central platform.

Launch thesis: agent communication + private messaging, aimed at Spike OS / Box users and design partners. Mass-market consumer chat is a later option, not the launch thesis.

**[Updated 2026-11-15, see `business-plan.md` sections 1 to 3]** Two lead pitches, both running alongside a customer's existing suite: agent approvals with a signed log, and the continuity channel (an independent channel that keeps working when the main suite is down). **Center Work** is the first named customer of the Spike SDK (owner's decision), so the browser client (ADR 0033) is part of the launch. Personal Plus is deferred; the offers are Agent Network and Business / Enterprise.

## 2. Pillars
| Pillar | Scope |
|---|---|
| Connect Chat | DMs, groups, attachments, presence; read receipts opt-in |
| Connect Agents | Agent identity, discovery, permission scopes, approval flows, signed requests/receipts |
| Connect Link | Trusted device pairing, file transfer, sync |
| Connect Network | Identity, encrypted transport, routing, relays, push, delivery queues |
| Connect Admin | Org provisioning, policies, retention, observability without content access |

## 3. Phases and exit criteria
| Phase | Window | Deliverables | Exit criteria |
|---|---|---|---|
| 0 Feasibility | Days 0–30 | Threat model, protocol decisions, 2-device encrypted chat, identity prototype | Two devices exchange verified encrypted messages; risks documented |
| 1 Alpha | Days 31–60 | Relay fallback, offline queue, encrypted local store, pairing, attachment prototype | Recovers from disconnects; relay never sees plaintext |
| 2 Pilot | Days 61–90 | Spike OS integration, agent-to-agent requests with approvals, telemetry, security testing | 5–10 pilot orgs/groups; validated task completion |
| 3 Private beta | Months 4–6 | Mobile, groups, multi-device sync, backup/recovery, admin basics | Retention + delivery targets met; independent review scheduled |
| 4 Commercial v1 | Months 7–9 | Hosted relays, billing, org accounts, SDK beta, self-host docs | First paying design partners; support runbooks, incident response |
| 5 Scale decision | Months 10–12 | Developer onboarding, hardened agent directory, enterprise pilots, external audit | Expand only if unit economics, reliability, security gates pass |

### 3a. Actual status, 2026-11-15 (day 33)
The windows above are the paper's. The build did not follow them:

| Area | State |
|---|---|
| Phases 0 and 1 deliverables | Done |
| Phase 2: agent requests with approvals | Done (ADR 0014 to 0017). Spike OS integration and telemetry not started; no pilot groups |
| Phase 3: mobile, groups, multi-device sync, backup | Built (Android app, small groups, large MLS groups as experimental, backup and recovery). Admin basics not built |
| Beyond the paper's scope | Desktop app, calls, direct connections, app channels built; group calls in progress |
| Phase 4: hosted relays, billing, org accounts | Relay hardening done (ADR 0022). Tenants and enrollment designed only (ADR 0023). No billing or accounts |
| SDK | In progress: client crate for native and browser (ADR 0033), with Center Work as first customer |
| Exit criteria that need people, not code | None met: no interviews, no pilot groups, no retention or delivery measurements, no independent review scheduled |

**[Decided by the owner, 2026-11-15]** No feature freeze: feature work continues in parallel with validation and the review.

**Next, in order:** start the 15 interviews; book the independent review; build relay enrollment (ADR 0023) so a pilot relay can face the internet; check and, if missing, build client failover to a second relay (the continuity pitch needs it); recruit pilot groups.

## 4. MVP scope (first pilot) [Paper]
**In:** desktop-to-desktop chat; identity + device pairing; E2EE 1:1; encrypted local history; relay fallback + offline queue; agent messages with explicit approval; basic attachments + diagnostics.
**Deferred:** public discovery/large communities; custom crypto; video/livestream; autonomous payments / unbounded agent actions; compliance certification claims.

**[Updated 2026-11-15]** Video calls and large groups were deferred above but have since been built. Because there is no feature freeze, the pilot scope is set by review, not by what exists: the pilot ships 1:1 chat, small groups, attachments and agent approvals. Calls, group calls, large groups and direct connections stay switched off for pilot users until they have been independently reviewed (SECURITY-STATUS G1 and G10).

## 5. Release gates (targets, not forecasts) [Paper]
| Gate | Target |
|---|---|
| Delivery | ≥99.9% of accepted online messages delivered within SLO in pilot; offline queue measured separately |
| Engagement | ≥40% weekly active of onboarded pilot users by week 8 |
| Business | ≥3 paying design partners before significant scale spend |
| Security | No unresolved criticals before public launch; incident response and key-recovery tested |
| Agent value | ≥70% of pilot agent tasks completed with correct authorization and no manual rework |
| Release integrity [Proposed] | Before public launch: reproducible builds, binary transparency log, threshold release signing; key transparency before scale |
| Legal readiness [Proposed] | Before launch in any country: lawful-access exposure review and a published legal-requests policy |

| Pilot readiness [Proposed, 2026-11-15] | Before any pilot relay faces the internet: relay enrollment (ADR 0023). Before any reliability claim: relay load and soak test (SECURITY-STATUS G3) and client failover between relays |

The detailed security gates G1 to G10 are in `SECURITY-STATUS.md`; none is met as of 2026-11-15.

## 6. Proposed refinements
- **[Proposed]** Instrument the gates from Phase 1: define "weekly active", "accepted message", "task completed" precisely before measuring them.
- **[Proposed]** Phase 0 demo = 2 CLI clients + 1 relay (headless core first, UI later).
- **[Proposed]** Keep the data model multi-device and multi-party aware from day one even though groups and sync ship in Phase 3.
- **[Proposed]** Run the 15 user interviews in Phase 0 in parallel with the build; their output can change Phase 2 scope.
- **[Proposed]** Every pre-audit release states plainly that it has not been independently reviewed.
- **[Proposed, 2026-11-15]** The interviews did not run in Phase 0. Start them now and open with the 12–13 November 2026 Microsoft 365 outage ("what did your team do?"); see `business-plan.md` section 6.
- **[Proposed, 2026-11-15]** Add a "continuity drill" to the demo: relay stopped, messages queue, devices on one network keep talking directly.

## 7. Kill / pivot criteria [Proposed]
Review at the end of Phase 2 (day 90, which is 2027-01-11 from the 2026-10-13 start). Reconsider scope if: fewer than 5 pilot groups engage, WAU is well below 40%, no design partner will discuss paying, or agent tasks need frequent manual rework. Pivot options: narrow to device-to-device (Connect Link) or to an agent-channel SDK without a chat client.
