//! Durable storage for queued ciphertext and prekeys (SQLite).
//!
//! `synchronous=FULL` so a message the relay has said "accepted" to survives a
//! crash or power loss. The database holds ciphertext and routing metadata
//! (recipient ids, timing, sizes), never plaintext. Protect the file like any
//! other metadata store: owner-only permissions are set on creation, and disk
//! encryption is the operator's responsibility.

use rusqlite::{params, Connection, OptionalExtension};
use spike_core::wire::WirePrekey;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Store {
    conn: Mutex<Connection>,
}

pub enum Enqueued {
    Id(i64),
    Full,
}

pub enum PutChunk {
    Stored,
    NotOwner,
    TotalMismatch,
    OverQuota,
}

pub struct Stored {
    pub id: i64,
    pub blob: Vec<u8>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Store {
    /// Open (creating if needed) a database file, or an in-memory one for `None`.
    pub fn open(path: Option<&Path>) -> rusqlite::Result<Self> {
        let conn = match path {
            Some(p) => {
                let existed = p.exists();
                let c = Connection::open(p)?;
                #[cfg(unix)]
                if !existed {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
                }
                c.pragma_update(None, "journal_mode", "WAL")?;
                c
            }
            None => Connection::open_in_memory()?,
        };
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                 id        INTEGER PRIMARY KEY AUTOINCREMENT,
                 recipient TEXT    NOT NULL,
                 blob      BLOB    NOT NULL,
                 expires   INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS messages_recipient ON messages(recipient, id);
             CREATE TABLE IF NOT EXISTS prekeys (
                 id     INTEGER PRIMARY KEY AUTOINCREMENT,
                 device TEXT NOT NULL,
                 key    TEXT NOT NULL,
                 sig    TEXT NOT NULL,
                 UNIQUE(device, key)
             );
             CREATE TABLE IF NOT EXISTS blob_chunks (
                 blob     TEXT    NOT NULL,
                 idx      INTEGER NOT NULL,
                 total    INTEGER NOT NULL,
                 uploader TEXT    NOT NULL,
                 data     BLOB    NOT NULL,
                 expires  INTEGER NOT NULL,
                 PRIMARY KEY (blob, idx)
             );
             CREATE INDEX IF NOT EXISTS blob_chunks_uploader ON blob_chunks(uploader);
             CREATE TABLE IF NOT EXISTS device_lists (
                 owner   TEXT PRIMARY KEY,
                 version INTEGER NOT NULL,
                 list    TEXT    NOT NULL
             );
             CREATE TABLE IF NOT EXISTS fallbacks (
                 device   TEXT PRIMARY KEY,
                 key      TEXT    NOT NULL,
                 sig      TEXT    NOT NULL,
                 uploaded INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Drop expired messages and attachment chunks. Returns how many rows were removed.
    pub fn purge_expired(&self) -> rusqlite::Result<usize> {
        let c = self.conn.lock().unwrap();
        let now = now_ms();
        Ok(
            c.execute("DELETE FROM messages WHERE expires <= ?1", [now])?
                + c.execute("DELETE FROM blob_chunks WHERE expires <= ?1", [now])?,
        )
    }

    pub fn enqueue(
        &self,
        to: &str,
        blob: &[u8],
        ttl_ms: i64,
        cap: usize,
    ) -> rusqlite::Result<Enqueued> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM messages WHERE recipient = ?1 AND expires <= ?2",
            params![to, now_ms()],
        )?;
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE recipient = ?1",
            [to],
            |r| r.get(0),
        )?;
        if n as usize >= cap {
            return Ok(Enqueued::Full);
        }
        tx.execute(
            "INSERT INTO messages (recipient, blob, expires) VALUES (?1, ?2, ?3)",
            params![to, blob, now_ms() + ttl_ms],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Enqueued::Id(id))
    }

    /// Unacknowledged, unexpired messages for `me`, oldest first.
    pub fn backlog(&self, me: &str) -> rusqlite::Result<Vec<Stored>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id, blob FROM messages WHERE recipient = ?1 AND expires > ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![me, now_ms()], |r| {
            Ok(Stored {
                id: r.get(0)?,
                blob: r.get(1)?,
            })
        })?;
        rows.collect()
    }

    pub fn ack(&self, me: &str, id: i64) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM messages WHERE id = ?1 AND recipient = ?2",
            params![id, me],
        )?;
        Ok(())
    }

    pub fn prekey_count(&self, device: &str) -> rusqlite::Result<usize> {
        let n: i64 = self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM prekeys WHERE device = ?1",
            [device],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Store prekeys up to `cap` total for `device`; duplicates are ignored.
    /// Returns the new total.
    pub fn publish_prekeys(
        &self,
        device: &str,
        keys: &[WirePrekey],
        cap: usize,
    ) -> rusqlite::Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let mut count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM prekeys WHERE device = ?1",
            [device],
            |r| r.get(0),
        )?;
        for k in keys {
            if count as usize >= cap {
                break;
            }
            count += tx.execute(
                "INSERT OR IGNORE INTO prekeys (device, key, sig) VALUES (?1, ?2, ?3)",
                params![device, k.key, k.sig],
            )? as i64;
        }
        tx.commit()?;
        Ok(count as usize)
    }

    /// Replace the device's fallback prekey.
    pub fn publish_fallback(&self, device: &str, k: &WirePrekey) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO fallbacks (device, key, sig, uploaded) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(device) DO UPDATE SET key = ?2, sig = ?3, uploaded = ?4",
            params![device, k.key, k.sig, now_ms()],
        )?;
        Ok(())
    }

    /// Age of the device's stored fallback prekey, if any.
    pub fn fallback_age_secs(&self, device: &str) -> rusqlite::Result<Option<u64>> {
        let up: Option<i64> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT uploaded FROM fallbacks WHERE device = ?1",
                [device],
                |r| r.get(0),
            )
            .optional()?;
        Ok(up.map(|u| ((now_ms() - u).max(0) / 1000) as u64))
    }

    /// Remove and return the oldest one-time prekey for `device` (each is
    /// handed out once). If none remain, return the reusable fallback instead
    /// (not removed). The bool is true for the fallback.
    pub fn claim_prekey(&self, device: &str) -> rusqlite::Result<Option<(WirePrekey, bool)>> {
        let c = self.conn.lock().unwrap();
        let one_time = c
            .query_row(
                "DELETE FROM prekeys WHERE id = (SELECT id FROM prekeys WHERE device = ?1 ORDER BY id LIMIT 1)
                 RETURNING key, sig",
                [device],
                |r| Ok(WirePrekey { key: r.get(0)?, sig: r.get(1)? }),
            )
            .optional()?;
        if let Some(k) = one_time {
            return Ok(Some((k, false)));
        }
        c.query_row(
            "SELECT key, sig FROM fallbacks WHERE device = ?1",
            [device],
            |r| {
                Ok((
                    WirePrekey {
                        key: r.get(0)?,
                        sig: r.get(1)?,
                    },
                    true,
                ))
            },
        )
        .optional()
    }

    /// The stored signed device list for `owner`: (version, JSON).
    pub fn device_list(&self, owner: &str) -> rusqlite::Result<Option<(u64, String)>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT version, list FROM device_lists WHERE owner = ?1",
                [owner],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
            )
            .optional()
    }

    /// Store a list the caller has already verified and checked against the stored one.
    pub fn put_device_list(&self, owner: &str, version: u64, list: &str) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO device_lists (owner, version, list) VALUES (?1, ?2, ?3)
             ON CONFLICT(owner) DO UPDATE SET version = ?2, list = ?3",
            params![owner, version as i64, list],
        )?;
        Ok(())
    }

    /// Store one attachment chunk (idempotent per index, so uploads can be retried).
    #[allow(clippy::too_many_arguments)]
    pub fn put_chunk(
        &self,
        blob: &str,
        idx: u32,
        total: u32,
        uploader: &str,
        data: &[u8],
        ttl_ms: i64,
        quota: usize,
    ) -> rusqlite::Result<PutChunk> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let now = now_ms();
        tx.execute("DELETE FROM blob_chunks WHERE expires <= ?1", [now])?;
        let first: Option<(String, i64)> = tx
            .query_row(
                "SELECT uploader, total FROM blob_chunks WHERE blob = ?1 LIMIT 1",
                [blob],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((owner, t)) = first {
            if owner != uploader {
                return Ok(PutChunk::NotOwner);
            }
            if t != total as i64 {
                return Ok(PutChunk::TotalMismatch);
            }
        }
        let used: i64 = tx.query_row(
            "SELECT COALESCE(SUM(length(data)), 0) FROM blob_chunks WHERE uploader = ?1 AND NOT (blob = ?2 AND idx = ?3)",
            params![uploader, blob, idx],
            |r| r.get(0),
        )?;
        if used as usize + data.len() > quota {
            return Ok(PutChunk::OverQuota);
        }
        tx.execute(
            "INSERT OR REPLACE INTO blob_chunks (blob, idx, total, uploader, data, expires) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![blob, idx, total, uploader, data, now + ttl_ms],
        )?;
        tx.commit()?;
        Ok(PutChunk::Stored)
    }

    pub fn get_chunk(&self, blob: &str, idx: u32) -> rusqlite::Result<Option<Vec<u8>>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT data FROM blob_chunks WHERE blob = ?1 AND idx = ?2 AND expires > ?3",
                params![blob, idx, now_ms()],
                |r| r.get(0),
            )
            .optional()
    }
}
