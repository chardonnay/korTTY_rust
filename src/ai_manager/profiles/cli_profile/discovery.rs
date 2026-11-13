use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, pick_list, row, text};
use iced::{Element, Task};

use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::cli::{self, ModelList};

use super::super::{Editor, Field, Message, Profile, ProfileId, picker};
use super::{Action as CliAction, preset_arguments};

#[derive(Clone)]
pub(crate) enum Action {
    Start(ModelList),
    Inspected(Arc<Ticket>, Result<Arc<cli::Discovery>, String>),
    Run(Arc<Ticket>),
    Finished(Arc<Ticket>, Result<Arc<cli::Discovered>, String>),
    Choose(Arc<Ticket>, String),
    SelectLoad(Arc<Ticket>, String),
    ReviewLoad(Arc<Ticket>, String),
    Load(Arc<LoadTicket>),
    BackFromLoad(Arc<LoadTicket>),
    Loaded(Arc<LoadTicket>, Result<(), String>),
    Cancel(Arc<Ticket>),
}

impl Action {
    pub(super) fn completed(&self) -> bool {
        matches!(
            self,
            Self::Inspected(..) | Self::Finished(..) | Self::Loaded(..)
        )
    }
}

pub(crate) struct Ticket {
    profile_id: ProfileId,
    path: String,
    list: ModelList,
    root: PathBuf,
}

impl Ticket {
    fn matches(&self, profile: &Profile) -> bool {
        profile.id == self.profile_id && studio_path(profile).is_some_and(|path| path == self.path)
    }
}

pub(crate) struct LoadTicket {
    discovery: Arc<Ticket>,
    model: String,
}

pub(in crate::ai_manager::profiles) struct State {
    ticket: Arc<Ticket>,
    phase: Phase,
}

enum Phase {
    Inspecting,
    Review(Arc<cli::Discovery>),
    Running {
        _cancel: Cancel,
    },
    Choices(Choices),
    LoadReview {
        ticket: Arc<LoadTicket>,
        choices: Choices,
        load: Arc<cli::Load>,
    },
    Loading {
        ticket: Arc<LoadTicket>,
        choices: Choices,
        _cancel: Cancel,
    },
}

#[derive(Clone)]
struct Choices {
    models: Arc<cli::Discovered>,
    load_selection: Option<String>,
}

struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn studio_path(profile: &Profile) -> Option<&str> {
    let settings = profile.cli.as_ref().filter(|_| profile.is_cli())?;
    (settings.has_preset(cli::LM_STUDIO_STDIN) || settings.has_preset(cli::LM_STUDIO_LOADED_STDIN))
        .then_some(settings.path.as_str())
}

fn cli_message(action: Action) -> Message {
    Message::Cli(CliAction::Discovery(action))
}

fn worker<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
    message: impl Fn(Result<T, String>) -> Message + Send + 'static,
) -> Result<Task<Message>, String> {
    let (send, receive) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kortty-lm-studio-discovery".into())
        .spawn(move || {
            let _ = send.try_send(operation());
        })
        .map_err(|_| "Could not start LM Studio model discovery.")?;
    Ok(Task::perform(
        async move {
            receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("LM Studio model discovery stopped.".into()))
        },
        message,
    ))
}

impl Editor {
    pub(in crate::ai_manager) fn cancel_cli_discovery(&mut self) {
        self.cli_discovery = None;
    }

    pub(in crate::ai_manager) fn cli_discovery_busy(&self) -> bool {
        self.cli_discovery
            .as_ref()
            .is_some_and(|state| !matches!(state.phase, Phase::Choices(_)))
    }

    pub(super) fn update_cli_discovery(
        &mut self,
        action: Action,
        editable: bool,
        root: Option<&Path>,
    ) -> Task<Message> {
        let (ticket, action) = match action {
            Action::Start(list) => {
                return self.start_cli_discovery(list, editable, root);
            }
            Action::Inspected(ref ticket, _)
            | Action::Run(ref ticket)
            | Action::Finished(ref ticket, _)
            | Action::Choose(ref ticket, _)
            | Action::SelectLoad(ref ticket, _)
            | Action::ReviewLoad(ref ticket, _)
            | Action::Cancel(ref ticket) => (ticket.clone(), action),
            Action::Load(ref ticket)
            | Action::BackFromLoad(ref ticket)
            | Action::Loaded(ref ticket, _) => (ticket.discovery.clone(), action),
        };
        let Some(state) = &self.cli_discovery else {
            return Task::none();
        };
        if !Arc::ptr_eq(&ticket, &state.ticket) {
            return Task::none();
        }
        if matches!(action, Action::Cancel(_))
            || !editable
            || root != Some(ticket.root.as_path())
            || !self
                .selected()
                .is_some_and(|profile| ticket.matches(profile))
        {
            self.cancel_cli_discovery();
            return Task::none();
        }
        let next = match (action, &state.phase) {
            (Action::Inspected(_, result), Phase::Inspecting) => result.map(Phase::Review),
            (Action::Finished(_, result), Phase::Running { .. }) => result.map(|models| {
                Phase::Choices(Choices {
                    models,
                    load_selection: None,
                })
            }),
            (Action::Run(_), Phase::Review(discovery)) => {
                return self.run_cli_discovery(ticket, discovery.clone());
            }
            (Action::Choose(_, model), Phase::Choices(choices))
                if choices.models.keys().contains(&model) =>
            {
                if let Some(settings) = self.selected_mut().and_then(|profile| profile.cli.as_mut())
                    && settings.has_preset(cli::LM_STUDIO_LOADED_STDIN)
                {
                    settings.arguments = preset_arguments(cli::LM_STUDIO_STDIN);
                    settings.fingerprint = None;
                }
                self.edit(Field::Model, model);
                return Task::none();
            }
            (Action::SelectLoad(_, model), Phase::Choices(choices))
                if ticket.list == ModelList::Downloaded
                    && choices.models.keys().contains(&model) =>
            {
                Ok(Phase::Choices(Choices {
                    models: Arc::clone(&choices.models),
                    load_selection: Some(model),
                }))
            }
            (Action::ReviewLoad(_, model), Phase::Choices(choices))
                if choices.load_selection.as_ref() == Some(&model)
                    && cli::allowed(&ClipboardPolicy::load(), false) =>
            {
                choices
                    .models
                    .prepare_load(&model)
                    .map(|load| Phase::LoadReview {
                        ticket: Arc::new(LoadTicket {
                            discovery: ticket.clone(),
                            model,
                        }),
                        choices: choices.clone(),
                        load: Arc::new(load),
                    })
            }
            (
                Action::Load(request),
                Phase::LoadReview {
                    ticket: current,
                    choices,
                    load,
                },
            ) if Arc::ptr_eq(&request, current) => {
                return self.run_cli_load(request, choices.clone(), Arc::clone(load));
            }
            (
                Action::BackFromLoad(request),
                Phase::LoadReview {
                    ticket: current,
                    choices,
                    ..
                },
            )
            | (
                Action::BackFromLoad(request),
                Phase::Loading {
                    ticket: current,
                    choices,
                    ..
                },
            ) if Arc::ptr_eq(&request, current) => Ok(Phase::Choices(choices.clone())),
            (
                Action::Loaded(request, result),
                Phase::Loading {
                    ticket: current,
                    choices,
                    ..
                },
            ) if Arc::ptr_eq(&request, current) => {
                if !cli::allowed(&ClipboardPolicy::load(), false) {
                    self.cancel_cli_discovery();
                    return Task::none();
                }
                let choices = choices.clone();
                self.set_status(result.map_or_else(
                    |error| error,
                    |()| format!("Loaded in LM Studio: {}", request.model),
                ));
                Ok(Phase::Choices(choices))
            }
            _ => return Task::none(),
        };
        match next {
            Ok(phase) => self.cli_discovery = Some(State { ticket, phase }),
            Err(error) => {
                self.cancel_cli_discovery();
                self.set_status(error);
            }
        }
        Task::none()
    }

    fn start_cli_discovery(
        &mut self,
        list: ModelList,
        editable: bool,
        root: Option<&Path>,
    ) -> Task<Message> {
        if !editable
            || self.cli_review.checking
            || self.cli_review.candidate.is_some()
            || self.cli_discovery_busy()
        {
            return Task::none();
        }
        let Some(root) = root.filter(|_| cli::allowed(&ClipboardPolicy::load(), false)) else {
            self.set_status("Enable AI features and finish the current request. Managed policy must allow local CLI use.");
            return Task::none();
        };
        let Some(profile) = self.selected() else {
            return Task::none();
        };
        let Some(path) = studio_path(profile) else {
            return Task::none();
        };
        let ticket = Arc::new(Ticket {
            profile_id: profile.id,
            path: path.into(),
            list,
            root: root.to_path_buf(),
        });
        let root = root.to_path_buf();
        let path = PathBuf::from(path);
        let reply = ticket.clone();
        match worker(
            move || cli::Discovery::inspect(&path, root, list).map(Arc::new),
            move |result| cli_message(Action::Inspected(reply.clone(), result)),
        ) {
            Ok(task) => {
                self.status = None;
                self.cli_discovery = Some(State {
                    ticket,
                    phase: Phase::Inspecting,
                });
                task
            }
            Err(error) => {
                self.set_status(error);
                Task::none()
            }
        }
    }

    fn run_cli_discovery(
        &mut self,
        ticket: Arc<Ticket>,
        discovery: Arc<cli::Discovery>,
    ) -> Task<Message> {
        if !cli::allowed(&ClipboardPolicy::load(), false) {
            self.cancel_cli_discovery();
            self.set_status("LM Studio model discovery is currently unavailable.");
            return Task::none();
        }
        let canceled = Arc::new(AtomicBool::new(false));
        let signal = canceled.clone();
        let reply = ticket.clone();
        match worker(
            move || discovery.run(&signal).map(Arc::new),
            move |result| cli_message(Action::Finished(reply.clone(), result)),
        ) {
            Ok(task) => {
                self.cli_discovery = Some(State {
                    ticket,
                    phase: Phase::Running {
                        _cancel: Cancel(canceled),
                    },
                });
                task
            }
            Err(error) => {
                self.cancel_cli_discovery();
                self.set_status(error);
                Task::none()
            }
        }
    }

    fn run_cli_load(
        &mut self,
        ticket: Arc<LoadTicket>,
        choices: Choices,
        load: Arc<cli::Load>,
    ) -> Task<Message> {
        if !cli::allowed(&ClipboardPolicy::load(), false) {
            self.cancel_cli_discovery();
            self.set_status("Loading an LM Studio model is currently unavailable.");
            return Task::none();
        }
        let canceled = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&canceled);
        let reply = Arc::clone(&ticket);
        match worker(
            move || load.run(&signal),
            move |result| cli_message(Action::Loaded(reply.clone(), result)),
        ) {
            Ok(task) => {
                self.status = None;
                self.cli_discovery = Some(State {
                    ticket: Arc::clone(&ticket.discovery),
                    phase: Phase::Loading {
                        ticket,
                        choices,
                        _cancel: Cancel(canceled),
                    },
                });
                task
            }
            Err(error) => {
                self.cancel_cli_discovery();
                self.set_status(error);
                Task::none()
            }
        }
    }

    pub(super) fn cli_discovery_view<'a>(
        &'a self,
        profile: &'a Profile,
        enabled: bool,
    ) -> Option<Element<'a, Message>> {
        let path = studio_path(profile)?;
        let state = self
            .cli_discovery
            .as_ref()
            .filter(|state| state.ticket.matches(profile));
        let actions = row![
            button("Review loaded models…")
                .style(button::secondary)
                .on_press_maybe(
                    (enabled && !path.is_empty())
                        .then_some(cli_message(Action::Start(ModelList::Loaded)))
                ),
            button("Review downloaded models…")
                .style(button::secondary)
                .on_press_maybe(
                    (enabled && !path.is_empty())
                        .then_some(cli_message(Action::Start(ModelList::Downloaded)))
                ),
        ]
        .spacing(8)
        .wrap();
        let Some(state) = state else {
            return Some(actions.into());
        };
        let cancel = || {
            button("Cancel").style(button::secondary).on_press_maybe(
                enabled.then_some(cli_message(Action::Cancel(state.ticket.clone()))),
            )
        };
        Some(match &state.phase {
            Phase::Inspecting => row![text("Checking the executable…"), cancel()].spacing(8).into(),
            Phase::Review(review) => column![
                text(review.command_review()).size(13),
                text("This command receives no prompt and runs with your operating system permissions. LM Studio may start its local service.").size(13),
                row![cancel(), button("Run this list command").on_press_maybe(
                    enabled.then_some(cli_message(Action::Run(state.ticket.clone())))
                )].spacing(8),
            ].spacing(8).into(),
            Phase::Running { .. } => row![text("Reading LM Studio models…"), cancel()].spacing(8).into(),
            Phase::LoadReview { ticket, load, .. } => column![
                text(format!("Load {} in LM Studio?", ticket.model)).size(14),
                text(load.command_review()).size(13).font(iced::Font::MONOSPACE),
                text("LM Studio will load the downloaded model into memory. The command receives no prompt and runs with your operating system permissions.").size(13),
                row![
                    button("Back").style(button::secondary).on_press(cli_message(Action::BackFromLoad(Arc::clone(ticket)))),
                    button("Load this model").on_press_maybe(enabled.then(|| cli_message(Action::Load(Arc::clone(ticket))))),
                ].spacing(8),
            ].spacing(8).into(),
            Phase::Loading { ticket, .. } => row![
                text(format!("Loading {} in LM Studio…", ticket.model)),
                button("Cancel").style(button::secondary).on_press(cli_message(Action::BackFromLoad(Arc::clone(ticket)))),
            ].spacing(8).into(),
            Phase::Choices(choices) => {
                let models = choices.models.keys();
                let label = match state.ticket.list {
                    ModelList::Loaded => "Choose a loaded chat model",
                    ModelList::Downloaded => "Choose a downloaded chat model",
                };
                let mut content = column![actions].spacing(8);
                if models.is_empty() {
                    content = content.push(text(match state.ticket.list {
                        ModelList::Loaded => "LM Studio returned no loaded chat models.",
                        ModelList::Downloaded => "LM Studio returned no downloaded chat models.",
                    }).size(13));
                } else if enabled {
                    let ticket = state.ticket.clone();
                    let selected = models.iter().find(|key| *key == &profile.model);
                    content = content.push(picker::escape_dismissible(
                        pick_list(models, selected, move |model| {
                            cli_message(Action::Choose(ticket.clone(), model))
                        }).placeholder(label),
                    ));
                    if profile.cli.as_ref().is_some_and(|settings| settings.has_preset(cli::LM_STUDIO_LOADED_STDIN)) {
                        content = content.push(text("Choosing a model switches to the explicit model preset.").size(13));
                    }
                    if state.ticket.list == ModelList::Downloaded {
                        let ticket = Arc::clone(&state.ticket);
                        let review = choices.load_selection.as_ref().map(|model| {
                            cli_message(Action::ReviewLoad(Arc::clone(&ticket), model.clone()))
                        });
                        content = content.push(row![
                            picker::escape_dismissible(pick_list(models, choices.load_selection.as_ref(), move |model| {
                                cli_message(Action::SelectLoad(Arc::clone(&ticket), model))
                            }).placeholder("Choose a model to load")),
                            button("Review model load…").on_press_maybe(review),
                        ].spacing(8).wrap());
                    }
                }
                content.into()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::profiles::{ConnectionMode, WizardAction};

    #[test]
    fn lm_studio_discovery_applies_only_current_list_choices_in_the_wizard() {
        let mut editor = Editor::default();
        editor.update_wizard(WizardAction::Open);
        editor.select_connection_mode(ConnectionMode::Cli);
        let _ = editor.update_cli(CliAction::LmStudioPreset { loaded_model: true }, true, None);
        let settings = editor.selected_mut().unwrap().cli.as_mut().unwrap();
        settings.path = "native lms fixture".into();
        settings.fingerprint = Some([1; 32]);
        let profile = editor.selected().unwrap();
        let profile_id = profile.id;
        let path = PathBuf::from(studio_path(profile).unwrap());
        let root = PathBuf::from("fixture root");
        let new_ticket = || {
            Arc::new(Ticket {
                profile_id,
                path: path.to_string_lossy().into_owned(),
                list: ModelList::Downloaded,
                root: root.clone(),
            })
        };
        let current = new_ticket();
        let stale = new_ticket();
        editor.cli_discovery = Some(State {
            ticket: current.clone(),
            phase: Phase::Choices(Choices {
                models: Arc::new(cli::Discovered::fixture(
                    vec!["openai/gpt-oss-20b".into()],
                    path,
                    root.clone(),
                    ModelList::Downloaded,
                )),
                load_selection: None,
            }),
        });
        for (ticket, model) in [
            (stale, "openai/gpt-oss-20b"),
            (current.clone(), "unexpected-model"),
        ] {
            let _ = editor.update_cli_discovery(
                Action::Choose(ticket, model.into()),
                true,
                Some(&root),
            );
            let profile = editor.selected().unwrap();
            assert!(profile.model.is_empty());
            assert_eq!(profile.cli.as_ref().unwrap().fingerprint, Some([1; 32]));
        }
        let _ = editor.update_cli_discovery(
            Action::Choose(current, "openai/gpt-oss-20b".into()),
            true,
            Some(&root),
        );
        let profile = editor.selected().unwrap();
        let settings = profile.cli.as_ref().unwrap();
        assert_eq!(profile.model, "openai/gpt-oss-20b");
        assert!(settings.has_preset(cli::LM_STUDIO_STDIN));
        assert_eq!(settings.fingerprint, None);
        editor.update_wizard(WizardAction::Cancel);
        assert!(editor.cli_discovery.is_none());
    }

    #[test]
    fn lm_studio_load_preview_preserves_the_profile_and_rejects_stale_actions() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let path = root.join(format!("lms{}", std::env::consts::EXE_SUFFIX));
        let model = "openai/gpt-oss-20b";
        let mut editor = Editor::default();
        editor.update_wizard(WizardAction::Open);
        editor.select_connection_mode(ConnectionMode::Cli);
        let _ = editor.update_cli(CliAction::LmStudioPreset { loaded_model: true }, true, None);
        let settings = editor.selected_mut().unwrap().cli.as_mut().unwrap();
        settings.path = path.to_str().unwrap().into();
        settings.fingerprint = Some([1; 32]);
        let ticket = Arc::new(Ticket {
            profile_id: editor.selected().unwrap().id,
            path: path.to_str().unwrap().into(),
            list: ModelList::Downloaded,
            root: root.clone(),
        });
        let choices = Choices {
            models: Arc::new(cli::Discovered::fixture(
                vec![model.into()],
                path,
                root.clone(),
                ModelList::Downloaded,
            )),
            load_selection: Some(model.into()),
        };
        let review = |editor: &mut Editor| {
            let _ = editor.update_cli_discovery(
                Action::ReviewLoad(Arc::clone(&ticket), model.into()),
                true,
                Some(&root),
            );
            let Phase::LoadReview { ticket, .. } = &editor.cli_discovery.as_ref().unwrap().phase
            else {
                panic!("expected load review");
            };
            Arc::clone(ticket)
        };
        editor.cli_discovery = Some(State {
            ticket: Arc::clone(&ticket),
            phase: Phase::Choices(choices.clone()),
        });
        let stale = review(&mut editor);
        let _ = editor.update_cli_discovery(
            Action::BackFromLoad(Arc::clone(&stale)),
            true,
            Some(&root),
        );
        let current = review(&mut editor);
        let _ = editor.update_cli_discovery(Action::Load(Arc::clone(&stale)), true, Some(&root));
        assert!(matches!(&editor.cli_discovery.as_ref().unwrap().phase,
            Phase::LoadReview { ticket, .. } if Arc::ptr_eq(ticket, &current)));

        let canceled = Arc::new(AtomicBool::new(false));
        editor.cli_discovery = Some(State {
            ticket: Arc::clone(&ticket),
            phase: Phase::Loading {
                ticket: Arc::clone(&current),
                choices,
                _cancel: Cancel(Arc::clone(&canceled)),
            },
        });
        let _ = editor.update_cli_discovery(Action::Loaded(stale, Ok(())), true, Some(&root));
        assert!(!canceled.load(Ordering::Acquire));
        let _ = editor.update_cli_discovery(
            Action::BackFromLoad(Arc::clone(&current)),
            true,
            Some(&root),
        );
        assert!(canceled.load(Ordering::Acquire));
        let _ = editor.update_cli_discovery(Action::Loaded(current, Ok(())), true, Some(&root));
        assert!(matches!(
            &editor.cli_discovery.as_ref().unwrap().phase,
            Phase::Choices(_)
        ));
        let profile = editor.selected().unwrap();
        let settings = profile.cli.as_ref().unwrap();
        assert!(profile.model.is_empty());
        assert!(settings.has_preset(cli::LM_STUDIO_LOADED_STDIN));
        assert_eq!(settings.fingerprint, Some([1; 32]));
    }
}
