//! Where sealed bytes live (ADR 0033, step 2). `Home` keeps its files through a `Store`: named blobs that are
//! read, written (atomically) and deleted. The native `FileStore` is the directory this program has always used;
//! a browser will bring its own (IndexedDB) in a later stage.
//!
//! The trait is async because IndexedDB cannot be read synchronously. The futures are `Send` where the native
//! app needs it (it spawns them) and not on `wasm32`, where the browser's handles are not `Send`: that is what
//! `MaybeSend` says. A plain `async fn` in a trait promises neither, so the methods are written with an explicit
//! `impl Future<..> + MaybeSend` return type.

use anyhow::Result;
use std::future::Future;
use std::path::{Path, PathBuf};

// `Send` on native, nothing on `wasm32` (see the module comment). Defined in `spike_core::clock`, where the
// `Clock` trait needs them too.
pub use spike_core::clock::{MaybeSend, MaybeSync};

/// Named blobs. Names are plain relative paths such as `contacts.enc` or `attachments/<tag>.enc`; what is stored
/// is already sealed (or, for the vault header, not secret), so a store needs no key.
pub trait Store: MaybeSend + MaybeSync {
    /// The bytes under `name`, or `None` if never written.
    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + MaybeSend;
    /// Atomic: a reader sees the old bytes or the new, never half. Owner-only wherever the store has a notion
    /// of permissions.
    fn put(&self, name: &str, bytes: &[u8]) -> impl Future<Output = Result<()>> + MaybeSend;
    /// Remove `name`; removing what is not there is not an error.
    fn delete(&self, name: &str) -> impl Future<Output = Result<()>> + MaybeSend;
}

/// The state directory: one file per name, written to a temporary file, flushed, and renamed over the old one,
/// with owner-only permissions (0600 files, 0700 directories on Unix). Same files as before the trait existed.
#[derive(Clone, Debug)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether `name` exists as a file.
    pub fn has(&self, name: &str) -> bool {
        self.dir.join(name).exists()
    }

    /// Create the state directory itself (owner-only), before the first write.
    pub(crate) fn make_dir(&self) -> Result<()> {
        make_private_dir(&self.dir)
    }

    // The three operations are plain file calls and finish without waiting; the async methods below only
    // wrap them. `Home`'s synchronous API relies on that (see `state::ready`).
    fn get_now(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.dir.join(name)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn put_now(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let path = self.dir.join(name);
        if name.contains('/') {
            if let Some(parent) = path.parent() {
                make_private_dir(parent)?;
            }
        }
        write_private(&path, bytes)
    }

    fn delete_now(&self, name: &str) -> Result<()> {
        match std::fs::remove_file(self.dir.join(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl Store for FileStore {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.get_now(name)
    }

    async fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.put_now(name, bytes)
    }

    async fn delete(&self, name: &str) -> Result<()> {
        self.delete_now(name)
    }
}

pub(crate) fn make_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Atomic write with owner-only permissions: write a temp file, fsync, rename.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spike-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[tokio::test]
    async fn put_get_delete_round_trip_and_missing_is_none() {
        let dir = temp("rt");
        let s = FileStore::new(&dir);
        s.make_dir().unwrap();
        assert_eq!(s.get("a.enc").await.unwrap(), None);
        s.put("a.enc", b"one").await.unwrap();
        s.put("a.enc", b"two").await.unwrap();
        assert_eq!(s.get("a.enc").await.unwrap().as_deref(), Some(&b"two"[..]));
        s.delete("a.enc").await.unwrap();
        s.delete("a.enc").await.unwrap(); // already gone: not an error
        assert_eq!(s.get("a.enc").await.unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_write_leaves_no_temporary_file_and_a_nested_name_makes_its_folder() {
        let dir = temp("nested");
        let s = FileStore::new(&dir);
        s.make_dir().unwrap();
        s.put("attachments/x.enc", b"sealed").await.unwrap();
        assert_eq!(
            s.get("attachments/x.enc").await.unwrap().as_deref(),
            Some(&b"sealed"[..])
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("attachments"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            leftovers.len(),
            1,
            "only the file itself, no .tmp: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("perm");
        let s = FileStore::new(&dir);
        s.make_dir().unwrap();
        s.put("attachments/x.enc", b"sealed").await.unwrap();
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.join("attachments/x.enc")), 0o600);
        assert_eq!(mode(dir.join("attachments")), 0o700);
        assert_eq!(mode(dir.clone()), 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Home`'s synchronous API polls these futures once; that is only sound if they never wait.
    #[test]
    fn file_store_futures_finish_without_waiting() {
        let dir = temp("ready");
        let s = FileStore::new(&dir);
        s.make_dir().unwrap();
        s.put("a.enc", b"x")
            .now_or_never()
            .expect("put is ready")
            .unwrap();
        assert!(s
            .get("a.enc")
            .now_or_never()
            .expect("get is ready")
            .unwrap()
            .is_some());
        s.delete("a.enc")
            .now_or_never()
            .expect("delete is ready")
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
