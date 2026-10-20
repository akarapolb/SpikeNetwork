//! Abuse limits for a relay that strangers can reach (ADR 0022).
//!
//! The relay cannot see who sends a message (it only knows the recipient), so limits attach to
//! what it can see: the network address of a connection, and how fast one connection asks for
//! things. Behind a reverse proxy every connection comes from the proxy's address, so the real
//! client address is read from `X-Forwarded-For`, but only when the connection really comes from
//! a proxy the operator listed. Anyone else could write that header themselves.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// An IP network such as `10.0.0.0/8` or a single address (`127.0.0.1`, meaning /32).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    ip: IpAddr,
    prefix: u8,
}

/// IPv4 addresses that arrive as IPv4-mapped IPv6 (`::ffff:a.b.c.d`) are the same address.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

impl Cidr {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let ip = canonical(
            addr.trim()
                .parse::<IpAddr>()
                .map_err(|_| format!("'{s}' is not an IP address or network"))?,
        );
        let max = if ip.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("'{s}': the prefix must be 0 to {max}"))?,
            None => max,
        };
        Ok(Self { ip, prefix })
    }

    pub fn contains(&self, other: IpAddr) -> bool {
        let bits = |ip: IpAddr| -> u128 {
            match canonical(ip) {
                IpAddr::V4(a) => u128::from(u32::from(a)),
                IpAddr::V6(a) => u128::from(a),
            }
        };
        if self.ip.is_ipv4() != canonical(other).is_ipv4() {
            return false;
        }
        let width = if self.ip.is_ipv4() { 32 } else { 128 };
        let shift = width - u32::from(self.prefix);
        if shift >= 128 {
            return true; // /0 matches everything of that family
        }
        (bits(self.ip) >> shift) == (bits(other) >> shift)
    }
}

pub fn is_trusted(ip: IpAddr, trusted: &[Cidr]) -> bool {
    trusted.iter().any(|c| c.contains(ip))
}

/// The address a connection really comes from. If the TCP peer is one of the operator's trusted
/// proxies, take the rightmost `X-Forwarded-For` entry that is not itself a trusted proxy (the
/// proxy appends the address it saw, so entries on the left may have been written by the client).
/// Otherwise the header is ignored: it is just text a client chose.
pub fn client_ip(peer: IpAddr, forwarded_for: Option<&str>, trusted: &[Cidr]) -> IpAddr {
    let peer = canonical(peer);
    if !is_trusted(peer, trusted) {
        return peer;
    }
    let Some(header) = forwarded_for else {
        return peer;
    };
    header
        .split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .map(canonical)
        .rev()
        .find(|ip| !is_trusted(*ip, trusted))
        .unwrap_or(peer)
}

/// Why a connection was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Too many connections open from this address at once.
    TooManyConnections,
    /// Too many new connections from this address in the last minute.
    TooFast,
}

struct Entry {
    conns: usize,
    window_start: Instant,
    opened_in_window: u32,
}

/// Connection counts and rates per client address. `0` for either limit means unlimited.
pub struct IpLimits {
    inner: Mutex<HashMap<IpAddr, Entry>>,
    max_conns: usize,
    per_minute: u32,
}

const WINDOW: Duration = Duration::from_secs(60);

impl IpLimits {
    pub fn new(max_conns: usize, per_minute: u32) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            max_conns,
            per_minute,
        })
    }

    /// Admit a new connection from `ip`, or say why not. The returned guard frees the slot when dropped.
    pub fn admit(self: &Arc<Self>, ip: IpAddr) -> Result<Admission, Refused> {
        let ip = canonical(ip);
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        let e = map.entry(ip).or_insert(Entry {
            conns: 0,
            window_start: now,
            opened_in_window: 0,
        });
        if now.duration_since(e.window_start) >= WINDOW {
            e.window_start = now;
            e.opened_in_window = 0;
        }
        if self.max_conns > 0 && e.conns >= self.max_conns {
            return Err(Refused::TooManyConnections);
        }
        if self.per_minute > 0 && e.opened_in_window >= self.per_minute {
            return Err(Refused::TooFast);
        }
        e.conns += 1;
        e.opened_in_window += 1;
        Ok(Admission {
            limits: self.clone(),
            ip,
        })
    }

    fn release(&self, ip: IpAddr) {
        let mut map = self.inner.lock().unwrap();
        if let Some(e) = map.get_mut(&ip) {
            e.conns = e.conns.saturating_sub(1);
        }
    }

    /// Forget addresses with no open connection and an expired window, so the table cannot grow without bound.
    pub fn purge(&self) {
        let now = Instant::now();
        self.inner
            .lock()
            .unwrap()
            .retain(|_, e| e.conns > 0 || now.duration_since(e.window_start) < WINDOW);
    }

    /// How many addresses are being tracked (for tests and metrics).
    pub fn tracked(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

/// A claimed connection slot. Dropping it gives the slot back.
pub struct Admission {
    limits: Arc<IpLimits>,
    ip: IpAddr,
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.limits.release(self.ip);
    }
}

/// A token bucket: `rate` tokens a second, up to `burst` saved up. A rate of 0 never limits.
pub struct TokenBucket {
    tokens: f64,
    burst: f64,
    rate: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self {
            tokens: burst,
            burst,
            rate: rate_per_sec,
            last: Instant::now(),
        }
    }

    /// Take `cost` tokens if there are enough.
    pub fn allow(&mut self, cost: f64) -> bool {
        if self.rate <= 0.0 {
            return true;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn networks_match_by_prefix() {
        let net = Cidr::parse("10.1.0.0/16").unwrap();
        assert!(net.contains(ip("10.1.200.3")) && !net.contains(ip("10.2.0.1")));
        assert!(Cidr::parse("127.0.0.1").unwrap().contains(ip("127.0.0.1")));
        assert!(!Cidr::parse("127.0.0.1").unwrap().contains(ip("127.0.0.2")));
        assert!(Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains(ip("203.0.113.7")));
        assert!(
            !Cidr::parse("0.0.0.0/0").unwrap().contains(ip("::1")),
            "families do not mix"
        );
        assert!(Cidr::parse("fd00::/8").unwrap().contains(ip("fd12::1")));
        // An IPv4 address that arrives mapped into IPv6 is still that IPv4 address.
        assert!(Cidr::parse("192.168.0.0/16")
            .unwrap()
            .contains(ip("::ffff:192.168.1.1")));
        for bad in ["", "nope", "10.0.0.0/33", "10.0.0.0/x", "::1/129"] {
            assert!(Cidr::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_forwarded_header_is_trusted_only_from_a_trusted_proxy() {
        let proxy = [Cidr::parse("10.0.0.5").unwrap()];
        // From the proxy: the rightmost entry that is not the proxy itself.
        assert_eq!(
            client_ip(ip("10.0.0.5"), Some("203.0.113.9"), &proxy),
            ip("203.0.113.9")
        );
        assert_eq!(
            client_ip(ip("10.0.0.5"), Some("1.1.1.1, 203.0.113.9"), &proxy),
            ip("203.0.113.9")
        );
        assert_eq!(
            client_ip(
                ip("10.0.0.5"),
                Some("1.1.1.1, 203.0.113.9, 10.0.0.5"),
                &proxy
            ),
            ip("203.0.113.9")
        );
        // No header, or only garbage: fall back to the proxy's own address.
        assert_eq!(client_ip(ip("10.0.0.5"), None, &proxy), ip("10.0.0.5"));
        assert_eq!(
            client_ip(ip("10.0.0.5"), Some("not an ip"), &proxy),
            ip("10.0.0.5")
        );
        // From anyone else the header is ignored: it would let a client pick its own address.
        assert_eq!(
            client_ip(ip("198.51.100.4"), Some("203.0.113.9"), &proxy),
            ip("198.51.100.4")
        );
        assert_eq!(
            client_ip(ip("198.51.100.4"), Some("203.0.113.9"), &[]),
            ip("198.51.100.4")
        );
    }

    #[test]
    fn connections_are_capped_per_address_and_freed_on_drop() {
        let l = IpLimits::new(2, 0);
        let a = l.admit(ip("1.2.3.4")).unwrap();
        let _b = l.admit(ip("1.2.3.4")).unwrap();
        assert_eq!(
            l.admit(ip("1.2.3.4")).err(),
            Some(Refused::TooManyConnections)
        );
        // Another address is unaffected, and so is the same address once a slot frees.
        let _other = l.admit(ip("5.6.7.8")).unwrap();
        drop(a);
        let _c = l.admit(ip("1.2.3.4")).unwrap();
        // IPv4 and its IPv6-mapped form share one budget.
        assert_eq!(
            l.admit(ip("::ffff:1.2.3.4")).err(),
            Some(Refused::TooManyConnections)
        );
    }

    #[test]
    fn new_connections_are_rate_limited_per_minute() {
        let l = IpLimits::new(0, 3);
        for _ in 0..3 {
            drop(l.admit(ip("9.9.9.9")).unwrap()); // opened and closed right away still counts
        }
        assert_eq!(l.admit(ip("9.9.9.9")).err(), Some(Refused::TooFast));
        assert!(l.admit(ip("9.9.9.10")).is_ok());
        // Unlimited when both limits are 0.
        let free = IpLimits::new(0, 0);
        let guards: Vec<_> = (0..500)
            .map(|_| free.admit(ip("9.9.9.9")).unwrap())
            .collect();
        assert_eq!(guards.len(), 500);
    }

    #[test]
    fn idle_addresses_are_forgotten() {
        let l = IpLimits::new(5, 5);
        drop(l.admit(ip("1.1.1.1")).unwrap());
        let held = l.admit(ip("2.2.2.2")).unwrap();
        assert_eq!(l.tracked(), 2);
        l.purge(); // windows have not expired yet: everything is kept
        assert_eq!(l.tracked(), 2);
        drop(held);
    }

    #[test]
    fn the_token_bucket_allows_a_burst_then_the_steady_rate() {
        let mut b = TokenBucket::new(1000.0, 5.0);
        assert!((0..5).all(|_| b.allow(1.0)), "the burst");
        assert!(!b.allow(1.0), "then empty");
        std::thread::sleep(Duration::from_millis(30));
        assert!(b.allow(1.0), "refilled at the steady rate");
        assert!(
            !b.allow(100.0),
            "a request can cost more than the bucket holds"
        );
        let mut free = TokenBucket::new(0.0, 0.0);
        assert!((0..1000).all(|_| free.allow(1.0)), "rate 0 never limits");
    }
}
