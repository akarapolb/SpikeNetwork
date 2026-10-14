//! File attachments: encrypted client-side, stored on a relay as opaque chunks.
//!
//! The sender picks a fresh random 256-bit key per file, splits the file into
//! [`CHUNK`]-byte pieces and seals each with XChaCha20-Poly1305. The associated
//! data binds every chunk to its blob id, its index and the chunk count, so
//! chunks cannot be swapped between files, reordered or dropped unnoticed. A
//! SHA-256 of the whole plaintext is checked after reassembly as well.
//! The [`Attachment`] descriptor (name, size, key, hash, blob id, relay) travels
//! only inside an end-to-end encrypted message; the relay sees opaque chunks.

use crate::{Error, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Plaintext bytes per chunk.
pub const CHUNK: usize = 256 * 1024;
/// Largest file accepted (100 MiB).
pub const MAX_FILE: u64 = 100 * 1024 * 1024;
/// Bytes a sealed chunk adds: 24-byte nonce + 16-byte tag.
pub const OVERHEAD: usize = 40;
const AAD_CTX: &[u8] = b"spike-attachment-v1";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// Display name only. Never used as a path without [`safe_file_name`].
    pub name: String,
    pub size: u64,
    /// SHA-256 of the whole plaintext, hex.
    pub sha256: String,
    /// Random 128-bit blob id, hex: where the chunks live on the relay.
    pub blob: String,
    /// The file key, hex. Secret: only ever inside an encrypted message or the local vault.
    pub key: String,
    pub chunks: u32,
    /// The relay holding the chunks (the sender's home relay), if not the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn chunks_for(size: u64) -> u32 {
    (size.div_ceil(CHUNK as u64)).max(1) as u32
}

fn aad(blob: &str, idx: u32, total: u32) -> Vec<u8> {
    [
        AAD_CTX,
        blob.as_bytes(),
        &idx.to_be_bytes(),
        &total.to_be_bytes(),
    ]
    .concat()
}

impl Attachment {
    /// Reject descriptors that are malformed or inconsistent before using them.
    pub fn validate(&self) -> Result<()> {
        let ok = !self.name.is_empty()
            && self.name.len() <= 255
            && self.size <= MAX_FILE
            && is_hex(&self.sha256, 64)
            && is_hex(&self.blob, 32)
            && is_hex(&self.key, 64)
            && self.chunks == chunks_for(self.size);
        if ok {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305> {
        let key: [u8; 32] = hex::decode(&self.key)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        Ok(XChaCha20Poly1305::new((&key).into()))
    }

    /// Open chunk `idx` as fetched from the relay.
    pub fn open_chunk(&self, idx: u32, sealed: &[u8]) -> Result<Vec<u8>> {
        if idx >= self.chunks || sealed.len() < OVERHEAD || sealed.len() > CHUNK + OVERHEAD {
            return Err(Error::Malformed);
        }
        self.cipher()?
            .decrypt(
                XNonce::from_slice(&sealed[..24]),
                Payload {
                    msg: &sealed[24..],
                    aad: &aad(&self.blob, idx, self.chunks),
                },
            )
            .map_err(|_| Error::Corrupted)
    }

    /// Check the reassembled file against the size and hash in the descriptor.
    pub fn verify(&self, plain: &[u8]) -> Result<()> {
        if plain.len() as u64 != self.size || hex::encode(Sha256::digest(plain)) != self.sha256 {
            return Err(Error::Corrupted);
        }
        Ok(())
    }
}

/// Encrypt a file. Returns the descriptor (without relay) and the sealed chunks in order.
pub fn seal_file(name: &str, plain: &[u8]) -> Result<(Attachment, Vec<Vec<u8>>)> {
    if plain.len() as u64 > MAX_FILE {
        return Err(Error::TooLarge {
            max: MAX_FILE as usize,
        });
    }
    let mut key = [0u8; 32];
    let mut blob = [0u8; 16];
    OsRng.fill_bytes(&mut key);
    OsRng.fill_bytes(&mut blob);
    let att = Attachment {
        name: safe_file_name(name),
        size: plain.len() as u64,
        sha256: hex::encode(Sha256::digest(plain)),
        blob: hex::encode(blob),
        key: hex::encode(key),
        chunks: chunks_for(plain.len() as u64),
        relay: None,
    };
    let cipher = att.cipher()?;
    let pieces: Vec<&[u8]> = if plain.is_empty() {
        vec![&[][..]]
    } else {
        plain.chunks(CHUNK).collect()
    };
    let mut out = Vec::with_capacity(pieces.len());
    for (i, p) in pieces.iter().enumerate() {
        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let ct = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: p,
                    aad: &aad(&att.blob, i as u32, att.chunks),
                },
            )
            .map_err(|_| Error::Malformed)?;
        out.push([nonce.to_vec(), ct].concat());
    }
    Ok((att, out))
}

/// A name that is safe to create in a chosen directory: no path separators,
/// no parent-directory tricks, no control characters, not hidden, bounded length.
pub fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let mut s: String = if trimmed.is_empty() {
        "attachment".into()
    } else {
        trimmed.into()
    };
    while s.len() > 200 {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_all(att: &Attachment, sealed: &[Vec<u8>]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for (i, c) in sealed.iter().enumerate() {
            out.extend(att.open_chunk(i as u32, c)?);
        }
        att.verify(&out)?;
        Ok(out)
    }

    #[test]
    fn roundtrip_multi_chunk_and_empty() {
        let data: Vec<u8> = (0..CHUNK * 2 + 777).map(|i| (i * 7) as u8).collect();
        let (att, sealed) = seal_file("plans.pdf", &data).unwrap();
        att.validate().unwrap();
        assert_eq!((att.chunks, sealed.len()), (3, 3));
        assert_eq!(open_all(&att, &sealed).unwrap(), data);
        let (e, s) = seal_file("empty.txt", b"").unwrap();
        assert_eq!((e.chunks, open_all(&e, &s).unwrap().len()), (1, 0));
    }

    #[test]
    fn plaintext_and_key_are_not_in_the_chunks() {
        let marker = b"recognisable-attachment-text";
        let (att, sealed) = seal_file("a.txt", &marker.repeat(100)).unwrap();
        let all: Vec<u8> = sealed.concat();
        assert!(!all.windows(marker.len()).any(|w| w == marker));
        assert!(!all
            .windows(32)
            .any(|w| w == hex::decode(&att.key).unwrap().as_slice()));
    }

    #[test]
    fn swapped_reordered_tampered_or_foreign_chunks_fail() {
        let data = vec![1u8; CHUNK + 10];
        let (att, mut sealed) = seal_file("a", &data).unwrap();
        assert!(
            att.open_chunk(1, &sealed[0]).is_err(),
            "chunk 0 presented as chunk 1"
        );
        let (other, other_sealed) = seal_file("b", &data).unwrap();
        assert!(
            att.open_chunk(0, &other_sealed[0]).is_err(),
            "chunk from another file"
        );
        let mut wrong_blob = att.clone();
        wrong_blob.blob = other.blob.clone();
        assert!(
            wrong_blob.open_chunk(0, &sealed[0]).is_err(),
            "chunk bound to its blob id"
        );
        let last = sealed[1].len() - 1;
        sealed[1][last] ^= 1;
        assert!(open_all(&att, &sealed).is_err());
    }

    #[test]
    fn whole_file_hash_is_checked() {
        let (mut att, sealed) = seal_file("a", b"hello").unwrap();
        att.sha256 = hex::encode(Sha256::digest(b"other"));
        assert!(open_all(&att, &sealed).is_err());
    }

    #[test]
    fn descriptors_are_validated() {
        let (att, _) = seal_file("a", b"x").unwrap();
        for bad in [
            Attachment {
                chunks: 2,
                ..att.clone()
            },
            Attachment {
                size: MAX_FILE + 1,
                chunks: chunks_for(MAX_FILE + 1),
                ..att.clone()
            },
            Attachment {
                key: "00".into(),
                ..att.clone()
            },
            Attachment {
                name: String::new(),
                ..att.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!(matches!(
            seal_file("big", &vec![0u8; MAX_FILE as usize + 1]),
            Err(Error::TooLarge { .. })
        ));
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\Users\\a\\evil.exe"), "evil.exe");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name("..."), "attachment");
        assert_eq!(safe_file_name("a\nb\u{0}c.txt"), "abc.txt");
        assert_eq!(safe_file_name("report: final?.pdf"), "report_ final_.pdf");
        assert_eq!(safe_file_name(&"x".repeat(500)).len(), 200);
    }
}
