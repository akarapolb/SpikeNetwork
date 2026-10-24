# Metadata review: what a relay sees

Status: written 2026-10-24 from reading `relay/src/lib.rs`, `relay/src/store.rs`, `core/src/wire.rs` and
`core/src/mailbox.rs`. Nothing here was measured on a live deployment. **No metadata protection is claimed.**
This is the list a reviewer or an operator should start from.

## What a relay learns, by request

| Request (see `core/src/wire.rs`) | The relay learns | Stored? |
|---|---|---|
| Connect + sign in | Network address, device id (hash of the Ed25519 key), time, connection length, TLS server name | Not stored. In memory while connected; per-address counters only |
| `Send { to, blob }` | **Sender device id** (the connection is authenticated), **recipient device id**, blob size, time. Recipient and sender are linked for the life of the queue entry | Recipient id, blob, expiry, in SQLite. The sender id is not stored in the row (the recipient is not told it); it is visible in memory and to anyone watching the process or the wire |
| `Ack` / backlog delivery | When the recipient reads, from which address | Deletes the row |
| `ClaimPrekey { target }` | That device A is about to start a conversation with device B (claimer id + target id + time). The strongest contact-graph signal the relay gets | Counters only |
| Publish / fetch device list | The person's owner key, every device id and the relay each uses, version history; who fetches whose list and when | Lists are stored in the clear by design (signed, public) |
| Prekeys / key packages | Which devices exist, how many keys remain (`Ready.prekeys`) | Stored in the clear (public keys) |
| Group mailbox create / append / read / subscribe (large groups) | A mailbox id, which device ids append and subscribe (= the member devices, not their names), entry sizes and times, commit vs message kind | Entries are ciphertext (outer AEAD under a key the relay never holds); sizes, kinds, sequence numbers and times are visible |
| Attachment chunks | Uploader id, chunk count, total size, who downloads and when | Ciphertext, 30 days |
| `/metrics` (operator) | Aggregate counters only; a test checks no id or content appears | n/a |

## What an on-path observer adds

With `wss://`: address, relay host name (SNI), timing and rough size of each record. Without it (`ws://`, only
by explicit opt-in off loopback): also device ids and which requests are made. Content stays encrypted either
way.

## Honest assessment

- The relay can build a **who-talks-to-whom graph with timing** from `Send` alone (sender id and recipient id are
  both visible while the request is handled) and from prekey claims. A hostile or compelled operator can log
  this even though the shipped relay does not (`no content logging` is a code property you can read, not
  something a client can verify on a hosted relay).
- Device ids are stable. The same device is recognisable across connections, networks and relays.
- Message sizes are exact (no padding), so short replies, typing-style traffic and files are distinguishable.
- A device list names every device of a person and the relay each reads from: this is public to anyone who can
  connect to any relay that holds it, and anyone can connect (open mode, ADR 0023 is not built).
- Large-group membership is visible as the set of device ids subscribed to a mailbox.
- A contact (or anyone) can **choose which relay your device connects to** by naming it in their own device
  list entry (ADR 0009): that relay then sees your device id and timing. See audit finding R-08 (fixed: the
  sign-in answer is now bound to the relay it was made for, so such a relay cannot reuse it elsewhere).

## Design notes (not built; each needs its own ADR and review)

1. **Sealed sender.** Make `Send` work without the relay learning the sender: sender proves authority with a
   short-lived *delivery token* the recipient issued (derived from a per-contact secret, rotated), or sends over
   an unauthenticated connection with the token as capability. The recipient learns the sender from inside the
   Olm session (this already holds). Costs: capability distribution through the signed card / first message;
   abuse control moves from "per authenticated device" to "per token" (revocable, which is also the fix for
   queue-fill DoS, audit R-03). Signal's sealed sender is the reference, including its known weakness: the relay
   still sees the sender's IP, so it only helps against a relay that does not correlate addresses.
2. **Prekey claims without a graph.** Claim through a separate, unauthenticated, rate-limited path (or batch
   claims through a proxy) so claimer identity is not tied to target. Or distribute a few one-time prekeys inside
   the card the contact shares (no relay claim at all for people who met in person).
3. **Padding and batching.** Pad blobs to a small set of bucket sizes (e.g. 1, 4, 16, 64 KiB; files already move
   in fixed chunks) and have idle clients send occasional dummy `Send`s to themselves. Cost is bandwidth and
   battery; effect is limited against an observer who sees the connection endpoints.
4. **Hide the address.** Optional relay access through a proxy or Tor-like transport; `TransportOpts` is the
   place (one `dial` function). Direct device-to-device connections (ADR 0027) change the picture again and need
   their own metadata review.
5. **Relay-hop separation.** A person's device list entry already names a home relay: encourage self-hosted
   relays for sensitive groups so the operator is a party the users chose.
6. **Large-group membership.** Subscribe with per-mailbox pseudonymous keys (derived from the group, not the
   device identity) so the relay sees mailbox activity but not stable device ids. MLS already makes the leaf
   keys per-group; the relay-side authentication currently uses the device key.

## Cheap things worth doing before any real use

- Say in the app (it does: "What any relay can see") and in the first-run text that a relay learns who talks to
  whom and when. Add "sizes are not hidden".
- Run the project's own relay, on hardware the group controls, behind `wss://`, with `--db` on an encrypted
  volume, no access logs on the front proxy, and metrics bound to loopback.
- Treat device-list publication as public (it is).
