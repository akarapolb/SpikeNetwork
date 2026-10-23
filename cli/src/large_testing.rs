//! Test hooks (feature `test-hooks`): make a commit the honest client refuses to make and put it in
//! the group's mailbox, so tests can show that every other member refuses it too.

use crate::large::{at, connect, find, load_mls, save_mls, Status};
use crate::transport::Target;
use anyhow::{anyhow, bail, Result};
use spike_core::wire::GroupKind;
use spike_mls::outer::{self, Kind};

/// Commit an add of `key_package` with no checks (whoever we are) and append it at the next
/// position. Returns that position. The commit stays pending in our own state.
pub async fn append_unchecked_add(
    home: &crate::state::Home,
    relay: impl Into<Target>,
    group: &str,
    key_package: &[u8],
) -> Result<u64> {
    let base: Target = relay.into();
    let rec = find(home, group)?;
    if rec.status != Status::Active {
        bail!("not in the group");
    }
    let mut mls = load_mls(home)?;
    let g = mls
        .groups
        .get_mut(&rec.id)
        .ok_or_else(|| anyhow!("group state is missing"))?;
    let commit = spike_mls::testing::unchecked_add(&mls.dev, g, key_package);
    let m = g.mailbox_material();
    let blob = outer::seal(&m, &g.id(), g.mailbox_generation(), Kind::Commit, &commit);
    save_mls(home, &mls)?;
    let mut r = connect(home, at(&rec, &base)).await?;
    match r
        .group_append(
            &m.mailbox_key(),
            &m.mailbox_id(),
            GroupKind::Commit,
            Some(rec.cursor + 1),
            &blob,
            None,
        )
        .await?
    {
        crate::client::Appended::Seq(n) => Ok(n),
        other => bail!("not appended: {other:?}"),
    }
}

/// A key package of this device, for a test to hand to [`append_unchecked_add`].
pub fn key_package(home: &crate::state::Home) -> Result<Vec<u8>> {
    Ok(load_mls(home)?.dev.key_package())
}
