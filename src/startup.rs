//! Apply installation startup requirements before constructing the workspace.

use std::path::PathBuf;

use iced::widget::{button, column, scrollable, text};
use iced::{Element, Event, Size, Subscription, Task, Theme, event, keyboard, window};

use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::policy_models::{StartupPreparation, prepare_on_startup};
use crate::profile;
use crate::vault_window::{Message as VaultMessage, StartupPassword, StartupResult, VaultWindow};
use crate::workspace::{Message, Workspace};

pub(crate) struct Application(State);

enum State {
    Locked(Box<Locked>),
    Running(Box<Running>),
    Failed { window: window::Id, error: String },
    Stopped,
}

struct Locked {
    root: PathBuf,
    window: window::Id,
    prompt: StartupPassword,
}

struct Running {
    workspace: Workspace,
    _model_preparation: Option<StartupPreparation>,
}

enum Permission<'a> {
    Ready,
    PasswordRequired { managed: bool },
    PolicyUnavailable(&'a str),
}

fn permission(policy: &ClipboardPolicy, personal: bool, unlocked: bool) -> Permission<'_> {
    if let Some(problem) = &policy.problem {
        Permission::PolicyUnavailable(problem)
    } else if (policy.require_master_password || personal) && !unlocked {
        Permission::PasswordRequired {
            managed: policy.require_master_password,
        }
    } else {
        Permission::Ready
    }
}

impl Application {
    pub(crate) fn boot(root: PathBuf) -> (Self, Task<Message>) {
        Self::start(root, ClipboardPolicy::load(), None, None, false)
    }

    fn start(
        root: PathBuf,
        policy: ClipboardPolicy,
        previous_window: Option<window::Id>,
        vault: Option<VaultWindow>,
        recovery_key_changed: bool,
    ) -> (Self, Task<Message>) {
        match permission(
            &policy,
            Workspace::startup_password_requested(&root),
            vault.as_ref().is_some_and(VaultWindow::is_unlocked),
        ) {
            Permission::PolicyUnavailable(problem) => Self::failed(
                previous_window,
                format!("Could not apply the installed policy ({problem})."),
            ),
            Permission::PasswordRequired { managed } => {
                match StartupPassword::open(profile::vault_path_at(&root), managed) {
                    Ok(prompt) => {
                        let (window, opened) = startup_window();
                        (
                            Self(State::Locked(Box::new(Locked {
                                root,
                                window,
                                prompt,
                            }))),
                            opened.map(|id| Message::Vault(id, VaultMessage::FocusUnlock)),
                        )
                    }
                    Err(error) => Self::failed(
                        previous_window,
                        format!("Could not open the required vault ({error})."),
                    ),
                }
            }
            Permission::Ready => {
                if recovery_key_changed
                    && let Err(error) = profile::session::Store::purge_saved_output(&root)
                {
                    eprintln!("kortty: could not clear output from the previous vault ({error})");
                }
                #[cfg(target_os = "macos")]
                kortty_macos::initialize_desktop_notifications();
                let model_preparation = prepare_on_startup(root, &policy);
                let (workspace, task) = Workspace::boot(policy, vault);
                let task = match previous_window {
                    Some(window) => Task::batch([task, window::close(window)]),
                    None => task,
                };
                (
                    Self(State::Running(Box::new(Running {
                        workspace,
                        _model_preparation: model_preparation,
                    }))),
                    task,
                )
            }
        }
    }

    fn failed(previous_window: Option<window::Id>, error: String) -> (Self, Task<Message>) {
        eprintln!("kortty: {error}");
        let (window, opened) = previous_window.map_or_else(startup_window, |id| (id, Task::none()));
        (
            Self(State::Failed { window, error }),
            opened.map(|_| Message::Noop),
        )
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        let (id, result) = match &mut self.0 {
            State::Running(running) => return running.workspace.update(message),
            State::Locked(gate) => {
                let message = match message {
                    Message::Vault(id, VaultMessage::FocusUnlock) if id == gate.window => {
                        return iced::widget::operation::focus(gate.prompt.input());
                    }
                    Message::Vault(id, message) if id == gate.window => message,
                    Message::Window(id, window::Event::CloseRequested | window::Event::Closed)
                        if id == gate.window =>
                    {
                        VaultMessage::Close
                    }
                    Message::Quit => VaultMessage::Close,
                    _ => return Task::none(),
                };
                (gate.window, gate.prompt.update(message))
            }
            State::Failed { window, .. } => match message {
                Message::Quit => return self.exit(),
                Message::Window(id, window::Event::CloseRequested | window::Event::Closed)
                    if id == *window =>
                {
                    return self.exit();
                }
                _ => return Task::none(),
            },
            State::Stopped => return Task::none(),
        };
        match result {
            StartupResult::Wait(task) => task.map(move |message| Message::Vault(id, message)),
            StartupResult::Focus => {
                let State::Locked(gate) = &self.0 else {
                    unreachable!("the password prompt remains active")
                };
                iced::widget::operation::focus(gate.prompt.input())
            }
            StartupResult::Ready => {
                let State::Locked(gate) = std::mem::replace(&mut self.0, State::Stopped) else {
                    unreachable!("only the password prompt can finish authentication")
                };
                let (vault, recovery_key_changed) = gate.prompt.into_unlocked();
                let (ready, task) = Self::start(
                    gate.root,
                    ClipboardPolicy::load(),
                    Some(id),
                    Some(vault),
                    recovery_key_changed,
                );
                *self = ready;
                task
            }
            StartupResult::Exit => self.exit(),
        }
    }

    fn exit(&mut self) -> Task<Message> {
        self.0 = State::Stopped;
        iced::exit()
    }

    pub(crate) fn view(&self, id: window::Id) -> Element<'_, Message> {
        match &self.0 {
            State::Running(running) => running.workspace.view(id),
            State::Locked(gate) if gate.window == id => gate
                .prompt
                .view()
                .map(move |message| Message::Vault(id, message)),
            State::Failed { window, error } if *window == id => scrollable(
                column![
                    text("Could not start kortty").size(21),
                    text(error).wrapping(text::Wrapping::WordOrGlyph),
                    button("Close").on_press(Message::Quit),
                ]
                .spacing(12)
                .padding(16),
            )
            .into(),
            _ => text("").into(),
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        match &self.0 {
            State::Running(running) => running.workspace.subscription(),
            State::Locked(_) | State::Failed { .. } => {
                event::listen_with(|event, _, id| match event {
                    Event::Window(
                        event @ (window::Event::CloseRequested | window::Event::Closed),
                    ) => Some(Message::Window(id, event)),
                    Event::Keyboard(keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::Escape),
                        ..
                    }) => Some(Message::Window(id, window::Event::CloseRequested)),
                    _ => None,
                })
            }
            State::Stopped => Subscription::none(),
        }
    }

    pub(crate) fn title(&self, id: window::Id) -> String {
        match &self.0 {
            State::Running(running) => running.workspace.title(id),
            State::Locked(_) => "kortty · Master Password".into(),
            _ => "kortty · Startup".into(),
        }
    }

    pub(crate) fn window_theme(&self, id: window::Id) -> Theme {
        match &self.0 {
            State::Running(running) => running.workspace.window_theme(id),
            _ => Theme::Dark,
        }
    }

    pub(crate) fn window_style(&self, theme: &Theme) -> iced::theme::Style {
        match &self.0 {
            State::Running(running) => running.workspace.window_style(theme),
            _ => iced::theme::default(theme),
        }
    }
}

fn startup_window() -> (window::Id, Task<window::Id>) {
    window::open(window::Settings {
        size: Size::new(510.0, 285.0),
        min_size: Some(Size::new(360.0, 170.0)),
        position: window::Position::Centered,
        exit_on_close_request: false,
        ..window::Settings::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_gates_the_workspace_and_fails_closed_when_policy_is_unavailable() {
        let profile = crate::snippets::tests::Profile::new();
        let required = || ClipboardPolicy {
            require_master_password: true,
            ..ClipboardPolicy::default()
        };
        assert!(matches!(
            permission(&ClipboardPolicy::default(), false, false),
            Permission::Ready
        ));
        assert!(matches!(
            permission(&required(), false, true),
            Permission::Ready
        ));
        let (mut application, _) =
            Application::start(profile.0.clone(), required(), None, None, false);
        let State::Locked(gate) = &application.0 else {
            panic!("managed startup must wait for the vault");
        };
        let id = gate.window;
        assert!(!profile::vault_path_at(&profile.0).exists());
        let _ = application.update(Message::OpenVault(id));
        assert!(matches!(application.0, State::Locked(_)));
        let _ = application.update(Message::Window(id, window::Event::CloseRequested));
        assert!(matches!(application.0, State::Stopped));
        assert!(!profile::vault_path_at(&profile.0).exists());

        let unavailable = ClipboardPolicy {
            problem: Some("test policy cannot be read".into()),
            ..ClipboardPolicy::default()
        };
        assert!(matches!(
            permission(&unavailable, false, true),
            Permission::PolicyUnavailable(_)
        ));
        let (application, _) =
            Application::start(profile.0.clone(), unavailable, None, None, false);
        assert!(matches!(application.0, State::Failed { .. }));

        profile::save_new_atomic(
            &profile.0.join("window-settings.json"),
            br#"{"startup":{"require_master_password":true}}"#,
        )
        .unwrap();
        let (application, _) = Application::start(
            profile.0.clone(),
            ClipboardPolicy::default(),
            None,
            None,
            false,
        );
        assert!(matches!(application.0, State::Locked(_)));
    }

    #[test]
    fn invalid_required_vault_location_does_not_fall_back_to_workspace_boot() {
        let profile = crate::snippets::tests::Profile::new();
        std::fs::create_dir_all(profile::vault_path_at(&profile.0)).unwrap();
        let policy = ClipboardPolicy {
            require_master_password: true,
            ..ClipboardPolicy::default()
        };
        let (application, _) = Application::start(profile.0.clone(), policy, None, None, false);
        assert!(matches!(
            application.0,
            State::Failed { error, .. } if error.starts_with("Could not open the required vault")
        ));
    }
}
