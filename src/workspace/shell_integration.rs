use iced::widget::text_editor::{Action, Binding, Content};
use iced::widget::{button, column, row, rule, space, text, text_editor};
use iced::{Alignment, Element, Font, Length, Size, Task, window};
use kortty_pty::ShellIntegrationSnippet as Snippet;
use kortty_terminal::TerminalProfile;

use crate::session::{Frame, SessionId};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, PaneSession, Workspace, chrome};

const TITLE: &str = "Set Up Shell Integration";

#[derive(Clone, Debug)]
pub enum Message {
    Open(Option<SessionId>),
    Select(Snippet),
    Text(Action),
    Copy,
    CopySelection,
    Guide,
    Close,
}

#[derive(Default)]
pub(super) struct Setup {
    open: Option<Dialog>,
    last: Snippet,
}

struct Dialog {
    id: window::Id,
    owner: window::Id,
    selected: Snippet,
    snippets: [Content; 3],
    copied: Option<&'static str>,
    guide: bool,
}

impl Dialog {
    fn new(id: window::Id, owner: window::Id, selected: Snippet) -> Self {
        Self {
            id,
            owner,
            selected,
            snippets: Snippet::ALL.map(|snippet| Content::with_text(snippet.text())),
            copied: None,
            guide: false,
        }
    }

    fn select(&mut self, snippet: Snippet) {
        if self.selected != snippet {
            self.selected = snippet;
            self.copied = None;
        }
    }

    fn selected_index(&self) -> usize {
        match self.selected {
            Snippet::Bash => 0,
            Snippet::Zsh => 1,
            Snippet::Fish => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Entries {
    Setup,
    Prompts { available: bool },
}

impl Entries {
    pub(super) fn from_frame(enabled: bool, frame: &Frame) -> Option<Self> {
        if !enabled || frame.ibm.is_some() || !supports_marks(frame.screen.profile) {
            return None;
        }
        Some(if frame.has_prompt_marks {
            Self::Prompts {
                available: !frame.screen.alternate_screen,
            }
        } else {
            Self::Setup
        })
    }
}

impl Workspace {
    pub(super) fn shell_integration_title(&self, id: window::Id) -> Option<&'static str> {
        self.shell_integration
            .open
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        Some(TITLE)
    }

    pub(super) fn close_shell_integration(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        self.shell_integration_title(id)?;
        let dialog = self.shell_integration.open.take()?;
        Some(Task::batch([
            self.close_placed_dialog(id),
            window::gain_focus(dialog.owner),
        ]))
    }

    pub(super) fn close_shell_integration_owner(&mut self, owner: window::Id) -> Task<Desktop> {
        self.shell_integration
            .open
            .take_if(|dialog| dialog.owner == owner)
            .map_or_else(Task::none, |dialog| self.close_placed_dialog(dialog.id))
    }

    pub(super) fn update_shell_integration(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Open(session) => return self.show_shell_integration(id, session),
            Message::Close => return self.close_shell_integration(id).unwrap_or_else(Task::none),
            _ => {}
        }
        let Some(dialog) = self
            .shell_integration
            .open
            .as_mut()
            .filter(|dialog| dialog.id == id)
        else {
            return Task::none();
        };
        let copied = match message {
            Message::Select(snippet) => {
                dialog.select(snippet);
                self.shell_integration.last = snippet;
                None
            }
            Message::Text(action) if !action.is_edit() => {
                dialog.snippets[dialog.selected_index()].perform(action);
                None
            }
            Message::Copy => Some(dialog.selected.text().to_owned()),
            Message::CopySelection => dialog.snippets[dialog.selected_index()].selection(),
            Message::Guide => {
                dialog.guide = !dialog.guide;
                None
            }
            _ => None,
        };
        let Some(value) = copied else {
            return Task::none();
        };
        dialog.copied = Some(if self.clipboard.policy.internal {
            "Copied to the managed internal clipboard. Paste it into a kortty terminal or editor."
        } else {
            "Copied to the clipboard."
        });
        self.write_clipboard(value)
    }

    fn show_shell_integration(
        &mut self,
        owner: window::Id,
        session: Option<SessionId>,
    ) -> Task<Desktop> {
        let selected = if let Some(session) = session {
            if !self.window_contains_visible_pane(owner, session) {
                return Task::none();
            }
            let Some(pane) = self.sessions.get(&session) else {
                return Task::none();
            };
            if Entries::from_frame(self.window_settings.prompt_navigation_enabled, &pane.frame)
                != Some(Entries::Setup)
            {
                return Task::none();
            }
            local_shell(pane)
        } else if self.windows.contains_key(&owner) || self.settings.title(owner).is_some() {
            None
        } else {
            return Task::none();
        };
        if let Some(selected) = selected {
            self.shell_integration.last = selected;
        }
        if let Some(open) = self
            .shell_integration
            .open
            .as_mut()
            .filter(|open| open.owner == owner)
        {
            if let Some(selected) = selected {
                open.select(selected);
            }
            return window::gain_focus(open.id);
        }
        let (id, open) = self.open_placed_dialog(
            DialogKind::ShellIntegration,
            window::Settings {
                size: Size::new(880.0, 620.0),
                min_size: Some(Size::new(565.0, 450.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let former = self.shell_integration.open.replace(Dialog::new(
            id,
            owner,
            self.shell_integration.last,
        ));
        Task::batch([
            open.map(|_| Desktop::Noop),
            former.map_or_else(Task::none, |dialog| self.close_placed_dialog(dialog.id)),
        ])
    }

    pub(super) fn shell_integration_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self
            .shell_integration
            .open
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        let send = move |message| Desktop::ShellIntegration(id, message);
        let mut tabs = row![].spacing(5);
        for snippet in Snippet::ALL {
            tabs = tabs.push(
                button(snippet.name())
                    .style(move |theme, status| {
                        chrome::tab_button_style(theme, status, snippet == dialog.selected)
                    })
                    .on_press(send(Message::Select(snippet))),
            );
        }
        let content = &dialog.snippets[dialog.selected_index()];
        let snippet = text_editor(content)
            .font(Font::MONOSPACE)
            .size(13)
            .wrapping(iced::widget::text::Wrapping::None)
            .height(Length::Fill)
            .on_action(move |action| send(Message::Text(action)))
            .key_binding(move |key| {
                use Binding::{Copy, Custom, Move, Select, SelectAll, SelectLine, SelectWord};
                match Binding::from_key_press(key)? {
                    Copy => Some(Custom(send(Message::CopySelection))),
                    Binding::Enter => Some(Custom(send(Message::Copy))),
                    Binding::Unfocus => Some(Custom(send(Message::Close))),
                    binding @ (Move(_) | Select(_) | SelectAll | SelectLine | SelectWord) => {
                        Some(binding)
                    }
                    _ => None,
                }
            });
        let explanation = if dialog.guide {
            "OSC 133 marks prompts, the start of command output, and command completion. OSC 7 reports the shell's current directory. Open a new shell after installing the snippet, run a command, and use Previous Prompt in the terminal context menu to confirm the marks arrived."
        } else {
            "Open a new shell or reconnect after adding the snippet. Run a command, then right-click the terminal. When the menu offers Previous Prompt, the marks are arriving."
        };
        Some(column![
            text("Mark every prompt and command with a few lines in your shell's startup file").size(22),
            text("Add the snippet to the startup file of each local or remote shell you want marked. For SSH, install it on the server yourself."),
            text(if cfg!(windows) {
                "Use these snippets to set up local shells on Windows."
            } else {
                "For a local bash, zsh or fish, Quick Connect → Local Shell can add the snippet for one launch."
            }).size(13),
            tabs,
            text(instructions(dialog.selected)).size(14),
            snippet,
            row![
                button("Copy").on_press(send(Message::Copy)),
                button("Copy Selection").style(button::text).on_press_maybe(content.cursor().selection.is_some().then(|| send(Message::CopySelection))),
                button("Select All").style(button::text).on_press(send(Message::Text(Action::SelectAll))),
                text(dialog.copied.unwrap_or("")).size(13),
            ].spacing(8).align_y(Alignment::Center),
            text(explanation).size(13),
            rule::horizontal(1),
            row![
                button(if dialog.guide { "Back to setup" } else { "Shell integration in the manual" }).style(button::text).on_press(send(Message::Guide)),
                space::horizontal(),
                button("Close").on_press(send(Message::Close)),
            ].align_y(Alignment::Center),
        ].spacing(10).padding(16).width(Length::Fill).height(Length::Fill).into())
    }
}

fn local_shell(pane: &PaneSession) -> Option<Snippet> {
    if cfg!(windows) || pane.local_security.is_none() {
        return None;
    }
    let shell = std::env::var("SHELL").ok()?;
    Snippet::from_program(&shell)
}

pub(super) fn supports_marks(profile: TerminalProfile) -> bool {
    use TerminalProfile::*;
    matches!(
        profile,
        Xterm | Vt100 | Vt102 | Vt220 | Vt320 | Vt420 | Vt520 | ScoAnsi | SunCde | Cterm
    )
}

fn instructions(snippet: Snippet) -> &'static str {
    match snippet {
        Snippet::Bash => {
            "For bash 4.4 or later. Save as ~/.kortty-shell-integration.bash and add source ~/.kortty-shell-integration.bash to the end of ~/.bashrc, or paste the snippet there after anything that sets PROMPT_COMMAND or PS1. The bash 3.2 shipped with macOS ignores this snippet."
        }
        Snippet::Zsh => {
            "For zsh 5.1 or later. Save as ~/.kortty-shell-integration.zsh and add source ~/.kortty-shell-integration.zsh to the end of ~/.zshrc, or paste the snippet there after your prompt theme. With powerlevel10k, use POWERLEVEL9K_TERM_SHELL_INTEGRATION=true in ~/.p10k.zsh."
        }
        Snippet::Fish => {
            "For fish 3.1 or later. Save as ~/.config/fish/conf.d/kortty.fish. Fish reads that folder on its own."
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::text_editor::{Edit, Motion};

    use super::*;

    #[test]
    fn setup_is_modeless_remembers_shell_and_copies_only_through_policy() {
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        workspace.window_settings.prompt_navigation_enabled = false;
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let _ = workspace.update_shell_integration(owner, Message::Open(None));
        let first = workspace.shell_integration.open.as_ref().unwrap();
        assert_eq!(first.selected, Snippet::Bash);
        let id = first.id;
        assert!(workspace.owner_dialog(owner).is_none());
        assert!(workspace.shell_integration_view(id).is_some());
        let _ = workspace.update_shell_integration(id, Message::Select(Snippet::Fish));
        let _ = workspace.update_shell_integration(owner, Message::Open(None));
        assert_eq!(workspace.shell_integration.open.as_ref().unwrap().id, id);
        let _ = workspace.update_shell_integration(id, Message::Copy);
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some(Snippet::Fish.text())
        );

        let _ = workspace.update_shell_integration(other, Message::Open(None));
        let second = workspace.shell_integration.open.as_ref().unwrap();
        assert_ne!(second.id, id);
        assert_eq!(second.selected, Snippet::Fish);
        assert!(second.copied.is_none());
        let next = second.id;
        let _ = workspace
            .update_shell_integration(next, Message::Text(Action::Move(Motion::DocumentStart)));
        let _ = workspace
            .update_shell_integration(next, Message::Text(Action::Edit(Edit::Insert('!'))));
        let _ =
            workspace.update_shell_integration(next, Message::Text(Action::Select(Motion::End)));
        let _ = workspace.update_shell_integration(next, Message::CopySelection);
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Snippet::Fish.text().lines().next()
        );
        let _ = workspace.update_shell_integration(id, Message::Copy);
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Snippet::Fish.text().lines().next()
        );
        let _ = workspace.close_shell_integration_owner(owner);
        assert!(workspace.shell_integration_title(next).is_some());
        let _ = workspace.close_shell_integration_owner(other);
        assert!(workspace.shell_integration_title(next).is_none());
    }

    #[test]
    fn pane_setup_is_bound_to_the_visible_pane_and_command_mark_setting() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let grid = crate::session::GridSize::default();
        let profile = TerminalProfile::Xterm;
        let session = crate::session::Session::ssh(None, grid, profile, true, None).unwrap();
        let session = workspace.store_session(session, grid, 14);
        workspace.add_tab(owner, session);
        let _ = workspace.update_shell_integration(other, Message::Open(Some(session)));
        assert!(workspace.shell_integration.open.is_none());

        workspace.window_settings.prompt_navigation_enabled = false;
        let _ = workspace.update_shell_integration(owner, Message::Open(Some(session)));
        assert!(workspace.shell_integration.open.is_none());
        workspace.window_settings.prompt_navigation_enabled = true;
        let _ = workspace.update_shell_integration(owner, Message::Open(Some(session)));
        assert_eq!(
            workspace.shell_integration.open.as_ref().unwrap().owner,
            owner
        );
    }
}
