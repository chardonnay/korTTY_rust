//! Interactive recovery of the exact SSH panes left offline by project restore.

use std::collections::{HashMap, HashSet};

use iced::widget::{button, column, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Task, window};
use kortty_ssh::ConnectError;

use crate::session::SessionId;
use crate::workspace::reconnect::ReconnectTarget;

use super::{Message, Workspace, WorkspaceMessage, wrap};

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Details(u64),
    Connect(u64),
    Unlock(u64),
    Dismiss(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum WaitReason {
    Profile,
    SignIn,
    VaultLocked,
    UnknownHostKey,
    ChangedHostKey,
    Policy,
    ConnectionFailed,
    Canceled,
}

impl WaitReason {
    pub(in crate::workspace) fn from_connect_error(error: &ConnectError) -> Self {
        match error {
            ConnectError::UnknownHostKey { .. } => Self::UnknownHostKey,
            ConnectError::ChangedHostKey { .. } => Self::ChangedHostKey,
            _ => Self::ConnectionFailed,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Profile => "Saved profile unavailable or ambiguous",
            Self::SignIn => "Sign-in required",
            Self::VaultLocked => "Vault locked",
            Self::UnknownHostKey => "Host key needs verification",
            Self::ChangedHostKey => "Host key changed",
            Self::Policy => "Blocked by policy or required isolation",
            Self::ConnectionFailed => "Connection failed",
            Self::Canceled => "Reconnect canceled",
        }
    }
}

#[derive(Default)]
pub(super) struct Attention {
    entries: HashMap<SessionId, Entry>,
    details: HashSet<window::Id>,
    vault_unlocked: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    ticket: u64,
    target: ReconnectTarget,
    reason: WaitReason,
}

struct Row {
    tab: usize,
    pane: usize,
    entry: Entry,
}

impl Workspace {
    pub(in crate::workspace) fn defer_project_ssh(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
        reason: WaitReason,
    ) {
        if self.project_attention_tab(owner, target).is_none() {
            return;
        }
        let ticket = self.allocate();
        self.project_reconnects.attention.entries.insert(
            target.source,
            Entry {
                ticket,
                target,
                reason,
            },
        );
    }

    pub(in crate::workspace) fn forget_project_ssh(&mut self, source: SessionId) {
        self.project_reconnects.attention.entries.remove(&source);
    }

    fn project_attention_tab(&self, owner: window::Id, target: ReconnectTarget) -> Option<usize> {
        let window = self.windows.get(&owner)?;
        let (index, _) = window.pane_location(target.source)?;
        (window.tabs.get(index)?.id() == target.tab
            && self.sessions.get(&target.source)?.ssh.is_some())
        .then_some(index)
    }

    fn project_attention_rows(&self, owner: window::Id) -> Vec<Row> {
        let entries = &self.project_reconnects.attention.entries;
        let mut rows = Vec::new();
        if entries.is_empty() {
            return rows;
        }
        if let Some(window) = self.windows.get(&owner) {
            for (index, tab) in window.tabs.iter().enumerate() {
                if let Some(tab) = tab.terminal() {
                    rows.extend(tab.grid.iter().filter_map(|(pane, source)| {
                        let &entry = entries.get(source)?;
                        let pane = tab.pane_index(*pane)?;
                        (entry.target.tab == tab.id).then_some(Row {
                            tab: index,
                            pane,
                            entry,
                        })
                    }));
                }
            }
        }
        rows.sort_unstable_by_key(|row| (row.tab, row.pane));
        rows
    }

    fn project_attention_connect_available(
        &self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> bool {
        self.quick_connect.is_none()
            && self.palette_can_open(owner)
            && !self.reconnect_worker_pending(owner, target)
            && self.reconnect_target(owner, target).is_some()
    }

    pub(super) fn update_project_attention(
        &mut self,
        owner: window::Id,
        event: Event,
    ) -> Task<WorkspaceMessage> {
        let rows = self.project_attention_rows(owner);
        let revision = rows.iter().map(|row| row.entry.ticket).max();
        match event {
            Event::Details(ticket) if revision == Some(ticket) => {
                let details = &mut self.project_reconnects.attention.details;
                if !details.remove(&owner) {
                    details.insert(owner);
                }
            }
            Event::Dismiss(ticket) if revision == Some(ticket) => {
                for row in rows {
                    self.forget_project_ssh(row.entry.target.source);
                }
                self.project_reconnects.attention.details.remove(&owner);
            }
            Event::Unlock(ticket)
                if revision == Some(ticket)
                    && rows
                        .iter()
                        .any(|row| row.entry.reason == WaitReason::VaultLocked)
                    && self.palette_can_open(owner) =>
            {
                return self.show_vault_unlock(owner);
            }
            Event::Connect(ticket) => {
                if let Some(entry) = rows
                    .into_iter()
                    .map(|row| row.entry)
                    .find(|entry| entry.ticket == ticket)
                    && self.project_attention_connect_available(owner, entry.target)
                {
                    self.select_tab(owner, entry.target.tab, false);
                    if let Some((_, pane)) = self.windows[&owner].pane_location(entry.target.source)
                        && let Some(tab) = self.active_tab_mut(owner)
                    {
                        tab.focused = pane;
                    }
                    return self.review_project_ssh_reconnect(owner, entry.target);
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn retry_unlocked_project_sessions(&mut self) {
        let attention = &mut self.project_reconnects.attention;
        attention
            .details
            .retain(|owner| self.windows.contains_key(owner));
        let unlocked = self
            .vault
            .as_ref()
            .is_some_and(crate::vault_window::VaultWindow::is_unlocked);
        let was_unlocked = std::mem::replace(&mut attention.vault_unlocked, unlocked);
        if !unlocked || was_unlocked {
            return;
        }
        let ready: Vec<_> = attention
            .entries
            .values()
            .filter(|entry| entry.reason == WaitReason::VaultLocked)
            .map(|entry| entry.target)
            .collect();
        for target in ready {
            if let Some(owner) = self
                .windows
                .keys()
                .copied()
                .find(|&owner| self.project_attention_tab(owner, target).is_some())
            {
                self.forget_project_ssh(target.source);
                self.requeue_unlocked_project_ssh(owner, target);
            }
        }
    }

    pub(in crate::workspace) fn project_attention_bar(
        &self,
        owner: window::Id,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        let rows = self.project_attention_rows(owner);
        let revision = rows.iter().map(|row| row.entry.ticket).max()?;
        let opened = self.project_reconnects.attention.details.contains(&owner);
        let message = move |event| wrap(owner, Message::Attention(event));
        let connect = rows
            .iter()
            .find(|row| self.project_attention_connect_available(owner, row.entry.target))
            .map(|row| message(Event::Connect(row.entry.ticket)));
        let unlock = self.vault_unlock_available()
            && rows
                .iter()
                .any(|row| row.entry.reason == WaitReason::VaultLocked);
        let count = rows.len();
        let summary = if count == 1 {
            format!(
                "1 project SSH pane needs attention · {}",
                rows[0].entry.reason.label()
            )
        } else {
            format!("{count} project SSH panes need attention")
        };
        let mut header = row![text(summary).size(13), space::horizontal()]
            .spacing(8)
            .align_y(Alignment::Center);
        if let Some(connect) = connect {
            header = header.push(
                button("Connect next…")
                    .style(button::text)
                    .on_press(connect),
            );
        }
        if unlock {
            header = header.push(
                button("Unlock Vault…")
                    .style(button::text)
                    .on_press(message(Event::Unlock(revision))),
            );
        }
        header = header
            .push(
                button(if opened { "Hide details" } else { "Details" })
                    .style(button::text)
                    .on_press(message(Event::Details(revision))),
            )
            .push(
                button("Dismiss")
                    .style(button::text)
                    .on_press(message(Event::Dismiss(revision))),
            );
        let mut content = column![header].spacing(5);
        if opened {
            let mut details = column![].spacing(2);
            for Row { tab, pane, entry } in rows {
                let target = self.sessions[&entry.target.source]
                    .ssh
                    .as_ref()
                    .expect("visible SSH pane");
                let label = format!("Tab {} · Pane {} · {}", tab + 1, pane + 1, target.label());
                details = details.push(
                    row![
                        text(label).size(13).width(Length::Fill),
                        text(entry.reason.label()).size(13),
                        button("Connect…").style(button::text).on_press_maybe(
                            self.project_attention_connect_available(owner, entry.target)
                                .then_some(message(Event::Connect(entry.ticket)))
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
            content = content
                .push(scrollable(details).height(Length::Fixed((count as f32 * 30.0).min(144.0))));
        }
        Some(content.into())
    }
}

#[cfg(test)]
mod tests {
    use crate::profile::{PaneLayout, WindowLayout, WorkspaceLayout};

    use super::*;

    fn ssh_pane(host: &str, focused: bool) -> PaneLayout {
        PaneLayout::Ssh {
            font: 14,
            focused,
            term: "xterm-256color".into(),
            initial_size: Default::default(),
            target: serde_json::from_value(serde_json::json!({
                "host": host, "port": 22, "username": "operator",
                // No store can contain this ID because next_id must be greater.
                "saved": u64::MAX,
            }))
            .unwrap(),
        }
    }

    #[test]
    fn project_attention_follows_split_panes_and_rejects_a_stale_owner() {
        let mut workspace = Workspace::default();
        let (first, _) = workspace.open_window(false);
        let (second, _) = workspace.open_window(false);
        let layout = WorkspaceLayout::new(vec![WindowLayout {
            width: 920.0,
            height: 600.0,
            position: None,
            maximized: false,
            dashboard_visible: None,
            active: 0,
            tab_titles: Default::default(),
            tab_groups: Default::default(),
            tab_primary_panes: Default::default(),
            tab_terminal_effects: Default::default(),
            tab_terminal_appearance: Default::default(),
            tabs: vec![PaneLayout::Split {
                vertical: true,
                ratio: 0.5,
                first: Box::new(ssh_pane("left.example", true)),
                second: Box::new(ssh_pane("right.example", false)),
            }],
        }]);
        let _ = workspace
            .restore_layout_into_with_tools(&layout, Some(first), &[])
            .unwrap();
        workspace.queue_project_reconnects(&[first]);
        assert_eq!(workspace.advance_project_reconnects().units(), 0);
        let rows = workspace.project_attention_rows(first);
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.entry.reason == WaitReason::Profile)
        );
        let right = rows[1].entry;
        let tab = workspace.windows[&first].tab().unwrap().id;
        let _ = workspace.transfer(first, Some(second), tab);
        assert!(workspace.project_attention_rows(first).is_empty());
        assert_eq!(workspace.project_attention_rows(second).len(), 2);
        assert_eq!(
            workspace
                .update_project_attention(first, Event::Connect(right.ticket))
                .units(),
            0
        );
        assert!(workspace.windows[&first].pending_ssh.is_none());

        let _ = workspace.update_project_attention(second, Event::Connect(right.ticket));
        assert!(workspace.windows[&second].pending_ssh.is_some());
        assert_eq!(
            workspace.windows[&second].tab().unwrap().focused_session(),
            Some(right.target.source)
        );
        let _ = workspace.update_ssh(crate::workspace::ssh_access::Message::Cancel(second));
        assert_eq!(workspace.project_attention_rows(second).len(), 2);
        let _ = workspace.update_project_attention(second, Event::Dismiss(right.ticket));
        assert!(workspace.project_attention_bar(second).is_none());
        assert_eq!(workspace.windows[&second].tab().unwrap().grid.len(), 2);
    }
}
