//! Application-wide membership for mirroring typed input across terminal panes.

use crate::session::SessionId;

/// Membership follows the session ID through tab moves. The workspace validates
/// pane ownership and eligibility before passing IDs to the join operations.
#[derive(Debug, Default)]
pub(super) struct Membership {
    members: Vec<SessionId>,
}

impl Membership {
    pub(super) fn contains(&self, pane: SessionId) -> bool {
        self.members.contains(&pane)
    }

    /// Panes in the order in which they joined.
    pub(super) fn members(&self) -> &[SessionId] {
        &self.members
    }

    pub(super) fn includes_all(&self, panes: &[SessionId]) -> bool {
        !panes.is_empty() && panes.iter().all(|&pane| self.contains(pane))
    }

    /// Returns whether the pane is a member after the toggle.
    pub(super) fn toggle_pane(&mut self, pane: SessionId, join_allowed: bool) -> bool {
        if self.forget(pane) || !join_allowed {
            return false;
        }
        self.members.push(pane);
        true
    }

    /// Includes a partial group, removes a complete group and allows leaving
    /// under a managed deny. Returns whether the whole group is included after.
    pub(super) fn toggle_all(&mut self, panes: &[SessionId], join_allowed: bool) -> bool {
        let include = join_allowed && !self.includes_all(panes);
        self.set_panes(panes, include, join_allowed);
        include && !panes.is_empty()
    }

    /// Sets all supplied panes to the requested state. Returns whether it changed.
    pub(super) fn set_panes(
        &mut self,
        panes: &[SessionId],
        include: bool,
        join_allowed: bool,
    ) -> bool {
        if include && !join_allowed {
            return false;
        }
        let before = self.members.len();
        if include {
            for &pane in panes {
                if !self.contains(pane) {
                    self.members.push(pane);
                }
            }
        } else {
            self.members.retain(|pane| !panes.contains(pane));
        }
        self.members.len() != before
    }

    pub(super) fn forget(&mut self, pane: SessionId) -> bool {
        self.set_panes(&[pane], false, false)
    }

    pub(super) fn stop(&mut self) -> bool {
        let changed = !self.members.is_empty();
        self.members.clear();
        changed
    }

    /// Moves membership to a replacement session for the same logical pane.
    pub(super) fn replace(&mut self, old: SessionId, new: SessionId) -> bool {
        if old == new {
            return false;
        }
        let Some(index) = self.members.iter().position(|&pane| pane == old) else {
            return false;
        };
        if self.contains(new) {
            self.members.remove(index);
        } else {
            self.members[index] = new;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: SessionId = SessionId(1);
    const SECOND: SessionId = SessionId(2);
    const THIRD: SessionId = SessionId(3);
    const REPLACEMENT: SessionId = SessionId(4);

    #[test]
    fn group_join_deduplicates_and_leaving_then_rejoining_moves_to_end() {
        let mut membership = Membership::default();
        assert!(!membership.includes_all(&[]));
        assert!(!membership.toggle_all(&[], true));
        assert!(membership.toggle_pane(SECOND, true));
        assert!(membership.toggle_all(&[FIRST, SECOND, FIRST], true));
        assert!(!membership.set_panes(&[SECOND, FIRST], true, true));
        assert_eq!(membership.members(), &[SECOND, FIRST]);

        assert!(!membership.toggle_all(&[FIRST, SECOND], true));
        assert!(membership.members().is_empty());
        assert!(membership.set_panes(&[FIRST, SECOND], true, true));
        assert!(membership.forget(FIRST));
        assert!(!membership.forget(FIRST));
        assert!(membership.toggle_pane(FIRST, true));
        assert_eq!(membership.members(), &[SECOND, FIRST]);
    }

    #[test]
    fn denied_policy_keeps_stop_and_leaving_a_partial_group_available() {
        let mut membership = Membership::default();
        assert!(membership.set_panes(&[FIRST, THIRD], true, true));
        assert!(!membership.set_panes(&[SECOND], true, false));
        assert!(!membership.toggle_pane(SECOND, false));
        assert!(!membership.toggle_all(&[FIRST, SECOND], false));
        assert_eq!(membership.members(), &[THIRD]);

        assert!(!membership.toggle_pane(THIRD, false));
        assert!(!membership.stop());
        assert!(membership.set_panes(&[SECOND, FIRST], true, true));
        assert!(membership.stop());
        assert!(!membership.stop());
    }

    #[test]
    fn reconnect_retains_membership_at_the_same_join_index() {
        let mut membership = Membership::default();
        assert!(membership.set_panes(&[FIRST, SECOND, THIRD], true, true));
        assert!(membership.replace(SECOND, REPLACEMENT));
        assert_eq!(membership.members(), &[FIRST, REPLACEMENT, THIRD]);
        assert!(!membership.replace(SECOND, REPLACEMENT));
        assert!(!membership.replace(REPLACEMENT, REPLACEMENT));
        assert!(membership.replace(FIRST, THIRD));
        assert_eq!(membership.members(), &[REPLACEMENT, THIRD]);
    }
}
