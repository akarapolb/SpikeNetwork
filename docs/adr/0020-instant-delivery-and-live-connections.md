# ADR 0020: Instant delivery and live connections

Status: **Proposed** (implemented)
Date: 2026-10-20

## Problem
The desktop app polled the relay every 15 seconds (ADR 0019), so a message could arrive up to 15 seconds late. Receiving also has a subtle rule: `send` and `recv` each load and save the ratchet state, so two of them running at once corrupt it. Making delivery instant must not add a second place that touches that state.

## Decision
**A listener only waits; a normal sync does the work.**
- After unlock the app starts one background task. It holds a relay connection and waits for the relay to push a message. It **never acknowledges and never decrypts**. When something arrives it drops the connection and runs the ordinary sync (`ops::recv`), which decrypts, stores and acknowledges. Because the listener did not acknowledge, the relay still has the message and the sync receives it. Ratchet state is touched only by the code that always touched it, under the app's one network lock.
- The listener connects only **while holding that lock**, so it never opens a connection in the middle of an operation. While a new device waits to be linked, the listener is **paused**: a sync would take the link frames off the relay and acknowledge them.
- It emits `synced` and `relay-status` events so the UI refreshes at once, sets the window title to the number of unread messages and waiting requests, and flashes the taskbar if the window is not focused. A 60-second poll remains as a safety net.
- **Locking aborts the task**, and it holds only a weak reference to the session while waiting, so no copy of the keys outlives the lock.
- Measured in the real app: 113 ms from Bob's send to both of Alice's devices showing the message (previously up to 15 s).

**Relay rule: a device may hold up to 4 live connections, and a new message is pushed to all of them.** The oldest beyond four is closed (its client sees the end at once).

## Why the relay rule changed (what went wrong first)
The relay used to keep one live connection per device: a newer connection **replaced** the older one's delivery channel but left the older socket open. That is a trap: the listener's connection went silently deaf the moment the app did anything that connected (its own start-up sync), and it never found out, so push never worked in the real app although a library-level test passed (that test's assertion was too loose and accepted a quiet timeout). Closing the replaced connection looked like the fix, but then `ops::recv`, which opens a second connection from the same device while it is still receiving (to send receipts and forward requests), had its own connection closed under it, and eight agent tests failed. Allowing several live connections, and pushing to all, fixes both: the listener stays connected, and nested sends are harmless. The cap bounds what a buggy or hostile client can hold open.

## Tested
`desktop/src-tauri/tests/api.rs`: the listener connection is woken by a message and the message is still unacknowledged (a normal sync then receives it exactly once); an operation that connects does not deafen it and it is woken again afterwards; a device that opens five connections loses the oldest and the newer four stay open; linking pauses the listener until it finishes or is cancelled. In the real app: two people and a linked second device, latency measured at 113 ms; lock and unlock restart the listener.

## Limits (honest list)
- **No keepalive.** The connection carries nothing while idle. A router or firewall that silently drops idle connections goes unnoticed until the 60-second safety check, so delivery can then be up to a minute late. A ping with a pong deadline is the fix.
- A proxy with a short idle timeout (nginx's default is 60 s) closes the connection; the app reconnects within a second and loses nothing, but it is wasteful (see `docs/deploy-relay.md`).
- While the app is locked, nothing is received: the keys are not in memory. That is the point of locking.
- The cap of four live connections per device is fixed in code, not a flag.
- Unreviewed code, like everything else here.
