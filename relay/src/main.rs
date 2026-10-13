use clap::Parser;
use spike_relay::{serve, Config};
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
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let a = Args::parse();
    let listener = tokio::net::TcpListener::bind(&a.listen).await?;
    let storage = match &a.db {
        Some(p) => format!("durable queues in {}", p.display()),
        None => "IN-MEMORY queues: lost on restart (use --db for durability)".to_string(),
    };
    eprintln!(
        "spike-relay listening on {} ({storage}; no content logging)",
        listener.local_addr()?
    );
    let cfg = Config {
        ttl: Duration::from_secs(a.ttl_secs),
        queue_cap: a.queue_cap,
        db_path: a.db,
        ..Config::default()
    };
    serve(listener, cfg).await
}
