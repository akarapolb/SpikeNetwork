# ADR 0027: Direct device-to-device connections

Status: slice 1 built, 2026-10-24 (Rust engine, bridge, Android, desktop). Same Wi-Fi only. Not reviewed.

## Decision

Two Spike devices can hand each other messages **directly over the local network, without a relay**. The relay stays
the default and the fallback; direct is an optimisation for when both devices can see each other. It is a
**setting that is off by default** ("Direct connections"), because turning it on opens a listening port.

### What travels

Exactly what the relay would carry: the opaque blob that `Sessions::encrypt` produces (an Olm/Double Ratchet message).
Nothing about the end-to-end layer changes: a direct blob is opened by the same `process` function as one that
came from the relay, produces the same `Outcome`s, and is acknowledged only after it is stored. The ratchet is
indifferent to which road a message took. That is also why there is no second copy of any feature: one-to-one
messages, files' descriptors, edits, deletes, read markers, catch-up and small-group traffic all go through
`send_to_device`, which now tries the direct road first.

Not carried in this slice: large groups (their mailboxes live at the relay by design, ADR 0025), attachment
bytes (chunks are fetched from the relay that holds them; a file's *message* is direct, its download needs the
relay), and the first message to a device we have no session with (starting a session needs one of its prekeys, and
prekeys are claimed from a relay). These keep using the relay and say so.

### Discovery

Discovery is by **address, not by broadcast**, in this slice:

1. A device that has direct connections on shows its addresses (`ip:port`, from the interface that routes to the LAN)
   in Settings. A person types the other device's address into that contact's "Direct address" field, or scans it
   (later).
2. A device that *accepts* a direct connection learns the caller's address (the source IP and the listening port the
   caller states, both authenticated afterwards), so the reverse direction needs no typing. Learned addresses are
   per device id, newest first, at most three.
3. mDNS/DNS-SD (`_spike-connect._tcp`) is the intended next step so no typing is needed. It is not in slice 1:
   Android needs a `MulticastLock` and the emulator cannot carry multicast, so it could not be tested here. The
   advertisement would carry **no identity**, only a random per-boot instance id and the port; the handshake below is
   what says who answers, so a LAN neighbour learns "something Spike listens here", never whose.

An address is only a hint about where to knock. **Nothing is trusted because of where it came from.**

### Transport and encryption

TCP, length-prefixed frames, then an authenticated key exchange of our own (`core/src/direct.rs`, sans-IO and unit
tested), then every frame sealed with XChaCha20-Poly1305, as the rest of the product seals things at rest and in
transit (vault, attachments, link sessions). TLS was rejected: it authenticates by certificates, we authenticate by
device keys, and bending one into the other is the usual way to get this wrong.

The handshake is a SIGMA-style exchange over X25519 ephemerals, with the device's Ed25519 identity key as the signing key
(the one whose hash is its device id):

1. `I -> R`: version, `E_i`.
2. `R -> I`: `E_r`, then, sealed under a key from `DH(E_i, E_r)`: `R_ed`, and `sig_R` over
   `H("R", E_i, E_r, R_ed)`. The initiator *aborts* unless `R_ed` hashes to the device id it meant to reach.
3. `I -> R`, sealed: `I_ed`, its listening port, and `sig_I` over `H("I", E_i, E_r, R_ed, I_ed, port, sig_R)`.
   The responder then checks `I_ed` against the devices it knows (below).
4. Both derive two directional keys from the shared secret salted with the whole transcript.

Properties: mutual authentication by device key; both signatures cover both ephemerals and both identities, so no
unknown-key-share and no replay of a recorded handshake; fresh keys per connection (forward secrecy); frame counters
are the nonces, so a replayed, dropped or reordered frame fails to open; the identities are sent encrypted, so a
passive listener learns nothing about who talks. A *signing* context of its own is used (`spike-direct-v1`), distinct
from the relay's challenge context, so a hostile relay cannot hand a device a "nonce" that is really a direct
transcript.

**Who may connect.** A responder accepts a handshake only from a device that is in a contact's verified (signed,
version-checked) device list, or one of our own. A stranger on the LAN is refused after the handshake with no reply, and
costs us one key exchange. Strangers who want to say hello still use the relay and the message-request path
(ADR 0021).

**What the frames say.** One frame type in slice 1: `Blob`. The receiver stores it through `process`, then answers `Ack`
(or `Refused`). The sender counts the message as delivered *only on `Ack`*. Anything else (no route, refused, timeout,
connection closed before `Ack`) means the same blob goes to the relay queue exactly as before. If the `Ack` was lost after
the receiver stored the message, the relay copy is a duplicate; the receiver cannot open the same ratchet message twice, so the copy is dropped as a duplicate or
an unreadable blob, which is harmless.

### When the peer is offline or unreachable

Direct is tried first with a short budget (about 2 seconds to connect, 5 for the whole exchange); on any failure the blob
goes to the relay queue. So an offline peer, a firewall, a different network or a changed IP degrade to today's behaviour,
only slower by that budget on the first attempt. Failed addresses are skipped for a minute so a dead peer does not slow
every message. If the relay is also unreachable and the direct send failed, the send fails as it always did.

A device with direct connections on no longer *requires* its relay to send: the pre-send housekeeping (prekey top-up,
device-list refresh) becomes best effort and reports a note instead of failing, so two devices in a house with the
router's uplink down can still talk. Housekeeping runs again on the next relay contact.

### NAT and Internet traversal: later

Not built. The honest options, in order of preference: (1) learned/typed addresses of IPv6 hosts and port-forwarded
desktops, which already work with this protocol unchanged; (2) UDP hole punching with a rendezvous that the relay
already is, carrying only the encrypted address hints that the devices send each other (the relay would learn
timing it already learns); (3) a relay-assisted stream (TURN-like), which is a relay again. The handshake is
transport-agnostic, so each of these is a new way to obtain a byte stream, not a new security design.

### Threat model

| Attacker | What they can do | Outcome |
|---|---|---|
| Passive on the LAN | See that two addresses exchange traffic of certain sizes and times | Contents and identities are encrypted; **metadata** (that these IPs talk, when, how much) is visible, as it is for a Wi-Fi network operator today. The relay path hides IPs from the LAN but shows them to the relay; direct trades one observer for the other. |
| Active on the LAN (MITM, impersonating an address) | Redirect or answer a connection | Fails: the initiator checks the responder's key against the device id it wanted; the responder checks the caller against signed device lists. Nothing is authenticated by IP. |
| Rogue device that connects to our port | Make us do one key exchange; probe whether an address runs Spike; learn a responder's *public* device key by connecting (identities are protected from passive observers, not from an active prober that completes message 1/2) | Refused after the handshake. Limits: handshake timeout, 8 concurrent connections, per-IP backoff. **Residual:** a LAN neighbour who dials us learns our device id (a hash of a public key). The relay already knows it. |
| Replay of a recorded session | Resend frames | Each connection has fresh ephemerals and counter nonces; replays fail. A replayed *blob* at the application layer is the ratchet's concern, as on the relay. |
| Denial of service on the port | Fill connections | Capped; the relay path is unaffected. |
| Compromised device key | Everything, as always | Unchanged by this ADR. Removing the device from the signed list stops others accepting it. |
| Relay (and what it no longer sees) | Nothing new | A direct message never reaches the relay: it learns *less* (no queue entry, no timing) -- but only for messages that went direct, and the Ack-lost case sends a duplicate. |

Also stated plainly: a listening port on a phone or laptop is attack surface that did not exist. That is why it is opt-in,
why strangers are dropped before any application bytes are parsed, why frames and the handshake are size-capped, and why
the code is small.

## What was built (slice 1)

- `core/src/direct.rs`: the handshake and the frame cipher, no I/O, with tests (round trip, wrong responder refused,
  tampered and replayed frames rejected, direction separation, signature binding).
- `cli/src/direct.rs`: the listener, the dialer, address book (`direct.json` in the sealed home), allow-list from
  signed device lists, rate limits; `ops::send_to_device` tries direct first; `ops::receive_blob` is the receive half
  of `recv` factored out so both roads share it.
- `app`: `direct_enable`, `direct_disable`, `direct_status`, `direct_set_address`, `direct_wait` in `Session` and the
  bridge; the Tauri commands.
- Android and desktop: a Direct connections card in Settings (switch, addresses to give the other device, an address
  field per contact, counters of direct sends, relay fallbacks and direct receives). The screens refresh when a direct
  message arrives; Android notifies as for a relay message.
- Tests: `core/src/direct.rs` unit tests (7); `cli/tests/direct.rs` (6: two homes with **no relay running**, both
  ways, the reverse address learned; fallback when nothing listens; direct off means no dialling; an unreadable blob is
  refused, not acknowledged; a stranger gets nothing and is not remembered; a different device at the address is not
  trusted); `app/tests/bridge.rs` (a bridge test with the relay replaced by a dead address);
  `android/drive_direct.mjs`: the real Android app (emulator) and the real desktop app, a relay of the script's own on
  port 8797 that is **killed**, then 1 message each way typed/sent with the relay dead (both read on the receiving
  screen), counters on both sides, then the relay is restarted and the phone's listener switched off to show the relay
  still delivers. 19 checks, all pass. The emulator is reached as `10.0.2.2` from inside and `adb forward` from outside;
  a real phone on Wi-Fi needs neither, only the address.
- Honest limits of the test: the emulator is not a second device on a real LAN; the real-phone case (two physical
  devices on one Wi-Fi, Android doze, router client isolation) has not been run. Many Wi-Fi networks isolate clients
  from each other, which silently defeats direct connections and falls back to the relay.

## Not done

mDNS discovery; address QR; NAT traversal; large groups and file bytes over direct; reuse of one connection for several
blobs (each send dials); a per-contact "never use direct" switch; Windows firewall prompts are the operating system's.
