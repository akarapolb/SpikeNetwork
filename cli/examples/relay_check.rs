//! Checks a freshly deployed relay from the outside, the way a phone would meet it (deploy/README.md):
//! the name resolves, the HTTPS certificate is valid, the relay signs a device in, it hands out TURN logins,
//! and the TURN server accepts those logins and gives a relayed address, over UDP and TCP.
//!
//!   cargo run -p spike-cli --example relay_check -- wss://relay.example.org
//!
//! It signs in as a throwaway device (a new key each run) and sends no message. Exit code 0 only if every check passed.

use spike_cli::client::Relay;
use spike_core::identity::Identity;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{lookup_host, TcpStream, UdpSocket};
use turn::client::{Client, ClientConfig};

struct Report {
    failed: u32,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        println!(
            "  ok    {what}{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    fn bad(&mut self, what: &str, why: &str, hint: &str) {
        self.failed += 1;
        println!("  FAIL  {what}: {why}\n        -> {hint}");
    }
    fn warn(&self, what: &str, why: &str) {
        println!("  warn  {what}: {why}");
    }
}

fn host_port(url: &str) -> (String, u16) {
    // "turn:host:3478?transport=udp", "turns:host:5349", "wss://host", "wss://host:8443/x"
    let rest = url
        .split_once("://")
        .map(|x| x.1)
        .or_else(|| url.split_once(':').map(|x| x.1))
        .unwrap_or(url);
    let rest = rest.split(['?', '/']).next().unwrap_or(rest);
    match rest.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap()),
        _ => (
            rest.to_string(),
            if url.starts_with("wss") { 443 } else { 3478 },
        ),
    }
}

fn is_private(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified()
        }
        std::net::IpAddr::V6(v) => v.is_loopback() || v.is_unspecified(),
    }
}

async fn udp_allocate(
    host_addr: SocketAddr,
    username: &str,
    password: &str,
) -> Result<SocketAddr, String> {
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    let client = Client::new(ClientConfig {
        stun_serv_addr: host_addr.to_string(),
        turn_serv_addr: host_addr.to_string(),
        username: username.to_string(),
        password: password.to_string(),
        realm: String::new(),
        software: String::new(),
        rto_in_ms: 0,
        conn: Arc::new(sock),
        vnet: None,
    })
    .await
    .map_err(|e| e.to_string())?;
    client.listen().await.map_err(|e| e.to_string())?;
    let out = tokio::time::timeout(Duration::from_secs(8), async {
        use webrtc_util::Conn;
        let relayed = client.allocate().await.map_err(|e| e.to_string())?;
        relayed.local_addr().map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|_| Err("no answer within 8 seconds".to_string()));
    let _ = client.close().await;
    out
}

#[tokio::main]
async fn main() {
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("usage: relay_check wss://relay.example.org");
        std::process::exit(2);
    };
    let mut r = Report { failed: 0 };
    println!("Checking {url}");

    // 1. The name.
    let (host, port) = host_port(&url);
    match lookup_host((host.as_str(), port)).await {
        Ok(a) => {
            let ips: Vec<String> = a.map(|x| x.ip().to_string()).collect();
            r.ok("the name resolves", &ips.join(", "));
        }
        Err(e) => {
            r.bad(
                "the name resolves",
                &e.to_string(),
                "make the DNS A record (README step 1) and wait for it to spread",
            );
            std::process::exit(1);
        }
    }
    if !url.starts_with("wss://") {
        r.warn("the address", "it is not wss://: a phone away from home will not reach it, and nothing is encrypted in transit");
    }

    // 2. HTTPS certificate + sign in.
    let me = Identity::generate().unwrap();
    let probe = async {
        let mut relay = Relay::connect(url.as_str(), &me).await?;
        let rtt = relay.ping().await?;
        Ok::<_, anyhow::Error>((relay, rtt))
    };
    let mut relay = match tokio::time::timeout(Duration::from_secs(20), probe).await {
        Err(_) => {
            r.bad(
                "sign in over a verified certificate",
                "no answer within 20 seconds",
                "ports 80 and 443 open at the provider's firewall too? `docker compose logs caddy`",
            );
            std::process::exit(1);
        }
        Ok(Err(e)) => {
            r.bad(
                "sign in over a verified certificate",
                &format!("{e:#}"),
                "a certificate error means Caddy has not got one yet (port 80 reachable? DNS right?): `docker compose logs caddy`; a refused connection means the relay is not running",
            );
            std::process::exit(1);
        }
        Ok(Ok((relay, rtt))) => {
            r.ok(
                "sign in over a verified certificate",
                &format!("answered in {} ms", rtt.as_millis().max(1)),
            );
            relay
        }
    };

    // 3. TURN logins.
    let turn = match relay.turn_credentials().await {
        Ok(Some(t)) => {
            r.ok(
                "the relay hands out TURN logins",
                &format!("{} address(es), valid {} s", t.urls.len(), t.ttl_secs),
            );
            t
        }
        Ok(None) => {
            r.bad("the relay hands out TURN logins", "it has none", "TURN_SECRET and DOMAIN set in .env? `docker compose logs relay` shows the --turn-url flags");
            std::process::exit(1);
        }
        Err(e) => {
            r.bad(
                "the relay hands out TURN logins",
                &format!("{e:#}"),
                "see `docker compose logs relay`",
            );
            std::process::exit(1);
        }
    };

    // 4. The TURN server accepts them.
    for u in &turn.urls {
        let (h, p) = host_port(u);
        let tcp = u.contains("transport=tcp") || u.starts_with("turns:");
        let Some(addr) = lookup_host((h.as_str(), p))
            .await
            .ok()
            .and_then(|mut a| a.find(|x| x.is_ipv4()))
        else {
            r.bad(
                u,
                "the name does not resolve to an IPv4 address",
                "DOMAIN in .env must be the name you made the A record for",
            );
            continue;
        };
        if tcp {
            match tokio::time::timeout(Duration::from_secs(8), TcpStream::connect(addr)).await {
                Ok(Ok(_)) => r.ok(u, "port accepts connections"),
                _ => r.bad(u, "cannot connect", "open this port (TCP) at the firewall and the provider's security group; `docker compose ps` shows coturn running?"),
            }
            continue;
        }
        match udp_allocate(addr, &turn.username, &turn.credential).await {
            Ok(relayed) => {
                r.ok(u, &format!("login accepted, relayed address {relayed}"));
                if is_private(relayed.ip()) {
                    r.warn(u, "the relayed address is a private one: set PUBLIC_IP in .env to the server's public address (coturn --external-ip)");
                }
            }
            Err(e) => r.bad(
                u,
                &e,
                "UDP 3478 open? TURN_SECRET the same for relay and coturn? coturn must run with --use-auth-secret / --static-auth-secret: `docker compose logs coturn`",
            ),
        }
    }

    println!();
    if r.failed == 0 {
        println!(
            "All checks passed. Set Settings, Home relay to {url} in each app and try a call."
        );
    } else {
        println!("{} check(s) failed.", r.failed);
        std::process::exit(1);
    }
}
