//! On-disk state. Every file is sealed with the vault key (see
//! `spike_core::vault`); nothing secret or identifying is stored in the clear.
//! The vault header (salt, KDF parameters) is the only plaintext file.
//!
//! `Home` keeps its files through a [`Store`] (ADR 0033, step 2). The logic is written once, as `async`
//! methods on `Home<S>` with an `_async` suffix. `Home` on its own means `Home<FileStore>`, the state
//! directory, and keeps the synchronous methods every caller already uses; they run the async ones, which a
//! `FileStore` finishes without waiting. A store that has to wait (IndexedDB) has only the async methods.

use crate::messages::MessageStore;
use crate::store::{FileStore, Store};
use anyhow::{bail, Context, Result};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use spike_core::identity::{DeviceCard, Identity};
use spike_core::owner::{DeviceEntry, DeviceList, IdentityCard, OwnerKey, Update};
use spike_core::session::Sessions;
use spike_core::vault::{KdfParams, Vault, VaultHeader};
use spike_core::Error as CoreError;
use std::future::Future;
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

/// An unlocked state: a vault key and the store its sealed files live in. Without a type argument this is the
/// state directory (`Home<FileStore>`), so existing code reads as before.
pub struct Home<S: Store = FileStore> {
    store: S,
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

/// Run a future that is known not to wait (every `FileStore` operation). A `Store` that does wait would panic
/// here, which is why the synchronous methods exist only for `Home<FileStore>`.
fn ready<T>(f: impl Future<Output = T>) -> T {
    f.now_or_never().expect("a FileStore operation never waits")
}

/// The attachment's name inside the store: a keyed tag, so the folder reveals sizes only.
fn attachment_name(tag: &str) -> String {
    format!("attachments/{tag}.enc")
}

// ---------------------------------------------------------------------------------------------------------
// The logic, over any store.
// ---------------------------------------------------------------------------------------------------------
impl<S: Store> Home<S> {
    /// Whether `store` already holds a vault.
    pub async fn exists_in(store: &S) -> Result<bool> {
        Ok(store.get(HEADER_FILE).await?.is_some())
    }

    /// Create a new vault and a fresh identity in `store`. Refuses to overwrite existing
    /// state unless `force`.
    pub async fn create_with(
        store: S,
        passphrase: &str,
        kdf: KdfParams,
        force: bool,
    ) -> Result<Self> {
        if Self::exists_in(&store).await? && !force {
            bail!("state already exists (use --force to replace it, destroying the old identity)");
        }
        let (vault, header) = Vault::create(passphrase, kdf)?;
        store.put(HEADER_FILE, header.to_json()?.as_bytes()).await?;
        let home = Self { store, vault };
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
        home.save_identity_async(&device).await?;
        home.write(OWNER, owner.to_hex().as_bytes()).await?;
        home.save_device_list_async(&list).await?;
        home.save_sessions_async(&Sessions::default()).await?;
        home.save_contacts_async(&[]).await?;
        Ok(home)
    }

    pub async fn unlock_with(store: S, passphrase: &str) -> Result<Self> {
        let Some(header) = store.get(HEADER_FILE).await? else {
            bail!("no state here (run `spike init`)");
        };
        let header = String::from_utf8(header).context("the vault header is not text")?;
        let vault = match Vault::unlock(passphrase, &VaultHeader::from_json(&header)?) {
            Ok(v) => v,
            Err(CoreError::WrongPassphrase) => bail!("wrong passphrase"),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { store, vault })
    }

    pub async fn load_identity_async(&self) -> Result<Identity> {
        Ok(Identity::from_json(std::str::from_utf8(
            &self.read(IDENTITY).await?,
        )?)?)
    }

    pub async fn save_identity_async(&self, id: &Identity) -> Result<()> {
        self.write(IDENTITY, id.to_json()?.as_bytes()).await
    }

    pub async fn load_sessions_async(&self) -> Result<Sessions> {
        Ok(Sessions::from_json(std::str::from_utf8(
            &self.read(SESSIONS).await?,
        )?)?)
    }

    pub async fn save_sessions_async(&self, s: &Sessions) -> Result<()> {
        self.write(SESSIONS, s.to_json()?.as_bytes()).await
    }

    /// Sessions with strangers whose first messages wait as message requests (ADR 0021).
    /// Kept apart from `sessions.enc` so a stranger can never touch a contact's ratchet.
    pub async fn load_quarantine_async(&self) -> Result<Sessions> {
        match self.read_opt(QUARANTINE).await? {
            Some(b) => Ok(Sessions::from_json(std::str::from_utf8(&b)?)?),
            None => Ok(Sessions::default()),
        }
    }

    pub async fn save_quarantine_async(&self, s: &Sessions) -> Result<()> {
        self.write(QUARANTINE, s.to_json()?.as_bytes()).await
    }

    pub async fn load_owner_async(&self) -> Result<OwnerKey> {
        Ok(OwnerKey::from_hex(std::str::from_utf8(
            &self.read(OWNER).await?,
        )?)?)
    }

    pub async fn load_device_list_async(&self) -> Result<DeviceList> {
        Ok(serde_json::from_slice(&self.read(DEVICE_LIST).await?)?)
    }

    pub async fn save_device_list_async(&self, l: &DeviceList) -> Result<()> {
        l.verify()?;
        self.write(DEVICE_LIST, &serde_json::to_vec(l)?).await
    }

    /// Install a person's owner key and signed device list on this device. Used
    /// when this device joins an existing identity (device linking, M4); tests
    /// use it directly. The list must contain this device.
    pub async fn install_identity_async(&self, owner: &OwnerKey, list: &DeviceList) -> Result<()> {
        let me = self.load_identity_async().await?.device_id();
        let cards = list.verify()?;
        if list.owner != hex::encode(owner.public()) {
            bail!("device list is not signed by this owner key");
        }
        if !cards.iter().any(|c| c.device_id() == me) {
            bail!("device list does not include this device");
        }
        self.write(OWNER, owner.to_hex().as_bytes()).await?;
        self.save_device_list_async(list).await
    }

    /// Apply a copy of our own device list seen elsewhere (e.g. on the relay).
    /// Accepts only a valid, newer list signed by our own owner key. An older
    /// one is ignored (stale relay); a different list at the same version is refused.
    pub async fn accept_own_list_async(&self, offered: &DeviceList) -> Result<OwnUpdate> {
        let cards = offered.verify()?;
        let current = self.load_device_list_async().await?;
        if offered.owner != current.owner {
            bail!("relay offered a device list for a different owner");
        }
        if offered.version < current.version || *offered == current {
            return Ok(OwnUpdate::Unchanged);
        }
        if offered.version == current.version {
            bail!("{}", CoreError::Equivocation(offered.version));
        }
        self.save_device_list_async(offered).await?;
        let me = self.load_identity_async().await?.device_id();
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
    pub async fn this_device_is_listed_async(&self) -> Result<bool> {
        let me = self.load_identity_async().await?.device_id();
        Ok(self
            .load_device_list_async()
            .await?
            .verify()?
            .iter()
            .any(|c| c.device_id() == me))
    }

    /// The card to share with people: `spike2:` + this person's signed device list.
    pub async fn identity_card_async(&self) -> Result<String> {
        Ok(IdentityCard::encode(&self.load_device_list_async().await?))
    }

    /// Add a contact, or update one from a newer card. Refuses rollbacks and
    /// equivocation always, and owner-key changes unless `force`.
    pub async fn add_contact_async(&self, name: &str, card: &str, force: bool) -> Result<Added> {
        let new = IdentityCard::decode(card)?;
        let mut contacts = self.load_contacts_async().await?;
        let Some(existing) = contacts.iter_mut().find(|c| c.name == name) else {
            contacts.push(Contact {
                name: name.into(),
                card: card.trim().into(),
                verified: false,
            });
            self.save_contacts_async(&contacts).await?;
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
        self.save_contacts_async(&contacts).await?;
        Ok(added)
    }

    /// Keep a received or sent attachment on this device, sealed with the vault
    /// key. File names are keyed tags, so the directory reveals sizes only.
    pub async fn save_attachment_async(&self, msg_id: &str, bytes: &[u8]) -> Result<()> {
        let tag = hex::encode(self.vault.index_tag("attachment", msg_id.as_bytes()));
        let sealed = self.vault.seal(&format!("att:{tag}"), bytes)?;
        self.store.put(&attachment_name(&tag), &sealed).await
    }

    /// The locally kept bytes of a message's attachment, if this device has them.
    pub async fn load_attachment_async(&self, msg_id: &str) -> Result<Option<Vec<u8>>> {
        let tag = hex::encode(self.vault.index_tag("attachment", msg_id.as_bytes()));
        match self.store.get(&attachment_name(&tag)).await? {
            Some(sealed) => Ok(Some(
                self.vault
                    .open(&format!("att:{tag}"), &sealed)
                    .map_err(|_| {
                        anyhow::anyhow!("stored attachment is corrupted or was tampered with")
                    })?,
            )),
            None => Ok(None),
        }
    }

    /// Remove a locally kept attachment (best effort), e.g. when its message is deleted.
    pub async fn delete_attachment_async(&self, msg_id: &str) -> Result<()> {
        let tag = hex::encode(self.vault.index_tag("attachment", msg_id.as_bytes()));
        self.store.delete(&attachment_name(&tag)).await
    }

    /// Sealed bytes under `name`, or `None` if never written.
    pub async fn load_bytes_async(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.read_opt(name).await
    }

    pub async fn save_bytes_async(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.write(name, bytes).await
    }

    /// Everything in this conversation up to this sender-time (ms) counts as read. 0 if never.
    pub async fn read_marker_async(&self, peer: &str) -> Result<u64> {
        let map: std::collections::BTreeMap<String, u64> = match self.read_opt(READ).await? {
            Some(b) => serde_json::from_slice(&b)?,
            None => Default::default(),
        };
        Ok(map.get(peer).copied().unwrap_or(0))
    }

    /// Move the read marker forward (never back). Returns whether it moved.
    pub async fn advance_read_marker_async(&self, peer: &str, upto: u64) -> Result<bool> {
        let mut map: std::collections::BTreeMap<String, u64> = match self.read_opt(READ).await? {
            Some(b) => serde_json::from_slice(&b)?,
            None => Default::default(),
        };
        if map.get(peer).copied().unwrap_or(0) >= upto {
            return Ok(false);
        }
        map.insert(peer.to_string(), upto);
        self.write(READ, &serde_json::to_vec(&map)?).await?;
        Ok(true)
    }

    /// When this device last finished collecting its relay queue (ms, local clock); 0 if never.
    pub async fn last_sync_async(&self) -> u64 {
        self.read_opt(LAST_SYNC)
            .await
            .ok()
            .flatten()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub async fn set_last_sync_async(&self, ms: u64) -> Result<()> {
        self.write(LAST_SYNC, ms.to_string().as_bytes()).await
    }

    async fn lamport(&self) -> u64 {
        self.read(LAMPORT)
            .await
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Next value of this device's logical clock (monotonic, persisted).
    pub async fn next_lamport_async(&self) -> Result<u64> {
        let n = self.lamport().await.saturating_add(1);
        self.write(LAMPORT, n.to_string().as_bytes()).await?;
        Ok(n)
    }

    /// Fold in a clock value seen on a received message.
    pub async fn observe_lamport_async(&self, seen: u64) -> Result<()> {
        // A peer chooses `seen`. Cap it so a hostile value cannot push the clock to the end of the range.
        let seen = seen.min(MAX_LAMPORT);
        if seen > self.lamport().await {
            self.write(LAMPORT, seen.to_string().as_bytes()).await?;
        }
        Ok(())
    }

    pub async fn load_contacts_async(&self) -> Result<Vec<Contact>> {
        Ok(serde_json::from_slice(&self.read(CONTACTS).await?)?)
    }

    pub async fn save_contacts_async(&self, c: &[Contact]) -> Result<()> {
        self.write(CONTACTS, serde_json::to_string_pretty(c)?.as_bytes())
            .await
    }

    /// A sealed JSON file under `name` (default value if it does not exist yet).
    pub async fn load_json_async<T: serde::de::DeserializeOwned + Default>(
        &self,
        name: &str,
    ) -> Result<T> {
        match self.read_opt(name).await? {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Ok(T::default()),
        }
    }

    pub async fn save_json_async<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.write(name, &bytes).await
    }

    async fn read_opt(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match self.store.get(name).await? {
            Some(sealed) => Ok(Some(self.open(name, &sealed)?)),
            None => Ok(None),
        }
    }

    async fn read(&self, name: &str) -> Result<Vec<u8>> {
        let sealed = self
            .store
            .get(name)
            .await?
            .with_context(|| format!("reading {name}: no such file"))?;
        self.open(name, &sealed)
    }

    fn open(&self, name: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        self.vault.open(name, sealed).map_err(|e| {
            anyhow::anyhow!("{name}: {e} (modified, truncated, or copied from another vault)")
        })
    }

    async fn write(&self, name: &str, plaintext: &[u8]) -> Result<()> {
        self.store
            .put(name, &self.vault.seal(name, plaintext)?)
            .await
    }
}

// ---------------------------------------------------------------------------------------------------------
// The state directory: what every caller has always used. Same names, same behaviour; each method runs the
// async one above, which a `FileStore` completes without waiting.
// ---------------------------------------------------------------------------------------------------------
impl Home<FileStore> {
    pub fn exists(dir: &Path) -> bool {
        FileStore::new(dir).has(HEADER_FILE)
    }

    /// Create a new vault and a fresh identity. Refuses to overwrite existing
    /// state unless `force`.
    pub fn create(
        dir: impl Into<PathBuf>,
        passphrase: &str,
        kdf: KdfParams,
        force: bool,
    ) -> Result<Self> {
        let store = FileStore::new(dir);
        if store.has(HEADER_FILE) && !force {
            bail!("state already exists in {} (use --force to replace it, destroying the old identity)", store.dir().display());
        }
        store.make_dir()?;
        ready(Self::create_with(store, passphrase, kdf, force))
    }

    pub fn unlock(dir: impl Into<PathBuf>, passphrase: &str) -> Result<Self> {
        let store = FileStore::new(dir);
        if !store.has(HEADER_FILE) {
            bail!("no state in {} (run `spike init`)", store.dir().display());
        }
        ready(Self::unlock_with(store, passphrase))
    }

    pub fn path(&self) -> &Path {
        self.store.dir()
    }

    pub fn load_identity(&self) -> Result<Identity> {
        ready(self.load_identity_async())
    }

    pub fn save_identity(&self, id: &Identity) -> Result<()> {
        ready(self.save_identity_async(id))
    }

    pub fn load_sessions(&self) -> Result<Sessions> {
        ready(self.load_sessions_async())
    }

    pub fn save_sessions(&self, s: &Sessions) -> Result<()> {
        ready(self.save_sessions_async(s))
    }

    pub fn load_quarantine(&self) -> Result<Sessions> {
        ready(self.load_quarantine_async())
    }

    pub fn save_quarantine(&self, s: &Sessions) -> Result<()> {
        ready(self.save_quarantine_async(s))
    }

    pub fn load_owner(&self) -> Result<OwnerKey> {
        ready(self.load_owner_async())
    }

    pub fn load_device_list(&self) -> Result<DeviceList> {
        ready(self.load_device_list_async())
    }

    pub fn save_device_list(&self, l: &DeviceList) -> Result<()> {
        ready(self.save_device_list_async(l))
    }

    pub fn install_identity(&self, owner: &OwnerKey, list: &DeviceList) -> Result<()> {
        ready(self.install_identity_async(owner, list))
    }

    pub fn accept_own_list(&self, offered: &DeviceList) -> Result<OwnUpdate> {
        ready(self.accept_own_list_async(offered))
    }

    pub fn this_device_is_listed(&self) -> Result<bool> {
        ready(self.this_device_is_listed_async())
    }

    pub fn identity_card(&self) -> Result<String> {
        ready(self.identity_card_async())
    }

    pub fn add_contact(&self, name: &str, card: &str, force: bool) -> Result<Added> {
        ready(self.add_contact_async(name, card, force))
    }

    pub fn save_attachment(&self, msg_id: &str, bytes: &[u8]) -> Result<()> {
        ready(self.save_attachment_async(msg_id, bytes))
    }

    pub fn load_attachment(&self, msg_id: &str) -> Result<Option<Vec<u8>>> {
        ready(self.load_attachment_async(msg_id))
    }

    pub fn delete_attachment(&self, msg_id: &str) -> Result<()> {
        ready(self.delete_attachment_async(msg_id))
    }

    pub fn load_bytes(&self, name: &str) -> Result<Option<Vec<u8>>> {
        ready(self.load_bytes_async(name))
    }

    pub fn save_bytes(&self, name: &str, bytes: &[u8]) -> Result<()> {
        ready(self.save_bytes_async(name, bytes))
    }

    pub fn read_marker(&self, peer: &str) -> Result<u64> {
        ready(self.read_marker_async(peer))
    }

    pub fn advance_read_marker(&self, peer: &str, upto: u64) -> Result<bool> {
        ready(self.advance_read_marker_async(peer, upto))
    }

    pub fn last_sync(&self) -> u64 {
        ready(self.last_sync_async())
    }

    pub fn set_last_sync(&self, ms: u64) -> Result<()> {
        ready(self.set_last_sync_async(ms))
    }

    pub fn next_lamport(&self) -> Result<u64> {
        ready(self.next_lamport_async())
    }

    pub fn observe_lamport(&self, seen: u64) -> Result<()> {
        ready(self.observe_lamport_async(seen))
    }

    pub fn load_contacts(&self) -> Result<Vec<Contact>> {
        ready(self.load_contacts_async())
    }

    pub fn save_contacts(&self, c: &[Contact]) -> Result<()> {
        ready(self.save_contacts_async(c))
    }

    pub fn load_json<T: serde::de::DeserializeOwned + Default>(&self, name: &str) -> Result<T> {
        ready(self.load_json_async(name))
    }

    pub fn save_json<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<()> {
        ready(self.save_json_async(name, value))
    }

    /// Encrypted message history (see [`crate::messages`]). SQLite, so it stays with the state directory
    /// until the chat store gets its own trait (ADR 0033, step 4).
    pub fn messages(&self) -> Result<MessageStore<'_>> {
        MessageStore::open(&self.store.dir().join(MESSAGES), &self.vault)
    }
}
