//! Tenants, enrolments, token uses and rules (ADR 0023, ADR 0035).
//!
//! What is kept is the minimum the console needs: which tenant a device id is in, the *day* it
//! enrolled, the *week* it last signed in, and the rules version it last fetched. No names, no
//! addresses, no times finer than a day, and no per-device traffic.

use super::{now_ms, Store};
use rusqlite::{params, OptionalExtension};
use spike_core::tenant::TokenBody;
use spike_core::wire::{WireTenantDevice, WireTenantUsage};

/// Version 5: organisations.
pub(super) const V5: &str = "CREATE TABLE IF NOT EXISTS tenants (
         id            TEXT PRIMARY KEY,
         name          TEXT    NOT NULL,
         admin_pub     TEXT    NOT NULL,
         max_devices   INTEGER NOT NULL,
         storage_bytes INTEGER NOT NULL,
         state         TEXT    NOT NULL,
         created       INTEGER NOT NULL,
         rules_version INTEGER NOT NULL DEFAULT 0,
         rules         TEXT
     );
     CREATE TABLE IF NOT EXISTS enrollments (
         device        TEXT PRIMARY KEY,
         tenant        TEXT    NOT NULL,
         token_id      TEXT    NOT NULL,
         enrolled_day  INTEGER NOT NULL,
         active_week   INTEGER NOT NULL,
         rules_version INTEGER NOT NULL DEFAULT 0
     );
     CREATE INDEX IF NOT EXISTS enrollments_tenant ON enrollments(tenant);
     CREATE TABLE IF NOT EXISTS token_uses (
         token_id  TEXT PRIMARY KEY,
         tenant    TEXT    NOT NULL,
         device    TEXT,
         not_after INTEGER NOT NULL
     );
     CREATE TABLE IF NOT EXISTS revoked_devices (
         device TEXT NOT NULL,
         tenant TEXT NOT NULL,
         PRIMARY KEY (device, tenant)
     );";

pub const ACTIVE: &str = "active";
pub const SUSPENDED: &str = "suspended";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    pub id: String,
    pub name: String,
    pub admin_pub: String,
    pub max_devices: u64,
    pub storage_bytes: u64,
    pub state: String,
    pub rules_version: u64,
    pub rules: Option<String>,
}

/// The outcome of presenting a token at sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redeem {
    Enrolled,
    /// The device was already in this tenant; nothing changed.
    AlreadyMember,
    NoTenant,
    /// The token's admin key is not the tenant's (a forged or stale token).
    WrongKey,
    Suspended,
    Withdrawn,
    Used,
    Full,
    /// The device is already in another tenant on this relay.
    OtherTenant,
}

fn unix_day(ms: i64) -> i64 {
    ms.div_euclid(86_400_000)
}

/// Weeks since 1970-01-01 (a Thursday): coarse on purpose.
fn unix_week(ms: i64) -> i64 {
    unix_day(ms).div_euclid(7)
}

fn activity(active_week: i64, now_week: i64) -> &'static str {
    match now_week - active_week {
        i64::MIN..=0 => "active",
        1..=4 => "idle",
        _ => "dormant",
    }
}

fn tenant_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Tenant> {
    Ok(Tenant {
        id: r.get(0)?,
        name: r.get(1)?,
        admin_pub: r.get(2)?,
        max_devices: r.get::<_, i64>(3)?.max(0) as u64,
        storage_bytes: r.get::<_, i64>(4)?.max(0) as u64,
        state: r.get(5)?,
        rules_version: r.get::<_, i64>(6)?.max(0) as u64,
        rules: r.get(7)?,
    })
}

const TENANT_COLS: &str =
    "id, name, admin_pub, max_devices, storage_bytes, state, rules_version, rules";

/// Delete everything the relay holds for one device: its queue, prekeys, fallback and push endpoint.
fn forget_device(tx: &rusqlite::Transaction<'_>, device: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM messages WHERE recipient = ?1", [device])?;
    tx.execute("DELETE FROM push_endpoints WHERE device = ?1", [device])?;
    Ok(())
}

impl Store {
    pub fn tenant_create(
        &self,
        id: &str,
        name: &str,
        admin_pub: &str,
        max_devices: u64,
        storage_bytes: u64,
    ) -> rusqlite::Result<bool> {
        let n = self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO tenants (id, name, admin_pub, max_devices, storage_bytes, state, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, name, admin_pub, max_devices as i64, storage_bytes as i64, ACTIVE, now_ms()],
        )?;
        Ok(n == 1)
    }

    pub fn tenant(&self, id: &str) -> rusqlite::Result<Option<Tenant>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("SELECT {TENANT_COLS} FROM tenants WHERE id = ?1"),
                [id],
                tenant_row,
            )
            .optional()
    }

    pub fn tenants(&self) -> rusqlite::Result<Vec<Tenant>> {
        let c = self.conn.lock().unwrap();
        let mut s = c.prepare(&format!("SELECT {TENANT_COLS} FROM tenants ORDER BY id"))?;
        let rows = s.query_map([], tenant_row)?;
        rows.collect()
    }

    pub fn tenant_set_limits(
        &self,
        id: &str,
        max_devices: Option<u64>,
        storage_bytes: Option<u64>,
    ) -> rusqlite::Result<bool> {
        let c = self.conn.lock().unwrap();
        let mut n = 0;
        if let Some(d) = max_devices {
            n += c.execute(
                "UPDATE tenants SET max_devices = ?2 WHERE id = ?1",
                params![id, d as i64],
            )?;
        }
        if let Some(b) = storage_bytes {
            n += c.execute(
                "UPDATE tenants SET storage_bytes = ?2 WHERE id = ?1",
                params![id, b as i64],
            )?;
        }
        Ok(n > 0)
    }

    pub fn tenant_set_state(&self, id: &str, state: &str) -> rusqlite::Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE tenants SET state = ?2 WHERE id = ?1",
            params![id, state],
        )? == 1)
    }

    /// Re-register the admin public key (rotation or a lost key: an operator action).
    pub fn tenant_set_admin(&self, id: &str, admin_pub: &str) -> rusqlite::Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE tenants SET admin_pub = ?2 WHERE id = ?1",
            params![id, admin_pub],
        )? == 1)
    }

    /// Delete a tenant: its enrolments, the queues of its devices, its tokens and rules.
    pub fn tenant_delete(&self, id: &str) -> rusqlite::Result<bool> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let devices: Vec<String> = {
            let mut s = tx.prepare("SELECT device FROM enrollments WHERE tenant = ?1")?;
            let rows = s.query_map([id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for d in &devices {
            forget_device(&tx, d)?;
        }
        tx.execute("DELETE FROM enrollments WHERE tenant = ?1", [id])?;
        tx.execute("DELETE FROM token_uses WHERE tenant = ?1", [id])?;
        tx.execute("DELETE FROM revoked_devices WHERE tenant = ?1", [id])?;
        let n = tx.execute("DELETE FROM tenants WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(n == 1)
    }

    /// Present a token whose signature the caller has already checked and which has not expired.
    pub fn redeem(&self, device: &str, t: &TokenBody) -> rusqlite::Result<Redeem> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let tenant: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT admin_pub, state, max_devices FROM tenants WHERE id = ?1",
                [&t.tenant],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((admin_pub, state, max_devices)) = tenant else {
            return Ok(Redeem::NoTenant);
        };
        if !admin_pub.eq_ignore_ascii_case(&t.admin_pub) {
            return Ok(Redeem::WrongKey);
        }
        if state != ACTIVE {
            return Ok(Redeem::Suspended);
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT tenant FROM enrollments WHERE device = ?1",
                [device],
                |r| r.get(0),
            )
            .optional()?;
        match current {
            Some(ten) if ten == t.tenant => return Ok(Redeem::AlreadyMember),
            Some(_) => return Ok(Redeem::OtherTenant),
            None => {}
        }
        let used: Option<Option<String>> = tx
            .query_row(
                "SELECT device FROM token_uses WHERE token_id = ?1",
                [&t.token_id],
                |r| r.get(0),
            )
            .optional()?;
        match used {
            Some(None) => return Ok(Redeem::Withdrawn),
            Some(Some(d)) if d != device => return Ok(Redeem::Used),
            _ => {}
        }
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM enrollments WHERE tenant = ?1",
            [&t.tenant],
            |r| r.get(0),
        )?;
        if max_devices > 0 && n >= max_devices {
            return Ok(Redeem::Full);
        }
        let now = now_ms();
        tx.execute(
            "INSERT OR REPLACE INTO token_uses (token_id, tenant, device, not_after) VALUES (?1, ?2, ?3, ?4)",
            params![t.token_id, t.tenant, device, t.not_after as i64],
        )?;
        tx.execute(
            "INSERT INTO enrollments (device, tenant, token_id, enrolled_day, active_week) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![device, t.tenant, t.token_id, unix_day(now), unix_week(now)],
        )?;
        tx.execute(
            "DELETE FROM revoked_devices WHERE device = ?1 AND tenant = ?2",
            params![device, t.tenant],
        )?;
        tx.commit()?;
        Ok(Redeem::Enrolled)
    }

    /// The tenant a device is in and that tenant's state.
    pub fn membership(&self, device: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT e.tenant, t.state FROM enrollments e JOIN tenants t ON t.id = e.tenant WHERE e.device = ?1",
                [device],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    pub fn is_revoked(&self, device: &str) -> rusqlite::Result<bool> {
        self.conn.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM revoked_devices WHERE device = ?1)",
            [device],
            |r| r.get(0),
        )
    }

    /// Record that a member signed in this week (only the week is kept).
    pub fn touch(&self, device: &str) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE enrollments SET active_week = ?2 WHERE device = ?1",
            params![device, unix_week(now_ms())],
        )?;
        Ok(())
    }

    pub fn tenant_devices(&self, tenant: &str) -> rusqlite::Result<Vec<WireTenantDevice>> {
        let c = self.conn.lock().unwrap();
        let now_week = unix_week(now_ms());
        let mut s = c.prepare(
            "SELECT device, enrolled_day, active_week, rules_version FROM enrollments WHERE tenant = ?1
             ORDER BY active_week DESC, enrolled_day, device",
        )?;
        let rows = s.query_map([tenant], |r| {
            Ok(WireTenantDevice {
                device: r.get(0)?,
                enrolled_day: r.get::<_, i64>(1)?.max(0) as u64,
                activity: activity(r.get(2)?, now_week).into(),
                rules_version: r.get::<_, i64>(3)?.max(0) as u64,
            })
        })?;
        rows.collect()
    }

    pub fn tenant_members(&self, tenant: &str) -> rusqlite::Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut s = c.prepare("SELECT device FROM enrollments WHERE tenant = ?1")?;
        let rows = s.query_map([tenant], |r| r.get(0))?;
        rows.collect()
    }

    /// Remove a device from a tenant: refuse it from then on and drop its queue. False if it was not a member.
    pub fn revoke(&self, tenant: &str, device: &str) -> rusqlite::Result<bool> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let n = tx.execute(
            "DELETE FROM enrollments WHERE device = ?1 AND tenant = ?2",
            params![device, tenant],
        )?;
        if n == 1 {
            forget_device(&tx, device)?;
            tx.execute(
                "INSERT OR IGNORE INTO revoked_devices (device, tenant) VALUES (?1, ?2)",
                params![device, tenant],
            )?;
        }
        tx.commit()?;
        Ok(n == 1)
    }

    /// A member leaves: the enrolment and its queue go. Returns the tenant it left.
    pub fn leave(&self, device: &str) -> rusqlite::Result<Option<String>> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let tenant: Option<String> = tx
            .query_row(
                "SELECT tenant FROM enrollments WHERE device = ?1",
                [device],
                |r| r.get(0),
            )
            .optional()?;
        if tenant.is_some() {
            tx.execute("DELETE FROM enrollments WHERE device = ?1", [device])?;
            forget_device(&tx, device)?;
        }
        tx.commit()?;
        Ok(tenant)
    }

    /// Withdraw an unused token. False if a device already used it.
    pub fn cancel_token(
        &self,
        tenant: &str,
        token_id: &str,
        not_after: u64,
    ) -> rusqlite::Result<bool> {
        let c = self.conn.lock().unwrap();
        let used: Option<Option<String>> = c
            .query_row(
                "SELECT device FROM token_uses WHERE token_id = ?1",
                [token_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(Some(_)) = used {
            return Ok(false);
        }
        c.execute(
            "INSERT OR IGNORE INTO token_uses (token_id, tenant, device, not_after) VALUES (?1, ?2, NULL, ?3)",
            params![token_id, tenant, not_after as i64],
        )?;
        Ok(true)
    }

    /// Store rules if `version` is higher than the stored one. False otherwise.
    pub fn put_rules(&self, tenant: &str, version: u64, doc: &str) -> rusqlite::Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE tenants SET rules_version = ?2, rules = ?3 WHERE id = ?1 AND rules_version < ?2",
            params![tenant, version as i64, doc],
        )? == 1)
    }

    /// A member's tenant and its rules; records which version the device was given.
    pub fn rules_for(&self, device: &str) -> rusqlite::Result<Option<(String, Option<String>)>> {
        let c = self.conn.lock().unwrap();
        let row: Option<(String, i64, Option<String>)> = c
            .query_row(
                "SELECT e.tenant, t.rules_version, t.rules FROM enrollments e JOIN tenants t ON t.id = e.tenant WHERE e.device = ?1",
                [device],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((tenant, version, rules)) = row else {
            return Ok(None);
        };
        c.execute(
            "UPDATE enrollments SET rules_version = ?2 WHERE device = ?1",
            params![device, version],
        )?;
        Ok(Some((tenant, rules)))
    }

    /// Bytes held for a tenant's devices: queued ciphertext plus attachment chunks they uploaded.
    pub fn tenant_stored_bytes(&self, tenant: &str) -> rusqlite::Result<u64> {
        let c = self.conn.lock().unwrap();
        let q: i64 = c.query_row(
            "SELECT COALESCE(SUM(LENGTH(m.blob)), 0) FROM messages m JOIN enrollments e ON e.device = m.recipient WHERE e.tenant = ?1",
            [tenant],
            |r| r.get(0),
        )?;
        let b: i64 = c.query_row(
            "SELECT COALESCE(SUM(LENGTH(b.data)), 0) FROM blob_chunks b JOIN enrollments e ON e.device = b.uploader WHERE e.tenant = ?1",
            [tenant],
            |r| r.get(0),
        )?;
        Ok((q + b).max(0) as u64)
    }

    /// Whether storing `extra` more bytes for `device` would take its tenant over its storage limit.
    /// A device in no tenant, or a tenant without a limit, is never over.
    pub fn tenant_storage_over(&self, device: &str, extra: u64) -> rusqlite::Result<bool> {
        let t: Option<(String, i64)> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT t.id, t.storage_bytes FROM enrollments e JOIN tenants t ON t.id = e.tenant WHERE e.device = ?1",
                [device],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((tenant, limit)) = t else {
            return Ok(false);
        };
        if limit <= 0 {
            return Ok(false);
        }
        Ok(self.tenant_stored_bytes(&tenant)? + extra > limit as u64)
    }

    pub fn tenant_usage(&self, tenant: &str) -> rusqlite::Result<Option<WireTenantUsage>> {
        let Some(t) = self.tenant(tenant)? else {
            return Ok(None);
        };
        let devices: i64 = self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM enrollments WHERE tenant = ?1",
            [tenant],
            |r| r.get(0),
        )?;
        Ok(Some(WireTenantUsage {
            devices: devices.max(0) as u64,
            device_limit: t.max_devices,
            stored_bytes: self.tenant_stored_bytes(tenant)?,
            storage_limit: t.storage_bytes,
            rules_version: t.rules_version,
            state: t.state,
            ..Default::default()
        }))
    }

    /// Forget token uses whose tokens have expired (an expired token is refused anyway).
    pub(super) fn purge_tokens(
        tx: &rusqlite::Transaction<'_>,
        now: i64,
    ) -> rusqlite::Result<usize> {
        tx.execute("DELETE FROM token_uses WHERE not_after * 1000 < ?1", [now])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spike_core::owner::OwnerKey;
    use spike_core::tenant::EnrolToken;

    fn setup() -> (Store, OwnerKey) {
        let s = Store::open(None).unwrap();
        let k = OwnerKey::generate();
        assert!(s
            .tenant_create("nw", "Northwind", &hex::encode(k.public()), 2, 0)
            .unwrap());
        assert!(!s.tenant_create("nw", "Again", "00", 1, 0).unwrap());
        (s, k)
    }

    fn token(k: &OwnerKey, tenant: &str) -> TokenBody {
        EnrolToken::issue(k, tenant, "wss://r", "Northwind", 3600, 1_800_000_000)
            .unwrap()
            .1
    }

    #[test]
    fn a_token_enrols_one_device_once() {
        let (s, k) = setup();
        let t = token(&k, "nw");
        assert_eq!(s.redeem("dev-a", &t).unwrap(), Redeem::Enrolled);
        assert_eq!(s.redeem("dev-a", &t).unwrap(), Redeem::AlreadyMember);
        assert_eq!(s.redeem("dev-b", &t).unwrap(), Redeem::Used);
        assert_eq!(
            s.membership("dev-a").unwrap(),
            Some(("nw".into(), ACTIVE.into()))
        );
        let devs = s.tenant_devices("nw").unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].activity, "active");
    }

    #[test]
    fn the_wrong_key_an_unknown_tenant_and_a_full_tenant_are_refused() {
        let (s, k) = setup();
        assert_eq!(
            s.redeem("d", &token(&OwnerKey::generate(), "nw")).unwrap(),
            Redeem::WrongKey
        );
        assert_eq!(
            s.redeem("d", &token(&k, "other")).unwrap(),
            Redeem::NoTenant
        );
        assert_eq!(s.redeem("d1", &token(&k, "nw")).unwrap(), Redeem::Enrolled);
        assert_eq!(s.redeem("d2", &token(&k, "nw")).unwrap(), Redeem::Enrolled);
        assert_eq!(s.redeem("d3", &token(&k, "nw")).unwrap(), Redeem::Full);
        s.tenant_set_limits("nw", Some(0), None).unwrap();
        assert_eq!(s.redeem("d3", &token(&k, "nw")).unwrap(), Redeem::Enrolled);
    }

    #[test]
    fn suspended_tenants_and_withdrawn_tokens_are_refused() {
        let (s, k) = setup();
        let t = token(&k, "nw");
        assert!(s.cancel_token("nw", &t.token_id, t.not_after).unwrap());
        assert_eq!(s.redeem("d", &t).unwrap(), Redeem::Withdrawn);
        s.tenant_set_state("nw", SUSPENDED).unwrap();
        assert_eq!(s.redeem("d", &token(&k, "nw")).unwrap(), Redeem::Suspended);
    }

    #[test]
    fn a_used_token_cannot_be_withdrawn() {
        let (s, k) = setup();
        let t = token(&k, "nw");
        s.redeem("d", &t).unwrap();
        assert!(!s.cancel_token("nw", &t.token_id, t.not_after).unwrap());
    }

    #[test]
    fn revoking_drops_the_queue_and_marks_the_device() {
        let (s, k) = setup();
        s.redeem("d", &token(&k, "nw")).unwrap();
        s.enqueue("d", b"ciphertext", 60_000, 10).unwrap();
        assert_eq!(s.tenant_stored_bytes("nw").unwrap(), 10);
        assert!(s.revoke("nw", "d").unwrap());
        assert!(!s.revoke("nw", "d").unwrap());
        assert!(s.backlog("d").unwrap().is_empty());
        assert!(s.is_revoked("d").unwrap());
        assert_eq!(s.membership("d").unwrap(), None);
        // A fresh token from the admin brings it back deliberately.
        assert_eq!(s.redeem("d", &token(&k, "nw")).unwrap(), Redeem::Enrolled);
        assert!(!s.is_revoked("d").unwrap());
    }

    #[test]
    fn rules_only_move_forward_and_record_what_a_device_fetched() {
        let (s, k) = setup();
        s.redeem("d", &token(&k, "nw")).unwrap();
        assert_eq!(s.rules_for("d").unwrap(), Some(("nw".into(), None)));
        assert!(s.put_rules("nw", 2, "doc2").unwrap());
        assert!(!s.put_rules("nw", 2, "again").unwrap());
        assert!(!s.put_rules("nw", 1, "older").unwrap());
        assert_eq!(
            s.rules_for("d").unwrap(),
            Some(("nw".into(), Some("doc2".into())))
        );
        assert_eq!(s.tenant_devices("nw").unwrap()[0].rules_version, 2);
        assert_eq!(s.rules_for("stranger").unwrap(), None);
    }

    #[test]
    fn storage_limit_counts_the_tenants_devices_only() {
        let (s, k) = setup();
        s.tenant_set_limits("nw", None, Some(15)).unwrap();
        s.redeem("d", &token(&k, "nw")).unwrap();
        s.enqueue("d", b"0123456789", 60_000, 10).unwrap();
        s.enqueue("outsider", b"0123456789012345678901234", 60_000, 10)
            .unwrap();
        assert!(!s.tenant_storage_over("d", 5).unwrap());
        assert!(s.tenant_storage_over("d", 6).unwrap());
        assert!(!s.tenant_storage_over("outsider", 1_000_000).unwrap());
    }

    #[test]
    fn leaving_and_deleting_forget_the_device() {
        let (s, k) = setup();
        s.redeem("d", &token(&k, "nw")).unwrap();
        s.redeem("e", &token(&k, "nw")).unwrap();
        s.enqueue("e", b"x", 60_000, 10).unwrap();
        assert_eq!(s.leave("d").unwrap(), Some("nw".into()));
        assert_eq!(s.leave("d").unwrap(), None);
        assert!(!s.is_revoked("d").unwrap());
        assert!(s.tenant_delete("nw").unwrap());
        assert!(s.backlog("e").unwrap().is_empty());
        assert_eq!(s.membership("e").unwrap(), None);
        assert!(s.tenants().unwrap().is_empty());
    }

    #[test]
    fn activity_buckets_are_coarse() {
        assert_eq!(activity(10, 10), "active");
        assert_eq!(activity(9, 10), "idle");
        assert_eq!(activity(6, 10), "idle");
        assert_eq!(activity(5, 10), "dormant");
    }
}
