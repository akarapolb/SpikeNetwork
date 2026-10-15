//! Using approvals: the agent presents one, the service redeems it.
//! Logic lives in `spike_core::verifier`; this adds storage. See ADR 0015.

use crate::state::Home;
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection};
use spike_core::verifier::{Presentation, SpentStore};
use std::path::Path;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Agent side: package an approved request (by id or its first 4+ characters)
/// for the service named `audience`.
pub fn present(home: &Home, prefix: &str, audience: &str) -> Result<Presentation> {
    if prefix.len() < 4 {
        bail!("give at least 4 characters of the request id");
    }
    let all = crate::agent::list_outbox(home)?;
    let hits: Vec<_> = all
        .iter()
        .filter(|r| r.signed.req.id.starts_with(prefix))
        .collect();
    let r = match hits.as_slice() {
        [r] => *r,
        [] => bail!("no request starts with '{prefix}'"),
        _ => bail!("'{prefix}' matches several requests; type more characters"),
    };
    let Some(receipt) = &r.receipt else {
        bail!("that request has no answer yet; run `spike recv`");
    };
    if receipt.body.decision != spike_core::agent::Decision::Approved {
        bail!("that request was denied");
    }
    Presentation::create(
        &home.load_owner()?,
        r.signed.clone(),
        receipt.clone(),
        audience,
        now_ms(),
    )
    .map_err(|e| anyhow!("cannot present it: {e}"))
}

/// Durable [`SpentStore`] in a SQLite file. Several service processes may share
/// one file: the primary key makes `spend` atomic across them.
pub struct SqliteSpent(Connection);

impl SqliteSpent {
    pub fn open(path: &Path) -> Result<Self> {
        let c = Connection::open(path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS spent(
                 key TEXT PRIMARY KEY,
                 keep_until INTEGER NOT NULL,
                 used_at INTEGER NOT NULL);",
        )?;
        Ok(Self(c))
    }
}

impl SpentStore for SqliteSpent {
    fn spend(&mut self, key: &str, keep_until: u64, now: u64) -> std::result::Result<bool, String> {
        let e = |e: rusqlite::Error| e.to_string();
        self.0
            .execute(
                "DELETE FROM spent WHERE keep_until <= ?1",
                params![now as i64],
            )
            .map_err(e)?;
        let n = self
            .0
            .execute(
                "INSERT OR IGNORE INTO spent(key, keep_until, used_at) VALUES (?1, ?2, ?3)",
                params![key, keep_until as i64, now as i64],
            )
            .map_err(e)?;
        Ok(n == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_store_is_single_use_and_survives_reopening() {
        let dir = std::env::temp_dir().join(format!("spike-spent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("spent.db");
        {
            let mut s = SqliteSpent::open(&db).unwrap();
            assert_eq!(s.spend("a", 100, 10), Ok(true));
            assert_eq!(s.spend("a", 100, 11), Ok(false));
        }
        let mut s = SqliteSpent::open(&db).unwrap();
        assert_eq!(
            s.spend("a", 100, 50),
            Ok(false),
            "a restart forgets nothing"
        );
        assert_eq!(
            s.spend("a", 300, 100),
            Ok(true),
            "forgotten only after expiry"
        );
        drop(s);
        std::fs::remove_dir_all(&dir).ok();
    }
}
