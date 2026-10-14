//! On-disk state. Every file is sealed with the vault key (see
//! `spike_core::vault`); nothing secret or identifying is stored in the clear.
//! The vault header (salt, KDF parameters) is the only plaintext file.

use crate::messages::MessageStore;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::identity::{DeviceCard, Identity};
use spike_core::session::Sessions;
use spike_core::vault::{KdfParams, Vault, VaultHeader};
use spike_core::Error as CoreError;
use std::fs;
use std::path::{Path, PathBuf};

const HEADER_FILE: &str = "vault.json";
const IDENTITY: &str = "identity.enc";
const SESSIONS: &str = "sessions.enc";
const CONTACTS: &str = "contacts.enc";
const MESSAGES: &str = "messages.db";

/// An unlocked state directory.
pub struct Home {
    dir: PathBuf,
    vault: Vault,
}

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
    pub fn exists(dir: &Path) -> bool {
        dir.join(HEADER_FILE).exists()
    }

    /// Create a new vault and a fresh identity. Refuses to overwrite existing
    /// state unless `force`.
    pub fn create(
        dir: impl Into<PathBuf>,
        passphrase: &str,
        kdf: KdfParams,
        force: bool,
    ) -> Result<Self> {
        let dir = dir.into();
        if Self::exists(&dir) && !force {
            bail!("state already exists in {} (use --force to replace it, destroying the old identity)", dir.display());
        }
        make_private_dir(&dir)?;
        let (vault, header) = Vault::create(passphrase, kdf)?;
        write_private(&dir.join(HEADER_FILE), header.to_json()?.as_bytes())?;
        let home = Self { dir, vault };
        home.save_identity(&Identity::generate()?)?;
        home.save_sessions(&Sessions::default())?;
        home.save_contacts(&[])?;
        Ok(home)
    }

    pub fn unlock(dir: impl Into<PathBuf>, passphrase: &str) -> Result<Self> {
        let dir = dir.into();
        let header = fs::read_to_string(dir.join(HEADER_FILE))
            .with_context(|| format!("no state in {} (run `spike init`)", dir.display()))?;
        let vault = match Vault::unlock(passphrase, &VaultHeader::from_json(&header)?) {
            Ok(v) => v,
            Err(CoreError::WrongPassphrase) => bail!("wrong passphrase"),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { dir, vault })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn load_identity(&self) -> Result<Identity> {
        Ok(Identity::from_json(std::str::from_utf8(
            &self.read(IDENTITY)?,
        )?)?)
    }

    pub fn save_identity(&self, id: &Identity) -> Result<()> {
        self.write(IDENTITY, id.to_json()?.as_bytes())
    }

    pub fn load_sessions(&self) -> Result<Sessions> {
        Ok(Sessions::from_json(std::str::from_utf8(
            &self.read(SESSIONS)?,
        )?)?)
    }

    pub fn save_sessions(&self, s: &Sessions) -> Result<()> {
        self.write(SESSIONS, s.to_json()?.as_bytes())
    }

    /// Encrypted message history (see [`crate::messages`]).
    pub fn messages(&self) -> Result<MessageStore<'_>> {
        MessageStore::open(&self.dir.join(MESSAGES), &self.vault)
    }

    pub fn load_contacts(&self) -> Result<Vec<Contact>> {
        Ok(serde_json::from_slice(&self.read(CONTACTS)?)?)
    }

    pub fn save_contacts(&self, c: &[Contact]) -> Result<()> {
        self.write(CONTACTS, serde_json::to_string_pretty(c)?.as_bytes())
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        let sealed = fs::read(self.dir.join(name)).with_context(|| format!("reading {name}"))?;
        self.vault.open(name, &sealed).map_err(|e| {
            anyhow::anyhow!("{name}: {e} (modified, truncated, or copied from another vault)")
        })
    }

    fn write(&self, name: &str, plaintext: &[u8]) -> Result<()> {
        write_private(&self.dir.join(name), &self.vault.seal(name, plaintext)?)
    }
}

fn make_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Atomic write with owner-only permissions: write a temp file, fsync, rename.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}
