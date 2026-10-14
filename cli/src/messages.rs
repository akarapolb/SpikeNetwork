//! Encrypted local message history (milestone M1 of the multi-device design).
//!
//! SQLite holds one row per message. Each row's content (id, peer, direction,
//! time, text) is sealed with the vault key; the only clear columns are two
//! keyed tags (HMAC under a vault subkey) used to find a conversation and to
//! make inserts idempotent. A thief of the file without the passphrase learns
//! how many messages exist and their sizes, not who they are with, when, or
//! what they say. The sealed blob is bound to its row's tags, so rows cannot
//! be moved between conversations or ids without detection.

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use spike_core::vault::Vault;
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    In,
    Out,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// Envelope id (hex), identical on every device that holds the message.
    pub id: String,
    /// The other party's identity key (hex Curve25519), i.e. the conversation.
    pub peer: String,
    pub direction: Direction,
    /// Sender's clock in ms since the epoch.
    pub ts: u64,
    pub body: String,
    /// The file sent with this message, if any (the key lives only in the sealed row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<spike_core::attachment::Attachment>,
    /// Logical clock of the message or of its latest accepted edit (see `spike_core::merge`).
    #[serde(default)]
    pub lamport: u64,
    /// Device that produced the version with that clock value; breaks ties.
    #[serde(default)]
    pub stamp_device: String,
    #[serde(default)]
    pub edited: bool,
    /// Tombstone: the text and attachment are gone; the row stays so the
    /// message cannot reappear from a late or replayed copy.
    #[serde(default)]
    pub deleted: bool,
}

impl StoredMessage {
    pub fn merge_state(&self) -> spike_core::merge::State {
        spike_core::merge::State {
            authored_by_me: self.direction == Direction::Out,
            deleted: self.deleted,
            stamp: (self.lamport, self.stamp_device.clone()),
        }
    }
}

/// (seq, id_tag, conv_tag, sealed) as read from the table.
type RawRow = (i64, Vec<u8>, Vec<u8>, Vec<u8>);

pub struct MessageStore<'v> {
    conn: Connection,
    vault: &'v Vault,
}

fn aad(id_tag: &[u8], conv_tag: &[u8]) -> String {
    format!("msg:{}:{}", hex::encode(id_tag), hex::encode(conv_tag))
}

impl<'v> MessageStore<'v> {
    pub(crate) fn open(path: &Path, vault: &'v Vault) -> Result<Self> {
        if !path.exists() {
            // Create the file owner-only before SQLite opens it; its journal files inherit the mode.
            let f = std::fs::File::create(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            drop(f);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                 seq      INTEGER PRIMARY KEY AUTOINCREMENT,
                 id_tag   BLOB NOT NULL UNIQUE,
                 conv_tag BLOB NOT NULL,
                 sealed   BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS messages_conv ON messages(conv_tag, seq);",
        )?;
        Ok(Self { conn, vault })
    }

    fn tags(&self, id: &str, peer: &str) -> ([u8; 32], [u8; 32]) {
        (
            self.vault.index_tag("msg-id", id.as_bytes()),
            self.vault.index_tag("conv", peer.as_bytes()),
        )
    }

    /// Store a message. Returns false if a message with this id is already
    /// stored (redelivery, replay of the same envelope, or a second copy).
    pub fn insert(&self, m: &StoredMessage) -> Result<bool> {
        let (id_tag, conv_tag) = self.tags(&m.id, &m.peer);
        let sealed = self
            .vault
            .seal(&aad(&id_tag, &conv_tag), &serde_json::to_vec(m)?)?;
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO messages (id_tag, conv_tag, sealed) VALUES (?1, ?2, ?3)",
            params![&id_tag[..], &conv_tag[..], sealed],
        )?;
        Ok(n == 1)
    }

    pub fn contains(&self, id: &str) -> Result<bool> {
        let id_tag = self.vault.index_tag("msg-id", id.as_bytes());
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM messages WHERE id_tag = ?1",
                [&id_tag[..]],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The last `limit` messages with `peer`, oldest first. Fails loudly if
    /// any row is corrupted or was tampered with, rather than hiding it.
    pub fn conversation(&self, peer: &str, limit: usize) -> Result<Vec<StoredMessage>> {
        let conv_tag = self.vault.index_tag("conv", peer.as_bytes());
        let mut stmt = self
            .conn
            .prepare("SELECT id_tag, sealed FROM messages WHERE conv_tag = ?1")?;
        let rows = stmt.query_map([&conv_tag[..]], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id_tag, sealed) = row?;
            out.push(self.unseal(&id_tag, &conv_tag, &sealed)?);
        }
        out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        let skip = out.len().saturating_sub(limit);
        Ok(out.split_off(skip))
    }

    /// The message with exactly this id, or the only one whose id starts with
    /// `prefix` (at least 6 hex characters), as shown by `spike history`.
    pub fn find(&self, prefix: &str) -> Result<StoredMessage> {
        let p = prefix.trim().to_lowercase();
        if p.len() == 32 {
            let id_tag = self.vault.index_tag("msg-id", p.as_bytes());
            let row: Option<(Vec<u8>, Vec<u8>)> = self
                .conn
                .query_row(
                    "SELECT conv_tag, sealed FROM messages WHERE id_tag = ?1",
                    [&id_tag[..]],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (conv_tag, sealed) = row.ok_or_else(|| anyhow!("no message with id {p}"))?;
            return self.unseal(&id_tag, &conv_tag, &sealed);
        }
        if p.len() < 6 {
            return Err(anyhow!("give at least 6 characters of the message id"));
        }
        let mut hits: Vec<StoredMessage> = self
            .all()?
            .into_iter()
            .filter(|m| m.id.starts_with(&p))
            .collect();
        match hits.len() {
            1 => Ok(hits.remove(0)),
            0 => Err(anyhow!("no message id starts with {p}")),
            n => Err(anyhow!("{n} messages start with {p}; give more of the id")),
        }
    }

    /// The stored message with exactly this id, if any.
    pub fn get(&self, id: &str) -> Result<Option<StoredMessage>> {
        let id_tag = self.vault.index_tag("msg-id", id.as_bytes());
        let row: Option<(Vec<u8>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT conv_tag, sealed FROM messages WHERE id_tag = ?1",
                [&id_tag[..]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(conv_tag, sealed)| self.unseal(&id_tag, &conv_tag, &sealed))
            .transpose()
    }

    /// Replace the content of an already stored message (same id and conversation).
    pub fn update(&self, m: &StoredMessage) -> Result<()> {
        let (id_tag, conv_tag) = self.tags(&m.id, &m.peer);
        let sealed = self
            .vault
            .seal(&aad(&id_tag, &conv_tag), &serde_json::to_vec(m)?)?;
        let n = self.conn.execute(
            "UPDATE messages SET sealed = ?1 WHERE id_tag = ?2 AND conv_tag = ?3",
            params![sealed, &id_tag[..], &conv_tag[..]],
        )?;
        if n != 1 {
            return Err(anyhow!(
                "cannot update a message that is not stored in that conversation"
            ));
        }
        Ok(())
    }

    /// Every stored message, oldest first (used to build history bundles).
    pub fn all(&self) -> Result<Vec<StoredMessage>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id_tag, conv_tag, sealed FROM messages")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id_tag, conv_tag, sealed) = row?;
            out.push(self.unseal(&id_tag, &conv_tag, &sealed)?);
        }
        out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Delete messages older than `cutoff_ms` across all conversations.
    /// Times are sealed, so this decrypts each row; fine at personal scale.
    pub fn prune_older_than(&self, cutoff_ms: u64) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, id_tag, conv_tag, sealed FROM messages")?;
        let rows: Vec<RawRow> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        let mut removed = 0;
        for (seq, id_tag, conv_tag, sealed) in rows {
            if self.unseal(&id_tag, &conv_tag, &sealed)?.ts < cutoff_ms {
                removed += self
                    .conn
                    .execute("DELETE FROM messages WHERE seq = ?1", [seq])?;
            }
        }
        Ok(removed)
    }

    fn unseal(&self, id_tag: &[u8], conv_tag: &[u8], sealed: &[u8]) -> Result<StoredMessage> {
        let plain = self
            .vault
            .open(&aad(id_tag, conv_tag), sealed)
            .map_err(|_| anyhow!("message history is corrupted or was tampered with"))?;
        let m: StoredMessage = serde_json::from_slice(&plain)?;
        // Belt and braces: the content must agree with the tags it is filed under.
        let (want_id, want_conv) = self.tags(&m.id, &m.peer);
        if want_id[..] != *id_tag || want_conv[..] != *conv_tag {
            return Err(anyhow!("message history is corrupted or was tampered with"));
        }
        Ok(m)
    }

    #[cfg(test)]
    fn raw(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spike_core::vault::KdfParams;

    struct T(std::path::PathBuf);
    impl Drop for T {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    fn tmp(tag: &str) -> T {
        let mut b = [0u8; 6];
        b.copy_from_slice(
            &spike_core::identity::Identity::generate()
                .unwrap()
                .ed_public()[..6],
        );
        let p = std::env::temp_dir().join(format!("spike-msgs-{tag}-{}", hex::encode(b)));
        std::fs::create_dir_all(&p).unwrap();
        T(p)
    }
    fn vault() -> Vault {
        Vault::create("pass", KdfParams::insecure_for_tests())
            .unwrap()
            .0
    }
    fn msg(id: &str, peer: &str, ts: u64, body: &str, d: Direction) -> StoredMessage {
        StoredMessage {
            attachment: None,
            lamport: 0,
            stamp_device: String::new(),
            edited: false,
            deleted: false,
            id: id.into(),
            peer: peer.into(),
            direction: d,
            ts,
            body: body.into(),
        }
    }

    #[test]
    fn insert_is_idempotent_and_ordered_per_conversation() {
        let d = tmp("order");
        let v = vault();
        let s = MessageStore::open(&d.0.join("m.db"), &v).unwrap();
        assert!(s
            .insert(&msg("b", "bob", 20, "second", Direction::In))
            .unwrap());
        assert!(s
            .insert(&msg("a", "bob", 10, "first", Direction::Out))
            .unwrap());
        assert!(s
            .insert(&msg("c", "carol", 15, "other chat", Direction::In))
            .unwrap());
        assert!(
            !s.insert(&msg("a", "bob", 10, "first", Direction::Out))
                .unwrap(),
            "duplicate id must be ignored"
        );
        let bob = s.conversation("bob", 100).unwrap();
        assert_eq!(
            bob.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            s.conversation("bob", 1).unwrap()[0].body,
            "second",
            "limit keeps the newest"
        );
        assert!(s.contains("c").unwrap() && !s.contains("zzz").unwrap());
    }

    #[test]
    fn nothing_readable_on_disk_and_other_vault_cannot_read() {
        let d = tmp("disk");
        let v = vault();
        let path = d.0.join("m.db");
        {
            let s = MessageStore::open(&path, &v).unwrap();
            s.insert(&msg(
                "a1",
                "bob-identity-key",
                1,
                "meet at the usual place",
                Direction::In,
            ))
            .unwrap();
        }
        for e in std::fs::read_dir(&d.0).unwrap() {
            let bytes = std::fs::read(e.unwrap().path()).unwrap();
            for needle in [&b"usual place"[..], b"bob-identity-key", b"\"direction\""] {
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle),
                    "plaintext on disk"
                );
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let other = vault();
        let s2 = MessageStore::open(&path, &other).unwrap();
        assert!(
            s2.conversation("bob-identity-key", 10).unwrap().is_empty(),
            "another vault cannot even find the rows"
        );
    }

    #[test]
    fn tampering_and_moving_rows_are_detected() {
        let d = tmp("tamper");
        let v = vault();
        let s = MessageStore::open(&d.0.join("m.db"), &v).unwrap();
        s.insert(&msg("a", "bob", 1, "to bob", Direction::Out))
            .unwrap();
        s.insert(&msg("b", "carol", 2, "to carol", Direction::Out))
            .unwrap();
        // Move Carol's sealed row into Bob's conversation.
        let bob_tag = v.index_tag("conv", b"bob");
        s.raw()
            .execute(
                "UPDATE messages SET conv_tag = ?1 WHERE conv_tag != ?1",
                [&bob_tag[..]],
            )
            .unwrap();
        assert!(
            s.conversation("bob", 10).is_err(),
            "moved row must not be accepted"
        );
        // Flip a byte of a sealed blob.
        let d2 = tmp("tamper2");
        let s = MessageStore::open(&d2.0.join("m.db"), &v).unwrap();
        s.insert(&msg("a", "bob", 1, "to bob", Direction::Out))
            .unwrap();
        let mut blob: Vec<u8> = s
            .raw()
            .query_row("SELECT sealed FROM messages", [], |r| r.get(0))
            .unwrap();
        blob[40] ^= 1;
        s.raw()
            .execute("UPDATE messages SET sealed = ?1", [blob])
            .unwrap();
        assert!(s
            .conversation("bob", 10)
            .unwrap_err()
            .to_string()
            .contains("tampered"));
    }

    #[test]
    fn prune_removes_only_old_messages() {
        let d = tmp("prune");
        let v = vault();
        let s = MessageStore::open(&d.0.join("m.db"), &v).unwrap();
        s.insert(&msg("old", "bob", 1_000, "old", Direction::In))
            .unwrap();
        s.insert(&msg("new", "bob", 9_000, "new", Direction::In))
            .unwrap();
        assert_eq!(s.prune_older_than(5_000).unwrap(), 1);
        assert_eq!(
            s.conversation("bob", 10)
                .unwrap()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["new"]
        );
    }
}
