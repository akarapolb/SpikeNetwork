//! One device's MLS state and the group wrapper built on it (ADR 0025).
//!
//! The rules this wrapper adds on top of openmls:
//!
//! * every leaf a commit or welcome introduces is checked against the owner-bound credential and the
//!   directory **before** anything is merged ([`credential::verify_leaf`]);
//! * every commit is checked against the group's policy ([`policy::authorize`]) before it is merged;
//! * a commit we create is *pending*: the group does not move until the relay has accepted it and
//!   the caller says [`Group::confirm`], and a commit that won at the relay first replaces it;
//! * a commit that removes anyone rotates the mailbox, and the successor's secret comes from the
//!   *next* epoch, which the removed member cannot reach.

use crate::credential::{self, CredentialError, Directory, LeafIdentity};
use crate::outer::{self, Material};
use crate::policy::{self, Change, Policy, PolicyError, Roster};
use crate::CIPHERSUITE;
use ::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use sha2::{Digest, Sha256};
use spike_core::owner::OwnerKey;
use std::collections::VecDeque;

/// The GroupContext extension that carries the policy.
pub const POLICY_EXTENSION: u16 = 0xF2A0;
/// Epochs back from which application messages are still accepted: a message sent just before a
/// commit may reach us just after it.
const PAST_EPOCHS: usize = 3;
const SENT_MEMORY: usize = 64;

const L_MAILBOX: &str = "spike-mailbox";
const L_BUNDLE: &str = "spike-join-bundle";
/// Largest mailbox generation accepted from a join bundle (the inviter picks it; later code adds one).
const MAX_GENERATION: u64 = 1 << 32;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("mls: {0}")]
    Mls(String),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("a commit is already waiting for the relay")]
    PendingCommit,
    #[error("there is no commit waiting")]
    NoPendingCommit,
    #[error("no such device in the group")]
    NoSuchDevice,
    #[error("the join bundle does not open")]
    Bundle,
    #[error("{0}")]
    Malformed(&'static str),
}

fn mls<E: std::fmt::Display>(e: E) -> Error {
    Error::Mls(e.to_string())
}

fn capabilities() -> Capabilities {
    Capabilities::builder()
        .ciphersuites(vec![CIPHERSUITE])
        .extensions(vec![
            ExtensionType::Unknown(POLICY_EXTENSION),
            ExtensionType::LastResort,
        ])
        .build()
}

/// openmls insists that a custom GroupContext extension is listed in RequiredCapabilities.
fn policy_extensions(policy: &Policy) -> Result<Extensions<GroupContext>, Error> {
    Extensions::from_vec(vec![
        Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
            &[ExtensionType::Unknown(POLICY_EXTENSION)],
            &[],
            &[],
        )),
        Extension::Unknown(POLICY_EXTENSION, UnknownExtension(policy.encode())),
    ])
    .map_err(mls)
}

fn policy_of(ext: &Extensions<GroupContext>) -> Result<Policy, PolicyError> {
    let u = ext
        .unknown(POLICY_EXTENSION)
        .ok_or(PolicyError::Malformed)?;
    Policy::decode(&u.0)
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .max_past_epochs(PAST_EPOCHS)
        .build()
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn wire(m: &MlsMessageOut) -> Result<Vec<u8>, Error> {
    m.tls_serialize_detached().map_err(mls)
}

/// One device: its MLS signature key, its owner-certified credential and its crypto provider.
pub struct Device {
    pub(crate) provider: OpenMlsRustCrypto,
    pub(crate) signer: SignatureKeyPair,
    pub(crate) cwk: CredentialWithKey,
    pub(crate) device: [u8; 16],
    pub(crate) list_version: u64,
}

impl Device {
    /// A fresh MLS device identity, certified by `owner` under device-list version `list_version`.
    pub fn new(owner: &OwnerKey, device: [u8; 16], list_version: u64) -> Result<Self, Error> {
        let signer = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm()).map_err(mls)?;
        let ident = credential::certify(owner, &signer.to_public_vec(), device, list_version);
        let provider = OpenMlsRustCrypto::default();
        signer.store(provider.storage()).map_err(mls)?;
        let cwk = CredentialWithKey {
            credential: BasicCredential::new(ident).into(),
            signature_key: signer.to_public_vec().into(),
        };
        Ok(Self {
            provider,
            signer,
            cwk,
            device,
            list_version,
        })
    }

    /// A key package other members can add this device with. Single use.
    pub fn key_package(&self) -> Vec<u8> {
        let kp = KeyPackage::builder()
            .leaf_node_capabilities(capabilities())
            .build(CIPHERSUITE, &self.provider, &self.signer, self.cwk.clone())
            .expect("a key package builds")
            .key_package()
            .clone();
        kp.tls_serialize_detached().expect("serializes")
    }

    /// A key package marked "last resort": the relay hands it out only when the one-time ones have
    /// run out, and unlike those it can be used again (this device keeps its private half). Weaker,
    /// because anyone who learns it can replay it, so it is a fallback, not the plan.
    pub fn last_resort_key_package(&self) -> Vec<u8> {
        let kp = KeyPackage::builder()
            .leaf_node_capabilities(capabilities())
            .mark_as_last_resort()
            .build(CIPHERSUITE, &self.provider, &self.signer, self.cwk.clone())
            .expect("a key package builds")
            .key_package()
            .clone();
        kp.tls_serialize_detached().expect("serializes")
    }

    pub fn device_id(&self) -> [u8; 16] {
        self.device
    }

    /// A copy of everything openmls has stored. Taken before an operation that consumes secrets
    /// (decrypting a commit, using a key package) so a refusal can put them back.
    fn snapshot(&self) -> Snapshot {
        Snapshot(self.provider.storage().values.read().expect("lock").clone())
    }

    fn restore(&self, snapshot: Snapshot) {
        *self.provider.storage().values.write().expect("lock") = snapshot.0;
    }

    pub(crate) fn identity_bytes(&self) -> Vec<u8> {
        BasicCredential::try_from(self.cwk.credential.clone())
            .expect("our own credential is basic")
            .identity()
            .to_vec()
    }

    pub(crate) fn from_parts(
        signer_public: Vec<u8>,
        identity: Vec<u8>,
        device: [u8; 16],
        list_version: u64,
        storage: std::collections::HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<Self, Error> {
        let provider = OpenMlsRustCrypto::default();
        *provider.storage().values.write().expect("lock") = storage;
        // The signing key was stored with the rest when the device was made.
        let signer = SignatureKeyPair::read(
            provider.storage(),
            &signer_public,
            CIPHERSUITE.signature_algorithm(),
        )
        .ok_or(Error::Malformed("signing key missing from storage"))?;
        Ok(Self {
            provider,
            signer,
            cwk: CredentialWithKey {
                credential: BasicCredential::new(identity).into(),
                signature_key: signer_public.into(),
            },
            device,
            list_version,
        })
    }

    fn identity(&self) -> LeafIdentity {
        credential::parse(&self.cwk.credential)
            .expect("our own credential parses")
            .0
    }
}

struct Snapshot(std::collections::HashMap<Vec<u8>, Vec<u8>>);

/// What a commit we made needs to go out: the commit, a welcome for anyone it adds, and, when it
/// moves the mailbox, the next generation and its secret.
#[derive(Debug)]
pub struct Outgoing {
    pub commit: Vec<u8>,
    pub welcome: Option<Vec<u8>>,
    /// Sealed under the epoch the commit creates: the mailbox generation the newcomers should use.
    pub join_bundle: Option<Vec<u8>>,
    /// `(generation, secret)` of the mailbox this commit rotates to.
    pub successor: Option<(u64, [u8; 32])>,
}

#[derive(Debug)]
pub enum Processed {
    /// A decrypted application message and who sent it (an authenticated member of this group).
    Message(Vec<u8>, LeafIdentity),
    /// A commit, validated and merged.
    Commit {
        epoch: u64,
        removed_me: bool,
        rotated: bool,
    },
    /// Something we sent ourselves, coming back from the relay.
    Own,
}

struct Mailbox {
    generation: u64,
    secret: [u8; 32],
}

pub struct Group {
    pub(crate) mls: MlsGroup,
    policy: Policy,
    mailbox: Mailbox,
    sent: VecDeque<[u8; 32]>,
}

impl Group {
    /// Start a group with ourselves as its only member.
    pub fn create(dev: &Device, policy: Policy) -> Result<Self, Error> {
        policy.validate()?;
        let mls = MlsGroup::builder()
            .ciphersuite(CIPHERSUITE)
            .with_capabilities(capabilities())
            .with_group_context_extensions(policy_extensions(&policy)?)
            .with_wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
            .use_ratchet_tree_extension(true)
            .max_past_epochs(PAST_EPOCHS)
            .build(&dev.provider, &dev.signer, dev.cwk.clone())
            .map_err(mls)?;
        let mut g = Self {
            mls,
            policy,
            mailbox: Mailbox {
                generation: 0,
                secret: [0; 32],
            },
            sent: VecDeque::new(),
        };
        g.mailbox.secret = g.export(dev, L_MAILBOX, &0u64.to_be_bytes())?;
        Ok(g)
    }

    pub(crate) fn mailbox_parts(&self) -> (u64, [u8; 32]) {
        (self.mailbox.generation, self.mailbox.secret)
    }

    pub(crate) fn restore(
        dev: &Device,
        id: &[u8],
        generation: u64,
        secret: [u8; 32],
    ) -> Result<Self, Error> {
        let mls = MlsGroup::load(dev.provider.storage(), &GroupId::from_slice(id))
            .map_err(mls)?
            .ok_or(Error::Malformed("group missing from storage"))?;
        let policy = policy_of(mls.extensions())?;
        Ok(Self {
            mls,
            policy,
            mailbox: Mailbox { generation, secret },
            sent: VecDeque::new(),
        })
    }

    pub fn id(&self) -> Vec<u8> {
        self.mls.group_id().as_slice().to_vec()
    }
    pub fn epoch(&self) -> u64 {
        self.mls.epoch().as_u64()
    }
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
    pub fn is_active(&self) -> bool {
        self.mls.is_active()
    }
    pub fn has_pending(&self) -> bool {
        self.mls.pending_commit().is_some()
    }
    pub fn mailbox_generation(&self) -> u64 {
        self.mailbox.generation
    }
    pub fn mailbox_material(&self) -> Material {
        Material::derive(&self.mailbox.secret)
    }

    pub fn members(&self) -> Vec<LeafIdentity> {
        self.mls
            .members()
            .filter_map(|m| credential::parse(&m.credential).ok().map(|(i, _)| i))
            .collect()
    }

    fn roster(&self) -> Roster {
        let mut r = Roster::default();
        for m in self.members() {
            r.insert(m.owner, m.device);
        }
        r
    }

    fn export(&self, dev: &Device, label: &str, ctx: &[u8]) -> Result<[u8; 32], Error> {
        self.mls
            .export_secret(dev.provider.crypto(), label, ctx, 32)
            .map_err(mls)?
            .try_into()
            .map_err(|_| Error::Malformed("export length"))
    }

    fn remember(&mut self, bytes: &[u8]) {
        if self.sent.len() == SENT_MEMORY {
            self.sent.pop_front();
        }
        self.sent.push_back(digest(bytes));
    }

    // --- making commits ----------------------------------------------------------------------

    fn ensure_no_pending(&self) -> Result<(), Error> {
        if self.has_pending() {
            return Err(Error::PendingCommit);
        }
        Ok(())
    }

    /// What our own pending commit says about the mailbox and who can join under it.
    fn outgoing(
        &mut self,
        dev: &Device,
        commit: &MlsMessageOut,
        welcome: Option<&MlsMessageOut>,
        rotates: bool,
    ) -> Result<Outgoing, Error> {
        let pending = self.mls.pending_commit().ok_or(Error::NoPendingCommit)?;
        let gid = self.id();
        let next_generation = self.mailbox.generation + 1;
        let successor = if rotates {
            let secret: [u8; 32] = pending
                .export_secret(
                    dev.provider.crypto(),
                    L_MAILBOX,
                    &next_generation.to_be_bytes(),
                    32,
                )
                .map_err(mls)?
                .try_into()
                .map_err(|_| Error::Malformed("export length"))?;
            Some((next_generation, secret))
        } else {
            None
        };
        let join_bundle = if welcome.is_some() {
            // Newcomers get the mailbox generation that will be current once this commit lands.
            let (generation, secret) =
                successor.unwrap_or((self.mailbox.generation, self.mailbox.secret));
            let key: [u8; 32] = pending
                .export_secret(dev.provider.crypto(), L_BUNDLE, &gid, 32)
                .map_err(mls)?
                .try_into()
                .map_err(|_| Error::Malformed("export length"))?;
            let mut pt = generation.to_be_bytes().to_vec();
            pt.extend_from_slice(&secret);
            Some(outer::seal_raw(
                &key,
                &bundle_aad(&gid, self.epoch() + 1),
                &pt,
            ))
        } else {
            None
        };
        let commit = wire(commit)?;
        self.remember(&commit);
        Ok(Outgoing {
            commit,
            welcome: welcome.map(wire).transpose()?,
            join_bundle,
            successor,
        })
    }

    /// Add devices. Every key package's credential is checked against `dir`, and the add against
    /// the policy, exactly as every receiver will.
    pub fn add(
        &mut self,
        dev: &Device,
        dir: &dyn Directory,
        key_packages: &[Vec<u8>],
    ) -> Result<Outgoing, Error> {
        self.ensure_no_pending()?;
        let mut kps = Vec::new();
        let mut changes = Vec::new();
        for bytes in key_packages {
            let kp = KeyPackageIn::tls_deserialize_exact(bytes)
                .map_err(mls)?
                .validate(dev.provider.crypto(), ProtocolVersion::Mls10)
                .map_err(mls)?;
            let id = credential::verify_leaf(
                dir,
                kp.leaf_node().credential(),
                kp.leaf_node().signature_key().as_slice(),
            )?;
            changes.push(Change::Add {
                owner: id.owner,
                device: id.device,
            });
            kps.push(kp);
        }
        policy::authorize(
            &self.policy,
            &dev.identity().owner,
            &self.roster(),
            &changes,
        )?;
        let (commit, welcome, _info) = self
            .mls
            .add_members(&dev.provider, &dev.signer, &kps)
            .map_err(mls)?;
        self.outgoing(dev, &commit, Some(&welcome), false)
    }

    /// Remove devices (by device id). Rotates the mailbox.
    pub fn remove(&mut self, dev: &Device, devices: &[[u8; 16]]) -> Result<Outgoing, Error> {
        self.ensure_no_pending()?;
        let mut indices = Vec::new();
        let mut changes = Vec::new();
        for d in devices {
            let (index, id) = self
                .mls
                .members()
                .find_map(|m| {
                    let (id, _) = credential::parse(&m.credential).ok()?;
                    (id.device == *d).then_some((m.index, id))
                })
                .ok_or(Error::NoSuchDevice)?;
            indices.push(index);
            changes.push(Change::Remove {
                owner: id.owner,
                device: id.device,
            });
        }
        policy::authorize(
            &self.policy,
            &dev.identity().owner,
            &self.roster(),
            &changes,
        )?;
        let (commit, _w, _i) = self
            .mls
            .remove_members(&dev.provider, &dev.signer, &indices)
            .map_err(mls)?;
        self.outgoing(dev, &commit, None, true)
    }

    /// Replace the group's policy. Admins only.
    pub fn set_policy(&mut self, dev: &Device, policy: Policy) -> Result<Outgoing, Error> {
        self.ensure_no_pending()?;
        policy::authorize(
            &self.policy,
            &dev.identity().owner,
            &self.roster(),
            &[Change::SetPolicy(policy.clone())],
        )?;
        let commit = self.commit_policy(dev, &policy)?;
        self.outgoing(dev, &commit, None, false)
    }

    pub(crate) fn commit_policy(
        &mut self,
        dev: &Device,
        policy: &Policy,
    ) -> Result<MlsMessageOut, Error> {
        let bundle = self
            .mls
            .commit_builder()
            .propose_group_context_extensions(policy_extensions(policy)?)
            .map_err(mls)?
            .load_psks(dev.provider.storage())
            .map_err(mls)?
            .build(
                dev.provider.rand(),
                dev.provider.crypto(),
                &dev.signer,
                |_| true,
            )
            .map_err(mls)?
            .stage_commit(&dev.provider)
            .map_err(mls)?;
        Ok(bundle.commit().clone())
    }

    /// Refresh our own leaf key (post-compromise security).
    pub fn self_update(&mut self, dev: &Device) -> Result<Outgoing, Error> {
        self.ensure_no_pending()?;
        let bundle = self
            .mls
            .self_update(
                &dev.provider,
                &dev.signer,
                openmls::treesync::LeafNodeParameters::default(),
            )
            .map_err(mls)?;
        let commit = bundle.commit().clone();
        self.outgoing(dev, &commit, None, false)
    }

    /// The relay accepted our commit: apply it.
    pub fn confirm(&mut self, dev: &Device) -> Result<(), Error> {
        let pending = self.mls.pending_commit().ok_or(Error::NoPendingCommit)?;
        let rotates = pending.remove_proposals().next().is_some();
        let removed_me = pending.self_removed();
        self.mls.merge_pending_commit(&dev.provider).map_err(mls)?;
        self.after_merge(dev, rotates, removed_me)
    }

    /// The relay refused our commit, or another one won: forget it.
    pub fn discard(&mut self, dev: &Device) -> Result<(), Error> {
        self.mls
            .clear_pending_commit(dev.provider.storage())
            .map_err(mls)
    }

    fn after_merge(&mut self, dev: &Device, rotates: bool, removed_me: bool) -> Result<(), Error> {
        if removed_me || !self.mls.is_active() {
            return Ok(());
        }
        self.policy = policy_of(self.mls.extensions())?;
        if rotates {
            let generation = self.mailbox.generation + 1;
            self.mailbox = Mailbox {
                generation,
                secret: self.export(dev, L_MAILBOX, &generation.to_be_bytes())?,
            };
        }
        Ok(())
    }

    // --- messages ----------------------------------------------------------------------------

    pub fn encrypt(&mut self, dev: &Device, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let m = self
            .mls
            .create_message(&dev.provider, &dev.signer, plaintext)
            .map_err(mls)?;
        let bytes = wire(&m)?;
        self.remember(&bytes);
        Ok(bytes)
    }

    /// Process one message from the relay. A commit is checked in full and only then merged; if it
    /// does not pass, the group is exactly as it was.
    ///
    /// Reading a commit uses up the secrets that opened it, so a commit we refuse (a stale
    /// directory, say) would otherwise be unreadable the second time. For commits, the stored state
    /// is therefore put back on any refusal, and the same bytes can be offered again.
    pub fn process(
        &mut self,
        dev: &Device,
        dir: &dyn Directory,
        bytes: &[u8],
    ) -> Result<Processed, Error> {
        if self.sent.contains(&digest(bytes)) {
            return Ok(Processed::Own);
        }
        let msg = MlsMessageIn::tls_deserialize_exact(bytes)
            .map_err(mls)?
            .try_into_protocol_message()
            .map_err(mls)?;
        if msg.content_type() != ContentType::Commit {
            return self.process_message(dev, dir, msg);
        }
        let before = dev.snapshot();
        let result = self.process_message(dev, dir, msg);
        if result.is_err() {
            dev.restore(before);
            let id = self.mls.group_id().clone();
            if let Ok(Some(g)) = MlsGroup::load(dev.provider.storage(), &id) {
                self.mls = g;
            }
        }
        result
    }

    fn process_message(
        &mut self,
        dev: &Device,
        dir: &dyn Directory,
        msg: ProtocolMessage,
    ) -> Result<Processed, Error> {
        let processed = self.mls.process_message(&dev.provider, msg).map_err(mls)?;
        let sender = processed.sender().clone();
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(a) => {
                let Sender::Member(index) = sender else {
                    return Err(PolicyError::Forbidden("external").into());
                };
                Ok(Processed::Message(a.into_bytes(), self.identity_at(index)?))
            }
            ProcessedMessageContent::OwnPrivateMessage => Ok(Processed::Own),
            ProcessedMessageContent::StagedCommitMessage(sc) => {
                self.validate(dir, &sender, &sc)?;
                // It passed: if we were waiting on a commit of our own, this one won.
                self.mls
                    .clear_pending_commit(dev.provider.storage())
                    .map_err(mls)?;
                let rotates = sc.remove_proposals().next().is_some();
                let removed_me = sc.self_removed();
                self.mls
                    .merge_staged_commit(&dev.provider, *sc)
                    .map_err(mls)?;
                self.after_merge(dev, rotates, removed_me)?;
                Ok(Processed::Commit {
                    epoch: self.epoch(),
                    removed_me,
                    rotated: rotates && !removed_me,
                })
            }
            _ => Err(Error::Policy(PolicyError::Forbidden("proposal"))),
        }
    }

    fn identity_at(&self, index: LeafNodeIndex) -> Result<LeafIdentity, Error> {
        let m = self
            .mls
            .member_at(index)
            .ok_or(Error::Malformed("no such leaf"))?;
        Ok(credential::parse(&m.credential)?.0)
    }

    /// Everything that must hold before a staged commit may be merged.
    fn validate(
        &self,
        dir: &dyn Directory,
        sender: &Sender,
        sc: &StagedCommit,
    ) -> Result<(), Error> {
        let Sender::Member(index) = sender else {
            return Err(PolicyError::Forbidden("external").into());
        };
        let committer = self.identity_at(*index)?;
        // The sender's own leaf too: a commit that only adds people carries no new leaf of theirs,
        // and a device that has since been revoked must not keep its rights until someone removes it.
        let member = self
            .mls
            .member_at(*index)
            .ok_or(Error::Malformed("no such leaf"))?;
        credential::verify_leaf(dir, &member.credential, &member.signature_key)?;
        let verify = |leaf: &LeafNode| {
            credential::verify_leaf(dir, leaf.credential(), leaf.signature_key().as_slice())
        };

        let mut changes = Vec::new();
        if let Some(leaf) = sc.update_path_leaf_node() {
            let id = verify(leaf)?;
            changes.push(Change::Update {
                old: (committer.owner, committer.device),
                new: (id.owner, id.device),
            });
        }
        for q in sc.queued_proposals() {
            match q.proposal() {
                Proposal::Add(a) => {
                    let id = verify(a.key_package().leaf_node())?;
                    changes.push(Change::Add {
                        owner: id.owner,
                        device: id.device,
                    });
                }
                Proposal::Remove(r) => {
                    let id = self.identity_at(r.removed())?;
                    changes.push(Change::Remove {
                        owner: id.owner,
                        device: id.device,
                    });
                }
                Proposal::GroupContextExtensions(g) => {
                    changes.push(Change::SetPolicy(policy_of(g.extensions())?));
                }
                Proposal::ExternalInit(_) => changes.push(Change::Forbidden("external")),
                Proposal::ReInit(_) => changes.push(Change::Forbidden("reinit")),
                Proposal::PreSharedKey(_) => changes.push(Change::Forbidden("psk")),
                _ => changes.push(Change::Forbidden("proposal")),
            }
        }
        policy::authorize(&self.policy, &committer.owner, &self.roster(), &changes)?;
        Ok(())
    }

    // --- joining -----------------------------------------------------------------------------

    /// Join from a welcome and its bundle. Every member in the group is checked first, and nothing
    /// is kept if anything fails.
    pub fn join(
        dev: &Device,
        dir: &dyn Directory,
        welcome: &[u8],
        join_bundle: &[u8],
    ) -> Result<Self, Error> {
        let MlsMessageBodyIn::Welcome(w) = MlsMessageIn::tls_deserialize_exact(welcome)
            .map_err(mls)?
            .extract()
        else {
            return Err(Error::Malformed("not a welcome"));
        };
        // A welcome uses up the key package it was made for. If anything below refuses it (a bad
        // bundle, say), put that back so the same welcome can be tried again.
        let before = dev.snapshot();
        let result = Self::join_inner(dev, dir, w, join_bundle);
        if result.is_err() {
            dev.restore(before);
        }
        result
    }

    fn join_inner(
        dev: &Device,
        dir: &dyn Directory,
        w: Welcome,
        join_bundle: &[u8],
    ) -> Result<Self, Error> {
        let staged =
            StagedWelcome::new_from_welcome(&dev.provider, &join_config(), w, None).map_err(mls)?;
        let policy = policy_of(staged.group_context().extensions())?;
        let mut roster = Roster::default();
        for m in staged.members() {
            let id = credential::verify_leaf(dir, &m.credential, &m.signature_key)?;
            roster.insert(id.owner, id.device);
        }
        if roster.people() > policy.max_people as usize
            || roster.devices() > policy.max_devices as usize
        {
            return Err(PolicyError::TooMany("members").into());
        }
        let mls_group = staged.into_group(&dev.provider).map_err(mls)?;
        let mut g = Self {
            mls: mls_group,
            policy,
            mailbox: Mailbox {
                generation: 0,
                secret: [0; 32],
            },
            sent: VecDeque::new(),
        };
        match g.open_bundle(dev, join_bundle) {
            Ok(mailbox) => {
                g.mailbox = mailbox;
                Ok(g)
            }
            Err(e) => Err(e),
        }
    }

    fn open_bundle(&self, dev: &Device, bundle: &[u8]) -> Result<Mailbox, Error> {
        let gid = self.id();
        let key = self.export(dev, L_BUNDLE, &gid)?;
        let pt = outer::open_raw(&key, &bundle_aad(&gid, self.epoch()), bundle)
            .map_err(|_| Error::Bundle)?;
        if pt.len() != 40 {
            return Err(Error::Bundle);
        }
        // The inviter chose this number; later code adds one to it. Refuse one near the end of the range.
        let generation = u64::from_be_bytes(pt[..8].try_into().expect("8 bytes"));
        if generation > MAX_GENERATION {
            return Err(Error::Bundle);
        }
        Ok(Mailbox {
            generation,
            secret: pt[8..].try_into().expect("32 bytes"),
        })
    }
}

fn bundle_aad(group: &[u8], epoch: u64) -> Vec<u8> {
    let mut v = b"spike-mls-bundle-v1".to_vec();
    v.extend_from_slice(group);
    v.extend_from_slice(&epoch.to_be_bytes());
    v
}
