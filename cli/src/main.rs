use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use spike_cli::{
    messages::Direction,
    ops,
    state::{Added, Home},
    transport::{Target, TransportOpts},
    Outcome,
};
use spike_core::owner::IdentityCard;
use spike_core::vault::KdfParams;
use zeroize::Zeroizing;

const MIN_PASSPHRASE_CHARS: usize = 10;
use std::path::PathBuf;
use std::time::Duration;

/// Spike Connect (Phase 0 prototype). Not independently reviewed: do not use for real secrets.
#[derive(Parser)]
#[command(name = "spike", version)]
struct Cli {
    /// State directory.
    #[arg(long, env = "SPIKE_HOME", global = true)]
    home: Option<PathBuf>,
    /// File containing the passphrase (otherwise SPIKE_PASSPHRASE, otherwise a prompt).
    #[arg(long, global = true)]
    passphrase_file: Option<PathBuf>,
    /// Extra trusted CA certificate (PEM) for a self-hosted wss:// relay.
    #[arg(long, env = "SPIKE_RELAY_CA", global = true)]
    relay_ca: Option<PathBuf>,
    /// Allow plain ws:// to a non-loopback relay (metadata is then exposed on the network).
    #[arg(long, global = true)]
    allow_insecure_transport: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create this device's identity.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Print this device's card (share it) and fingerprint.
    Card,
    /// Add a contact from their card. Starts unverified.
    Add {
        name: String,
        card: String,
        /// Replace an existing contact whose key differs (key change!).
        #[arg(long)]
        force: bool,
    },
    /// Mark a contact verified by typing the fingerprint they read to you.
    Verify { name: String, fingerprint: String },
    /// List contacts.
    Contacts,
    /// Connect to a relay so others can start encrypted sessions with you while you are offline.
    Keys {
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Send an encrypted message through a relay.
    Send {
        name: String,
        message: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Receive queued messages from a relay.
    Recv {
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
        /// Stop after this many milliseconds with no new message.
        #[arg(long, default_value_t = 1500)]
        idle_ms: u64,
        /// Keep running and print messages as they arrive.
        #[arg(long)]
        follow: bool,
    },
    /// Show stored message history with a contact (read from this device only).
    History {
        name: String,
        /// How many of the most recent messages to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Delete stored messages older than this many days, in all conversations.
    Prune {
        #[arg(long)]
        older_than_days: u64,
    },
}

/// Passphrase from `--passphrase-file`, then `SPIKE_PASSPHRASE`, then a prompt.
/// The environment variable is visible to other processes of the same user;
/// prefer the file or the prompt.
fn passphrase(file: &Option<PathBuf>, confirm: bool) -> Result<Zeroizing<String>> {
    let p = if let Some(f) = file {
        let s = std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
        Zeroizing::new(s.trim_end_matches(['\n', '\r']).to_string())
    } else if let Ok(v) = std::env::var("SPIKE_PASSPHRASE") {
        Zeroizing::new(v)
    } else {
        let p = Zeroizing::new(rpassword::prompt_password("Passphrase: ").context(
            "cannot prompt for a passphrase here; use --passphrase-file or SPIKE_PASSPHRASE",
        )?);
        if confirm && *p != *Zeroizing::new(rpassword::prompt_password("Repeat passphrase: ")?) {
            bail!("passphrases do not match");
        }
        p
    };
    if p.is_empty() {
        bail!("empty passphrase");
    }
    Ok(p)
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let dir = match cli.home {
        Some(p) => p,
        None => {
            PathBuf::from(std::env::var("HOME").context("HOME not set")?).join(".spike-connect")
        }
    };

    // `init` creates the vault; every other command unlocks it.
    let home = if let Cmd::Init { force } = &cli.cmd {
        let p = passphrase(&cli.passphrase_file, true)?;
        if p.chars().count() < MIN_PASSPHRASE_CHARS {
            bail!("passphrase must be at least {MIN_PASSPHRASE_CHARS} characters");
        }
        let home = Home::create(&dir, &p, KdfParams::default(), *force)?;
        let id = home.load_identity()?;
        let me = IdentityCard::decode(&home.identity_card()?)?;
        println!("created encrypted state in {}", home.path().display());
        println!("device id:   {}", id.device_id());
        println!(
            "fingerprint: {}  (your identity; stays the same when you add devices)",
            me.fingerprint()
        );
        println!("If you lose the passphrase the identity cannot be recovered.");
        return Ok(());
    } else {
        Home::unlock(&dir, &passphrase(&cli.passphrase_file, false)?)?
    };

    let opts = TransportOpts {
        ca_file: cli.relay_ca,
        allow_insecure: cli.allow_insecure_transport,
    };
    let target = |url: String| Target {
        url,
        opts: opts.clone(),
    };

    match cli.cmd {
        Cmd::Init { .. } => unreachable!("handled above"),
        Cmd::Card => {
            let card = home.identity_card()?;
            let me = IdentityCard::decode(&card)?;
            println!("{card}");
            eprintln!("device id:   {}", home.load_identity()?.device_id());
            eprintln!(
                "devices:     {} (signed list v{})",
                me.devices().len(),
                me.version()
            );
            eprintln!("fingerprint: {}", me.fingerprint());
            eprintln!("Share the card; compare the fingerprint over a second channel.");
        }
        Cmd::Add { name, card, force } => {
            let new = IdentityCard::decode(&card)?;
            match home.add_contact(&name, &card, force)? {
                Added::AlreadyKnown => println!("{name} already added, nothing changed"),
                Added::Updated { version } => {
                    println!("updated {name}'s device list to v{version} ({} devices); verification kept", new.devices().len());
                }
                outcome => {
                    let what = match outcome {
                        Added::Upgraded => "upgraded to a signed identity",
                        Added::Replaced => "REPLACED (identity key changed)",
                        _ => "added",
                    };
                    println!("{name} {what} (UNVERIFIED)");
                    if new.is_legacy() {
                        println!("note: this is an older single-device card");
                    }
                    println!("fingerprint: {}", new.fingerprint());
                    println!("Compare it with {name} over a second channel, then: spike verify {name} <fingerprint>");
                }
            }
        }
        Cmd::Verify { name, fingerprint } => {
            let mut contacts = home.load_contacts()?;
            let c = contacts
                .iter_mut()
                .find(|c| c.name == name)
                .with_context(|| format!("no contact '{name}'"))?;
            let actual = c.identity()?.fingerprint();
            if actual != fingerprint.trim().to_lowercase() {
                bail!("fingerprint does NOT match (stored key is {actual}); not verified");
            }
            c.verified = true;
            home.save_contacts(&contacts)?;
            println!("{name} verified");
        }
        Cmd::Contacts => {
            for c in home.load_contacts()? {
                let tag = if c.verified { "verified" } else { "UNVERIFIED" };
                let id = c.identity()?;
                let devices = if id.is_legacy() {
                    "legacy card".to_string()
                } else {
                    format!("{} device(s), list v{}", id.devices().len(), id.version())
                };
                println!("{:<16} {:<10} {}  {devices}", c.name, tag, id.fingerprint());
            }
        }
        Cmd::Keys { relay } => {
            let n = ops::publish_keys(&home, target(relay)).await?;
            println!("relay holds {n} one-time prekeys for this device");
        }
        Cmd::Send {
            name,
            message,
            relay,
        } => {
            let r = ops::send(&home, target(relay), &name, message.as_bytes()).await?;
            if !r.verified {
                eprintln!(
                    "warning: {name} is UNVERIFIED; the key has not been checked out of band"
                );
            }
            let kind = if r.new_session {
                "new session"
            } else {
                "ratchet"
            };
            println!(
                "queued at relay (id {}); {} bytes of ciphertext ({kind})",
                r.relay_id, r.bytes
            );
        }
        Cmd::History { name, limit } => {
            let contacts = home.load_contacts()?;
            let c = contacts
                .iter()
                .find(|c| c.name == name)
                .with_context(|| format!("no contact '{name}'"))?;
            let peer = c.conversation_key()?;
            let msgs = home.messages()?.conversation(&peer, limit)?;
            if msgs.is_empty() {
                eprintln!("no stored messages with {name}");
            }
            for m in msgs {
                let who = match m.direction {
                    Direction::Out => "you",
                    Direction::In => name.as_str(),
                };
                println!("{}  {who}: {}", fmt_utc(m.ts), m.body);
            }
        }
        Cmd::Prune { older_than_days } => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64;
            let n = home
                .messages()?
                .prune_older_than(now.saturating_sub(older_than_days * 86_400_000))?;
            println!(
                "deleted {n} stored message{}",
                if n == 1 { "" } else { "s" }
            );
        }
        Cmd::Recv {
            relay,
            idle_ms,
            follow,
        } => {
            let idle = if follow {
                None
            } else {
                Some(Duration::from_millis(idle_ms))
            };
            let mut n = 0;
            ops::recv(&home, target(relay), idle, |o| match o {
                Outcome::Message {
                    from,
                    verified,
                    body,
                    ..
                } => {
                    let tag = if verified { "" } else { " (UNVERIFIED sender)" };
                    println!("[{from}{tag}] {}", String::from_utf8_lossy(&body));
                    n += 1;
                }
                Outcome::UnknownSender { fingerprint_hint } => {
                    eprintln!("dropped message from unknown key {fingerprint_hint}…; add them with `spike add` to receive");
                }
                Outcome::Invalid => eprintln!("dropped undecryptable or replayed message"),
                Outcome::Duplicate => {}
            })
            .await?;
            if !follow && n == 0 {
                eprintln!("no new messages");
            }
        }
    }
    Ok(())
}

/// "2026-10-14 09:41 UTC" from ms since the epoch (no time-zone database needed).
fn fmt_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(run()) {
        // One readable line; no backtrace regardless of RUST_BACKTRACE.
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_formatting() {
        assert_eq!(super::fmt_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(super::fmt_utc(951_782_400_000), "2000-02-29 00:00 UTC");
        assert_eq!(super::fmt_utc(1_791_986_460_000), "2026-10-14 14:01 UTC");
    }
}
