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
    /// The name clients dial this relay by (`relay.example` or `relay.example:8443`). Needed only when a
    /// reverse proxy rewrites the Host header; otherwise the Host header is used.
    #[arg(long)]
    auth_audience: Option<String>,
    /// Largest entry in a group mailbox, in bytes.
    #[arg(long, default_value_t = 256 * 1024)]
    group_max_entry: usize,
    /// Days a group message is kept.
    #[arg(long, default_value_t = 30)]
    group_message_days: u64,
    /// Days a group commit is kept, and a mailbox after its last append.
    #[arg(long, default_value_t = 180)]
    group_handshake_days: u64,
    /// Bytes one group mailbox may hold; past it the oldest messages (never commits) are dropped.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    group_max_bytes: usize,
    /// Group mailboxes one device may have created.
    #[arg(long, default_value_t = 200)]
    group_max_mailboxes_per_device: usize,
    /// Group mailboxes one connection may be subscribed to.
    #[arg(long, default_value_t = 256)]
    max_group_subs_per_conn: usize,
    /// A TURN server this relay's operator runs, for calls between devices that cannot reach each other
    /// directly (ADR 0028). Repeat or comma-separate: `turn:turn.example:3478`, `turns:turn.example:5349`.
    /// Needs --turn-secret.
    #[arg(long, value_delimiter = ',', requires = "turn_secret")]
    turn_url: Vec<String>,
    /// The secret the TURN server shares with this relay (coturn `static-auth-secret`). Signed-in devices get
    /// credentials derived from it that expire after --turn-ttl-secs.
    #[arg(long, env = "SPIKE_RELAY_TURN_SECRET", hide_env_values = true)]
    turn_secret: Option<String>,
    /// Seconds an issued TURN credential is valid.
    #[arg(long, default_value_t = 3600)]
    turn_ttl_secs: u64,
    /// Do not send wake-up pushes to the UnifiedPush endpoints devices register (ADR 0028). By default the relay
    /// sends one content-free POST to a registered https endpoint when a message waits for a device that is offline.
    #[arg(long)]
    no_push: bool,
    /// Also allow http:// and private addresses for push endpoints. For tests and local setups only: a push endpoint
    /// is a URL a client chose, and the relay must not be made to call into its own network.
    #[arg(long)]
    push_allow_insecure: bool,
    /// Seconds between wake-up pushes to one device.
    #[arg(long, default_value_t = 5)]
    push_min_interval_secs: u64,
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
        auth_audience: a.auth_audience,
        ttl: Duration::from_secs(a.ttl_secs),
        queue_cap: a.queue_cap,
        group_max_entry: a.group_max_entry,
        group_message_ttl: Duration::from_secs(a.group_message_days * 24 * 3600),
        group_handshake_ttl: Duration::from_secs(a.group_handshake_days * 24 * 3600),
        group_max_bytes: a.group_max_bytes,
        group_max_mailboxes_per_device: a.group_max_mailboxes_per_device,
        max_group_subs_per_conn: a.max_group_subs_per_conn,
        turn_urls: a.turn_url,
        turn_secret: a.turn_secret,
        turn_ttl: Duration::from_secs(a.turn_ttl_secs),
        push_enabled: !a.no_push,
        push_allow_insecure: a.push_allow_insecure,
        push_min_interval: Duration::from_secs(a.push_min_interval_secs),
        db_path: a.db,
        tls,
        ..Config::default()
    };
    serve_with_metrics(listener, metrics_listener, cfg).await
}
