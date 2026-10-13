use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use spike_cli::{
    client::Relay,
    process,
    state::{Contact, Home},
    Outcome,
};
use spike_core::{identity::DeviceCard, session};
use std::path::PathBuf;
use std::time::Duration;

/// Spike Connect (Phase 0 prototype). Not independently reviewed: do not use for real secrets.
#[derive(Parser)]
#[command(name = "spike", version)]
struct Cli {
    /// State directory.
    #[arg(long, env = "SPIKE_HOME", global = true)]
    home: Option<PathBuf>,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = Home::open(match cli.home {
        Some(p) => p,
        None => {
            PathBuf::from(std::env::var("HOME").context("HOME not set")?).join(".spike-connect")
        }
    });

    match cli.cmd {
        Cmd::Init { force } => {
            let id = home.create_identity(force)?;
            println!("created identity in {}", home.path().display());
            println!("device id:   {}", id.device_id());
            println!("fingerprint: {}", id.card().fingerprint());
        }
        Cmd::Card => {
            let id = home.load_identity()?;
            let card = id.card();
            println!("{}", card.encode());
            eprintln!("device id:   {}", card.device_id());
            eprintln!("fingerprint: {}", card.fingerprint());
            eprintln!("Share the card; compare the fingerprint over a second channel.");
        }
        Cmd::Add { name, card, force } => {
            let new = DeviceCard::decode(&card)?;
            let mut contacts = home.load_contacts()?;
            if let Some(existing) = contacts.iter_mut().find(|c| c.name == name) {
                if existing.decode()? == new {
                    println!("{name} already added");
                    return Ok(());
                }
                if !force {
                    bail!("KEY CHANGE: '{name}' already exists with a different key. If you expect this, re-run with --force and re-verify.");
                }
                *existing = Contact {
                    name: name.clone(),
                    card,
                    verified: false,
                };
            } else {
                contacts.push(Contact {
                    name: name.clone(),
                    card,
                    verified: false,
                });
            }
            home.save_contacts(&contacts)?;
            println!("added {name} (UNVERIFIED)");
            println!("fingerprint: {}", new.fingerprint());
            println!("Compare it with {name} over a second channel, then: spike verify {name} <fingerprint>");
        }
        Cmd::Verify { name, fingerprint } => {
            let mut contacts = home.load_contacts()?;
            let c = contacts
                .iter_mut()
                .find(|c| c.name == name)
                .with_context(|| format!("no contact '{name}'"))?;
            let actual = c.decode()?.fingerprint();
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
                println!("{:<16} {:<10} {}", c.name, tag, c.decode()?.fingerprint());
            }
        }
        Cmd::Send {
            name,
            message,
            relay,
        } => {
            let me = home.load_identity()?;
            let contacts = home.load_contacts()?;
            let c = contacts
                .iter()
                .find(|c| c.name == name)
                .with_context(|| format!("no contact '{name}'"))?;
            if !c.verified {
                eprintln!(
                    "warning: {name} is UNVERIFIED; the key has not been checked out of band"
                );
            }
            let peer = c.decode()?;
            let sealed = session::seal(&me, &peer, message.as_bytes())?;
            let mut r = Relay::connect(&relay, &me).await?;
            let id = r.send(&peer.device_id(), &sealed).await?;
            println!(
                "queued at relay (id {id}); {} bytes of ciphertext",
                sealed.len()
            );
        }
        Cmd::Recv {
            relay,
            idle_ms,
            follow,
        } => {
            let me = home.load_identity()?;
            let mut r = Relay::connect(&relay, &me).await?;
            let idle = if follow {
                None
            } else {
                Some(Duration::from_millis(idle_ms))
            };
            let mut n = 0;
            while let Some(d) = r.next_delivery(idle).await? {
                match process(&home, &me, &d.blob)? {
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
                    Outcome::Duplicate => {}
                    Outcome::UnknownSender { fingerprint_hint } => {
                        eprintln!("dropped message from unknown key {fingerprint_hint}…; add them with `spike add` to receive");
                    }
                    Outcome::Invalid => eprintln!("dropped undecryptable message"),
                }
                r.ack(&d.id).await?;
            }
            if !follow && n == 0 {
                eprintln!("no new messages");
            }
        }
    }
    Ok(())
}
