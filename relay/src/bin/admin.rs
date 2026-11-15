//! `spike-relay-admin`: the relay operator's tool for tenants (ADR 0023).
//!
//! ```text
//! spike-relay-admin keygen --out operator.key        # prints the public half for --operator-key
//! spike-relay-admin --relay 127.0.0.1:9100 --key operator.key create-tenant t-0042 --admin-pub <hex> --devices 50
//! spike-relay-admin --relay 127.0.0.1:9100 --key operator.key suspend t-0042
//! spike-relay-admin --relay 127.0.0.1:9100 --key operator.key usage
//! ```
//!
//! The operator key never leaves this machine; the relay holds only its public half.

use clap::{Args, Parser, Subcommand};
use spike_core::enroll::{AdminOp, SigningKey, TenantLimits};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "spike-relay-admin", version)]
struct Cli {
    /// The relay's operator listener (`--metrics-listen`), host:port.
    #[arg(long, env = "SPIKE_RELAY_ADMIN_ADDR", default_value = "127.0.0.1:9100")]
    relay: String,
    /// File holding the operator's private key (hex), made by `keygen`.
    #[arg(long, env = "SPIKE_RELAY_OPERATOR_KEY_FILE")]
    key: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone, Copy)]
struct Limits {
    /// Devices that may be enrolled (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    devices: u64,
    /// Waiting messages per device (0 = the relay's own cap).
    #[arg(long, default_value_t = 0)]
    queue_depth: u64,
    /// Attachment bytes all devices together may store (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    attachment_bytes: u64,
    /// Open connections all devices together may hold (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    connections: u64,
    /// Requests per second all devices together may sustain (0 = unlimited).
    #[arg(long, default_value_t = 0.0)]
    req_rate: f64,
}

impl From<Limits> for TenantLimits {
    fn from(l: Limits) -> Self {
        TenantLimits {
            devices: l.devices,
            queue_depth: l.queue_depth,
            attachment_bytes: l.attachment_bytes,
            connections: l.connections,
            req_rate: l.req_rate,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Make an operator key pair. The private half goes to --out; the public half is printed.
    Keygen {
        #[arg(long)]
        out: PathBuf,
    },
    /// Register a tenant with its admin's public key and limits.
    CreateTenant {
        tenant: String,
        #[arg(long)]
        admin_pub: String,
        #[command(flatten)]
        limits: Limits,
    },
    /// Replace a tenant's limits.
    SetLimits {
        tenant: String,
        #[command(flatten)]
        limits: Limits,
    },
    /// Replace a tenant's admin public key (the customer rotated it).
    SetAdminKey {
        tenant: String,
        #[arg(long)]
        admin_pub: String,
    },
    /// Refuse all of a tenant's devices until resumed. Nothing is deleted.
    Suspend {
        tenant: String,
    },
    Resume {
        tenant: String,
    },
    /// Delete a tenant, its enrollments and everything the relay stores for its devices.
    Delete {
        tenant: String,
        /// Required: this cannot be undone.
        #[arg(long)]
        yes: bool,
    },
    /// Usage numbers (counts and bytes), for one tenant or all.
    Usage {
        tenant: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("spike-relay-admin: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), String> {
    let op = match cli.cmd {
        Cmd::Keygen { out } => {
            if out.exists() {
                return Err(format!("{} exists; not overwriting a key", out.display()));
            }
            let k = SigningKey::generate();
            std::fs::write(&out, k.to_hex()).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600));
            }
            println!("{}", k.public_hex());
            eprintln!(
                "private key written to {}; start the relay with --operator-key {}",
                out.display(),
                k.public_hex()
            );
            return Ok(());
        }
        Cmd::CreateTenant {
            tenant,
            admin_pub,
            limits,
        } => AdminOp::CreateTenant {
            tenant,
            admin_pub,
            limits: limits.into(),
        },
        Cmd::SetLimits { tenant, limits } => AdminOp::SetLimits {
            tenant,
            limits: limits.into(),
        },
        Cmd::SetAdminKey { tenant, admin_pub } => AdminOp::SetAdminKey { tenant, admin_pub },
        Cmd::Suspend { tenant } => AdminOp::Suspend { tenant },
        Cmd::Resume { tenant } => AdminOp::Resume { tenant },
        Cmd::Delete { tenant, yes } => {
            if !yes {
                return Err(format!(
                    "deleting {tenant} removes its enrollments and all its queued data; pass --yes"
                ));
            }
            AdminOp::Delete { tenant }
        }
        Cmd::Usage { tenant } => AdminOp::Usage { tenant },
    };
    let path = cli.key.ok_or("--key (the operator key file) is required")?;
    let hex = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let key = SigningKey::from_hex(&hex).map_err(|_| "the key file does not hold a key")?;
    let a = spike_relay::admin_client::send(&cli.relay, &key, &op).await?;
    if a.ok {
        println!(
            "{}",
            serde_json::to_string_pretty(&a.body).unwrap_or_default()
        );
        Ok(())
    } else {
        Err(a.body.as_str().unwrap_or("refused").to_string())
    }
}
