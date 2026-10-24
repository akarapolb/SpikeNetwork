//! Direct device-to-device connections over the local network (ADR 0027).
//!
//! A message that would go to a device's relay queue is first offered to the device itself, over a
//! TCP connection authenticated with the two devices' keys (`spike_core::direct`). Only an `Ack`
//! from the device counts; anything else sends the same blob to the relay as before. An address is
//! a hint about where to knock, never a reason to trust.

use crate::ops;
use crate::state::Home;
use crate::transport::Target;
use crate::Outcome;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::direct::{self, MAX_FRAME};
use spike_core::identity::DeviceCard;
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const FILE: &str = "direct.json";
const TAG_BLOB: u8 = 1;
const TAG_ACK: u8 = 2;
const TAG_REFUSED: u8 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(6);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const BUSY_WAIT: Duration = Duration::from_millis(1500);
const SKIP_FAILED_FOR: Duration = Duration::from_secs(60);
const MAX_CONNECTIONS: usize = 8;
const MAX_LEARNED: usize = 3;
const MAX_ADDR: usize = 100;
/// Handshakes that fail this many times from one address in a minute are dropped unanswered.
const MAX_BAD_HANDSHAKES: u32 = 10;

/// What the person chose and what we learned, sealed in the home as `direct.json`.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct DirectSettings {
    pub enabled: bool,
    /// The port we listen on. 0 until the first start picks one; then it stays, so an address typed
    /// on another device keeps working.
    pub port: u16,
    /// Where each device last called us from (device id -> `ip:port`, newest first).
    #[serde(default)]
    pub learned: BTreeMap<String, Vec<String>>,
    /// Addresses a person typed for a contact (owner key -> `host:port`).
    #[serde(default)]
    pub typed: BTreeMap<String, Vec<String>>,
}

fn file_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn bad_addrs() -> &'static Mutex<HashMap<String, Instant>> {
    static L: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn load(home: &Home) -> Result<DirectSettings> {
    home.load_json(FILE)
}

fn update(home: &Home, f: impl FnOnce(&mut DirectSettings)) -> Result<DirectSettings> {
    let _g = file_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(home)?;
    f(&mut s);
    home.save_json(FILE, &s)?;
    Ok(s)
}

pub fn enabled(home: &Home) -> bool {
    load(home).map(|s| s.enabled).unwrap_or(false)
}

/// Turn the setting on or off. The listener itself is started by whoever owns the runtime.
pub fn set_enabled(home: &Home, on: bool) -> Result<DirectSettings> {
    update(home, |s| s.enabled = on)
}

/// Remember (or with `None` forget) the address typed for the person with this owner key.
pub fn set_typed_address(home: &Home, owner: &str, addr: Option<&str>) -> Result<()> {
    let addr = match addr.map(str::trim).filter(|a| !a.is_empty()) {
        None => None,
        Some(a) => Some(check_address(a)?),
    };
    update(home, |s| match addr {
        Some(a) => {
            s.typed.insert(owner.to_string(), vec![a]);
        }
        None => {
            s.typed.remove(owner);
        }
    })?;
    Ok(())
}

/// `host:port`, as typed by a person: refuse anything else before it is stored or dialled.
pub fn check_address(a: &str) -> Result<String> {
    let a = a.trim();
    let ok_chars = a
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ".-:[]_".contains(c));
    let Some((host, port)) = a.rsplit_once(':') else {
        bail!("the address must look like 192.168.1.20:8788");
    };
    if a.len() > MAX_ADDR
        || !ok_chars
        || host.trim_matches(['[', ']']).is_empty()
        || port.parse::<u16>().ok().filter(|p| *p != 0).is_none()
    {
        bail!("the address must look like 192.168.1.20:8788");
    }
    Ok(a.to_string())
}

/// The addresses other devices on this network can try for us: the interface that routes to the
/// LAN, plus the port. Nothing is sent to find it.
pub fn local_addresses(port: u16) -> Vec<String> {
    let probe = || -> Option<IpAddr> {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("192.0.2.1:9").ok()?;
        Some(s.local_addr().ok()?.ip())
    };
    probe()
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .map(|ip| vec![SocketAddr::new(ip, port).to_string()])
        .unwrap_or_default()
}

static SENT_DIRECT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SENT_FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RECEIVED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many blobs this process sent directly, had to send through the relay after trying, and
/// received directly. For the status screen and the tests.
pub fn counters() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        SENT_DIRECT.load(Relaxed),
        SENT_FALLBACK.load(Relaxed),
        RECEIVED.load(Relaxed),
    )
}

/// A direct send did not work; the blob goes to the relay. Quiet about "nothing to try".
pub(crate) fn note_fallback(e: &anyhow::Error) {
    use std::sync::atomic::Ordering::Relaxed;
    let msg = format!("{e:#}");
    if !msg.contains("no direct address") {
        SENT_FALLBACK.fetch_add(1, Relaxed);
    }
}

/// The owner key of the person (or of ourselves) that this device belongs to, if we know it.
fn owner_of_device(home: &Home, device_id: &str) -> Result<Option<String>> {
    for c in home.load_contacts()? {
        let id = c.identity()?;
        if id.devices().iter().any(|d| d.device_id() == device_id) {
            return Ok(Some(id.owner_hex()));
        }
    }
    let mine = home.load_device_list()?;
    if mine.verify()?.iter().any(|d| d.device_id() == device_id) {
        return Ok(Some(mine.owner.clone()));
    }
    Ok(None)
}

// ------------------------------------------------------------------ frames

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<()> {
    w.write_all(&(data.len() as u32).to_be_bytes()).await?;
    w.write_all(data).await?;
    w.flush().await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        bail!("frame too large");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

/// The peer answered and said no. Not a network failure: the same address works, the message was not taken.
#[derive(Debug)]
struct Refused(String);
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the other device did not take it: {}", self.0)
    }
}
impl std::error::Error for Refused {}

// ------------------------------------------------------------------ dialling

/// Offer `blob` to `card`'s device directly. `Ok` means the device stored it and said so. Any error
/// means it did not (no address, no route, refused, timeout): the caller then uses the relay.
pub async fn try_send(
    home: &Home,
    me: &spike_core::identity::Identity,
    card: &DeviceCard,
    blob: &[u8],
) -> Result<()> {
    let st = load(home)?;
    if !st.enabled {
        bail!("direct connections are off");
    }
    let dev = card.device_id();
    let mut addrs = st.learned.get(&dev).cloned().unwrap_or_default();
    if let Some(owner) = owner_of_device(home, &dev)? {
        addrs.extend(st.typed.get(&owner).cloned().unwrap_or_default());
    }
    let mut seen = std::collections::HashSet::new();
    addrs.retain(|a| seen.insert(a.clone()));
    if addrs.is_empty() {
        bail!("no direct address known for this device");
    }
    let my_port = if st.port != 0 { st.port } else { 0 };
    let mut last = anyhow!("every known address failed recently");
    for a in addrs {
        let skip = bad_addrs()
            .lock()
            .map(|m| m.get(&a).is_some_and(|t| t.elapsed() < SKIP_FAILED_FOR))
            .unwrap_or(false);
        if skip {
            continue;
        }
        let r = tokio::time::timeout(EXCHANGE_TIMEOUT, exchange(&a, me, my_port, &dev, blob)).await;
        match r {
            Ok(Ok(())) => {
                SENT_DIRECT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut m) = bad_addrs().lock() {
                    m.remove(&a);
                }
                return Ok(());
            }
            Ok(Err(e)) if e.downcast_ref::<Refused>().is_some() => return Err(e),
            Ok(Err(e)) => last = e.context(format!("direct to {a}")),
            Err(_) => last = anyhow!("direct to {a} timed out"),
        }
        if let Ok(mut m) = bad_addrs().lock() {
            m.insert(a, Instant::now());
        }
    }
    Err(last)
}

async fn exchange(
    addr: &str,
    me: &spike_core::identity::Identity,
    my_port: u16,
    expect_device: &str,
    blob: &[u8],
) -> Result<()> {
    let mut tcp = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let target = tokio::net::lookup_host(addr)
            .await?
            .next()
            .ok_or_else(|| anyhow!("no such host"))?;
        Ok::<_, anyhow::Error>(TcpStream::connect(target).await?)
    })
    .await
    .map_err(|_| anyhow!("connecting timed out"))??;
    tcp.set_nodelay(true).ok();
    let (init, m1) = direct::initiate();
    write_frame(&mut tcp, &m1).await?;
    let m2 = read_frame(&mut tcp, 512).await.context("handshake")?;
    let (m3, mut ch) = init.finish(me, my_port, expect_device, &m2)?;
    write_frame(&mut tcp, &m3).await?;
    let mut frame = vec![TAG_BLOB];
    frame.extend_from_slice(blob);
    write_frame(&mut tcp, &ch.seal(&frame)?).await?;
    let reply = ch.open(
        &read_frame(&mut tcp, MAX_FRAME)
            .await
            .context("waiting for the answer")?,
    )?;
    match reply.split_first() {
        Some((&TAG_ACK, _)) => Ok(()),
        Some((&TAG_REFUSED, why)) => Err(Refused(String::from_utf8_lossy(why).into_owned()).into()),
        _ => bail!("unintelligible answer"),
    }
}

// ------------------------------------------------------------------ listening

/// Told about everything a direct delivery produced (after it was stored and acknowledged).
pub type Sink = Arc<dyn Fn(Outcome) + Send + Sync>;

struct Ctx {
    home: Arc<Home>,
    net: Arc<tokio::sync::Mutex<()>>,
    target: Arc<dyn Fn() -> Target + Send + Sync>,
    sink: Sink,
    bad: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

/// A running listener. Dropping it stops the accept loop and every connection.
pub struct Listener {
    pub port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Listen on all interfaces. `net` is the lock that serialises sends and syncs; a delivery waits
/// for it briefly and is refused as busy (so the sender uses the relay) if it cannot get it.
pub async fn listen(
    home: Arc<Home>,
    net: Arc<tokio::sync::Mutex<()>>,
    target: Arc<dyn Fn() -> Target + Send + Sync>,
    sink: Sink,
) -> Result<Listener> {
    let st = load(&home)?;
    let listener = TcpListener::bind(("0.0.0.0", st.port))
        .await
        .with_context(|| {
            format!(
                "could not listen on port {} (is something else using it?)",
                st.port
            )
        })?;
    let port = listener.local_addr()?.port();
    if port != st.port {
        update(&home, |s| s.port = port)?;
    }
    let ctx = Arc::new(Ctx {
        home,
        net,
        target,
        sink,
        bad: Mutex::new(HashMap::new()),
    });
    let task = tokio::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
        // Connections live in this set, so aborting the accept loop drops and ends them all.
        let mut conns = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((tcp, remote)) = accepted else {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    };
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        continue;
                    };
                    if ctx.rate_limited(remote.ip()) {
                        continue;
                    }
                    let ctx = ctx.clone();
                    conns.spawn(async move {
                        let _permit = permit;
                        if let Err(e) = serve(&ctx, tcp, remote).await {
                            #[cfg(debug_assertions)]
                            eprintln!("[direct] {remote}: {e:#}");
                            let _ = e;
                        }
                    });
                }
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
    });
    Ok(Listener { port, task })
}

impl Ctx {
    fn rate_limited(&self, ip: IpAddr) -> bool {
        let Ok(m) = self.bad.lock() else { return false };
        m.get(&ip)
            .is_some_and(|(t, n)| t.elapsed() < Duration::from_secs(60) && *n >= MAX_BAD_HANDSHAKES)
    }

    fn note_bad(&self, ip: IpAddr) {
        if let Ok(mut m) = self.bad.lock() {
            let e = m.entry(ip).or_insert((Instant::now(), 0));
            if e.0.elapsed() >= Duration::from_secs(60) {
                *e = (Instant::now(), 0);
            }
            e.1 += 1;
        }
    }
}

async fn serve(ctx: &Ctx, mut tcp: TcpStream, remote: SocketAddr) -> Result<()> {
    tcp.set_nodelay(true).ok();
    let shake = async {
        let me = ctx.home.load_identity()?;
        let m1 = read_frame(&mut tcp, 64).await?;
        let (resp, m2) = direct::respond(&me, &m1)?;
        write_frame(&mut tcp, &m2).await?;
        let m3 = read_frame(&mut tcp, 512).await?;
        resp.finish(&m3).map_err(anyhow::Error::from)
    };
    let shaken = tokio::time::timeout(HANDSHAKE_TIMEOUT, shake).await;
    let (peer, mut ch) = match shaken {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            ctx.note_bad(remote.ip());
            return Err(e);
        }
        Err(_) => {
            ctx.note_bad(remote.ip());
            bail!("handshake timed out");
        }
    };
    // Authenticated, but is it someone we talk to? Strangers get nothing, not even a refusal.
    if owner_of_device(&ctx.home, &peer.device_id)?.is_none() {
        ctx.note_bad(remote.ip());
        bail!("a device that is in nobody's device list");
    }
    if peer.port != 0 {
        let addr = SocketAddr::new(remote.ip(), peer.port).to_string();
        let dev = peer.device_id.clone();
        update(&ctx.home, |s| {
            let v = s.learned.entry(dev).or_default();
            v.retain(|a| *a != addr);
            v.insert(0, addr);
            v.truncate(MAX_LEARNED);
        })?;
    }
    loop {
        let frame = match tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut tcp, MAX_FRAME)).await
        {
            Err(_) => return Ok(()),
            Ok(Err(_)) => return Ok(()),
            Ok(Ok(f)) => f,
        };
        let plain = ch.open(&frame)?;
        let reply = match plain.split_first() {
            Some((&TAG_BLOB, blob)) => deliver(ctx, blob).await,
            _ => bail!("unknown frame"),
        };
        write_frame(&mut tcp, &ch.seal(&reply.0)?).await?;
        if let Some(after) = reply.1 {
            after.await;
        }
    }
}

type After = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Store one blob. Returns the frame to answer with, and whatever is still owed after the answer
/// went out (it keeps the lock until then, so nothing else touches the ratchets meanwhile).
async fn deliver(ctx: &Ctx, blob: &[u8]) -> (Vec<u8>, Option<After>) {
    let refuse = |why: &str| {
        let mut f = vec![TAG_REFUSED];
        f.extend_from_slice(why.as_bytes());
        (f, None)
    };
    let Ok(guard) = tokio::time::timeout(BUSY_WAIT, ctx.net.clone().lock_owned()).await else {
        return refuse("busy");
    };
    let base = (ctx.target)();
    let received = match ops::receive_direct(&ctx.home, &base, blob).await {
        Ok(r) => r,
        Err(e) => return refuse(&format!("{e:#}").chars().take(100).collect::<String>()),
    };
    if matches!(received.outcome(), Outcome::Invalid) {
        return refuse("not readable");
    }
    let home = ctx.home.clone();
    let sink = ctx.sink.clone();
    RECEIVED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let after: After = Box::pin(async move {
        let _guard = guard;
        if let Err(e) = ops::finish_direct(&home, &base, received, |o| sink(o)).await {
            eprintln!("note: could not finish a direct delivery: {e:#}");
        }
    });
    (vec![TAG_ACK], Some(after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_checked() {
        for ok in [
            "192.168.1.20:8788",
            "pc.local:1",
            "[fe80::1]:9",
            "host_a-b:65535",
        ] {
            assert!(check_address(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "192.168.1.20",
            "192.168.1.20:0",
            "192.168.1.20:99999",
            ":80",
            "a b:80",
            "http://x:80",
            "x:80/../../",
            &format!("{}:80", "a".repeat(200)),
        ] {
            assert!(check_address(bad).is_err(), "{bad}");
        }
    }
}
