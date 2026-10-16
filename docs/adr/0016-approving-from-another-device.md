# ADR 0016: Approving agent requests from another device

Status: **Proposed** (implemented)
Date: 2026-10-16

## Problem
ADR 0014 lets only the **approver device** (first in the signed device list) handle agent requests, so a request is decided and logged once. That left the natural case unsolved: the request lands on a desktop, the human holds a phone.

## Decision
Keep one place that decides and logs; let any of the person's devices ask it to. Two new own-device sync messages (ADR 0013 channel, so already end-to-end encrypted and refused from anyone outside the person's signed device list):

- **`AgentPending { request, from }`**: the approver, when a request needs a human, forwards the agent's signed request to the person's other devices. Receivers accept it only if it comes from the approver device, the agent's signature verifies, and it is addressed to this person; they keep it (capped at 100, dropped at expiry) and `agent-pending` lists it as waiting.
- **`AgentDecision { request_id, approve, note }`**: `agent-approve` / `agent-deny` on a non-approver device sends this to the person's devices and forgets the local copy. Only the approver acts on it: it runs the same `decide` as a local decision, so **the grant is re-checked as it is now** (a revoked or expired grant cannot approve), the decision goes into the one audit chain, and it signs and sends the receipt itself. The phone never signs a receipt and holds no log.

An undelivered or refused forward fails visibly: no reachable device means the command errors and the request stays waiting; a refused approval reaches the approver as a `Rejected` outcome and no receipt is sent.

## Why not let each device decide
Two devices answering the same request would produce two receipts and two diverging audit chains, and the agent-side truncation check (ADR 0014) assumes one chain per principal. A single decider keeps both properties; the price is that the approver must come online to apply a phone's decision.

## Tested (`cli/tests/agents_remote.rs`)
Approve from the phone end to end over a relay (receipt, note, one audit chain only on the desktop, phone logs nothing, double decision refused); a revoked grant refuses a forwarded approval and the request stays decidable by denying; forwarded requests are refused from non-approver devices, decisions arriving at non-approvers are ignored, unknown request ids are rejected, forged requests fail wire validation.

## Limits (honest list)
- The decision only takes effect when the approver device next runs `spike recv`. If it is off, the agent waits (until the request expires).
- When the approver decides locally, other devices keep showing the request as waiting until it expires; deciding it there is then rejected by the approver. No "resolved" notice is sent yet.
- Every device holds the owner key, so a stolen phone can approve (and could sign anything anyway). This adds no new power, only a new path; the grant limits still bind.
- If the approver is removed from the device list, the next first device becomes the approver and forwarded copies from the old one are refused.
