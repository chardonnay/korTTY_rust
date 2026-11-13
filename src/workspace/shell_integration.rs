use iced::widget::text_editor::{Action, Binding, Content};
use iced::widget::{button, column, row, rule, scrollable, space, text, text_editor};
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
    ReopenAfterClose,
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
    closing: Option<Closing>,
    last: Snippet,
}

struct Closing {
    id: window::Id,
    owner: window::Id,
    next: Option<(window::Id, Option<SessionId>)>,
}

struct Dialog {
    id: window::Id,
    owner: window::Id,
    session: Option<SessionId>,
    selected: Snippet,
    snippets: Vec<Content>,
    copied: Option<&'static str>,
    guide: bool,
}

impl Dialog {
    fn new(id: window::Id, owner: window::Id, selected: Snippet) -> Self {
        Self {
            id,
            owner,
            session: None,
            selected,
            snippets: Snippet::ALL
                .into_iter()
                .map(|snippet| Content::with_text(snippet.text()))
                .collect(),
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
        Snippet::ALL
            .iter()
            .position(|snippet| *snippet == self.selected)
            .expect("selected snippet is in the shipped catalog")
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
        Some(self.begin_shell_integration_close(dialog, None, true, Task::none()))
    }

    pub(super) fn close_shell_integration_owner(&mut self, owner: window::Id) -> Task<Desktop> {
        self.close_shell_integration_owner_then(owner, Task::none())
    }

    pub(super) fn close_shell_integration_owner_then(
        &mut self,
        owner: window::Id,
        after: Task<Desktop>,
    ) -> Task<Desktop> {
        let closing = self.shell_integration.closing.as_mut().and_then(|closing| {
            closing.next.take_if(|(next, _)| *next == owner);
            (closing.owner == owner).then_some(closing.id)
        });
        if let Some(id) = closing
            && (after.units() > 0 || self.placed_dialog_owner(id).is_some())
        {
            return self.close_placed_dialog_then(id, after);
        }
        let dialog = self
            .shell_integration
            .open
            .take_if(|dialog| dialog.owner == owner)
            .filter(|dialog| self.placed_dialog_owner(dialog.id).is_some());
        match dialog {
            Some(dialog) => self.begin_shell_integration_close(dialog, None, false, after),
            None => after,
        }
    }

    fn begin_shell_integration_close(
        &mut self,
        dialog: Dialog,
        next: Option<(window::Id, Option<SessionId>)>,
        focus_owner: bool,
        after: Task<Desktop>,
    ) -> Task<Desktop> {
        let Dialog { id, owner, .. } = dialog;
        self.shell_integration.closing = Some(Closing { id, owner, next });
        self.close_placed_dialog_with_followup(
            id,
            focus_owner.then_some(owner),
            after.chain(Task::done(Desktop::ShellIntegration(
                id,
                Message::ReopenAfterClose,
            ))),
        )
    }

    pub(super) fn update_shell_integration(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Open(session) => return self.show_shell_integration(id, session),
            Message::ReopenAfterClose => {
                let request = self
                    .shell_integration
                    .closing
                    .take_if(|closing| closing.id == id)
                    .and_then(|closing| closing.next);
                return request.map_or_else(Task::none, |(owner, session)| {
                    self.show_shell_integration(owner, session)
                });
            }
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
                let selected = dialog.selected_index();
                dialog.snippets[selected].perform(action);
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
                .is_none()
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
        if let Some(closing) = &mut self.shell_integration.closing {
            closing.next = Some((owner, session));
            return Task::none();
        }
        if let Some(open) = self
            .shell_integration
            .open
            .as_mut()
            .filter(|open| open.owner == owner)
        {
            open.session = session;
            if let Some(selected) = selected {
                open.select(selected);
            }
            return window::minimize(open.id, false).chain(window::gain_focus(open.id));
        }
        if let Some(former) = self.shell_integration.open.take() {
            return self.begin_shell_integration_close(
                former,
                Some((owner, session)),
                false,
                Task::none(),
            );
        }
        let (id, open) = self.open_modeless_placed_dialog(
            owner,
            DialogKind::ShellIntegration,
            window::Settings {
                size: Size::new(880.0, 620.0),
                min_size: Some(Size::new(565.0, 450.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let mut dialog = Dialog::new(id, owner, self.shell_integration.last);
        dialog.session = session;
        self.shell_integration.open = Some(dialog);
        open
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
            .height(240)
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
            "OSC 133 A/B marks the prompt and input boundary used by terminal Agent shortcuts. Some shells also report command output/completion and the current directory. Previous Prompt confirms a prompt mark, but Agent shortcuts also require the input boundary and an empty prompt."
        } else {
            "Open a new shell or reconnect after setup. Enable terminal agent shortcuts in AI Manager → General, then type agent and press Tab at an empty prompt."
        };
        let status = dialog.session.map_or(
            "Open this dialog from a terminal's context menu to check that pane's Agent shortcut readiness.",
            |session| self.agent_shortcut_status(session),
        );
        Some(column![
            text("Shell integration and terminal Agent shortcuts").size(22),
            scrollable(column![
            text("Add the snippet to the startup file of each local or remote shell you want marked. For SSH, install it on the server yourself."),
            text("For a supported local shell, enable automatic integration in Quick Connect → Local Shell or its saved connection profile.").size(13),
            text(status).size(13),
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
            ].spacing(10).padding(iced::Padding::ZERO.right(12)))
                .height(Length::Fill),
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
    let settings = pane.local_security.as_ref()?;
    if let Some(command) = &settings.command {
        return command
            .first()
            .and_then(|program| Snippet::from_program(program));
    }
    let shell = kortty_pty::shell_program(settings.shell).ok()?;
    Snippet::from_program(shell.to_str()?)
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
        Snippet::PowerShell => {
            "For PowerShell, add the snippet after your prompt setup in $PROFILE, or use automatic integration for a local launch. The prompt wrapper marks the input boundary. PSReadLine support adds command-output marks when available."
        }
        Snippet::CommandPrompt => {
            "For cmd.exe, use automatic integration for a local launch, or run the copied command in the intended interactive shell. This uses a drive/path prompt for this session. CMD reports prompt boundaries only. Command duration, exit status and directory reports are unavailable."
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::futures::{StreamExt, executor::block_on, future::ready};
    use iced::widget::text_editor::{Edit, Motion};
    use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

    use super::*;

    #[test]
    fn setup_is_modeless_remembers_shell_and_copies_only_through_policy() {
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        workspace.window_settings.prompt_navigation_enabled = false;
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let (latest, _) = workspace.open_window(false);
        let _ = workspace.update_shell_integration(owner, Message::Open(None));
        let first = workspace.shell_integration.open.as_ref().unwrap();
        assert_eq!(first.selected, Snippet::Bash);
        let id = first.id;
        assert_eq!(workspace.placed_dialog_owner(id), Some(owner));
        assert!(workspace.owner_dialog(owner).is_none());
        assert!(workspace.shell_integration_view(id).is_some());
        let _ = workspace.update_shell_integration(id, Message::Select(Snippet::Fish));
        let reveal = workspace.update_shell_integration(owner, Message::Open(None));
        assert_eq!(workspace.shell_integration.open.as_ref().unwrap().id, id);
        let mut actions = task::into_stream(reveal).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Minimize(target, false))) if target == id
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(target))) if target == id
        ));
        let _ = workspace.update_shell_integration(id, Message::Copy);
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some(Snippet::Fish.text())
        );

        let replace = workspace.update_shell_integration(other, Message::Open(None));
        assert!(workspace.shell_integration.open.is_none());
        assert!(
            task::into_stream(workspace.update_shell_integration(latest, Message::Open(None)))
                .is_none()
        );
        let mut actions = task::into_stream(replace).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(target))) if target == id
        ));
        let Some(RuntimeAction::Output(reopen)) = block_on(actions.next()) else {
            panic!("expected shell setup to reopen after closing");
        };
        let _ = workspace.update(reopen);
        let second = workspace.shell_integration.open.as_ref().unwrap();
        assert_ne!(second.id, id);
        assert_eq!(second.selected, Snippet::Fish);
        assert!(second.copied.is_none());
        let next = second.id;
        assert_eq!(workspace.placed_dialog_owner(next), Some(latest));
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
        let _ = workspace.close_shell_integration_owner(latest);
        assert!(workspace.shell_integration_title(next).is_none());
    }

    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    #[test]
    fn settings_shell_setup_is_modeless_and_closes_before_settings_and_main() {
        use super::super::settings_window;

        for shutdown in [false, true] {
            let mut workspace = Workspace::default();
            let (owner, _) = workspace.open_window(false);
            let _ = workspace.open_window(false);
            let _ = workspace.update_settings(settings_window::Message::Open(owner));
            let settings = workspace.settings.for_owner(owner).unwrap();
            let _ = workspace.update_shell_integration(settings, Message::Open(None));
            let setup = workspace.shell_integration.open.as_ref().unwrap().id;
            assert_eq!(workspace.placed_dialog_owner(setup), Some(settings));
            assert!(workspace.owner_dialog(settings).is_none());
            let opened = workspace.finish_owned_dialog_open(setup, settings, None);
            assert!(matches!(
                block_on(task::into_stream(opened).unwrap().next()),
                Some(RuntimeAction::Window(WindowAction::Run(id, _)))
                    if id == setup || id == settings
            ));

            let close = if shutdown {
                workspace.close_window(owner)
            } else {
                workspace.close_settings(settings).unwrap()
            };
            let actions = task::into_stream(close).unwrap().filter_map(|action| {
                ready(match action {
                    RuntimeAction::Window(WindowAction::Close(id)) => Some((id, false)),
                    RuntimeAction::Window(WindowAction::GainFocus(id)) => Some((id, true)),
                    _ => None,
                })
            });
            assert_eq!(
                block_on(actions.collect::<Vec<_>>()),
                [(setup, false), (settings, false), (owner, !shutdown)]
            );
            assert!(workspace.shell_integration.open.is_none());
        }
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
