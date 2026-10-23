//! Spike: a thin wrapper around openmls 0.9.2 shaped like Spike Connect's identity model.
//!
//! * One MLS leaf per DEVICE.
//! * The leaf's credential is a BasicCredential whose identity is
//!   `owner_pub(32) | device_id(16) | owner_sig(64)`, where `owner_sig` is the person's
//!   Ed25519 owner-key signature over `DOMAIN | leaf_signature_pub | device_id`.
//! * openmls has no credential-validation callback. It hands us the credentials of every
//!   leaf a commit/welcome introduces and we must check them ourselves BEFORE merging
//!   (`StagedCommit::credentials_to_verify`, `update_path_leaf_node`, add/update proposals).

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use openmls::prelude::*;
use openmls::treesync::RatchetTree;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use ::tls_codec::{Deserialize as _, Serialize as _};

pub const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
const DOMAIN: &[u8] = b"spike-mls-leaf-v1";

/// A person's long-term owner key (stand-in for core/src/owner.rs).
pub struct Owner(pub SigningKey);

impl Owner {
    pub fn random() -> Self {
        Owner(SigningKey::from_bytes(&rand::random::<[u8; 32]>()))
    }
    pub fn public(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    /// The credential identity bytes for a device whose MLS signature key is `leaf_pub`.
    pub fn certify(&self, leaf_pub: &[u8], device: [u8; 16]) -> Vec<u8> {
        let sig = self.0.sign(&signed_bytes(leaf_pub, &device));
        let mut v = Vec::with_capacity(112);
        v.extend_from_slice(&self.public());
        v.extend_from_slice(&device);
        v.extend_from_slice(&sig.to_bytes());
        v
    }
}

fn signed_bytes(leaf_pub: &[u8], device: &[u8; 16]) -> Vec<u8> {
    [DOMAIN, leaf_pub, device].concat()
}

/// Check that `cred` says "this leaf belongs to owner X" and X really signed this leaf key.
/// `trusted` decides whether we know/accept owner X (contact list, group member list...).
/// Returns (owner_pub, device_id).
pub fn verify_leaf(
    trusted: &dyn Fn(&[u8; 32]) -> bool,
    cred: &Credential,
    leaf_pub: &[u8],
) -> Result<([u8; 32], [u8; 16]), String> {
    if cred.credential_type() != CredentialType::Basic {
        return Err("not a basic credential".into());
    }
    let basic = BasicCredential::try_from(cred.clone()).map_err(|e| e.to_string())?;
    let id = basic.identity();
    if id.len() != 112 {
        return Err(format!("bad identity length {}", id.len()));
    }
    let owner: [u8; 32] = id[..32].try_into().unwrap();
    let device: [u8; 16] = id[32..48].try_into().unwrap();
    let sig = Signature::from_slice(&id[48..]).map_err(|e| e.to_string())?;
    if !trusted(&owner) {
        return Err("unknown owner".into());
    }
    VerifyingKey::from_bytes(&owner)
        .map_err(|e| e.to_string())?
        .verify(&signed_bytes(leaf_pub, &device), &sig)
        .map_err(|_| "owner signature does not cover this leaf key".to_string())?;
    Ok((owner, device))
}

pub enum Processed {
    App(Vec<u8>),
    /// A commit was merged. `removed_me` = this client is no longer in the group.
    Commit { removed_me: bool },
}

/// One device: MLS state + keys. Everything is in memory (openmls_memory_storage).
pub struct Client {
    pub provider: OpenMlsRustCrypto,
    pub signer: SignatureKeyPair,
    pub cwk: CredentialWithKey,
    pub group: Option<MlsGroup>,
}

pub fn config() -> MlsGroupCreateConfig {
    MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
        // The ratchet tree travels out of band (relay blob), not inside the Welcome.
        .use_ratchet_tree_extension(false)
        .build()
}

pub fn to_wire(m: &MlsMessageOut) -> Vec<u8> {
    m.tls_serialize_detached().unwrap()
}
pub fn from_wire(b: &[u8]) -> MlsMessageIn {
    MlsMessageIn::tls_deserialize_exact(b).unwrap()
}

impl Client {
    pub fn new(owner: &Owner, device: [u8; 16]) -> Self {
        let signer = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        let ident = owner.certify(&signer.to_public_vec(), device);
        let cwk = CredentialWithKey {
            credential: BasicCredential::new(ident).into(),
            signature_key: signer.to_public_vec().into(),
        };
        Client { provider: OpenMlsRustCrypto::default(), signer, cwk, group: None }
    }

    pub fn key_package(&self) -> KeyPackage {
        KeyPackage::builder()
            .build(CS, &self.provider, &self.signer, self.cwk.clone())
            .unwrap()
            .key_package()
            .clone()
    }

    pub fn create_group(&mut self) {
        self.group =
            Some(MlsGroup::new(&self.provider, &self.signer, &config(), self.cwk.clone()).unwrap());
    }

    /// Returns (commit, welcome) wire bytes and merges our own commit.
    pub fn add(&mut self, kps: &[KeyPackage]) -> (Vec<u8>, Vec<u8>) {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        let (commit, welcome, _gi) = g.add_members(p, s, kps).unwrap();
        g.merge_pending_commit(p).unwrap();
        (to_wire(&commit), to_wire(&welcome))
    }

    pub fn remove(&mut self, idx: &[LeafNodeIndex]) -> Vec<u8> {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        let (commit, _w, _gi) = g.remove_members(p, s, idx).unwrap();
        g.merge_pending_commit(p).unwrap();
        to_wire(&commit)
    }

    pub fn self_update(&mut self) -> Vec<u8> {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        let bundle = g.self_update(p, s, openmls::treesync::LeafNodeParameters::default()).unwrap();
        g.merge_pending_commit(p).unwrap();
        to_wire(bundle.commit())
    }

    pub fn send(&mut self, msg: &[u8]) -> Vec<u8> {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().expect("no group");
        to_wire(&g.create_message(p, s, msg).unwrap())
    }

    pub fn tree(&self) -> RatchetTree {
        self.group.as_ref().unwrap().export_ratchet_tree()
    }

    /// Join from a Welcome. Every member credential is verified; on failure the group is dropped.
    pub fn join(
        &mut self,
        welcome: &[u8],
        tree: RatchetTree,
        trusted: &dyn Fn(&[u8; 32]) -> bool,
    ) -> Result<(), String> {
        let MlsMessageBodyIn::Welcome(w) = from_wire(welcome).extract() else {
            return Err("not a welcome".into());
        };
        let staged =
            StagedWelcome::new_from_welcome(&self.provider, config().join_config(), w, Some(tree.into()))
                .map_err(|e| e.to_string())?;
        // Verify every member BEFORE the group is written to storage.
        for m in staged.members() {
            verify_leaf(trusted, &m.credential, &m.signature_key)?;
        }
        self.group = Some(staged.into_group(&self.provider).map_err(|e| e.to_string())?);
        Ok(())
    }

    /// Process an incoming handshake/application message. Commits are credential-checked first.
    pub fn process(
        &mut self,
        wire: &[u8],
        trusted: &dyn Fn(&[u8; 32]) -> bool,
    ) -> Result<Processed, String> {
        let msg = from_wire(wire).try_into_protocol_message().map_err(|e| e.to_string())?;
        let p = &self.provider;
        let g = self.group.as_mut().ok_or("no group")?;
        let processed = g.process_message(p, msg).map_err(|e| e.to_string())?;
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(a) => Ok(Processed::App(a.into_bytes())),
            ProcessedMessageContent::StagedCommitMessage(sc) => {
                // The hook openmls gives us: check every leaf this commit introduces.
                let mut leaves: Vec<LeafNode> = Vec::new();
                leaves.extend(sc.update_path_leaf_node().cloned());
                for a in sc.add_proposals() {
                    leaves.push(a.add_proposal().key_package().leaf_node().clone());
                }
                for u in sc.update_proposals() {
                    leaves.push(u.update_proposal().leaf_node().clone());
                }
                for l in &leaves {
                    verify_leaf(trusted, l.credential(), l.signature_key().as_slice())?;
                }
                let removed_me = sc.self_removed();
                g.merge_staged_commit(p, *sc).map_err(|e| e.to_string())?;
                Ok(Processed::Commit { removed_me })
            }
            _ => Err("proposal / external message not expected".into()),
        }
    }
}
