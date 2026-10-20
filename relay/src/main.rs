use clap::Parser;
use spike_relay::{limits::Cidr, serve_with_metrics, tls::TlsFiles, Config};
use std::time::Duration;

/// Spike Network relay. Speaks plain WebSocket; terminate TLS in front of it.
#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: String,
    /// Seconds an unacknowledged message is kept.
    #[arg(long, default_value_t = 7 * 24 * 3600)]
    ttl_secs: u64,
    #[arg(long, default_value_t = 1000)]
    queue_cap: usize,
    /// SQLite file for queued messages and prekeys. Without it everything is
    /// kept in memory and lost on restart.
    #[arg(long, env = "SPIKE_RELAY_DB")]
    db: Option<std::path::PathBuf>,
    /// PEM certificate chain; with --tls-key the relay serves wss://.
    #[arg(long, requires = "tls_key")]
    tls_cert: Option<std::path::PathBuf>,
    /// PEM private key for --tls-cert.
    #[arg(long, requires = "tls_cert")]
    tls_key: Option<std::path::PathBuf>,
    /// Allow plain WebSocket on a non-loopback address (only behind a TLS-terminating proxy).
    #[arg(long)]
    insecure_no_tls: bool,
    /// Connections one client address may hold open at once (0 = unlimited).
    #[arg(long, default_value_t = 64)]
    max_conns_per_ip: usize,
    /// New connections one client address may open per minute (0 = unlimited).
    #[arg(long, default_value_t = 300)]
    conn_rate_per_ip: u32,
    /// Requests per second one connection may sustain (0 = unlimited).
    #[arg(long, default_value_t = 50.0)]
    req_rate: f64,
    /// Requests one connection may make in a burst.
    #[arg(long, default_value_t = 200.0)]
    req_burst: f64,
    /// Reverse proxy address or network (e.g. 10.0.0.0/8) whose X-Forwarded-For is believed.
    /// Repeat or comma-separate. Without it the header is ignored.
    #[arg(long, value_delimiter = ',', value_parser = Cidr::parse)]
    trusted_proxy: Vec<Cidr>,
    /// Serve /healthz and /metrics (plain HTTP, content-free numbers) on this address.
    #[arg(long)]
    metrics_listen: Option<String>,
    /// Require `Authorization: Bearer <token>` for /metrics. Needed on a non-loopback address.
    #[arg(long, env = "SPIKE_RELAY_METRICS_TOKEN")]
    metrics_token: Option<String>,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let a = Args::parse();
    let listener = tokio::net::TcpListener::bind(&a.listen).await?;
    let tls = a
        .tls_cert
        .clone()
        .zip(a.tls_key.clone())
        .map(|(cert, key)| TlsFiles { cert, key });
    if tls.is_none() && !listener.local_addr()?.ip().is_loopback() && !a.insecure_no_tls {
        eprintln!(
            "refusing to serve plain WebSocket on {}: pass --tls-cert/--tls-key, listen on loopback, \
             or pass --insecure-no-tls if a proxy terminates TLS in front",
            listener.local_addr()?
        );
        std::process::exit(2);
    }
    let transport = if tls.is_some() {
        "wss (TLS)"
    } else {
        "plain ws"
    };
    let storage = match &a.db {
        Some(p) => format!("durable queues in {}", p.display()),
        None => "IN-MEMORY queues: lost on restart (use --db for durability)".to_string(),
    };
    eprintln!(
        "spike-relay listening on {} [{transport}] ({storage}; no content logging)",
        listener.local_addr()?
    );
    let metrics_listener = match &a.metrics_listen {
        Some(addr) => {
            let l = tokio::net::TcpListener::bind(addr).await?;
            if !l.local_addr()?.ip().is_loopback() && a.metrics_token.is_none() {
                eprintln!(
                    "refusing to serve /metrics on {} without --metrics-token",
                    l.local_addr()?
                );
                std::process::exit(2);
            }
            eprintln!("spike-relay /healthz and /metrics on {}", l.local_addr()?);
            Some(l)
        }
        None => None,
    };
    let cfg = Config {
        max_conns_per_ip: a.max_conns_per_ip,
        conn_rate_per_ip: a.conn_rate_per_ip,
        req_rate: a.req_rate,
        req_burst: a.req_burst,
        trusted_proxies: a.trusted_proxy,
        metrics_token: a.metrics_token,
        ttl: Duration::from_secs(a.ttl_secs),
        queue_cap: a.queue_cap,
        db_path: a.db,
        tls,
        ..Config::default()
    };
    serve_with_metrics(listener, metrics_listener, cfg).await
}
