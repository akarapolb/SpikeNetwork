//! The container of an identity backup file (ADR 0029).
//!
//! A backup is the payload bytes sealed with a *separate* backup passphrase through the same vault
//! format as everything at rest (Argon2id, then XChaCha20-Poly1305). This module knows nothing about
//! what is inside the payload; the CLI crate builds and reads it. The file is
//!
//! ```text
//! "SPKBAK1\n" | header length (u32, big endian) | header (JSON, not secret) | sealed payload
//! ```
//!
//! The header holds the format name, the creation time and the vault header (salt, KDF cost, a known
//! plaintext sealed with the key). The creation time is also associated data of the sealed payload, so it
//! cannot be edited to make an old backup look new.

use crate::vault::{KdfParams, Vault, VaultHeader};
use crate::{Error, Result};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"SPKBAK1\n";
const FORMAT: &str = "spike-backup";
const MAX_HEADER: usize = 4096;
/// Refuse files larger than this before decrypting anything.
pub const MAX_FILE: usize = 256 * 1024 * 1024;
/// A passphrase chosen by the person must be at least this long.
pub const MIN_PASSPHRASE_CHARS: usize = 14;
const KEY_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const KEY_CHARS: usize = 25;

#[derive(Serialize, Deserialize)]
struct FileHeader {
    format: String,
    v: u32,
    created_ms: u64,
    vault: VaultHeader,
}

/// Cost of stretching the backup passphrase. Heavier than the vault's: the file leaves the device, so
/// whoever finds it can guess offline, without any rate limit.
pub fn backup_kdf() -> KdfParams {
    KdfParams {
        memory_kib: 256 * 1024,
        iterations: 3,
        parallelism: 1,
    }
}

fn aad(created_ms: u64) -> String {
    format!("spike-backup-v1:{created_ms}")
}

/// A random backup key: 25 characters from a 32-letter alphabet (125 bits), in groups of five.
/// Shown once; the app never stores it.
pub fn generate_key() -> String {
    let mut raw = [0u8; KEY_CHARS];
    OsRng.fill_bytes(&mut raw);
    let chars: Vec<char> = raw
        .iter()
        // 256 is a multiple of 32, so masking is unbiased.
        .map(|b| KEY_ALPHABET[(b & 31) as usize] as char)
        .collect();
    chars
        .chunks(5)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// The generated-key spelling of what the person typed (upper case, no dashes or spaces, and the
/// look-alike letters Crockford folds: O is 0, I and L are 1), or `None` if it is not that shape.
fn normalise_key(typed: &str) -> Option<String> {
    let s: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            u => u,
        })
        .collect();
    (s.len() == KEY_CHARS && s.bytes().all(|b| KEY_ALPHABET.contains(&b))).then(|| {
        s.as_bytes()
            .chunks(5)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    })
}

/// Seal `payload` into a backup file.
pub fn seal(passphrase: &str, kdf: KdfParams, created_ms: u64, payload: &[u8]) -> Result<Vec<u8>> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(Error::Backup("the backup passphrase is too short"));
    }
    let (vault, vault_header) = Vault::create(passphrase, kdf)?;
    let header = serde_json::to_vec(&FileHeader {
        format: FORMAT.into(),
        v: 1,
        created_ms,
        vault: vault_header,
    })?;
    let sealed = vault.seal(&aad(created_ms), payload)?;
    let mut out = Vec::with_capacity(8 + 4 + header.len() + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn split(file: &[u8]) -> Result<(FileHeader, &[u8])> {
    if file.len() > MAX_FILE || file.len() < 12 || &file[..8] != MAGIC {
        return Err(Error::Backup("this is not a Spike backup file"));
    }
    let n = u32::from_be_bytes(file[8..12].try_into().unwrap()) as usize;
    if n > MAX_HEADER || file.len() < 12 + n {
        return Err(Error::Backup("this backup file is damaged"));
    }
    let header: FileHeader = serde_json::from_slice(&file[12..12 + n])
        .map_err(|_| Error::Backup("this backup file is damaged"))?;
    if header.format != FORMAT || header.v != 1 {
        return Err(Error::Backup(
            "this backup was made by a newer version of the app",
        ));
    }
    Ok((header, &file[12 + n..]))
}

/// When the backup was made (ms since 1970), without the passphrase.
pub fn created_at(file: &[u8]) -> Result<u64> {
    Ok(split(file)?.0.created_ms)
}

/// Open a backup file: the creation time and the payload. A key in the generated shape is accepted
/// however it was typed (case, dashes, spaces, O for 0).
pub fn open(passphrase: &str, file: &[u8]) -> Result<(u64, Vec<u8>)> {
    let (header, sealed) = split(file)?;
    let try_with = |p: &str| -> Result<Vec<u8>> {
        let vault = Vault::unlock(p, &header.vault)?;
        vault
            .open(&aad(header.created_ms), sealed)
            .map_err(|_| Error::Backup("this backup file is damaged"))
    };
    match try_with(passphrase) {
        Ok(p) => Ok((header.created_ms, p)),
        Err(Error::WrongPassphrase) => match normalise_key(passphrase) {
            Some(n) if n != passphrase => Ok((header.created_ms, try_with(&n)?)),
            _ => Err(Error::WrongPassphrase),
        },
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &str = "a long enough backup phrase";

    fn cheap(p: &str, payload: &[u8]) -> Vec<u8> {
        seal(
            p,
            KdfParams::insecure_for_tests(),
            1_700_000_000_000,
            payload,
        )
        .unwrap()
    }

    #[test]
    fn round_trip_and_creation_time() {
        let f = cheap(PASS, b"the payload");
        assert_eq!(created_at(&f).unwrap(), 1_700_000_000_000);
        let (t, p) = open(PASS, &f).unwrap();
        assert_eq!(t, 1_700_000_000_000);
        assert_eq!(p, b"the payload");
    }

    #[test]
    fn plaintext_is_not_visible_and_each_file_is_different() {
        let marker = b"very-recognisable-owner-key";
        let f = cheap(PASS, marker);
        assert!(!f.windows(marker.len()).any(|w| w == marker));
        assert_ne!(f, cheap(PASS, marker));
    }

    #[test]
    fn wrong_passphrase_is_refused() {
        let f = cheap(PASS, b"x");
        assert!(matches!(
            open("another long passphrase", &f),
            Err(Error::WrongPassphrase)
        ));
    }

    #[test]
    fn short_passphrase_is_refused_when_sealing() {
        assert!(seal("short", KdfParams::insecure_for_tests(), 1, b"x").is_err());
    }

    #[test]
    fn every_region_is_tamper_evident() {
        let f = cheap(PASS, b"payload bytes");
        // magic, header length, header body, vault check, sealed payload, last byte
        for i in [0, 9, 14, 40, f.len() / 2, f.len() - 1] {
            let mut t = f.clone();
            t[i] ^= 1;
            assert!(open(PASS, &t).is_err(), "flip at {i} accepted");
        }
        assert!(open(PASS, &f[..f.len() - 1]).is_err());
        assert!(open(PASS, &[]).is_err());
        assert!(open(PASS, b"SPKBAK1\n\0\0\0\0").is_err());
    }

    #[test]
    fn creation_time_cannot_be_edited() {
        let f = cheap(PASS, b"x");
        let n = u32::from_be_bytes(f[8..12].try_into().unwrap()) as usize;
        let header = std::str::from_utf8(&f[12..12 + n]).unwrap();
        let edited = header.replace("1700000000000", "1800000000000");
        assert_ne!(header, edited);
        let mut t = f[..8].to_vec();
        t.extend_from_slice(&(edited.len() as u32).to_be_bytes());
        t.extend_from_slice(edited.as_bytes());
        t.extend_from_slice(&f[12 + n..]);
        assert!(open(PASS, &t).is_err(), "an edited date was accepted");
    }

    #[test]
    fn hostile_parameters_and_sizes_are_refused() {
        let f = cheap(PASS, b"x");
        let n = u32::from_be_bytes(f[8..12].try_into().unwrap()) as usize;
        let header = std::str::from_utf8(&f[12..12 + n]).unwrap();
        let hostile = header.replace("\"memory_kib\":8", "\"memory_kib\":4294967295");
        assert_ne!(header, hostile);
        let mut t = f[..8].to_vec();
        t.extend_from_slice(&(hostile.len() as u32).to_be_bytes());
        t.extend_from_slice(hostile.as_bytes());
        t.extend_from_slice(&f[12 + n..]);
        assert!(open(PASS, &t).is_err());
        let mut huge = f[..8].to_vec();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        huge.extend_from_slice(&f[12..]);
        assert!(open(PASS, &huge).is_err());
    }

    #[test]
    fn generated_keys_have_the_promised_shape_and_are_not_repeated() {
        let a = generate_key();
        assert_eq!(a.len(), 29);
        assert_eq!(a.matches('-').count(), 4);
        assert!(a.bytes().all(|b| b == b'-' || KEY_ALPHABET.contains(&b)));
        assert_ne!(a, generate_key());
        assert!(a.chars().count() >= MIN_PASSPHRASE_CHARS);
    }

    #[test]
    fn a_generated_key_opens_however_it_is_typed() {
        let key = generate_key();
        let f = cheap(&key, b"secret");
        for typed in [
            key.clone(),
            key.to_lowercase(),
            key.replace('-', ""),
            key.replace('-', " ").to_lowercase(),
            format!("  {key}\n"),
        ] {
            assert_eq!(open(&typed, &f).unwrap().1, b"secret", "typed {typed:?}");
        }
        assert!(open("AAAAA-AAAAA-AAAAA-AAAAA-AAAAA", &f).is_err());
    }
}
