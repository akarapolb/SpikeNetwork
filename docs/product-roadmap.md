# Product Roadmap

## 1. Product definition [Paper]
**Spike Connect** is the application/service; **Spike Network** is the underlying interoperable infrastructure.
North star: every person, AI agent and trusted device can communicate and coordinate privately, across vendors, without surrendering message content to a central platform.

Launch thesis: agent communication + private messaging, aimed at Spike OS / Box users and design partners. Mass-market consumer chat is a later option, not the launch thesis.

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

## 4. MVP scope (first pilot) [Paper]
**In:** desktop-to-desktop chat; identity + device pairing; E2EE 1:1; encrypted local history; relay fallback + offline queue; agent messages with explicit approval; basic attachments + diagnostics.
**Deferred:** public discovery/large communities; custom crypto; video/livestream; autonomous payments / unbounded agent actions; compliance certification claims.

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

## 6. Proposed refinements
- **[Proposed]** Instrument the gates from Phase 1: define "weekly active", "accepted message", "task completed" precisely before measuring them.
- **[Proposed]** Phase 0 demo = 2 CLI clients + 1 relay (headless core first, UI later).
- **[Proposed]** Keep the data model multi-device and multi-party aware from day one even though groups and sync ship in Phase 3.
- **[Proposed]** Run the 15 user interviews in Phase 0 in parallel with the build; their output can change Phase 2 scope.
- **[Proposed]** Every pre-audit release states plainly that it has not been independently reviewed.

## 7. Kill / pivot criteria [Proposed]
Review at the end of Phase 2 (day 90). Reconsider scope if: fewer than 5 pilot groups engage, WAU is well below 40%, no design partner will discuss paying, or agent tasks need frequent manual rework. Pivot options: narrow to device-to-device (Connect Link) or to an agent-channel SDK without a chat client.
