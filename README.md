# Spike Connect / Spike Network

A privacy-first communication network for people, AI agents and devices. See [`docs/`](docs/) for the roadmap, business plan, engineering plan and architecture diagrams.

**Status: Phase 0 prototype. Not independently reviewed. Do not use it for real secrets.**

## What works today
- Device identity and shareable, self-signed device cards with fingerprints
- End-to-end encrypted one-way messages (Noise_X via the `snow` crate)
- A store-and-forward relay that holds only ciphertext (auth, size/queue/TTL limits, ack)
- A CLI: `init`, `card`, `add`, `verify`, `contacts`, `send`, `recv`
- Contacts start unverified; key changes are refused unless forced; replays are ignored

## Try it
```sh
./scripts/demo.sh      # two devices + one relay, recipient offline during send
cargo test --all       # unit and end-to-end tests
```
By hand:
```sh
cargo run -q -p spike-relay -- --listen 127.0.0.1:8787 &
spike --home /tmp/a init;  spike --home /tmp/b init
spike --home /tmp/a card            # prints card; fingerprint on stderr
spike --home /tmp/b add alice <card-from-a>
spike --home /tmp/b verify alice <fingerprint-read-out-of-band>
```

## Layout
| Path | Purpose |
|---|---|
| `core/` | Identity, sealed messages, relay wire types |
| `relay/` | Relay library and `spike-relay` binary |
| `cli/` | Client library and `spike` binary |
| `docs/adr/` | Decision records (start with 0001) |
| `docs/threat-model.md` | What is and is not protected |

## Known limitations (Phase 0)
No forward secrecy, plaintext key file, `ws://` only, in-memory relay queues, no direct P2P path yet, no attachments, no groups. Details in `docs/threat-model.md` and `docs/adr/0001-*`.

No license has been chosen yet (Apache-2.0 is the candidate).
