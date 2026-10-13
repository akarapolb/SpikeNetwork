use clap::Parser;
use spike_relay::{serve, tls::TlsFiles, Config};
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
    let cfg = Config {
        ttl: Duration::from_secs(a.ttl_secs),
        queue_cap: a.queue_cap,
        db_path: a.db,
        tls,
        ..Config::default()
    };
    serve(listener, cfg).await
}
