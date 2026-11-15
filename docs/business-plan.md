# Business Plan

## 1. Strategy [Paper]
Free private messaging; paid hosted reliability, enterprise governance and agent connectivity. Open protocol/SDK, with commercial value in managed operations and Spike integration. Commercial focus is B2B / agent-first. Funding beyond the 90-day capped pilot is gated on retention, reliability and willingness to pay.

**[Proposed, 2026-11-15] Positioning.** Lead with two pitches, both of which run alongside a customer's existing suite and need no one to switch:
1. **Agent approvals with proof:** an agent asks, a person approves on their phone, the auditor gets a signed log, and the operator sees no content. This is the part Element, Wire, Threema Work and Mattermost do not offer.
2. **The continuity channel:** an independent channel on a relay the customer controls, which keeps working when the main suite is down. The Microsoft 365 outage of 12–13 November 2026 (Outlook, Teams and SharePoint, reported in 74 countries) is the reference event. Sell independence of failure, never better uptime: a single-node relay is less reliable than Microsoft until the load and soak gate (SECURITY-STATUS G3) is met.

## 1a. Status against this plan [Proposed, 2026-11-15]
The project started 2026-10-13. Engineering is far ahead of the roadmap (desktop and Android apps, groups, calls, backup, direct connections). The business side has not started: no interviews, no product owner, no budget figure, no trademark clearance. None of the ten security gates in `SECURITY-STATUS.md` is met. **[Decided by the owner, 2026-11-15]** No feature freeze: feature work continues in parallel with validation (section 6) and the independent review. Consequence to manage: every new feature widens what the review must cover, so the pilot ships only reviewed features and the rest stays switched off until reviewed.

## 2. Segments
| Segment | Pain | First offer |
|---|---|---|
| Spike OS / Box users | Secure device + agent communication | Free app + managed relay upgrade |
| SMEs / regulated teams | Data control, secure internal collaboration | Business workspace, self-hosting |
| AI application builders | Reliable authenticated agent channels | SDK, agent identity, hosted APIs |
| Enterprise IT | Policy, provisioning, audit, support | Managed or private deployment |
| Center Work **[Decided by the owner, 2026-11-15]** | An office suite built on app channels (ADR 0030, 0033) | The first named customer of the Spike SDK. The browser client work (ADR 0033) is part of the launch |

**[Open]** The size of the Spike OS / Box installed base is not recorded anywhere. Go-to-market step 1 depends on it; add the figure or reorder section 5.

## 3. Packaging and pricing hypotheses [Paper — unvalidated]
| Tier | Price hypothesis | Buys |
|---|---|---|
| Personal Free | $0 | Private chat, basic devices, community/self-host relay |
| Personal Plus | $3–6/user/mo | Managed relay reliability, storage/backup, premium sync |
| Business | $5–12/user/mo | Org controls, managed service, support, policies |
| Agent Network | $19–99/org/mo + usage | Agent directory, verified identities, hosted relay/API quotas |
| Enterprise | Custom annual | Private deployment, SSO, SLA, governance |

**[Decided by the owner, 2026-11-15]** Sell only two offers until there is demand evidence: **Agent Network** and **Business / Enterprise**. Personal Plus is deferred: it competes with free consumer messengers and is furthest from the wedge. Personal use stays free. Revisit after the pilot.

Principles: never charge for encryption or imply free users get weaker privacy; usage billing reflects relay traffic, storage and agent transactions, never message contents.

## 4. Illustrative year-two scenario (sensitivity example, not forecast) [Paper]
| Driver | Assumption | ARR |
|---|---|---|
| Business seats | 5,000 × $8/mo | $480,000 |
| Personal Plus | 10,000 × $4/mo | $480,000 |
| Agent orgs | 500 × $49/mo | $294,000 |
| Enterprise | 10 × $30,000/yr | $300,000 |
| **Total** | | **$1,554,000** |

Excludes overages, taxes, discounts, churn. Arithmetic checks out. About 62% of the total ($960k of $1,554k) comes from Business and Personal Plus seats, the segments furthest from the stated launch wedge. Treat as a test of which assumptions matter, not demand evidence.

## 5. Go-to-market sequence [Paper]
1. Bundle with Spike OS / Box for installed-base utility.
2. Recruit 5–10 design partners from existing enterprise relationships; sell secure agent workflows, not another chat app.
3. Publish SDK and protocol docs; encourage third-party clients and agents.
4. Managed regional relays, private deployments, integrator partnerships.

**[Proposed, 2026-11-15]** Before step 1: use the November 2026 Microsoft 365 outage as the opening for interviews and design-partner talks while it is fresh (two to four weeks), and again when Microsoft publishes its post-incident report. Show a "continuity drill" (relay stopped, messages queue, devices on one network keep talking directly) instead of making reliability claims. The pilot offer is limited to reviewed features: 1:1 chat, small groups, attachments, agent approvals. Position the agent layer as the approval and audit channel that works with agent standards such as MCP and A2A, not as a rival protocol.

## 6. Validation plan [Proposed]
| Question | Method | By | Pass signal |
|---|---|---|---|
| Is the pain real? | 15 interviews: Spike users, IT buyers, agent developers | Day 30 | ≥8 describe a current workaround they dislike |
| Is agent coordination the hook? | Show Phase 0 demo to interviewees | Day 45 | ≥5 ask for a pilot |
| Will anyone pay? | Price-test Business and Agent Network with design partners (LOIs / paid pilots) | Day 90 | ≥3 paying or signed-intent partners |
| Is continuity a second hook? **[Proposed]** | Ask every interviewee what their team did during the 12–13 Nov 2026 Microsoft 365 outage | Day 45 | ≥5 used an unapproved channel (personal chat apps, phones) and see it as a problem |
| What does it cost to serve? | Instrument relay bandwidth, storage, push, support time per user | Day 90 | Unit-cost model per tier with real inputs |

## 7. Unit economics to model [Paper cost buckets + Proposed]
Gross margin must account for bandwidth, push, storage, support, security, abuse prevention and enterprise deployment. **[Proposed]** Build a simple spreadsheet before Phase 4 with: cost per active user per month (relay + storage + push), support minutes per account, and break-even seats per tier. Open-source self-hosting caps what managed relays can charge; model the self-host fraction explicitly.

## 8. Open source and IP [Paper]
**Decided by the owner 2026-11-13, see [ADR 0034](adr/0034-licensing.md); not yet applied, legal review pending.** Split licence: relay and SFU under AGPL-3.0; core, SDK and clients under Apache-2.0; specs under CC BY 4.0; a contributor licence agreement before outside contributions. This replaces the paper's wording: publish specs, SDKs, reference clients, self-hostable relay under a permissive license (Apache-2.0 candidate). Maintain a trademark policy. Before release: trademark and domain clearance for "Spike Connect", patent and dependency review, export/compliance review.

## 8a. Regulators and lawful requests [Proposed]
- **Posture:** cooperative and transparent, not adversarial. Comply with lawful orders where Spike operates; be precise about what is technically possible (no message content, limited metadata). Engage regulators early, before launch in each market.
- **Lawful-access exposure review per country** before launch: encryption licensing, data-localisation rules, interception or "technical assistance" obligations, and export controls. Some of these can conflict with the product; decide market entry with that knowledge, not after.
- **Published legal-requests policy** stating what can and cannot be provided, plus **transparency reports** of requests received, where the law allows. Warrant canaries are legally untested; do not rely on them.
- **Pre-decided response** to an order to weaken the product (for example, withdraw from that market rather than ship a weaker version), agreed by the board before it is ever needed.
- **Structure:** choose the company's jurisdiction and hosted-relay locations deliberately; self-hosted relays (Spike Box, enterprise) keep traffic out of Spike's hands entirely.
- Not legal advice: engage counsel in each target jurisdiction.

## 9. Budget and organization
- **[Paper]** Appoint a single product owner; approve a 90-day capped discovery/pilot budget. Suggested core team: product lead, 2 protocol/security engineers, 2 desktop/mobile engineers, 1 backend/relay engineer, 1 QA/SRE, shared design and security review.
- **[Open]** Budget figure for the 90-day cap, and whether the independent security review (Phase 3 scheduling, Phase 5 audit) is funded separately.

## 10. Key risks
| Risk | Mitigation |
|---|---|
| Incumbent network effects | Start where no big social graph is needed (agents, devices) |
| Open-source competitors host the relay | Differentiate on Spike integration and managed enterprise service |
| Willingness to pay is low | Gate spend on 3 paying partners; keep Phase 4 scope contingent |
| Brand conflict | Trademark/domain clearance in first 30 days |
| Unit costs exceed price | Real cost instrumentation from the pilot |
| **[Proposed]** Self-hosted encrypted chat is already sold (Element, Wire, Threema Work, Mattermost) | Lead with agent approvals and the continuity channel, not "data control" alone |
| **[Proposed]** Agent standards (MCP, A2A) absorb the agent channel | Interoperate: be the approval and audit layer beside them |
| **[Proposed]** A reliability claim Spike cannot back | Claim independence of failure only, until load and soak tests and relay failover exist |
| Build outruns validation and review (no freeze, decided 2026-11-15) | Run interviews and book the independent review now, in parallel; the pilot ships reviewed features only |
