//! Small groups over pairwise sessions (ADR 0024).
//!
//! Membership is a chain of signed states (`spike_core::group`); messages are
//! ordinary envelopes with `group` and `gstate` set, encrypted separately for
//! every device of every member over the one-to-one sessions. The relay sees
//! nothing new.
//!
//! What this module keeps, all sealed:
//! - `groups.enc`: each group's states, whether we are in it, and messages
//!   waiting for a state we have not seen yet;
//! - `members.enc`: identity cards of members who are not contacts, fetched
//!   from their relay and checked like contacts' cards;
//! - `group-outbox.enc`: replies owed to other members (states they asked for
//!   or need), sent by `recv` after it has acknowledged what it received.

use crate::client::Relay;
use crate::messages::{Direction, StoredMessage};
use crate::ops::{self, DeviceSend, Pool};
use crate::state::{Contact, Home};
use crate::transport::Target;
use crate::{MergeAction, Outcome};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spike_core::envelope::{Envelope, Kind};
use spike_core::group::{AddBy, Added, Chain, Draft, GroupState, History, Member, MessageCheck};
use spike_core::identity::{DeviceCard, Identity};
use spike_core::owner::{IdentityCard, Update};
use std::collections::{BTreeMap, HashMap};

const GROUPS: &str = "groups.enc";
const MEMBERS: &str = "members.enc";
const OUTBOX: &str = "group-outbox.enc";
/// Messages held per group while the state they name is fetched.
pub const MAX_HELD: usize = 50;
/// Groups waiting for you to accept, from people who are not contacts.
pub const MAX_STRANGER_INVITES: usize = 20;
/// Largest batch of states in one envelope (the envelope itself is capped at 50 KB of text).
const STATES_PER_ENVELOPE_BYTES: usize = 40_000;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// You are in it: its messages are shown and you can send.
    Joined,
    /// Someone added you; nothing is shown until you accept (ADR 0024, section 6).
    Invited,
    /// You left, declined, or were removed. History stays; a later add is an invite.
    Left,
}

/// A group message that names a state we do not hold yet.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HeldMsg {
    pub env: Envelope,
    pub sender: String,
    pub device: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Record {
    pub states: Vec<GroupState>,
    pub status: Status,
    /// Who invited us (owner key), while `Invited`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<String>,
    /// For an invite from someone who is not a contact: the device key whose session waits in
    /// quarantine, and their card, until you accept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stranger: Option<(String, String)>,
    #[serde(default)]
    pub held: Vec<HeldMsg>,
}

impl Record {
    pub fn chain(&self) -> Result<Chain> {
        Ok(Chain::from_states(self.states.clone())?)
    }
    pub fn current(&self) -> Result<GroupState> {
        Ok(self.chain()?.current().clone())
    }
    pub fn id(&self) -> &str {
        &self.states[0].group
    }
    fn set_chain(&mut self, c: &Chain) {
        self.states = c.states();
    }
}

/// A reply owed to another member.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
enum Reply {
    /// Send them our states after this version.
    Send {
        group: String,
        to: String,
        after: u64,
    },
    /// Ask them for the states after this version.
    Ask {
        group: String,
        to: String,
        after: u64,
    },
}

pub fn list(home: &Home) -> Result<Vec<Record>> {
    home.load_json(GROUPS)
}

fn save(home: &Home, all: &[Record]) -> Result<()> {
    home.save_json(GROUPS, &all)
}

fn my_owner(home: &Home) -> Result<String> {
    Ok(home.load_device_list()?.owner)
}

/// Find a group by exact name or by the first 6+ characters of its id.
pub fn find(home: &Home, query: &str) -> Result<Record> {
    let all = list(home)?;
    let q = query.trim();
    let mut hits: Vec<&Record> = all
        .iter()
        .filter(|r| r.current().is_ok_and(|s| s.name == q))
        .collect();
    if hits.is_empty() && q.len() >= 6 {
        let lower = q.to_lowercase();
        hits = all.iter().filter(|r| r.id().starts_with(&lower)).collect();
    }
    match hits.as_slice() {
        [r] => Ok((*r).clone()),
        [] => bail!("no group named '{q}' (or with an id starting with it)"),
        _ => bail!("'{q}' matches several groups; use the group id from `spike group list`"),
    }
}

fn update_record(home: &Home, rec: &Record) -> Result<()> {
    let mut all = list(home)?;
    match all.iter_mut().find(|r| r.id() == rec.id()) {
        Some(r) => *r = rec.clone(),
        None => all.push(rec.clone()),
    }
    save(home, &all)
}

// ---------- people ----------

/// Cards of group members who are not contacts, by owner key.
pub fn member_cards(home: &Home) -> Result<BTreeMap<String, String>> {
    home.load_json(MEMBERS)
}

fn contact_for<'a>(contacts: &'a [Contact], owner: &str) -> Option<&'a Contact> {
    contacts
        .iter()
        .find(|c| c.conversation_key().ok().as_deref() == Some(owner))
}

/// How a member is shown: "you", a contact's name, or "~" and the start of their fingerprint.
pub fn display_name(home: &Home, owner: &str) -> String {
    if my_owner(home).ok().as_deref() == Some(owner) {
        return "you".into();
    }
    if let Some(c) = home
        .load_contacts()
        .ok()
        .and_then(|cs| contact_for(&cs, owner).cloned())
    {
        return c.name;
    }
    let fp = member_cards(home)
        .ok()
        .and_then(|m| m.get(owner).cloned())
        .and_then(|card| IdentityCard::decode(&card).ok())
        .map(|c| c.fingerprint());
    match fp {
        Some(fp) => format!("~{}", &fp[..11]),
        None => format!("~{}", &owner[..8]),
    }
}

/// Fetch the device lists of members who are not contacts from their relays and
/// keep them under the usual update rules. Returns notes about anything refused.
pub async fn refresh_member_cards(
    home: &Home,
    base: &Target,
    me: &Identity,
) -> Result<Vec<String>> {
    let contacts = home.load_contacts()?;
    let mine = my_owner(home)?;
    let mut wanted: BTreeMap<String, Option<String>> = BTreeMap::new();
    for r in list(home)? {
        if r.status == Status::Left {
            continue;
        }
        for m in r.current()?.members {
            if m.owner != mine && contact_for(&contacts, &m.owner).is_none() {
                wanted.insert(m.owner, m.relay);
            }
        }
    }
    if wanted.is_empty() {
        return Ok(vec![]);
    }
    let mut cards = member_cards(home)?;
    let mut notes = vec![];
    let mut pool = Pool {
        base: base.clone(),
        me,
        conns: HashMap::new(),
    };
    for (owner, hint) in wanted {
        let url = hint.unwrap_or_else(|| base.url.clone());
        let fetched = match pool.get(&url).await {
            Ok(r) => r.fetch_device_list(&owner).await,
            Err(e) => Err(e),
        };
        let list = match fetched {
            Ok(Some(l)) => l,
            Ok(None) => continue,
            Err(e) => {
                notes.push(format!("could not reach {url} for a group member: {e:#}"));
                continue;
            }
        };
        if list.owner != owner {
            notes.push("a relay answered with someone else's device list".into());
            continue;
        }
        let card = IdentityCard::encode(&list);
        let Ok(new) = IdentityCard::decode(&card) else {
            notes.push("ignored an invalid device list for a group member".into());
            continue;
        };
        let accept = match cards.get(&owner).map(|c| IdentityCard::decode(c)) {
            None | Some(Err(_)) => true,
            Some(Ok(old)) => match old.check_update(&new) {
                Ok(Update::Newer) => true,
                Ok(_) => false,
                Err(e) => {
                    notes.push(format!("ignored a group member's device list: {e}"));
                    false
                }
            },
        };
        if accept {
            cards.insert(owner, card);
        }
    }
    home.save_json(MEMBERS, &cards)?;
    Ok(notes)
}

/// The identity card for each owner key: a contact's, or a member's from `members.enc`.
fn cards_for(home: &Home, owners: &[String]) -> Result<Vec<(String, Option<IdentityCard>)>> {
    let contacts = home.load_contacts()?;
    let members = member_cards(home)?;
    owners
        .iter()
        .map(|o| {
            let card = match contact_for(&contacts, o) {
                Some(c) => Some(c.identity()?),
                None => members
                    .get(o)
                    .map(|c| IdentityCard::decode(c))
                    .transpose()?,
            };
            Ok((o.clone(), card))
        })
        .collect()
}

// ---------- sending ----------

/// What a group send reached.
#[derive(Debug)]
pub struct GroupSend {
    pub id: String,
    /// Other members, and how many of them got it on at least one device.
    pub people: usize,
    pub reached: usize,
    pub devices: Vec<DeviceSend>,
    /// Every other member is a verified contact.
    pub all_verified: bool,
    pub notes: Vec<String>,
}

impl GroupSend {
    pub fn delivered_devices(&self) -> usize {
        self.devices
            .iter()
            .filter(|d| !d.own && d.error.is_none())
            .count()
    }
}

/// Encrypt `env` for every device of `owners` (and our own other devices if `own_copies`).
async fn fan_out(
    home: &Home,
    base: &Target,
    owners: &[String],
    env: &mut Envelope,
    own_copies: bool,
) -> Result<GroupSend> {
    let mut me = home.load_identity()?;
    let mut notes;
    {
        let mut r = Relay::connect(base.clone(), &me).await?;
        ops::top_up_prekeys(&mut r, home, &mut me).await?;
        notes = ops::sync_own_list(&mut r, home).await?;
        notes.extend(ops::refresh_contacts(&mut r, home, None).await?);
    }
    if !home.this_device_is_listed()? {
        bail!("this device has been removed from your identity and can no longer send");
    }
    notes.extend(refresh_member_cards(home, base, &me).await?);
    let mut sessions = home.load_sessions()?;
    env.lamport = home.next_lamport()?;

    let contacts = home.load_contacts()?;
    let my_id = me.device_id();
    // (device, relay, own, owner index)
    let mut targets: Vec<(DeviceCard, Option<String>, bool, usize)> = vec![];
    let people = cards_for(home, owners)?;
    for (i, (owner, card)) in people.iter().enumerate() {
        match card {
            Some(c) => targets.extend(c.device_relays().into_iter().map(|(d, r)| (d, r, false, i))),
            None => notes.push(format!(
                "no devices known yet for {}; they will not get this",
                display_name(home, owner)
            )),
        }
    }
    if own_copies {
        let mine = IdentityCard::decode(&home.identity_card()?)?;
        targets.extend(
            mine.device_relays()
                .into_iter()
                .filter(|(d, _)| d.device_id() != my_id)
                .map(|(d, r)| (d, r, true, usize::MAX)),
        );
    }
    // Someone who has never had a session with us needs to know who we are (ADR 0021).
    if matches!(env.kind, Kind::Text | Kind::GroupState)
        && targets
            .iter()
            .any(|(d, _, own, _)| !*own && !sessions.has_session(d))
    {
        env.intro = Some(home.identity_card()?);
    }
    let plaintext = env.to_bytes();
    let mut pool = Pool {
        base: base.clone(),
        me: &me,
        conns: HashMap::new(),
    };
    let mut devices = vec![];
    let mut reached = vec![false; owners.len()];
    for (card, relay, own, who) in targets {
        let url = relay.unwrap_or_else(|| base.url.clone());
        let r = ops::send_to_device(&mut pool, home, &mut sessions, &card, &url, &plaintext).await;
        if r.is_ok() && !own {
            reached[who] = true;
        }
        devices.push(DeviceSend {
            device_id: card.device_id(),
            own,
            new_session: *r.as_ref().unwrap_or(&false),
            error: r.err().map(|e| format!("{e:#}")),
        });
    }
    let all_verified = owners
        .iter()
        .all(|o| contact_for(&contacts, o).is_some_and(|c| c.verified));
    Ok(GroupSend {
        id: env.id.clone(),
        people: owners.len(),
        reached: reached.iter().filter(|r| **r).count(),
        devices,
        all_verified,
        notes,
    })
}

fn others(state: &GroupState, me: &str) -> Vec<String> {
    state
        .members
        .iter()
        .map(|m| m.owner.clone())
        .filter(|o| o != me)
        .collect()
}

/// Send a text message to a group you are in.
pub async fn send(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    text: &str,
) -> Result<GroupSend> {
    send_env(home, &relay.into(), group, Envelope::text(text)?).await
}

/// Send a text, edit or delete to a group, store it here once someone has it.
async fn send_env(home: &Home, base: &Target, group: &str, mut env: Envelope) -> Result<GroupSend> {
    let rec = find(home, group)?;
    if rec.status != Status::Joined {
        bail!("you are not in this group (see `spike group list`)");
    }
    let chain = rec.chain()?;
    let me = my_owner(home)?;
    let cur = chain.current();
    if !cur.is_member(&me) {
        bail!("you are no longer a member of this group");
    }
    env.group = Some(cur.group.clone());
    env.gstate = Some(cur.hash());
    let owners = others(cur, &me);
    let report = fan_out(home, base, &owners, &mut env, true).await?;
    if !owners.is_empty() && report.reached == 0 {
        let why: Vec<String> = report
            .devices
            .iter()
            .filter(|d| !d.own)
            .filter_map(|d| d.error.clone())
            .collect();
        bail!(
            "could not deliver to anyone in the group: {}",
            why.join("; ")
        );
    }
    let my_device = home.load_identity()?.device_id();
    match env.kind {
        Kind::Text => {
            store_message(home, &cur.group, &env, &me, &my_device, Direction::Out)?;
        }
        Kind::Edit | Kind::Delete => {
            apply_group_change(home, &cur.group, &env, &me, &me, &my_device)?;
        }
        _ => {}
    }
    Ok(report)
}

/// Edit or delete your own message in a group (called from `ops::edit` / `ops::delete`).
pub async fn send_change(
    home: &Home,
    base: Target,
    m: &StoredMessage,
    env: Envelope,
) -> Result<GroupSend> {
    send_env(home, &base, &m.peer, env).await
}

/// Distribute a new state to everyone in the old or new member list, and our own other devices.
async fn publish(
    home: &Home,
    base: &Target,
    prev: Option<&GroupState>,
    next: &GroupState,
) -> Result<GroupSend> {
    let me = my_owner(home)?;
    let mut owners = others(next, &me);
    if let Some(p) = prev {
        for o in others(p, &me) {
            if !owners.contains(&o) {
                owners.push(o);
            }
        }
    }
    let mut env = Envelope::group_control(
        Kind::GroupState,
        &next.group,
        &serde_json::to_string(&[next])?,
    )?;
    fan_out(home, base, &owners, &mut env, true).await
}

fn member_for(home: &Home, contacts: &[Contact], base: &Target, name: &str) -> Result<Member> {
    let c = contacts
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no contact '{name}'"))?;
    let id = c.identity()?;
    if id.is_legacy() {
        bail!("{name} uses an old single-device card; ask them to update before adding them to a group");
    }
    if crate::requests::blocked(home)?
        .iter()
        .any(|b| b.owner == id.owner_hex())
    {
        bail!("you blocked {name}");
    }
    // Where their device list can be fetched: the relay their devices read from.
    let relay = id
        .device_relays()
        .into_iter()
        .find_map(|(_, r)| r)
        .or_else(|| Some(base.url.clone()));
    Ok(Member {
        owner: id.owner_hex(),
        relay,
    })
}

/// Create a group with some of your contacts. You are its only admin.
pub async fn create(
    home: &Home,
    relay: impl Into<Target>,
    name: &str,
    with: &[String],
    add_by: AddBy,
) -> Result<(GroupState, GroupSend)> {
    let base: Target = relay.into();
    let contacts = home.load_contacts()?;
    let members: Vec<Member> = with
        .iter()
        .map(|n| member_for(home, &contacts, &base, n))
        .collect::<Result<_>>()?;
    let owner = home.load_owner()?;
    let state = GroupState::create(
        &owner,
        Some(base.url.clone()),
        name,
        members,
        add_by,
        History::Join,
    )?;
    let chain = Chain::new(state.clone())?;
    let mut rec = Record {
        states: vec![],
        status: Status::Joined,
        invited_by: None,
        stranger: None,
        held: vec![],
    };
    rec.set_chain(&chain);
    update_record(home, &rec)?;
    let report = publish(home, &base, None, &state).await?;
    Ok((state, report))
}

/// One change to a group you are in, signed by you, applied here and sent to everyone affected.
async fn change(
    home: &Home,
    base: Target,
    group: &str,
    edit: impl FnOnce(&mut Draft) -> Result<()>,
) -> Result<(GroupState, GroupSend)> {
    let mut rec = find(home, group)?;
    if rec.status != Status::Joined {
        bail!("you are not in this group");
    }
    let mut chain = rec.chain()?;
    let prev = chain.current().clone();
    let owner = home.load_owner()?;
    let mut draft = prev.draft();
    edit(&mut draft)?;
    let next = prev
        .successor(&owner, |d| *d = draft)
        .map_err(|e| match e {
            spike_core::Error::BadGroup("only admins can make this change") => {
                anyhow!("only admins can do that in this group")
            }
            other => other.into(),
        })?;
    chain.add(next.clone())?;
    rec.set_chain(&chain);
    if !chain.current().is_member(&my_owner(home)?) {
        rec.status = Status::Left;
    }
    update_record(home, &rec)?;
    let report = publish(home, &base, Some(&prev), &next).await?;
    Ok((next, report))
}

pub async fn add(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    names: &[String],
) -> Result<(GroupState, GroupSend)> {
    let base: Target = relay.into();
    let contacts = home.load_contacts()?;
    let new: Vec<Member> = names
        .iter()
        .map(|n| member_for(home, &contacts, &base, n))
        .collect::<Result<_>>()?;
    change(home, base, group, |d| {
        for m in new {
            if d.members.iter().any(|x| x.owner == m.owner) {
                bail!("{} is already in the group", display_name(home, &m.owner));
            }
            d.members.push(m);
        }
        Ok(())
    })
    .await
}

/// A member by contact name, or by the first 8+ characters of their owner key.
fn resolve_member(home: &Home, state: &GroupState, who: &str) -> Result<String> {
    let contacts = home.load_contacts()?;
    if let Some(c) = contacts.iter().find(|c| c.name == who) {
        let o = c.conversation_key()?;
        if state.is_member(&o) {
            return Ok(o);
        }
        bail!("{who} is not in this group");
    }
    let w = who.trim_start_matches('~').to_lowercase();
    if w.len() >= 8 {
        let hits: Vec<&Member> = state
            .members
            .iter()
            .filter(|m| m.owner.starts_with(&w))
            .collect();
        if let [m] = hits.as_slice() {
            return Ok(m.owner.clone());
        }
    }
    bail!(
        "no member '{who}': use a contact name or 8+ characters of their key (`spike group show`)"
    )
}

pub async fn remove(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    who: &str,
) -> Result<(GroupState, GroupSend)> {
    let cur = find(home, group)?.current()?;
    let owner = resolve_member(home, &cur, who)?;
    if owner == my_owner(home)? {
        bail!("use `spike group leave` to leave");
    }
    change(home, relay.into(), group, |d| {
        d.members.retain(|m| m.owner != owner);
        d.admins.retain(|a| *a != owner);
        Ok(())
    })
    .await
}

pub async fn make_admin(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    who: &str,
) -> Result<(GroupState, GroupSend)> {
    let cur = find(home, group)?.current()?;
    let owner = resolve_member(home, &cur, who)?;
    change(home, relay.into(), group, |d| {
        if d.admins.contains(&owner) {
            bail!("already an admin");
        }
        d.admins.push(owner);
        Ok(())
    })
    .await
}

pub async fn rename(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    name: &str,
) -> Result<(GroupState, GroupSend)> {
    let name = name.trim().to_string();
    change(home, relay.into(), group, |d| {
        d.name = name;
        Ok(())
    })
    .await
}

pub async fn set_add_by(
    home: &Home,
    relay: impl Into<Target>,
    group: &str,
    add_by: AddBy,
) -> Result<(GroupState, GroupSend)> {
    change(home, relay.into(), group, |d| {
        d.add_by = add_by;
        Ok(())
    })
    .await
}

/// Leave a group. Your history stays on this device; nobody can add you back without your OK.
pub async fn leave(home: &Home, relay: impl Into<Target>, group: &str) -> Result<GroupSend> {
    let rec = find(home, group)?;
    let cur = rec.current()?;
    let me = my_owner(home)?;
    if cur.members.len() == 1 {
        // The last member: nobody to tell.
        let mut rec = rec;
        rec.status = Status::Left;
        update_record(home, &rec)?;
        return Ok(GroupSend {
            id: String::new(),
            people: 0,
            reached: 0,
            devices: vec![],
            all_verified: true,
            notes: vec![],
        });
    }
    let owner = home.load_owner()?;
    let next = cur.leave(&owner)?;
    debug_assert!(!next.is_member(&me));
    let mut rec = rec;
    let mut chain = rec.chain()?;
    chain.add(next.clone())?;
    rec.set_chain(&chain);
    rec.status = Status::Left;
    update_record(home, &rec)?;
    publish(home, &relay.into(), Some(&cur), &next).await
}

/// Accept an invite: the group's messages are shown from now on.
pub fn accept(home: &Home, group: &str) -> Result<GroupState> {
    let mut rec = find(home, group)?;
    if rec.status != Status::Invited {
        bail!("there is no pending invite for this group");
    }
    if let Some((key, card)) = rec.stranger.take() {
        // Their session moves out of quarantine; their card is kept as a group member's.
        let k = key_bytes(&key)?;
        let mut q = home.load_quarantine()?;
        let mut main = home.load_sessions()?;
        main.adopt(&mut q, &k);
        home.save_sessions(&main)?;
        home.save_quarantine(&q)?;
        let id = IdentityCard::decode(&card)?;
        let mut cards = member_cards(home)?;
        cards.insert(id.owner_hex(), card);
        home.save_json(MEMBERS, &cards)?;
    }
    rec.status = Status::Joined;
    rec.invited_by = None;
    update_record(home, &rec)?;
    rec.current()
}

/// Decline an invite. The inviter is not told; a later add is a new invite.
pub fn decline(home: &Home, group: &str) -> Result<()> {
    let mut rec = find(home, group)?;
    if rec.status != Status::Invited {
        bail!("there is no pending invite for this group");
    }
    if let Some((key, _)) = rec.stranger.take() {
        let mut q = home.load_quarantine()?;
        q.forget(&key_bytes(&key)?);
        home.save_quarantine(&q)?;
    }
    rec.status = Status::Left;
    rec.invited_by = None;
    rec.held.clear();
    update_record(home, &rec)
}

fn key_bytes(hex_key: &str) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_key, &mut out).map_err(|_| anyhow!("bad key"))?;
    Ok(out)
}

/// Stored messages of a group, oldest first.
pub fn history(home: &Home, group: &str, limit: usize) -> Result<(Record, Vec<StoredMessage>)> {
    let rec = find(home, group)?;
    let msgs = home.messages()?.conversation(rec.id(), limit)?;
    Ok((rec, msgs))
}

// ---------- receiving ----------

/// Who sent a group envelope, as `process` established it from the session.
pub struct Sender<'a> {
    /// Owner key of the sending person (ours, for our own other devices).
    pub owner: &'a str,
    pub device: &'a str,
    /// One of our own other devices.
    pub own: bool,
}

fn store_message(
    home: &Home,
    gid: &str,
    env: &Envelope,
    author: &str,
    device: &str,
    dir: Direction,
) -> Result<bool> {
    let store = home.messages()?;
    let m = StoredMessage {
        id: env.id.clone(),
        peer: gid.to_string(),
        direction: dir,
        ts: env.ts,
        body: env.body.clone(),
        attachment: env.attachment.clone(),
        lamport: env.lamport,
        stamp_device: device.to_string(),
        edited: false,
        deleted: false,
        changed_at: 0,
        author: Some(author.to_string()),
    };
    // A tombstone left by someone else for this id must not suppress the real message:
    // in a group, other members see a message before you might and could "delete" it first.
    if let Some(t) = store.get(&env.id)? {
        if t.deleted && t.peer == gid && t.author.as_deref() != Some(author) {
            store.update(&m)?;
            return Ok(true);
        }
    }
    store.insert(&m)
}

/// Apply an edit or delete from `author` to a group message: only its author may change it.
fn apply_group_change(
    home: &Home,
    gid: &str,
    env: &Envelope,
    author: &str,
    me: &str,
    device: &str,
) -> Result<MergeAction> {
    let target = env.target.as_deref().unwrap_or_default();
    let store = home.messages()?;
    if let Some(m) = store.get(target)? {
        if m.peer != gid || m.author.as_deref() != Some(author) {
            return Ok(MergeAction::NotAuthor);
        }
    }
    let action = crate::apply_change(home, env, author == me, gid, device)?;
    if action == MergeAction::Tombstone {
        // Remember whose delete this was, so it binds only that author's message.
        if let Some(mut t) = store.get(target)? {
            t.author = Some(author.to_string());
            store.update(&t)?;
        }
    }
    Ok(action)
}

fn push_reply(home: &Home, r: Reply) -> Result<()> {
    let mut out: Vec<Reply> = home.load_json(OUTBOX)?;
    if !out.contains(&r) {
        out.push(r);
        home.save_json(OUTBOX, &out)?;
    }
    Ok(())
}

/// Handle any envelope with `group` set, from a contact, a known member, or our own device.
pub fn on_envelope(home: &Home, sender: Sender<'_>, env: &Envelope) -> Result<Outcome> {
    let gid = env.group.clone().expect("caller checked");
    let me = my_owner(home)?;
    let all = list(home)?;
    let rec = all.iter().find(|r| r.id() == gid).cloned();
    match env.kind {
        Kind::GroupState => {
            let Ok(states) = serde_json::from_str::<Vec<GroupState>>(&env.body) else {
                return Ok(Outcome::Invalid);
            };
            if states.is_empty() || states.iter().any(|s| s.group != gid) {
                return Ok(Outcome::Invalid);
            }
            on_states(home, &sender, &me, rec, states)
        }
        Kind::GroupStateRequest => {
            let (Some(rec), Some(after)) = (rec, parse_after(&env.body)) else {
                return Ok(Outcome::Ignored);
            };
            let cur = rec.current()?;
            if rec.status == Status::Joined && (sender.own || cur.is_member(sender.owner)) {
                push_reply(
                    home,
                    Reply::Send {
                        group: gid,
                        to: sender.owner.to_string(),
                        after,
                    },
                )?;
            }
            Ok(Outcome::Ignored)
        }
        Kind::Text | Kind::Edit | Kind::Delete => {
            let Some(mut rec) = rec else {
                return Ok(Outcome::Ignored);
            };
            if rec.status != Status::Joined {
                return Ok(Outcome::Ignored);
            }
            let chain = rec.chain()?;
            let name = chain.current().name.clone();
            if sender.own {
                // A copy of something we sent from another device.
                if env.kind == Kind::Text {
                    let fresh = store_message(home, &gid, env, &me, sender.device, Direction::Out)?;
                    return Ok(if fresh {
                        Outcome::SentCopy {
                            to: name,
                            body: env.body.clone().into_bytes(),
                            id: env.id.clone(),
                        }
                    } else {
                        Outcome::Duplicate
                    });
                }
                let a = apply_group_change(home, &gid, env, &me, &me, sender.device)?;
                return Ok(crate::change_outcome(
                    env.kind,
                    a,
                    "you".into(),
                    env.target.clone().unwrap_or_default(),
                ));
            }
            let gstate = env.gstate.as_deref().unwrap_or_default();
            match chain.check_message(gstate, sender.owner) {
                MessageCheck::NotMember => Ok(Outcome::Rejected {
                    reason: format!(
                        "{} is not a member of {name}",
                        display_name(home, sender.owner)
                    ),
                }),
                MessageCheck::Unknown => {
                    if rec.held.len() < MAX_HELD && !rec.held.iter().any(|h| h.env.id == env.id) {
                        rec.held.push(HeldMsg {
                            env: env.clone(),
                            sender: sender.owner.to_string(),
                            device: sender.device.to_string(),
                        });
                        update_record(home, &rec)?;
                    }
                    push_reply(
                        home,
                        Reply::Ask {
                            group: gid,
                            to: sender.owner.to_string(),
                            after: chain.base().saturating_sub(1).max(highest(&chain)),
                        },
                    )?;
                    Ok(Outcome::Ignored)
                }
                check @ (MessageCheck::Current
                | MessageCheck::Earlier
                | MessageCheck::OutOfDate) => {
                    let out_of_date = check == MessageCheck::OutOfDate;
                    if out_of_date {
                        // They use a member list that lost to ours: send them ours.
                        push_reply(
                            home,
                            Reply::Send {
                                group: gid.clone(),
                                to: sender.owner.to_string(),
                                after: chain.base().saturating_sub(1),
                            },
                        )?;
                    }
                    accept_message(
                        home,
                        &gid,
                        &name,
                        env,
                        sender.owner,
                        sender.device,
                        &me,
                        out_of_date,
                    )
                }
            }
        }
        _ => Ok(Outcome::Invalid),
    }
}

/// The highest version we hold on the current chain.
fn highest(chain: &Chain) -> u64 {
    chain.current().version
}

fn parse_after(body: &str) -> Option<u64> {
    #[derive(Deserialize)]
    struct After {
        after: u64,
    }
    serde_json::from_str::<After>(body).ok().map(|a| a.after)
}

#[allow(clippy::too_many_arguments)] // the message and everything known about its sender
fn accept_message(
    home: &Home,
    gid: &str,
    name: &str,
    env: &Envelope,
    author: &str,
    device: &str,
    me: &str,
    out_of_date: bool,
) -> Result<Outcome> {
    let who = display_name(home, author);
    if env.kind != Kind::Text {
        let a = apply_group_change(home, gid, env, author, me, device)?;
        return Ok(crate::change_outcome(
            env.kind,
            a,
            who,
            env.target.clone().unwrap_or_default(),
        ));
    }
    if !store_message(home, gid, env, author, device, Direction::In)? {
        return Ok(Outcome::Duplicate);
    }
    let verified = home
        .load_contacts()?
        .iter()
        .any(|c| c.verified && c.conversation_key().ok().as_deref() == Some(author));
    Ok(Outcome::GroupMessage {
        group: name.to_string(),
        from: who,
        verified,
        body: env.body.clone().into_bytes(),
        id: env.id.clone(),
        out_of_date,
    })
}

fn on_states(
    home: &Home,
    sender: &Sender<'_>,
    me: &str,
    rec: Option<Record>,
    states: Vec<GroupState>,
) -> Result<Outcome> {
    let by = display_name(home, sender.owner);
    let newest = states.last().expect("not empty").clone();
    let fresh_start = |status: Status| -> Result<Outcome> {
        let chain = match Chain::from_checkpoint(newest.clone()) {
            Ok(c) => c,
            Err(e) => {
                return Ok(Outcome::Rejected {
                    reason: format!("a group state from {by} was invalid: {e}"),
                })
            }
        };
        if !chain.current().is_member(me) {
            return Ok(Outcome::Ignored);
        }
        let mut r = Record {
            states: vec![],
            status,
            invited_by: (status == Status::Invited).then(|| sender.owner.to_string()),
            stranger: None,
            held: vec![],
        };
        r.set_chain(&chain);
        update_record(home, &r)?;
        let group = chain.current().name.clone();
        Ok(match status {
            Status::Invited => Outcome::GroupInvite {
                group,
                from: by.clone(),
            },
            _ => Outcome::GroupJoined {
                group,
                by: by.clone(),
            },
        })
    };
    let is_contact = contact_for(&home.load_contacts()?, sender.owner).is_some();
    let Some(mut rec) = rec else {
        // A group we have never seen: added by a contact (or by ourselves on another device) we
        // are in it; added by anyone else it is an invite.
        let status = if sender.own || is_contact {
            Status::Joined
        } else {
            Status::Invited
        };
        return fresh_start(status);
    };
    if rec.status == Status::Left {
        // After leaving, being added again is always an invite (ADR 0024, section 6). Our own
        // devices are the exception: that is us accepting elsewhere.
        if !newest.is_member(me) {
            return Ok(Outcome::Ignored);
        }
        return fresh_start(if sender.own {
            Status::Joined
        } else {
            Status::Invited
        });
    }
    let mut chain = rec.chain()?;
    let before = chain.current().hash();
    let mut asked = false;
    let mut refused = None;
    for s in states {
        match chain.add(s) {
            Ok(Added::MissingPrev { have }) if !asked => {
                push_reply(
                    home,
                    Reply::Ask {
                        group: rec.id().to_string(),
                        to: sender.owner.to_string(),
                        after: have,
                    },
                )?;
                asked = true;
            }
            Ok(_) => {}
            Err(e) => refused = Some(e.to_string()),
        }
    }
    rec.set_chain(&chain);
    let cur = chain.current().clone();
    if cur.hash() == before {
        update_record(home, &rec)?;
        return Ok(match refused {
            Some(r) => Outcome::Rejected {
                reason: format!("a change to {} from {by} was refused: {r}", cur.name),
            },
            None => Outcome::Ignored,
        });
    }
    let changed_by = display_name(home, &cur.author);
    if !cur.is_member(me) {
        rec.status = Status::Left;
        rec.held.clear();
        update_record(home, &rec)?;
        return Ok(Outcome::GroupRemoved {
            group: cur.name,
            by: changed_by,
        });
    }
    // Messages that waited for this state can be checked now.
    let mut released = 0;
    let held = std::mem::take(&mut rec.held);
    update_record(home, &rec)?;
    for h in held {
        match chain.check_message(h.env.gstate.as_deref().unwrap_or_default(), &h.sender) {
            MessageCheck::Unknown => rec.held.push(h),
            MessageCheck::NotMember => {}
            c => {
                let o = accept_message(
                    home,
                    rec.id(),
                    &cur.name,
                    &h.env,
                    &h.sender,
                    &h.device,
                    me,
                    c == MessageCheck::OutOfDate,
                )?;
                if matches!(o, Outcome::GroupMessage { .. }) {
                    released += 1;
                }
            }
        }
    }
    update_record(home, &rec)?;
    Ok(Outcome::GroupChanged {
        group: cur.name,
        by: changed_by,
        version: cur.version,
        released,
    })
}

/// A stranger's first message is a group invite (from `requests::on_stranger`): keep it as an
/// invite with its session in quarantine. Returns `None` if it is not a usable invite.
pub fn on_stranger_invite(
    home: &Home,
    env: &Envelope,
    device_key: &str,
    card: &str,
    owner: &str,
) -> Result<Option<Outcome>> {
    let Some(gid) = env.group.clone() else {
        return Ok(None);
    };
    let Ok(states) = serde_json::from_str::<Vec<GroupState>>(&env.body) else {
        return Ok(None);
    };
    let Some(newest) = states.last().filter(|s| s.group == gid).cloned() else {
        return Ok(None);
    };
    let me = my_owner(home)?;
    let all = list(home)?;
    if let Some(r) = all.iter().find(|r| r.id() == gid) {
        if r.status != Status::Left {
            return Ok(None);
        }
    }
    let waiting = all
        .iter()
        .filter(|r| r.stranger.is_some() && r.status == Status::Invited)
        .count();
    if waiting >= MAX_STRANGER_INVITES {
        return Ok(None);
    }
    let Ok(chain) = Chain::from_checkpoint(newest) else {
        return Ok(None);
    };
    if !chain.current().is_member(&me) || !chain.current().is_member(owner) {
        return Ok(None);
    }
    let mut r = Record {
        states: vec![],
        status: Status::Invited,
        invited_by: Some(owner.to_string()),
        stranger: Some((device_key.to_string(), card.to_string())),
        held: vec![],
    };
    r.set_chain(&chain);
    update_record(home, &r)?;
    let fp = IdentityCard::decode(card)
        .map(|c| c.fingerprint())
        .unwrap_or_default();
    Ok(Some(Outcome::GroupInvite {
        group: chain.current().name.clone(),
        from: format!("{fp} (not a contact)"),
    }))
}

/// Is this quarantined device key waiting on a group invite (so its session must be kept)?
pub fn is_invite_key(home: &Home, device_key: &str) -> Result<bool> {
    Ok(list(home)?.iter().any(|r| {
        r.status == Status::Invited && r.stranger.as_ref().is_some_and(|(k, _)| k == device_key)
    }))
}

/// Send the replies other members are owed. Called by `recv` after it acknowledged what it
/// received; replies that fail stay queued for next time. Returns how many replies were
/// tried: when it is not 0, ratchet state on disk has changed and callers must reload theirs.
pub async fn flush_outbox(home: &Home, base: Target) -> Result<usize> {
    let pending: Vec<Reply> = home.load_json(OUTBOX)?;
    if pending.is_empty() {
        return Ok(0);
    }
    let tried = pending.len();
    let mut left = vec![];
    for r in pending {
        let (group, to) = match &r {
            Reply::Send { group, to, .. } | Reply::Ask { group, to, .. } => {
                (group.clone(), to.clone())
            }
        };
        let Some(rec) = list(home)?.into_iter().find(|x| x.id() == group) else {
            continue;
        };
        let envs = match &r {
            Reply::Send { after, .. } => {
                let chain = rec.chain()?;
                batches(&chain.after(*after))
                    .into_iter()
                    .map(|b| Envelope::group_control(Kind::GroupState, &group, &b))
                    .collect::<spike_core::Result<Vec<_>>>()?
            }
            Reply::Ask { after, .. } => vec![Envelope::group_control(
                Kind::GroupStateRequest,
                &group,
                &serde_json::json!({ "after": after }).to_string(),
            )?],
        };
        let mut ok = true;
        for mut env in envs {
            match fan_out(home, &base, std::slice::from_ref(&to), &mut env, false).await {
                Ok(rep) if rep.reached > 0 => {}
                _ => ok = false,
            }
        }
        if !ok {
            left.push(r);
        }
    }
    home.save_json(OUTBOX, &left)?;
    Ok(tried)
}

/// Split states into JSON arrays that each fit one envelope.
fn batches(states: &[GroupState]) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Vec<&GroupState> = vec![];
    for s in states {
        cur.push(s);
        if serde_json::to_vec(&cur).map_or(0, |v| v.len()) > STATES_PER_ENVELOPE_BYTES
            && cur.len() > 1
        {
            let last = cur.pop().expect("more than one");
            out.push(serde_json::to_string(&cur).expect("serializes"));
            cur = vec![last];
        }
    }
    if !cur.is_empty() {
        out.push(serde_json::to_string(&cur).expect("serializes"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use spike_core::owner::OwnerKey;

    #[test]
    fn batches_fit_an_envelope_and_keep_order() {
        let a = OwnerKey::generate();
        let mut s =
            GroupState::create(&a, None, "G", vec![], AddBy::Admins, History::Join).unwrap();
        let mut all = vec![s.clone()];
        for i in 0..150 {
            s = s
                .successor(&a, |d| d.name = format!("{i}{}", "n".repeat(90)))
                .unwrap();
            all.push(s.clone());
        }
        let b = batches(&all);
        assert!(b.len() > 1);
        let back: Vec<GroupState> = b
            .iter()
            .flat_map(|j| {
                assert!(j.len() <= STATES_PER_ENVELOPE_BYTES);
                serde_json::from_str::<Vec<GroupState>>(j).unwrap()
            })
            .collect();
        assert_eq!(back, all);
    }
}
