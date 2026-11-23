//! Local managed model actions in the AI Manager.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::keyboard::Modifiers;
use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use iced::{Element, Length, Task};

use crate::local_ai::managed_models::{self, Model, ModelBackend, Purpose, Registry};
use crate::local_ai::runtime_index::PackageBackend;
use crate::local_ai::runtime_package::{self, ModelStatus};
use crate::workspace::dialog_design;

use super::Message::Managed;
use super::{BackendRepair, Dialog, Message as LocalMessage, Output, field, limited};
use crate::ai_manager::java_models::{self, Hint, Metadata};
use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{ManagedSetting, Reveal, Setting};

mod batch;
pub(super) mod inventory;
mod preference;
pub(crate) use batch::ReviewedModel;
use batch::change_models;
pub(crate) use inventory::ModelId;
pub(super) use inventory::Selection as InventorySelection;
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
    Browse,
    Source(String),
    ProjectorPath(String),
    ImportPurpose(Purpose),
    SelectInstalled(ModelId, Option<Modifiers>),
    SelectAll,
    Sort(Column),
    CloseImport,
    Refresh,
    SetDefault,
    ClearDefault,
    OpenDefaultMenu(ModelId, iced::Point),
    CloseDefaultMenu,
    MoveDefaultMenu,
    Import,
    Reference,
    AskReattachment,
    ConfirmReattachment,
    Start,
    ConfirmStart,
    Stop,
    Test,
    RepairBackend,
    AcceptBackendOffer,
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
            Self::SelectInstalled(..)
                | Self::SelectAll
                | Self::Sort(_)
                | Self::Refresh
                | Self::Stop
                | Self::SetDefault
                | Self::ClearDefault
                | Self::OpenDefaultMenu(..)
                | Self::CloseDefaultMenu
                | Self::MoveDefaultMenu
                | Self::CancelAction
                | Self::CloseImport
        )
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) enum Column {
    #[default]
    Name,
    File,
    Purpose,
    Backend,
    Unload,
    Age,
    State,
}

impl Column {
    const ALL: [Self; 7] = [
        Self::Name,
        Self::File,
        Self::Purpose,
        Self::Backend,
        Self::Unload,
        Self::Age,
        Self::State,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::File => "GGUF file",
            Self::Purpose => "Purpose",
            Self::Backend => "Backend",
            Self::Unload => "Unload after",
            Self::Age => "Age",
            Self::State => "State",
        }
    }
}

enum Action {
    ConfirmImport,
    ConfirmBackend {
        model_name: String,
        active: PackageBackend,
        required: PackageBackend,
    },
    Configure(Configuration),
    ConfirmStart {
        models: Vec<ReviewedModel>,
        memory_percent: u128,
    },
    ConfirmRemoval(Vec<ReviewedModel>),
    Reattach {
        weights: PathBuf,
        projector: Option<PathBuf>,
    },
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
    java_review: bool,
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
            java_review: false,
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

    fn fields<'a>(
        &'a self,
        location: String,
        enabled: bool,
        active_backend: Option<PackageBackend>,
        reveal: &Reveal,
    ) -> Element<'a, LocalMessage> {
        use super::model_configuration::gguf_field as field;
        use ManagedSetting::*;
        let number = |setting,
                      value: &'a str,
                      bounds,
                      step,
                      action: fn(String) -> LocalMessage,
                      editable| {
            super::model_configuration::number(
                reveal.input(
                    Setting::Managed(setting),
                    editable,
                    text_input("", value).on_input_maybe(editable.then_some(action)),
                ),
                value,
                bounds,
                step,
                editable.then_some(action),
            )
        };
        let idle_enabled = enabled && !self.never_unload;
        column![
            reveal.wrap(
                Setting::Managed(Name),
                field(
                    "Name",
                    reveal.input(
                        Setting::Managed(Name),
                        enabled,
                        text_input("Name", &self.name)
                            .size(13)
                            .padding([7, 5])
                            .style(dialog_design::form_input)
                            .on_input_maybe(enabled.then_some(|name| Managed(Message::Name(name)))),
                    )
                ),
            ),
            field("GGUF file", super::model_configuration::gguf_path(location)),
            reveal.wrap(
                Setting::Managed(Backend),
                field("Backend", backend_picker(self.backend, enabled)),
            ),
            unavailable_backend(self.backend, active_backend).map(|reason| field(
                "",
                text(reason).size(13).wrapping(text::Wrapping::WordOrGlyph)
            )),
            reveal.wrap(
                Setting::Managed(Purpose),
                field(
                    "Purpose",
                    purpose_picker(self.purpose, enabled, Message::ConfigurePurpose)
                ),
            ),
            reveal.wrap(
                Setting::Managed(Context),
                field(
                    "Context size",
                    number(
                        Context,
                        &self.context,
                        512..=2_097_152,
                        512,
                        |value| Managed(Message::Context(value)),
                        enabled
                    )
                ),
            ),
            reveal.wrap(
                Setting::Managed(CpuThreads),
                field(
                    "CPU threads (0 = Auto)",
                    number(
                        CpuThreads,
                        &self.cpu_threads,
                        0..=1024,
                        1,
                        |value| Managed(Message::CpuThreads(value)),
                        enabled
                    )
                ),
            ),
            reveal.wrap(
                Setting::Managed(GpuLayers),
                field(
                    "GPU layers (-1 = Auto)",
                    number(
                        GpuLayers,
                        &self.gpu_layers,
                        -1..=10_000,
                        1,
                        |value| Managed(Message::GpuLayers(value)),
                        enabled
                    )
                ),
            ),
            field(
                "Unload after",
                row![
                    reveal.action(
                        Setting::Managed(IdleMinutes),
                        number(
                            IdleMinutes,
                            &self.idle_minutes,
                            1..=1440,
                            1,
                            |value| Managed(Message::IdleMinutes(value)),
                            idle_enabled
                        )
                    ),
                    text("minutes").size(13),
                    reveal.action(
                        Setting::Managed(NeverUnload),
                        checkbox(self.never_unload).label("Never").on_toggle_maybe(
                            enabled.then_some(|value| Managed(Message::NeverUnload(value)))
                        ),
                    ),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center)
                .wrap()
            ),
        ]
        .spacing(9)
        .into()
    }
}

impl Models {
    pub(super) fn java_target(&self, purpose: Purpose) -> Option<&Model> {
        self.configurable_model()
            .filter(|model| model.purpose == purpose)
    }

    pub(super) fn stage_java_settings(&mut self, hint: &Hint) -> bool {
        let Metadata::Gguf {
            backend,
            purpose,
            context,
            threads,
            gpu_layers,
            idle_minutes,
        } = hint.metadata
        else {
            return false;
        };
        let Some(model) = self.java_target(purpose) else {
            return false;
        };
        let mut draft = Configuration::new(model);
        draft.name.clone_from(&hint.name);
        if backend.available(self.active_backend) {
            draft.backend = backend;
        }
        if context != 0 {
            draft.context = context.to_string();
        }
        draft.cpu_threads = threads.to_string();
        draft.gpu_layers = gpu_layers.to_string();
        draft.never_unload = idle_minutes == 0;
        if idle_minutes != 0 {
            draft.idle_minutes = idle_minutes.to_string();
        }
        draft.java_review = true;
        self.action = Some(Action::Configure(draft));
        true
    }
}

#[derive(Default)]
pub(super) struct Models {
    source: String,
    picked_files: Vec<PathBuf>,
    file_picker: Option<Arc<()>>,
    projector_path: String,
    import_purpose: Purpose,
    import_open: bool,
    choices: Vec<Choice>,
    sort_column: Column,
    sort_descending: bool,
    row_statuses: BTreeMap<String, ModelStatus>,
    selected: Option<Choice>,
    selected_ids: BTreeSet<String>,
    default: Option<SavedDefault>,
    active_backend: Option<PackageBackend>,
    active_runtime_version: Option<String>,
    status: Option<ModelStatus>,
    can_reattach: bool,
    action: Option<Action>,
}

impl Models {
    fn is_selected(&self, choice: &Choice) -> bool {
        self.selected_ids.contains(&choice.model.id) || self.selected.as_ref() == Some(choice)
    }

    fn selected_models(&self) -> impl Iterator<Item = &Model> {
        self.choices
            .iter()
            .filter(|choice| self.is_selected(choice))
            .map(|choice| &choice.model)
    }

    fn start_candidates(&self) -> impl Iterator<Item = &Model> {
        self.selected_models().filter(|model| {
            matches!(
                self.model_status(model),
                ModelStatus::Stopped | ModelStatus::Failed
            ) && model.backend.supported()
                && (!self.can_reattach
                    || self
                        .selected
                        .as_ref()
                        .is_none_or(|selected| selected.model.id != model.id))
        })
    }

    fn can_stop_selected(&self) -> bool {
        self.selected_models().any(|model| {
            matches!(
                self.model_status(model),
                ModelStatus::Ready | ModelStatus::Sleeping | ModelStatus::Failed
            )
        })
    }

    fn start_weights(&self, selected: &[ReviewedModel]) -> u128 {
        let selected_ids = selected
            .iter()
            .filter_map(|model| match model {
                ReviewedModel::Gguf(model) => Some(model.id.as_str()),
                ReviewedModel::Mlx(_) => None,
            })
            .collect::<BTreeSet<_>>();
        self.choices
            .iter()
            .filter(|choice| {
                selected_ids.contains(choice.model.id.as_str())
                    || matches!(
                        self.model_status(&choice.model),
                        ModelStatus::Starting
                            | ModelStatus::Ready
                            | ModelStatus::Busy
                            | ModelStatus::Sleeping
                    )
            })
            .map(|choice| {
                u128::from(choice.model.bytes)
                    + u128::from(choice.model.projector_bytes.unwrap_or_default())
            })
            .sum()
    }

    fn model_status(&self, model: &Model) -> &ModelStatus {
        self.row_statuses
            .get(&model.id)
            .unwrap_or(&ModelStatus::Stopped)
    }

    fn sort_by(&mut self, column: Column) {
        if self.sort_column == column {
            self.sort_descending = !self.sort_descending;
        } else {
            self.sort_column = column;
            self.sort_descending = false;
        }
    }

    pub(super) fn pending_file_picker(&self, ticket: &Arc<()>) -> bool {
        self.file_picker
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, ticket))
    }

    pub(super) fn finish_file_picker(&mut self) {
        self.file_picker = None;
    }

    pub(super) fn stage_files(&mut self, paths: Vec<PathBuf>) -> Result<(), &'static str> {
        if paths.is_empty() || self.has_action() {
            return Ok(());
        }
        if paths.len() > managed_models::MAX_MODELS {
            return Err("Choose at most 256 GGUF files per import.");
        }
        if paths.iter().any(|path| {
            !path.is_absolute()
                || !path
                    .extension()
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case("gguf"))
        }) {
            return Err("Choose local files with a .gguf extension.");
        }
        let mut seen = BTreeSet::new();
        self.picked_files = paths
            .iter()
            .map(|path| managed_models::import_source(path))
            .filter(|path| seen.insert(path.clone()))
            .collect();
        self.source.clear();
        self.projector_path.clear();
        self.action = Some(Action::ConfirmImport);
        Ok(())
    }

    fn import_review(&self, enabled: bool) -> Element<'_, LocalMessage> {
        let count = self.picked_files.len();
        let paired = !self.projector_path.trim().is_empty();
        let can_copy = enabled
            && count > 0
            && (!paired || (count == 1 && self.import_purpose == Purpose::Chat));
        let can_reference = enabled && count > 0 && !paired;
        let can_reattach = enabled
            && count == 1
            && self.selected.as_ref().is_some_and(|choice| {
                self.can_reattach && choice.model.projector_bytes.is_some() == paired
            });
        let form = column![
            text(if count == 1 {
                "Choose how to store the selected GGUF file.".into()
            } else {
                format!("Choose how to store the {count} selected GGUF files.")
            }),
            self.picked_file_rows(),
            field(
                "Import purpose",
                purpose_picker(self.import_purpose, enabled, Message::ImportPurpose)
            ),
            (count == 1).then(|| field(
                "Vision projector path (optional)",
                text_input(
                    "Absolute path to a compatible GGUF projector",
                    &self.projector_path
                )
                .on_input_maybe(enabled.then_some(|path| Managed(Message::ProjectorPath(path))))
            )),
            paired.then(|| text("Vision pairs use the Chat purpose and a private copy.").size(13)),
            (count == 1 && self.can_reattach).then(|| {
                dialog_design::secondary_action("Reattach to selected model…")
                    .on_press_maybe(can_reattach.then_some(Managed(Message::AskReattachment)))
            }),
        ]
        .spacing(10);
        iced::widget::container(
            column![
                iced::widget::scrollable(form).height(Length::Fill),
                row![
                    iced::widget::space::horizontal().width(Length::Fill),
                    button("Import private copy")
                        .on_press_maybe(can_copy.then_some(Managed(Message::Import))),
                    dialog_design::secondary_action("Register external GGUF")
                        .on_press_maybe(can_reference.then_some(Managed(Message::Reference))),
                    dialog_design::secondary_action("Cancel")
                        .on_press(Managed(Message::CancelAction)),
                ]
                .spacing(8)
                .wrap(),
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn picked_file_rows(&self) -> iced::widget::Column<'_, LocalMessage> {
        column(self.picked_files.iter().map(|path| {
            text(super::file_chooser::display_path(path).into_owned())
                .wrapping(text::Wrapping::WordOrGlyph)
                .size(13)
                .into()
        }))
        .spacing(4)
    }

    fn single_source(&self) -> Option<&Path> {
        match self.picked_files.as_slice() {
            [] => {
                let path = self.source.trim();
                (!path.is_empty()).then(|| Path::new(path))
            }
            [path] => Some(path),
            _ => None,
        }
    }

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
                is_default: id.as_ref().is_some_and(|id| id.is_gguf(&model.id)),
                model,
            })
            .collect();
        self.default = default;
        self.selected_ids
            .retain(|id| self.choices.iter().any(|choice| choice.model.id == *id));
        self.selected = self
            .selected
            .as_ref()
            .and_then(|selected| {
                self.choices
                    .iter()
                    .find(|choice| choice.model.id == selected.model.id)
            })
            .or_else(|| {
                self.choices
                    .iter()
                    .find(|choice| self.selected_ids.contains(&choice.model.id))
            })
            .cloned();
        if let Some(choice) = &self.selected {
            self.selected_ids.insert(choice.model.id.clone());
        }
        self.dismiss_action();
        self.update_status(profile);
        self.update_reattachment(profile);
    }

    pub(super) fn default_change(
        &self,
        model_id: Option<ModelId>,
        legacy: Option<&str>,
    ) -> Option<DefaultChange> {
        let current = self.default.as_ref()?;
        (current.resolve(legacy) != model_id).then(|| DefaultChange {
            previous: current.clone(),
            model_id,
        })
    }

    pub(super) fn set_default(&mut self, id: Option<ModelId>) {
        for choice in self.choices.iter_mut().chain(self.selected.as_mut()) {
            choice.is_default = id.as_ref().is_some_and(|id| id.is_gguf(&choice.model.id));
        }
        self.default = Some(SavedDefault::Stored(id));
    }

    pub(super) fn set_active_package(&mut self, package: Option<(String, PackageBackend)>) {
        (self.active_runtime_version, self.active_backend) = package.unzip();
    }

    pub(super) fn runtime_summary(&self) -> (Option<&str>, Option<PackageBackend>) {
        (self.active_runtime_version.as_deref(), self.active_backend)
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

    pub(super) fn configuring(&self) -> bool {
        matches!(self.action, Some(Action::Configure(_)))
    }

    pub(super) fn offer_matching_backend(&mut self) {
        if let Some(active) = self.active_backend
            && let Some((model, required)) = self.backend_repair()
        {
            self.action = Some(Action::ConfirmBackend {
                model_name: model.name.clone(),
                active,
                required,
            });
        }
    }

    pub(super) fn return_to_model(&mut self, id: &str, profile: &Path) {
        self.selected = self
            .choices
            .iter()
            .find(|choice| choice.model.id == id)
            .cloned();
        self.selected_ids = self
            .selected
            .iter()
            .map(|choice| choice.model.id.clone())
            .collect();
        self.update_status(profile);
        self.update_reattachment(profile);
    }

    #[cfg(test)]
    fn has_selection(&self) -> bool {
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

    pub(super) fn escape_message(&self) -> Option<Message> {
        if self.has_action() {
            Some(Message::CancelAction)
        } else {
            (self.import_open || self.file_picker.is_some()).then_some(Message::CloseImport)
        }
    }

    pub(super) fn dismiss_action(&mut self) {
        if matches!(self.action, Some(Action::ConfirmImport)) {
            self.picked_files.clear();
            self.projector_path.clear();
        }
        self.action = None;
    }

    pub(super) fn update_status(&mut self, profile: &Path) {
        self.row_statuses = self
            .choices
            .iter()
            .map(|choice| {
                let id = &choice.model.id;
                (id.clone(), runtime_package::model_status(profile, id))
            })
            .collect();
        self.status = self
            .selected
            .as_ref()
            .and_then(|choice| self.row_statuses.get(&choice.model.id).cloned());
    }

    fn update_reattachment(&mut self, profile: &Path) {
        self.can_reattach = self.selected.as_ref().is_some_and(|choice| {
            Registry::new(profile.to_owned())
                .check_reattachment(&choice.model)
                .is_ok()
        });
    }

    fn configurable_model(&self) -> Option<&Model> {
        self.selected
            .as_ref()
            .filter(|_| {
                !self.has_action()
                    && !matches!(self.status, Some(ModelStatus::Starting | ModelStatus::Busy))
            })
            .map(|choice| &choice.model)
    }

    fn begin_configuration(&mut self) {
        if let Some(model) = self.configurable_model() {
            self.action = Some(Action::Configure(Configuration::new(model)));
        }
    }

    pub(super) fn has_search_setting(
        &self,
        setting: ManagedSetting,
        editable: bool,
        ai_allowed: bool,
        mlx: &super::mlx::State,
    ) -> bool {
        use ManagedSetting::*;
        match setting {
            Name | Backend | Purpose | Context | CpuThreads | GpuLayers | IdleMinutes
            | NeverUnload => {
                ai_allowed
                    && match &self.action {
                        Some(Action::Configure(draft)) => {
                            setting != IdleMinutes || !draft.never_unload
                        }
                        None => self.configurable_model().is_some_and(|model| {
                            setting != IdleMinutes || model.idle_timeout_minutes > 0
                        }),
                        _ => false,
                    }
            }
            Source | Projector | ImportPurpose | Import | Reference => {
                ai_allowed && !self.has_action()
            }
            Refresh | ClearDefault => editable && !self.has_action(),
            Select => true,
            SetDefault => {
                editable
                    && !self.has_action()
                    && !mlx.has_review()
                    && (self.selected.is_some() || mlx.has_selection())
            }
            Stop => {
                editable
                    && !self.has_action()
                    && !mlx.has_review()
                    && (self.can_stop_selected() || mlx.can_stop_inventory_models())
            }
            Start => {
                ai_allowed
                    && !self.has_action()
                    && !mlx.has_review()
                    && (self.start_candidates().next().is_some()
                        || mlx.start_candidates().next().is_some())
            }
            Test => ai_allowed && self.selected.is_some() && !self.has_action(),
            ReviewRemoval => {
                ai_allowed
                    && !self.has_action()
                    && !mlx.has_review()
                    && (self.selected_models().next().is_some() || mlx.has_inventory_selection())
            }
            Configure => {
                ai_allowed
                    && (self.configurable_model().is_some()
                        || (mlx.has_selection() && !mlx.has_review()))
            }
            RepairBackend => ai_allowed && !self.has_action() && self.backend_repair().is_some(),
            Reattach => ai_allowed && !self.has_action() && self.can_reattach,
            Save => ai_allowed && matches!(self.action, Some(Action::Configure(_))),
            ConfirmReattachment => {
                ai_allowed && matches!(self.action, Some(Action::Reattach { .. }))
            }
            ConfirmRemoval => ai_allowed && matches!(self.action, Some(Action::ConfirmRemoval(_))),
            Cancel => ai_allowed && self.has_action(),
        }
    }

    pub(super) fn reveal_search_setting(&mut self, setting: ManagedSetting) {
        if setting.configuration_field() {
            self.begin_configuration();
        }
        use ManagedSetting::*;
        if matches!(
            setting,
            Source | Projector | ImportPurpose | Import | Reference | Reattach
        ) {
            self.import_open = true;
        }
    }

    pub(super) fn picker_needs_models(&self) -> bool {
        self.choices.is_empty()
    }

    pub(super) fn has_models(&self) -> bool {
        !self.choices.is_empty()
    }
}

impl Dialog {
    pub(super) fn managed_models_view(
        &self,
        editable: bool,
        ai_allowed: bool,
        legacy: Option<&str>,
        setup_available: bool,
        reveal: &Reveal,
    ) -> Element<'_, LocalMessage> {
        use ManagedSetting as Target;
        let models = &self.managed;
        let mlx = &self.mlx;
        let reveal_action = |setting, button| reveal.action(Setting::Managed(setting), button);
        let enabled =
            ai_allowed && !models.has_action() && !mlx.has_review() && models.file_picker.is_none();
        let configure = if ai_allowed && mlx.has_selection() && !mlx.has_review() {
            Some(LocalMessage::RevealMlxConfiguration)
        } else {
            (ai_allowed && models.configurable_model().is_some())
                .then_some(LocalMessage::RevealManagedSetting(Target::Name))
        };
        let start =
            models.start_candidates().next().is_some() || mlx.start_candidates().next().is_some();
        let remove = models.selected_models().next().is_some() || mlx.has_inventory_selection();
        let has_source = models.single_source().is_some() || !models.picked_files.is_empty();
        let default = self.installed_default(legacy);
        let table = self.installed_models_table(editable, default.as_ref(), reveal);
        let has_default = default.is_some();
        let can_mark = editable && self.can_select_installed() && models.default.is_some();
        let can_set_default = can_mark
            && self
                .installed_current()
                .is_some_and(|id| Some(id) != default);
        let default_available = default
            .as_ref()
            .is_some_and(|id| self.installed_contains(id));
        let status = models.status.as_ref().unwrap_or(&ModelStatus::Stopped);
        let idle = !matches!(status, ModelStatus::Starting | ModelStatus::Busy);
        let selected_count = models.selected_models().count();
        let action: Option<Element<'_, LocalMessage>> = models
            .selected
            .as_ref()
            .and_then(|Choice { model, .. }| match &models.action {
                Some(Action::Reattach { weights, projector }) => Some(
                    column![
                        text(if projector.is_some() {
                            format!("Reattach {} as a private vision pair?", model.name)
                        } else {
                            format!("Reattach {} as an external GGUF?", model.name)
                        })
                            .wrapping(text::Wrapping::WordOrGlyph),
                        text(weights.display().to_string()).wrapping(text::Wrapping::WordOrGlyph),
                        projector.as_ref().map(|path| text(format!("Projector · {}", path.display()))
                            .wrapping(text::Wrapping::WordOrGlyph)),
                        text("Choose files you trust. The backup records component sizes and shard layout. The application validates the selected GGUF files.")
                            .size(13),
                        row![
                            reveal_action(Target::Cancel, button("Cancel").on_press_maybe(
                                ai_allowed.then_some(Managed(Message::CancelAction))
                            )),
                            reveal_action(Target::ConfirmReattachment, button("Reattach model").on_press_maybe(
                                ai_allowed.then_some(Managed(Message::ConfirmReattachment))
                            )),
                        ]
                        .spacing(8)
                        .wrap(),
                    ]
                    .spacing(8)
                    .into(),
                ),
                Some(Action::Configure(_) | Action::ConfirmImport | Action::ConfirmBackend { .. } | Action::ConfirmStart { .. } | Action::ConfirmRemoval(_)) => None,
                None => None,
            });
        column![
            text("Installed local models").size(17),
            table,
            row![
                reveal.action(
                    Setting::Control(crate::ai_manager::search::Control::LocalSetup),
                    dialog_design::secondary_action("Setup assistant…").on_press_maybe(setup_available.then_some(LocalMessage::OpenModelSetup)),
                ),
                dialog_design::secondary_action(if models.import_open { "Close import" } else { "Import GGUF…" })
                    .on_press_maybe(enabled.then_some(if models.import_open {
                        Managed(Message::CloseImport)
                    } else {
                        Managed(Message::Browse)
                    })),
                reveal_action(Target::Configure, dialog_design::secondary_action("Configure…")
                    .on_press_maybe(configure)),
                reveal_action(Target::SetDefault, dialog_design::secondary_action("Set as default").on_press_maybe(
                    can_set_default
                        .then_some(Managed(Message::SetDefault)),
                )),
                reveal_action(Target::Start, dialog_design::secondary_action("Start selected").on_press_maybe(
                    (enabled && start).then_some(Managed(Message::Start)),
                )),
                reveal_action(Target::Stop, dialog_design::secondary_action("Stop selected").on_press_maybe(
                    (editable && !models.has_action() && !mlx.has_review() && (models.can_stop_selected() || mlx.can_stop_inventory_models()))
                        .then_some(Managed(Message::Stop)),
                )),
                reveal_action(Target::ReviewRemoval, dialog_design::secondary_action("Remove…").on_press_maybe(
                    (enabled && remove).then_some(Managed(Message::AskRemoval)),
                )),
                reveal_action(Target::Refresh, dialog_design::secondary_action("Refresh").on_press_maybe(
                    (editable && !models.has_action()).then_some(Managed(Message::Refresh)),
                )),
                (!models.import_open).then(|| button("Enter path…").style(button::text)
                    .on_press_maybe(enabled.then_some(LocalMessage::RevealManagedSetting(Target::Source)))),
            ]
            .spacing(8)
            .wrap(),
            action,
            (models.import_open || models.can_reattach).then(|| column![
            reveal.wrap(Setting::Managed(Target::Source), column![
                (!models.picked_files.is_empty() && !matches!(models.action, Some(Action::ConfirmImport)))
                    .then(|| iced::widget::scrollable(models.picked_file_rows())
                        .height(Length::Fixed((models.picked_files.len().min(4) as f32) * 24.0))),
                field(
                    if models.picked_files.is_empty() { "GGUF model path" } else { "Replace selection with a path" },
                    reveal.input(Setting::Managed(Target::Source), enabled,
                        text_input("Absolute path to the local GGUF", &models.source)
                            .on_input_maybe(enabled.then_some(|path| Managed(Message::Source(path)))))
                ),
            ].spacing(8)),
            reveal.wrap(Setting::Managed(Target::Projector), field(
                "Vision projector path (optional)",
                reveal.input(Setting::Managed(Target::Projector), enabled,
                    text_input("Absolute path to a compatible GGUF projector", &models.projector_path)
                        .on_input_maybe(enabled.then_some(|path| Managed(Message::ProjectorPath(path)))))
            )),
            reveal.wrap(Setting::Managed(Target::ImportPurpose), field(
                "Import purpose",
                purpose_picker(models.import_purpose, enabled, Message::ImportPurpose)
            )),
            row![
                dialog_design::secondary_action("Choose GGUF files…")
                    .on_press_maybe(enabled.then_some(Managed(Message::Browse))),
                reveal_action(Target::Import, button("Import private copy").on_press_maybe(
                    (enabled && has_source
                        && (models.projector_path.trim().is_empty() || (models.single_source().is_some()
                            && models.import_purpose == Purpose::Chat)))
                        .then_some(Managed(Message::Import)),
                )),
                reveal_action(Target::Reference, button("Register external GGUF").on_press_maybe(
                    (enabled && has_source && models.projector_path.trim().is_empty())
                        .then_some(Managed(Message::Reference)),
                )),
            ]
            .spacing(8)
            .wrap(),
            (!models.projector_path.trim().is_empty()).then(|| text(
                "Vision pairs use the Chat purpose. Import both components as a private copy."
            ).size(13)),
            ].spacing(8)),
            has_default.then(|| reveal_action(Target::ClearDefault, dialog_design::secondary_action("Clear default")
                .on_press_maybe(can_mark.then_some(Managed(Message::ClearDefault))))),
            (editable && has_default && !default_available)
                .then(|| text("The default managed model is unavailable. Select another model or clear the default.").size(13)),
            models.selected.as_ref().map(|Choice { model, .. }| {
                let idle_timeout = match model.idle_timeout_minutes {
                    0 => "Never unload while idle".into(),
                    minutes => format!("Unload after {minutes} min idle"),
                };
                column![
                    (selected_count > 1).then(|| text(format!("{selected_count} models selected · Current model: {}", model.name))
                        .size(13).wrapping(text::Wrapping::WordOrGlyph)),
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
                    unavailable_backend(model.backend, models.active_backend).map(|reason| text(reason).size(13)),
                    models.backend_repair().map(|_| reveal_action(Target::RepairBackend, button("Install matching runtime…")
                        .style(button::secondary)
                        .on_press_maybe(enabled.then_some(Managed(Message::RepairBackend))))),
                    row![
                        models.can_reattach.then(|| reveal_action(Target::Reattach, button("Reattach local GGUF…")
                            .on_press_maybe(
                                (enabled && idle && models.single_source().is_some()
                                    && model.projector_bytes.is_some() != models.projector_path.trim().is_empty())
                                    .then_some(Managed(Message::AskReattachment)),
                            ))),
                        reveal_action(Target::Test, button("Test model")
                            .on_press_maybe((enabled && idle && !models.can_reattach && model.backend.available(models.active_backend))
                                .then_some(Managed(Message::Test)))),
                    ]
                    .spacing(8)
                    .wrap(),
                ].spacing(8)
            }),
        ]
        .spacing(8)
        .width(Length::Fill)
        .into()
    }
}

fn reviewed_models(models: &[ReviewedModel]) -> Element<'_, LocalMessage> {
    let entries = column(models.iter().map(|model| {
        text(format!("{} · {}", model.name(), model.source_kind()))
            .size(13)
            .wrapping(text::Wrapping::WordOrGlyph)
            .into()
    }));
    iced::widget::scrollable(entries)
        .height(Length::Fixed(models.len().min(5) as f32 * 22.0))
        .into()
}

fn start_memory_warning(weights: u128, system_bytes: Option<u64>) -> Option<u128> {
    let system_bytes = u128::from(system_bytes.filter(|&bytes| bytes > 0)?);
    let estimated = weights * 135;
    (estimated >= system_bytes * 80).then(|| estimated.div_ceil(system_bytes).min(999))
}

fn model_file(model: &Model) -> String {
    model
        .external
        .as_ref()
        .and_then(|reference| reference.path().file_name())
        .map_or_else(
            || {
                if model.shard_bytes.is_empty() {
                    managed_models::WEIGHTS.to_owned()
                } else {
                    format!("{} managed shards", model.shard_bytes.len())
                }
            },
            |file| file.to_string_lossy().into_owned(),
        )
}

fn publication_days(published: Option<&str>, now: jiff::Timestamp) -> Option<i64> {
    published
        .and_then(|value| value.parse::<jiff::Timestamp>().ok())
        .filter(|date| *date <= now)
        .map(|date| now.duration_since(date).as_secs() / 86_400)
}

fn publication_age(published: Option<&str>, now: jiff::Timestamp) -> String {
    let Some(days) = publication_days(published, now) else {
        return "—".into();
    };
    match days {
        0 => "today".into(),
        1..=89 => format!("{days} d"),
        90..=729 => format!("{} mo", days / 30),
        _ => format!("{} yr", days / 365),
    }
}

fn import_models(
    registry: &Registry,
    paths: Vec<PathBuf>,
    purpose: Purpose,
    reference: bool,
    projector: Option<&Path>,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    let verb = if reference { "Registered" } else { "Imported" };
    let mut imported = 0;
    let mut failures = 0;
    let mut last_name = String::new();
    let mut first_error = None;
    for path in &paths {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        let result = if reference {
            registry.reference(path, purpose, cancelled)
        } else if let Some(projector) = projector {
            registry.import_projector_pair(path, projector, cancelled)
        } else {
            registry.import(path, purpose, cancelled)
        };
        match result {
            Ok(model) => {
                imported += 1;
                last_name = model.name;
            }
            Err(error) => {
                failures += 1;
                first_error.get_or_insert_with(|| {
                    let name = path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy();
                    format!("{name}: {error}")
                });
            }
        }
    }
    let was_cancelled = cancelled.load(Ordering::Acquire);
    if imported == 0 {
        return Err(if was_cancelled {
            "Managed model import cancelled.".into()
        } else {
            format!(
                "Could not import the managed model ({})",
                first_error.as_deref().unwrap_or("no models selected")
            )
        });
    }
    if paths.len() == 1 && !was_cancelled {
        return Ok(format!("{verb} {last_name}."));
    }
    let mut status = format!("{verb} {imported} of {} models.", paths.len());
    if let Some(error) = first_error {
        status.push_str(&format!(" Failed: {failures}. {error}"));
    }
    if was_cancelled {
        status.push_str(" Import cancelled.");
    }
    Ok(status)
}

impl Dialog {
    fn configured_gguf_path(&self) -> Option<String> {
        let model = &self.managed.selected.as_ref()?.model;
        Some(
            super::file_chooser::display_path(&model.registered_weights_path(&self.profile_dir))
                .into_owned(),
        )
    }

    pub(in crate::ai_manager) fn gguf_configuration_width(&self) -> f32 {
        self.configured_gguf_path().map_or(
            super::model_configuration::GGUF_MINIMUM_PREFERRED_WIDTH,
            |path| super::model_configuration::gguf_preferred_width(&path),
        )
    }

    pub(in crate::ai_manager) fn model_configuration_kind(
        &self,
    ) -> Option<super::ModelConfigurationKind> {
        use super::ModelConfigurationKind;
        if matches!(self.managed.action, Some(Action::Configure(_))) {
            Some(ModelConfigurationKind::Gguf)
        } else if self.mlx.configuration_open() {
            Some(ModelConfigurationKind::Mlx)
        } else {
            None
        }
    }

    pub(in crate::ai_manager) fn cancel_model_configuration(&mut self) {
        use super::ModelConfigurationKind;
        match self.model_configuration_kind() {
            Some(ModelConfigurationKind::Gguf) => self.managed.dismiss_action(),
            Some(ModelConfigurationKind::Mlx) => self.mlx.cancel_configuration(),
            None => return,
        }
        self.status = None;
    }

    pub(in crate::ai_manager) fn model_configuration_view(
        &self,
        enabled: bool,
        reveal: &Reveal,
    ) -> Option<Element<'_, LocalMessage>> {
        use super::ModelConfigurationKind;
        match self.model_configuration_kind()? {
            ModelConfigurationKind::Gguf => {
                let Some(Action::Configure(configuration)) = &self.managed.action else {
                    return None;
                };
                let fields = configuration.fields(
                    self.configured_gguf_path()?,
                    enabled,
                    self.managed.active_backend,
                    reveal,
                );
                let save = reveal.action(
                    Setting::Managed(ManagedSetting::Save),
                    button(text("Save").size(13).center().width(Length::Fill))
                        .padding([6, 10])
                        .width(94)
                        .on_press_maybe(enabled.then_some(Managed(Message::SaveConfiguration))),
                );
                let cancel = reveal.action(
                    Setting::Managed(ManagedSetting::Cancel),
                    button(text("Cancel").size(13).center().width(Length::Fill))
                        .padding([6, 10])
                        .width(94)
                        .style(dialog_design::secondary_button)
                        .on_press_maybe((!self.busy()).then_some(Managed(Message::CancelAction))),
                );
                Some(super::model_configuration::window(
                    fields,
                    reveal.viewport.clone(),
                    self.status.as_deref(),
                    save,
                    cancel,
                ))
            }
            ModelConfigurationKind::Mlx => self.mlx.configuration_view(
                &self.profile_dir,
                enabled,
                !self.busy(),
                self.status.as_deref(),
                reveal,
            ),
        }
    }

    pub(in crate::ai_manager) fn model_review_title(&self) -> Option<&'static str> {
        match self.managed.action {
            Some(Action::ConfirmImport) => Some("Import GGUF"),
            Some(Action::ConfirmBackend { .. }) => Some("Matching runtime"),
            Some(Action::ConfirmRemoval(_)) => Some("Remove selected models"),
            Some(Action::ConfirmStart { .. }) => Some("Start selected models"),
            _ => None,
        }
    }

    pub(in crate::ai_manager) fn cancel_model_review(&mut self) {
        if self.model_review_title().is_some() {
            self.managed.dismiss_action();
        }
    }

    pub(in crate::ai_manager) fn model_review_view(
        &self,
        enabled: bool,
    ) -> Option<Element<'_, LocalMessage>> {
        let (description, models, action) = match self.managed.action.as_ref()? {
            Action::ConfirmImport => return Some(self.managed.import_review(enabled)),
            Action::ConfirmBackend {
                model_name,
                active,
                required,
            } => {
                return Some(iced::widget::container(column![
                    text(format!("The saved model {model_name} needs a signed {required} runtime. The active runtime uses {active}."))
                        .wrapping(text::Wrapping::WordOrGlyph),
                    iced::widget::space::vertical().height(Length::Fill),
                    row![
                        iced::widget::space::horizontal().width(Length::Fill),
                        button("Review matching runtime").on_press_maybe(enabled.then_some(Managed(Message::AcceptBackendOffer))),
                        dialog_design::secondary_action("Keep current runtime").on_press(Managed(Message::CancelAction)),
                    ].spacing(8).wrap(),
                ].spacing(12))
                .padding(16)
                .width(Length::Fill)
                .height(Length::Fill)
                .into());
            }
            Action::ConfirmStart {
                models,
                memory_percent,
            } => (
                format!(
                    "Start {} selected model(s)? Estimated usage is {memory_percent}% of system RAM.",
                    models.len()
                ),
                models,
                button("Start selected")
                    .on_press_maybe(enabled.then_some(Managed(Message::ConfirmStart))),
            ),
            Action::ConfirmRemoval(models) => (
                format!("Remove {} selected model registration(s)?", models.len()),
                models,
                button("Remove")
                    .style(button::danger)
                    .on_press_maybe(enabled.then_some(Managed(Message::Remove))),
            ),
            _ => return None,
        };
        Some(iced::widget::container(column![
            text(description).wrapping(text::Wrapping::WordOrGlyph),
            matches!(self.managed.action, Some(Action::ConfirmRemoval(_))).then(|| {
                text("Removing a private model deletes its local copy. External source files stay on disk.").size(13)
                    .wrapping(text::Wrapping::WordOrGlyph)
            }),
            reviewed_models(models),
            iced::widget::space::vertical().height(Length::Fill),
            row![
                iced::widget::space::horizontal().width(Length::Fill),
                action,
                dialog_design::secondary_action("Cancel")
                    .on_press(Managed(Message::CancelAction)),
            ].spacing(8),
        ].spacing(12))
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into())
    }

    pub(super) fn update_managed(&mut self, message: Message) -> Task<LocalMessage> {
        match message {
            Message::OpenDefaultMenu(row, point) => self.open_default_menu(row, point),
            Message::MoveDefaultMenu => self.move_default_menu(),
            Message::CloseDefaultMenu | Message::SetDefault | Message::ClearDefault => {
                self.close_default_menu();
            }
            Message::Browse if !self.managed.has_action() && self.managed.file_picker.is_none() => {
                let ticket = Arc::new(());
                self.managed.file_picker = Some(Arc::clone(&ticket));
                return Task::done(LocalMessage::OpenManagedFilePicker(ticket));
            }
            Message::CloseImport => {
                self.managed.import_open = false;
                self.managed.finish_file_picker();
                self.managed.picked_files.clear();
            }
            Message::Source(path) => {
                self.managed.picked_files.clear();
                self.managed.source = limited(&path, 4096).into();
            }
            Message::ProjectorPath(path) => {
                self.managed.projector_path = limited(&path, 4096).into()
            }
            Message::ImportPurpose(purpose) => self.managed.import_purpose = purpose,
            Message::Sort(column) if self.can_select_installed() => {
                self.close_default_menu();
                self.managed.sort_by(column);
            }
            Message::SelectInstalled(id, modifiers) if self.can_select_installed() => {
                return self.select_installed(Some(id), modifiers);
            }
            Message::SelectAll if self.can_select_installed() => {
                return self.select_installed(None, None);
            }
            Message::Refresh if !self.managed.has_action() => {
                return self.read_models(
                    "Reading managed models…",
                    false,
                    crate::local_ai::mlx_runtime::supported(),
                    |_, _, _| Ok(None),
                );
            }
            Message::Import | Message::Reference
                if !self.managed.has_action()
                    || matches!(self.managed.action, Some(Action::ConfirmImport)) =>
            {
                let paths = self.managed.single_source().map_or_else(
                    || self.managed.picked_files.clone(),
                    |path| vec![path.to_owned()],
                );
                if paths.is_empty() {
                    return Task::none();
                }
                let purpose = self.managed.import_purpose;
                let reference = matches!(message, Message::Reference);
                let projector = self.managed.projector_path.trim();
                if !projector.is_empty()
                    && (reference || paths.len() != 1 || purpose != Purpose::Chat)
                {
                    return Task::none();
                }
                let projector = (!projector.is_empty()).then(|| PathBuf::from(projector));
                let status = if reference {
                    "Registering the external GGUF…"
                } else {
                    "Importing the managed GGUF model…"
                };
                let task = self.manage_models(status, true, move |_, registry, cancelled| {
                    import_models(
                        registry,
                        paths,
                        purpose,
                        reference,
                        projector.as_deref(),
                        cancelled,
                    )
                    .map(Some)
                });
                if self.busy() && matches!(self.managed.action, Some(Action::ConfirmImport)) {
                    self.managed.dismiss_action();
                    self.managed.import_open = false;
                }
                return task;
            }
            Message::AskReattachment
                if self.managed.can_reattach
                    && (!self.managed.has_action()
                        || matches!(self.managed.action, Some(Action::ConfirmImport))) =>
            {
                let Some(Choice { model, .. }) = &self.managed.selected else {
                    return Task::none();
                };
                let Some(weights) = self.managed.single_source().map(Path::to_path_buf) else {
                    return Task::none();
                };
                let projector_path = self.managed.projector_path.trim();
                let projector = (!projector_path.is_empty()).then(|| PathBuf::from(projector_path));
                if model.projector_bytes.is_some() == projector.is_some()
                    && weights.is_absolute()
                    && projector.as_ref().is_none_or(|path| path.is_absolute())
                {
                    self.managed.action = Some(Action::Reattach { weights, projector });
                } else {
                    self.status = Some(
                        if model.projector_bytes.is_some() {
                            "Enter absolute paths to the GGUF weights and vision projector."
                        } else {
                            "Enter an absolute GGUF path and clear the vision projector field."
                        }
                        .into(),
                    );
                }
            }
            Message::ConfirmReattachment => {
                if let (Some(Action::Reattach { weights, projector }), Some(Choice { model, .. })) =
                    (&self.managed.action, &self.managed.selected)
                {
                    let weights = weights.clone();
                    let projector = projector.clone();
                    let model = model.clone();
                    return self.manage_models(
                        "Reattaching the local GGUF…",
                        true,
                        move |_, registry, cancelled| {
                            let result = if let Some(projector) = projector {
                                registry.reattach_projector_pair(
                                    &model, &weights, &projector, cancelled,
                                )
                            } else {
                                registry.reattach(&model, &weights, cancelled)
                            };
                            result
                                .map(|model| Some(format!("Reattached {}.", model.name)))
                                .map_err(|error| {
                                    format!("Could not reattach the managed model ({error})")
                                })
                        },
                    );
                }
            }
            Message::Start if self.can_select_installed() => return self.prepare_group_start(),
            Message::ConfirmStart
                if matches!(self.managed.action, Some(Action::ConfirmStart { .. })) =>
            {
                if let Some(Action::ConfirmStart { models, .. }) = self.managed.action.take() {
                    return self.start_selected_models(models);
                }
            }
            Message::Stop
                if self.can_select_installed()
                    && (self.managed.can_stop_selected()
                        || self.mlx.can_stop_inventory_models()) =>
            {
                let models = self
                    .managed
                    .selected_models()
                    .filter(|model| *self.managed.model_status(model) != ModelStatus::Stopped)
                    .map(|model| (ModelId::Gguf(model.id.clone()), model.name.clone()))
                    .chain(
                        self.mlx
                            .inventory_stops()
                            .map(|(id, name)| (ModelId::Mlx(id.to_owned()), name.to_owned())),
                    )
                    .collect();
                return self.stop_selected_models(models);
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
            Message::RepairBackend | Message::AcceptBackendOffer => {
                if matches!(message, Message::AcceptBackendOffer) {
                    if !matches!(self.managed.action, Some(Action::ConfirmBackend { .. })) {
                        return Task::none();
                    }
                    self.managed.dismiss_action();
                }
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
            Message::Backend(backend) if backend.supported() => {
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
                    let java_review = draft.java_review;
                    if java_review && let Err(error) = java_models::require_review_policy() {
                        self.status = Some(error.to_string());
                        return Task::none();
                    }
                    match draft.settings(model) {
                        Ok(model) => {
                            return self.modify_model(
                                "Saving managed model settings…",
                                move |registry, expected| {
                                    let updated = registry.configure_checked(model, |current| {
                                        if java_review {
                                            java_models::require_review_policy()?;
                                        }
                                        if current != expected {
                                            return Err(io::Error::other("The selected GGUF model changed. Refresh and review it again."));
                                        }
                                        Ok(())
                                    })?;
                                    Ok(format!("Saved settings for {}.", updated.name))
                                },
                            );
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::AskRemoval
                if self.can_select_installed()
                    && (self.managed.selected_models().next().is_some()
                        || self.mlx.has_inventory_selection()) =>
            {
                self.managed.action = Some(Action::ConfirmRemoval(
                    self.managed
                        .selected_models()
                        .cloned()
                        .map(ReviewedModel::Gguf)
                        .chain(self.mlx.selected_models().cloned().map(ReviewedModel::Mlx))
                        .collect(),
                ));
            }
            Message::Remove if matches!(self.managed.action, Some(Action::ConfirmRemoval(_))) => {
                if let Some(Action::ConfirmRemoval(models)) = self.managed.action.take() {
                    let include_mlx = models
                        .iter()
                        .any(|model| matches!(model, ReviewedModel::Mlx(_)));
                    return self.read_models(
                        "Removing the selected model registrations…",
                        true,
                        include_mlx,
                        move |profile, registry, cancelled| {
                            Ok(Some(change_models(
                                &models,
                                "Removed",
                                cancelled,
                                ReviewedModel::name,
                                |model| model.remove(profile, registry, cancelled),
                            )))
                        },
                    );
                }
            }
            Message::CancelAction if matches!(self.managed.action, Some(Action::Configure(_))) => {
                self.cancel_model_configuration();
            }
            Message::CancelAction => self.managed.dismiss_action(),
            _ => {}
        }
        Task::none()
    }

    fn prepare_group_start(&mut self) -> Task<LocalMessage> {
        let gguf: Vec<_> = self.managed.start_candidates().collect();
        let mlx: Vec<_> = self.mlx.start_candidates().collect();
        if gguf.is_empty() && mlx.is_empty() {
            return Task::none();
        }
        let mut required = gguf
            .iter()
            .filter_map(|model| model.backend.required_package());
        let backend = required.next();
        if required.any(|candidate| Some(candidate) != backend) {
            self.status = Some("The selected GGUF models require different runtime backends. Select models with compatible backends.".into());
            return Task::none();
        }
        let install = !gguf.is_empty()
            && (self.managed.active_runtime_version.is_none()
                || backend.is_some_and(|required| Some(required) != self.managed.active_backend));
        let models = gguf
            .into_iter()
            .cloned()
            .map(ReviewedModel::Gguf)
            .chain(mlx.into_iter().cloned().map(ReviewedModel::Mlx))
            .collect();
        if install {
            self.open_runtime_install(backend, models);
            return Task::none();
        }
        self.start_reviewed_models(models)
    }

    pub(super) fn start_reviewed_models(
        &mut self,
        models: Vec<ReviewedModel>,
    ) -> Task<LocalMessage> {
        let weights = self.managed.start_weights(&models) + self.mlx.start_weights(&models);
        if let Some(memory_percent) =
            start_memory_warning(weights, crate::host_memory::physical_bytes())
        {
            self.managed.action = Some(Action::ConfirmStart {
                models,
                memory_percent,
            });
            return Task::none();
        }
        self.start_selected_models(models)
    }

    fn start_selected_models(&mut self, models: Vec<ReviewedModel>) -> Task<LocalMessage> {
        let profile = self.profile_dir.clone();
        self.run_managed(
            runtime_package::work(move |cancelled| {
                Ok(batch::start_models(&profile, &models, cancelled))
            }),
            "Starting the selected models…",
            true,
            Output::ManagedNotice,
        )
    }

    fn stop_selected_models(&mut self, models: Vec<(ModelId, String)>) -> Task<LocalMessage> {
        let profile = self.profile_dir.clone();
        self.run_managed(
            runtime_package::work(move |cancelled| {
                Ok(change_models(
                    &models,
                    "Stopped",
                    cancelled,
                    |(_, name)| name,
                    |(id, name)| {
                        match id {
                            ModelId::Gguf(id) => runtime_package::stop_model(&profile, id),
                            ModelId::Mlx(id) => {
                                runtime_package::with_stopped_mlx_model(&profile, id, || Ok(()))
                            }
                        }
                        .map_err(|error| error.to_string())?;
                        Ok(format!("Stopped {name}."))
                    },
                ))
            }),
            "Stopping the selected models…",
            false,
            Output::ManagedNotice,
        )
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
        self.read_models(status, requires_ai, false, update)
    }

    fn read_models(
        &mut self,
        status: &'static str,
        requires_ai: bool,
        include_mlx: bool,
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
                let active_package = runtime_package::active_package_summary(&profile)
                    .ok()
                    .flatten();
                let mlx = include_mlx.then(|| super::mlx::snapshot(&profile, String::new()));
                Ok(Output::ManagedModels {
                    models,
                    default,
                    active_package,
                    mlx,
                    notice,
                })
            }),
            status,
            requires_ai,
            |output| output,
        )
    }
}

fn backend_picker(selected: ModelBackend, enabled: bool) -> Element<'static, LocalMessage> {
    if !enabled {
        return text(selected.to_string()).into();
    }
    let options: Vec<_> = ModelBackend::ALL
        .into_iter()
        .filter(|backend| backend.supported())
        .collect();
    pick_list(options, Some(selected), |backend| {
        Managed(Message::Backend(backend))
    })
    .text_size(13)
    .padding([5, 8])
    .width(96)
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

    pub(super) fn model(id: &str, name: &str, purpose: Purpose) -> Model {
        Model {
            id: id.into(),
            name: name.into(),
            bytes: 1024,
            published_at: None,
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

    #[test]
    fn java_model_suggestions_only_stage_for_a_selected_matching_gguf_purpose() {
        let mut models = Models::default();
        let hint = Hint {
            id: "source-model".into(),
            name: "Java suggestion".into(),
            published_at: None,
            metadata: Metadata::Gguf {
                backend: ModelBackend::Metal,
                purpose: Purpose::Embedding,
                context: 0,
                threads: 16,
                gpu_layers: 5,
                idle_minutes: 0,
            },
        };
        assert!(!models.stage_java_settings(&hint));
        let chat = model("0123456789abcdef0123456789abcdef", "Chat", Purpose::Chat);
        models.selected = Some(Choice {
            model: chat,
            is_default: false,
        });
        assert!(!models.stage_java_settings(&hint));
        let embedding = model(
            "123456789abcdef0123456789abcdef0",
            "Native embeddings",
            Purpose::Embedding,
        );
        models.selected = Some(Choice {
            model: embedding.clone(),
            is_default: false,
        });
        assert!(models.stage_java_settings(&hint));
        let Some(Action::Configure(draft)) = models.action.as_ref() else {
            panic!("expected a reviewable draft");
        };
        assert!(draft.java_review);
        let proposed = draft.settings(&embedding).unwrap();
        assert_eq!(proposed.id, embedding.id);
        assert_eq!(proposed.backend, embedding.backend);
        assert_eq!(proposed.context_tokens, embedding.context_tokens);
        assert_eq!(proposed.purpose, embedding.purpose);
        assert_eq!(proposed.name, hint.name);
        assert_eq!(proposed.cpu_threads, 16);
        assert_eq!(proposed.gpu_layers, 5);
        assert_eq!(proposed.idle_timeout_minutes, 0);
        assert_eq!(models.selected.as_ref().unwrap().model, embedding);
    }

    fn select_gguf(choice: &Choice) -> ManagerMessage {
        ManagerMessage::Local(Managed(Message::SelectInstalled(
            ModelId::Gguf(choice.model.id.clone()),
            None,
        )))
    }

    fn loaded(models: Vec<Model>, root: &Path) -> ManagerMessage {
        loaded_with_mlx(models, None, root)
    }

    fn loaded_with_mlx(
        models: Vec<Model>,
        mlx: Option<Vec<crate::local_ai::mlx_models::Model>>,
        root: &Path,
    ) -> ManagerMessage {
        ManagerMessage::Local(LocalMessage::Finished(
            0,
            Ok(Output::ManagedModels {
                models,
                mlx: mlx.map(|models| {
                    Ok(super::super::mlx::ResultValue::Snapshot(
                        models,
                        Default::default(),
                        "MLX models loaded.".into(),
                    ))
                }),
                default: preference::load(root).map_err(|error| error.to_string()),
                active_package: None,
                notice: "Models loaded.".into(),
            }),
        ))
    }

    #[test]
    fn starting_without_a_runtime_prompts_before_download_and_binds_the_selected_models() {
        use iced::keyboard::Modifiers;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let entries = [
            model("11111111111111111111111111111111", "Alpha", Purpose::Chat),
            model("22222222222222222222222222222222", "Bravo", Purpose::Chat),
            model("33333333333333333333333333333333", "Charlie", Purpose::Chat),
        ];
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(loaded(entries.to_vec(), &root));
        let alpha = manager.local.managed.choices[0].clone();
        let bravo = manager.local.managed.choices[1].clone();
        let _ = manager.local.select_installed(
            Some(ModelId::Gguf(alpha.model.id.clone())),
            Some(Modifiers::empty()),
        );
        let _ = manager.local.select_installed(
            Some(ModelId::Gguf(bravo.model.id.clone())),
            Some(Modifiers::SHIFT),
        );
        let task = manager.update(ManagerMessage::Local(Managed(Message::Start)));
        assert!(iced_runtime::task::into_stream(task).is_none());
        assert!(!manager.local.busy());
        assert!(matches!(
            manager.local.online.review.as_ref(),
            Some(super::super::online::Review::Stable { start, approved: false, .. })
                if start.as_slice() == [ReviewedModel::Gguf(entries[0].clone()),
                    ReviewedModel::Gguf(entries[1].clone())]
        ));
        manager.local.set_runtime_downloads_allowed(false);
        let blocked = manager.update(ManagerMessage::Local(LocalMessage::ConfirmStableRuntime));
        assert!(iced_runtime::task::into_stream(blocked).is_none());
        assert!(!manager.local.busy());
        let escape = manager.escape_message();
        let _ = manager.update(escape);
        assert!(manager.local.online.review.is_none());
        assert_eq!(manager.local.managed.selected_models().count(), 2);
    }

    #[test]
    fn model_selection_uses_sorted_ranges_and_retains_ids_on_refresh() {
        use iced::keyboard::Modifiers;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let entries = [
            model("11111111111111111111111111111111", "Alpha", Purpose::Chat),
            model("22222222222222222222222222222222", "Bravo", Purpose::Chat),
            model("33333333333333333333333333333333", "Charlie", Purpose::Chat),
            model("44444444444444444444444444444444", "Delta", Purpose::Chat),
        ];
        let mut dialog = Dialog {
            profile_dir: root.clone(),
            ..Default::default()
        };
        dialog
            .managed
            .set_models(entries.to_vec(), None, None, &root);
        dialog.managed.sort_by(Column::Name);
        let [alpha, bravo, charlie, delta] = dialog.managed.choices.clone().try_into().unwrap();
        let _ = dialog.select_installed(
            Some(ModelId::Gguf(delta.model.id.clone())),
            Some(Modifiers::empty()),
        );
        let _ = dialog.select_installed(
            Some(ModelId::Gguf(bravo.model.id.clone())),
            Some(Modifiers::SHIFT),
        );
        assert_eq!(
            dialog
                .managed
                .selected_models()
                .map(|model| model.name.as_str())
                .collect::<Vec<_>>(),
            ["Bravo", "Charlie", "Delta"],
        );
        let command = if cfg!(target_os = "macos") {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL
        };
        let _ =
            dialog.select_installed(Some(ModelId::Gguf(charlie.model.id.clone())), Some(command));
        let _ = dialog.select_installed(Some(ModelId::Gguf(alpha.model.id.clone())), Some(command));
        dialog.managed.set_models(
            vec![entries[0].clone(), entries[2].clone(), entries[3].clone()],
            None,
            None,
            &root,
        );
        assert_eq!(
            dialog
                .managed
                .selected_models()
                .map(|model| model.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Delta"],
        );
        assert_eq!(dialog.managed.selected.as_ref(), Some(&alpha));
        assert_eq!(
            start_memory_warning(
                dialog.managed.start_weights(&[
                    ReviewedModel::Gguf(alpha.model),
                    ReviewedModel::Gguf(delta.model)
                ]),
                Some(3_000)
            ),
            Some(93)
        );
    }

    #[test]
    fn batch_removal_preserves_unselected_sources_and_rejects_a_changed_registration() {
        use iced::keyboard::Modifiers;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = Directory::open(&root, false).unwrap();
        let registry = Registry::new(root.clone());
        let mut installed = Vec::new();
        for name in ["alpha", "beta", "gamma"] {
            let source = format!("{name}-00001-of-00002.gguf");
            directory
                .publish_copy(&source, &managed_models::split_test_fixture(0, 2, 2, false))
                .unwrap();
            directory
                .publish_copy(
                    &format!("{name}-00002-of-00002.gguf"),
                    &managed_models::split_test_fixture(1, 2, 2, false),
                )
                .unwrap();
            installed.push(
                registry
                    .reference(&root.join(source), Purpose::Chat, &AtomicBool::new(false))
                    .unwrap(),
            );
        }
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(loaded(installed.clone(), &root));
        let mlx_registry = crate::local_ai::mlx_models::Registry::new(&root);
        let source = crate::local_ai::mlx_models::test_fixture(&root);
        let cancelled = AtomicBool::new(false);
        let review = crate::local_ai::mlx_models::Review::prepare(&source, &cancelled).unwrap();
        let mlx = mlx_registry.import(&review, &cancelled).unwrap();
        manager.local.mlx.add_hub_model(mlx.clone(), &root);
        let [alpha, beta, gamma] = manager.local.managed.choices.clone().try_into().unwrap();
        let _ = manager.update(select_gguf(&alpha));
        manager.local.set_selection_modifiers(Modifiers::SHIFT);
        let _ = manager.update(select_gguf(&beta));
        let toggle = if cfg!(target_os = "macos") {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL
        };
        let _ = manager.update(ManagerMessage::Local(Managed(Message::SelectInstalled(
            ModelId::Mlx(mlx.id.clone()),
            Some(toggle),
        ))));
        let _ = manager.update(ManagerMessage::Local(Managed(Message::AskRemoval)));
        let Some(Action::ConfirmRemoval(models)) = &manager.local.managed.action else {
            panic!("expected a removal review");
        };
        assert_eq!(models.len(), 3);
        assert!(models.contains(&ReviewedModel::Mlx(mlx)));
        manager.local.cancel_model_review();
        assert!(manager.local.mlx.has_inventory_selection());
        let _ = manager.update(ManagerMessage::Local(Managed(Message::AskRemoval)));
        manager.local.set_selection_modifiers(Modifiers::empty());
        let _ = manager.update(select_gguf(&gamma));
        let mut edited = installed[1].clone();
        edited.name = "Beta edited after review".into();
        registry.configure(edited.clone()).unwrap();

        let outcome = manager.update(ManagerMessage::Local(Managed(Message::Remove)));
        let mut outcomes = iced_runtime::task::into_stream(outcome).unwrap();
        while manager.local.busy() {
            let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
                panic!("expected the model removal result");
            };
            let _ = manager.update(event);
        }
        assert_eq!(registry.list().unwrap(), [edited, installed[2].clone()]);
        assert!(mlx_registry.list().unwrap().is_empty());
        assert!(
            manager
                .local
                .status
                .as_deref()
                .unwrap()
                .starts_with("Removed 2 of 3 models. Failed: 1.")
        );
        assert!(root.join("alpha-00001-of-00002.gguf").is_file());
        assert_eq!(manager.local.managed.selected_models().count(), 1);
    }

    #[test]
    fn chooser_imports_each_model_once_and_keeps_successes_when_one_file_fails() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let first = root.join("model-00001-of-00002.gguf");
        let second = root.join("model-00002-of-00002.gguf");
        std::fs::write(&first, managed_models::split_test_fixture(0, 2, 2, false)).unwrap();
        std::fs::write(&second, managed_models::split_test_fixture(1, 2, 2, false)).unwrap();
        let missing = root.join("missing.GGUF");
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        for confirm in [false, true] {
            let pick = manager.update(ManagerMessage::Local(Managed(Message::Browse)));
            let mut picker = iced_runtime::task::into_stream(pick).unwrap();
            let Some(iced_runtime::Action::Output(ManagerMessage::Local(
                LocalMessage::OpenManagedFilePicker(ticket),
            ))) = block_on(picker.next())
            else {
                panic!("expected the native picker request");
            };
            let _ = manager.update(ManagerMessage::Local(LocalMessage::ManagedFilesPicked(
                Arc::new(()),
                Some(vec![missing.clone()]),
            )));
            assert_eq!(manager.model_review_title(), None);
            let _ = manager.update(ManagerMessage::Local(LocalMessage::ManagedFilesPicked(
                ticket,
                Some(vec![second.clone(), first.clone(), missing.clone()]),
            )));
            assert_eq!(manager.model_review_title(), Some("Import GGUF"));
            assert!(manager.model_review_view().is_some());
            if !confirm {
                manager.cancel_model_review();
                assert_eq!(manager.model_review_title(), None);
                assert!(manager.local.managed.picked_files.is_empty());
                assert!(Registry::new(root.clone()).list().unwrap().is_empty());
            }
        }
        assert_eq!(manager.local.managed.picked_files, [first, missing]);
        let task = manager.update(ManagerMessage::Local(Managed(Message::Import)));
        let mut results = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(result)) = block_on(results.next()) else {
            panic!("expected the import completion");
        };
        let _ = manager.update(result);
        assert_eq!(manager.local.managed.choices.len(), 1);
        assert!(manager.local.managed.picked_files.is_empty());
        assert!(!manager.local.managed.import_open);
        assert!(
            manager
                .local
                .status
                .as_deref()
                .unwrap()
                .starts_with("Imported 1 of 2 models. Failed: 1. missing.GGUF:")
        );
        let registry = Registry::new(root);
        assert_eq!(registry.list().unwrap().len(), 1);
    }

    #[test]
    fn gguf_setting_search_focuses_the_draft_without_saving_or_replacing_it() {
        use iced::keyboard::Key;
        use iced::{Font, Renderer, Size};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::ai_manager::search::Target;
        use crate::settings_search::test_input;

        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        let gpu = Target::Setting(Setting::Managed(ManagedSetting::GpuLayers));
        let _ = manager.update(ManagerMessage::Search("gguf gpu layers".into()));
        assert!(!manager.search_results().any(|target| target == gpu));
        let hidden = manager.update(ManagerMessage::Reveal(gpu));
        assert!(iced_runtime::task::into_stream(hidden).is_none());

        let registered = model(
            "0123456789abcdef0123456789abcdef",
            "GGUF fixture",
            Purpose::Chat,
        );
        let _ = manager.update(loaded(vec![registered.clone()], root));
        let choice = manager.local.managed.choices[0].clone();
        let _ = manager.update(select_gguf(&choice));

        for (query, setting, key) in [
            ("gguf gpu layers", ManagedSetting::GpuLayers, "7"),
            ("gguf cpu threads", ManagedSetting::CpuThreads, "8"),
        ] {
            let _ = manager.update(ManagerMessage::Search(query.into()));
            let destination = Setting::Managed(setting);
            assert_eq!(
                manager.search_results().next(),
                Some(Target::Setting(destination))
            );
            let task = manager.update(ManagerMessage::SearchChoose);
            assert_eq!(manager.page, Page::LocalModels);
            assert_eq!(manager.revealed.setting, Some(destination));
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                manager
                    .model_configuration_view()
                    .expect("GGUF configuration window"),
                Size::new(900.0, 480.0),
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(&mut interface, &renderer, task);
            let typed = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Character(key.into()),
                Some(key),
            );
            assert!(match (setting, typed.as_slice()) {
                (
                    ManagedSetting::GpuLayers,
                    [ManagerMessage::Local(Managed(Message::GpuLayers(value)))],
                )
                | (
                    ManagedSetting::CpuThreads,
                    [ManagerMessage::Local(Managed(Message::CpuThreads(value)))],
                ) => value.ends_with(key),
                _ => false,
            });
            drop(interface);
            for message in typed {
                let _ = manager.update(message);
            }
        }
        let Some(Action::Configure(draft)) = &manager.local.managed.action else {
            panic!("the configuration draft should still be open");
        };
        assert!(draft.gpu_layers.ends_with('7'));
        assert!(draft.cpu_threads.ends_with('8'));
        assert_eq!(
            manager.local.managed.selected.as_ref().unwrap().model,
            registered
        );
        assert!(!manager.local.busy());
        let _ = manager.update(ManagerMessage::Local(Managed(Message::NeverUnload(true))));
        let _ = manager.update(ManagerMessage::Search("gguf idle timeout".into()));
        assert!(
            !manager
                .search_results()
                .any(|target| target
                    == Target::Setting(Setting::Managed(ManagedSetting::IdleMinutes)))
        );
        let _ = manager.update(ManagerMessage::Local(Managed(Message::CancelAction)));
        assert!(manager.local.managed.action.is_none());
        let _ = manager.update(ManagerMessage::SearchEscape);
        let _ = manager.update(ManagerMessage::Local(LocalMessage::RevealManagedSetting(
            ManagedSetting::Name,
        )));
        assert_eq!(
            manager.revealed.setting,
            Some(Setting::Managed(ManagedSetting::Name))
        );
        assert!(matches!(
            manager.local.managed.action,
            Some(Action::Configure(_))
        ));
        let cancel = manager.escape_message();
        assert!(matches!(
            cancel,
            ManagerMessage::Local(Managed(Message::CancelAction))
        ));
        let _ = manager.update(cancel);
        assert!(manager.local.managed.action.is_none());
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
    fn gguf_configuration_stays_open_after_validation_and_stale_write_failures() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = Directory::open(&root, false).unwrap();
        let first = "fixture-00001-of-00002.gguf";
        directory
            .publish_copy(first, &managed_models::split_test_fixture(0, 2, 2, false))
            .unwrap();
        directory
            .publish_copy(
                "fixture-00002-of-00002.gguf",
                &managed_models::split_test_fixture(1, 2, 2, false),
            )
            .unwrap();
        let registry = Registry::new(root.clone());
        let original = registry
            .reference(&root.join(first), Purpose::Chat, &AtomicBool::new(false))
            .unwrap();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(loaded(vec![original.clone()], &root));
        let selected = manager.local.managed.choices[0].clone();
        let _ = manager.update(select_gguf(&selected));
        let _ = manager.update(ManagerMessage::Local(LocalMessage::RevealManagedSetting(
            ManagedSetting::Name,
        )));
        assert_eq!(
            manager.model_configuration_kind(),
            Some(super::super::ModelConfigurationKind::Gguf)
        );
        let send =
            |manager: &mut Manager, action| manager.update(ManagerMessage::Local(Managed(action)));
        let _ = send(&mut manager, Message::Context("1".into()));
        assert!(
            iced_runtime::task::into_stream(send(&mut manager, Message::SaveConfiguration))
                .is_none()
        );
        assert!(manager.model_configuration_kind().is_some());
        let _ = send(&mut manager, Message::Context("4608".into()));
        let mut concurrent = original;
        concurrent.name = "Concurrent change".into();
        registry.configure(concurrent.clone()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut events = loop {
            if let Some(events) =
                iced_runtime::task::into_stream(send(&mut manager, Message::SaveConfiguration))
            {
                break events;
            }
            assert_eq!(
                manager.local.status.as_deref(),
                Some("A managed local model operation is already running")
            );
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let Some(iced_runtime::Action::Output(event)) = block_on(events.next()) else {
            panic!("expected the model save result");
        };
        let _ = manager.update(event);
        assert!(
            manager
                .local
                .status
                .as_deref()
                .unwrap()
                .contains("registration changed")
                || manager
                    .local
                    .status
                    .as_deref()
                    .unwrap()
                    .contains("selected GGUF model changed")
        );
        assert_eq!(registry.list().unwrap(), [concurrent]);
        assert!(manager.model_configuration_kind().is_some());
        manager.cancel_model_configuration();
        assert!(manager.model_configuration_kind().is_none());
    }

    #[test]
    fn manager_registers_reattaches_and_removes_a_split_model_from_any_shard() {
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
            while manager.local.busy() {
                let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
                    panic!("expected the model operation result");
                };
                let _ = manager.update(event);
            }
        };
        complete(&mut manager, Message::Reference);
        assert_eq!(manager.local.status.as_deref(), Some("Registered fixture."));
        let choice = manager.local.managed.choices[0].clone();
        assert_eq!(choice.to_string(), "fixture · Chat · 2 shards · Reference");
        assert_eq!(
            choice.model.external.as_ref().unwrap().path(),
            root.join(first)
        );
        let model_dir = directory
            .private_child(managed_models::BACKUP_DIRECTORY, false)
            .unwrap()
            .private_child(&choice.model.id, false)
            .unwrap();
        let portable = managed_models::portable_registration(
            &serde_json::to_vec(&choice.model).unwrap(),
            &choice.model.id,
        )
        .unwrap();
        model_dir
            .replace_file(managed_models::BACKUP_MANIFEST, &portable)
            .unwrap();
        drop(model_dir);
        complete(&mut manager, Message::Refresh);
        let detached = manager.local.managed.choices[0].clone();
        let _ = manager.update(select_gguf(&detached));
        assert!(manager.local.managed.can_reattach);
        let picked_source = managed_models::import_source(&root.join(second));
        manager
            .local
            .managed
            .stage_files(vec![root.join(second)])
            .unwrap();
        assert_eq!(manager.model_review_title(), Some("Import GGUF"));
        let _ = manager.update(ManagerMessage::Local(Managed(Message::AskReattachment)));
        assert!(matches!(
            &manager.local.managed.action,
            Some(Action::Reattach { weights, projector: None }) if *weights == picked_source
        ));
        let _ = manager.update(ManagerMessage::Local(Managed(Message::CancelAction)));
        let _ = manager.update(ManagerMessage::Local(Managed(Message::Source(
            root.join(second).to_string_lossy().into_owned(),
        ))));
        assert!(
            iced_runtime::task::into_stream(
                manager.update(ManagerMessage::Local(Managed(Message::ConfirmReattachment))),
            )
            .is_none()
        );
        let _ = manager.update(ManagerMessage::Local(Managed(Message::AskReattachment)));
        assert!(matches!(
            &manager.local.managed.action,
            Some(Action::Reattach { weights, projector: None }) if *weights == root.join(second)
        ));
        let _ = manager.update(ManagerMessage::Local(Managed(Message::CancelAction)));
        let detached = &manager.local.managed.selected.as_ref().unwrap().model;
        assert!(detached.external.is_none());
        let _ = manager.update(ManagerMessage::Local(Managed(Message::AskReattachment)));
        complete(&mut manager, Message::ConfirmReattachment);
        assert_eq!(manager.local.status.as_deref(), Some("Reattached fixture."));
        assert_eq!(manager.local.managed.selected.as_ref(), Some(&choice));
        assert!(!manager.local.managed.can_reattach);
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
        let _ = manager.update(select_gguf(&selected));
        manager
            .local
            .managed
            .set_active_package(Some(("fixture".into(), PackageBackend::Cpu)));

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
    fn saving_gguf_settings_offers_the_matching_backend_without_installing_it() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let original = model(
            "0123456789abcdef0123456789abcdef",
            "Fixture model",
            Purpose::Chat,
        );
        let mut saved = original.clone();
        saved.backend = if cfg!(target_os = "macos") {
            ModelBackend::Metal
        } else {
            ModelBackend::Vulkan
        };
        let required = saved.backend.required_package().unwrap();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(loaded(vec![original], &root));
        let selected = manager.local.managed.choices[0].clone();
        let _ = manager.update(select_gguf(&selected));

        for accept in [false, true] {
            let _ = manager.update(ManagerMessage::Local(LocalMessage::RevealManagedSetting(
                ManagedSetting::Name,
            )));
            assert!(manager.model_configuration_kind().is_some());
            let _ = manager.update(ManagerMessage::Local(LocalMessage::Finished(
                0,
                Ok(Output::ManagedModels {
                    models: vec![saved.clone()],
                    default: Ok(SavedDefault::Missing),
                    active_package: Some(("fixture-cpu".into(), PackageBackend::Cpu)),
                    mlx: None,
                    notice: "Saved settings.".into(),
                }),
            )));
            assert_eq!(manager.model_review_title(), Some("Matching runtime"));
            assert!(manager.model_review_view().is_some());
            assert!(!manager.local.backend_repair_open());
            let action = if accept {
                Message::AcceptBackendOffer
            } else {
                Message::CancelAction
            };
            let _ = manager.update(ManagerMessage::Local(Managed(action)));
            assert_eq!(manager.model_review_title(), None);
            assert_eq!(manager.local.backend_repair_open(), accept);
            assert!(manager.local.request.is_none());
            assert_eq!(
                manager.local.managed.selected.as_ref().unwrap().model,
                saved
            );
        }
        assert_eq!(
            manager.local.runtime_import().expected_backend,
            Some(required)
        );
    }

    #[test]
    fn guided_backend_repair_keeps_model_and_separates_install_paths() {
        use iced::{Font, Renderer, Size, keyboard::Key};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::ai_manager::search::{Control, Setting, Target};
        use crate::settings_search::test_input;

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
        let _ = manager.update(select_gguf(&selected));
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
        let _ = manager.update(ManagerMessage::Search("memory".into()));
        assert!(manager.search_results().next().is_none());
        assert!(matches!(
            manager.child_close_message(),
            Some(ManagerMessage::SearchEscape)
        ));
        let _ = manager.update(ManagerMessage::Search("index signature".into()));
        let signature = Setting::Control(Control::IndexSignature);
        assert_eq!(
            manager.search_results().next(),
            Some(Target::Setting(signature))
        );
        let reveal = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(manager.revealed.setting, Some(signature));
        assert!(manager.local.request.is_none());
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            manager.view(Vec::new(), Vec::new()),
            Size::new(760.0, 360.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, reveal);
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Character("x".into()),
            Some("x"),
        );
        assert!(
            matches!(messages.as_slice(), [ManagerMessage::Local(LocalMessage::RuntimePath(super::super::RuntimeFile::Signature, value))] if value == "x")
        );
        drop(interface);

        let _ = manager.update(ManagerMessage::Search(
            "check signed runtime updates".into(),
        ));
        let _ = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(
            manager.revealed.setting,
            Some(Setting::Control(Control::CheckRuntimeOnline))
        );
        assert!(manager.local.request.is_none());
        assert_eq!(manager.local.runtime_import().archive, PathBuf::new());
        let second = manager.local.managed.choices[1].clone();
        let _ = manager.update(select_gguf(&second));
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
        let _ = dialog.update(select_gguf(&choice));
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
        let _ = reopened.update(select_gguf(&other));
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

    #[test]
    fn mixed_default_context_menu_persists_the_current_mlx_selection() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        let gguf = model(id, "Alpha", Purpose::Chat);
        let mlx: crate::local_ai::mlx_models::Model = serde_json::from_value(serde_json::json!({
            "id": id, "name": "Bravo", "bytes": 0, "idle_minutes": 10, "assets": {},
        }))
        .unwrap();
        let mlx_id = ModelId::Mlx(id.into());
        let gguf_id = ModelId::Gguf(id.into());
        let open = || {
            let mut manager = Manager::new(
                Arc::new(Configuration::default()),
                root.clone(),
                Vec::new(),
                true,
                None,
            );
            manager.page = Page::LocalModels;
            manager
        };
        let snapshot = || loaded_with_mlx(vec![gguf.clone()], Some(vec![mlx.clone()]), &root);
        let mut manager = open();
        let _ = manager.update(snapshot());
        let _ = manager.update(ManagerMessage::Local(Managed(Message::OpenDefaultMenu(
            mlx_id.clone(),
            iced::Point::ORIGIN,
        ))));
        assert!(manager.local.default_menu_open());
        assert_eq!(manager.local.installed_current(), Some(mlx_id.clone()));
        assert_eq!(preference::load(&root).unwrap(), SavedDefault::Missing);
        change_default(&mut manager, Message::SetDefault);
        assert!(!manager.local.default_menu_open());
        assert_eq!(manager.local.installed_default(None), Some(mlx_id.clone()));
        assert!(!manager.local.managed.choices[0].is_default);
        let _ = manager
            .local
            .select_installed(Some(gguf_id.clone()), Some(Modifiers::empty()));
        let _ = manager.update(snapshot());
        assert_eq!(manager.local.installed_current(), Some(gguf_id));
        let mut reopened = open();
        let _ = reopened.update(snapshot());
        assert_eq!(reopened.local.installed_current(), Some(mlx_id.clone()));
        assert!(reopened.local.mlx.has_selection());
        let _ = reopened.update(loaded_with_mlx(vec![gguf], Some(Vec::new()), &root));
        assert_eq!(reopened.local.installed_current(), None);
        assert_eq!(reopened.local.installed_default(None), Some(mlx_id));
        change_default(&mut reopened, Message::ClearDefault);
        assert_eq!(preference::load(&root).unwrap(), SavedDefault::Stored(None));
    }
}
