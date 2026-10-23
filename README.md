# Spike Connect / Spike Network

A privacy-first communication network for people, AI agents and devices. See [`docs/`](docs/) for the roadmap, business plan, engineering plan and architecture diagrams.

**Picking this up? Read [`docs/HANDOFF.md`](docs/HANDOFF.md) first.**

**Status: Phase 0 prototype. Not independently reviewed. Do not use it for real secrets.**

## What works today
- Device identity and shareable, self-signed device cards with fingerprints
- End-to-end encrypted two-way conversations using Olm (Double Ratchet) via the audited-lineage `vodozemac` crate: forward secrecy, replay rejection, out-of-order delivery
- Asynchronous session start: signed one-time prekeys served by the relay, so you can message someone who is offline, plus a rotating fallback prekey so a drained device stays reachable
- A store-and-forward relay that holds only ciphertext (auth, size/queue/TTL limits, ack, prekey store), durable across crashes with `--db`, TLS (`wss://`) with `--tls-cert/--tls-key`; plain `ws://` is refused off loopback by default
- All local state (keys, sessions, contacts) encrypted at rest: Argon2id passphrase KDF + XChaCha20-Poly1305
- Own-device sync: read state follows you across devices; a device that missed messages, edits or deletes catches up from another; an expired attachment is re-supplied by another device (`spike read`, `unread`, `sync`, `request-file`)
- Edits and deletes that merge identically on every device (logical clocks, delete always wins, only the author may change a message): `spike edit` / `spike delete`
- Encrypted attachments up to 100 MiB: encrypted before upload, stored on the relay as opaque chunks with expiry and quotas, integrity-checked after download, kept sealed on each device (`spike send-file` / `spike fetch`)
- Device linking: a new device shows a link code, both screens show the same safety code, then your identity, contacts and history are copied over encrypted (`spike link-new` / `spike link-device`)
- Multi-device delivery: every message is encrypted separately for each of the recipient's devices and copied to your own other devices, each delivered to that device's home relay; signed device lists are published on relays so contacts learn new devices automatically
- One identity per person: an owner key signs a versioned list of that person's devices; contacts verify the person once; rollbacks and conflicting lists are refused; older single-device cards still work
- Relay **group mailboxes and key package storage** for large groups (ADR 0025, slice 0): ordered, access-controlled, sealed on removal, pushed to subscribers, with separate retention for commits and messages; no client uses them yet
- **`spike-mls`** (ADR 0025, slice 1): a library for groups larger than 20 on MLS (openmls, pinned): owner-bound credentials checked against signed device lists, a policy in the group, commits checked before they are merged and applied only after the relay accepts them, mailbox rotation on removal, state sealed on the vault. Tested against the real relay; not used by any client yet, not independently reviewed
- Encrypted local message history: every sent and received message stored per conversation, sealed row by row, exactly once even across crashes
- Agent requests with human approval: signed requests, standing grants, signed receipts, a hash-chained audit log (ADR 0014), and a **receipt verifier** a service runs before acting: trusted principals, audience binding, scope and amount checks, single use (ADR 0015), and **typed actions** whose fields the human sees and the service must match exactly (ADR 0017): `agent-grant [--typed-only]`, `agent-request [--field k=v]`, `agent-present`, `verify-receipt [--field k=v]`, `agent-types`
- **A desktop app (Tauri 2):** create or unlock an identity, chat with instant delivery, edit and delete, attachments, contacts and verification, agent grants and approvals shown by their signed typed fields, linking and removing devices, message requests from people who are not yet contacts (with a block list), small groups, relay trust settings, auto-lock. Windows is exercised; see [`desktop/README.md`](desktop/README.md) and ADRs 0019 and 0020. Run your own relay for several computers: [`docs/deploy-relay.md`](docs/deploy-relay.md)
- Small groups of up to 20 people (ADR 0024): signed member lists that every app converges on, members who are not your contacts, invites that wait for you, files for the whole group, the last 7 days for new members when the group offers it, edits and deletes by the author only. in the CLI (`spike group new|list|show|send|send-file|history|add|remove|admin|rename|who-can-add|new-members-see|leave|accept|decline|block`) and in the desktop app
- A CLI: `init`, `card`, `add`, `verify`, `contacts`, `keys`, `send`, `recv`, `history`, `prune`, `link-new`, `link-device`, `send-file`, `fetch`, `edit`, `delete`, `read`, `unread`, `sync`, `request-file`
- Contacts start unverified; key changes are refused unless forced

## Try it
```sh
./scripts/demo.sh      # two devices + one relay, recipient offline during send, then a reply
cargo test --all       # unit and end-to-end tests
```
State is encrypted with a passphrase. Supply it with `--passphrase-file`, `SPIKE_PASSPHRASE`, or at the prompt. By hand:
```sh
cargo run -q -p spike-relay -- --listen 127.0.0.1:8787 --db /tmp/relay.db &   # loopback: plain ws is fine for a local trial
# public relay: add --tls-cert relay.crt --tls-key relay.key; clients use wss:// (and --relay-ca for a private CA)
export SPIKE_PASSPHRASE="a long passphrase"   # demo only; prefer --passphrase-file or the prompt
spike --home /tmp/a init;  spike --home /tmp/b init
spike --home /tmp/a card            # prints card; fingerprint on stderr
spike --home /tmp/b add alice <card-from-a>
spike --home /tmp/b verify alice <fingerprint-read-out-of-band>
spike --home /tmp/b keys            # bob registers prekeys, then can go offline
spike --home /tmp/a add bob <bob-card> && spike --home /tmp/a send bob "hello"
spike --home /tmp/b recv
```

## Layout
| Path | Purpose |
|---|---|
| `core/` | Identity, sealed messages, relay wire types |
| `relay/` | Relay library and `spike-relay` binary |
| `cli/` | Client library and `spike` binary |
| `desktop/` | Desktop app (Tauri 2, own Cargo workspace; UI is plain HTML/CSS/JS) |
| `docs/adr/` | Decision records (start with 0001) |
| `docs/threat-model.md` | What is and is not protected |

## Known limitations (Phase 0)
No passphrase change or recovery, no rollback protection, keys live in memory while the CLI runs, single-node relay storage, no direct P2P path yet, groups only up to 20 people and not yet on mobile, no calls (designed, not built), no independent review. Details in `docs/threat-model.md` and `docs/adr/`.

No license has been chosen yet (Apache-2.0 is the candidate).
