//! Local managed model actions in the AI Manager.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use iced::{Element, Length, Task};

use crate::local_ai::managed_models::{self, Model, ModelBackend, Purpose, Registry};
use crate::local_ai::runtime_index::PackageBackend;
use crate::local_ai::runtime_package::{self, ModelStatus};

use super::Message::Managed;
use super::{BackendRepair, Dialog, Message as LocalMessage, Output, field, limited};
use crate::ai_manager::profiles::picker::DismissOnEscape as _;

mod preference;
pub(in crate::ai_manager) use preference::Change as DefaultChange;
pub(crate) use preference::SavedDefault;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Choice {
    model: Model,
    is_default: bool,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_default {
            formatter.write_str("★ ")?;
        }
        write!(formatter, "{} · {}", self.model.name, self.model.purpose)?;
        if !self.model.shard_bytes.is_empty() {
            write!(formatter, " · {} shards", self.model.shard_bytes.len())?;
        }
        if self.model.external.is_some() {
            formatter.write_str(" · Reference")?;
        }
        if self.model.projector_bytes.is_some() {
            formatter.write_str(" · Vision")?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) enum Message {
    Source(String),
    ProjectorPath(String),
    ImportPurpose(Purpose),
    Select(Box<Choice>),
    Refresh,
    SetDefault,
    ClearDefault,
    Import,
    Reference,
    Start,
    Stop,
    Test,
    Configure,
    RepairBackend,
    Name(String),
    Backend(ModelBackend),
    Context(String),
    CpuThreads(String),
    GpuLayers(String),
    ConfigurePurpose(Purpose),
    IdleMinutes(String),
    NeverUnload(bool),
    SaveConfiguration,
    AskRemoval,
    Remove,
    CancelAction,
}

impl Message {
    pub(super) fn requires_ai(&self) -> bool {
        !matches!(
            self,
            Self::Select(_) | Self::Refresh | Self::Stop | Self::SetDefault | Self::ClearDefault
        )
    }
}

enum Action {
    Configure(Configuration),
    ConfirmRemoval,
}

struct Configuration {
    name: String,
    backend: ModelBackend,
    context: String,
    cpu_threads: String,
    gpu_layers: String,
    purpose: Purpose,
    idle_minutes: String,
    never_unload: bool,
}

impl Configuration {
    fn new(model: &Model) -> Self {
        let idle_minutes = if model.idle_timeout_minutes == 0 {
            managed_models::DEFAULT_IDLE_TIMEOUT_MINUTES
        } else {
            model.idle_timeout_minutes
        };
        Self {
            name: model.name.clone(),
            backend: model.backend,
            context: model.context_tokens.to_string(),
            cpu_threads: model.cpu_threads.to_string(),
            gpu_layers: model.gpu_layers.to_string(),
            purpose: model.purpose,
            idle_minutes: idle_minutes.to_string(),
            never_unload: model.idle_timeout_minutes == 0,
        }
    }

    fn settings(&self, model: &Model) -> Result<Model, String> {
        let context_tokens = self
            .context
            .trim()
            .parse()
            .map_err(|_| "Enter a whole number for the context size.")?;
        let cpu_threads = self
            .cpu_threads
            .trim()
            .parse()
            .map_err(|_| "Enter a whole number for CPU threads.")?;
        let gpu_layers = self
            .gpu_layers
            .trim()
            .parse()
            .map_err(|_| "Enter a whole number for GPU layers.")?;
        let idle_timeout_minutes = if self.never_unload {
            0
        } else {
            self.idle_minutes
                .trim()
                .parse()
                .ok()
                .filter(|&minutes| minutes > 0)
                .ok_or("Enter a positive number of idle minutes or choose Never.")?
        };
        let edited = Model {
            name: self.name.trim().into(),
            backend: self.backend,
            context_tokens,
            cpu_threads,
            gpu_layers,
            idle_timeout_minutes,
            purpose: self.purpose,
            ..model.clone()
        };
        edited
            .validate_settings()
            .map_err(|error| error.to_string())?;
        Ok(edited)
    }

    fn view(
        &self,
        enabled: bool,
        active_backend: Option<PackageBackend>,
    ) -> Element<'_, LocalMessage> {
        column![
            text("Configure managed model").size(16),
            field(
                "Model name",
                text_input("Name", &self.name)
                    .on_input_maybe(enabled.then_some(|name| Managed(Message::Name(name)))),
            ),
            field(
                "Backend",
                backend_picker(self.backend, active_backend, enabled)
            ),
            unavailable_backend(self.backend, active_backend).map(|reason| text(reason).size(13)),
            field(
                "Purpose",
                purpose_picker(self.purpose, enabled, Message::ConfigurePurpose),
            ),
            field(
                "Context size (tokens)",
                text_input("512–2097152", &self.context).on_input_maybe(
                    enabled.then_some(|context| Managed(Message::Context(context)))
                ),
            ),
            field(
                "CPU threads (0 = auto)",
                text_input("0 = automatic", &self.cpu_threads).on_input_maybe(
                    enabled.then_some(|threads| Managed(Message::CpuThreads(threads)))
                ),
            ),
            field(
                "GPU layers (-1 = auto)",
                text_input("-1 = automatic", &self.gpu_layers).on_input_maybe(
                    enabled.then_some(|layers| Managed(Message::GpuLayers(layers)))
                ),
            ),
            field(
                "Idle timeout (minutes)",
                text_input("1–1440", &self.idle_minutes).on_input_maybe(
                    (enabled && !self.never_unload)
                        .then_some(|minutes| Managed(Message::IdleMinutes(minutes))),
                ),
            ),
            checkbox(self.never_unload)
                .label("Never unload while idle")
                .on_toggle_maybe(enabled.then_some(|value| Managed(Message::NeverUnload(value)))),
            row![
                button("Save model settings")
                    .on_press_maybe(enabled.then_some(Managed(Message::SaveConfiguration))),
                button("Cancel").on_press_maybe(enabled.then_some(Managed(Message::CancelAction))),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(8)
        .into()
    }
}

#[derive(Default)]
pub(super) struct Models {
    source: String,
    projector_path: String,
    import_purpose: Purpose,
    choices: Vec<Choice>,
    selected: Option<Choice>,
    default: Option<SavedDefault>,
    active_backend: Option<PackageBackend>,
    status: Option<ModelStatus>,
    action: Option<Action>,
}

impl Models {
    pub(super) fn add_model(&mut self, model: Model, profile: &Path) {
        let id = model.id.clone();
        self.choices.push(Choice {
            model,
            is_default: false,
        });
        self.choices.sort_by(|left, right| {
            left.model
                .name
                .cmp(&right.model.name)
                .then(left.model.id.cmp(&right.model.id))
        });
        self.dismiss_action();
        self.return_to_model(&id, profile);
    }

    pub(super) fn set_models(
        &mut self,
        models: Vec<Model>,
        default: Option<SavedDefault>,
        legacy: Option<&str>,
        profile: &Path,
    ) {
        let id = default.as_ref().and_then(|value| value.resolve(legacy));
        self.choices = models
            .into_iter()
            .map(|model| Choice {
                is_default: id == Some(model.id.as_str()),
                model,
            })
            .collect();
        self.default = default;
        self.selected = self
            .selected
            .as_ref()
            .and_then(|selected| {
                self.choices
                    .iter()
                    .find(|choice| choice.model.id == selected.model.id)
            })
            .or_else(|| self.choices.iter().find(|choice| choice.is_default))
            .cloned();
        self.dismiss_action();
        self.update_status(profile);
    }

    pub(super) fn default_change(&self, mark: bool, legacy: Option<&str>) -> Option<DefaultChange> {
        let current = self.default.as_ref()?;
        let model_id = if mark {
            Some(self.selected.as_ref()?.model.id.clone())
        } else {
            None
        };
        (current.resolve(legacy) != model_id.as_deref()).then(|| DefaultChange {
            previous: current.clone(),
            model_id,
        })
    }

    pub(super) fn set_default(&mut self, id: Option<String>) {
        for choice in self.choices.iter_mut().chain(self.selected.as_mut()) {
            choice.is_default = id.as_deref() == Some(choice.model.id.as_str());
        }
        self.default = Some(SavedDefault::Stored(id));
    }

    pub(super) fn set_active_backend(&mut self, backend: Option<PackageBackend>) {
        self.active_backend = backend;
    }

    fn backend_repair(&self) -> Option<(&Model, PackageBackend)> {
        let model = &self.selected.as_ref()?.model;
        if self.has_action()
            || !model.backend.supported()
            || model.backend.available(self.active_backend)
        {
            return None;
        }
        Some((model, model.backend.required_package()?))
    }

    pub(super) fn return_to_model(&mut self, id: &str, profile: &Path) {
        self.selected = self
            .choices
            .iter()
            .find(|choice| choice.model.id == id)
            .cloned();
        self.update_status(profile);
    }

    pub(super) fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

    pub(super) fn embedding_models(&self) -> impl Iterator<Item = &Model> {
        self.choices
            .iter()
            .map(|choice| &choice.model)
            .filter(|model| model.purpose == Purpose::Embedding)
    }

    pub(super) fn has_action(&self) -> bool {
        self.action.is_some()
    }

    pub(super) fn dismiss_action(&mut self) {
        self.action = None;
    }

    pub(super) fn update_status(&mut self, profile: &Path) {
        self.status = self
            .selected
            .as_ref()
            .map(|choice| runtime_package::model_status(profile, &choice.model.id));
    }

    pub(super) fn view(
        &self,
        editable: bool,
        ai_allowed: bool,
        legacy: Option<&str>,
    ) -> Element<'_, LocalMessage> {
        let models = pick_list(self.choices.as_slice(), self.selected.as_ref(), |choice| {
            Managed(Message::Select(Box::new(choice)))
        })
        .placeholder("Select a managed GGUF model")
        .width(Length::Fill)
        .dismiss_on_escape();
        let enabled = ai_allowed && !self.has_action();
        let can_mark = editable && !self.has_action() && self.default.is_some();
        let has_default = self
            .default
            .as_ref()
            .and_then(|value| value.resolve(legacy))
            .is_some();
        let status = self.status.as_ref().unwrap_or(&ModelStatus::Stopped);
        let startable = matches!(status, ModelStatus::Stopped | ModelStatus::Failed);
        let idle = !matches!(status, ModelStatus::Starting | ModelStatus::Busy);
        let action = self
            .selected
            .as_ref()
            .and_then(|Choice { model, .. }| match &self.action {
                Some(Action::Configure(configuration)) => {
                    Some(configuration.view(ai_allowed, self.active_backend))
                }
                Some(Action::ConfirmRemoval) => Some(
                    column![
                        text(if model.external.is_some() {
                            format!("Remove the external registration for {}?", model.name)
                        } else {
                            format!("Remove the managed copy of {}?", model.name)
                        })
                        .wrapping(text::Wrapping::WordOrGlyph),
                        row![
                            button("Cancel").on_press_maybe(
                                ai_allowed.then_some(Managed(Message::CancelAction))
                            ),
                            button(if model.external.is_some() {
                                "Remove registration"
                            } else {
                                "Remove managed copy"
                            })
                            .style(button::danger)
                            .on_press_maybe(ai_allowed.then_some(Managed(Message::Remove))),
                        ]
                        .spacing(8)
                        .wrap(),
                    ]
                    .spacing(8)
                    .into(),
                ),
                None => None,
            });
        column![
            text("Managed GGUF Models").size(17),
            text("Import a private copy or register GGUF files in place for the signed llama.cpp runtime. For a split model, select any numbered shard with all sibling shards in the same folder.").size(13),
            text(self.active_backend.map_or_else(
                || "Active signed package backend · Unavailable".into(),
                |backend| format!("Active signed package backend · {backend}"),
            )).size(13),
            field(
                "GGUF model path",
                text_input("Absolute path to the local GGUF", &self.source)
                    .on_input_maybe(enabled.then_some(|path| Managed(Message::Source(path)))),
            ),
            field(
                "Vision projector path (optional)",
                text_input("Absolute path to a compatible GGUF projector", &self.projector_path)
                    .on_input_maybe(enabled.then_some(|path| Managed(Message::ProjectorPath(path)))),
            ),
            field(
                "Import purpose",
                purpose_picker(self.import_purpose, enabled, Message::ImportPurpose),
            ),
            row![
                button("Import private copy").on_press_maybe(
                    (enabled && !self.source.trim().is_empty()
                        && (self.projector_path.trim().is_empty() || self.import_purpose == Purpose::Chat))
                        .then_some(Managed(Message::Import)),
                ),
                button("Register external GGUF").on_press_maybe(
                    (enabled && !self.source.trim().is_empty() && self.projector_path.trim().is_empty())
                        .then_some(Managed(Message::Reference)),
                ),
                button("Refresh managed models").on_press_maybe(
                    (editable && !self.has_action()).then_some(Managed(Message::Refresh)),
                ),
            ]
            .spacing(8)
            .wrap(),
            (!self.projector_path.trim().is_empty()).then(|| text(
                "Vision pairs use the Chat purpose. Import both components as a private copy."
            ).size(13)),
            models,
            row![
                button("Set as default").on_press_maybe(
                    (can_mark && self.selected.as_ref().is_some_and(|choice| !choice.is_default))
                        .then_some(Managed(Message::SetDefault)),
                ),
                button("Clear default").on_press_maybe(
                    (can_mark && has_default).then_some(Managed(Message::ClearDefault)),
                ),
            ]
            .spacing(8)
            .wrap(),
            (editable && has_default && !self.choices.iter().any(|choice| choice.is_default))
                .then(|| text("The default managed model is unavailable. Select another model or clear the default.").size(13)),
            self.selected.as_ref().map(|Choice { model, .. }| {
                let idle_timeout = match model.idle_timeout_minutes {
                    0 => "Never unload while idle".into(),
                    minutes => format!("Unload after {minutes} min idle"),
                };
                column![
                    text(format!("Runtime status · {status}")),
                    model.external.as_ref().map(|reference| {
                        let source = if model.shard_bytes.is_empty() { "External file" } else { "First external shard" };
                        text(format!("{source} · {}", reference.path().display()))
                            .size(13).wrapping(text::Wrapping::WordOrGlyph)
                    }),
                    model.projector_bytes.map(|bytes| text(format!("Vision projector · {bytes} bytes")).size(13)),
                    text(format!(
                        "llama.cpp · {} · {} backend · {} bytes · {} context tokens · {idle_timeout}",
                        model.purpose, model.backend, model.bytes, model.context_tokens
                    )).size(13),
                    unavailable_backend(model.backend, self.active_backend).map(|reason| text(reason).size(13)),
                    self.backend_repair().map(|_| button("Install matching runtime…")
                        .style(button::secondary)
                        .on_press_maybe(enabled.then_some(Managed(Message::RepairBackend)))),
                    row![
                        button("Start model")
                            .on_press_maybe((enabled && startable && model.backend.available(self.active_backend))
                                .then_some(Managed(Message::Start))),
                        button("Stop model").on_press_maybe(
                            (editable && !self.has_action() && idle && !matches!(status, ModelStatus::Stopped))
                                .then_some(Managed(Message::Stop)),
                        ),
                        button("Test model")
                            .on_press_maybe((enabled && idle && model.backend.available(self.active_backend))
                                .then_some(Managed(Message::Test))),
                        button("Configure model")
                            .on_press_maybe((enabled && idle).then_some(Managed(Message::Configure))),
                        button("Remove model")
                            .on_press_maybe((enabled && idle).then_some(Managed(Message::AskRemoval))),
                    ]
                    .spacing(8)
                    .wrap(),
                ].spacing(8)
            }),
            action,
        ]
        .spacing(8)
        .width(Length::Fill)
        .into()
    }
}

impl Dialog {
    pub(super) fn update_managed(&mut self, message: Message) -> Task<LocalMessage> {
        match message {
            Message::Source(path) => self.managed.source = limited(&path, 4096).into(),
            Message::ProjectorPath(path) => {
                self.managed.projector_path = limited(&path, 4096).into()
            }
            Message::ImportPurpose(purpose) => self.managed.import_purpose = purpose,
            Message::Select(choice) if self.managed.choices.contains(choice.as_ref()) => {
                if self.managed.selected.as_ref() != Some(choice.as_ref()) {
                    self.managed.selected = Some(*choice);
                    self.managed.dismiss_action();
                    self.managed.update_status(&self.profile_dir);
                }
            }
            Message::Refresh if !self.managed.has_action() => {
                return self.manage_models("Reading managed models…", false, |_, _, _| Ok(None));
            }
            Message::Import | Message::Reference if !self.managed.source.trim().is_empty() => {
                let path = PathBuf::from(self.managed.source.trim());
                let purpose = self.managed.import_purpose;
                let reference = matches!(message, Message::Reference);
                let projector = self.managed.projector_path.trim();
                if !projector.is_empty() && (reference || purpose != Purpose::Chat) {
                    return Task::none();
                }
                let projector = (!projector.is_empty()).then(|| PathBuf::from(projector));
                let status = if reference {
                    "Registering the external GGUF…"
                } else {
                    "Importing the managed GGUF model…"
                };
                return self.manage_models(status, true, move |_, registry, cancelled| {
                    let model = if reference {
                        registry.reference(&path, purpose, cancelled)
                    } else if let Some(projector) = projector {
                        registry.import_projector_pair(&path, &projector, cancelled)
                    } else {
                        registry.import(&path, purpose, cancelled)
                    };
                    model
                        .map(|model| {
                            Some(if reference {
                                format!("Registered {}.", model.name)
                            } else {
                                format!("Imported {}.", model.name)
                            })
                        })
                        .map_err(|error| format!("Could not import the managed model ({error})"))
                });
            }
            Message::Start | Message::Stop => {
                if let Some(Choice { model, .. }) = self.managed.selected.clone() {
                    let profile = self.profile_dir.clone();
                    let start = matches!(message, Message::Start);
                    let status = if start {
                        "Starting the managed llama.cpp model…"
                    } else {
                        "Stopping the managed llama.cpp model…"
                    };
                    return self.run_managed(
                        runtime_package::work(move |cancelled| {
                            if start {
                                runtime_package::start_model(&profile, &model.id, cancelled)?;
                                Ok(format!("Started {}.", model.name))
                            } else {
                                check_cancelled(cancelled).map_err(|error| error.to_string())?;
                                runtime_package::stop_model(&profile, &model.id).map_err(
                                    |error| format!("Could not stop the managed model ({error})"),
                                )?;
                                Ok(format!("Stopped {}.", model.name))
                            }
                        }),
                        status,
                        start,
                        Output::ManagedNotice,
                    );
                }
            }
            Message::Test => {
                if let Some(Choice { model, .. }) = &self.managed.selected {
                    return self.run_managed(
                        runtime_package::test_model(self.profile_dir.clone(), model.id.clone()),
                        "Starting the managed llama.cpp model…",
                        true,
                        |name| {
                            Output::ManagedNotice(format!(
                                "{name} replied through the managed runtime."
                            ))
                        },
                    );
                }
            }
            Message::Configure => {
                if let Some(Choice { model, .. }) = &self.managed.selected {
                    self.managed.action = Some(Action::Configure(Configuration::new(model)));
                }
            }
            Message::RepairBackend => {
                if let Some((model, required)) = self.managed.backend_repair() {
                    self.runtime_removal = None;
                    self.backend_repair = Some(BackendRepair {
                        model_id: model.id.clone(),
                        model_name: model.name.clone(),
                        required,
                        paths: Default::default(),
                        installed: false,
                    });
                    self.status = None;
                }
            }
            Message::Name(value) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.name = limited(&value, 800).into();
                }
            }
            Message::Backend(backend) if backend.available(self.managed.active_backend) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.backend = backend;
                }
            }
            Message::Context(value) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.context = limited(&value, 10).into();
                }
            }
            Message::CpuThreads(value) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.cpu_threads = limited(&value, 10).into();
                }
            }
            Message::GpuLayers(value) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.gpu_layers = limited(&value, 10).into();
                }
            }
            Message::ConfigurePurpose(purpose) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.purpose = purpose;
                }
            }
            Message::IdleMinutes(value) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.idle_minutes = limited(&value, 10).into();
                }
            }
            Message::NeverUnload(value) => {
                if let Some(Action::Configure(draft)) = &mut self.managed.action {
                    draft.never_unload = value;
                }
            }
            Message::SaveConfiguration => {
                if let (Some(Action::Configure(draft)), Some(Choice { model, .. })) =
                    (&self.managed.action, &self.managed.selected)
                {
                    match draft.settings(model) {
                        Ok(model) => {
                            return self.modify_model(
                                "Saving managed model settings…",
                                move |registry, _| {
                                    let updated = registry.configure(model)?;
                                    Ok(format!("Saved settings for {}.", updated.name))
                                },
                            );
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::AskRemoval if self.managed.has_selection() => {
                self.managed.action = Some(Action::ConfirmRemoval);
            }
            Message::Remove if matches!(self.managed.action, Some(Action::ConfirmRemoval)) => {
                return self.modify_model(
                    "Removing the model registration…",
                    |registry, model| {
                        registry.remove(&model.id)?;
                        Ok(if model.external.is_some() {
                            format!("Removed the external registration for {}.", model.name)
                        } else {
                            format!("Removed the managed copy of {}.", model.name)
                        })
                    },
                );
            }
            Message::CancelAction => self.managed.dismiss_action(),
            _ => {}
        }
        Task::none()
    }

    fn modify_model(
        &mut self,
        status: &'static str,
        update: impl FnOnce(&Registry, &Model) -> io::Result<String> + Send + 'static,
    ) -> Task<LocalMessage> {
        let Some(Choice { model, .. }) = self.managed.selected.clone() else {
            return Task::none();
        };
        self.manage_models(status, true, move |profile, registry, cancelled| {
            runtime_package::with_stopped_model(profile, &model.id, || {
                check_cancelled(cancelled)?;
                update(registry, &model)
            })
            .map(Some)
            .map_err(|error| format!("Could not change the managed model ({error})"))
        })
    }

    fn manage_models(
        &mut self,
        status: &'static str,
        requires_ai: bool,
        update: impl FnOnce(&Path, &Registry, &AtomicBool) -> Result<Option<String>, String>
        + Send
        + 'static,
    ) -> Task<LocalMessage> {
        let profile = self.profile_dir.clone();
        let registry = Registry::new(profile.clone());
        self.run_managed(
            runtime_package::work(move |cancelled| {
                check_cancelled(cancelled).map_err(|error| error.to_string())?;
                let notice = update(&profile, &registry, cancelled)?;
                let models = registry
                    .list()
                    .map_err(|error| format!("Could not list managed models ({error})"))?;
                let default = preference::load(&profile).map_err(|error| error.to_string());
                let notice = notice.unwrap_or_else(|| format!("{} managed models", models.len()));
                let active_backend = runtime_package::active_package_backend(&profile)
                    .ok()
                    .flatten();
                Ok(Output::ManagedModels {
                    models,
                    default,
                    active_backend,
                    notice,
                })
            }),
            status,
            requires_ai,
            |output| output,
        )
    }
}

fn backend_picker(
    selected: ModelBackend,
    active: Option<PackageBackend>,
    enabled: bool,
) -> Element<'static, LocalMessage> {
    if !enabled {
        return text(selected.to_string()).into();
    }
    let options: Vec<_> = ModelBackend::ALL
        .into_iter()
        .filter(|backend| backend.available(active))
        .collect();
    pick_list(options, Some(selected), |backend| {
        Managed(Message::Backend(backend))
    })
    .dismiss_on_escape()
}

fn unavailable_backend(backend: ModelBackend, active: Option<PackageBackend>) -> Option<String> {
    if backend.available(active) {
        return None;
    }
    Some(if backend.supported() {
        format!(
            "{backend} is unavailable. Install a matching signed runtime package or choose Auto or CPU."
        )
    } else {
        format!("{backend} is unavailable on this platform. Choose Auto or CPU.")
    })
}

fn purpose_picker(
    purpose: Purpose,
    editable: bool,
    change: fn(Purpose) -> Message,
) -> Element<'static, LocalMessage> {
    if editable {
        pick_list(Purpose::ALL, Some(purpose), move |value| {
            Managed(change(value))
        })
        .dismiss_on_escape()
    } else {
        text(purpose.to_string()).into()
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Managed model operation cancelled.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::{StreamExt, executor::block_on};

    use crate::ai_manager::{
        Configuration, Dialog as Manager, FILE_NAME, Message as ManagerMessage, Page,
    };
    use crate::private_files::Directory;
    use std::sync::Arc;

    fn model(id: &str, name: &str, purpose: Purpose) -> Model {
        Model {
            id: id.into(),
            name: name.into(),
            bytes: 1024,
            backend: ModelBackend::Auto,
            context_tokens: 4096,
            cpu_threads: 0,
            gpu_layers: managed_models::AUTO_GPU_LAYERS,
            purpose,
            idle_timeout_minutes: 10,
            shard_bytes: Vec::new(),
            external: None,
            projector_bytes: None,
        }
    }

    fn loaded(models: Vec<Model>, root: &Path) -> ManagerMessage {
        ManagerMessage::Local(LocalMessage::Finished(
            0,
            Ok(Output::ManagedModels {
                models,
                default: preference::load(root).map_err(|error| error.to_string()),
                active_backend: None,
                notice: "Models loaded.".into(),
            }),
        ))
    }

    fn change_default(manager: &mut Manager, action: Message) {
        let task = manager.update(ManagerMessage::Local(Managed(action)));
        assert!(manager.busy());
        assert!(!manager.can_write_skill_rule());
        assert!(iced_runtime::task::into_stream(manager.update(ManagerMessage::Save)).is_none());
        let mut outcomes = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
            panic!("expected the marker result");
        };
        assert!(matches!(event, ManagerMessage::ManagedDefaultSaved(Ok(_))));
        let _ = manager.update(event);
        assert!(!manager.busy());
    }

    #[test]
    fn manager_registers_and_removes_an_external_split_model_from_any_shard() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let directory = Directory::open(&root, false).unwrap();
        let first = "fixture-00001-of-00002.gguf";
        let second = "fixture-00002-of-00002.gguf";
        let first_bytes = managed_models::split_test_fixture(0, 2, 2, false);
        let second_bytes = managed_models::split_test_fixture(1, 2, 2, false);
        directory.publish_copy(first, &first_bytes).unwrap();
        directory.publish_copy(second, &second_bytes).unwrap();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(ManagerMessage::Local(Managed(Message::Source(
            root.join(second).to_str().unwrap().into(),
        ))));
        let complete = |manager: &mut Manager, action: Message| {
            let task = manager.update(ManagerMessage::Local(Managed(action)));
            assert!(manager.local.busy(), "{:?}", manager.local.status);
            let mut outcomes = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
                panic!("expected the model operation result");
            };
            let _ = manager.update(event);
            assert!(!manager.local.busy());
        };
        complete(&mut manager, Message::Reference);
        assert_eq!(manager.local.status.as_deref(), Some("Registered fixture."));
        let choice = manager.local.managed.choices[0].clone();
        assert_eq!(choice.to_string(), "fixture · Chat · 2 shards · Reference");
        assert_eq!(
            choice.model.external.as_ref().unwrap().path(),
            root.join(first)
        );
        let _ = manager.update(ManagerMessage::Local(Managed(Message::Select(Box::new(
            choice,
        )))));
        let _ = manager.update(ManagerMessage::Local(Managed(Message::AskRemoval)));
        complete(&mut manager, Message::Remove);
        assert_eq!(
            manager.local.status.as_deref(),
            Some("Removed the external registration for fixture.")
        );
        assert!(manager.local.managed.choices.is_empty());
        assert_eq!(std::fs::read(root.join(first)).unwrap(), first_bytes);
        assert_eq!(std::fs::read(root.join(second)).unwrap(), second_bytes);
    }

    #[test]
    fn runtime_removal_confirmation_can_be_cancelled_and_keeps_the_model_selection() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let runtime = Directory::open(&root, false)
            .unwrap()
            .private_child("llama-runtime", true)
            .unwrap();
        runtime
            .publish_copy("activation.json", b"fixture activation")
            .unwrap();
        let plan = runtime_package::inspect_removal(&root).unwrap().unwrap();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(loaded(
            vec![model(
                "0123456789abcdef0123456789abcdef",
                "Retained model",
                Purpose::Chat,
            )],
            &root,
        ));
        let selected = manager.local.managed.choices[0].clone();
        let _ = manager.update(ManagerMessage::Local(Managed(Message::Select(Box::new(
            selected.clone(),
        )))));
        manager
            .local
            .managed
            .set_active_backend(Some(PackageBackend::Cpu));

        let event = |event| ManagerMessage::Local(event);
        let reviewed = || {
            event(LocalMessage::Finished(
                0,
                Ok(Output::RuntimeRemovalReviewed(Some(plan.clone()))),
            ))
        };
        let _ = manager.update(reviewed());
        assert!(manager.local.runtime_removal.is_some());
        let _ = manager.update(event(LocalMessage::CancelRuntimeRemoval));
        assert!(manager.local.runtime_removal.is_none());
        assert!(
            iced_runtime::task::into_stream(
                manager.update(event(LocalMessage::ConfirmRuntimeRemoval))
            )
            .is_none()
        );
        assert!(runtime.has_entry("activation.json").unwrap());
        assert_eq!(
            manager.local.managed.active_backend,
            Some(PackageBackend::Cpu)
        );

        let _ = manager.update(reviewed());
        let _ = manager.update(event(LocalMessage::Finished(
            0,
            Ok(Output::RuntimeRemoved(None)),
        )));
        assert!(manager.local.runtime_removal.is_none());
        assert_eq!(manager.local.managed.active_backend, None);
        assert_eq!(manager.local.managed.selected.as_ref(), Some(&selected));
        assert_eq!(
            manager.local.status.as_deref(),
            Some("Removed the installed llama.cpp runtime.")
        );
    }

    #[test]
    fn guided_backend_repair_keeps_model_and_separates_install_paths() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let mut gpu = model(
            "0123456789abcdef0123456789abcdef",
            "Fixture GPU model",
            Purpose::Chat,
        );
        gpu.backend = if cfg!(target_os = "macos") {
            ModelBackend::Metal
        } else {
            ModelBackend::Vulkan
        };
        if !gpu.backend.supported() {
            return;
        }
        let required = gpu.backend.required_package().unwrap();
        let other = model(
            "fedcba9876543210fedcba9876543210",
            "Other model",
            Purpose::Chat,
        );
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(loaded(vec![gpu.clone(), other], &root));
        let selected = manager.local.managed.choices[0].clone();
        let _ = manager.update(ManagerMessage::Local(Managed(Message::Select(Box::new(
            selected,
        )))));
        let path = |file, value: &str| {
            ManagerMessage::Local(LocalMessage::RuntimePath(file, value.into()))
        };
        let _ = manager.update(path(super::super::RuntimeFile::Archive, "generic.zip"));
        let _ = manager.update(ManagerMessage::Local(Managed(Message::RepairBackend)));
        assert!(manager.local.backend_repair_open());
        let _ = manager.update(ManagerMessage::Page(Page::Knowledge));
        assert_eq!(manager.page, Page::LocalModels);
        assert!(!manager.local.runtime_paths_ready());
        assert_eq!(
            manager.local.runtime_import().expected_backend,
            Some(required)
        );
        let second = manager.local.managed.choices[1].clone();
        let _ = manager.update(ManagerMessage::Local(Managed(Message::Select(Box::new(
            second,
        )))));
        assert_eq!(manager.local.managed.selected.as_ref().unwrap().model, gpu);
        assert!(matches!(
            manager.escape_message(),
            ManagerMessage::Local(LocalMessage::CloseBackendRepair)
        ));
        let escape = manager.escape_message();
        let _ = manager.update(escape);
        assert!(!manager.local.backend_repair_open());
        assert_eq!(
            manager.local.runtime_import().archive,
            PathBuf::from("generic.zip")
        );
        assert_eq!(manager.local.runtime_import().expected_backend, None);

        let _ = manager.update(ManagerMessage::Local(Managed(Message::RepairBackend)));
        use super::super::RuntimeFile::{Archive, Index, Signature};
        for (file, value) in [
            (Index, "index.json"),
            (Signature, "index.sig"),
            (Archive, "repair.zip"),
        ] {
            let _ = manager.update(path(file, value));
        }
        assert!(manager.local.runtime_paths_ready());
        assert_eq!(
            manager.local.runtime_import().archive,
            PathBuf::from("repair.zip")
        );
        for backend in [None, Some(PackageBackend::Cpu)] {
            let _ = manager.update(ManagerMessage::Local(LocalMessage::Finished(
                0,
                Ok(Output::RuntimeActivated(
                    "installed fixture package".into(),
                    backend,
                    false,
                )),
            )));
            assert!(!manager.local.backend_repair.as_ref().unwrap().installed);
            assert!(manager.local.runtime_paths_ready());
            assert!(
                manager
                    .local
                    .status
                    .as_deref()
                    .unwrap()
                    .contains("Could not confirm")
            );
        }
        let _ = manager.update(ManagerMessage::Local(LocalMessage::Finished(
            0,
            Ok(Output::RuntimeActivated(
                "verified test package".into(),
                Some(required),
                false,
            )),
        )));
        assert!(!manager.local.runtime_paths_ready());
        assert!(manager.local.backend_repair.as_ref().unwrap().installed);
        let _ = manager.update(ManagerMessage::Local(LocalMessage::CloseBackendRepair));
        assert!(!manager.local.backend_repair_open());
        assert_eq!(manager.local.managed.selected.as_ref().unwrap().model, gpu);
        assert_eq!(
            manager.local.runtime_import().archive,
            PathBuf::from("generic.zip")
        );
        assert!(!manager.dirty());
    }

    #[test]
    fn default_managed_model_restores_by_id_without_changing_roles_or_live_selection() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let chat = model(
            "0123456789abcdef0123456789abcdef",
            "Chat model",
            Purpose::Chat,
        );
        let embedding = model(
            "fedcba9876543210fedcba9876543210",
            "Embed model",
            Purpose::Embedding,
        );
        let models = vec![chat.clone(), embedding.clone()];
        let saved: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "default": 1, "last_id": 1,
                "profiles": [{"id": 1, "name": "Existing profile", "model": "fixture"}]
            },
            "local": {
                "roles": [1, 1, null, null],
                "managed_embedding": embedding.id.as_str(),
                "default_managed_model": embedding.id.as_str()
            }
        }))
        .unwrap();
        saved
            .save_against(&root, &Configuration::default())
            .unwrap();
        let baseline = std::fs::read(root.join(FILE_NAME)).unwrap();
        let saved = Arc::new(saved);
        let open = || {
            let mut manager =
                Manager::new(Arc::clone(&saved), root.clone(), Vec::new(), true, None);
            manager.page = Page::LocalModels;
            manager
        };
        let mut dialog = open();
        let _ = dialog.update(loaded(models.clone(), &root));
        assert_eq!(
            dialog.local.managed.selected.as_ref().unwrap().model,
            embedding
        );
        let _ = dialog.update(ManagerMessage::Timeout("17".into()));
        let choice = dialog.local.managed.choices[0].clone();
        let _ = dialog.update(ManagerMessage::Local(Managed(Message::Select(Box::new(
            choice,
        )))));
        change_default(&mut dialog, Message::SetDefault);
        assert!(dialog.dirty());
        assert_eq!(dialog.timeout, "17");
        assert!(Arc::ptr_eq(&dialog.saved, &saved));
        assert_eq!(std::fs::read(root.join(FILE_NAME)).unwrap(), baseline);

        let mut reopened = open();
        let _ = reopened.update(loaded(models.clone(), &root));
        let selected = reopened.local.managed.selected.as_ref().unwrap();
        assert_eq!(selected.model, chat);
        assert_eq!(selected.to_string(), "★ Chat model · Chat");
        assert!(!reopened.dirty());
        let other = reopened.local.managed.choices[1].clone();
        let _ = reopened.update(ManagerMessage::Local(Managed(Message::Select(Box::new(
            other,
        )))));
        let _ = reopened.update(loaded(models.clone(), &root));
        assert_eq!(
            reopened.local.managed.selected.as_ref().unwrap().model,
            embedding
        );
        assert!(reopened.local.managed.choices[0].is_default);

        let mut missing = open();
        let _ = missing.update(loaded(vec![embedding], &root));
        assert!(!missing.local.managed.has_selection());
        change_default(&mut missing, Message::ClearDefault);
        assert!(!missing.dirty());
        assert_eq!(std::fs::read(root.join(FILE_NAME)).unwrap(), baseline);
        let mut cleared = open();
        let _ = cleared.update(loaded(models, &root));
        assert!(!cleared.local.managed.has_selection());
        assert!(
            cleared
                .local
                .managed
                .choices
                .iter()
                .all(|choice| !choice.is_default)
        );
    }
}
