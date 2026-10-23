//! Facts about openmls 0.9.2 that ADR 0025's design depends on, checked directly against the
//! library. If an upgrade breaks one of these, the design (not just the code) needs another look.

use ::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
const EXT: u16 = 0xF2A0;

struct Dev {
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    cwk: CredentialWithKey,
}

fn dev(name: &str) -> Dev {
    let signer = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
    let cwk = CredentialWithKey {
        credential: BasicCredential::new(name.as_bytes().to_vec()).into(),
        signature_key: signer.to_public_vec().into(),
    };
    Dev {
        provider: OpenMlsRustCrypto::default(),
        signer,
        cwk,
    }
}

fn caps() -> Capabilities {
    Capabilities::builder()
        .ciphersuites(vec![CS])
        .extensions(vec![ExtensionType::Unknown(EXT)])
        .build()
}

/// openmls requires a custom group-context extension to be listed in the group's
/// RequiredCapabilities, so every set of extensions carries both.
fn ext(bytes: &[u8]) -> Extensions<GroupContext> {
    Extensions::from_vec(vec![
        Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
            &[ExtensionType::Unknown(EXT)],
            &[],
            &[],
        )),
        Extension::Unknown(EXT, UnknownExtension(bytes.to_vec())),
    ])
    .unwrap()
}

fn key_package(d: &Dev) -> KeyPackage {
    KeyPackage::builder()
        .leaf_node_capabilities(caps())
        .build(CS, &d.provider, &d.signer, d.cwk.clone())
        .unwrap()
        .key_package()
        .clone()
}

fn new_group(d: &Dev, policy: &[u8]) -> MlsGroup {
    MlsGroup::builder()
        .ciphersuite(CS)
        .with_capabilities(caps())
        .with_group_context_extensions(ext(policy))
        .with_wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .build(&d.provider, &d.signer, d.cwk.clone())
        .unwrap()
}

fn join(d: &Dev, welcome: &MlsMessageOut) -> MlsGroup {
    let bytes = welcome.tls_serialize_detached().unwrap();
    let MlsMessageBodyIn::Welcome(w) = MlsMessageIn::tls_deserialize_exact(&bytes)
        .unwrap()
        .extract()
    else {
        panic!("not a welcome")
    };
    StagedWelcome::new_from_welcome(
        &d.provider,
        &MlsGroupJoinConfig::builder()
            .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
            .use_ratchet_tree_extension(true)
            .build(),
        w,
        None,
    )
    .unwrap()
    .into_group(&d.provider)
    .unwrap()
}

fn open(msg: &MlsMessageOut) -> ProtocolMessage {
    let bytes = msg.tls_serialize_detached().unwrap();
    MlsMessageIn::tls_deserialize_exact(&bytes)
        .unwrap()
        .try_into_protocol_message()
        .unwrap()
}

fn staged(d: &Dev, g: &mut MlsGroup, msg: &MlsMessageOut) -> StagedCommit {
    match g
        .process_message(&d.provider, open(msg))
        .unwrap()
        .into_content()
    {
        ProcessedMessageContent::StagedCommitMessage(s) => *s,
        _ => panic!("not a commit"),
    }
}

#[test]
fn a_custom_group_context_extension_reaches_everyone_through_a_welcome() {
    let (a, b) = (dev("a"), dev("b"));
    let mut ga = new_group(&a, b"policy v1");
    assert_eq!(ga.extensions().unknown(EXT).unwrap().0, b"policy v1");
    let (_c, w, _i) = ga
        .add_members(&a.provider, &a.signer, &[key_package(&b)])
        .unwrap();
    ga.merge_pending_commit(&a.provider).unwrap();
    let gb = join(&b, &w);
    assert_eq!(gb.extensions().unknown(EXT).unwrap().0, b"policy v1");
}

#[test]
fn handshake_messages_are_encrypted_with_the_pure_ciphertext_policy() {
    let (a, b) = (dev("a"), dev("b"));
    let mut ga = new_group(&a, b"p");
    let (commit, _w, _i) = ga
        .add_members(&a.provider, &a.signer, &[key_package(&b)])
        .unwrap();
    let bytes = commit.tls_serialize_detached().unwrap();
    assert!(matches!(
        MlsMessageIn::tls_deserialize_exact(&bytes)
            .unwrap()
            .extract(),
        MlsMessageBodyIn::PrivateMessage(_)
    ));
}

/// The ADR first said a removal commit could carry the new mailbox key, because the removed member
/// "cannot process the removal". She can: the commit is encrypted under the epoch she is still in.
/// So nothing secret may travel inside it. This pins that fact.
#[test]
fn a_removed_member_can_read_what_is_inside_the_commit_that_removes_her() {
    let (a, b, c) = (dev("a"), dev("b"), dev("c"));
    let mut ga = new_group(&a, b"p");
    let (_c, w, _i) = ga
        .add_members(&a.provider, &a.signer, &[key_package(&b), key_package(&c)])
        .unwrap();
    ga.merge_pending_commit(&a.provider).unwrap();
    let mut gb = join(&b, &w);
    let mut gc = join(&c, &w);
    let c_leaf = gc.own_leaf_index();

    // Alice removes Carol and, in the same commit, changes the group context to carry a "secret".
    let bundle = ga
        .commit_builder()
        .propose_removals([c_leaf])
        .propose_group_context_extensions(ext(b"NEW MAILBOX KEY"))
        .unwrap()
        .load_psks(a.provider.storage())
        .unwrap()
        .build(a.provider.rand(), a.provider.crypto(), &a.signer, |_| true)
        .unwrap()
        .stage_commit(&a.provider)
        .unwrap();
    let commit = bundle.commit().clone();

    let sc = staged(&c, &mut gc, &commit);
    assert!(sc.self_removed(), "she learns she is out...");
    let read: Vec<Vec<u8>> = sc
        .queued_proposals()
        .filter_map(|q| match q.proposal() {
            Proposal::GroupContextExtensions(g) => g.extensions().unknown(EXT).map(|u| u.0.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        read,
        [b"NEW MAILBOX KEY".to_vec()],
        "...and can read the proposals inside it"
    );
    // But she cannot derive anything from the epoch that follows.
    assert!(sc
        .export_secret(c.provider.crypto(), "spike-mailbox", b"ctx", 32)
        .is_err());
    // Bob, who stays, can.
    let sb = staged(&b, &mut gb, &commit);
    assert!(sb
        .export_secret(b.provider.crypto(), "spike-mailbox", b"ctx", 32)
        .is_ok());
}

/// The committer can know the next epoch's exporter secret before its commit is merged, and it is
/// the one everyone else derives. That lets a seal carry a successor key derived from the new
/// epoch, without merging before the relay has accepted the commit.
#[test]
fn the_committer_can_derive_the_next_epoch_secret_from_a_pending_commit() {
    let (a, b, c) = (dev("a"), dev("b"), dev("c"));
    let mut ga = new_group(&a, b"p");
    let (_c, w, _i) = ga
        .add_members(&a.provider, &a.signer, &[key_package(&b), key_package(&c)])
        .unwrap();
    ga.merge_pending_commit(&a.provider).unwrap();
    let mut gb = join(&b, &w);
    let gc = join(&c, &w);

    let (commit, _w, _i) = ga
        .remove_members(&a.provider, &a.signer, &[gc.own_leaf_index()])
        .unwrap();
    let epoch_before = ga.epoch();
    // Pending, not merged: the group is still in the old epoch.
    let pending = ga
        .pending_commit()
        .unwrap()
        .export_secret(a.provider.crypto(), "spike-mailbox", b"ctx", 32)
        .unwrap();
    assert_eq!(ga.epoch(), epoch_before);

    let sb = staged(&b, &mut gb, &commit);
    let at_b = sb
        .export_secret(b.provider.crypto(), "spike-mailbox", b"ctx", 32)
        .unwrap();
    assert_eq!(
        pending, at_b,
        "the committer and a member agree before anyone merges"
    );
    gb.merge_staged_commit(&b.provider, sb).unwrap();
    ga.merge_pending_commit(&a.provider).unwrap();
    let after = ga
        .export_secret(a.provider.crypto(), "spike-mailbox", b"ctx", 32)
        .unwrap();
    assert_eq!(after, pending, "and it is what the merged group exports");
    assert_eq!(
        gb.export_secret(b.provider.crypto(), "spike-mailbox", b"ctx", 32)
            .unwrap(),
        after
    );
    // A different label or context gives a different secret.
    assert_ne!(
        ga.export_secret(a.provider.crypto(), "other", b"ctx", 32)
            .unwrap(),
        after
    );
}
