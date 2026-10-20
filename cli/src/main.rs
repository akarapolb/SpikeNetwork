use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use spike_cli::{
    link,
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
    /// Send a file (encrypted on this device; the relay stores only ciphertext).
    SendFile {
        name: String,
        path: PathBuf,
        /// Optional text sent with the file.
        #[arg(long, default_value = "")]
        caption: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Mark a conversation read here and on your other devices.
    Read {
        name: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Show unread incoming messages per contact (as marked by `spike read` on any of your devices).
    Unread,
    /// Ask your other devices for messages, edits and deletes this device missed
    /// (e.g. after being offline longer than the relay keeps messages). They answer when they next run `recv`.
    Sync {
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
        /// Seconds to wait for answers from devices that are online right now.
        #[arg(long, default_value_t = 3)]
        wait_secs: u64,
    },
    /// Ask your other devices to re-supply an attachment the relay no longer has.
    RequestFile {
        id: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Principal: let an agent (a contact) ask you for things within limits.
    AgentGrant {
        name: String,
        /// Allowed scope, repeatable (e.g. files.read, payments.send).
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        #[arg(long, default_value_t = 30)]
        days: u64,
        /// Hard cap per request, in cents. Without it, requests with money are denied.
        #[arg(long)]
        max_cents: Option<u64>,
        /// Approve without asking at or below this many cents (0 = requests with no money).
        #[arg(long)]
        auto_below_cents: Option<u64>,
        #[arg(long, default_value_t = 10)]
        per_hour: u32,
        /// Most money in any rolling 24 hours, in cents, counting approved and waiting requests.
        #[arg(long)]
        daily_cents: Option<u64>,
        /// Accept only typed actions (fields you see and services check), never free text.
        /// Every scope must be a known type: see `spike agent-types`.
        #[arg(long)]
        typed_only: bool,
    },
    /// Principal: remove an agent's grant.
    AgentRevoke { name: String },
    /// Principal: list grants and requests (waiting first).
    AgentPending,
    /// Principal: approve a waiting request (id or first 4+ characters).
    AgentApprove {
        id: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Principal: deny a waiting request.
    AgentDeny {
        id: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Principal: send any receipts that have not reached their agent yet.
    AgentFlush {
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Principal: verify and print the tamper-evident audit log.
    AgentAudit,
    /// Agent: ask a contact (your principal) to allow an action.
    AgentRequest {
        name: String,
        #[arg(long)]
        scope: String,
        /// What you want to do, in words the human will read.
        #[arg(long)]
        action: String,
        #[arg(long)]
        amount_cents: Option<u64>,
        /// Typed action field, `key=value`, repeatable (e.g. --field payee=acme
        /// --field currency=USD). Makes the request typed; the scope must be a
        /// known type (`spike agent-types`).
        #[arg(long = "field")]
        fields: Vec<String>,
        /// How long the request stays valid.
        #[arg(long, default_value_t = 60)]
        minutes: u64,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Agent: show your requests and the receipts you got.
    AgentStatus,
    /// List the known typed actions and their fields.
    AgentTypes,
    /// Agent: package an approved request for the service that will act on it
    /// (prints a presentation; give it to that service, which runs `verify-receipt`).
    AgentPresent {
        /// Request id or its first 4+ characters (from `agent-status`).
        id: String,
        /// The service's name, exactly as it is configured there (e.g. pay.example.com).
        #[arg(long)]
        audience: String,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Service: check an agent's presentation before acting, and use it up.
    /// Needs no identity or passphrase. Exit 0 and JSON on stdout = authorized;
    /// exit 2 = refused (reason on stderr); exit 1 = usage or store error.
    VerifyReceipt {
        /// This service's name; presentations for other services are refused.
        #[arg(long)]
        audience: String,
        /// Whose approvals to accept: owner key (64 hex) or identity card. Repeatable.
        #[arg(long = "principal", required = true)]
        principals: Vec<String>,
        /// The scope of what you are about to do.
        #[arg(long)]
        scope: String,
        /// The money you are about to move, in cents (must not exceed the approved amount).
        #[arg(long)]
        amount_cents: Option<u64>,
        /// What you are about to do, as typed fields `key=value` (repeatable). Required
        /// for a typed approval and must match it exactly; refuses free-text approvals.
        #[arg(long = "field")]
        fields: Vec<String>,
        /// SQLite file remembering used approvals. Keep it; share it between instances.
        #[arg(long)]
        spent_db: PathBuf,
        /// Presentation file (default: stdin).
        #[arg(long)]
        input: Option<PathBuf>,
        /// Check only; do not use the approval up.
        #[arg(long)]
        check_only: bool,
    },
    /// List message requests: people who are not contacts and wrote to you.
    Requests,
    /// Accept a message request: they become a contact (not verified) and their messages join the chat.
    RequestAccept {
        /// Request id or its first 8+ characters (from `requests`).
        id: String,
        /// The name to give them.
        name: String,
    },
    /// Decline a message request: it is deleted and the sender is not told.
    RequestDecline { id: String },
    /// Block the sender of a request: decline it, and drop anything they send from now on.
    RequestBlock { id: String },
    /// List the people you blocked.
    Blocked,
    /// Stop blocking someone (owner key or fingerprint, 8+ characters).
    Unblock { who: String },
    /// Edit a message you sent (by id or its first 6+ characters from `spike history`).
    Edit {
        id: String,
        text: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Delete a message you sent, on every device that has it.
    Delete {
        id: String,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Save a received attachment, by message id (or its first 6+ characters from `spike history`).
    Fetch {
        id: String,
        /// Where to write it (default: its sanitised name in the current directory).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
    },
    /// Show stored message history with a contact (read from this device only).
    History {
        name: String,
        /// How many of the most recent messages to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// On a NEW device: show a link code, then join your identity when an existing device approves.
    LinkNew {
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
        /// Seconds to wait for each step.
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
        /// Skip the safety-code question (only for scripts that compare codes another way).
        #[arg(long)]
        yes: bool,
    },
    /// On an EXISTING device: add the new device that shows this link code.
    LinkDevice {
        code: String,
        /// History to copy: all, recent, or none.
        #[arg(long, default_value = "all")]
        history: String,
        /// Days kept by `--history recent`.
        #[arg(long, default_value_t = 30)]
        recent_days: u64,
        #[arg(long, env = "SPIKE_RELAY", default_value = "ws://127.0.0.1:8787")]
        relay: String,
        #[arg(long)]
        yes: bool,
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
    // Services verify without an identity of their own: no vault to unlock.
    if let Cmd::VerifyReceipt {
        audience,
        principals,
        scope,
        amount_cents,
        fields,
        spent_db,
        input,
        check_only,
    } = &cli.cmd
    {
        return verify_receipt(
            audience,
            principals,
            scope,
            *amount_cents,
            fields,
            spent_db,
            input.as_deref(),
            *check_only,
        );
    }
    if let Cmd::AgentTypes = &cli.cmd {
        for t in spike_core::action::REGISTRY {
            let fields: Vec<String> = t
                .fields
                .iter()
                .map(|(n, k)| format!("{n}=<{k:?}>").to_lowercase())
                .collect();
            println!(
                "{:<14} {}{}   shown as: {}",
                t.name,
                fields.join(" "),
                if t.money == spike_core::action::Money::Required {
                    " + --amount-cents"
                } else {
                    ""
                },
                t.template
            );
        }
        return Ok(());
    }
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
        Cmd::Init { .. } | Cmd::VerifyReceipt { .. } | Cmd::AgentTypes => {
            unreachable!("handled above")
        }
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
            for n in &r.notes {
                eprintln!("note: {n}");
            }
            if !r.verified {
                eprintln!(
                    "warning: {name} is UNVERIFIED; the key has not been checked out of band"
                );
            }
            let theirs = r.devices.iter().filter(|d| !d.own).count();
            println!("sent to {} of {name}'s {theirs} device(s)", r.delivered());
            for d in &r.devices {
                let who = if d.own { "your device" } else { "their device" };
                match &d.error {
                    None => println!(
                        "  {who} {}: queued{}",
                        &d.device_id[..8],
                        if d.new_session { " (new session)" } else { "" }
                    ),
                    Some(e) => println!("  {who} {}: NOT sent: {e}", &d.device_id[..8]),
                }
            }
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
                let short = &m.id[..8];
                if m.deleted {
                    println!("{}  {who}: [message deleted]  ({short})", fmt_utc(m.ts));
                    continue;
                }
                let edited = if m.edited { " (edited)" } else { "" };
                println!("{}  {who}: {}{edited}  ({short})", fmt_utc(m.ts), m.body);
                if let Some(a) = &m.attachment {
                    let here = if home.load_attachment(&m.id)?.is_some() {
                        "on this device"
                    } else {
                        "on the relay; `spike fetch` to get it"
                    };
                    println!(
                        "{:>22}[file {} · {} bytes · id {} · {here}]",
                        "",
                        a.name,
                        a.size,
                        &m.id[..8]
                    );
                }
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
        Cmd::SendFile {
            name,
            path,
            caption,
            relay,
        } => {
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let file_name = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "attachment".into());
            let r =
                ops::send_file(&home, target(relay), &name, &file_name, &bytes, &caption).await?;
            for n in &r.notes {
                eprintln!("note: {n}");
            }
            let theirs = r.devices.iter().filter(|d| !d.own).count();
            println!("sent {file_name} ({} bytes, encrypted) to {} of {name}'s {theirs} device(s); message id {}", bytes.len(), r.delivered(), &r.id[..8]);
        }
        Cmd::Read { name, relay } => {
            let n = ops::mark_read(&home, target(relay), &name).await?;
            println!("{name} marked read; told {n} of your other devices");
        }
        Cmd::Unread => {
            let mut any = false;
            for (name, n) in ops::unread_counts(&home)? {
                if n > 0 {
                    println!("{name}: {n} unread");
                    any = true;
                }
            }
            if !any {
                println!("nothing unread");
            }
        }
        Cmd::Sync { relay, wait_secs } => {
            let n = ops::request_catch_up(&home, target(relay.clone())).await?;
            println!("asked {n} of your other devices for what this one missed");
            let mut changed = 0;
            ops::recv(
                &home,
                target(relay),
                Some(Duration::from_secs(wait_secs)),
                |o| {
                    if let Outcome::CaughtUp { changed: c, .. } = o {
                        changed += c;
                    }
                },
            )
            .await?;
            println!("caught up: {changed} message(s) added or updated (devices that are offline answer when they next run `spike recv`)");
        }
        Cmd::RequestFile { id, relay } => {
            let n = ops::request_file(&home, target(relay), &id).await?;
            println!("asked {n} of your other devices for the file; run `spike recv` shortly, then `spike fetch {id}`");
        }
        Cmd::AgentGrant {
            name,
            scopes,
            days,
            max_cents,
            auto_below_cents,
            per_hour,
            daily_cents,
            typed_only,
        } => {
            let key = contact_key(&home, &name)?;
            let g = spike_cli::agent::grant(
                &home,
                &key,
                scopes,
                days,
                max_cents,
                auto_below_cents,
                per_hour,
                daily_cents,
                typed_only,
            )?;
            println!(
                "{name} may ask for: {}{}; until {}; max {} ; auto-approve up to {}; at most {}/hour; {} per 24 h",
                g.body.scopes.join(", "),
                if g.body.typed_only {
                    " (typed actions only)"
                } else {
                    ""
                },
                fmt_utc(g.body.expires),
                g.body
                    .max_cents
                    .map_or("no money".into(), |c| format!("{c} cents")),
                g.body
                    .auto_below_cents
                    .map_or("nothing (always ask)".into(), |c| format!("{c} cents")),
                g.body.per_hour,
                g.body
                    .daily_cents
                    .map_or("no cumulative cap".into(), |c| format!("{c} cents"))
            );
        }
        Cmd::AgentRevoke { name } => {
            let key = contact_key(&home, &name)?;
            if spike_cli::agent::revoke(&home, &key)? {
                println!(
                    "{name}'s grant removed; requests still waiting can no longer be approved"
                );
            } else {
                println!("{name} has no grant");
            }
        }
        Cmd::AgentPending => {
            for g in spike_cli::agent::list_grants(&home)? {
                println!(
                    "grant {}.. agent {}.. scopes {} until {}",
                    &g.body.id[..8],
                    &g.body.agent[..8],
                    g.body.scopes.join(","),
                    fmt_utc(g.body.expires)
                );
            }
            let mut any = false;
            for r in spike_cli::agent::list_inbox(&home)? {
                let q = &r.signed.req;
                let state = match &r.status {
                    spike_cli::agent::Status::Pending => "WAITING".to_string(),
                    spike_cli::agent::Status::Decided { receipt, delivered } => format!(
                        "{:?} by {:?}{}",
                        receipt.body.decision,
                        receipt.body.by,
                        if *delivered {
                            ""
                        } else {
                            " (receipt not delivered yet)"
                        }
                    ),
                };
                any = true;
                println!(
                    "{} {} from {}: {} - {state}",
                    &q.id[..8],
                    fmt_utc(r.received),
                    r.from,
                    describe_request(&q.summary(), &q.action, &q.scope, q.amount_cents)
                );
            }
            for r in spike_cli::agent::list_remote(&home)? {
                let q = &r.signed.req;
                any = true;
                println!(
                    "{} {} from {}: {} - WAITING (forwarded by your main device)",
                    &q.id[..8],
                    fmt_utc(r.received),
                    r.from,
                    describe_request(&q.summary(), &q.action, &q.scope, q.amount_cents)
                );
            }
            if !any {
                println!("no requests");
            }
        }
        Cmd::AgentApprove { id, note, relay } => {
            decide_command(&home, target(relay), &id, true, &note).await?;
        }
        Cmd::AgentDeny { id, note, relay } => {
            decide_command(&home, target(relay), &id, false, &note).await?;
        }
        Cmd::AgentFlush { relay } => {
            let n = ops::flush_receipts(&home, target(relay)).await?;
            println!("{n} receipt(s) sent");
        }
        Cmd::AgentAudit => {
            let log = spike_cli::agent::verified_audit(&home)?;
            for e in &log {
                println!(
                    "{:>4} {} {:<8} agent {}.. {}",
                    e.seq,
                    fmt_utc(e.ts),
                    e.event,
                    &e.agent[..8],
                    e.detail
                );
            }
            match log.last() {
                Some(h) => println!("log intact: {} entries, head {}", log.len(), &h.hash[..16]),
                None => println!("log is empty"),
            }
        }
        Cmd::AgentRequest {
            name,
            scope,
            action,
            amount_cents,
            fields,
            minutes,
            relay,
        } => {
            let typed = if fields.is_empty() {
                None
            } else {
                Some(
                    spike_core::action::parse_pairs(fields.iter().map(String::as_str))
                        .map_err(|e| anyhow::anyhow!("--field: {e}"))?,
                )
            };
            let id = ops::agent_request_typed(
                &home,
                target(relay),
                &name,
                &scope,
                &action,
                amount_cents,
                typed,
                minutes,
            )
            .await?;
            println!(
                "request {} sent to {name}; run `spike recv` for the answer",
                &id[..8]
            );
        }
        Cmd::AgentStatus => {
            let mut any = false;
            for r in spike_cli::agent::list_outbox(&home)? {
                any = true;
                let q = &r.signed.req;
                let state = match &r.receipt {
                    None => "no answer yet".to_string(),
                    Some(x) => format!("{:?} by {:?} {}", x.body.decision, x.body.by, x.body.note),
                };
                println!(
                    "{} to {}: {} - {state}",
                    &q.id[..8],
                    r.to,
                    describe_request(&q.summary(), &q.action, &q.scope, q.amount_cents)
                );
            }
            if !any {
                println!("no requests");
            }
        }
        Cmd::AgentPresent { id, audience, out } => {
            let p = spike_cli::verifier::present(&home, &id, &audience)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, p.to_json())
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!(
                        "presentation for {audience} written to {}; it expires with the request",
                        path.display()
                    );
                }
                None => println!("{}", p.to_json()),
            }
        }
        Cmd::Requests => {
            let all = spike_cli::requests::list(&home)?;
            if all.is_empty() {
                println!("no message requests");
            }
            for r in all {
                println!(
                    "{} {} fingerprint {} ({} message{} held)",
                    &r.key[..8],
                    fmt_utc(r.received),
                    r.fingerprint,
                    r.held.len(),
                    if r.held.len() == 1 { "" } else { "s" }
                );
                for h in &r.held {
                    println!(
                        "    {}{}",
                        h.body
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(120)
                            .collect::<String>(),
                        h.file
                            .as_ref()
                            .map_or(String::new(), |f| format!("  [file \"{f}\" not kept]"))
                    );
                }
            }
        }
        Cmd::RequestAccept { id, name } => {
            let n = spike_cli::requests::accept(&home, &id, &name)?;
            println!("{name} added (UNVERIFIED); {n} message(s) joined the conversation. Compare their fingerprint over a second channel, then `spike verify {name} <fingerprint>`.");
        }
        Cmd::RequestDecline { id } => {
            spike_cli::requests::decline(&home, &id)?;
            println!("declined; the sender is not told");
        }
        Cmd::RequestBlock { id } => {
            spike_cli::requests::block(&home, &id)?;
            println!("blocked: their messages are dropped when they arrive (the relay still carries them until then)");
        }
        Cmd::Blocked => {
            let all = spike_cli::requests::blocked(&home)?;
            if all.is_empty() {
                println!("nobody is blocked");
            }
            for b in all {
                println!(
                    "{} fingerprint {} blocked {}",
                    &b.owner[..16],
                    b.fingerprint,
                    fmt_utc(b.at)
                );
            }
        }
        Cmd::Unblock { who } => {
            spike_cli::requests::unblock(&home, &who)?;
            println!("unblocked");
        }
        Cmd::Edit { id, text, relay } => {
            let r = ops::edit(&home, target(relay), &id, &text).await?;
            for n in &r.notes {
                eprintln!("note: {n}");
            }
            println!(
                "edit sent to {} of their devices and your other devices",
                r.delivered()
            );
        }
        Cmd::Delete { id, relay } => {
            let r = ops::delete(&home, target(relay), &id).await?;
            for n in &r.notes {
                eprintln!("note: {n}");
            }
            println!(
                "delete sent to {} of their devices and your other devices",
                r.delivered()
            );
        }
        Cmd::Fetch {
            id,
            out,
            force,
            relay,
        } => {
            let (att, bytes) = ops::fetch_attachment(&home, target(relay), &id).await?;
            let path = out.unwrap_or_else(|| {
                PathBuf::from(spike_core::attachment::safe_file_name(&att.name))
            });
            if path.exists() && !force {
                bail!("{} already exists (use --out or --force)", path.display());
            }
            std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
            println!(
                "saved {} ({} bytes, integrity checked)",
                path.display(),
                bytes.len()
            );
        }
        Cmd::LinkNew {
            relay,
            timeout_secs,
            yes,
        } => {
            let t = Duration::from_secs(timeout_secs);
            let w = link::offer(&home, target(relay))?;
            eprintln!("On a device that already uses your identity, run:");
            eprintln!("  spike link-device <code>");
            eprintln!(
                "with this code (in the app it is a QR code; never send it through the relay):"
            );
            println!("{}", w.code);
            let session = link::await_hello(&home, &w, t).await?;
            println!("safety code: {}", session.safety_code);
            if !yes && !confirm("Does the other device show exactly the same code?")? {
                bail!("linking cancelled: codes did not match, nothing was installed");
            }
            eprintln!("waiting for your identity and history...");
            let r = link::receive(&home, &w, &session, t).await?;
            println!(
                "linked: device list v{}, {} contacts, {} messages copied",
                r.version, r.contacts, r.messages
            );
        }
        Cmd::LinkDevice {
            code,
            history,
            recent_days,
            relay,
            yes,
        } => {
            let mode = match history.as_str() {
                "all" => link::History::All,
                "recent" => link::History::Recent { days: recent_days },
                "none" => link::History::None,
                other => bail!("unknown --history '{other}': use all, recent or none"),
            };
            let p = link::begin(&home, target(relay.clone()), &code).await?;
            println!("safety code: {}", p.safety_code);
            if !yes && !confirm("Does the new device show exactly the same code?")? {
                bail!("linking cancelled: nothing was shared");
            }
            let r = link::finish(&home, target(relay), p, mode).await?;
            println!(
                "linked: published device list v{}; sent {} contacts, {} messages and {} recent files, encrypted to the new device",
                r.version, r.contacts, r.messages, r.files
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
                    id,
                    ..
                } => {
                    let tag = if verified { "" } else { " (UNVERIFIED sender)" };
                    if let Ok(Some(a)) = home.messages().and_then(|s| Ok(s.find(&id)?.attachment)) {
                        println!("[{from}{tag}] sent a file: {} ({} bytes) · `spike fetch {}`", a.name, a.size, &id[..8]);
                    }
                    println!("[{from}{tag}] {}", String::from_utf8_lossy(&body));
                    n += 1;
                }
                Outcome::UnknownSender { fingerprint_hint, .. } => {
                    eprintln!("dropped message from unknown key {fingerprint_hint}… (no valid introduction); add them with `spike add` to receive");
                }
                Outcome::MessageRequest { id, fingerprint, held, new } => println!(
                    "[message request {}{}: {fingerprint} wrote to you ({held} held); `spike request-accept {} <name>`, `request-decline` or `request-block`]",
                    &id[..8],
                    if new { " (new)" } else { "" },
                    &id[..8]
                ),
                Outcome::Invalid => eprintln!("dropped undecryptable or replayed message"),
                Outcome::Duplicate | Outcome::Ignored => {}
                Outcome::AgentRequest { from, id, scope, action, amount_cents, summary, verdict } => println!(
                    "[agent request {} from {from}: {} - {verdict}{}]",
                    &id[..8],
                    describe_request(&summary, &action, &scope, amount_cents),
                    if verdict == "pending" { format!("; `spike agent-approve {}` or `agent-deny`", &id[..8]) } else { String::new() }
                ),
                Outcome::AgentRemotePending { from, id, scope, action, amount_cents, summary } => println!(
                    "[agent request {} from {from}, forwarded by your main device: {}; `spike agent-approve {}` or `agent-deny`]",
                    &id[..8],
                    describe_request(&summary, &action, &scope, amount_cents),
                    &id[..8]
                ),
                Outcome::AgentRemoteDecision { id, approved } => println!(
                    "[your other device {} request {}; the receipt is on its way]",
                    if approved { "approved" } else { "denied" },
                    &id[..8]
                ),
                Outcome::AgentReceipt { from, id, approved, by_policy, note } => println!(
                    "[{from} {} request {}{}{}]",
                    if approved { "APPROVED" } else { "DENIED" },
                    &id[..8],
                    if by_policy { " (by policy)" } else { "" },
                    if note.is_empty() { String::new() } else { format!(": {note}") }
                ),
                Outcome::Edited { who, id } => println!("[{who} edited message {}]", &id[..8]),
                Outcome::Deleted { who, id } => println!("[{who} deleted message {}]", &id[..8]),
                Outcome::Rejected { reason } => eprintln!("ignored a change: {reason}"),
                Outcome::ReadSynced { peer } => println!("[your other device marked {peer} read]"),
                Outcome::CaughtUp { changed, last } => {
                    println!("[caught up from your other device: {changed} change(s){}]", if last { ", complete" } else { "" });
                }
                Outcome::FileReady { id } => println!("[your other device re-supplied a file: `spike fetch {}`]", &id[..8]),
                Outcome::SyncRequest { .. } => println!("[answered a request from your other device]"),
                Outcome::SentCopy { to, body, .. } => {
                    println!("[you -> {to}, sent from your other device] {}", String::from_utf8_lossy(&body));
                }
            })
            .await?;
            if !follow && n == 0 {
                eprintln!("no new messages");
            }
        }
    }
    Ok(())
}

/// How a request is shown to the human. A typed request is shown by what its
/// fields say, with the agent's own words marked as such; a free-text request
/// is marked as unchecked, because only its scope and amount mean anything.
fn describe_request(
    summary: &Option<String>,
    action: &str,
    scope: &str,
    amount_cents: Option<u64>,
) -> String {
    match summary {
        Some(s) => format!("{s} [{scope}, typed] (agent's note: \"{action}\")"),
        None => format!(
            "\"{action}\" [{scope}{}; free text in the agent's own words, not checked]",
            amount_cents.map_or(String::new(), |c| format!(", {c} cents"))
        ),
    }
}

/// `agent-approve` / `agent-deny`. On the approver device this decides and sends
/// the receipt; on any other device it passes the decision to the approver.
async fn decide_command(
    home: &Home,
    relay: Target,
    id: &str,
    approve: bool,
    note: &str,
) -> Result<()> {
    let word = if approve { "approved" } else { "denied" };
    let sent = ops::agent_decide(home, relay, id, approve, note).await?;
    let me = home.load_identity()?.device_id();
    if !spike_cli::agent::is_approver(home, &me)? {
        println!("{word}; sent to your main device, which records it and sends the receipt when it next runs `spike recv`");
    } else if sent {
        println!("{word}, receipt sent");
    } else {
        println!("{word}; receipt queued (run `spike agent-flush` when the agent is reachable)");
    }
    Ok(())
}

/// `spike verify-receipt`: see the command's help. Refusals exit with code 2.
#[allow(clippy::too_many_arguments)] // one argument per command-line flag
fn verify_receipt(
    audience: &str,
    principals: &[String],
    scope: &str,
    amount_cents: Option<u64>,
    fields: &[String],
    spent_db: &std::path::Path,
    input: Option<&std::path::Path>,
    check_only: bool,
) -> Result<()> {
    use spike_core::verifier::{self, Action, Config, Presentation};
    if !verifier::valid_audience(audience) {
        bail!(
            "--audience must be 1 to {} printable characters without spaces",
            verifier::MAX_AUDIENCE
        );
    }
    let principals = principals
        .iter()
        .map(|p| {
            let p = p.trim();
            if p.len() == 64 && p.bytes().all(|c| c.is_ascii_hexdigit()) {
                Ok(p.to_ascii_lowercase())
            } else {
                Ok(IdentityCard::decode(p)
                    .context("--principal is neither an owner key nor an identity card")?
                    .owner_hex())
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let text = match input {
        Some(f) => {
            std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
        }
        None => std::io::read_to_string(std::io::stdin()).context("reading stdin")?,
    };
    let cfg = Config {
        audience: audience.into(),
        principals,
    };
    let fields = if fields.is_empty() {
        None
    } else {
        let f = spike_core::action::parse_pairs(fields.iter().map(String::as_str))
            .map_err(|e| anyhow::anyhow!("--field: {e}"))?;
        spike_core::action::validate(scope, &f, amount_cents)
            .map_err(|e| anyhow::anyhow!("--field: {e}"))?;
        Some(f)
    };
    let action = Action {
        scope,
        amount_cents,
        fields: fields.as_ref(),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let result = match Presentation::from_json(text.trim()) {
        Err(_) => Err(verifier::Refusal::Malformed),
        Ok(p) if check_only => verifier::check(&cfg, &p, &action, now),
        Ok(p) => {
            let mut store = spike_cli::verifier::SqliteSpent::open(spent_db)?;
            verifier::redeem(&cfg, &mut store, &p, &action, now)
        }
    };
    match result {
        Ok(a) => {
            println!("{}", serde_json::to_string(&a)?);
            Ok(())
        }
        Err(verifier::Refusal::Store(e)) => bail!("spent-request store: {e}"),
        Err(r) => {
            eprintln!("refused: {r}");
            std::process::exit(2);
        }
    }
}

fn contact_key(home: &Home, name: &str) -> Result<String> {
    home.load_contacts()?
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no contact '{name}'"))?
        .conversation_key()
}

/// Ask a yes/no question on the terminal. Anything but y/yes is no.
fn confirm(question: &str) -> Result<bool> {
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading your answer (use --yes in scripts)")?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
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

/// Whether a panic is `println!` failing because the reader went away
/// (e.g. `spike history bob | head`). Unix EPIPE, or Windows 109 / 232.
fn is_closed_pipe(msg: &str) -> bool {
    msg.starts_with("failed printing to std")
        && [
            "Broken pipe",
            "os error 32)",
            "os error 109)",
            "os error 232)",
        ]
        .iter()
        .any(|m| msg.contains(m))
}

fn main() {
    // A closed pipe is the reader's choice, not a crash: stop quietly.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        if is_closed_pipe(msg) {
            std::process::exit(0);
        }
        default_hook(info)
    }));
    // Windows gives the main thread 1 MiB of stack, which clap's generated
    // parser for this many commands overflows in debug builds. Use 8 MiB.
    let worker = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(|| {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(run())
        })
        .expect("start main thread");
    match worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            // One readable line; no backtrace regardless of RUST_BACKTRACE.
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        // The panic hook has already reported it.
        Err(_) => std::process::exit(101),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn closed_pipes_are_recognised_and_nothing_else() {
        use super::is_closed_pipe;
        assert!(is_closed_pipe(
            "failed printing to stdout: Broken pipe (os error 32)"
        ));
        assert!(is_closed_pipe(
            "failed printing to stdout: The pipe is being closed. (os error 232)"
        ));
        assert!(!is_closed_pipe(
            "failed printing to stdout: No space left on device (os error 28)"
        ));
        assert!(!is_closed_pipe("index out of bounds"));
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(super::fmt_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(super::fmt_utc(951_782_400_000), "2000-02-29 00:00 UTC");
        assert_eq!(super::fmt_utc(1_791_986_460_000), "2026-10-14 14:01 UTC");
    }
}
