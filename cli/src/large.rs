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
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Invite {
    /// Contact name of whoever invited us.
    pub from: String,
    /// The Welcome and join bundle, as an uploaded file.
    pub welcome: Attachment,
    /// The relay position of the commit that added us.
    pub seq: u64,
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
struct Mls {
    dev: Device,
    groups: HashMap<String, Group>,
}

fn load_mls(home: &Home) -> Result<Mls> {
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

fn save_mls(home: &Home, mls: &Mls) -> Result<()> {
    let groups: Vec<&Group> = mls.groups.values().collect();
    let bytes = mls_state::export(&mls.dev, &groups).map_err(|e| anyhow!("{e}"))?;
    home.save_bytes(STATE, &bytes)
}

// --- who is who ---------------------------------------------------------------------------------

/// Device lists for checking credentials: our contacts', our own, and any fetched from relays.
struct HomeDir {
    lists: Mutex<HashMap<[u8; 32], DeviceList>>,
    missed: Mutex<BTreeSet<[u8; 32]>>,
}

fn owner_bytes(list: &DeviceList) -> Option<[u8; 32]> {
    hex::decode(&list.owner).ok()?.try_into().ok()
}

impl HomeDir {
    fn load(home: &Home) -> Result<Self> {
        let mut lists = HashMap::new();
        let mut put = |l: &DeviceList| {
            if let Some(o) = owner_bytes(l) {
                let newer = lists
                    .get(&o)
                    .is_none_or(|have: &DeviceList| have.version < l.version);
                if newer {
                    lists.insert(o, l.clone());
                }
            }
        };
        for c in home.load_contacts()? {
            if let Some(l) = c.identity()?.list() {
                put(l);
            }
        }
        put(&home.load_device_list()?);
        for l in home.load_json::<Vec<DeviceList>>(LISTS)? {
            put(&l);
        }
        Ok(Self {
            lists: Mutex::new(lists),
            missed: Mutex::new(BTreeSet::new()),
        })
    }

    fn take_missed(&self) -> Vec<[u8; 32]> {
        std::mem::take(&mut *self.missed.lock().expect("lock"))
            .into_iter()
            .collect()
    }

    /// Fetch these owners' lists from the relay, keep the valid newer ones. Returns how many changed.
    async fn refresh(&self, home: &Home, relay: &mut Relay, owners: &[[u8; 32]]) -> Result<usize> {
        let mut changed = 0;
        for o in owners {
            let Some(list) = relay.fetch_device_list(&hex::encode(o)).await? else {
                continue;
            };
            if list.verify().is_err() || owner_bytes(&list) != Some(*o) {
                continue;
            }
            let newer = self
                .lists
                .lock()
                .expect("lock")
                .get(o)
                .is_none_or(|have| have.version < list.version);
            if newer {
                self.lists.lock().expect("lock").insert(*o, list);
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

impl Directory for HomeDir {
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

fn at(rec: &Record, base: &Target) -> Target {
    Target {
        url: rec.relay.clone(),
        opts: base.opts.clone(),
    }
}

async fn connect(home: &Home, target: Target) -> Result<Relay> {
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
    // The private halves are in the device's state: save before anyone can claim a package.
    save_mls(home, &mls)?;
    let mut r = connect(home, relay.into()).await?;
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
    let more: Vec<Vec<u8>> = (held..target).map(|_| mls.dev.key_package()).collect();
    save_mls(home, &mls)?;
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
    let group = Group::create(&mls.dev, Policy::new(vec![owner])).map_err(|e| anyhow!("{e}"))?;
    let id = hex::encode(group.id());
    let m = group.mailbox_material();
    let mut r = connect(home, base.clone()).await?;
    r.group_create(&m.mailbox_key(), &m.mailbox_id()).await?;
    mls.groups.insert(id.clone(), group);
    save_mls(home, &mls)?;
    let rec = Record {
        id,
        name: name.to_string(),
        relay: base.url,
        status: Status::Active,
        cursor: 0,
        members: 1,
        invite: None,
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
        Some(rec.cursor + 1),
        &blob,
        seal.as_ref().map(|(i, k)| (i.as_str(), k)),
    )
    .await
}

fn mls_err(e: MlsError) -> anyhow::Error {
    anyhow!("{e}")
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
    let mut rec = find(home, group)?;
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
    for (card, relay_url) in identity.device_relays() {
        let url = relay_url.unwrap_or_else(|| base.url.clone());
        let r = pool.get(&url).await?;
        match r.claim_key_package(&card.device_id()).await? {
            Some((kp, _last_resort)) => packages.push(kp),
            None => bail!(
                "{who}'s device {} has no key packages on {url}; ask them to run `spike large keys`",
                &card.device_id()[..8]
            ),
        }
    }
    let devices = packages.len();

    let mut mls = load_mls(home)?;
    let dir = HomeDir::load(home)?;
    let target = at(&rec, &base);
    let mut r = connect(home, target.clone()).await?;
    for _ in 0..RETRIES {
        sync_group(home, &mut r, &mut mls, &dir, &mut rec).await?;
        let g = mls
            .groups
            .get_mut(&rec.id)
            .ok_or_else(|| anyhow!("group state is missing"))?;
        if g.has_pending() {
            g.discard(&mls.dev).map_err(mls_err)?;
        }
        let out = match g.add(&mls.dev, &dir, &packages) {
            Err(e) if is_missing_list(&e) => {
                let missed = dir.take_missed();
                dir.refresh(home, &mut r, &missed).await?;
                continue;
            }
            other => other.map_err(mls_err)?,
        };
        // The pending commit is saved first: if we die after the relay accepts it, the next sync
        // recognises our own commit and confirms it.
        save_mls(home, &mls)?;
        let g = mls.groups.get_mut(&rec.id).expect("present");
        match publish_commit(&mut r, g, &rec, &out).await? {
            Appended::Seq(seq) => {
                g.confirm(&mls.dev).map_err(mls_err)?;
                rec.cursor = seq;
                rec.members = g.members().len();
                save_mls(home, &mls)?;
                update_record(home, &rec)?;
                invite(home, &base, &rec, &contact.name, &out, seq).await?;
                return Ok(AddReport { devices, seq });
            }
            Appended::Conflict { .. } => {
                g.discard(&mls.dev).map_err(mls_err)?;
            }
        }
    }
    bail!(
        "others kept changing '{}' at the same time; try again",
        rec.name
    )
}

/// Send the Welcome to the person added, as an uploaded file plus a small message naming it.
async fn invite(
    home: &Home,
    base: &Target,
    rec: &Record,
    contact: &str,
    out: &spike_mls::group::Outgoing,
    seq: u64,
) -> Result<()> {
    #[derive(Serialize)]
    struct Payload<'a> {
        welcome: &'a str,
        bundle: &'a str,
    }
    let payload = serde_json::to_vec(&Payload {
        welcome: &hex::encode(out.welcome.as_ref().ok_or_else(|| anyhow!("no welcome"))?),
        bundle: &hex::encode(
            out.join_bundle
                .as_ref()
                .ok_or_else(|| anyhow!("no bundle"))?,
        ),
    })?;
    let att = ops::upload_file(home, &at(rec, base), "welcome", &payload).await?;
    let body =
        serde_json::json!({ "group": rec.id, "name": rec.name, "relay": rec.relay, "seq": seq });
    let env = Envelope::large_welcome(&body.to_string(), att)?;
    ops::deliver_envelope(home, base.clone(), contact, env)
        .await
        .with_context(|| {
            format!("{contact} is in the group now, but the invitation could not be delivered")
        })?;
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
    let mut rec = find(home, group)?;
    if rec.status != Status::Active {
        bail!("you are not in '{}' any more", rec.name);
    }
    let mut mls = load_mls(home)?;
    let dir = HomeDir::load(home)?;
    let mut r = connect(home, at(&rec, &base)).await?;
    for _ in 0..RETRIES {
        sync_group(home, &mut r, &mut mls, &dir, &mut rec).await?;
        let g = mls
            .groups
            .get_mut(&rec.id)
            .ok_or_else(|| anyhow!("group state is missing"))?;
        if g.has_pending() {
            g.discard(&mls.dev).map_err(mls_err)?;
        }
        let devices: Vec<[u8; 16]> = g
            .members()
            .iter()
            .filter(|m| m.owner == *owner)
            .map(|m| m.device)
            .collect();
        if devices.is_empty() {
            bail!("{who} is not in '{}'", rec.name);
        }
        let out = g.remove(&mls.dev, &devices).map_err(mls_err)?;
        save_mls(home, &mls)?;
        let g = mls.groups.get_mut(&rec.id).expect("present");
        match publish_commit(&mut r, g, &rec, &out).await? {
            Appended::Seq(_) => {
                g.confirm(&mls.dev).map_err(mls_err)?;
                // The sealed mailbox is closed; the group continues in the next one, from its start.
                rec.cursor = 0;
                rec.members = g.members().len();
                save_mls(home, &mls)?;
                update_record(home, &rec)?;
                return Ok(());
            }
            Appended::Conflict { .. } => g.discard(&mls.dev).map_err(mls_err)?,
        }
    }
    bail!(
        "others kept changing '{}' at the same time; try again",
        rec.name
    )
}

// --- invitations --------------------------------------------------------------------------------

/// A Welcome arrived from a contact. It is kept as an invitation; nothing is joined or read.
pub fn on_welcome(home: &Home, from: &str, env: &Envelope) -> Result<crate::Outcome> {
    #[derive(Deserialize)]
    struct Body {
        group: String,
        name: String,
        relay: String,
        seq: u64,
    }
    let Ok(b) = serde_json::from_str::<Body>(&env.body) else {
        return Ok(crate::Outcome::Invalid);
    };
    let valid_id = b.group.len() == 32 && b.group.bytes().all(|c| c.is_ascii_hexdigit());
    let welcome = env.attachment.clone();
    let (true, Some(welcome)) = (
        valid_id && !b.name.trim().is_empty() && b.name.len() <= 100,
        welcome,
    ) else {
        return Ok(crate::Outcome::Invalid);
    };
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
            }),
        },
    )?;
    Ok(crate::Outcome::LargeInvite {
        group: b.name,
        from: from.to_string(),
    })
}

/// Join a group we were invited to.
pub async fn accept(home: &Home, relay: impl Into<Target>, group: &str) -> Result<Record> {
    let base: Target = relay.into();
    let mut rec = find(home, group)?;
    let inv = match (&rec.status, &rec.invite) {
        (Status::Invited, Some(i)) => i.clone(),
        _ => bail!("'{}' is not an open invitation", rec.name),
    };
    #[derive(Deserialize)]
    struct Payload {
        welcome: String,
        bundle: String,
    }
    let bytes = ops::download_file(home, &at(&rec, &base), &inv.welcome).await?;
    let p: Payload = serde_json::from_slice(&bytes).context("the invitation is damaged")?;
    let (welcome, bundle) = (hex::decode(p.welcome)?, hex::decode(p.bundle)?);

    let mut mls = load_mls(home)?;
    let dir = HomeDir::load(home)?;
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
    rec.status = Status::Active;
    rec.cursor = inv.seq;
    rec.invite = None;
    mls.groups.insert(rec.id.clone(), g);
    save_mls(home, &mls)?;
    update_record(home, &rec)?;
    Ok(rec)
}

pub fn decline(home: &Home, group: &str) -> Result<()> {
    let rec = find(home, group)?;
    if rec.status != Status::Invited {
        bail!("'{}' is not an open invitation", rec.name);
    }
    let all: Vec<Record> = list(home)?.into_iter().filter(|r| r.id != rec.id).collect();
    save_records(home, &all)
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
    let mut mls = load_mls(home)?;
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
    save_mls(home, &mls)?;
    let mut r = connect(home, at(&rec, &base)).await?;
    r.group_append(
        &m.mailbox_key(),
        &m.mailbox_id(),
        GroupKind::Message,
        None,
        &blob,
        None,
    )
    .await?;
    let me = my_owner(home)?;
    store(
        home,
        &rec.id,
        &env,
        &me,
        &mls.dev.device_id(),
        Direction::Out,
    )?;
    Ok(env.id)
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

/// Read and apply everything new in every group we are in.
pub async fn sync(home: &Home, relay: impl Into<Target>) -> Result<Vec<Event>> {
    let base: Target = relay.into();
    let mut events = Vec::new();
    let mut mls = load_mls(home)?;
    let dir = HomeDir::load(home)?;
    for mut rec in list(home)? {
        if rec.status != Status::Active || !mls.groups.contains_key(&rec.id) {
            continue;
        }
        let mut r = connect(home, at(&rec, &base)).await?;
        events.extend(sync_group(home, &mut r, &mut mls, &dir, &mut rec).await?);
    }
    Ok(events)
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
    dir: &HomeDir,
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
            let Ok(inner) = outer::open(&m, &gid, generation, kind_of(e.kind), &e.blob) else {
                events.push(Event::Refused {
                    group: rec.name.clone(),
                    seq: e.seq,
                    why: "not sealed with this group's key".into(),
                });
                rec.cursor = e.seq;
                continue;
            };
            let mut res = g.process(&mls.dev, dir, &inner);
            if let Err(err) = &res {
                if is_missing_list(err) {
                    let missed = dir.take_missed();
                    if dir.refresh(home, r, &missed).await? > 0 {
                        res = g.process(&mls.dev, dir, &inner);
                    }
                }
            }
            rec.cursor = e.seq;
            match res {
                Ok(Processed::Message(body, from)) => {
                    let Ok(env) = Envelope::from_bytes(&body) else {
                        events.push(Event::Refused {
                            group: rec.name.clone(),
                            seq: e.seq,
                            why: "a message that is not one of ours".into(),
                        });
                        continue;
                    };
                    let owner = hex::encode(from.owner);
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
                Err(err) => events.push(Event::Refused {
                    group: rec.name.clone(),
                    seq: e.seq,
                    why: err.to_string(),
                }),
            }
        }
        if moved || rec.cursor > 0 {
            save_mls(home, mls)?;
            update_record(home, rec)?;
        }
    }
}
