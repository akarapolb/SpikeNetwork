//! On-disk state. Every file is sealed with the vault key (see
//! `spike_core::vault`); nothing secret or identifying is stored in the clear.
//! The vault header (salt, KDF parameters) is the only plaintext file.

use crate::messages::MessageStore;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::identity::{DeviceCard, Identity};
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard, OwnerKey, Update};
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
const OWNER: &str = "owner.enc";
const LAMPORT: &str = "lamport.enc";
/// Largest clock value taken from a received message (2^48: more than any real use will count).
const MAX_LAMPORT: u64 = 1 << 48;
const READ: &str = "read.enc";
const LAST_SYNC: &str = "lastsync.enc";
const DEVICE_LIST: &str = "devicelist.enc";
const QUARANTINE: &str = "requests-sessions.enc";

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
    /// The person: owner key and signed device list (or a legacy single-device card).
    pub fn identity(&self) -> Result<IdentityCard> {
        Ok(IdentityCard::decode(&self.card)?)
    }

    /// The device to message. One per contact until fan-out (milestone M3).
    pub fn decode(&self) -> Result<DeviceCard> {
        Ok(self.identity()?.devices()[0].clone())
    }

    /// Stable key for this person's conversation: their owner key, hex.
    pub fn conversation_key(&self) -> Result<String> {
        Ok(self.identity()?.owner_hex())
    }
}

/// What happened when a newer copy of our own device list was offered.
#[derive(Debug, PartialEq, Eq)]
pub enum OwnUpdate {
    Unchanged,
    Updated {
        version: u64,
    },
    /// The newer list no longer contains this device: it has been removed.
    RemovedThisDevice {
        version: u64,
    },
}

/// What `add_contact` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    New,
    AlreadyKnown,
    /// Same person, newer signed device list. Verification is kept: it covers the owner key.
    Updated {
        version: u64,
    },
    /// A legacy single-device contact moved to a signed identity. Must be re-verified.
    Upgraded,
    /// Different owner key, replaced because `force` was given. Must be re-verified.
    Replaced,
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
        let device = Identity::generate()?;
        let owner = OwnerKey::generate();
        let list = DeviceList::sign(
            &owner,
            1,
            vec![DeviceEntry {
                card: device.card().encode(),
                relay: None,
            }],
        )?;
        home.save_identity(&device)?;
        home.write(OWNER, owner.to_hex().as_bytes())?;
        home.save_device_list(&list)?;
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

    /// Sessions with strangers whose first messages wait as message requests (ADR 0021).
    /// Kept apart from `sessions.enc` so a stranger can never touch a contact's ratchet.
    pub fn load_quarantine(&self) -> Result<Sessions> {
        match self.read_opt(QUARANTINE)? {
            Some(b) => Ok(Sessions::from_json(std::str::from_utf8(&b)?)?),
            None => Ok(Sessions::default()),
        }
    }

    pub fn save_quarantine(&self, s: &Sessions) -> Result<()> {
        self.write(QUARANTINE, s.to_json()?.as_bytes())
    }

    pub fn load_owner(&self) -> Result<OwnerKey> {
        Ok(OwnerKey::from_hex(std::str::from_utf8(
            &self.read(OWNER)?,
        )?)?)
    }

    pub fn load_device_list(&self) -> Result<DeviceList> {
        Ok(serde_json::from_slice(&self.read(DEVICE_LIST)?)?)
    }

    pub fn save_device_list(&self, l: &DeviceList) -> Result<()> {
        l.verify()?;
        self.write(DEVICE_LIST, &serde_json::to_vec(l)?)
    }

    /// Install a person's owner key and signed device list on this device. Used
    /// when this device joins an existing identity (device linking, M4); tests
    /// use it directly. The list must contain this device.
    pub fn install_identity(&self, owner: &OwnerKey, list: &DeviceList) -> Result<()> {
        let me = self.load_identity()?.device_id();
        let cards = list.verify()?;
        if list.owner != hex::encode(owner.public()) {
            bail!("device list is not signed by this owner key");
        }
        if !cards.iter().any(|c| c.device_id() == me) {
            bail!("device list does not include this device");
        }
        self.write(OWNER, owner.to_hex().as_bytes())?;
        self.save_device_list(list)
    }

    /// Apply a copy of our own device list seen elsewhere (e.g. on the relay).
    /// Accepts only a valid, newer list signed by our own owner key. An older
    /// one is ignored (stale relay); a different list at the same version is refused.
    pub fn accept_own_list(&self, offered: &DeviceList) -> Result<OwnUpdate> {
        let cards = offered.verify()?;
        let current = self.load_device_list()?;
        if offered.owner != current.owner {
            bail!("relay offered a device list for a different owner");
        }
        if offered.version < current.version || *offered == current {
            return Ok(OwnUpdate::Unchanged);
        }
        if offered.version == current.version {
            bail!("{}", CoreError::Equivocation(offered.version));
        }
        self.save_device_list(offered)?;
        let me = self.load_identity()?.device_id();
        if cards.iter().any(|c| c.device_id() == me) {
            Ok(OwnUpdate::Updated {
                version: offered.version,
            })
        } else {
            Ok(OwnUpdate::RemovedThisDevice {
                version: offered.version,
            })
        }
    }

    /// Whether this device is still in our own signed device list.
    pub fn this_device_is_listed(&self) -> Result<bool> {
        let me = self.load_identity()?.device_id();
        Ok(self
            .load_device_list()?
            .verify()?
            .iter()
            .any(|c| c.device_id() == me))
    }

    /// The card to share with people: `spike2:` + this person's signed device list.
    pub fn identity_card(&self) -> Result<String> {
        Ok(IdentityCard::encode(&self.load_device_list()?))
    }

    /// Add a contact, or update one from a newer card. Refuses rollbacks and
    /// equivocation always, and owner-key changes unless `force`.
    pub fn add_contact(&self, name: &str, card: &str, force: bool) -> Result<Added> {
        let new = IdentityCard::decode(card)?;
        let mut contacts = self.load_contacts()?;
        let Some(existing) = contacts.iter_mut().find(|c| c.name == name) else {
            contacts.push(Contact {
                name: name.into(),
                card: card.trim().into(),
                verified: false,
            });
            self.save_contacts(&contacts)?;
            return Ok(Added::New);
        };
        let added = match existing.identity()?.check_update(&new) {
            Ok(Update::Same) => return Ok(Added::AlreadyKnown),
            Ok(Update::Newer) => Added::Updated { version: new.version() },
            Ok(Update::Upgrade) => {
                existing.verified = false;
                Added::Upgraded
            }
            Err(CoreError::KeyChange) if force => {
                existing.verified = false;
                Added::Replaced
            }
            Err(CoreError::KeyChange) => bail!(
                "KEY CHANGE: '{name}' already exists with a different identity key. If you expect this, re-run with --force and re-verify."
            ),
            Err(e) => bail!("refused update for '{name}': {e}"),
        };
        existing.card = card.trim().into();
        self.save_contacts(&contacts)?;
        Ok(added)
    }

    /// Keep a received or sent attachment on this device, sealed with the vault
    /// key. File names are keyed tags, so the directory reveals sizes only.
    pub fn save_attachment(&self, msg_id: &str, bytes: &[u8]) -> Result<()> {
        let dir = self.dir.join("attachments");
        make_private_dir(&dir)?;
        let tag = hex::encode(self.vault.index_tag("attachment", msg_id.as_bytes()));
        let sealed = self.vault.seal(&format!("att:{tag}"), bytes)?;
        write_private(&dir.join(format!("{tag}.enc")), &sealed)
    }

    /// The locally kept bytes of a message's attachment, if this device has them.
    pub fn load_attachment(&self, msg_id: &str) -> Result<Option<Vec<u8>>> {
        let tag = hex::encode(self.vault.index_tag("attachment", msg_id.as_bytes()));
        match fs::read(self.dir.join("attachments").join(format!("{tag}.enc"))) {
            Ok(sealed) => Ok(Some(
                self.vault
                    .open(&format!("att:{tag}"), &sealed)
                    .map_err(|_| {
                        anyhow::anyhow!("stored attachment is corrupted or was tampered with")
                    })?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Sealed bytes under `name`, or `None` if never written.
    pub fn load_bytes(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.read_opt(name)
    }

    pub fn save_bytes(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.write(name, bytes)
    }

    fn read_opt(&self, name: &str) -> Result<Option<Vec<u8>>> {
        if self.dir.join(name).exists() {
            Ok(Some(self.read(name)?))
        } else {
            Ok(None)
        }
    }

    /// Everything in this conversation up to this sender-time (ms) counts as read. 0 if never.
    pub fn read_marker(&self, peer: &str) -> Result<u64> {
        let map: std::collections::BTreeMap<String, u64> = match self.read_opt(READ)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => Default::default(),
        };
        Ok(map.get(peer).copied().unwrap_or(0))
    }

    /// Move the read marker forward (never back). Returns whether it moved.
    pub fn advance_read_marker(&self, peer: &str, upto: u64) -> Result<bool> {
        let mut map: std::collections::BTreeMap<String, u64> = match self.read_opt(READ)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => Default::default(),
        };
        if map.get(peer).copied().unwrap_or(0) >= upto {
            return Ok(false);
        }
        map.insert(peer.to_string(), upto);
        self.write(READ, &serde_json::to_vec(&map)?)?;
        Ok(true)
    }

    /// When this device last finished collecting its relay queue (ms, local clock); 0 if never.
    pub fn last_sync(&self) -> u64 {
        self.read_opt(LAST_SYNC)
            .ok()
            .flatten()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn set_last_sync(&self, ms: u64) -> Result<()> {
        self.write(LAST_SYNC, ms.to_string().as_bytes())
    }

    /// Remove a locally kept attachment (best effort), e.g. when its message is deleted.
    pub fn delete_attachment(&self, msg_id: &str) -> Result<()> {
        let tag = hex::encode(self.vault.index_tag("attachment", msg_id.as_bytes()));
        match fs::remove_file(self.dir.join("attachments").join(format!("{tag}.enc"))) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn lamport(&self) -> u64 {
        self.read(LAMPORT)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Next value of this device's logical clock (monotonic, persisted).
    pub fn next_lamport(&self) -> Result<u64> {
        let n = self.lamport().saturating_add(1);
        self.write(LAMPORT, n.to_string().as_bytes())?;
        Ok(n)
    }

    /// Fold in a clock value seen on a received message.
    pub fn observe_lamport(&self, seen: u64) -> Result<()> {
        // A peer chooses `seen`. Cap it so a hostile value cannot push the clock to the end of the range.
        let seen = seen.min(MAX_LAMPORT);
        if seen > self.lamport() {
            self.write(LAMPORT, seen.to_string().as_bytes())?;
        }
        Ok(())
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

    /// A sealed JSON file under `name` (default value if it does not exist yet).
    pub fn load_json<T: serde::de::DeserializeOwned + Default>(&self, name: &str) -> Result<T> {
        match self.read_opt(name)? {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Ok(T::default()),
        }
    }

    pub fn save_json<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<()> {
        self.write(name, &serde_json::to_vec(value)?)
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
