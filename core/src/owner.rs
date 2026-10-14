//! The person behind the devices (multi-device milestone M2).
//!
//! An [`OwnerKey`] (Ed25519) represents a person. It never signs messages; it
//! only signs a versioned [`DeviceList`] naming that person's devices (each a
//! [`DeviceCard`] with its own Olm keys). Contacts verify the owner key once,
//! by fingerprint, and then trust any device the owner key signs.
//!
//! An [`IdentityCard`] is what people exchange: `spike2:` + the signed list.
//! Older single-device `spike1:` cards are still accepted as "legacy" cards
//! whose owner key is the device's own Ed25519 key and whose version is 0.

use crate::identity::DeviceCard;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vodozemac::{Ed25519PublicKey, Ed25519SecretKey, Ed25519Signature};

const LIST_CTX: &[u8] = b"spike-device-list-v1";
const FP_CTX: &[u8] = b"spike-owner-fp-v1";
const CARD2_PREFIX: &str = "spike2:";
/// Upper bound on devices per person: keeps cards and fan-out bounded.
pub const MAX_DEVICES: usize = 16;

pub struct OwnerKey(Ed25519SecretKey);

impl OwnerKey {
    pub fn generate() -> Self {
        Self(Ed25519SecretKey::new())
    }
    pub fn to_hex(&self) -> String {
        hex::encode(*self.0.to_bytes())
    }
    pub fn from_hex(s: &str) -> Result<Self> {
        let b: [u8; 32] = hex::decode(s.trim())?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        Ok(Self(Ed25519SecretKey::from_slice(&b)))
    }
    pub fn public(&self) -> [u8; 32] {
        *self.0.public_key().as_bytes()
    }
}

/// One device in a list: its device card (`spike1:`) and, optionally, the relay it reads from.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DeviceEntry {
    pub card: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
}

/// The part that is signed. Field order is the signing order; do not reorder.
#[derive(Serialize)]
struct Signed<'a> {
    owner: &'a str,
    version: u64,
    devices: &'a [DeviceEntry],
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DeviceList {
    /// Owner Ed25519 public key, hex.
    pub owner: String,
    /// Starts at 1, only ever increases.
    pub version: u64,
    pub devices: Vec<DeviceEntry>,
    /// Owner signature over the fields above, hex.
    pub sig: String,
}

impl DeviceList {
    fn signing_bytes(owner: &str, version: u64, devices: &[DeviceEntry]) -> Vec<u8> {
        let body = serde_json::to_vec(&Signed {
            owner,
            version,
            devices,
        })
        .expect("serializes");
        [LIST_CTX, &body].concat()
    }

    /// Sign a list. Validates it first so an owner can never sign nonsense.
    pub fn sign(owner: &OwnerKey, version: u64, devices: Vec<DeviceEntry>) -> Result<Self> {
        let owner_hex = hex::encode(owner.public());
        Self::check_shape(version, &devices)?;
        let sig = owner
            .0
            .sign(&Self::signing_bytes(&owner_hex, version, &devices))
            .to_bytes();
        Ok(Self {
            owner: owner_hex,
            version,
            devices,
            sig: hex::encode(sig),
        })
    }

    fn check_shape(version: u64, devices: &[DeviceEntry]) -> Result<Vec<DeviceCard>> {
        if version == 0 {
            return Err(Error::BadDeviceList("version must start at 1"));
        }
        if devices.is_empty() || devices.len() > MAX_DEVICES {
            return Err(Error::BadDeviceList("needs between 1 and 16 devices"));
        }
        let cards: Vec<DeviceCard> = devices
            .iter()
            .map(|d| DeviceCard::decode(&d.card))
            .collect::<Result<_>>()?;
        let mut ids: Vec<String> = cards.iter().map(DeviceCard::device_id).collect();
        ids.sort();
        ids.dedup();
        if ids.len() != cards.len() {
            return Err(Error::BadDeviceList("duplicate device"));
        }
        Ok(cards)
    }

    /// Check the signature and contents. Returns the device cards.
    pub fn verify(&self) -> Result<Vec<DeviceCard>> {
        let owner: [u8; 32] = hex::decode(&self.owner)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let sig: [u8; 64] = hex::decode(&self.sig)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let pk = Ed25519PublicKey::from_slice(&owner).map_err(|_| Error::BadSignature)?;
        let s = Ed25519Signature::from_slice(&sig).map_err(|_| Error::BadSignature)?;
        pk.verify(
            &Self::signing_bytes(&self.owner, self.version, &self.devices),
            &s,
        )
        .map_err(|_| Error::BadSignature)?;
        Self::check_shape(self.version, &self.devices)
    }

    /// A new list, version + 1, with `entry` added.
    pub fn with_device(&self, owner: &OwnerKey, entry: DeviceEntry) -> Result<Self> {
        self.owned_by(owner)?;
        let mut devices = self.devices.clone();
        devices.push(entry);
        Self::sign(owner, self.version + 1, devices)
    }

    /// A new list, version + 1, without the device whose id is `device_id`.
    pub fn without_device(&self, owner: &OwnerKey, device_id: &str) -> Result<Self> {
        self.owned_by(owner)?;
        let devices: Vec<DeviceEntry> = self
            .devices
            .iter()
            .filter(|d| {
                DeviceCard::decode(&d.card)
                    .map(|c| c.device_id() != device_id)
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        if devices.len() == self.devices.len() {
            return Err(Error::BadDeviceList("no such device"));
        }
        Self::sign(owner, self.version + 1, devices)
    }

    fn owned_by(&self, owner: &OwnerKey) -> Result<()> {
        if self.owner != hex::encode(owner.public()) {
            return Err(Error::KeyChange);
        }
        Ok(())
    }
}

/// What a contact holds about a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityCard {
    owner: [u8; 32],
    list: Option<DeviceList>,
    devices: Vec<DeviceCard>,
}

/// How a newly offered card relates to the one already stored for a contact.
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// Identical; nothing to do.
    Same,
    /// Same person, newer device list: accept.
    Newer,
    /// A legacy single-device contact now presents a signed list that still
    /// includes that device: accept as an upgrade to the identity model.
    Upgrade,
}

impl IdentityCard {
    pub fn encode(list: &DeviceList) -> String {
        format!(
            "{CARD2_PREFIX}{}",
            hex::encode(serde_json::to_vec(list).expect("serializes"))
        )
    }

    /// Decode and fully verify a `spike2:` card, or accept a legacy `spike1:` device card.
    pub fn decode(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some(body) = s.strip_prefix(CARD2_PREFIX) {
            let list: DeviceList = serde_json::from_slice(&hex::decode(body)?)
                .map_err(|_| Error::BadCard("not a device list"))?;
            let devices = list.verify()?;
            let owner = hex::decode(&list.owner)?
                .try_into()
                .map_err(|_| Error::Malformed)?;
            return Ok(Self {
                owner,
                list: Some(list),
                devices,
            });
        }
        let device = DeviceCard::decode(s)?;
        Ok(Self {
            owner: device.ed25519,
            list: None,
            devices: vec![device],
        })
    }

    pub fn owner_hex(&self) -> String {
        hex::encode(self.owner)
    }
    /// 0 for a legacy single-device card.
    pub fn version(&self) -> u64 {
        self.list.as_ref().map_or(0, |l| l.version)
    }
    pub fn is_legacy(&self) -> bool {
        self.list.is_none()
    }
    pub fn devices(&self) -> &[DeviceCard] {
        &self.devices
    }

    /// What people compare to verify each other. For a signed card it covers
    /// the owner key only, so it stays the same when devices are added. A
    /// legacy card keeps its old device fingerprint.
    pub fn fingerprint(&self) -> String {
        if self.is_legacy() {
            return self.devices[0].fingerprint();
        }
        let d = Sha256::new()
            .chain_update(FP_CTX)
            .chain_update(self.owner)
            .finalize();
        let s = hex::encode(&d[..15]);
        s.as_bytes()
            .chunks(5)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Decide whether `new` may replace `self`. Refuses rollbacks, owner key
    /// changes, and two different lists claiming the same version.
    pub fn check_update(&self, new: &IdentityCard) -> Result<Update> {
        if self.is_legacy() && !new.is_legacy() {
            let old_device = &self.devices[0];
            return if new.devices.contains(old_device) {
                Ok(Update::Upgrade)
            } else {
                Err(Error::KeyChange)
            };
        }
        if new.is_legacy() && !self.is_legacy() {
            return Err(Error::Rollback {
                seen: self.version(),
                offered: 0,
            });
        }
        if self.owner != new.owner {
            return Err(Error::KeyChange);
        }
        match new.version().cmp(&self.version()) {
            std::cmp::Ordering::Less => Err(Error::Rollback {
                seen: self.version(),
                offered: new.version(),
            }),
            std::cmp::Ordering::Equal if self == new => Ok(Update::Same),
            std::cmp::Ordering::Equal => Err(Error::Equivocation(new.version())),
            std::cmp::Ordering::Greater => Ok(Update::Newer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn entry(id: &Identity) -> DeviceEntry {
        DeviceEntry {
            card: id.card().encode(),
            relay: None,
        }
    }

    #[test]
    fn signed_list_roundtrips_through_a_card() {
        let owner = OwnerKey::generate();
        let phone = Identity::generate().unwrap();
        let list = DeviceList::sign(&owner, 1, vec![entry(&phone)]).unwrap();
        let card = IdentityCard::decode(&IdentityCard::encode(&list)).unwrap();
        assert_eq!(card.version(), 1);
        assert_eq!(card.devices(), &[phone.card()]);
        assert_eq!(card.owner_hex(), hex::encode(owner.public()));
        assert!(!card.is_legacy());
        // Owner key survives persistence.
        assert_eq!(
            OwnerKey::from_hex(&owner.to_hex()).unwrap().public(),
            owner.public()
        );
    }

    #[test]
    fn tampered_or_forged_lists_are_rejected() {
        let owner = OwnerKey::generate();
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let list = DeviceList::sign(&owner, 3, vec![entry(&a)]).unwrap();
        let mut bumped = list.clone();
        bumped.version = 9;
        let mut added = list.clone();
        added.devices.push(entry(&b));
        let mut other_owner = list.clone();
        other_owner.owner = hex::encode(OwnerKey::generate().public());
        for bad in [bumped, added, other_owner] {
            assert!(bad.verify().is_err());
            assert!(IdentityCard::decode(&IdentityCard::encode(&bad)).is_err());
        }
    }

    #[test]
    fn shape_rules() {
        let owner = OwnerKey::generate();
        let a = Identity::generate().unwrap();
        assert!(
            DeviceList::sign(&owner, 0, vec![entry(&a)]).is_err(),
            "version 0 is reserved for legacy cards"
        );
        assert!(DeviceList::sign(&owner, 1, vec![]).is_err());
        assert!(
            DeviceList::sign(&owner, 1, vec![entry(&a), entry(&a)]).is_err(),
            "duplicate device"
        );
        let many: Vec<DeviceEntry> = (0..=MAX_DEVICES)
            .map(|_| entry(&Identity::generate().unwrap()))
            .collect();
        assert!(DeviceList::sign(&owner, 1, many).is_err());
    }

    #[test]
    fn adding_and_removing_devices_bumps_the_version() {
        let owner = OwnerKey::generate();
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let v1 = DeviceList::sign(&owner, 1, vec![entry(&a)]).unwrap();
        let v2 = v1.with_device(&owner, entry(&b)).unwrap();
        assert_eq!((v2.version, v2.verify().unwrap().len()), (2, 2));
        let v3 = v2.without_device(&owner, &a.device_id()).unwrap();
        assert_eq!(v3.version, 3);
        assert_eq!(v3.verify().unwrap(), vec![b.card()]);
        assert!(
            v3.without_device(&owner, &a.device_id()).is_err(),
            "already removed"
        );
        assert!(matches!(
            v1.with_device(&OwnerKey::generate(), entry(&b)),
            Err(Error::KeyChange)
        ));
    }

    #[test]
    fn update_rules_refuse_rollback_key_change_and_equivocation() {
        let owner = OwnerKey::generate();
        let (a, b, c) = (
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
        );
        let v1 = DeviceList::sign(&owner, 1, vec![entry(&a)]).unwrap();
        let v2 = v1.with_device(&owner, entry(&b)).unwrap();
        let card = |l: &DeviceList| IdentityCard::decode(&IdentityCard::encode(l)).unwrap();
        assert_eq!(card(&v1).check_update(&card(&v1)).unwrap(), Update::Same);
        assert_eq!(card(&v1).check_update(&card(&v2)).unwrap(), Update::Newer);
        assert!(matches!(
            card(&v2).check_update(&card(&v1)),
            Err(Error::Rollback {
                seen: 2,
                offered: 1
            })
        ));
        let fork = DeviceList::sign(&owner, 2, vec![entry(&a), entry(&c)]).unwrap();
        assert!(matches!(
            card(&v2).check_update(&card(&fork)),
            Err(Error::Equivocation(2))
        ));
        let stranger = DeviceList::sign(&OwnerKey::generate(), 5, vec![entry(&a)]).unwrap();
        assert!(matches!(
            card(&v2).check_update(&card(&stranger)),
            Err(Error::KeyChange)
        ));
        // A signed contact cannot be downgraded to an unsigned legacy card.
        let legacy = IdentityCard::decode(&a.card().encode()).unwrap();
        assert!(matches!(
            card(&v2).check_update(&legacy),
            Err(Error::Rollback { .. })
        ));
    }

    #[test]
    fn legacy_cards_still_work_and_can_upgrade() {
        let a = Identity::generate().unwrap();
        let legacy = IdentityCard::decode(&a.card().encode()).unwrap();
        assert!(legacy.is_legacy());
        assert_eq!(legacy.version(), 0);
        assert_eq!(
            legacy.fingerprint(),
            a.card().fingerprint(),
            "legacy fingerprints are unchanged"
        );
        let owner = OwnerKey::generate();
        let signed = |devs: Vec<DeviceEntry>| {
            IdentityCard::decode(&IdentityCard::encode(
                &DeviceList::sign(&owner, 1, devs).unwrap(),
            ))
            .unwrap()
        };
        assert_eq!(
            legacy.check_update(&signed(vec![entry(&a)])).unwrap(),
            Update::Upgrade
        );
        let other = Identity::generate().unwrap();
        assert!(
            matches!(
                legacy.check_update(&signed(vec![entry(&other)])),
                Err(Error::KeyChange)
            ),
            "upgrade must keep the known device"
        );
    }

    #[test]
    fn owner_fingerprint_is_stable_across_device_changes() {
        let owner = OwnerKey::generate();
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let v1 = DeviceList::sign(&owner, 1, vec![entry(&a)]).unwrap();
        let v2 = v1.with_device(&owner, entry(&b)).unwrap();
        let fp = |l: &DeviceList| {
            IdentityCard::decode(&IdentityCard::encode(l))
                .unwrap()
                .fingerprint()
        };
        assert_eq!(fp(&v1), fp(&v2));
        assert_ne!(
            fp(&v1),
            IdentityCard::decode(&a.card().encode())
                .unwrap()
                .fingerprint()
        );
    }
}
