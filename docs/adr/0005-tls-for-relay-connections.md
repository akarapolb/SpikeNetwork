# ADR 0005: TLS for relay connections

Status: **Proposed** (implemented)
Date: 2026-10-13

## Why, given end-to-end encryption
Message content is already hidden from the relay and the network. TLS adds protection for what E2EE cannot hide: it stops on-path observers reading device ids, queue contents' sizes and timing, and stops them tampering with or blocking selectively inside the relay connection (a downgraded or modified protocol exchange). It does not replace E2EE and the relay is still untrusted.

## Decision
- **Relay:** terminates TLS itself with `--tls-cert` / `--tls-key` (PEM; rustls with the `ring` provider, TLS 1.2 and 1.3). TLS handshakes are time-limited so a stalled client cannot hold a slot.
- **Safe default:** the relay refuses to start with plain WebSocket on a non-loopback address unless `--insecure-no-tls` is passed (for deployments behind a TLS-terminating proxy).
- **Client:** `wss://` verifies the certificate chain and host name against the built-in Mozilla roots (`webpki-roots`) plus an optional extra CA (`--relay-ca` / `SPIKE_RELAY_CA`) for self-hosted relays.
- **Client safe default:** plain `ws://` is accepted only for loopback hosts. Anything else is refused before any connection is made unless `--allow-insecure-transport` is passed.
- **No custom TLS code:** `rustls`, `tokio-rustls`, `webpki-roots`. No certificate verification bypass exists.

## Tested
Full conversation over `wss://`; self-signed cert rejected without the CA; rejected with a different CA; rejected when the certificate does not name the host; plain client against a TLS relay fails promptly; `ws://` to a public host and non-WebSocket schemes refused; the relay binary refuses to start insecurely.

## Notes for operators
- A self-signed certificate must not be marked as a CA (`basicConstraints CA:FALSE`), or the client rejects it as a CA used as an end entity. Let's Encrypt or an internal CA avoids the question.
- Certificates are loaded at startup. Renew by restarting the relay; queues and prekeys are durable (ADR 0004).

## Not done
Certificate pinning, TLS client certificates (device authentication is Ed25519 challenge-response inside the connection), hot reload of certificates, and OCSP/CT policy beyond what rustls enforces. TLS hides content sizes only partially: record sizes still leak approximate message lengths.
