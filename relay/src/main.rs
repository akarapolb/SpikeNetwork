use clap::{Parser, Subcommand};
use spike_relay::store::{tenants, Store};
use spike_relay::{limits::Cidr, serve_with_metrics, tls::TlsFiles, Config};
use std::time::Duration;

/// Spike Network relay. Speaks plain WebSocket; terminate TLS in front of it.
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Enrolled mode (ADR 0023): only devices an organisation enrolled may sign in.
    #[arg(long)]
    enrolled: bool,
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

#[derive(Subcommand)]
enum Cmd {
    /// Organisations on this relay (ADR 0023, ADR 0035). Acts on the --db file directly; run it on the
    /// relay machine. The relay may keep running.
    Tenant {
        #[command(subcommand)]
        op: TenantOp,
    },
}

#[derive(Subcommand)]
enum TenantOp {
    /// Create an organisation with the public half of its admin key.
    Create {
        id: String,
        #[arg(long)]
        name: String,
        /// The admin public key (64 hex digits), from the organisation's console.
        #[arg(long)]
        admin_pub: String,
        /// Most devices (0 = unlimited).
        #[arg(long, default_value_t = 50)]
        max_devices: u64,
        /// Bytes held for the organisation's devices, in MB (0 = unlimited).
        #[arg(long, default_value_t = 10_240)]
        storage_mb: u64,
    },
    /// List organisations with their totals.
    List,
    /// Change an organisation's limits.
    Limits {
        id: String,
        #[arg(long)]
        max_devices: Option<u64>,
        #[arg(long)]
        storage_mb: Option<u64>,
    },
    /// Refuse all of an organisation's devices until resumed.
    Suspend { id: String },
    Resume { id: String },
    /// Delete an organisation, its enrolments and its devices' queues.
    Delete { id: String },
    /// Replace the admin public key (a lost or rotated key). Old console sessions end.
    RotateAdmin {
        id: String,
        #[arg(long)]
        admin_pub: String,
    },
}

fn tenant_cmd(db: Option<&std::path::Path>, op: TenantOp) -> Result<(), String> {
    let db = db.ok_or("tenant commands need --db (the relay's database file)")?;
    let s = Store::open(Some(db)).map_err(|e| e.to_string())?;
    let e = |e: rusqlite::Error| e.to_string();
    let key = |k: &str| -> Result<String, String> {
        let k = k.trim().to_ascii_lowercase();
        match hex::decode(&k) {
            Ok(b) if b.len() == 32 => Ok(k),
            _ => Err("--admin-pub must be 64 hex digits".into()),
        }
    };
    let found = |ok: bool, id: &str| if ok { Ok(()) } else { Err(format!("no organisation {id}")) };
    match op {
        TenantOp::Create { id, name, admin_pub, max_devices, storage_mb } => {
            if !spike_core::tenant::is_valid_tenant_id(&id) {
                return Err("the id must be 1 to 32 of a-z, 0-9 and -".into());
            }
            if !s.tenant_create(&id, name.trim(), &key(&admin_pub)?, max_devices, storage_mb * 1024 * 1024).map_err(e)? {
                return Err(format!("organisation {id} already exists"));
            }
            println!("created {id}");
        }
        TenantOp::List => {
            for t in s.tenants().map_err(e)? {
                let u = s.tenant_usage(&t.id).map_err(e)?.unwrap_or_default();
                println!(
                    "{}	{}	{}	devices {}/{}	stored {} of {} bytes	rules v{}",
                    t.id, t.name, t.state, u.devices, t.max_devices, u.stored_bytes, t.storage_bytes, t.rules_version
                );
            }
        }
        TenantOp::Limits { id, max_devices, storage_mb } => {
            found(s.tenant_set_limits(&id, max_devices, storage_mb.map(|m| m * 1024 * 1024)).map_err(e)?, &id)?
        }
        TenantOp::Suspend { id } => found(s.tenant_set_state(&id, tenants::SUSPENDED).map_err(e)?, &id)?,
        TenantOp::Resume { id } => found(s.tenant_set_state(&id, tenants::ACTIVE).map_err(e)?, &id)?,
        TenantOp::Delete { id } => found(s.tenant_delete(&id).map_err(e)?, &id)?,
        TenantOp::RotateAdmin { id, admin_pub } => found(s.tenant_set_admin(&id, &key(&admin_pub)?).map_err(e)?, &id)?,
    }
    Ok(())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut a = Args::parse();
    if let Some(Cmd::Tenant { op }) = a.cmd.take() {
        if let Err(why) = tenant_cmd(a.db.as_deref(), op) {
            eprintln!("{why}");
            std::process::exit(1);
        }
        return Ok(());
    }
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
        enrolled: a.enrolled,
        ..Config::default()
    };
    serve_with_metrics(listener, metrics_listener, cfg).await
}
