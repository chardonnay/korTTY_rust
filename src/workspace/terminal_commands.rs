use iced::Task;
use kortty_terminal::{LastOutput, LastOutputRange};

use super::{Message as WorkspaceMessage, Workspace};
use crate::{
    session::{Frame, SearchError, SessionId, TerminalCommand, TerminalCommandResult},
    terminal_view::Selection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    ClearBuffer,
    SelectLastOutput,
    CopyLastOutput,
    CopySelection,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Finished(SessionId, Command, u64, Option<TerminalCommandResult>),
}

pub(super) struct SelectedOutput {
    pending: Option<u64>,
    selection: Option<(u64, LastOutputRange)>,
}

impl SelectedOutput {
    pub(super) fn visible(&self, frame: &Frame) -> Option<Selection> {
        let (revision, range) = self.selection?;
        if revision != frame.search_revision || frame.screen.alternate_screen {
            return None;
        }
        let first_row = frame
            .screen
            .scrollback_lines
            .saturating_sub(frame.scroll_offset);
        let last_row = first_row + usize::from(frame.screen.rows.saturating_sub(1));
        if range.start_row > last_row || range.end_row < first_row {
            return None;
        }
        let start = if range.start_row < first_row {
            (0, 0)
        } else {
            ((range.start_row - first_row) as u16, range.start_column)
        };
        let end = if range.end_row > last_row {
            (
                frame.screen.rows.saturating_sub(1),
                frame.screen.columns.saturating_sub(1),
            )
        } else {
            (
                (range.end_row - first_row) as u16,
                range.end_column.saturating_sub(1),
            )
        };
        (start <= end).then(|| Selection::from_cells(start, end))
    }

    pub(super) fn stale(&self, frame: &Frame) -> bool {
        self.pending.is_none()
            && self
                .selection
                .is_some_and(|(revision, _)| revision != frame.search_revision)
    }
}

impl Workspace {
    pub(super) fn terminal_command(
        &mut self,
        session: SessionId,
        command: Command,
    ) -> Task<WorkspaceMessage> {
        let Some(owner) = self.terminal_command_owner(session) else {
            return Task::none();
        };
        let request = self.allocate();
        let Some(pane) = self.sessions.get_mut(&session) else {
            return Task::none();
        };
        let actor_command = match command {
            Command::ClearBuffer => TerminalCommand::ClearBuffer,
            Command::SelectLastOutput => TerminalCommand::SelectLastOutput,
            Command::CopyLastOutput => TerminalCommand::CopyLastOutput,
            Command::CopySelection => {
                let Some((revision, range)) = pane
                    .selected_output
                    .as_ref()
                    .and_then(|output| output.selection)
                else {
                    return Task::none();
                };
                TerminalCommand::CopySelection { revision, range }
            }
        };
        match pane.session.terminal_command(actor_command) {
            Ok(reply) => {
                if command == Command::SelectLastOutput {
                    let selection = pane
                        .selected_output
                        .as_ref()
                        .and_then(|output| output.selection);
                    pane.selected_output = Some(SelectedOutput {
                        pending: Some(request),
                        selection,
                    });
                }
                Task::perform(async move { reply.recv().await.ok() }, move |result| {
                    WorkspaceMessage::TerminalCommand(Message::Finished(
                        session, command, request, result,
                    ))
                })
            }
            Err(error) => {
                self.notice(owner, terminal_command_error(error).into());
                Task::none()
            }
        }
    }

    pub(super) fn update_terminal_command(&mut self, message: Message) -> Task<WorkspaceMessage> {
        let Message::Finished(session, command, request, result) = message;
        let Some(owner) = self.terminal_command_owner(session) else {
            return Task::none();
        };
        let Some(pane) = self.sessions.get_mut(&session) else {
            return Task::none();
        };
        if command == Command::SelectLastOutput {
            let Some(output) = pane
                .selected_output
                .as_mut()
                .filter(|output| output.pending == Some(request))
            else {
                return Task::none();
            };
            output.pending = None;
            if output.selection.is_none() {
                pane.selected_output = None;
            }
        }
        let notice = match result {
            Some(TerminalCommandResult::Cleared) => {
                pane.selection = None;
                pane.selected_output = None;
                return Task::none();
            }
            Some(TerminalCommandResult::Selected { revision, range })
                if revision == pane.session.frame().search_revision =>
            {
                pane.selection = None;
                pane.selected_output = Some(SelectedOutput {
                    pending: None,
                    selection: Some((revision, range)),
                });
                output_notice("Selected", range.truncated)
            }
            Some(TerminalCommandResult::Copied { text, truncated }) => {
                if command != Command::CopySelection {
                    self.notice(owner, output_notice("Copied", truncated));
                }
                return self.write_clipboard(text);
            }
            Some(TerminalCommandResult::LastOutput(status)) => {
                last_output_notice(command, status).into()
            }
            Some(
                TerminalCommandResult::Selected { .. } | TerminalCommandResult::SelectionChanged,
            ) => "The terminal changed before the selection could be used.".into(),
            Some(TerminalCommandResult::OutputTooLarge) => {
                "The last command’s output is too large to copy.".into()
            }
            None => "The terminal session is closed.".into(),
        };
        self.notice(owner, notice);
        Task::none()
    }

    fn terminal_command_owner(&self, session: SessionId) -> Option<iced::window::Id> {
        self.windows
            .iter()
            .find_map(|(&id, window)| window.pane_location(session).map(|_| id))
    }
}

fn terminal_command_error(error: SearchError) -> &'static str {
    match error {
        SearchError::Busy => "The terminal is busy. Try again.",
        SearchError::Closed => "The terminal session is closed.",
        _ => "This pane does not support terminal buffer actions.",
    }
}

fn last_output_notice(command: Command, status: LastOutput) -> &'static str {
    match status {
        LastOutput::Disabled => "Shell integration is disabled.",
        LastOutput::AlternateScreen if command == Command::ClearBuffer => {
            "The buffer is unavailable while a full-screen program is running."
        }
        LastOutput::AlternateScreen => {
            "Last output is unavailable while a full-screen program is running."
        }
        LastOutput::NoMarks => "This pane has no shell integration marks.",
        LastOutput::NoCommand => {
            "This pane has no finished command whose output is still in the scrollback."
        }
        LastOutput::NoOutput => "The last command printed no output.",
        LastOutput::Found(_) => "The terminal selection changed.",
    }
}

fn output_notice(action: &str, truncated: bool) -> String {
    if truncated {
        format!(
            "{action} the retained output of the last command. Earlier lines have left the scrollback."
        )
    } else {
        format!("{action} the output of the last command.")
    }
}
