//! How edits and deletes merge, identically on every device (multi-device
//! design, section "Keeping devices in step"). Pure functions: no I/O, so the
//! rules can be tested exhaustively.
//!
//! * A message's *stamp* is `(lamport, sender device id)`; larger wins, device
//!   id breaks ties, so every device converges to the same text.
//! * A delete is a tombstone and always wins over an edit, whatever the order.
//! * Only the message's author (the same *person*, on any of their devices) may
//!   edit or delete it.
//! * A delete that arrives before its message leaves a tombstone, so the
//!   message does not appear when it arrives later.

use crate::envelope::Kind;

/// The merge-relevant state of a stored message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// True if we sent it (the author is our own person); false if the contact did.
    pub authored_by_me: bool,
    pub deleted: bool,
    pub stamp: (u64, String),
}

/// What to do with an incoming edit or delete.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Replace the text and stamp.
    ApplyEdit,
    /// Mark deleted (clear text and attachment).
    ApplyDelete,
    /// Nothing to do (older edit, edit of a deleted message, duplicate delete).
    Ignore,
    /// The sender is not the author: refuse and report.
    NotAuthor,
    /// The target is unknown: a delete leaves a tombstone, an edit is dropped.
    Tombstone,
    DropUnknown,
}

/// Decide the effect of `kind` (Edit or Delete) from `sender_is_me` on `current`
/// (None if the target message is not stored here) with the given `stamp`.
pub fn decide(
    kind: Kind,
    sender_is_me: bool,
    current: Option<&State>,
    stamp: &(u64, String),
) -> Action {
    let Some(cur) = current else {
        return if kind == Kind::Delete {
            Action::Tombstone
        } else {
            Action::DropUnknown
        };
    };
    if cur.authored_by_me != sender_is_me {
        return Action::NotAuthor;
    }
    match kind {
        Kind::Delete if cur.deleted => Action::Ignore,
        Kind::Delete => Action::ApplyDelete,
        Kind::Edit if cur.deleted || *stamp <= cur.stamp => Action::Ignore,
        Kind::Edit => Action::ApplyEdit,
        Kind::Text => Action::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(mine: bool, deleted: bool, l: u64, d: &str) -> State {
        State {
            authored_by_me: mine,
            deleted,
            stamp: (l, d.into()),
        }
    }
    fn stamp(l: u64, d: &str) -> (u64, String) {
        (l, d.into())
    }

    #[test]
    fn newer_edit_wins_older_and_equal_are_ignored() {
        let cur = st(false, false, 5, "b");
        assert_eq!(
            decide(Kind::Edit, false, Some(&cur), &stamp(6, "a")),
            Action::ApplyEdit
        );
        assert_eq!(
            decide(Kind::Edit, false, Some(&cur), &stamp(5, "c")),
            Action::ApplyEdit,
            "same counter: higher device id wins"
        );
        assert_eq!(
            decide(Kind::Edit, false, Some(&cur), &stamp(5, "a")),
            Action::Ignore
        );
        assert_eq!(
            decide(Kind::Edit, false, Some(&cur), &stamp(5, "b")),
            Action::Ignore,
            "replay of the same edit"
        );
        assert_eq!(
            decide(Kind::Edit, false, Some(&cur), &stamp(4, "z")),
            Action::Ignore
        );
    }

    #[test]
    fn delete_always_wins() {
        let live = st(true, false, 9, "a");
        assert_eq!(
            decide(Kind::Delete, true, Some(&live), &stamp(1, "a")),
            Action::ApplyDelete,
            "even with an old stamp"
        );
        let gone = st(true, true, 9, "a");
        assert_eq!(
            decide(Kind::Delete, true, Some(&gone), &stamp(2, "a")),
            Action::Ignore
        );
        assert_eq!(
            decide(Kind::Edit, true, Some(&gone), &stamp(99, "z")),
            Action::Ignore,
            "no edit resurrects a deleted message"
        );
    }

    #[test]
    fn only_the_author_may_edit_or_delete() {
        let theirs = st(false, false, 1, "b");
        assert_eq!(
            decide(Kind::Delete, true, Some(&theirs), &stamp(2, "a")),
            Action::NotAuthor,
            "I cannot delete their message"
        );
        assert_eq!(
            decide(Kind::Edit, true, Some(&theirs), &stamp(2, "a")),
            Action::NotAuthor
        );
        let mine = st(true, false, 1, "a");
        assert_eq!(
            decide(Kind::Edit, false, Some(&mine), &stamp(2, "b")),
            Action::NotAuthor,
            "they cannot edit mine"
        );
    }

    #[test]
    fn unknown_targets_tombstone_deletes_and_drop_edits() {
        assert_eq!(
            decide(Kind::Delete, false, None, &stamp(1, "a")),
            Action::Tombstone
        );
        assert_eq!(
            decide(Kind::Edit, false, None, &stamp(1, "a")),
            Action::DropUnknown
        );
    }

    #[test]
    fn concurrent_edits_converge_in_any_order() {
        // Two devices edit concurrently; every arrival order ends on the same result.
        let edits = [stamp(7, "a"), stamp(7, "b"), stamp(6, "c")];
        let orders = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for o in orders {
            let mut cur = st(false, false, 1, "z");
            for &i in &o {
                if decide(Kind::Edit, false, Some(&cur), &edits[i]) == Action::ApplyEdit {
                    cur.stamp = edits[i].clone();
                }
            }
            assert_eq!(cur.stamp, stamp(7, "b"), "order {o:?}");
        }
    }
}
