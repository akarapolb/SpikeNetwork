//! Shared by the fuzz targets: one small world (Alice, Bob, a candidate Dave) built once per
//! process, with real messages to mutate. Random bytes almost never parse as MLS, so each target
//! also turns its input into edits of a *valid* message: that is where the interesting paths are.
#![allow(dead_code)]

use spike_core::identity::Identity;
use spike_core::owner::{DeviceEntry, DeviceList, OwnerKey};
use spike_mls::credential::MapDirectory;
use spike_mls::group::{Device, Group};
use std::sync::OnceLock;
use spike_mls::policy::Policy;

pub struct World {
    pub dir: MapDirectory,
    /// Bob's device and group as they were before he saw `commit` and `message`; each run starts
    /// from a fresh copy (see [`World::bob`]), so nothing one input does reaches the next.
    bob_state: Vec<u8>,
    /// Dave's device, holding the key package the Welcome was made for.
    dave_state: Vec<u8>,
    /// A real commit (Alice adding Dave) that Bob has not applied.
    pub commit: Vec<u8>,
    /// A real application message from Alice that Bob has not read.
    pub message: Vec<u8>,
    /// The Welcome and join bundle for Dave from that same commit.
    pub welcome: Vec<u8>,
    pub bundle: Vec<u8>,
    /// Dave's key package, as published.
    pub key_package: Vec<u8>,
}

fn device_id(i: &Identity) -> [u8; 16] {
    hex::decode(i.device_id()).unwrap().try_into().unwrap()
}

impl World {
    /// A fresh copy of Bob and his group.
    pub fn bob(&self) -> (Device, Group) {
        let (dev, mut groups) = spike_mls::state::import(&self.bob_state).unwrap();
        (dev, groups.pop().unwrap())
    }

    /// A fresh copy of Dave, who has not yet used his key package.
    pub fn dave(&self) -> Device {
        spike_mls::state::import(&self.dave_state).unwrap().0
    }

    pub fn new() -> Self {
        let owners = [OwnerKey::generate(), OwnerKey::generate(), OwnerKey::generate()];
        let ids = [
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
            Identity::generate().unwrap(),
        ];
        let mut dir = MapDirectory::default();
        for (o, i) in owners.iter().zip(&ids) {
            let e = DeviceEntry {
                card: i.card().encode(),
                relay: None,
            };
            dir.insert(DeviceList::sign(o, 1, vec![e]).unwrap());
        }
        let dev = |n: usize| Device::new(&owners[n], device_id(&ids[n]), 1).unwrap();
        let (alice, bob, dave) = (dev(0), dev(1), dev(2));
        let mut ga = Group::create(&alice, Policy::new(vec![owners[0].public(), owners[1].public()])).unwrap();
        let first = ga.add(&alice, &dir, &[bob.key_package()]).unwrap();
        ga.confirm(&alice).unwrap();
        let bob_group = Group::join(
            &bob,
            &dir,
            first.welcome.as_ref().unwrap(),
            first.join_bundle.as_ref().unwrap(),
        )
        .unwrap();
        let message = ga.encrypt(&alice, b"a real message").unwrap();
        let key_package = dave.key_package();
        let next = ga.add(&alice, &dir, &[key_package.clone()]).unwrap();
        let bob_state = spike_mls::state::export(&bob, &[&bob_group]).unwrap();
        let dave_state = spike_mls::state::export(&dave, &[]).unwrap();
        Self {
            dir,
            bob_state,
            dave_state,
            commit: next.commit,
            message,
            welcome: next.welcome.unwrap(),
            bundle: next.join_bundle.unwrap(),
            key_package,
        }
    }
}

/// Apply edits described by `ops` to `base`: each 4 bytes are (position, kind, value).
/// Kinds: flip a byte, set a byte, delete a run, insert a run, truncate.
pub fn mutate(base: &[u8], ops: &[u8]) -> Vec<u8> {
    let mut v = base.to_vec();
    for op in ops.chunks_exact(4).take(16) {
        if v.is_empty() {
            break;
        }
        let pos = (u16::from_be_bytes([op[0], op[1]]) as usize) % v.len();
        let val = op[3];
        match op[2] % 5 {
            0 => v[pos] ^= val | 1,
            1 => v[pos] = val,
            2 => {
                let n = (val as usize % 32).min(v.len() - pos);
                v.drain(pos..pos + n);
            }
            3 => {
                let n = val as usize % 32;
                v.splice(pos..pos, std::iter::repeat_n(val, n));
            }
            _ => v.truncate(pos),
        }
    }
    v
}

/// Pick what to feed: raw bytes, or edits of one of the valid inputs.
pub fn pick<'a>(data: &'a [u8], bases: &[&[u8]]) -> Vec<u8> {
    let Some((&mode, rest)) = data.split_first() else {
        return vec![];
    };
    let m = mode as usize % (bases.len() + 1);
    if m == bases.len() {
        rest.to_vec()
    } else {
        mutate(bases[m], rest)
    }
}

/// The world, built once per process.
pub fn world() -> &'static World {
    static W: OnceLock<World> = OnceLock::new();
    W.get_or_init(World::new)
}
