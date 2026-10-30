//! Files and tags written by an earlier build of the crypto dependencies (RustCrypto: sha2 0.10, hmac 0.12,
//! hkdf 0.12, chacha20poly1305 0.10) must still open after those crates move to a new major version. The
//! values below were produced by that build with the cheap test KDF; if one of these tests fails, a dependency
//! bump has changed what existing users' vaults and attachments mean.

use spike_core::attachment::Attachment;
use spike_core::vault::{Vault, VaultHeader};

const HEADER: &str = r#"{  "version": 1,  "salt": "a2bf92f775ffae6d08a855dd365822ea",  "kdf": {    "memory_kib": 8,    "iterations": 1,    "parallelism": 1  },  "check": "53504b314561bac1946f557e973162ed08bba38e0f12b1c9cfdbba199b58df59b0cb16ffba17a0cb59fb2ab430602f4c181c2be55da011ce1822"}"#;
const SEALED: &str = "53504b312105b5ec405e97bbc300712a584eaf5e6b95bcda992244f16ba3cf7ce33e37f27ce9853862b58c48c83cc025308ca44e430d8018caa0fe8a";
const TAG: &str = "65d8f76643db425e5ae73821b344cf48f010d7c0e257eacd0b7dba0528292cc2";
const ATT: &str = r#"{"name":"a.txt","size":16,"sha256":"2508f58332a50c3fee16cc39d28bd45b17d7c3d65ec32b7ebd024d55b7a1393d","blob":"e1364e3d87707d85eab12f703eb71823","key":"1f3b8b0ccc284bc7d2862186feff4cfd7d95970b743680a19ce673996593782e","chunks":1}"#;
const CHUNK0: &str = "356ffc07711a75831d999547c303b16cea1609315d26272a0b0d892b6ed417da1e11e91ba962b9adc03366773ccc2baa01ae90f435d43a14";

#[test]
fn a_vault_written_by_the_previous_crypto_versions_still_opens() {
    let header = VaultHeader::from_json(HEADER).unwrap();
    let vault = Vault::unlock("fixture pass", &header).expect("same passphrase, same key");
    assert!(Vault::unlock("other pass", &header).is_err());
    let sealed = hex::decode(SEALED).unwrap();
    assert_eq!(
        vault.open("contacts.enc", &sealed).unwrap(),
        b"hello old crypto"
    );
    // the name is bound into the seal
    assert!(vault.open("owner.enc", &sealed).is_err());
    // keyed index tags (HMAC-SHA256 under a key derived with HKDF) are unchanged
    assert_eq!(hex::encode(vault.index_tag("attachment", b"msg-1")), TAG);
}

#[test]
fn an_attachment_chunk_sealed_by_the_previous_crypto_versions_still_opens() {
    let att: Attachment = serde_json::from_str(ATT).unwrap();
    let chunk = hex::decode(CHUNK0).unwrap();
    assert_eq!(att.open_chunk(0, &chunk).unwrap(), b"attachment bytes");
}
