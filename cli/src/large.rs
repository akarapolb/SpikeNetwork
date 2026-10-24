//! Large groups on MLS (ADR 0025), from the command-line client.
//!
//! The cryptography and the rules live in `spike-mls`; this module is the glue: where the keys and
//! state are kept, where device lists come from, how commits reach the relay and the other members,
//! and how invitations are consented to. Not here yet (see the ADR): the desktop, instant delivery
//! (this polls with `sync`), leaving, block, history for new members, and files.

use crate::client::{Appended, Relay};
use crate::groups;
use crate::messages::{Direction, StoredMessage};
use crate::ops;
use crate::state::Home;
use crate::transport::Target;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::attachment::Attachment;
use spike_core::envelope::Envelope;
use spike_core::owner::DeviceList;
use spike_core::wire::GroupKind;
use spike_mls::credential::{CredentialError, Directory};
use spike_mls::group::{Device, Error as MlsError, Group, Processed};
use spike_mls::outer::{self, Kind as OuterKind};
use spike_mls::policy::Policy;
use spike_mls::state as mls_state;
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

const STATE: &str = "large-mls.enc";
const RECORDS: &str = "large-groups.enc";
const LISTS: &str = "large-lists.enc";
/// How many times a commit is rebuilt on top of a winner before giving up.
const RETRIES: usize = 5;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Someone invited us; nothing has been joined or read.
    Invited,
    Active,
    /// We were removed. What we had stays; nothing more arrives.
    Removed,
    /// We left. What we had stays; nothing more is read or sent.
    Left,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Invite {
    /// Contact name of whoever invited us.
    pub from: String,
    /// The Welcome and join bundle, as an uploaded file.
    pub welcome: Attachment,
    /// The relay position of the commit that added us.
    pub seq: u64,
    /// Settled already without asking: one of our own devices added this device (true), or the
    /// person decided on another device (true to join, false to turn it down). `sync` acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Record {
    /// The MLS group id, hex. Also the conversation key in the message store.
    pub id: String,
    pub name: String,
    /// The relay that holds the group's mailbox.
    pub relay: String,
    pub status: Status,
    /// The last relay position processed in the current mailbox.
    pub cursor: u64,
    pub members: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<Invite>,
    /// The latest entries every member refused (they broke a rule or could not be read). They are
    /// skipped, identically on every device, so the group carries on; this is how someone finds out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<Refusal>,
    /// How many entries have been refused over the group's life.
    #[serde(default)]
    pub refused_total: u64,
    /// People (owner keys) who said they are leaving and are still in the group: an admin removes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_leavers: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub seq: u64,
    pub why: String,
}

const KEEP_REFUSALS: usize = 20;

fn refuse(events: &mut Vec<Event>, rec: &mut Record, seq: u64, why: String) {
    rec.refused_total += 1;
    rec.refused.push(Refusal {
        seq,
        why: why.clone(),
    });
    if rec.refused.len() > KEEP_REFUSALS {
        rec.refused.remove(0);
    }
    events.push(Event::Refused {
        group: rec.name.clone(),
        seq,
        why,
    });
}

/// What a sync found.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Message {
        group: String,
        from: String,
        body: String,
        id: String,
    },
    /// A commit was applied: people joined or left, or the policy changed.
    Changed {
        group: String,
        members: usize,
        rotated: bool,
    },
    /// We were removed from the group.
    Removed { group: String },
    /// Our own devices were added to, or removed from, some of our groups.
    OwnDevices,
    /// Someone told the group they are leaving; an admin removes them on their next sync.
    Left { group: String, who: String },
    /// A mailbox entry that every member must refuse (it broke a rule) or that could not be read.
    Refused {
        group: String,
        seq: u64,
        why: String,
    },
}

#[derive(Debug)]
pub struct AddReport {
    /// Devices of that person who were added.
    pub devices: usize,
    /// The relay position of the commit.
    pub seq: u64,
    /// At least one device was added with its reusable fallback package, because its one-time ones
    /// had run out. Works, but weaker: ask them to run `spike large keys`.
    pub used_last_resort: bool,
}

// --- state --------------------------------------------------------------------------------------

pub fn list(home: &Home) -> Result<Vec<Record>> {
    home.load_json(RECORDS)
}

fn save_records(home: &Home, all: &[Record]) -> Result<()> {
    home.save_json(RECORDS, &all)
}

pub fn find(home: &Home, query: &str) -> Result<Record> {
    let all = list(home)?;
    let q = query.to_lowercase();
    let hits: Vec<&Record> = all
        .iter()
        .filter(|r| r.name.to_lowercase() == q || r.id.starts_with(&q))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("no large group '{query}'"),
        _ => bail!("'{query}' matches more than one large group; use more of its id"),
    }
}

fn update_record(home: &Home, rec: &Record) -> Result<()> {
    let mut all = list(home)?;
    match all.iter_mut().find(|r| r.id == rec.id) {
        Some(slot) => *slot = rec.clone(),
        None => all.push(rec.clone()),
    }
    save_records(home, &all)
}

/// This device's MLS identity and every large group it is in.
pub(crate) struct Mls {
    pub(crate) dev: Device,
    pub(crate) groups: HashMap<String, Group>,
}

pub(crate) fn load_mls(home: &Home) -> Result<Mls> {
    if let Some(bytes) = home.load_bytes(STATE)? {
        let (dev, groups) =
            mls_state::import(&bytes).map_err(|e| anyhow!("large-group state: {e}"))?;
        return Ok(Mls {
            dev,
            groups: groups
                .into_iter()
                .map(|g| (hex::encode(g.id()), g))
                .collect(),
        });
    }
    let owner = home.load_owner()?;
    let me = home.load_identity()?;
    let device: [u8; 16] = hex::decode(me.device_id())?
        .try_into()
        .map_err(|_| anyhow!("device id"))?;
    let version = home.load_device_list()?.version;
    let dev = Device::new(&owner, device, version).map_err(|e| anyhow!("{e}"))?;
    let mls = Mls {
        dev,
        groups: HashMap::new(),
    };
    save_mls(home, &mls)?;
    Ok(mls)
}

pub(crate) fn save_mls(home: &Home, mls: &Mls) -> Result<()> {
    let groups: Vec<&Group> = mls.groups.values().collect();
    let bytes = mls_state::export(&mls.dev, &groups).map_err(|e| anyhow!("{e}"))?;
    home.save_bytes(STATE, &bytes)
}

// --- who is who ---------------------------------------------------------------------------------

/// Device lists for checking credentials, **as this group knows them** (ADR 0025, slice 5).
///
/// A credential is valid while its device is in the owner's current signed list, so what counts as
/// "current" decides whether a commit is accepted. If each member used whatever list it last fetched,
/// a member who was offline across a revocation would refuse commits that were legitimate when they
/// were made, and fall out of step for good. So the group's idea of each list comes only from the
/// group's own history: the lists in the Welcome that brought us in, and `LargeLists` messages in the
/// mailbox (an owner's device announces a new list *before* it commits anything that depends on it).
/// Every member, replaying the same log in order, then reaches the same verdicts.
struct GroupDir {
    id: String,
    lists: Mutex<HashMap<[u8; 32], DeviceList>>,
    missed: Mutex<BTreeSet<[u8; 32]>>,
    dirty: std::sync::atomic::AtomicBool,
}

fn owner_bytes(list: &DeviceList) -> Option<[u8; 32]> {
    hex::decode(&list.owner).ok()?.try_into().ok()
}

fn overlay_file(id: &str) -> String {
    format!("large-lists-{id}.enc")
}

impl GroupDir {
    /// What this group has announced so far. A group made before lists were announced has no saved
    /// overlay yet: it starts from everything this device knows, once.
    fn for_group(home: &Home, id: &str) -> Result<Self> {
        let saved: Option<Vec<DeviceList>> = match home.load_bytes(&overlay_file(id))? {
            Some(b) => Some(serde_json::from_slice(&b)?),
            None => None,
        };
        let dir = match saved {
            Some(lists) => {
                let d = Self::empty(id);
                for l in &lists {
                    d.apply(l);
                }
                d
            }
            None => {
                let d = Self::seed(home, id, &[])?;
                d.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                d
            }
        };
        Ok(dir)
    }

    fn empty(id: &str) -> Self {
        Self {
            id: id.to_string(),
            lists: Mutex::new(HashMap::new()),
            missed: Mutex::new(BTreeSet::new()),
            dirty: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Everything this device knows about anyone (contacts, our own list, lists fetched before), plus
    /// `extra` (the Welcome's). Used to join a group and to start one.
    fn seed(home: &Home, id: &str, extra: &[DeviceList]) -> Result<Self> {
        let d = Self::empty(id);
        for c in home.load_contacts()? {
            if let Some(l) = c.identity()?.list() {
                d.apply(l);
            }
        }
        d.apply(&home.load_device_list()?);
        for l in home.load_json::<Vec<DeviceList>>(LISTS)? {
            d.apply(&l);
        }
        for l in extra {
            d.apply(l);
        }
        Ok(d)
    }

    /// Take a list if it is genuinely the owner's and newer than ours. Returns whether it was taken.
    fn apply(&self, list: &DeviceList) -> bool {
        let Some(o) = owner_bytes(list) else {
            return false;
        };
        if list.verify().is_err() {
            return false;
        }
        let mut lists = self.lists.lock().expect("lock");
        if lists
            .get(&o)
            .is_some_and(|have| have.version >= list.version)
        {
            return false;
        }
        lists.insert(o, list.clone());
        self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
        true
    }

    /// Whether this list would teach the group something.
    fn is_news(&self, list: &DeviceList) -> bool {
        owner_bytes(list).is_some_and(|o| {
            list.verify().is_ok()
                && self
                    .lists
                    .lock()
                    .expect("lock")
                    .get(&o)
                    .is_none_or(|have| have.version < list.version)
        })
    }

    fn lists_of(&self, owners: &[[u8; 32]]) -> Vec<DeviceList> {
        let lists = self.lists.lock().expect("lock");
        owners
            .iter()
            .filter_map(|o| lists.get(o).cloned())
            .collect()
    }

    fn save(&self, home: &Home) -> Result<()> {
        if self.dirty.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let all: Vec<DeviceList> = self.lists.lock().expect("lock").values().cloned().collect();
            home.save_bytes(&overlay_file(&self.id), &serde_json::to_vec(&all)?)?;
        }
        Ok(())
    }

    fn take_missed(&self) -> Vec<[u8; 32]> {
        std::mem::take(&mut *self.missed.lock().expect("lock"))
            .into_iter()
            .collect()
    }

    /// Fetch these owners' lists from the relay, keep the valid newer ones. Only for joining, where
    /// there is no history to follow yet. Returns how many changed.
    async fn refresh(&self, home: &Home, relay: &mut Relay, owners: &[[u8; 32]]) -> Result<usize> {
        let mut changed = 0;
        for o in owners {
            let Some(list) = relay.fetch_device_list(&hex::encode(o)).await? else {
                continue;
            };
            if owner_bytes(&list) == Some(*o) && self.apply(&list) {
                changed += 1;
            }
        }
        if changed > 0 {
            let all: Vec<DeviceList> = self.lists.lock().expect("lock").values().cloned().collect();
            home.save_json(LISTS, &all)?;
        }
        Ok(changed)
    }
}

impl Directory for GroupDir {
    fn device_list(&self, owner: &[u8; 32]) -> Option<DeviceList> {
        self.lists.lock().expect("lock").get(owner).cloned()
    }
    fn missed(&self, owner: &[u8; 32]) {
        self.missed.lock().expect("lock").insert(*owner);
    }
}

fn is_missing_list(e: &MlsError) -> bool {
    matches!(
        e,
        MlsError::Credential(
            CredentialError::UnknownOwner | CredentialError::StaleDirectory { .. }
        )
    )
}

pub(crate) fn at(rec: &Record, base: &Target) -> Target {
    Target {
        url: rec.relay.clone(),
        opts: base.opts.clone(),
    }
}

pub(crate) async fn connect(home: &Home, target: Target) -> Result<Relay> {
    let me = home.load_identity()?;
    Relay::connect(target, &me).await
}

fn my_owner(home: &Home) -> Result<String> {
    Ok(home.load_device_list()?.owner)
}

// --- key packages -------------------------------------------------------------------------------

/// Publish `count` single-use MLS key packages for this device, so people can add it to large groups.
pub async fn publish_key_packages(
    home: &Home,
    relay: impl Into<Target>,
    count: usize,
) -> Result<usize> {
    let mls = load_mls(home)?;
    let packages: Vec<Vec<u8>> = (0..count).map(|_| mls.dev.key_package()).collect();
    let last_resort = mls.dev.last_resort_key_package();
    // The private halves are in the device's state: save before anyone can claim a package.
    save_mls(home, &mls)?;
    let mut r = connect(home, relay.into()).await?;
    // The reusable fallback, for when the one-time ones run out.
    r.publish_last_resort(&last_resort).await?;
    if packages.is_empty() {
        return Ok(0);
    }
    r.publish_key_packages(&packages).await
}

/// One person in a large group: all their devices count as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    /// Owner key, hex.
    pub owner: String,
    pub devices: usize,
    pub admin: bool,
}

/// The people in a group, from its current state.
pub fn members(home: &Home, group: &str) -> Result<Vec<MemberInfo>> {
    let rec = find(home, group)?;
    let mls = load_mls(home)?;
    let g = mls
        .groups
        .get(&rec.id)
        .ok_or_else(|| anyhow!("you are not in '{}'", rec.name))?;
    let mut by_owner: std::collections::BTreeMap<String, usize> = Default::default();
    for m in g.members() {
        *by_owner.entry(hex::encode(m.owner)).or_default() += 1;
    }
    Ok(by_owner
        .into_iter()
        .map(|(owner, devices)| MemberInfo {
            admin: g.policy().admins.contains(&owner),
            owner,
            devices,
        })
        .collect())
}

/// The group's settings as the group itself last stated them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Any member may add people (otherwise only admins).
    pub members_can_add: bool,
    /// How many entries every member refused, over the group's life.
    pub refused_total: u64,
    /// Why the latest one was refused.
    pub last_refused: Option<String>,
}

pub fn settings(home: &Home, group: &str) -> Result<Settings> {
    let rec = find(home, group)?;
    let mls = load_mls(home)?;
    let members_can_add = mls
        .groups
        .get(&rec.id)
        .is_some_and(|g| g.policy().add == spike_mls::policy::Who::Members);
    Ok(Settings {
        members_can_add,
        refused_total: rec.refused_total,
        last_refused: rec.refused.last().map(|r| r.why.clone()),
    })
}

/// Incoming messages after the read marker, per active group.
pub fn unread_counts(home: &Home) -> Result<Vec<(String, usize)>> {
    let store = home.messages()?;
    let mut out = Vec::new();
    for r in list(home)? {
        if r.status != Status::Active {
            continue;
        }
        let marker = home.read_marker(&r.id)?;
        let n = store
            .conversation(&r.id, usize::MAX)?
            .iter()
            .filter(|m| m.direction == Direction::In && !m.deleted && m.ts > marker)
            .count();
        out.push((r.id, n));
    }
    Ok(out)
}

/// Mark a group read up to its newest incoming message on this device.
pub fn mark_read(home: &Home, group: &str) -> Result<()> {
    let rec = find(home, group)?;
    let upto = home
        .messages()?
        .conversation(&rec.id, usize::MAX)?
        .iter()
        .filter(|m| m.direction == Direction::In)
        .map(|m| m.ts)
        .max()
        .unwrap_or(0);
    home.advance_read_marker(&rec.id, upto)?;
    Ok(())
}

/// Keep at least `target` key packages published, so people can add this device. Returns how many
/// the relay holds afterwards.
pub async fn top_up_key_packages(
    home: &Home,
    relay: impl Into<Target>,
    target: usize,
) -> Result<usize> {
    let mls = load_mls(home)?;
    let mut r = connect(home, relay.into()).await?;
    // Publishing one tells us how many the relay holds.
    let one = vec![mls.dev.key_package()];
    save_mls(home, &mls)?;
    let held = r.publish_key_packages(&one).await?;
    if held >= target {
        return Ok(held);
    }
    // Running low: also renew the reusable fallback.
    let more: Vec<Vec<u8>> = (held..target).map(|_| mls.dev.key_package()).collect();
    let last_resort = mls.dev.last_resort_key_package();
    save_mls(home, &mls)?;
    r.publish_last_resort(&last_resort).await?;
    r.publish_key_packages(&more).await
}

// --- creating, adding, removing -----------------------------------------------------------------

pub async fn create(home: &Home, relay: impl Into<Target>, name: &str) -> Result<Record> {
    let base: Target = relay.into();
    if name.trim().is_empty() || name.len() > 100 {
        bail!("a group needs a name of up to 100 characters");
    }
    if list(home)?
        .iter()
        .any(|r| r.name.eq_ignore_ascii_case(name))
    {
        bail!("you already have a large group called '{name}'");
    }
    let mut mls = load_mls(home)?;
    let owner = hex::decode(my_owner(home)?)?
        .try_into()
        .map_err(|_| anyhow!("owner key"))?;
    let mut policy = Policy::new(vec![owner]);
    policy.name = name.trim().to_string();
    let group = Group::create(&mls.dev, policy).map_err(|e| anyhow!("{e}"))?;
    let id = hex::encode(group.id());
    let m = group.mailbox_material();
    let mut r = connect(home, base.clone()).await?;
    r.group_create(&m.mailbox_key(), &m.mailbox_id()).await?;
    mls.groups.insert(id.clone(), group);
    save_mls(home, &mls)?;
    // The group starts out knowing only us; everyone else is announced when they are added.
    let dir = GroupDir::empty(&id);
    dir.apply(&home.load_device_list()?);
    dir.save(home)?;
    let rec = Record {
        id,
        name: name.to_string(),
        relay: base.url,
        status: Status::Active,
        cursor: 0,
        members: 1,
        invite: None,
        refused: vec![],
        refused_total: 0,
        pending_leavers: vec![],
    };
    update_record(home, &rec)?;
    Ok(rec)
}

/// Append a commit at the position after `rec.cursor`. `Ok(None)` means another commit won.
async fn publish_commit(
    r: &mut Relay,
    group: &Group,
    rec: &Record,
    out: &spike_mls::group::Outgoing,
) -> Result<Appended> {
    let m = group.mailbox_material();
    let blob = outer::seal(
        &m,
        &group.id(),
        group.mailbox_generation(),
        OuterKind::Commit,
        &out.commit,
    );
    let next = out.successor.map(|(_, s)| outer::Material::derive(&s));
    let seal = next.as_ref().map(|n| (n.mailbox_id(), n.mailbox_key()));
    r.group_append(
        &m.mailbox_key(),
        &m.mailbox_id(),
        GroupKind::Commit,
        Some(rec.cursor.saturating_add(1)),
        &blob,
        seal.as_ref().map(|(i, k)| (i.as_str(), k)),
    )
    .await
}

fn mls_err(e: MlsError) -> anyhow::Error {
    anyhow!("{e}")
}

/// Build a commit on the group's latest state, send it at the next position, and apply it once the
/// relay accepts it. A commit that loses a race is rebuilt on top of the winner. Returns the group's
/// record, the commit and its position.
async fn commit_with<F>(
    home: &Home,
    base: &Target,
    group: &str,
    announce: &[DeviceList],
    mut build: F,
) -> Result<(Record, spike_mls::group::Outgoing, u64)>
where
    F: FnMut(
        &mut Group,
        &Device,
        &GroupDir,
    ) -> std::result::Result<spike_mls::group::Outgoing, MlsError>,
{
    let mut rec = find(home, group)?;
    if rec.status != Status::Active {
        bail!("you are not in '{}' any more", rec.name);
    }
    let mut mls = load_mls(home)?;
    let dir = GroupDir::for_group(home, &rec.id)?;
    let mut r = connect(home, at(&rec, base)).await?;
    // Lists the group has not heard of yet, which this commit depends on: say them first, in the
    // log, so every member (including one who reads it all later) knows them when it reaches the commit.
    let news: Vec<DeviceList> = announce
        .iter()
        .filter(|l| dir.is_news(l))
        .cloned()
        .collect();
    let mut announced = news.is_empty();
    for l in &news {
        dir.apply(l);
    }
    for _ in 0..RETRIES {
        sync_group(home, &mut r, &mut mls, &dir, &mut rec).await?;
        if rec.status != Status::Active {
            bail!("you are not in '{}' any more", rec.name);
        }
        if !announced {
            let body = serde_json::json!({ "lists": news }).to_string();
            let mut env = Envelope::large_lists(&body)?;
            env.lamport = home.next_lamport()?;
            post_message(home, &mut r, &mut mls, &rec, &env).await?;
            announced = true;
            // Our own message now sits before the position our commit needs: read past it.
            sync_group(home, &mut r, &mut mls, &dir, &mut rec).await?;
        }
        let g = mls
            .groups
            .get_mut(&rec.id)
            .ok_or_else(|| anyhow!("group state is missing"))?;
        if g.has_pending() {
            g.discard(&mls.dev).map_err(mls_err)?;
        }
        let out = build(g, &mls.dev, &dir).map_err(mls_err)?;
        // The pending commit is saved first: if we die after the relay accepts it, the next sync
        // recognises our own commit and confirms it.
        save_mls(home, &mls)?;
        let g = mls.groups.get_mut(&rec.id).expect("present");
        match publish_commit(&mut r, g, &rec, &out).await? {
            Appended::Seq(seq) => {
                g.confirm(&mls.dev).map_err(mls_err)?;
                // A commit that seals the mailbox opens the next one, which starts from its first entry.
                rec.cursor = if out.successor.is_some() { 0 } else { seq };
                rec.members = g.members().len();
                if !g.policy().name.is_empty() {
                    rec.name = g.policy().name.clone();
                }
                save_mls(home, &mls)?;
                dir.save(home)?;
                update_record(home, &rec)?;
                return Ok((rec, out, seq));
            }
            Appended::Conflict { .. } => g.discard(&mls.dev).map_err(mls_err)?,
        }
    }
    bail!(
        "others kept changing '{}' at the same time; try again",
        rec.name
    )
}

/// Add every device of the contact `who` to the group. They are sent an invitation and join only
/// when they accept it.
pub async fn add(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    who: &str,
) -> Result<AddReport> {
    let base: Target = relay.into();
    let rec = find(home, group)?;
    if rec.status != Status::Active {
        bail!("you are not in '{}' any more", rec.name);
    }
    let contact = home
        .load_contacts()?
        .into_iter()
        .find(|c| c.name == who)
        .with_context(|| format!("no contact '{who}'"))?;
    let identity = contact.identity()?;

    // Their key packages come from the relay each device reads from.
    let me = home.load_identity()?;
    let mut pool = ops::Pool {
        base: base.clone(),
        me: &me,
        conns: HashMap::new(),
    };
    let mut packages = Vec::new();
    let mut used_last_resort = false;
    for (card, relay_url) in identity.device_relays() {
        let url = relay_url.unwrap_or_else(|| base.url.clone());
        let r = pool.get(&url).await?;
        match r.claim_key_package(&card.device_id()).await? {
            Some((kp, last_resort)) => {
                used_last_resort |= last_resort;
                packages.push(kp)
            }
            None => bail!(
                "{who}'s device {} has no key packages on {url}; ask them to run `spike large keys`",
                &card.device_id()[..8]
            ),
        }
    }
    let devices = packages.len();
    // The group learns who this person is before it is asked to accept their devices.
    let announce: Vec<DeviceList> = identity.list().cloned().into_iter().collect();
    let (rec, out, seq) = commit_with(home, &base, group, &announce, |g, dev, dir| {
        g.add(dev, dir, &packages)
    })
    .await?;
    invite(
        home,
        &base,
        &rec,
        Some(&contact.name),
        &identity.owner_hex(),
        &out,
        seq,
    )
    .await?;
    Ok(AddReport {
        devices,
        seq,
        used_last_resort,
    })
}

/// Send the Welcome to the person added, as an uploaded file plus a small message naming it.
async fn invite(
    home: &Home,
    base: &Target,
    rec: &Record,
    contact: Option<&str>,
    for_owner: &str,
    out: &spike_mls::group::Outgoing,
    seq: u64,
) -> Result<()> {
    #[derive(Serialize)]
    struct Payload<'a> {
        welcome: &'a str,
        bundle: &'a str,
        /// The signed lists of everyone in the group, as the group knows them.
        lists: Vec<DeviceList>,
    }
    let owners: Vec<[u8; 32]> = members(home, &rec.id)?
        .iter()
        .filter_map(|m| hex::decode(&m.owner).ok()?.try_into().ok())
        .collect();
    let lists = GroupDir::for_group(home, &rec.id)?.lists_of(&owners);
    let payload = serde_json::to_vec(&Payload {
        lists,
        welcome: &hex::encode(out.welcome.as_ref().ok_or_else(|| anyhow!("no welcome"))?),
        bundle: &hex::encode(
            out.join_bundle
                .as_ref()
                .ok_or_else(|| anyhow!("no bundle"))?,
        ),
    })?;
    let att = ops::upload_file(home, &at(rec, base), "welcome", &payload).await?;
    let body = serde_json::json!({ "group": rec.id, "name": rec.name, "relay": rec.relay, "seq": seq, "for": for_owner });
    let env = Envelope::large_welcome(&body.to_string(), att)?;
    match contact {
        Some(contact) => {
            ops::deliver_envelope(home, base.clone(), contact, env)
                .await
                .with_context(|| {
                    format!(
                        "{contact} is in the group now, but the invitation could not be delivered"
                    )
                })?;
        }
        // One of our own devices: no contact, nobody else.
        None => {
            if ops::deliver_to_own(home, base.clone(), env).await? == 0 {
                bail!("the new device is in the group, but the invitation could not be delivered");
            }
        }
    }
    Ok(())
}

/// Remove every device of the contact `who`. The group moves to a new mailbox they cannot reach.
pub async fn remove(home: &Home, relay: impl Into<Target>, group: &str, who: &str) -> Result<()> {
    let contact = home
        .load_contacts()?
        .into_iter()
        .find(|c| c.name == who)
        .with_context(|| format!("no contact '{who}'"))?;
    let owner: [u8; 32] = hex::decode(contact.identity()?.owner_hex())?
        .try_into()
        .map_err(|_| anyhow!("owner key"))?;
    remove_owner(home, relay, group, &owner, who).await
}

/// Remove every device of the person with this owner key (who need not be a contact).
pub async fn remove_owner(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    owner: &[u8; 32],
    who: &str,
) -> Result<()> {
    let base: Target = relay.into();
    commit_with(home, &base, group, &[], |g, dev, _| {
        let devices: Vec<[u8; 16]> = g
            .members()
            .iter()
            .filter(|m| m.owner == *owner)
            .map(|m| m.device)
            .collect();
        g.remove(dev, &devices)
    })
    .await
    .map_err(|e| {
        if e.to_string().contains("no such device") {
            anyhow!("{who} is not in the group")
        } else {
            e
        }
    })?;
    Ok(())
}

/// Change the group's policy (its name, admins, who may add). Admins only.
async fn change_policy(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    change: impl Fn(&mut Policy),
) -> Result<()> {
    let base: Target = relay.into();
    commit_with(home, &base, group, &[], |g, dev, _| {
        let mut p = g.policy().clone();
        change(&mut p);
        g.set_policy(dev, p)
    })
    .await?;
    Ok(())
}

/// Rename the group, for everyone. Admins only.
pub async fn rename(home: &Home, relay: impl Into<Target>, group: &str, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.len() > spike_mls::policy::MAX_NAME {
        bail!(
            "a group needs a name of up to {} characters",
            spike_mls::policy::MAX_NAME
        );
    }
    let rec = find(home, group)?;
    if list(home)?
        .iter()
        .any(|r| r.id != rec.id && r.name.eq_ignore_ascii_case(name))
    {
        bail!("you already have a large group called '{name}'");
    }
    change_policy(home, relay, group, |p| p.name = name.to_string()).await
}

/// Make a member (by owner key) an admin. Admins only.
pub async fn make_admin(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    owner: &str,
) -> Result<()> {
    if !members(home, group)?.iter().any(|m| m.owner == owner) {
        bail!("that person is not in the group");
    }
    change_policy(home, relay, group, |p| {
        if !p.admins.iter().any(|a| a == owner) {
            p.admins.push(owner.to_string());
        }
    })
    .await
}

/// Let any member add people (true), or only admins (false). Admins only.
pub async fn set_members_can_add(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    any: bool,
) -> Result<()> {
    use spike_mls::policy::Who;
    change_policy(home, relay, group, |p| {
        p.add = if any { Who::Members } else { Who::Admins }
    })
    .await
}

// --- invitations --------------------------------------------------------------------------------

/// What a Welcome message says about itself.
#[derive(Deserialize)]
struct WelcomeBody {
    group: String,
    name: String,
    relay: String,
    seq: u64,
    /// The owner key of the person it is for.
    #[serde(rename = "for", default)]
    for_owner: String,
}

fn parse_welcome(env: &Envelope) -> Option<(WelcomeBody, Attachment)> {
    let b: WelcomeBody = serde_json::from_str(&env.body).ok()?;
    let valid_id = b.group.len() == 32 && b.group.bytes().all(|c| c.is_ascii_hexdigit());
    let welcome = env.attachment.clone()?;
    (valid_id && !b.name.trim().is_empty() && b.name.len() <= 100).then_some((b, welcome))
}

/// A Welcome arrived from a contact. It is kept as an invitation; nothing is joined or read.
pub fn on_welcome(home: &Home, from: &str, env: &Envelope) -> Result<crate::Outcome> {
    let Some((b, welcome)) = parse_welcome(env) else {
        return Ok(crate::Outcome::Invalid);
    };
    // Not addressed to this person: a contact cannot invite us on someone else's behalf.
    if b.for_owner != my_owner(home)? {
        return Ok(crate::Outcome::Invalid);
    }
    if list(home)?.iter().any(|r| r.id == b.group) {
        return Ok(crate::Outcome::Duplicate);
    }
    update_record(
        home,
        &Record {
            id: b.group,
            name: b.name.clone(),
            relay: b.relay,
            status: Status::Invited,
            cursor: b.seq,
            members: 0,
            invite: Some(Invite {
                from: from.to_string(),
                welcome,
                seq: b.seq,
                auto: None,
            }),
            refused: vec![],
            refused_total: 0,
            pending_leavers: vec![],
        },
    )?;
    Ok(crate::Outcome::LargeInvite {
        group: b.name,
        from: from.to_string(),
    })
}

/// A Welcome from another of our own devices. If it is for us (that device added this one to a group
/// we are in), it is accepted on the next sync without asking: it is the same person. A copy of an
/// invitation we sent to a contact is not for us and is ignored.
pub fn on_own_welcome(home: &Home, env: &Envelope) -> Result<crate::Outcome> {
    let Some((b, welcome)) = parse_welcome(env) else {
        return Ok(crate::Outcome::Invalid);
    };
    if b.for_owner != my_owner(home)? || list(home)?.iter().any(|r| r.id == b.group) {
        return Ok(crate::Outcome::Ignored);
    }
    update_record(
        home,
        &Record {
            id: b.group,
            name: b.name,
            relay: b.relay,
            status: Status::Invited,
            cursor: b.seq,
            members: 0,
            invite: Some(Invite {
                from: "your other device".into(),
                welcome,
                seq: b.seq,
                auto: Some(true),
            }),
            refused: vec![],
            refused_total: 0,
            pending_leavers: vec![],
        },
    )?;
    Ok(crate::Outcome::Ignored)
}

/// Another of our devices accepted or declined an invitation we also hold: do the same on the next sync.
pub fn on_own_decision(home: &Home, group: &str, accepted: bool) -> Result<crate::Outcome> {
    if let Ok(mut rec) = find(home, group) {
        if let (Status::Invited, Some(inv)) = (rec.status, rec.invite.as_mut()) {
            inv.auto = Some(accepted);
            update_record(home, &rec)?;
        }
    }
    Ok(crate::Outcome::Ignored)
}

/// Act on invitations that were settled without us: our own devices' Welcomes, and decisions made
/// on another device.
async fn settle_invites(home: &Home, base: &Target) {
    for rec in list(home).unwrap_or_default() {
        match (rec.status, rec.invite.as_ref().and_then(|i| i.auto)) {
            (Status::Invited, Some(true)) => {
                if let Err(e) = accept_inner(home, base, &rec.id, false).await {
                    eprintln!("note: could not join '{}' yet: {e:#}", rec.name);
                }
            }
            (Status::Invited, Some(false)) => {
                let all: Vec<Record> = list(home)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| r.id != rec.id)
                    .collect();
                let _ = save_records(home, &all);
            }
            _ => {}
        }
    }
}

/// Join a group we were invited to.
pub async fn accept(home: &Home, relay: impl Into<Target>, group: &str) -> Result<Record> {
    let base: Target = relay.into();
    accept_inner(home, &base, group, true).await
}

async fn accept_inner(home: &Home, base: &Target, group: &str, tell_own: bool) -> Result<Record> {
    let base = base.clone();
    let mut rec = find(home, group)?;
    let inv = match (&rec.status, &rec.invite) {
        (Status::Invited, Some(i)) => i.clone(),
        _ => bail!("'{}' is not an open invitation", rec.name),
    };
    #[derive(Deserialize)]
    struct Payload {
        welcome: String,
        bundle: String,
        #[serde(default)]
        lists: Vec<DeviceList>,
    }
    let bytes = ops::download_file(home, &at(&rec, &base), &inv.welcome).await?;
    let p: Payload = serde_json::from_slice(&bytes).context("the invitation is damaged")?;
    let (welcome, bundle) = (hex::decode(p.welcome)?, hex::decode(p.bundle)?);

    let mut mls = load_mls(home)?;
    // What the inviter's group knew, on top of what we know ourselves.
    let dir = GroupDir::seed(home, &rec.id, &p.lists)?;
    let mut r = connect(home, at(&rec, &base)).await?;
    let mut joined = None;
    for _ in 0..3 {
        match Group::join(&mls.dev, &dir, &welcome, &bundle) {
            Ok(g) => {
                joined = Some(g);
                break;
            }
            // Members who are not our contacts: their lists come from the relay.
            Err(e) if is_missing_list(&e) => {
                let missed = dir.take_missed();
                if dir.refresh(home, &mut r, &missed).await? == 0 {
                    return Err(mls_err(e)).context("cannot check who is in this group");
                }
            }
            Err(e) => return Err(mls_err(e)).context("this invitation was refused"),
        }
    }
    let g = joined.ok_or_else(|| anyhow!("cannot check who is in this group"))?;
    rec.members = g.members().len();
    if !g.policy().name.is_empty() {
        rec.name = g.policy().name.clone();
    }
    rec.status = Status::Active;
    rec.cursor = inv.seq;
    rec.invite = None;
    mls.groups.insert(rec.id.clone(), g);
    save_mls(home, &mls)?;
    dir.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    dir.save(home)?;
    update_record(home, &rec)?;
    if tell_own {
        // The person has decided: their other devices holding this invitation join too.
        let msg = crate::sync::SyncMsg::LargeInvite {
            group: rec.id.clone(),
            accepted: true,
        };
        let _ = ops::broadcast_own(home, base, &msg).await;
    }
    Ok(rec)
}

pub async fn decline(home: &Home, relay: impl Into<Target>, group: &str) -> Result<()> {
    let rec = find(home, group)?;
    if rec.status != Status::Invited {
        bail!("'{}' is not an open invitation", rec.name);
    }
    let all: Vec<Record> = list(home)?.into_iter().filter(|r| r.id != rec.id).collect();
    save_records(home, &all)?;
    // Your other devices holding the same invitation turn it down too (best effort).
    let msg = crate::sync::SyncMsg::LargeInvite {
        group: rec.id,
        accepted: false,
    };
    let _ = ops::broadcast_own(home, relay.into(), &msg).await;
    Ok(())
}

// --- messages -----------------------------------------------------------------------------------

pub async fn send(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    text: &str,
) -> Result<String> {
    let base: Target = relay.into();
    let rec = find(home, group)?;
    if rec.status != Status::Active {
        bail!("you are not in '{}'", rec.name);
    }
    let mut env = Envelope::text(text)?;
    env.lamport = home.next_lamport()?;
    send_envelope(home, &base, &rec, &env).await?;
    let me = my_owner(home)?;
    let device = load_mls(home)?.dev.device_id();
    store(home, &rec.id, &env, &me, &device, Direction::Out)?;
    Ok(env.id)
}

/// Encrypt one envelope for the group and append it to the mailbox.
async fn send_envelope(home: &Home, base: &Target, rec: &Record, env: &Envelope) -> Result<()> {
    let mut mls = load_mls(home)?;
    let mut r = connect(home, at(rec, base)).await?;
    post_message(home, &mut r, &mut mls, rec, env).await
}

/// Encrypt one envelope with the state we are already holding, and append it. (Using a second copy
/// of the state would reuse message keys.)
async fn post_message(
    home: &Home,
    r: &mut Relay,
    mls: &mut Mls,
    rec: &Record,
    env: &Envelope,
) -> Result<()> {
    let g = mls
        .groups
        .get_mut(&rec.id)
        .ok_or_else(|| anyhow!("group state is missing"))?;
    let wire = g.encrypt(&mls.dev, &env.to_bytes()).map_err(mls_err)?;
    let m = g.mailbox_material();
    let blob = outer::seal(
        &m,
        &g.id(),
        g.mailbox_generation(),
        OuterKind::Message,
        &wire,
    );
    // The ratchet has moved: save before sending, so a crash can never reuse a message key.
    save_mls(home, mls)?;
    r.group_append(
        &m.mailbox_key(),
        &m.mailbox_id(),
        GroupKind::Message,
        None,
        &blob,
        None,
    )
    .await?;
    Ok(())
}

/// Leave a group. Tells the group, and stops reading or sending for it here at once; an admin
/// removes you from the group's keys on their next sync (until then you are still a member).
/// The only admin cannot leave: nobody would be left to remove people or add them.
pub async fn leave(home: &Home, relay: impl Into<Target>, group: &str) -> Result<()> {
    let base: Target = relay.into();
    let mut rec = find(home, group)?;
    if rec.status != Status::Active {
        bail!("you are not in '{}'", rec.name);
    }
    let me = my_owner(home)?;
    let infos = members(home, group)?;
    let i_am_admin = infos.iter().any(|m| m.owner == me && m.admin);
    if i_am_admin && infos.iter().filter(|m| m.admin).count() == 1 {
        bail!(
            "you are the only admin of '{}'; make someone else an admin before you leave",
            rec.name
        );
    }
    let mut env = Envelope::large_leave()?;
    env.lamport = home.next_lamport()?;
    send_envelope(home, &base, &rec, &env).await?;
    rec.status = Status::Left;
    update_record(home, &rec)
}

fn store(
    home: &Home,
    gid: &str,
    env: &Envelope,
    author: &str,
    device: &[u8; 16],
    direction: Direction,
) -> Result<bool> {
    home.messages()?.insert(&StoredMessage {
        id: env.id.clone(),
        peer: gid.to_string(),
        direction,
        ts: env.ts,
        body: env.body.clone(),
        attachment: None,
        lamport: env.lamport,
        stamp_device: hex::encode(device),
        edited: false,
        deleted: false,
        changed_at: 0,
        author: Some(author.to_string()),
        shared_by: None,
    })
}

pub fn history(home: &Home, group: &str, limit: usize) -> Result<(Record, Vec<StoredMessage>)> {
    let rec = find(home, group)?;
    let msgs = home.messages()?.conversation(&rec.id, limit)?;
    Ok((rec, msgs))
}

// --- reading the mailbox ------------------------------------------------------------------------

/// The mailboxes of the groups we are in on `relay_url`, with the keys that open them: what a
/// listener subscribes to so the relay can push new entries (instant delivery). Only a hint that
/// something is waiting; reading and applying it is `sync`.
pub fn subscriptions(
    home: &Home,
    relay_url: &str,
) -> Result<Vec<(String, spike_core::mailbox::MailboxKey)>> {
    let mls = load_mls(home)?;
    let mut out = Vec::new();
    for rec in list(home)? {
        if rec.status != Status::Active || rec.relay != relay_url {
            continue;
        }
        if let Some(g) = mls.groups.get(&rec.id) {
            let m = g.mailbox_material();
            out.push((m.mailbox_id(), m.mailbox_key()));
        }
    }
    Ok(out)
}

/// Read and apply everything new in every group we are in.
pub async fn sync(home: &Home, relay: impl Into<Target>) -> Result<Vec<Event>> {
    let base: Target = relay.into();
    let mut events = Vec::new();
    settle_invites(home, &base).await;
    let mut mls = load_mls(home)?;
    for mut rec in list(home)? {
        if rec.status != Status::Active || !mls.groups.contains_key(&rec.id) {
            continue;
        }
        let dir = GroupDir::for_group(home, &rec.id)?;
        let mut r = connect(home, at(&rec, &base)).await?;
        events.extend(sync_group(home, &mut r, &mut mls, &dir, &mut rec).await?);
        // The relay connection and the state are done with; removing people reloads both.
        if !rec.pending_leavers.is_empty() {
            drop(r);
            remove_leavers(home, &base, &rec.id).await;
            mls = load_mls(home)?;
        }
    }
    // Our own devices that are in our device list but not in a group get added, and ones that were
    // revoked get removed. Any of our devices that is in the group can do it.
    let mut changed = false;
    for rec in list(home)? {
        if rec.status == Status::Active && ensure_own_devices(home, &base, &rec.id).await {
            changed = true;
        }
    }
    if changed {
        events.push(Event::OwnDevices);
    }
    Ok(events)
}

/// Bring the group's leaves for our own devices in line with our signed device list. Returns
/// whether anything changed. Failures are not errors: another of our devices may have done it
/// first, or a new device may not have published key packages yet; the next sync tries again.
async fn ensure_own_devices(home: &Home, base: &Target, group: &str) -> bool {
    let Ok(card) = home.identity_card() else {
        return false;
    };
    let Ok(identity) = spike_core::owner::IdentityCard::decode(&card) else {
        return false;
    };
    let Ok(me) = my_owner(home) else {
        return false;
    };
    let Ok(mls) = load_mls(home) else {
        return false;
    };
    let Ok(rec) = find(home, group) else {
        return false;
    };
    let Some(g) = mls.groups.get(&rec.id) else {
        return false;
    };
    let in_group: Vec<[u8; 16]> = g
        .members()
        .iter()
        .filter(|m| hex::encode(m.owner) == me)
        .map(|m| m.device)
        .collect();
    let listed: Vec<([u8; 16], Option<String>)> = identity
        .device_relays()
        .into_iter()
        .filter_map(|(d, r)| Some((hex::decode(d.device_id()).ok()?.try_into().ok()?, r)))
        .collect();
    let missing: Vec<_> = listed
        .iter()
        .filter(|(d, _)| !in_group.contains(d))
        .cloned()
        .collect();
    let stale: Vec<[u8; 16]> = in_group
        .iter()
        .filter(|d| !listed.iter().any(|(l, _)| l == *d))
        .copied()
        .collect();
    let mut changed = false;
    // The group learns our newest list before it is asked to accept a commit that depends on it.
    let own_lists: Vec<DeviceList> = home.load_device_list().ok().into_iter().collect();

    if !missing.is_empty() {
        // One key package per missing device, from the relay it reads from; those with none yet wait.
        let Ok(id) = home.load_identity() else {
            return false;
        };
        let mut pool = ops::Pool {
            base: base.clone(),
            me: &id,
            conns: HashMap::new(),
        };
        let mut packages = Vec::new();
        for (device, relay) in &missing {
            let url = relay.clone().unwrap_or_else(|| base.url.clone());
            if let Ok(r) = pool.get(&url).await {
                if let Ok(Some((kp, _))) = r.claim_key_package(&hex::encode(device)).await {
                    packages.push(kp);
                }
            }
        }
        if !packages.is_empty() {
            let committed = commit_with(home, base, group, &own_lists, |g, dev, dir| {
                g.add(dev, dir, &packages)
            })
            .await;
            if let Ok((rec, out, seq)) = committed {
                match invite(home, base, &rec, None, &me, &out, seq).await {
                    Ok(()) => changed = true,
                    Err(e) => eprintln!("note: {e:#}"),
                }
            }
        }
    }
    if !stale.is_empty() {
        let removed = commit_with(home, base, group, &own_lists, |g, dev, _| {
            let still: Vec<[u8; 16]> = g
                .members()
                .iter()
                .filter(|m| stale.contains(&m.device))
                .map(|m| m.device)
                .collect();
            g.remove(dev, &still)
        })
        .await;
        changed |= removed.is_ok();
    }
    changed
}

/// As an admin, remove the people who said they are leaving. A failure (another admin got there
/// first, we are not an admin after all) is not an error: the notice is dropped once they are gone
/// or once it is not ours to act on.
async fn remove_leavers(home: &Home, base: &Target, group_id: &str) {
    let Ok(rec) = find(home, group_id) else {
        return;
    };
    let me = my_owner(home).unwrap_or_default();
    for owner in rec.pending_leavers.clone() {
        let i_am_admin = members(home, group_id)
            .map(|ms| ms.iter().any(|m| m.owner == me && m.admin))
            .unwrap_or(false);
        let still_in = members(home, group_id)
            .map(|ms| ms.iter().any(|m| m.owner == owner))
            .unwrap_or(false);
        if still_in && i_am_admin {
            if let Ok(bytes) = hex::decode(&owner) {
                if let Ok(key) = <[u8; 32]>::try_from(bytes) {
                    let name = groups::display_name(home, &owner);
                    let _ = remove_owner(home, base.clone(), group_id, &key, &name).await;
                }
            }
        }
        // Done, or not ours: either way stop carrying it.
        if let Ok(mut rec) = find(home, group_id) {
            let gone = members(home, group_id)
                .map(|ms| !ms.iter().any(|m| m.owner == owner))
                .unwrap_or(true);
            if gone || !i_am_admin {
                rec.pending_leavers.retain(|o| *o != owner);
                let _ = update_record(home, &rec);
            }
        }
    }
}

fn kind_of(k: GroupKind) -> OuterKind {
    match k {
        GroupKind::Commit => OuterKind::Commit,
        GroupKind::Message => OuterKind::Message,
    }
}

/// Process one group's mailbox from its cursor. State is saved after each batch that changed it,
/// and messages are stored before the state that read them is saved.
async fn sync_group(
    home: &Home,
    r: &mut Relay,
    mls: &mut Mls,
    dir: &GroupDir,
    rec: &mut Record,
) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    let me = my_owner(home)?;
    loop {
        let g = mls
            .groups
            .get_mut(&rec.id)
            .ok_or_else(|| anyhow!("group state is missing"))?;
        let m = g.mailbox_material();
        let (gid, generation) = (g.id(), g.mailbox_generation());
        let page = r
            .group_read(&m.mailbox_key(), &m.mailbox_id(), rec.cursor, 100)
            .await?;
        if page.entries.is_empty() {
            return Ok(events);
        }
        let mut moved = false;
        for e in page.entries {
            // Positions only move forward. A relay that repeats or rewinds them (or sends the end of the
            // range) would otherwise make this loop spin forever or leave the cursor where no commit fits.
            if e.seq <= rec.cursor {
                bail!("the relay sent mailbox entries out of order");
            }
            let Ok(inner) = outer::open(&m, &gid, generation, kind_of(e.kind), &e.blob) else {
                refuse(
                    &mut events,
                    rec,
                    e.seq,
                    "not sealed with this group's key".into(),
                );
                rec.cursor = e.seq;
                continue;
            };
            let res = g.process(&mls.dev, dir, &inner);
            rec.cursor = e.seq;
            match res {
                Ok(Processed::Message(body, from)) => {
                    let Ok(env) = Envelope::from_bytes(&body) else {
                        refuse(
                            &mut events,
                            rec,
                            e.seq,
                            "a message that is not one of ours".into(),
                        );
                        continue;
                    };
                    let owner = hex::encode(from.owner);
                    if env.kind == spike_core::envelope::Kind::LargeLists {
                        // Signed lists the group should know. Taken in log order, so that every
                        // member judges every later commit by the same lists.
                        #[derive(Deserialize)]
                        struct Lists {
                            lists: Vec<DeviceList>,
                        }
                        if let Ok(l) = serde_json::from_str::<Lists>(&env.body) {
                            for list in l.lists.iter().take(64) {
                                dir.apply(list);
                            }
                        }
                        continue;
                    }
                    if env.kind == spike_core::envelope::Kind::LargeLeave {
                        // A notice, not a message. Only the sender's own place can be asked for.
                        let i_am_admin = hex::decode(&me)
                            .ok()
                            .and_then(|b| <[u8; 32]>::try_from(b).ok())
                            .is_some_and(|k| g.policy().is_admin(&k));
                        if owner != me && i_am_admin && !rec.pending_leavers.contains(&owner) {
                            rec.pending_leavers.push(owner.clone());
                        }
                        if owner != me {
                            events.push(Event::Left {
                                group: rec.name.clone(),
                                who: groups::display_name(home, &owner),
                            });
                        }
                        continue;
                    }
                    let direction = if owner == me {
                        Direction::Out
                    } else {
                        Direction::In
                    };
                    if store(home, &rec.id, &env, &owner, &from.device, direction)?
                        && direction == Direction::In
                    {
                        events.push(Event::Message {
                            group: rec.name.clone(),
                            from: groups::display_name(home, &owner),
                            body: env.body,
                            id: env.id,
                        });
                    }
                }
                Ok(Processed::Own) => {
                    // Our own commit coming back: if we died before confirming it, do it now.
                    if e.kind == GroupKind::Commit && g.has_pending() {
                        g.confirm(&mls.dev).map_err(mls_err)?;
                        rec.members = g.members().len();
                        moved = true;
                    }
                }
                Ok(Processed::Commit {
                    removed_me,
                    rotated,
                    ..
                }) => {
                    moved = true;
                    rec.members = g.members().len();
                    if !removed_me && !g.policy().name.is_empty() {
                        rec.name = g.policy().name.clone();
                    }
                    if removed_me {
                        rec.status = Status::Removed;
                        events.push(Event::Removed {
                            group: rec.name.clone(),
                        });
                        save_mls(home, mls)?;
                        update_record(home, rec)?;
                        return Ok(events);
                    }
                    events.push(Event::Changed {
                        group: rec.name.clone(),
                        members: rec.members,
                        rotated,
                    });
                    if rotated {
                        // The next mailbox starts at its first position.
                        rec.cursor = 0;
                        break;
                    }
                }
                Err(err) => refuse(&mut events, rec, e.seq, err.to_string()),
            }
        }
        if moved || rec.cursor > 0 {
            save_mls(home, mls)?;
            dir.save(home)?;
            update_record(home, rec)?;
        }
    }
}
