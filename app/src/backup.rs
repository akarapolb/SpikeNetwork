//! Backup and restore of the identity, as the screens see them (ADR 0029). The file format and the
//! restore itself are in `spike_cli::backup`; this adds the app's own settings (which relay) and the
//! step the screens need between "I picked a file" and "restore".

use super::{target_for, write_ca, RelaySettings, Session, DEFAULT_RELAY, RELAY_SETTINGS};
use anyhow::{bail, Result};
use serde::Serialize;
use spike_cli::backup as cli;
use spike_core::vault::KdfParams;
use std::collections::BTreeMap;
use std::path::Path;

pub use cli::{generate_key, Opened};

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BackupStatusDto {
    /// When the last backup was made (ms), or null if never.
    pub last_ms: Option<u64>,
    /// Make a new one: there is none, it is old, or contacts or groups changed since.
    pub stale: bool,
    pub reason: Option<String>,
}

/// What an opened backup file holds, shown before the person confirms a restore.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BackupSummaryDto {
    pub created_ms: u64,
    pub contacts: usize,
    pub groups: usize,
    pub messages: usize,
    /// The relay the backup was made with.
    pub relay: String,
}

impl Session {
    pub fn backup_status(&self) -> Result<BackupStatusDto> {
        let s = cli::status(&self.home)?;
        Ok(BackupStatusDto {
            last_ms: s.last_ms,
            stale: s.stale,
            reason: s.reason,
        })
    }

    /// Seal a backup of this identity with its own passphrase. Returns the file's bytes.
    pub fn backup_export(&self, passphrase: &str, include_history: bool) -> Result<Vec<u8>> {
        let mut extra = BTreeMap::new();
        extra.insert(
            "relay".to_string(),
            serde_json::to_string(&self.settings())?,
        );
        cli::export(
            &self.home,
            passphrase,
            cli::backup_kdf(),
            include_history,
            extra,
        )
    }
}

fn settings_of(o: &Opened) -> RelaySettings {
    o.extra()
        .get("relay")
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default()
}

/// Read a backup file with its passphrase.
pub fn backup_open(file: &[u8], passphrase: &str) -> Result<(Opened, BackupSummaryDto)> {
    let o = Opened::open(file, passphrase)?;
    let relay = match settings_of(&o).url {
        u if u.is_empty() => DEFAULT_RELAY.to_string(),
        u => u,
    };
    let summary = BackupSummaryDto {
        created_ms: o.created_ms(),
        contacts: o.contacts(),
        groups: o.groups(),
        messages: o.messages(),
        relay,
    };
    Ok((o, summary))
}

/// Make this person's identity live in `dir` as a new device, protected by `device_passphrase`, and
/// unlock it. `relay` replaces the backup's relay address if the person typed another.
pub async fn backup_restore(
    opened: Opened,
    dir: &Path,
    device_passphrase: String,
    kdf: KdfParams,
    relay: Option<String>,
) -> Result<Session> {
    super::check_passphrase(&device_passphrase)?;
    let relay = relay
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    if let Some(u) = &relay {
        if !(u.starts_with("ws://") || u.starts_with("wss://"))
            || u.len() > 256
            || u.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            bail!("the relay address must look like wss://relay.example or ws://127.0.0.1:8787");
        }
    }
    let mut rs = settings_of(&opened);
    if let Some(u) = relay {
        rs.url = u;
    }
    if rs.url.is_empty() {
        rs.url = DEFAULT_RELAY.to_string();
    }
    opened
        .restore(dir, &device_passphrase, kdf, |home, _| {
            home.save_json(RELAY_SETTINGS, &rs)?;
            write_ca(home, &rs)?;
            Ok(target_for(&rs, home))
        })
        .await?;
    Session::unlock(dir, device_passphrase)
}
