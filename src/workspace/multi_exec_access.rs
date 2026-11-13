//! Desktop selection and keyboard routing for app-wide Multi-exec.

use iced::widget::{button, row, text, tooltip};
use iced::{Alignment, Element, window};

use crate::session::{Frame, SessionId, Status};

use super::control_access::ordered_panes;
use super::{Message, Tab, Workspace};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Pane,
    PaneAt(SessionId),
    Tab,
    TabAt(u64),
    Window,
    Stop,
}

pub(super) struct KeyboardRoute {
    pub(super) source: SessionId,
    password_prompt: bool,
    pub(super) panes: Vec<SessionId>,
    pub(super) predict: bool,
}

impl KeyboardRoute {
    pub(super) fn accepts(&self, target: SessionId, frame: &Frame) -> bool {
        target == self.source
            || (frame.status == Status::Running
                && frame.ibm.is_none()
                && !frame.coding_agent_waiting_for_decision()
                && (!self.password_prompt || frame.password_prompt))
    }
}

impl Workspace {
    fn multi_exec_policy_allowed(&self) -> bool {
        self.clipboard.policy.multi_exec_allowed && self.clipboard.policy.problem.is_none()
    }

    fn multi_exec_scope(&self, owner: window::Id, action: Action) -> Vec<SessionId> {
        let Some(window) = self.windows.get(&owner) else {
            return Vec::new();
        };
        let panes = |tab: &Tab| {
            if tab.incognito {
                return Vec::new();
            }
            ordered_panes(tab)
                .into_iter()
                .map(|(_, id)| id)
                .filter(|id| {
                    self.sessions
                        .get(id)
                        .is_some_and(|pane| pane.frame.ibm.is_none())
                })
                .collect::<Vec<_>>()
        };
        match action {
            Action::Pane => window.tab().map_or_else(Vec::new, |tab| {
                panes(tab)
                    .into_iter()
                    .filter(|id| tab.focused_session() == Some(*id))
                    .collect()
            }),
            Action::PaneAt(target) => window
                .terminal_tabs()
                .find(|tab| tab.grid.iter().any(|(_, id)| *id == target))
                .map_or_else(Vec::new, |tab| {
                    panes(tab).into_iter().filter(|&id| id == target).collect()
                }),
            Action::Tab => window.tab().map_or_else(Vec::new, panes),
            Action::TabAt(id) => window
                .terminal_tabs()
                .find(|tab| tab.id == id)
                .map_or_else(Vec::new, panes),
            Action::Window => window.terminal_tabs().flat_map(panes).collect(),
            Action::Stop => Vec::new(),
        }
    }

    pub(super) fn multi_exec_available(&self, owner: Option<window::Id>, action: Action) -> bool {
        if action == Action::Stop {
            return !self.multi_exec.members().is_empty();
        }
        let Some(owner) = owner.filter(|&id| self.palette_can_open(id)) else {
            return false;
        };
        let panes = self.multi_exec_scope(owner, action);
        let allowed = self.multi_exec_policy_allowed();
        match action {
            Action::Window => allowed && !panes.is_empty(),
            _ => {
                !panes.is_empty()
                    && (allowed || panes.iter().any(|&id| self.multi_exec.contains(id)))
            }
        }
    }

    pub(super) fn multi_exec_checked(&self, owner: window::Id, action: Action) -> bool {
        self.multi_exec
            .includes_all(&self.multi_exec_scope(owner, action))
    }

    pub(super) fn update_multi_exec(&mut self, owner: Option<window::Id>, action: Action) {
        if !self.multi_exec_available(owner, action) {
            return;
        }
        let previous = self.multi_exec.members().to_vec();
        let allowed = self.multi_exec_policy_allowed();
        if let Some(owner) = owner.filter(|_| action != Action::Stop) {
            let panes = self.multi_exec_scope(owner, action);
            match action {
                Action::Pane | Action::PaneAt(_) => {
                    if let [pane] = panes.as_slice() {
                        self.multi_exec.toggle_pane(*pane, allowed);
                    }
                }
                Action::Tab | Action::TabAt(_) => {
                    self.multi_exec.toggle_all(&panes, allowed);
                }
                Action::Window => {
                    self.multi_exec.set_panes(&panes, true, allowed);
                }
                Action::Stop => unreachable!(),
            }
        } else {
            self.multi_exec.stop();
        }
        for id in previous
            .iter()
            .copied()
            .filter(|&id| !self.multi_exec.contains(id))
            .chain(
                self.multi_exec
                    .members()
                    .iter()
                    .copied()
                    .filter(|id| !previous.contains(id)),
            )
        {
            self.sync_mirror_typeahead(id);
        }
    }

    pub(super) fn multi_exec_member(&self, id: SessionId) -> bool {
        self.multi_exec_policy_allowed() && self.multi_exec.contains(id)
    }

    pub(super) fn tab_broadcast_active(&self, tab: &Tab) -> bool {
        tab.broadcast && self.multi_exec_policy_allowed() && !self.tab_has_ibm(tab)
    }

    pub(super) fn broadcast_menu_enabled(&self, owner: window::Id) -> bool {
        self.windows
            .get(&owner)
            .and_then(super::Window::tab)
            .is_some_and(|tab| {
                tab.broadcast
                    || (tab.grid.len() >= 2
                        && self.multi_exec_policy_allowed()
                        && !self.tab_has_ibm(tab))
            })
    }

    pub(super) fn mirror_blocks_typeahead(&self, id: SessionId) -> bool {
        self.multi_exec_member(id)
            || self
                .windows
                .values()
                .flat_map(super::Window::terminal_tabs)
                .any(|tab| {
                    tab.grid.iter().any(|(_, session)| *session == id)
                        && self.tab_broadcast_active(tab)
                })
    }

    pub(super) fn sync_mirror_typeahead(&self, id: SessionId) {
        if let Some(pane) = self.sessions.get(&id)
            && pane.typeahead_enabled
        {
            pane.session
                .set_typeahead_enabled(!self.mirror_blocks_typeahead(id));
        }
    }

    fn mirror_input_held(&self, id: SessionId) -> bool {
        self.terminal_agents.holds_mirrored_input(id)
            || self
                .sessions
                .get(&id)
                .is_some_and(|pane| pane.session.frame().coding_agent_waiting_for_decision())
            || self.windows.values().any(|window| {
                window
                    .pending_paste
                    .as_ref()
                    .is_some_and(|paste| paste.targets.contains(&id))
            })
    }

    pub(super) fn keyboard_targets(&self, tab: &Tab) -> Option<KeyboardRoute> {
        let source = tab.focused_session()?;
        let frame = self.sessions.get(&source)?.session.frame();
        let mut panes = vec![source];
        let broadcast = self.tab_broadcast_active(tab);
        let multi_exec = !tab.incognito && self.multi_exec_member(source);
        if frame.status == Status::Running && frame.ibm.is_none() {
            let local = broadcast.then(|| tab.targets()).into_iter().flatten();
            let global = multi_exec
                .then_some(self.multi_exec.members())
                .into_iter()
                .flatten()
                .copied();
            for id in local.chain(global) {
                if !panes.contains(&id) && !self.mirror_input_held(id) {
                    panes.push(id);
                }
            }
        }
        Some(KeyboardRoute {
            source,
            password_prompt: frame.password_prompt,
            panes,
            predict: !broadcast && !multi_exec,
        })
    }

    pub(super) fn multi_exec_tab_marker(&self, tab: &Tab) -> Option<String> {
        let member_count = tab
            .grid
            .iter()
            .filter(|(_, id)| self.multi_exec_member(**id))
            .count();
        let mut marks = Vec::new();
        if member_count > 0 {
            marks.push(format!(
                "Multi-exec: {member_count} of {} panes of this tab take part",
                tab.grid.len()
            ));
        }
        if tab.grid.len() > 1 && self.tab_broadcast_active(tab) {
            let source = tab.focused_session();
            let held = tab
                .grid
                .iter()
                .filter(|(_, id)| Some(**id) != source && self.mirror_input_held(**id))
                .count();
            let available = tab.grid.len() - held;
            marks.push(if held == 0 {
                format!("Broadcast mode: typing goes to all {available} panes of this tab")
            } else {
                format!(
                    "Broadcast mode: {available} of {} panes receive keys right now",
                    tab.grid.len()
                )
            });
        }
        (!marks.is_empty()).then(|| marks.join("\n"))
    }

    pub(super) fn multi_exec_status(&self) -> Option<Element<'_, Message>> {
        let panes = self.multi_exec.members();
        if panes.is_empty() {
            return None;
        }
        let mut windows = 0;
        let mut tabs = 0;
        for window in self.windows.values() {
            let window_tabs = window
                .terminal_tabs()
                .filter(|tab| tab.grid.iter().any(|(_, id)| self.multi_exec.contains(*id)))
                .count();
            windows += usize::from(window_tabs > 0);
            tabs += window_tabs;
        }
        let held = panes
            .iter()
            .filter(|&&id| {
                !self.multi_exec_policy_allowed()
                    || self.mirror_input_held(id)
                    || self
                        .sessions
                        .get(&id)
                        .is_none_or(|pane| pane.frame.status != Status::Running)
            })
            .count();
        let mut label = format!(
            "Multi-exec · panes: {} · tabs: {tabs} · windows: {windows}",
            panes.len()
        );
        if held != 0 {
            label.push_str(&format!(" · left out right now: {held}"));
        }
        Some(
            row![
                tooltip(
                    text(label).size(12),
                    text("Keys typed in a participating pane go to the other participating panes."),
                    tooltip::Position::Top,
                ),
                button(text("Stop Multi-exec").size(12))
                    .style(button::text)
                    .on_press(Message::MultiExec(None, Action::Stop)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::pane_grid;

    use crate::session::{CodingAgentState, GridSize, IbmKind, Session};

    use super::*;

    fn pane(workspace: &mut Workspace, state: Option<CodingAgentState>) -> SessionId {
        // Exercise the real workspace routing without launching a terminal process.
        let session = Session::disconnected_ibm(IbmKind::Tn3270e);
        session.set_mirror_test_frame(state);
        workspace.store_session(session, GridSize::default(), 14)
    }

    #[test]
    fn mirror_routes_skip_blocked_agents_and_resume_without_changing_focused_paste_scope() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other_owner, _) = workspace.open_window(false);
        let source = pane(&mut workspace, None);
        let held = pane(&mut workspace, Some(CodingAgentState::Blocked));
        let ready = pane(&mut workspace, None);
        let remote = pane(&mut workspace, None);
        workspace.add_tab(owner, source);
        workspace.add_tab(other_owner, remote);
        let (tab_id, held_slot, source_slot) = {
            let tab = workspace
                .windows
                .get_mut(&owner)
                .unwrap()
                .tab_mut()
                .unwrap();
            let source_slot = tab.focused;
            let (held_slot, _) = tab
                .grid
                .split(pane_grid::Axis::Vertical, source_slot, held)
                .unwrap();
            tab.grid
                .split(pane_grid::Axis::Horizontal, source_slot, ready)
                .unwrap();
            (tab.id, held_slot, source_slot)
        };
        let route = |workspace: &Workspace| {
            workspace
                .keyboard_targets(workspace.windows[&owner].tab().unwrap())
                .unwrap()
        };
        assert_eq!(route(&workspace).panes, [source]);
        assert!(route(&workspace).predict);

        let _ = workspace.update(Message::ToggleBroadcast(owner));
        let broadcast = route(&workspace);
        assert_eq!(broadcast.panes, [source, ready]);
        assert!(!broadcast.predict);
        assert!(!broadcast.accepts(held, &workspace.sessions[&held].session.frame()));
        assert!(broadcast.accepts(ready, &workspace.sessions[&ready].session.frame()));
        assert!(
            workspace
                .multi_exec_tab_marker(workspace.windows[&owner].tab().unwrap())
                .is_some_and(|label| label.contains("2 of 3 panes receive keys"))
        );
        assert!(workspace.current_targets(owner, tab_id, &[source]));
        assert!(!workspace.current_targets(owner, tab_id, &[source, ready]));

        let _ = workspace.update(Message::FocusPane(owner, held_slot));
        let direct = route(&workspace);
        assert_eq!(direct.source, held);
        assert!(direct.accepts(held, &workspace.sessions[&held].session.frame()));
        assert!(workspace.current_targets(owner, tab_id, &[held]));
        let _ = workspace.update(Message::FocusPane(owner, source_slot));
        let _ = workspace.update(Message::ToggleBroadcast(owner));

        workspace.update_multi_exec(Some(owner), Action::Tab);
        workspace.update_multi_exec(Some(other_owner), Action::Pane);
        let global = route(&workspace);
        assert_eq!(global.panes, [source, ready, remote]);
        assert!(!global.accepts(held, &workspace.sessions[&held].session.frame()));
        assert!(workspace.mirror_input_held(held));

        workspace.sessions[&held]
            .session
            .set_mirror_test_frame(Some(CodingAgentState::Idle));
        let idle = route(&workspace);
        assert_eq!(idle.panes.len(), 4);
        assert!(idle.panes.contains(&held));
        assert!(idle.accepts(held, &workspace.sessions[&held].session.frame()));
        assert!(!workspace.mirror_input_held(held));
    }
}
