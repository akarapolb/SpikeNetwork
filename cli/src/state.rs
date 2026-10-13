//! On-disk state. Phase 0 stores keys in a 0600 file; encrypted storage and
//! OS keystore integration are Phase 1 work.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::identity::{DeviceCard, Identity};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Home(PathBuf);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Contact {
    pub name: String,
    pub card: String,
    pub verified: bool,
}

impl Contact {
    pub fn decode(&self) -> Result<DeviceCard> {
        Ok(DeviceCard::decode(&self.card)?)
    }
}

impl Home {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn has_identity(&self) -> bool {
        self.0.join("identity.json").exists()
    }

    pub fn create_identity(&self, force: bool) -> Result<Identity> {
        if self.has_identity() && !force {
            bail!(
                "identity already exists in {} (use --force to replace it)",
                self.0.display()
            );
        }
        let id = Identity::generate()?;
        write_private(&self.0.join("identity.json"), id.to_json()?.as_bytes())?;
        Ok(id)
    }

    pub fn load_identity(&self) -> Result<Identity> {
        let s = fs::read_to_string(self.0.join("identity.json"))
            .with_context(|| format!("no identity in {} (run `spike init`)", self.0.display()))?;
        Ok(Identity::from_json(&s)?)
    }

    pub fn load_contacts(&self) -> Result<Vec<Contact>> {
        read_json(&self.0.join("contacts.json")).map(|o| o.unwrap_or_default())
    }

    pub fn save_contacts(&self, c: &[Contact]) -> Result<()> {
        write_private(
            &self.0.join("contacts.json"),
            serde_json::to_string_pretty(c)?.as_bytes(),
        )
    }

    /// Returns false if `id` was already recorded.
    pub fn mark_seen(&self, id: &str) -> Result<bool> {
        let path = self.0.join("seen.json");
        let mut seen: HashSet<String> = read_json(&path)?.unwrap_or_default();
        if !seen.insert(id.to_string()) {
            return Ok(false);
        }
        write_private(&path, serde_json::to_string(&seen)?.as_bytes())?;
        Ok(true)
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Atomic write with owner-only permissions.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}
