# Spike Connect / Spike Network

A privacy-first communication network for people, AI agents and devices. See [`docs/`](docs/) for the roadmap, business plan, engineering plan and architecture diagrams.

**Status: Phase 0 prototype. Not independently reviewed. Do not use it for real secrets.**

## What works today
- Device identity and shareable, self-signed device cards with fingerprints
- End-to-end encrypted two-way conversations using Olm (Double Ratchet) via the audited-lineage `vodozemac` crate: forward secrecy, replay rejection, out-of-order delivery
- Asynchronous session start: signed one-time prekeys served by the relay, so you can message someone who is offline
- A store-and-forward relay that holds only ciphertext (auth, size/queue/TTL limits, ack, prekey store), durable across crashes with `--db`, TLS (`wss://`) with `--tls-cert/--tls-key`; plain `ws://` is refused off loopback by default
- All local state (keys, sessions, contacts) encrypted at rest: Argon2id passphrase KDF + XChaCha20-Poly1305
- A CLI: `init`, `card`, `add`, `verify`, `contacts`, `keys`, `send`, `recv`
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
| `docs/adr/` | Decision records (start with 0001) |
| `docs/threat-model.md` | What is and is not protected |

## Known limitations (Phase 0)
No passphrase change or recovery, no rollback protection, keys live in memory while the CLI runs, no fallback prekey, single-node relay storage, no direct P2P path yet, no attachments, no groups, no independent review. Details in `docs/threat-model.md` and `docs/adr/`.

No license has been chosen yet (Apache-2.0 is the candidate).
