//! Wake-up pushes through UnifiedPush (ADR 0028): a device that is not connected can register the URL of a push
//! endpoint (its own ntfy topic, for example), and when a message is queued for it the relay sends one tiny, empty-of-
//! content POST there, so the phone's push app wakes the Spike app, which then connects and reads. No Google or Apple
//! service is involved, and the push says nothing about who wrote, only that something is waiting.
//!
//! A relay that makes HTTP requests to addresses its clients choose is a gift to an attacker, so this is deliberately
//! narrow: https only (a test switch allows http), no credentials in the URL, a short length, the host resolved by us
//! and refused unless every address is public (so it cannot be pointed at the relay's own network or a cloud metadata
//! service), the connection made to that resolved address (so DNS cannot change its mind afterwards), no redirects, small
//! timeouts, a handful at a time, and at most one push per device every few seconds however many messages arrive.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Longest endpoint URL accepted.
pub const MAX_URL: usize = 512;

/// What a push carries. Not a secret and not derived from anything: it only says "wake up".
pub const BODY: &[u8] = b"spike";

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Endpoint {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub target: String,
}

fn ok_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '-'
}

/// Parse and check an endpoint URL. `allow_insecure` (tests and local setups) also allows `http` and private addresses.
pub fn parse(url: &str, allow_insecure: bool) -> Result<Endpoint, &'static str> {
    if url.len() > MAX_URL || url.is_empty() {
        return Err("push URL is empty or too long");
    }
    if url.chars().any(|c| c.is_control() || c == ' ' || c == '\\') {
        return Err("push URL has characters that are not allowed");
    }
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let (true, Some(r)) = (allow_insecure, url.strip_prefix("http://")) {
        (false, r)
    } else {
        return Err("push URL must start with https://");
    };
    let (authority, target) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    if authority.contains('@') || authority.is_empty() {
        return Err("push URL must not carry a user name or password");
    }
    if target.contains('#') {
        return Err("push URL must not have a fragment");
    }
    let (host, port) = if let Some(h) = authority.strip_prefix('[') {
        // [v6]:port
        let (h, p) = h.split_once(']').ok_or("push URL has a bad address")?;
        (h.to_string(), p.strip_prefix(':').map(str::to_string))
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        (h.to_string(), Some(p.to_string()))
    } else {
        (authority.to_string(), None)
    };
    let port: u16 = match port {
        Some(p) => p.parse().map_err(|_| "push URL has a bad port")?,
        None => {
            if tls {
                443
            } else {
                80
            }
        }
    };
    if port == 0 {
        return Err("push URL has a bad port");
    }
    let is_ip = host.parse::<IpAddr>().is_ok();
    if host.is_empty() || (!is_ip && !host.chars().all(ok_host_char)) {
        return Err("push URL has a bad host name");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !allow_insecure && !is_public(&ip) {
            return Err("push URL points at a private address");
        }
    } else if !allow_insecure && (!host.contains('.') || host.ends_with(".local") || host.ends_with(".internal") || host.ends_with(".localhost")) {
        return Err("push URL must name a public host");
    }
    Ok(Endpoint { tls, host, port, target })
}

/// True for an address that is ordinary and reachable on the public Internet.
pub fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => public_v4(a),
        IpAddr::V6(a) => public_v6(a),
    }
}

fn public_v4(a: &Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_private()
        || a.is_loopback()
        || a.is_link_local()
        || a.is_broadcast()
        || a.is_unspecified()
        || a.is_multicast()
        || a.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (64..128).contains(&o[1])) // carrier-grade NAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
        || o[0] >= 240) // reserved
}

fn public_v6(a: &Ipv6Addr) -> bool {
    if let Some(v4) = a.to_ipv4_mapped() {
        return public_v4(&v4);
    }
    let s = a.segments();
    !(a.is_loopback()
        || a.is_unspecified()
        || a.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // unique local
        || (s[0] & 0xffc0) == 0xfe80 // link local
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x64 && s[1] == 0xff9b)) // NAT64 can reach IPv4 private space
}

/// What came of one push attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// The endpoint says it is gone (404 or 410): forget it.
    Gone,
    Failed,
}

/// Send one wake-up to `ep`. Never panics, never takes longer than about ten seconds.
pub async fn send(ep: &Endpoint, allow_insecure: bool) -> Outcome {
    match tokio::time::timeout(Duration::from_secs(10), send_inner(ep, allow_insecure)).await {
        Ok(o) => o,
        Err(_) => Outcome::Failed,
    }
}

async fn send_inner(ep: &Endpoint, allow_insecure: bool) -> Outcome {
    // We resolve the name and keep only public addresses: connecting to what we checked, not to whatever the name says later.
    let Ok(addrs) = tokio::net::lookup_host((ep.host.as_str(), ep.port)).await else { return Outcome::Failed };
    let addrs: Vec<SocketAddr> = addrs.collect();
    if addrs.is_empty() || (!allow_insecure && addrs.iter().any(|a| !is_public(&a.ip()))) {
        return Outcome::Failed;
    }
    let Ok(Ok(tcp)) = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addrs[0])).await else {
        return Outcome::Failed;
    };
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: spike-relay\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nTTL: 60\r\nConnection: close\r\n\r\n",
        ep.target,
        host_header(ep),
        BODY.len()
    );
    let mut head = request.into_bytes();
    head.extend_from_slice(BODY);
    let status = if ep.tls {
        let Some(connector) = tls_connector() else { return Outcome::Failed };
        let Ok(name) = rustls_pki_types::ServerName::try_from(ep.host.clone()) else { return Outcome::Failed };
        let Ok(Ok(mut tls)) = tokio::time::timeout(Duration::from_secs(5), connector.connect(name, tcp)).await else {
            return Outcome::Failed;
        };
        if tls.write_all(&head).await.is_err() {
            return Outcome::Failed;
        }
        read_status(&mut tls).await
    } else {
        let mut tcp = tcp;
        if tcp.write_all(&head).await.is_err() {
            return Outcome::Failed;
        }
        read_status(&mut tcp).await
    };
    match status {
        Some(200..=299) => Outcome::Delivered,
        Some(404) | Some(410) => Outcome::Gone,
        _ => Outcome::Failed,
    }
}

fn host_header(ep: &Endpoint) -> String {
    let host = if ep.host.contains(':') { format!("[{}]", ep.host) } else { ep.host.clone() };
    let default = if ep.tls { 443 } else { 80 };
    if ep.port == default {
        host
    } else {
        format!("{host}:{}", ep.port)
    }
}

/// The status code of the response, reading no more than a small header.
async fn read_status<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Option<u16> {
    let mut buf = vec![0u8; 512];
    let mut n = 0;
    while n < buf.len() {
        let k = r.read(&mut buf[n..]).await.ok()?;
        if k == 0 {
            break;
        }
        n += k;
        if buf[..n].windows(2).any(|w| w == b"\r\n") {
            break;
        }
    }
    let line = std::str::from_utf8(&buf[..n]).ok()?.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

fn tls_connector() -> Option<tokio_rustls::TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Some(tokio_rustls::TlsConnector::from(Arc::new(cfg)))
}

/// Decides whether a push is due for a device: at most one per `min_interval`, however many messages arrive.
pub struct Throttle {
    last: Mutex<HashMap<String, Instant>>,
    min_interval: Duration,
}

impl Throttle {
    pub fn new(min_interval: Duration) -> Self {
        Self { last: Mutex::new(HashMap::new()), min_interval }
    }

    pub fn due(&self, device: &str) -> bool {
        let mut m = self.last.lock().unwrap();
        let now = Instant::now();
        if m.len() > 4096 {
            m.retain(|_, t| now.duration_since(*t) < self.min_interval);
        }
        match m.get(device) {
            Some(t) if now.duration_since(*t) < self.min_interval => false,
            _ => {
                m.insert(device.to_string(), now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_endpoints_parse() {
        let e = parse("https://ntfy.example.org/upabc123?x=1", false).unwrap();
        assert_eq!(e, Endpoint { tls: true, host: "ntfy.example.org".into(), port: 443, target: "/upabc123?x=1".into() });
        assert_eq!(parse("https://push.example.org:8443/t", false).unwrap().port, 8443);
        assert_eq!(parse("https://push.example.org", false).unwrap().target, "/");
        assert_eq!(parse("https://push.example.org?a=b", false).unwrap().target, "/?a=b");
        assert_eq!(parse("https://93.184.216.34/t", false).unwrap().host, "93.184.216.34");
    }

    #[test]
    fn dangerous_or_malformed_endpoints_are_refused() {
        for bad in [
            "",
            "http://ntfy.example.org/t",
            "ftp://ntfy.example.org/t",
            "https://user:pw@ntfy.example.org/t",
            "https://ntfy.example.org/t#frag",
            "https://ntfy.example.org/a b",
            "https://ntfy.example.org:0/t",
            "https://ntfy.example.org:99999/t",
            "https://localhost/t",
            "https://intranet/t",
            "https://printer.local/t",
            "https://metadata.internal/t",
            "https://127.0.0.1/t",
            "https://10.0.0.5/t",
            "https://192.168.1.1/t",
            "https://172.16.0.1/t",
            "https://169.254.169.254/latest/meta-data",
            "https://100.64.0.1/t",
            "https://0.0.0.0/t",
            "https://[::1]/t",
            "https://[fe80::1]/t",
            "https://[fd00::1]/t",
            "https://[::ffff:10.0.0.1]/t",
            "https://[64:ff9b::a00:1]/t",
            "https://exa_mple.org/t",
        ] {
            assert!(parse(bad, false).is_err(), "should refuse {bad}");
        }
        let long = format!("https://ntfy.example.org/{}", "a".repeat(MAX_URL));
        assert!(parse(&long, false).is_err());
    }

    #[test]
    fn the_test_switch_allows_http_and_private_addresses() {
        assert!(parse("http://127.0.0.1:8080/t", true).is_ok());
        assert!(parse("http://localhost:8080/t", true).is_ok());
        assert!(parse("http://127.0.0.1:8080/t", false).is_err());
    }

    #[test]
    fn public_addresses_are_told_from_private_ones() {
        for ok in ["93.184.216.34", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public(&ok.parse().unwrap()), "{ok}");
        }
        for no in ["127.0.0.1", "10.1.2.3", "172.31.0.1", "192.168.0.9", "169.254.1.1", "100.100.0.1", "0.1.2.3", "224.0.0.1", "255.255.255.255", "::1", "::", "fc00::1", "fe80::2", "::ffff:127.0.0.1"] {
            assert!(!is_public(&no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn at_most_one_push_per_interval_per_device() {
        let t = Throttle::new(Duration::from_millis(200));
        assert!(t.due("a"));
        assert!(!t.due("a"));
        assert!(t.due("b"));
        std::thread::sleep(Duration::from_millis(250));
        assert!(t.due("a"));
    }
}
