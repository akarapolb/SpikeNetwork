//! Backup and recovery of an identity (ADR 0029).
//!
//! `export` seals the person's owner key, contacts, groups and (if asked) history into a file with a
//! separate backup passphrase. `Opened::restore` turns such a file into a *new device* of the same
//! person: fresh device keys, a device list signed by the restored owner key that names only that
//! device, and nothing of the lost device's account, prekeys or ratchets.

use crate::groups;
use crate::messages::StoredMessage;
use crate::state::{Contact, Home};
use crate::{client::Relay, ops, transport::Target};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::backup as container;
use spike_core::owner::{DeviceEntry, DeviceList, OwnerKey};
use spike_core::vault::KdfParams;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

pub use container::{backup_kdf, created_at, generate_key, MIN_PASSPHRASE_CHARS};

const STATE_FILE: &str = "backup.enc";
const STAGING: &str = "restore-staging";
/// A backup older than this is reported as out of date.
const STALE_DAYS: u64 = 90;

/// What travels inside the sealed file.
#[derive(Serialize, Deserialize)]
struct Payload {
    v: u8,
    owner_key: String,
    /// Only its version is used on restore, and only as a lower bound.
    device_list: DeviceList,
    contacts: Vec<Contact>,
    #[serde(default)]
    groups: Vec<groups::Record>,
    #[serde(default)]
    member_cards: BTreeMap<String, String>,
    /// Empty unless the person chose to include history.
    #[serde(default)]
    messages: Vec<StoredMessage>,
    /// Settings the app layer wants to carry (the relay address, say), by name.
    #[serde(default)]
    extra: BTreeMap<String, String>,
}

/// What this device remembers about its last backup. Sealed with everything else; never in the file.
#[derive(Serialize, Deserialize, Default)]
struct State {
    last_ms: u64,
    contacts: usize,
    groups: usize,
}

/// Whether the person has a backup and whether it is still good.
#[derive(Debug, Serialize)]
pub struct Status {
    pub last_ms: Option<u64>,
    pub stale: bool,
    pub reason: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn status(home: &Home) -> Result<Status> {
    let st: State = home.load_json(STATE_FILE)?;
    if st.last_ms == 0 {
        return Ok(Status {
            last_ms: None,
            stale: true,
            reason: Some("You have not made a backup yet.".into()),
        });
    }
    let (contacts, groups) = (
        home.load_contacts()?.len(),
        groups::export_for_link(home)?.0.len(),
    );
    let age_days = now_ms().saturating_sub(st.last_ms) / 86_400_000;
    let reason = if age_days >= STALE_DAYS {
        Some(format!("Your backup is {age_days} days old."))
    } else if contacts != st.contacts || groups != st.groups {
        Some("Your contacts or groups have changed since your last backup.".into())
    } else {
        None
    };
    Ok(Status {
        last_ms: Some(st.last_ms),
        stale: reason.is_some(),
        reason,
    })
}

/// Seal a backup of this device's identity. Remembers when, for [`status`].
pub fn export(
    home: &Home,
    passphrase: &str,
    kdf: KdfParams,
    include_history: bool,
    extra: BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    let (groups, member_cards) = groups::export_for_link(home)?;
    let contacts = home.load_contacts()?;
    let payload = Payload {
        v: 1,
        owner_key: home.load_owner()?.to_hex(),
        device_list: home.load_device_list()?,
        messages: if include_history {
            home.messages()?.all()?
        } else {
            Vec::new()
        },
        extra,
        contacts,
        groups,
        member_cards,
    };
    let created = now_ms();
    let plain = Zeroizing::new(serde_json::to_vec(&payload)?);
    let file = container::seal(passphrase, kdf, created, &plain)?;
    home.save_json(
        STATE_FILE,
        &State {
            last_ms: created,
            contacts: payload.contacts.len(),
            groups: payload.groups.len(),
        },
    )?;
    Ok(file)
}

/// A backup file opened with its passphrase, not yet restored.
pub struct Opened {
    created_ms: u64,
    /// Cleared from memory when dropped (best effort: the parsed copy below is not).
    raw: Zeroizing<Vec<u8>>,
    payload: Payload,
}

impl Opened {
    pub fn open(file: &[u8], passphrase: &str) -> Result<Self> {
        let (created_ms, raw) = container::open(passphrase, file).map_err(|e| match e {
            spike_core::Error::WrongPassphrase => anyhow!("wrong backup passphrase"),
            other => anyhow!("{other}"),
        })?;
        let payload: Payload =
            serde_json::from_slice(&raw).context("this backup file is damaged")?;
        if payload.v != 1 {
            bail!("this backup was made by a newer version of the app");
        }
        // Everything is checked again: the key must match the list, and the list must verify.
        let owner = OwnerKey::from_hex(&payload.owner_key)?;
        if payload.device_list.owner != hex::encode(owner.public()) {
            bail!("this backup file is damaged");
        }
        payload.device_list.verify()?;
        Ok(Self {
            created_ms,
            raw: Zeroizing::new(raw),
            payload,
        })
    }

    pub fn created_ms(&self) -> u64 {
        self.created_ms
    }
    pub fn contacts(&self) -> usize {
        self.payload.contacts.len()
    }
    pub fn messages(&self) -> usize {
        self.payload.messages.len()
    }
    pub fn groups(&self) -> usize {
        self.payload.groups.len()
    }
    /// Settings the app stored in the file, by name.
    pub fn extra(&self) -> &BTreeMap<String, String> {
        &self.payload.extra
    }
    /// The decrypted payload as stored. For tests that check what the file does and does not hold.
    pub fn raw_payload(&self) -> &[u8] {
        &self.raw
    }

    /// Make this person's identity live in `dir` as a new device. `install` runs on the new, empty
    /// state with the file's `extra` settings and returns the relay to use. Needs that relay: the new
    /// device list must be newer than whatever it already holds. On any failure nothing is left in
    /// `dir`. Refuses if `dir` already holds an identity.
    pub async fn restore(
        self,
        dir: &Path,
        new_passphrase: &str,
        kdf: KdfParams,
        install: impl FnOnce(&Home, &BTreeMap<String, String>) -> Result<Target>,
    ) -> Result<Restored> {
        if Home::exists(dir) {
            bail!("this device already has an identity; restore on a fresh install");
        }
        let staging = dir.join(STAGING);
        std::fs::remove_dir_all(&staging).ok();
        let built = self.build(&staging, new_passphrase, kdf, install).await;
        let restored = match built {
            Ok(r) => r,
            Err(e) => {
                std::fs::remove_dir_all(&staging).ok();
                return Err(e);
            }
        };
        let moved = move_into(&staging, dir);
        std::fs::remove_dir_all(&staging).ok();
        moved?;
        Ok(restored)
    }

    async fn build(
        &self,
        staging: &Path,
        new_passphrase: &str,
        kdf: KdfParams,
        install: impl FnOnce(&Home, &BTreeMap<String, String>) -> Result<Target>,
    ) -> Result<Restored> {
        let p = &self.payload;
        let home = Home::create(staging, new_passphrase, kdf, false)?;
        let target = install(&home, &p.extra)?;
        let owner = OwnerKey::from_hex(&p.owner_key)?;
        let mut me = home.load_identity()?;
        let mut relay = Relay::connect(target.clone(), &me)
            .await
            .context("the relay could not be reached; restoring needs it")?;

        // Number above everything that exists: the file's list and the newest the relay holds.
        let mut version = p.device_list.version;
        if let Some(latest) = relay.fetch_device_list(&p.device_list.owner).await? {
            // A list the relay makes up (wrong owner, bad signature) is ignored, not trusted.
            if latest.owner == p.device_list.owner && latest.verify().is_ok() {
                version = version.max(latest.version);
            }
        }
        let list = DeviceList::sign(
            &owner,
            version + 1,
            vec![DeviceEntry {
                card: me.card().encode(),
                relay: Some(target.url.clone()),
            }],
        )?;
        home.install_identity(&owner, &list)?;
        relay
            .publish_device_list(&list)
            .await
            .context("publishing the new device list")?;
        ops::top_up_prekeys(&mut relay, &home, &mut me).await?;

        home.save_contacts(&p.contacts)?;
        groups::install_from_link(&home, p.groups.clone(), p.member_cards.clone())?;
        let store = home.messages()?;
        let mut messages = 0;
        for m in &p.messages {
            messages += usize::from(store.insert(m)?);
        }
        drop(store);
        // Bring the contacts' cards up to date (best effort: they also refresh when they next send).
        ops::refresh_contacts(&mut relay, &home, None).await.ok();
        home.save_json(
            STATE_FILE,
            &State {
                last_ms: self.created_ms,
                contacts: p.contacts.len(),
                groups: groups::export_for_link(&home)?.0.len(),
            },
        )?;
        Ok(Restored {
            version: list.version,
            contacts: p.contacts.len(),
            messages,
            groups: p.groups.len(),
        })
    }
}

/// Move every entry of `staging` into `dir`, the vault header last: a state without it does not
/// count as an identity, so an interrupted move can simply be run again.
fn move_into(staging: &Path, dir: &Path) -> Result<()> {
    let mut header = None;
    for e in std::fs::read_dir(staging)? {
        let e = e?;
        if e.file_name() == "vault.json" {
            header = Some(e.path());
            continue;
        }
        let to = dir.join(e.file_name());
        if to.is_dir() {
            std::fs::remove_dir_all(&to).ok();
        }
        std::fs::rename(e.path(), to)?;
    }
    if let Some(h) = header {
        std::fs::rename(h, dir.join("vault.json"))?;
    }
    Ok(())
}

/// What a restore did.
#[derive(Debug)]
pub struct Restored {
    pub version: u64,
    pub contacts: usize,
    pub messages: usize,
    pub groups: usize,
}
