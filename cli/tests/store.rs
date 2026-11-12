//! `Home` over a `Store` (ADR 0033, step 2): the same logic over a second, in-memory store, and the state
//! directory still holding the same files as before the trait.

use anyhow::Result;
use spike_cli::state::Home;
use spike_cli::store::{FileStore, Store};
use spike_core::vault::KdfParams;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A store that keeps everything in a map, the way a browser store would keep it in IndexedDB.
#[derive(Default, Clone)]
struct MemStore(Arc<Mutex<BTreeMap<String, Vec<u8>>>>);

impl Store for MemStore {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    async fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(name.to_string(), bytes.to_vec());
        Ok(())
    }
    async fn delete(&self, name: &str) -> Result<()> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

fn kdf() -> KdfParams {
    KdfParams::insecure_for_tests()
}

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("spike-storetest-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[tokio::test]
async fn home_works_over_a_store_that_is_not_a_directory() {
    let store = MemStore::default();
    assert!(!Home::exists_in(&store).await.unwrap());
    let home = Home::create_with(store, "pass phrase", kdf(), false)
        .await
        .unwrap();

    // the identity, contacts and an attachment all go through the store
    let card = home.identity_card_async().await.unwrap();
    assert!(card.starts_with("spike2:"));
    assert!(home.load_contacts_async().await.unwrap().is_empty());
    home.save_attachment_async("m1", b"file bytes")
        .await
        .unwrap();
    assert_eq!(
        home.load_attachment_async("m1").await.unwrap().as_deref(),
        Some(&b"file bytes"[..])
    );
    assert_eq!(home.load_attachment_async("nope").await.unwrap(), None);
    home.delete_attachment_async("m1").await.unwrap();
    assert_eq!(home.load_attachment_async("m1").await.unwrap(), None);
    assert_eq!(home.next_lamport_async().await.unwrap(), 1);
    assert_eq!(home.next_lamport_async().await.unwrap(), 2);
}

#[tokio::test]
async fn the_same_store_can_be_unlocked_again_and_a_wrong_passphrase_is_refused() {
    let store = MemStore::default();
    let home = Home::create_with(store.clone(), "right", kdf(), false)
        .await
        .unwrap();
    let card = home.identity_card_async().await.unwrap();
    // creating over a vault is refused without force
    let err = Home::create_with(store.clone(), "right", kdf(), false)
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().contains("already exists"), "{err}");

    let again = Home::unlock_with(store.clone(), "right").await.unwrap();
    assert_eq!(again.identity_card_async().await.unwrap(), card);
    let err = Home::unlock_with(store.clone(), "wrong")
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().contains("wrong passphrase"), "{err}");
    let err = Home::unlock_with(MemStore::default(), "right")
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().contains("no state"), "{err}");

    // a file copied from another vault is refused (each name is bound into its seal)
    let other = MemStore::default();
    let _ = Home::create_with(other.clone(), "right", kdf(), false)
        .await
        .unwrap();
    let theirs = other.get("contacts.enc").await.unwrap().unwrap();
    store.put("identity.enc", &theirs).await.unwrap();
    assert!(again.load_identity_async().await.is_err());
}

#[test]
fn the_state_directory_holds_the_same_files_as_before() {
    let dir = temp("files");
    let home = Home::create(&dir, "pass phrase", kdf(), false).unwrap();
    home.save_attachment("m1", b"file bytes").unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "attachments",
            "contacts.enc",
            "devicelist.enc",
            "identity.enc",
            "owner.enc",
            "sessions.enc",
            "vault.json"
        ]
    );
    let att: Vec<_> = std::fs::read_dir(dir.join("attachments"))
        .unwrap()
        .collect();
    assert_eq!(att.len(), 1);
    assert!(att[0]
        .as_ref()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".enc"));
    // reopening finds the same state, and `exists` agrees
    assert!(Home::exists(&dir));
    let again = Home::unlock(&dir, "pass phrase").unwrap();
    assert_eq!(
        again.identity_card().unwrap(),
        home.identity_card().unwrap()
    );
    assert_eq!(
        again.load_attachment("m1").unwrap().as_deref(),
        Some(&b"file bytes"[..])
    );
    // creating over existing state is refused without force, as before
    let err = Home::create(&dir, "pass phrase", kdf(), false)
        .err()
        .expect("refused");
    assert!(err.to_string().contains("already exists"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unlocking_a_folder_without_state_says_so() {
    let dir = temp("empty");
    std::fs::create_dir_all(&dir).unwrap();
    let err = Home::unlock(&dir, "x").err().expect("refused");
    assert!(err.to_string().contains("no state in"), "{err}");
    let _ = FileStore::new(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}
