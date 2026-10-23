//! Durable storage for queued ciphertext, prekeys, attachment chunks and group mailboxes (SQLite).
//!
//! `synchronous=FULL` so a message the relay has said "accepted" to survives a
//! crash or power loss. The database holds ciphertext and routing metadata
//! (recipient ids, timing, sizes), never plaintext. Protect the file like any
//! other metadata store: owner-only permissions are set on creation, and disk
//! encryption is the operator's responsibility.

use rusqlite::{params, Connection, OptionalExtension};
use spike_core::wire::{GroupKind, WirePrekey};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The schema this build writes. It is stored in the database (`PRAGMA user_version`) and
/// raised by [`migrate`], never lowered: a database from a newer relay is refused, not guessed at.
///
/// - 1: the original tables (databases from before versioning are treated as version 1).
/// - 2: indexes on `expires`, so the expiry purge no longer scans whole tables.
/// - 3: group mailboxes and their entries (ADR 0025).
pub const SCHEMA_VERSION: i64 = 3;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    /// The file was written by a newer relay than this one.
    Newer {
        found: i64,
        supported: i64,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "database error: {e}"),
            Self::Newer { found, supported } => write!(
                f,
                "this database has schema version {found} but this relay only understands up to {supported}: upgrade the relay, do not downgrade the data"
            ),
            Self::Io(e) => write!(f, "backing up the database before an upgrade failed: {e}"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Counts for the metrics endpoint. Numbers only: nothing that identifies a device or holds content.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub queued_messages: u64,
    pub queued_bytes: u64,
    pub blob_chunks: u64,
    pub blob_bytes: u64,
    pub prekeys: u64,
    pub device_lists: u64,
    pub group_mailboxes: u64,
    pub group_entries: u64,
    pub group_bytes: u64,
    pub key_packages: u64,
}

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

/// Limits a group mailbox is held to (ADR 0025).
#[derive(Clone, Copy, Debug)]
pub struct GroupLimits {
    pub message_ttl_ms: i64,
    pub handshake_ttl_ms: i64,
    pub max_bytes: usize,
    pub max_per_device: usize,
}

pub enum GroupCreated {
    Created,
    Exists,
    OverQuota,
}

pub enum GroupAppended {
    /// Stored at this position; `sealed` is true when this commit closed the mailbox.
    Appended {
        seq: u64,
        sealed: bool,
    },
    /// A commit expected another position; this is where the log really is.
    Conflict(u64),
    Unknown,
    Sealed,
    /// The mailbox is full of entries that may not be dropped.
    Full,
    OverQuota,
    /// The seal named a mailbox id that already exists.
    TargetExists,
}

pub struct GroupEntryRow {
    pub seq: u64,
    pub kind: GroupKind,
    pub blob: Vec<u8>,
}

pub struct GroupPageRow {
    pub entries: Vec<GroupEntryRow>,
    pub next_seq: u64,
    pub sealed: bool,
}

/// Version 1: the tables the relay has always had. `IF NOT EXISTS` makes it safe on a database that
/// predates versioning, which already has them.
const V1: &str = "CREATE TABLE IF NOT EXISTS messages (
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
     );";

/// Version 2: the periodic purge deletes by `expires`; without an index that scans every row.
const V2: &str = "CREATE INDEX IF NOT EXISTS messages_expires ON messages(expires);
     CREATE INDEX IF NOT EXISTS blob_chunks_expires ON blob_chunks(expires);";

/// Version 3: group mailboxes. A mailbox is an append-only log with relay-assigned positions and
/// one public key every request must be signed with; its entries are opaque to the relay. The
/// relay stores no member list and no group id, only these random mailbox ids.
const V3: &str = "CREATE TABLE IF NOT EXISTS group_mailboxes (
         mailbox   TEXT PRIMARY KEY,
         pubkey    TEXT    NOT NULL,
         creator   TEXT    NOT NULL,
         next_seq  INTEGER NOT NULL,
         sealed_at INTEGER,
         bytes     INTEGER NOT NULL,
         expires   INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS group_mailboxes_creator ON group_mailboxes(creator);
     CREATE INDEX IF NOT EXISTS group_mailboxes_expires ON group_mailboxes(expires);
     CREATE TABLE IF NOT EXISTS group_entries (
         mailbox TEXT    NOT NULL,
         seq     INTEGER NOT NULL,
         kind    INTEGER NOT NULL,
         blob    BLOB    NOT NULL,
         expires INTEGER NOT NULL,
         PRIMARY KEY (mailbox, seq)
     );
     CREATE INDEX IF NOT EXISTS group_entries_expires ON group_entries(expires);
     CREATE TABLE IF NOT EXISTS key_packages (
         id      INTEGER PRIMARY KEY AUTOINCREMENT,
         device  TEXT NOT NULL,
         package BLOB NOT NULL,
         UNIQUE(device, package)
     );
     CREATE TABLE IF NOT EXISTS key_package_last_resort (
         device  TEXT PRIMARY KEY,
         package BLOB NOT NULL
     );";

/// Apply every migration between the stored version and [`SCHEMA_VERSION`], each in its own
/// transaction. A database from before versioning has user_version 0 and the version 1 tables:
/// it is told apart from a brand new one by having a `messages` table.
fn migrate(conn: &mut Connection, path: Option<&Path>) -> Result<(), StoreError> {
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::Newer {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    if found == SCHEMA_VERSION {
        return Ok(());
    }
    let has_data: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'messages')",
        [],
        |r| r.get(0),
    )?;
    // Back up a file that already holds data, before changing it.
    if let (Some(p), true) = (path, has_data) {
        let from = found.max(1);
        let backup: PathBuf = {
            let mut name = p.as_os_str().to_owned();
            name.push(format!(".before-v{from}"));
            name.into()
        };
        if !backup.exists() {
            conn.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    let steps: [(i64, &str); 3] = [(1, V1), (2, V2), (3, V3)];
    for (version, sql) in steps {
        if found >= version {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Store {
    /// Open (creating if needed) a database file, or an in-memory one for `None`, and bring its
    /// schema up to [`SCHEMA_VERSION`]. A file with data from an older schema is copied to
    /// `<file>.before-v<N>` first, so a bad upgrade can be undone by hand.
    pub fn open(path: Option<&Path>) -> Result<Self, StoreError> {
        let mut conn = match path {
            Some(p) => {
                #[cfg(unix)]
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
        migrate(&mut conn, path)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// The schema version stored in the database.
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        let c = self.conn.lock().unwrap();
        c.pragma_query_value(None, "user_version", |r| r.get(0))
    }

    /// A cheap query that proves the database answers (for the health check).
    pub fn ping(&self) -> rusqlite::Result<()> {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT 1", [], |_| Ok(()))
    }

    pub fn stats(&self) -> rusqlite::Result<Stats> {
        let c = self.conn.lock().unwrap();
        let one = |sql: &str| -> rusqlite::Result<u64> {
            c.query_row(sql, [], |r| r.get::<_, i64>(0))
                .map(|v| v.max(0) as u64)
        };
        Ok(Stats {
            queued_messages: one("SELECT COUNT(*) FROM messages")?,
            queued_bytes: one("SELECT COALESCE(SUM(LENGTH(blob)), 0) FROM messages")?,
            blob_chunks: one("SELECT COUNT(*) FROM blob_chunks")?,
            blob_bytes: one("SELECT COALESCE(SUM(LENGTH(data)), 0) FROM blob_chunks")?,
            prekeys: one("SELECT COUNT(*) FROM prekeys")?,
            device_lists: one("SELECT COUNT(*) FROM device_lists")?,
            group_mailboxes: one("SELECT COUNT(*) FROM group_mailboxes")?,
            group_entries: one("SELECT COUNT(*) FROM group_entries")?,
            group_bytes: one("SELECT COALESCE(SUM(LENGTH(blob)), 0) FROM group_entries")?,
            key_packages: one("SELECT COUNT(*) FROM key_packages")?,
        })
    }

    /// Drop expired messages, attachment chunks and group entries and mailboxes. Returns how many
    /// rows were removed.
    pub fn purge_expired(&self) -> rusqlite::Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let now = now_ms();
        let mut n = c.execute("DELETE FROM messages WHERE expires <= ?1", [now])?
            + c.execute("DELETE FROM blob_chunks WHERE expires <= ?1", [now])?;
        let tx = c.transaction()?;
        n += purge_groups(&tx, now)?;
        tx.commit()?;
        Ok(n)
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

/// Remove expired group entries and whole expired mailboxes, and fix the byte count of mailboxes
/// that lost entries.
fn purge_groups(tx: &rusqlite::Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    let touched: Vec<String> = {
        let mut stmt =
            tx.prepare("SELECT DISTINCT mailbox FROM group_entries WHERE expires <= ?1")?;
        let rows = stmt.query_map([now], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut n = tx.execute("DELETE FROM group_entries WHERE expires <= ?1", [now])?;
    for mailbox in &touched {
        tx.execute(
            "UPDATE group_mailboxes SET bytes =
                 (SELECT COALESCE(SUM(LENGTH(blob)), 0) FROM group_entries WHERE mailbox = ?1)
             WHERE mailbox = ?1",
            [mailbox],
        )?;
    }
    n += tx.execute(
        "DELETE FROM group_entries WHERE mailbox IN
             (SELECT mailbox FROM group_mailboxes WHERE expires <= ?1)",
        [now],
    )?;
    n += tx.execute("DELETE FROM group_mailboxes WHERE expires <= ?1", [now])?;
    Ok(n)
}

impl Store {
    /// Store one-time key packages up to `cap` for `device`; duplicates are ignored. Returns the total.
    pub fn publish_key_packages(
        &self,
        device: &str,
        packages: &[Vec<u8>],
        cap: usize,
    ) -> rusqlite::Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let mut count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM key_packages WHERE device = ?1",
            [device],
            |r| r.get(0),
        )?;
        for p in packages {
            if count as usize >= cap {
                break;
            }
            count += tx.execute(
                "INSERT OR IGNORE INTO key_packages (device, package) VALUES (?1, ?2)",
                params![device, p],
            )? as i64;
        }
        tx.commit()?;
        Ok(count as usize)
    }

    /// Replace the device's last-resort key package.
    pub fn publish_last_resort_key_package(
        &self,
        device: &str,
        package: &[u8],
    ) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO key_package_last_resort (device, package) VALUES (?1, ?2)
             ON CONFLICT(device) DO UPDATE SET package = ?2",
            params![device, package],
        )?;
        Ok(())
    }

    /// Remove and return the oldest one-time key package for `device`; if none remain, the
    /// reusable last-resort one (kept). The bool is true for the last-resort package.
    pub fn claim_key_package(&self, device: &str) -> rusqlite::Result<Option<(Vec<u8>, bool)>> {
        let c = self.conn.lock().unwrap();
        let one_time: Option<Vec<u8>> = c
            .query_row(
                "DELETE FROM key_packages WHERE id =
                     (SELECT id FROM key_packages WHERE device = ?1 ORDER BY id LIMIT 1)
                 RETURNING package",
                [device],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(p) = one_time {
            return Ok(Some((p, false)));
        }
        c.query_row(
            "SELECT package FROM key_package_last_resort WHERE device = ?1",
            [device],
            |r| Ok((r.get(0)?, true)),
        )
        .optional()
    }

    /// The public key a mailbox's requests must be signed with. It never changes once registered.
    pub fn group_pubkey(&self, mailbox: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT pubkey FROM group_mailboxes WHERE mailbox = ?1 AND expires > ?2",
                params![mailbox, now_ms()],
                |r| r.get(0),
            )
            .optional()
    }

    /// Where a mailbox is: the position the next append will get, and whether it was sealed.
    pub fn group_status(&self, mailbox: &str) -> rusqlite::Result<Option<(u64, bool)>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT next_seq, sealed_at IS NOT NULL FROM group_mailboxes
                 WHERE mailbox = ?1 AND expires > ?2",
                params![mailbox, now_ms()],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, bool>(1)?)),
            )
            .optional()
    }

    /// Register a mailbox. `creator` is the device asking, for the per-device quota.
    pub fn create_group_mailbox(
        &self,
        mailbox: &str,
        pubkey: &str,
        creator: &str,
        lim: &GroupLimits,
    ) -> rusqlite::Result<GroupCreated> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let now = now_ms();
        purge_groups(&tx, now)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM group_mailboxes WHERE mailbox = ?1)",
            [mailbox],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(GroupCreated::Exists);
        }
        let mine: i64 = tx.query_row(
            "SELECT COUNT(*) FROM group_mailboxes WHERE creator = ?1",
            [creator],
            |r| r.get(0),
        )?;
        if mine as usize >= lim.max_per_device {
            return Ok(GroupCreated::OverQuota);
        }
        tx.execute(
            "INSERT INTO group_mailboxes (mailbox, pubkey, creator, next_seq, sealed_at, bytes, expires)
             VALUES (?1, ?2, ?3, 1, NULL, 0, ?4)",
            params![mailbox, pubkey, creator, now + lim.handshake_ttl_ms],
        )?;
        tx.commit()?;
        Ok(GroupCreated::Created)
    }

    /// Append one entry, all or nothing. A commit must arrive with the position it expects
    /// (compare-and-append: exactly one commit gets each position). If `seal` is given, the same
    /// transaction closes this mailbox and registers the successor. A full mailbox drops its
    /// oldest *messages* to make room, never commits.
    #[allow(clippy::too_many_arguments)]
    pub fn append_group(
        &self,
        mailbox: &str,
        kind: GroupKind,
        expected_seq: Option<u64>,
        blob: &[u8],
        seal: Option<(&str, &str)>,
        by: &str,
        lim: &GroupLimits,
    ) -> rusqlite::Result<GroupAppended> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let now = now_ms();
        let row: Option<(i64, bool, i64)> = tx
            .query_row(
                "SELECT next_seq, sealed_at IS NOT NULL, bytes FROM group_mailboxes
                 WHERE mailbox = ?1 AND expires > ?2",
                params![mailbox, now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((next_seq, sealed, mut bytes)) = row else {
            return Ok(GroupAppended::Unknown);
        };
        if sealed {
            return Ok(GroupAppended::Sealed);
        }
        if let Some(e) = expected_seq {
            if e != next_seq as u64 {
                return Ok(GroupAppended::Conflict(next_seq as u64));
            }
        }
        if let Some((successor, _)) = seal {
            purge_groups(&tx, now)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM group_mailboxes WHERE mailbox = ?1)",
                [successor],
                |r| r.get(0),
            )?;
            if exists {
                return Ok(GroupAppended::TargetExists);
            }
            let mine: i64 = tx.query_row(
                "SELECT COUNT(*) FROM group_mailboxes WHERE creator = ?1",
                [by],
                |r| r.get(0),
            )?;
            if mine as usize >= lim.max_per_device {
                return Ok(GroupAppended::OverQuota);
            }
        }
        // Make room: expired entries first, then the oldest messages. Commits stay.
        let expired = tx.execute(
            "DELETE FROM group_entries WHERE mailbox = ?1 AND expires <= ?2",
            params![mailbox, now],
        )?;
        if expired > 0 {
            bytes = tx.query_row(
                "SELECT COALESCE(SUM(LENGTH(blob)), 0) FROM group_entries WHERE mailbox = ?1",
                [mailbox],
                |r| r.get(0),
            )?;
        }
        let need = blob.len() as i64;
        while (bytes + need) as usize > lim.max_bytes {
            let victim: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT seq, LENGTH(blob) FROM group_entries WHERE mailbox = ?1 AND kind = 0
                     ORDER BY seq LIMIT 1",
                    [mailbox],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((seq, len)) = victim else {
                return Ok(GroupAppended::Full);
            };
            tx.execute(
                "DELETE FROM group_entries WHERE mailbox = ?1 AND seq = ?2",
                params![mailbox, seq],
            )?;
            bytes -= len;
        }
        let ttl = match kind {
            GroupKind::Message => lim.message_ttl_ms,
            GroupKind::Commit => lim.handshake_ttl_ms,
        };
        tx.execute(
            "INSERT INTO group_entries (mailbox, seq, kind, blob, expires) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mailbox, next_seq, kind.byte() as i64, blob, now + ttl],
        )?;
        // The mailbox itself lives as long as its handshake entries may: a sealed one too, so a
        // device that was offline can still replay up to the commit that moved the group on.
        tx.execute(
            "UPDATE group_mailboxes SET next_seq = ?2, bytes = ?3, expires = ?4,
                 sealed_at = CASE WHEN ?5 THEN ?6 ELSE sealed_at END
             WHERE mailbox = ?1",
            params![
                mailbox,
                next_seq + 1,
                bytes + need,
                now + lim.handshake_ttl_ms,
                seal.is_some(),
                next_seq
            ],
        )?;
        if let Some((successor, pubkey)) = seal {
            tx.execute(
                "INSERT INTO group_mailboxes (mailbox, pubkey, creator, next_seq, sealed_at, bytes, expires)
                 VALUES (?1, ?2, ?3, 1, NULL, 0, ?4)",
                params![successor, pubkey, by, now + lim.handshake_ttl_ms],
            )?;
        }
        tx.commit()?;
        Ok(GroupAppended::Appended {
            seq: next_seq as u64,
            sealed: seal.is_some(),
        })
    }

    /// Up to `limit` unexpired entries after `after_seq`, oldest first.
    pub fn read_group(
        &self,
        mailbox: &str,
        after_seq: u64,
        limit: u32,
    ) -> rusqlite::Result<Option<GroupPageRow>> {
        let c = self.conn.lock().unwrap();
        let now = now_ms();
        let head: Option<(i64, bool)> = c
            .query_row(
                "SELECT next_seq, sealed_at IS NOT NULL FROM group_mailboxes
                 WHERE mailbox = ?1 AND expires > ?2",
                params![mailbox, now],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((next_seq, sealed)) = head else {
            return Ok(None);
        };
        let mut stmt = c.prepare(
            "SELECT seq, kind, blob FROM group_entries
             WHERE mailbox = ?1 AND seq > ?2 AND expires > ?3 ORDER BY seq LIMIT ?4",
        )?;
        let entries = stmt
            .query_map(params![mailbox, after_seq as i64, now, limit as i64], |r| {
                Ok(GroupEntryRow {
                    seq: r.get::<_, i64>(0)? as u64,
                    kind: GroupKind::from_byte(r.get(1)?).unwrap_or(GroupKind::Message),
                    blob: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(GroupPageRow {
            entries,
            next_seq: next_seq as u64,
            sealed,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spike-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("relay.db")
    }

    /// What a relay from before versioning left on disk: the version 1 tables, user_version 0, one message.
    fn old_database(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(V1).unwrap();
        c.execute(
            "INSERT INTO messages (recipient, blob, expires) VALUES ('r', x'0102', 9999999999999)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn a_new_database_is_created_at_the_current_version() {
        let s = Store::open(None).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        s.ping().unwrap();
        assert_eq!(s.stats().unwrap(), Stats::default());
    }

    #[test]
    fn an_old_database_is_upgraded_keeping_its_data_and_backed_up_first() {
        let path = temp_db("upgrade");
        old_database(&path);
        let s = Store::open(Some(&path)).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(s.stats().unwrap().queued_messages, 1, "data survives");
        let backup = PathBuf::from(format!("{}.before-v1", path.display()));
        assert!(backup.exists(), "a copy of the old file is kept");
        // The copy is the old database: still version 0, still readable.
        let b = Connection::open(&backup).unwrap();
        let v: i64 = b
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 0);
        // The new indexes exist.
        let has: bool = s
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'messages_expires')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has);
    }

    #[test]
    fn opening_twice_is_harmless_and_makes_no_second_backup() {
        let path = temp_db("twice");
        old_database(&path);
        drop(Store::open(Some(&path)).unwrap());
        let s = Store::open(Some(&path)).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(s.stats().unwrap().queued_messages, 1);
        assert!(!PathBuf::from(format!("{}.before-v2", path.display())).exists());
    }

    #[test]
    fn a_version_two_database_gains_the_group_tables_and_is_backed_up_first() {
        let path = temp_db("v2");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(V1).unwrap();
            c.execute_batch(V2).unwrap();
            c.pragma_update(None, "user_version", 2).unwrap();
            c.execute(
                "INSERT INTO messages (recipient, blob, expires) VALUES ('r', x'0102', 9999999999999)",
                [],
            )
            .unwrap();
        }
        let s = Store::open(Some(&path)).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        let stats = s.stats().unwrap();
        assert_eq!((stats.queued_messages, stats.group_mailboxes), (1, 0));
        assert!(PathBuf::from(format!("{}.before-v2", path.display())).exists());
        // And the new tables work.
        let lim = GroupLimits {
            message_ttl_ms: 10_000,
            handshake_ttl_ms: 20_000,
            max_bytes: 1000,
            max_per_device: 2,
        };
        assert!(matches!(
            s.create_group_mailbox("m", "pk", "dev", &lim).unwrap(),
            GroupCreated::Created
        ));
        assert!(matches!(
            s.append_group("m", GroupKind::Message, None, b"x", None, "dev", &lim)
                .unwrap(),
            GroupAppended::Appended {
                seq: 1,
                sealed: false
            }
        ));
    }

    #[test]
    fn expired_group_entries_and_mailboxes_are_purged_and_bytes_recounted() {
        let s = Store::open(None).unwrap();
        let lim = GroupLimits {
            message_ttl_ms: 1,
            handshake_ttl_ms: 60_000,
            max_bytes: 10_000,
            max_per_device: 5,
        };
        s.create_group_mailbox("m", "pk", "dev", &lim).unwrap();
        s.append_group(
            "m",
            GroupKind::Message,
            None,
            &[0u8; 100],
            None,
            "dev",
            &lim,
        )
        .unwrap();
        s.append_group(
            "m",
            GroupKind::Commit,
            Some(2),
            &[0u8; 50],
            None,
            "dev",
            &lim,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(s.purge_expired().unwrap(), 1, "only the message expired");
        let stats = s.stats().unwrap();
        assert_eq!((stats.group_entries, stats.group_bytes), (1, 50));
        // The next append sees the corrected size, not the old one.
        let tight = GroupLimits {
            max_bytes: 120,
            ..lim
        };
        assert!(matches!(
            s.append_group(
                "m",
                GroupKind::Message,
                None,
                &[0u8; 70],
                None,
                "dev",
                &tight
            )
            .unwrap(),
            GroupAppended::Appended { .. }
        ));
        // A mailbox with no activity for its whole lifetime goes, entries and all.
        let short = GroupLimits {
            handshake_ttl_ms: 1,
            ..lim
        };
        s.create_group_mailbox("old", "pk", "dev", &short).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        s.purge_expired().unwrap();
        assert!(s.group_pubkey("old").unwrap().is_none());
    }

    #[test]
    fn a_database_from_a_newer_relay_is_refused_untouched() {
        let path = temp_db("newer");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(V1).unwrap();
            c.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        match Store::open(Some(&path)) {
            Err(StoreError::Newer { found, supported }) => {
                assert_eq!((found, supported), (SCHEMA_VERSION + 1, SCHEMA_VERSION));
            }
            other => panic!("expected a refusal, got {:?}", other.err()),
        }
        let c = Connection::open(&path).unwrap();
        let v: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION + 1, "the file was not downgraded");
    }
}
