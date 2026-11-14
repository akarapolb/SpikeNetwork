//! Encryption of state at rest.
//!
//! A passphrase is stretched with Argon2id into a 32-byte key. Each stored
//! file is sealed with XChaCha20-Poly1305 using a fresh random nonce and the
//! file's *name* as associated data, so a file cannot be silently swapped for
//! another vault file or modified. This protects against theft of the disk or a
//! backup; it does not protect a running, unlocked device.
//!
//! Changing the passphrase does not re-encrypt anything: the header is
//! rewritten to hold the same vault key, sealed under a key stretched from
//! the new passphrase (version 2). Version 1 headers, where the vault key *is*
//! the stretched passphrase, still unlock.

use crate::{Error, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::ZeroizeOnDrop;

const MAGIC: &[u8; 4] = b"SPK1";
const CHECK_NAME: &str = "vault-check";
const CHECK_PLAINTEXT: &[u8] = b"spike-vault-ok";
const VERSION: u32 = 1;
/// The vault key is sealed under the passphrase key (`wrapped`), so the passphrase can change.
const VERSION_WRAPPED: u32 = 2;
const WRAP_NAME: &str = "vault-key";
/// Refuse absurd parameters read from disk (denial of service on unlock).
const MAX_MEMORY_KIB: u32 = 1024 * 1024;
const MAX_ITERATIONS: u32 = 20;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for KdfParams {
    /// 64 MiB, 3 passes: above the OWASP minimum for Argon2id.
    fn default() -> Self {
        Self {
            memory_kib: 64 * 1024,
            iterations: 3,
            parallelism: 1,
        }
    }
}

impl KdfParams {
    /// Deliberately weak. Only for tests.
    pub fn insecure_for_tests() -> Self {
        Self {
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        }
    }
}

/// Stored next to the encrypted files (not secret).
#[derive(Serialize, Deserialize)]
pub struct VaultHeader {
    version: u32,
    salt: String,
    kdf: KdfParams,
    /// A known plaintext sealed with the key, to detect a wrong passphrase.
    check: String,
    /// Version 2: the vault key, sealed under the key stretched from the passphrase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wrapped: Option<String>,
}

impl VaultHeader {
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

#[derive(ZeroizeOnDrop)]
pub struct Vault {
    key: [u8; 32],
}

impl Vault {
    /// Create a new vault for `passphrase`. Store the returned header.
    pub fn create(passphrase: &str, kdf: KdfParams) -> Result<(Vault, VaultHeader)> {
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let vault = Vault {
            key: derive(passphrase, &salt, kdf)?,
        };
        let header = VaultHeader {
            version: VERSION,
            salt: hex::encode(salt),
            kdf,
            check: hex::encode(vault.seal(CHECK_NAME, CHECK_PLAINTEXT)?),
            wrapped: None,
        };
        Ok((vault, header))
    }

    pub fn unlock(passphrase: &str, header: &VaultHeader) -> Result<Vault> {
        let salt = hex::decode(&header.salt)?;
        let stretched = Vault {
            key: derive(passphrase, &salt, header.kdf)?,
        };
        let vault = match (header.version, &header.wrapped) {
            (VERSION, None) => stretched,
            (VERSION_WRAPPED, Some(w)) => {
                let mut key = stretched
                    .open(WRAP_NAME, &hex::decode(w)?)
                    .map_err(|_| Error::WrongPassphrase)?;
                let vault = Vault {
                    key: key.as_slice().try_into().map_err(|_| Error::Corrupted)?,
                };
                zeroize::Zeroize::zeroize(&mut key);
                vault
            }
            _ => return Err(Error::Corrupted),
        };
        let check = hex::decode(&header.check)?;
        match vault.open(CHECK_NAME, &check) {
            Ok(p) if p == CHECK_PLAINTEXT => Ok(vault),
            _ => Err(Error::WrongPassphrase),
        }
    }

    /// A new header that opens this same vault with `new_passphrase` and no longer with the old one.
    /// Everything sealed so far stays readable, and index tags do not change, because the vault key
    /// stays the same. `old_passphrase` must open `current`: the caller proves it knows it.
    pub fn change_passphrase(
        old_passphrase: &str,
        new_passphrase: &str,
        current: &VaultHeader,
    ) -> Result<VaultHeader> {
        let vault = Vault::unlock(old_passphrase, current)?;
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let kek = Vault {
            key: derive(new_passphrase, &salt, current.kdf)?,
        };
        Ok(VaultHeader {
            version: VERSION_WRAPPED,
            salt: hex::encode(salt),
            kdf: current.kdf,
            check: hex::encode(vault.seal(CHECK_NAME, CHECK_PLAINTEXT)?),
            wrapped: Some(hex::encode(kek.seal(WRAP_NAME, &vault.key)?)),
        })
    }

    pub fn seal(&self, name: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let ct = XChaCha20Poly1305::new((&self.key).into())
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: name.as_bytes(),
                },
            )
            .map_err(|_| Error::Corrupted)?;
        let mut out = Vec::with_capacity(4 + 24 + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// A deterministic keyed tag (HMAC-SHA256) for indexing sealed data by a
    /// value without storing the value: equal inputs give equal tags under the
    /// same vault, and nothing about the input is learnable without the key.
    /// `label` separates uses, so a tag for one purpose never matches another.
    /// Uses a subkey derived from the vault key, never the AEAD key itself.
    pub fn index_tag(&self, label: &str, data: &[u8]) -> [u8; 32] {
        type H = Hmac<Sha256>;
        let mut sub = <H as KeyInit>::new_from_slice(&self.key).expect("any key length");
        sub.update(b"spike-vault-index-key-v1");
        let subkey = sub.finalize().into_bytes();
        let mut m = <H as KeyInit>::new_from_slice(&subkey).expect("any key length");
        m.update(label.as_bytes());
        m.update(&[0]);
        m.update(data);
        m.finalize().into_bytes().into()
    }

    pub fn open(&self, name: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < 4 + 24 + 16 || &sealed[..4] != MAGIC {
            return Err(Error::Corrupted);
        }
        XChaCha20Poly1305::new((&self.key).into())
            .decrypt(
                &XNonce::try_from(&sealed[4..28]).map_err(|_| Error::Corrupted)?,
                Payload {
                    msg: &sealed[28..],
                    aad: name.as_bytes(),
                },
            )
            .map_err(|_| Error::Corrupted)
    }
}

fn derive(passphrase: &str, salt: &[u8], kdf: KdfParams) -> Result<[u8; 32]> {
    if kdf.memory_kib > MAX_MEMORY_KIB || kdf.iterations > MAX_ITERATIONS || salt.len() < 8 {
        return Err(Error::Corrupted);
    }
    let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
        .map_err(|_| Error::Corrupted)?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| Error::Corrupted)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(p: &str) -> (Vault, VaultHeader) {
        Vault::create(p, KdfParams::insecure_for_tests()).unwrap()
    }

    #[test]
    fn roundtrip_and_unlock() {
        let (v, h) = make("correct horse");
        let sealed = v.seal("identity.json", b"secret bytes").unwrap();
        let h = VaultHeader::from_json(&h.to_json().unwrap()).unwrap();
        let v2 = Vault::unlock("correct horse", &h).unwrap();
        assert_eq!(v2.open("identity.json", &sealed).unwrap(), b"secret bytes");
    }

    #[test]
    fn wrong_passphrase_is_detected() {
        let (_, h) = make("right");
        assert!(matches!(
            Vault::unlock("wrong", &h),
            Err(Error::WrongPassphrase)
        ));
    }

    #[test]
    fn plaintext_is_not_visible() {
        let (v, _) = make("p");
        let marker = b"very-recognisable-plaintext";
        let sealed = v.seal("f", marker).unwrap();
        assert!(!sealed.windows(marker.len()).any(|w| w == marker));
        assert_ne!(sealed, v.seal("f", marker).unwrap(), "nonce must be fresh");
    }

    #[test]
    fn tamper_and_file_swap_are_detected() {
        let (v, _) = make("p");
        let sealed = v.seal("sessions.json", b"data").unwrap();
        for i in [0, 10, 30, sealed.len() - 1] {
            let mut t = sealed.clone();
            t[i] ^= 1;
            assert!(v.open("sessions.json", &t).is_err(), "flip at {i} accepted");
        }
        // The same ciphertext presented under another file name must not open.
        assert!(v.open("contacts.json", &sealed).is_err());
        assert!(v
            .open("sessions.json", &sealed[..sealed.len() - 1])
            .is_err());
    }

    #[test]
    fn changing_the_passphrase_keeps_the_data_and_retires_the_old_one() {
        let (v, h) = make("first passphrase");
        let sealed = v.seal("contacts.enc", b"alice and bob").unwrap();
        let tag = v.index_tag("conv", b"bob");

        assert!(matches!(
            Vault::change_passphrase("not the passphrase", "second", &h),
            Err(Error::WrongPassphrase)
        ));
        let h2 = Vault::change_passphrase("first passphrase", "second passphrase", &h).unwrap();
        let h2 = VaultHeader::from_json(&h2.to_json().unwrap()).unwrap();
        assert!(matches!(
            Vault::unlock("first passphrase", &h2),
            Err(Error::WrongPassphrase)
        ));
        let v2 = Vault::unlock("second passphrase", &h2).unwrap();
        assert_eq!(v2.open("contacts.enc", &sealed).unwrap(), b"alice and bob");
        assert_eq!(v2.index_tag("conv", b"bob"), tag, "index tags survive");

        // And again, from a version 2 header.
        let h3 = Vault::change_passphrase("second passphrase", "third passphrase", &h2).unwrap();
        assert!(Vault::unlock("second passphrase", &h3).is_err());
        let v3 = Vault::unlock("third passphrase", &h3).unwrap();
        assert_eq!(v3.open("contacts.enc", &sealed).unwrap(), b"alice and bob");

        // A tampered wrapped key is refused, not half-opened.
        let mut bad = VaultHeader::from_json(&h3.to_json().unwrap()).unwrap();
        let w = bad.wrapped.clone().unwrap();
        let flip = if w.ends_with('0') { "1" } else { "0" };
        bad.wrapped = Some(format!("{}{flip}", &w[..w.len() - 1]));
        assert!(Vault::unlock("third passphrase", &bad).is_err());
        // A version 2 header without its wrapped key is corrupted.
        bad.wrapped = None;
        assert!(matches!(
            Vault::unlock("third passphrase", &bad),
            Err(Error::Corrupted)
        ));
    }

    #[test]
    fn hostile_parameters_are_refused() {
        let (_, mut h) = make("p");
        h.kdf.memory_kib = u32::MAX;
        assert!(Vault::unlock("p", &h).is_err());
    }

    #[test]
    fn index_tags_are_deterministic_label_separated_and_vault_bound() {
        let (a, _) = make("p");
        let (b, _) = make("p");
        assert_eq!(a.index_tag("conv", b"bob"), a.index_tag("conv", b"bob"));
        assert_ne!(a.index_tag("conv", b"bob"), a.index_tag("conv", b"carol"));
        assert_ne!(a.index_tag("conv", b"bob"), a.index_tag("msg", b"bob"));
        // Label/data boundary is unambiguous.
        assert_ne!(a.index_tag("ab", b"c"), a.index_tag("a", b"bc"));
        assert_ne!(
            a.index_tag("conv", b"bob"),
            b.index_tag("conv", b"bob"),
            "tags must depend on the vault key"
        );
    }

    #[test]
    fn different_vaults_do_not_open_each_other() {
        let (a, _) = make("same");
        let (b, _) = make("same");
        let sealed = a.seal("f", b"x").unwrap();
        assert!(b.open("f", &sealed).is_err(), "salts must differ");
    }
}
