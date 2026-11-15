# Interview script: validation round (15 interviews)

**Status: draft, 2026-11-15.** For the validation plan in [`business-plan.md`](business-plan.md) section 6. Edit the wording to sound like you; keep the order and the scoring.

## What this round must answer

| Question | Pass signal | Where it is scored |
|---|---|---|
| Is the pain real? | At least 8 of 15 describe a current workaround they dislike | Parts 2 and 3 |
| Is continuity a second hook? | At least 5 used an unapproved channel during the outage and see it as a problem | Part 2 |
| Is agent coordination the hook? | At least 5 ask for a pilot after the demo | Parts 4 and 5 |
| Will anyone pay? | Feeds the day-90 target of 3 paying or signed-intent partners | Part 6 |

## Who to interview

Fifteen people, five from each group. Prefer people who own the problem or the budget.

| Group | Who | Use parts |
|---|---|---|
| A. IT and security buyers | IT manager, security lead or operations head at a regulated or privacy-sensitive SME (legal, health, finance, government supplier) | 1, 2, 3, 4, 5, 6, 7 |
| B. Agent builders | Developers or product leads shipping AI agents that take actions for users | 1, 3 (short), 4, 5, 6, 7 |
| C. Spike OS / Box users | Existing users or prospects of Spike products | 1, 2, 3, 5, 6, 7 |

Screen out anyone who neither chooses tools nor feels the problem day to day.

## Rules for the interviewer

- **Ask about what happened, not what they would do.** "What did you do last Thursday?" gives evidence. "Would you use this?" gives politeness.
- **Do not pitch before Part 5.** If they ask what Spike is, say "I'll show you in fifteen minutes; I want your view first."
- **Follow every answer once** with "why?" or "what happened then?"
- **Write down their exact words** for problems and workarounds. Those words become the pitch.
- **Thirty minutes.** Stop on time unless they ask to continue.
- Two people if possible: one talks, one notes. Ask before recording.

## Part 1: Opening (2 minutes)

> Thanks for the time. I'm working on a communication product and I'm trying to understand how teams like yours work today, before we decide what to build next. I'm not selling anything in this call. There are no right answers, and criticism is the most useful thing you can give me. May I take notes [or record]?

1. What is your role, and how many people does your team or company have?
2. Which tools does your team use to talk to each other and to share files?

## Part 2: The outage (5 minutes; groups A and C)

3. On 12 and 13 November, Microsoft 365 had a long outage. Were you affected? *(If they don't use Microsoft 365: "When was the last time your main chat or email was down?" and continue with that event.)*
4. Walk me through that day. When did you notice, and what did people do first?
5. How did the team keep talking to each other? To customers?
6. Which apps or channels did people switch to? Were those approved by the company?
7. Was anything sent over those channels that should not have been (client data, credentials, documents)?
8. What did it cost you: missed deadlines, lost hours, an unhappy client?
9. Has anything changed since? Did anyone ask for a backup plan?
10. Before this outage, did you have a plan for it? Who owns that plan?

**Listen for:** personal WhatsApp or LINE, personal email, phone trees, "we just waited", a compliance worry, someone senior asking for a fix.

## Part 3: Control and trust today (5 minutes)

11. Is there anything your team is not allowed to send over your current chat or email? What do you do with those things?
12. Who outside your company can technically read your messages or files today? How do you feel about that?
13. Have you been asked by a client, auditor or regulator where your communications data is kept, or who can access it? What did you answer?
14. Have you ever looked at running your own messaging (Element, Mattermost, Wire, or similar)? What happened?
15. What is the most annoying thing about your current setup that you have simply learned to live with?

**Listen for:** a workaround they dislike (USB sticks, printed documents, a separate "secure" app only some people use), an audit question they could not answer well, a self-hosting attempt that was abandoned and why.

## Part 4: AI agents (6 minutes; groups A and B)

16. Are AI agents or assistants doing any real work in your company today? Doing what?
17. Do any of them take actions, not only answer questions (send something, change a record, spend money)?
18. Tell me about the last time an agent needed a person's permission to do something. How did the person approve it?
19. If an auditor asked "who approved this action, and when?", what would you show them?
20. Has an agent ever done something it should not have? What happened next?
21. What are you not letting agents do today, only because you cannot control or prove it?

For group B (builders), add:

22. How do your agents authenticate to each other or to your customers' systems?
23. What did you build yourselves for approvals and audit logs? How long did it take, and are you happy with it?
24. Are you using MCP, A2A or another standard? What is missing from it for you?

**Listen for:** approvals by chat message or email with no record, a home-built approval system they resent maintaining, actions blocked for lack of proof.

## Part 5: Demo and reaction (7 minutes)

Say this first, exactly:

> What I'm about to show is a working prototype. It has not had an independent security review, so it is not ready for real confidential use yet. I want your reaction to the idea.

Show two things, about two minutes each:

- **Agent approval.** An agent requests an action; a person approves on their phone; the signed receipt and activity log appear.
- **Continuity drill.** *(When built.)* The relay is stopped; messages queue; two devices on the same network keep talking; the relay returns and everything catches up.

Then ask, and stay quiet after each question:

25. What did you just see, in your own words?
26. What, if anything, would this replace or sit next to in your company?
27. What would stop you from trying it?
28. Who else would have to agree before you could use something like this?

**Listen for:** whether they retell it correctly, whether they name a real use without prompting, and whether they ask about a pilot before you raise it.

## Part 6: Commitment (3 minutes)

Compliments are not evidence. Ask for something that costs them a little.

29. If this had been reviewed and was ready, would you run a pilot with a small team? Which team, and when?
30. What would you need to see first (a security review, a reference customer, self-hosting, a contract term)?
31. What do you pay per person per month today for chat and collaboration? For any agent tooling?
32. We are thinking of roughly [$5–12 per user per month for teams / $19–99 per organisation per month plus usage for agents]. Is that cheap, fair, or too much, and compared with what?
33. Would you sign a non-binding letter of intent for a pilot, or introduce me to the person who could?

**Listen for:** a named team and a date, a request for a follow-up meeting, an introduction offered, or a clear "no" with a reason. All four are useful. "Sounds interesting, keep me posted" is a no.

## Part 7: Close (2 minutes)

34. Is there anything I should have asked you and didn't?
35. Who else should I talk to about this? May I use your name?
36. May I come back to you when the pilot is ready?

## Scoring sheet (fill in within an hour of each interview)

| Field | Answer |
|---|---|
| Date, name, role, company size, group (A, B or C) | |
| Affected by the outage? What did they switch to? | |
| **S1. Used an unapproved channel during the outage and sees it as a problem** | yes / no |
| **S2. Described a current workaround they dislike** (quote it) | yes / no |
| Agents taking actions today? How are approvals done and proven? | |
| Retold the demo correctly? | yes / partly / no |
| **S3. Asked for a pilot, unprompted or at question 29, with a team or date** | yes / no |
| **S4. Offered a real commitment** (letter of intent, follow-up meeting, introduction) | yes / no |
| Price reaction and what they compared it with | |
| Blockers they named | |
| Their best sentence, word for word | |
| Surprise: anything that contradicts our plan | |

## Reading the results after 15

| Result | Meaning |
|---|---|
| S2 yes from 8 or more | The pain is real. Continue. |
| S1 yes from 5 or more | Continuity is a second hook. Build relay failover and the continuity drill. |
| S3 yes from 5 or more | Agent approval is the hook. Lead with it. |
| S4 yes from 3 or more | Start design-partner agreements now. |
| S2 below 8 and S3 below 5 | The wedge is wrong for these groups. Review the kill and pivot options in [`product-roadmap.md`](product-roadmap.md) section 7 before spending more. |

Also compare the three groups. If one group produces most of the "yes" answers, that group is the launch segment, and the next round of interviews should be only with people like them.
