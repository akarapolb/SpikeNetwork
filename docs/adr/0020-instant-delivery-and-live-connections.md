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
- Measured in the real app: the relay's push reaches the app and the message is **stored in about 0.1 s** (113 ms to both of Alice's devices), and it is **on screen in about 0.85 s** (the rest is the sync's own reconnect, decrypt and redraw). Before, it could take up to 15 s. An earlier claim here of "113 ms showing" measured the store, not the screen: see "The UI event" below. Most of the remaining delay was the sync's idle wait: the screen was told only when the sync finished, 0.7 s after the last message. `Session::sync_with` now reports each stored item at once (the listener emits `synced` and updates the title from it), and a test checks the report comes well before the sync ends. The on-screen figure has not been re-measured in the real app since this change.
- **Keepalive.** While waiting, the client pings the relay every 25 s (an application-level `Ping`/`Pong`, not WebSocket control frames, whose replies can sit unflushed). If nothing comes back within 15 s the wait ends with an error and the listener reconnects. The relay answers pings and closes a connection that has sent nothing for 120 s. Verified in the real app: a listener idle for 150 s (longer than the relay's 120 s limit) was still connected and delivered at once; a connection black-holed by a proxy is noticed in about 2 s (`cli/tests/keepalive.rs`).

**Relay rule: a device may hold up to 4 live connections, and a new message is pushed to all of them.** The oldest beyond four is closed (its client sees the end at once).

## Why the relay rule changed (what went wrong first)
The relay used to keep one live connection per device: a newer connection **replaced** the older one's delivery channel but left the older socket open. That is a trap: the listener's connection went silently deaf the moment the app did anything that connected (its own start-up sync), and it never found out, so push never worked in the real app although a library-level test passed (that test's assertion was too loose and accepted a quiet timeout). Closing the replaced connection looked like the fix, but then `ops::recv`, which opens a second connection from the same device while it is still receiving (to send receipts and forward requests), had its own connection closed under it, and eight agent tests failed. Allowing several live connections, and pushing to all, fixes both: the listener stays connected, and nested sends are harmless. The cap bounds what a buggy or hostile client can hold open.

## Tested
`desktop/src-tauri/tests/api.rs`: the listener connection is woken by a message and the message is still unacknowledged (a normal sync then receives it exactly once); an operation that connects does not deafen it and it is woken again afterwards; a device that opens five connections loses the oldest and the newer four stay open; linking pauses the listener until it finishes or is cancelled. In the real app: two people and a linked second device, latency measured at 113 ms; lock and unlock restart the listener.

## The UI event (a bug found late)
The backend emits `synced` and `relay-status` events so the screen refreshes at once. In Tauri 2 a window may only listen to events if a capability grants it, and the app had none, so `listen` was refused silently and the screen updated only on the 60-second poll. Backend timings and tests could not see this. The fix is `capabilities/default.json` with exactly `core:event:allow-listen` and `core:event:allow-unlisten`. Lesson: measure what the user sees, not the store.

## Limits (honest list)
- A keepalive ping is sent only while the client waits for deliveries; connections used for a single request do not need one.
- The relay's 120 s idle limit and the 25 s / 15 s ping timings are fixed in code, not flags.
- While the app is locked, nothing is received: the keys are not in memory. That is the point of locking.
- The cap of four live connections per device is fixed in code, not a flag.
- Unreviewed code, like everything else here.
