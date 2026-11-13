//! Encrypted output belongs only to the device's recovery snapshot.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use iced::widget::{button, column, row, scrollable, space, text};
use iced::{Element, Length, window};
use kortty_terminal::{Cell, OutputSnapshot};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::profile::{WorkspaceLayout, session::Snapshot};
use crate::session::SessionId;

use super::{Message, Workspace, WorkspaceMessage};

const MAX_PANE_BYTES: usize = 256 * 1024;
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(in crate::workspace) struct Preferences {
    pub(in crate::workspace) enabled: bool,
    pub(in crate::workspace) lines: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false,
            lines: 1000,
        }
    }
}

impl Preferences {
    pub(in crate::workspace) const LINES: std::ops::RangeInclusive<u16> = 100..=5000;

    pub(in crate::workspace) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Workspace {
    pub(in crate::workspace) fn session_output_enabled(&self) -> bool {
        let policy = &self.clipboard.policy;
        policy.problem.is_none()
            && policy
                .session_restore_output
                .unwrap_or(self.window_settings.restore_output.enabled)
    }

    pub(in crate::workspace) fn purge_session_output(&mut self) {
        if let Some(snapshot) = &mut self.session_restore.previous {
            snapshot.output = None;
        }
        if let Some(snapshot) = &mut self.session_restore.latest
            && snapshot.output.take().is_some()
            && let Some(writer) = &self.session_restore.writer
        {
            writer.write(Some(snapshot.clone()));
        }
        for pane in self.sessions.values_mut() {
            pane.restored_output = None;
        }
    }

    pub(in crate::workspace) fn reconcile_session_output(
        &mut self,
        layout_changed: bool,
        updated: Option<SessionId>,
    ) {
        let enabled = self.session_output_enabled();
        let unlocked = self.vault.as_ref().is_some_and(|vault| vault.is_unlocked());
        let locked = self
            .vault
            .as_ref()
            .is_none_or(|vault| vault.recovery_output_locked());
        let restore = &self.session_restore;
        let lines = if enabled && unlocked && restore.writer.is_some() {
            self.window_settings.restore_output.lines
        } else {
            0
        };
        if restore.output_enabled && !enabled {
            self.purge_session_output();
        }
        self.session_restore.output_enabled = enabled;
        let newly_locked = locked && !self.session_restore.output_locked;
        self.session_restore.output_locked = locked;
        let unconfigured = updated
            .and_then(|id| self.sessions.get(&id))
            .is_some_and(|pane| pane.recovery_output_rows.is_none());
        if lines != self.session_restore.output_capture_lines
            || newly_locked
            || (lines != 0 && (layout_changed || unconfigured))
        {
            self.session_restore.output_capture_lines = lines;
            let panes: std::collections::HashSet<_> = self
                .windows
                .values()
                .flat_map(|window| window.terminal_tabs())
                .filter(|tab| !tab.incognito)
                .flat_map(|tab| tab.grid.iter().map(|(_, &session)| session))
                .collect();
            for (&id, pane) in &mut self.sessions {
                let saved = panes.contains(&id);
                let rows = if saved { lines } else { 0 };
                if pane.recovery_output_rows != Some(rows) {
                    pane.recovery_output_rows = Some(rows);
                    pane.session.set_recovery_output_rows(rows);
                }
                if locked || !saved {
                    pane.restored_output = None;
                }
            }
        }
        if updated
            .and_then(|id| self.sessions.get(&id))
            .is_some_and(|pane| pane.recovery_output_rows.is_some_and(|rows| rows > 0))
            && !self.exiting
            && !self.scheduler_draining
        {
            self.session_restore.changed(false);
        }
    }

    pub(super) fn capture_session_output(
        &self,
        layout: &WorkspaceLayout,
        panes: &[SessionId],
    ) -> Option<String> {
        if !self.session_output_enabled() {
            return None;
        }
        let Some(vault) = self.vault.as_ref().filter(|vault| vault.is_unlocked()) else {
            return self
                .session_restore
                .latest
                .as_ref()
                .filter(|snapshot| &snapshot.layout == layout)
                .and_then(|snapshot| snapshot.output.clone());
        };
        let lines = usize::from(self.window_settings.restore_output.lines);
        let budget = (MAX_CAPTURE_BYTES / panes.len().max(1)).min(MAX_PANE_BYTES);
        let contents: Vec<_> = panes
            .iter()
            .map(|id| {
                self.sessions
                    .get(id)
                    .and_then(|pane| {
                        let frame = pane.session.frame();
                        frame
                            .recovery_output
                            .as_ref()
                            .map(|output| tail(output, lines, budget))
                    })
                    .unwrap_or_default()
            })
            .collect();
        if contents.iter().all(|output| output.is_empty()) {
            return None;
        }
        let plaintext = Zeroizing::new(
            serde_json::to_vec(
                &contents
                    .iter()
                    .map(|output| output.as_str())
                    .collect::<Vec<_>>(),
            )
            .ok()?,
        );
        let layout = serde_json::to_vec(layout).ok()?;
        vault
            .seal_recovery_output(&layout, &plaintext)
            .ok()
            .map(|sealed| STANDARD.encode(sealed))
    }

    pub(super) fn restore_session_output(
        &mut self,
        owner: window::Id,
        snapshot: &Snapshot,
        panes: &[SessionId],
    ) {
        let Some(encoded) = snapshot
            .output
            .as_ref()
            .filter(|_| self.session_output_enabled())
        else {
            return;
        };
        let Some(vault) = self.vault.as_ref().filter(|vault| vault.is_unlocked()) else {
            self.notice(
                owner,
                "Previous terminal output was skipped because the vault is locked.".into(),
            );
            return;
        };
        let open = || {
            let layout = serde_json::to_vec(&snapshot.layout).ok()?;
            let sealed = STANDARD.decode(encoded).ok()?;
            let plaintext = vault.open_recovery_output(&layout, &sealed).ok()?;
            let rows: Vec<String> = serde_json::from_slice(&plaintext).ok()?;
            Some(rows.into_iter().map(Zeroizing::new).collect::<Vec<_>>())
        };
        let Some(outputs) = open().filter(|outputs| outputs.len() == panes.len()) else {
            self.notice(
                owner,
                "Previous terminal output could not be authenticated for this session and vault."
                    .into(),
            );
            return;
        };
        for (&id, output) in panes.iter().zip(outputs) {
            if !output.is_empty()
                && let Some(pane) = self.sessions.get_mut(&id)
            {
                pane.restored_output = Some(output);
            }
        }
    }

    pub(in crate::workspace) fn with_restored_output<'a>(
        &'a self,
        id: SessionId,
        terminal: Element<'a, WorkspaceMessage>,
    ) -> Element<'a, WorkspaceMessage> {
        let Some(output) = self
            .sessions
            .get(&id)
            .and_then(|pane| pane.restored_output.as_ref())
        else {
            return terminal;
        };
        if !self.session_output_enabled()
            || !self.vault.as_ref().is_some_and(|vault| vault.is_unlocked())
        {
            return terminal;
        }
        column![
            row![
                text("Previous session output").size(13),
                space::horizontal(),
                button("Hide").on_press(WorkspaceMessage::SessionRestore(Message::HideOutput(id))),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            scrollable(text(output.as_str()).font(iced::Font::MONOSPACE).size(12))
                .width(Length::Fill)
                .height(116),
            terminal,
        ]
        .height(Length::Fill)
        .into()
    }
}

fn tail(snapshot: &OutputSnapshot, lines: usize, budget: usize) -> Zeroizing<String> {
    let mut rows = Vec::new();
    let mut bytes = 0;
    let live = snapshot.live.as_ref().filter(|live| !live.alternate_screen);
    let live_rows = live.into_iter().flat_map(|screen| {
        (0..screen.rows)
            .rev()
            .filter_map(|row| screen.row(row))
            .map(std::borrow::Cow::Borrowed)
    });
    for cells in live_rows.chain(snapshot.history.iter().rev().map(|row| row.cells())) {
        let line = display_line(&cells);
        if rows.is_empty() && line.is_empty() {
            continue;
        }
        bytes += line.len() + 1;
        if bytes > budget {
            break;
        }
        rows.push(line);
        if rows.len() == lines {
            break;
        }
    }
    let mut output = Zeroizing::new(String::with_capacity(bytes.min(budget)));
    for (index, line) in rows.iter().rev().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(line);
    }
    output
}

fn display_line(cells: &[Cell]) -> Zeroizing<String> {
    let mut row = Zeroizing::new(String::new());
    for cell in cells.iter().filter(|cell| cell.width != 0) {
        if cell.style.hidden || cell.text.is_empty() {
            row.extend(std::iter::repeat_n(' ', usize::from(cell.width)));
        } else {
            row.extend(cell.text.chars().filter(|&c| !c.is_control()
                && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')));
        }
    }
    let length = row.trim_end_matches(' ').len();
    row.truncate(length);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_captures_primary_history_past_the_analysis_limit_and_skips_hidden_cells() {
        let mut terminal = kortty_terminal::Terminal::new(32, 4);
        for line in 0..450 {
            terminal.advance(format!("history row {line}\r\n").as_bytes());
        }
        terminal.advance(b"visible \x1b[8mhidden-value\x1b[28m\r\n");
        let snapshot = terminal.recovery_output_snapshot(300).unwrap();
        let output = tail(&snapshot, 300, 64 * 1024);
        assert_eq!(output.lines().count(), 300);
        assert!(output.contains("history row 160"));
        assert!(output.ends_with("visible"));
        assert!(!output.contains("hidden-value"));
        terminal.advance(b"\x1b[?1049h");
        assert!(terminal.recovery_output_snapshot(300).is_none());
    }
}
