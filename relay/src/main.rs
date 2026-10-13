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
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let a = Args::parse();
    let listener = tokio::net::TcpListener::bind(&a.listen).await?;
    eprintln!(
        "spike-relay listening on {} (in-memory queues, no content logging)",
        listener.local_addr()?
    );
    let cfg = Config {
        ttl: Duration::from_secs(a.ttl_secs),
        queue_cap: a.queue_cap,
        ..Config::default()
    };
    serve(listener, cfg).await
}
