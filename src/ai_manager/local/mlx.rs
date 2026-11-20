//! Explicit local installation and lifecycle controls for Apple MLX.

use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use iced::{Element, Length, Task};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::local_ai::{self, mlx_models, mlx_runtime, runtime_package};
use mlx_runtime::{InstallationState, online};

use super::{Dialog, Message, Output, limited};
use crate::ai_manager::java_models::{self, Hint, Metadata};
use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{MlxSetting, Reveal, Setting};

#[derive(Clone, Copy)]
pub(crate) enum Field {
    Index,
    Signature,
    Archive,
    Source,
    Name,
    Idle,
}

#[derive(Clone)]
pub(crate) enum Action {
    Edit(Field, String),
    NeverUnload(bool),
    Refresh,
    ReviewRuntime,
    CheckOnline,
    SelectOnline(online::Choice),
    ReviewOnline,
    Unpin(String),
    ReviewRuntimeRemoval,
    ReviewModel,
    ReviewReattachment,
    Confirm,
    ConfirmOnline(u64),
    Discard,
    Select(String),
    Start,
    Test,
    Stop,
    Configure,
    ReviewRemoval,
}

impl Action {
    fn online_download(&self) -> bool {
        matches!(
            self,
            Self::CheckOnline | Self::SelectOnline(_) | Self::ReviewOnline | Self::ConfirmOnline(_)
        )
    }

    pub(super) fn requires_ai(&self, state: &State) -> bool {
        match self {
            Self::ReviewRuntime
            | Self::CheckOnline
            | Self::ReviewOnline
            | Self::ConfirmOnline(_)
            | Self::ReviewModel
            | Self::ReviewReattachment
            | Self::Start
            | Self::Test
            | Self::Configure => true,
            Self::Confirm => !state.review.as_ref().is_some_and(Review::removal),
            _ => false,
        }
    }
}

#[derive(Clone)]
enum Review {
    Runtime(Arc<mlx_runtime::Review>),
    OnlineRuntime {
        ticket: u64,
        catalog: Arc<online::Catalog>,
        choice: online::Choice,
    },
    Catalog(Arc<mlx_runtime::CatalogReview>),
    Model(Arc<mlx_models::Review>, Option<mlx_models::Model>),
    Remove(Choice),
    RemoveRuntime(Arc<mlx_runtime::RemovalPlan>),
}

impl Review {
    fn removal(&self) -> bool {
        matches!(self, Self::Remove(_) | Self::RemoveRuntime(_))
    }

    fn confirmation_setting(&self) -> MlxSetting {
        match self {
            Self::Runtime(_) => MlxSetting::ConfirmRuntime,
            Self::OnlineRuntime { .. } => MlxSetting::ConfirmOnline,
            Self::Catalog(_) => MlxSetting::ConfirmCatalog,
            Self::Model(_, None) => MlxSetting::ConfirmImport,
            Self::Model(_, Some(_)) => MlxSetting::ConfirmReattachment,
            Self::Remove(_) => MlxSetting::ConfirmRemoval,
            Self::RemoveRuntime(_) => MlxSetting::ConfirmRuntimeRemoval,
        }
    }
}

#[derive(Clone)]
pub(crate) enum ResultValue {
    Runtime(Arc<mlx_runtime::Review>),
    OnlineCatalog(Arc<online::Catalog>),
    OnlineInstalled(online::InstallOutcome),
    Unpinned(InstallationState),
    Catalog(Arc<mlx_runtime::CatalogReview>),
    Model(Arc<mlx_models::Review>, Option<mlx_models::Model>),
    RuntimeRemovalReviewed(Option<Arc<mlx_runtime::RemovalPlan>>),
    RuntimeRemoved(Option<String>),
    Snapshot(Vec<mlx_models::Model>, InstallationState, String),
}

impl fmt::Debug for ResultValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MLX operation result")
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Choice {
    id: String,
    name: String,
    bytes: u64,
    idle_minutes: u16,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.name, &self.id[..8])
    }
}

#[derive(Default)]
pub(super) struct State {
    paths: [String; 3],
    source: String,
    name: String,
    idle: String,
    never_unload: bool,
    java_review: bool,
    review: Option<Review>,
    choices: Vec<Choice>,
    selected: Option<String>,
    detached: bool,
    runtime: InstallationState,
    online: Option<Arc<online::Catalog>>,
    online_choice: Option<online::Choice>,
    next_online_review: u64,
    status: Option<runtime_package::ModelStatus>,
}

impl State {
    fn choice(&self) -> Option<&Choice> {
        self.choices
            .iter()
            .find(|choice| Some(&choice.id) == self.selected.as_ref())
    }

    pub(super) fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

    pub(super) fn java_target(&self) -> Option<&str> {
        if !mlx_runtime::supported() || self.review.is_some() {
            return None;
        }
        self.choice().map(|choice| choice.name.as_str())
    }

    pub(super) fn stage_java_settings(&mut self, hint: &Hint) -> bool {
        let Metadata::Mlx { idle_minutes, .. } = hint.metadata else {
            return false;
        };
        if self.java_target().is_none() {
            return false;
        }
        self.name.clone_from(&hint.name);
        self.set_idle_draft(idle_minutes);
        self.java_review = true;
        true
    }

    pub(super) fn refresh_status(&mut self, profile: &Path) {
        self.status = self
            .selected
            .as_deref()
            .map(|id| runtime_package::mlx_model_status(profile, id));
    }

    fn refresh_reattachment(&mut self, profile: &Path) {
        self.detached = self.selected.as_deref().is_some_and(|id| {
            mlx_models::Registry::new(profile)
                .detached_model(id)
                .is_ok()
        });
    }

    pub(super) fn add_hub_model(&mut self, model: mlx_models::Model, profile: &Path) {
        let choice = Choice {
            id: model.id,
            name: model.name,
            bytes: model.bytes,
            idle_minutes: model.idle_minutes,
        };
        self.choices.retain(|other| other.id != choice.id);
        self.choices.push(choice.clone());
        self.choices
            .sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        self.select(Some(choice));
        self.refresh_status(profile);
        self.refresh_reattachment(profile);
    }

    pub(super) fn invalidate_review(&mut self) {
        if !self.review.as_ref().is_some_and(Review::removal) {
            self.review = None;
        }
        self.online = None;
        self.online_choice = None;
    }

    pub(super) fn runtime_removal_open(&self) -> bool {
        matches!(self.review, Some(Review::RemoveRuntime(_)))
    }

    pub(super) fn discard_runtime_removal(&mut self) {
        if self.runtime_removal_open() {
            self.review = None;
        }
    }

    pub(super) fn finish(&mut self, result: ResultValue, profile: &Path) -> String {
        match result {
            ResultValue::Runtime(review) => {
                self.review = Some(Review::Runtime(review));
                "Review the signed MLX runtime before installing it.".into()
            }
            ResultValue::OnlineCatalog(catalog) => {
                self.online_choice = catalog.choices().first().cloned();
                self.online = Some(catalog);
                if self.online_choice.is_some() {
                    "Select a signed MLX release to review its download.".into()
                } else {
                    "The signed stable MLX catalog has no compatible package.".into()
                }
            }
            ResultValue::OnlineInstalled(outcome) => {
                self.runtime = outcome.runtime;
                self.refresh_status(profile);
                if outcome.activated {
                    let id = self.runtime.active_installation_id().unwrap_or_default();
                    format!(
                        "Installed signed MLX runtime {id}. Test a model to confirm activation."
                    )
                } else if let Some(pin) = self.runtime.pinned_runtime_id() {
                    format!("Pinned MLX release {pin}.")
                } else {
                    "MLX follows stable releases.".into()
                }
            }
            ResultValue::Unpinned(runtime) => {
                self.runtime = runtime;
                "MLX follows stable releases.".into()
            }
            ResultValue::Catalog(review) => {
                self.review = Some(Review::Catalog(review));
                "Review the signed MLX catalog before accepting its trust changes.".into()
            }
            ResultValue::Model(review, restored) => {
                let notice = if restored.is_some() {
                    "Review the local files before reattaching the MLX model."
                } else {
                    "Review the local files before importing the MLX model."
                };
                self.review = Some(Review::Model(review, restored));
                notice.into()
            }
            ResultValue::RuntimeRemovalReviewed(plan) => {
                if let Some(plan) = plan {
                    self.review = Some(Review::RemoveRuntime(plan));
                    "Review removal of the installed MLX runtime.".into()
                } else {
                    self.runtime = InstallationState::default();
                    "There are no installed MLX runtime packages.".into()
                }
            }
            ResultValue::RuntimeRemoved(warning) => {
                self.runtime = InstallationState::default();
                self.refresh_status(profile);
                warning.unwrap_or_else(|| "Removed the installed MLX runtime.".into())
            }
            ResultValue::Snapshot(models, runtime, notice) => {
                self.runtime = runtime;
                self.choices = models
                    .into_iter()
                    .map(|model| Choice {
                        id: model.id,
                        name: model.name,
                        bytes: model.bytes,
                        idle_minutes: model.idle_minutes,
                    })
                    .collect();
                let selected = self
                    .choice()
                    .cloned()
                    .or_else(|| self.choices.first().cloned());
                self.select(selected);
                self.refresh_status(profile);
                self.refresh_reattachment(profile);
                notice
            }
        }
    }

    fn select(&mut self, choice: Option<Choice>) {
        self.java_review = false;
        self.selected = choice.as_ref().map(|choice| choice.id.clone());
        self.name = choice
            .as_ref()
            .map(|choice| choice.name.clone())
            .unwrap_or_default();
        if let Some(choice) = choice {
            self.set_idle_draft(choice.idle_minutes);
        } else {
            self.idle.clear();
            self.never_unload = false;
        }
    }

    fn set_idle_draft(&mut self, minutes: u16) {
        self.never_unload = minutes == 0;
        self.idle = if self.never_unload {
            mlx_models::DEFAULT_IDLE_MINUTES
        } else {
            minutes
        }
        .to_string();
    }

    pub(super) fn has_search_setting(
        &self,
        setting: MlxSetting,
        can_edit: bool,
        ai_allowed: bool,
        can_review_runtime_removal: bool,
        runtime_downloads_allowed: bool,
    ) -> bool {
        use MlxSetting::*;

        if !mlx_runtime::supported() || !can_edit {
            return false;
        }
        if let Some(review) = &self.review {
            return setting == CancelReview
                || (setting == review.confirmation_setting()
                    && (ai_allowed || review.removal())
                    && (setting != ConfirmOnline || runtime_downloads_allowed));
        }
        let online = ai_allowed && runtime_downloads_allowed;
        let has_signed_releases = || {
            self.online
                .as_ref()
                .is_some_and(|catalog| !catalog.choices().is_empty())
        };
        match setting {
            Unpin => self.runtime.pinned_runtime_id().is_some(),
            CheckOnline => online,
            OnlineChoice | ReviewOnline => online && has_signed_releases(),
            RuntimeIndex | RuntimeSignature | RuntimeArchive | Refresh | Source | Select => true,
            ReviewRuntime | ReviewImport => ai_allowed,
            ReviewRuntimeRemoval => can_review_runtime_removal,
            ReviewReattachment => ai_allowed && self.choice().is_some() && self.detached,
            Start | Test => ai_allowed && self.choice().is_some() && !self.detached,
            Stop | ReviewRemoval | Name | NeverUnload => self.choice().is_some(),
            Idle => self.choice().is_some() && !self.never_unload,
            Save => ai_allowed && self.choice().is_some(),
            CancelReview
            | ConfirmOnline
            | ConfirmRuntime
            | ConfirmCatalog
            | ConfirmImport
            | ConfirmReattachment
            | ConfirmRemoval
            | ConfirmRuntimeRemoval => false,
        }
    }

    pub(super) fn picker_needs_models(&self) -> bool {
        self.choices.is_empty()
    }

    pub(super) fn view(
        &self,
        can_edit: bool,
        ai_allowed: bool,
        can_review_runtime_removal: bool,
        runtime_downloads_allowed: bool,
        reveal: &Reveal,
    ) -> Option<Element<'_, Message>> {
        use MlxSetting as Target;
        if !mlx_runtime::supported() {
            return None;
        }
        let editing = can_edit && self.review.is_none();
        let can_request = editing && ai_allowed;
        let can_download = can_request && runtime_downloads_allowed;
        let field = |target, placeholder, value, field| {
            let setting = Setting::Mlx(target);
            let enabled = editing && !(matches!(field, Field::Idle) && self.never_unload);
            reveal.wrap(
                setting,
                reveal.input(
                    setting,
                    enabled,
                    text_input(placeholder, value).on_input_maybe(
                        enabled.then_some(move |value| Message::Mlx(Action::Edit(field, value))),
                    ),
                ),
            )
        };
        let action = |setting, label, action, enabled: bool| {
            reveal.action(
                Setting::Mlx(setting),
                button(label).on_press_maybe(enabled.then_some(Message::Mlx(action))),
            )
        };
        let selected = self.choice();
        let models = pick_list(self.choices.as_slice(), selected, |choice| {
            Message::Mlx(Action::Select(choice.id))
        })
        .placeholder("Choose a registered MLX model")
        .width(Length::Fill);
        let model_pick: Element<'_, Message> = if editing {
            models.dismiss_on_escape()
        } else {
            text(
                selected
                    .map(|choice| choice.name.as_str())
                    .unwrap_or("No MLX model selected"),
            )
            .into()
        };
        Some(column![
            text("Managed Apple MLX").size(18),
            text("Install a signed runtime and import a local safetensors directory. MLX profiles serve text requests on this computer.").size(13),
            text(self.runtime.active_installation_id().map(|id| {
                if self.runtime.pending_first_launch() {
                    format!("Installed MLX runtime · {id} · Pending first launch")
                } else {
                    format!("Active MLX runtime · {id}")
                }
            }).unwrap_or_else(|| "No active MLX runtime reported. Use Refresh MLX to check.".into())).size(13),
            self.runtime.pinned_runtime_id().map(|pin| row![
                text(if self.runtime.active_runtime_id() == Some(pin) {
                    format!("Pinned MLX release · {pin}")
                } else {
                    format!("Pinned MLX release awaiting installation · {pin}")
                }).size(13).wrapping(text::Wrapping::WordOrGlyph),
                action(Target::Unpin, "Follow stable MLX releases", Action::Unpin(pin.to_owned()), editing),
            ].spacing(8).wrap()),
            text("The signed stable MLX catalog is hosted in the korTTY runtime release repository on GitHub.").size(13),
            (!runtime_downloads_allowed).then(|| text("Managed policy blocks runtime downloads.").size(13)),
            action(Target::CheckOnline, "Check signed MLX releases", Action::CheckOnline, can_download),
            self.online.as_ref().filter(|catalog| !catalog.choices().is_empty()).map(|catalog| {
                let picker: Element<'_, Message> = if can_download {
                    pick_list(catalog.choices(), self.online_choice.as_ref(), |choice| {
                        Message::Mlx(Action::SelectOnline(choice))
                    })
                    .placeholder("Select a signed MLX release")
                    .width(Length::Fill)
                    .dismiss_on_escape()
                } else {
                    text(self.online_choice.as_ref().map(ToString::to_string).unwrap_or_default()).into()
                };
                column![
                    reveal.wrap(Setting::Mlx(Target::OnlineChoice), picker),
                    action(Target::ReviewOnline, "Review MLX download…", Action::ReviewOnline, can_download && self.online_choice.is_some()),
                ].spacing(8)
            }),
            self.review.as_ref().and_then(|review| {
                let Review::OnlineRuntime { ticket, catalog, choice } = review else {
                    return None;
                };
                Some(column![
                    text("Download and install this signed MLX runtime?").size(16),
                    text(format!("{choice} · {} bytes\nDownload destination · {}\nPackage SHA-256 {}", choice.size, choice.url, choice.sha256))
                        .size(13).wrapping(text::Wrapping::WordOrGlyph),
                    text(if catalog.pins(choice) {
                        "This selection pins the older MLX release while the signed channel offers it."
                    } else {
                        "This selection follows stable MLX releases."
                    }).size(13),
                    row![
                        action(Target::CancelReview, "Cancel", Action::Discard, can_edit),
                        action(Target::ConfirmOnline, "Download and install MLX runtime", Action::ConfirmOnline(*ticket), can_edit && ai_allowed && runtime_downloads_allowed),
                    ].spacing(8).wrap(),
                ].spacing(8))
            }),
            field(Target::RuntimeIndex, "Absolute path to mlx-runtime-index-v1.json", &self.paths[0], Field::Index),
            field(Target::RuntimeSignature, "Absolute path to the detached signature", &self.paths[1], Field::Signature),
            field(Target::RuntimeArchive, "Absolute path to the signed MLX package ZIP or TAR.ZST (optional for catalog review)", &self.paths[2], Field::Archive),
            row![
                action(Target::Refresh, "Refresh MLX", Action::Refresh, editing),
                action(Target::ReviewRuntime, "Review signed runtime…", Action::ReviewRuntime, can_request && self.runtime_paths_ready()),
                action(Target::ReviewRuntimeRemoval, "Remove installed MLX runtime…", Action::ReviewRuntimeRemoval, editing && can_review_runtime_removal),
            ].spacing(8).wrap(),
            field(Target::Source, "Absolute path to a local MLX model directory", &self.source, Field::Source),
            action(Target::ReviewImport, "Review model import…", Action::ReviewModel, can_request && Path::new(self.source.trim()).is_absolute()),
            reveal.wrap(Setting::Mlx(Target::Select), model_pick),
            selected.map(|choice| column![
                text(format!("{} bytes · {}", choice.bytes, self.status.map_or_else(|| "Checking status".into(), |status| status.to_string()))).size(13),
                row![
                    self.detached.then(|| action(Target::ReviewReattachment, "Reattach local MLX…", Action::ReviewReattachment,
                        can_request && Path::new(self.source.trim()).is_absolute())),
                    action(Target::Start, "Start", Action::Start, can_request && !self.detached),
                    action(Target::Test, "Test model", Action::Test, can_request && !self.detached),
                    action(Target::Stop, "Stop", Action::Stop, editing),
                    action(Target::ReviewRemoval, "Remove model…", Action::ReviewRemoval, editing),
                ].spacing(8).wrap(),
                field(Target::Name, "Model display name", &self.name, Field::Name),
                field(Target::Idle, "Idle timeout (1–1440 minutes)", &self.idle, Field::Idle),
                reveal.action(Setting::Mlx(Target::NeverUnload),
                    checkbox(self.never_unload)
                        .label("Never unload while idle")
                        .on_toggle_maybe(editing.then_some(|value| Message::Mlx(Action::NeverUnload(value))))),
                action(Target::Save, "Save MLX model settings", Action::Configure, can_request && !self.name.trim().is_empty() && self.idle_value().is_some()),
            ].spacing(8)),
            self.review.as_ref().and_then(|review| {
                let (heading, detail, confirm) = match review {
                    Review::Runtime(review) => (
                        "Install this signed MLX package?",
                        format!("{} · mlx-lm {} · {} bytes\nCatalog SHA-256 {}\nPackage SHA-256 {}", review.installation_id(), review.mlx_version(), review.archive_bytes(), review.index_sha256(), review.archive_sha256()),
                        "Install MLX runtime",
                    ),
                    Review::OnlineRuntime { .. } => return None,
                    Review::Catalog(review) => (
                        "Accept this signed MLX catalog?",
                        format!("{} withdrawn packages\nCatalog SHA-256 {}", review.revoked_count(), review.index_sha256()),
                        "Accept MLX catalog",
                    ),
                    Review::Model(review, restored) => (
                        if restored.is_some() { "Reattach these files to the restored MLX model?" } else { "Import a private copy of these MLX files?" },
                        format!("{} · {} bytes\n{}\n{}", restored.as_ref().unwrap_or_else(|| review.model()).name, review.model().bytes, review.source().display(), review.asset_names().collect::<Vec<_>>().join("\n")),
                        if restored.is_some() { "Reattach MLX model" } else { "Import MLX model" },
                    ),
                    Review::Remove(choice) => (
                        "Remove this private MLX model?",
                        format!("{} · {} bytes", choice.name, choice.bytes),
                        "Remove MLX model",
                    ),
                    Review::RemoveRuntime(plan) => (
                        "Remove the installed MLX runtime?",
                        format!("Installed packages to remove: {}. Registered models, AI profiles and the signed catalog trust state stay available.", plan.package_count()),
                        "Remove MLX runtime",
                    ),
                };
                let cancel = if matches!(review, Review::RemoveRuntime(_)) {
                    Message::CancelRuntimeRemoval
                } else {
                    Message::Mlx(Action::Discard)
                };
                Some(column![text(heading).size(16), text(detail).size(13).wrapping(text::Wrapping::WordOrGlyph), row![
                    reveal.action(Setting::Mlx(Target::CancelReview),
                        button("Cancel").on_press_maybe(can_edit.then_some(cancel))),
                    action(review.confirmation_setting(), confirm, Action::Confirm, can_edit && (ai_allowed || review.removal())),
                ].spacing(8).wrap()].spacing(8))
            }),
        ].spacing(8).into())
    }

    fn runtime_paths_ready(&self) -> bool {
        self.paths[..2]
            .iter()
            .all(|path| Path::new(path.trim()).is_absolute())
            && (self.paths[2].trim().is_empty() || Path::new(self.paths[2].trim()).is_absolute())
    }

    fn idle_value(&self) -> Option<u16> {
        if self.never_unload {
            return Some(0);
        }
        self.idle
            .trim()
            .parse()
            .ok()
            .filter(|minutes| (1..=1_440).contains(minutes))
    }
}

impl Dialog {
    pub(super) fn update_mlx(&mut self, action: Action) -> Task<Message> {
        if !mlx_runtime::supported()
            || (action.online_download() && !self.runtime_downloads_allowed)
        {
            return Task::none();
        }
        let requires_ai = action.requires_ai(&self.mlx);
        let profile = self.profile_dir.clone();
        let pending = match action {
            Action::Discard => {
                self.mlx.review = None;
                return Task::none();
            }
            Action::ConfirmOnline(ticket) => {
                let Some(Review::OnlineRuntime {
                    ticket: current,
                    catalog,
                    choice,
                }) = &self.mlx.review
                else {
                    return Task::none();
                };
                if *current != ticket {
                    return Task::none();
                }
                let (catalog, choice) = (Arc::clone(catalog), choice.clone());
                self.mlx.review = None;
                return self.run_managed(
                    online::download(profile, catalog, choice),
                    "Downloading and verifying the MLX runtime…",
                    true,
                    |outcome| Output::Mlx(ResultValue::OnlineInstalled(outcome)),
                );
            }
            Action::Confirm if matches!(self.mlx.review, Some(Review::OnlineRuntime { .. })) => {
                return Task::none();
            }
            Action::Confirm => match self.mlx.review.take() {
                Some(Review::Runtime(review)) => runtime_package::work(move |cancelled| {
                    let id = mlx_runtime::install(&profile, (*review).clone(), cancelled)?;
                    snapshot(
                        &profile,
                        format!(
                            "Installed signed MLX runtime {id}. Test a model to confirm activation."
                        ),
                    )
                }),
                Some(Review::Catalog(review)) => runtime_package::work(move |cancelled| {
                    mlx_runtime::accept_catalog(&profile, &review, cancelled)?;
                    snapshot(&profile, "Accepted the signed MLX runtime catalog.".into())
                }),
                Some(Review::Model(review, restored))
                    if restored
                        .as_ref()
                        .is_none_or(|model| self.mlx.selected.as_ref() == Some(&model.id)) =>
                {
                    runtime_package::work(move |cancelled| {
                        let registry = mlx_models::Registry::new(&profile);
                        let (result, verb) = if let Some(model) = restored {
                            (registry.reattach(&model, &review, cancelled), "Reattached")
                        } else {
                            (registry.import(&review, cancelled), "Imported")
                        };
                        let model = result.map_err(|error| error.to_string())?;
                        snapshot(&profile, format!("{verb} MLX model {}.", model.name))
                    })
                }
                Some(Review::Remove(choice)) if self.mlx.selected.as_ref() == Some(&choice.id) => {
                    runtime_package::work(move |_| {
                        runtime_package::with_stopped_mlx_model(&profile, &choice.id, || {
                            mlx_models::Registry::new(&profile).remove(&choice.id)
                        })
                        .map_err(|error| error.to_string())?;
                        snapshot(&profile, format!("Removed MLX model {}.", choice.name))
                    })
                }
                Some(Review::RemoveRuntime(plan)) => runtime_package::work(move |cancelled| {
                    mlx_runtime::remove(&profile, &plan, cancelled).map(ResultValue::RuntimeRemoved)
                }),
                _ => return Task::none(),
            },
            _ if self.mlx.review.is_some() => return Task::none(),
            Action::CheckOnline => {
                self.mlx.online = None;
                self.mlx.online_choice = None;
                return self.run_managed(
                    online::check(profile),
                    "Checking the signed MLX release catalog…",
                    true,
                    |catalog| Output::Mlx(ResultValue::OnlineCatalog(catalog)),
                );
            }
            Action::SelectOnline(choice) => {
                if self
                    .mlx
                    .online
                    .as_ref()
                    .is_some_and(|catalog| catalog.choices().contains(&choice))
                {
                    self.mlx.online_choice = Some(choice);
                }
                return Task::none();
            }
            Action::ReviewOnline => {
                if let Some(catalog) = &self.mlx.online
                    && let Some(choice) = &self.mlx.online_choice
                    && catalog.choices().contains(choice)
                {
                    self.mlx.next_online_review = self.mlx.next_online_review.wrapping_add(1);
                    self.mlx.review = Some(Review::OnlineRuntime {
                        ticket: self.mlx.next_online_review,
                        catalog: Arc::clone(catalog),
                        choice: choice.clone(),
                    });
                }
                return Task::none();
            }
            Action::Unpin(expected)
                if self.mlx.runtime.pinned_runtime_id() == Some(expected.as_str()) =>
            {
                runtime_package::work(move |cancelled| {
                    mlx_runtime::unpin(&profile, &expected, cancelled).map(ResultValue::Unpinned)
                })
            }
            Action::NeverUnload(value) if self.mlx.choice().is_some() => {
                self.mlx.never_unload = value;
                return Task::none();
            }
            Action::Edit(Field::Idle, _) if self.mlx.never_unload => return Task::none(),
            Action::Edit(field, value) => {
                let target = match field {
                    Field::Index => &mut self.mlx.paths[0],
                    Field::Signature => &mut self.mlx.paths[1],
                    Field::Archive => &mut self.mlx.paths[2],
                    Field::Source => &mut self.mlx.source,
                    Field::Name => &mut self.mlx.name,
                    Field::Idle => &mut self.mlx.idle,
                };
                *target = limited(&value, 4096).into();
                return Task::none();
            }
            Action::Select(id) => {
                if let Some(choice) = self
                    .mlx
                    .choices
                    .iter()
                    .find(|choice| choice.id == id)
                    .cloned()
                {
                    self.mlx.select(Some(choice));
                    self.mlx.refresh_status(&profile);
                    self.mlx.refresh_reattachment(&profile);
                }
                return Task::none();
            }
            Action::ReviewRemoval => {
                self.mlx.review = self.mlx.choice().cloned().map(Review::Remove);
                return Task::none();
            }
            Action::ReviewRuntimeRemoval if self.runtime_removal.is_none() => {
                return self.run_removal_review(
                    runtime_package::work(move |cancelled| {
                        mlx_runtime::review_removal(&profile, cancelled)
                            .map(|plan| ResultValue::RuntimeRemovalReviewed(plan.map(Arc::new)))
                    }),
                    "Checking the installed MLX runtime…",
                    Output::Mlx,
                );
            }
            Action::Refresh => runtime_package::work(move |_| {
                snapshot(&profile, "Read the MLX runtime and model registry.".into())
            }),
            Action::ReviewRuntime if self.mlx.runtime_paths_ready() => {
                let [index, signature, archive] = self
                    .mlx
                    .paths
                    .each_ref()
                    .map(|path| PathBuf::from(path.trim()));
                runtime_package::work(move |cancelled| {
                    if archive.as_os_str().is_empty() {
                        mlx_runtime::review_catalog(index, signature, cancelled)
                            .map(|review| ResultValue::Catalog(Arc::new(review)))
                    } else {
                        mlx_runtime::review(
                            mlx_runtime::Import {
                                index,
                                signature,
                                archive,
                            },
                            cancelled,
                        )
                        .map(|review| ResultValue::Runtime(Arc::new(review)))
                    }
                })
            }
            Action::ReviewModel => {
                let source = PathBuf::from(self.mlx.source.trim());
                runtime_package::work(move |cancelled| {
                    mlx_models::Review::prepare(&source, cancelled)
                        .map(|review| ResultValue::Model(Arc::new(review), None))
                        .map_err(|error| error.to_string())
                })
            }
            Action::ReviewReattachment if self.mlx.detached && self.mlx.review.is_none() => {
                let Some(id) = self.mlx.selected.clone() else {
                    return Task::none();
                };
                let source = PathBuf::from(self.mlx.source.trim());
                runtime_package::work(move |cancelled| {
                    mlx_models::Registry::new(&profile)
                        .review_reattachment(&id, &source, cancelled)
                        .map(|(model, review)| ResultValue::Model(Arc::new(review), Some(model)))
                        .map_err(|error| error.to_string())
                })
            }
            Action::Start | Action::Test | Action::Stop | Action::Configure => {
                let Some(choice) = self.mlx.choice().cloned() else {
                    return Task::none();
                };
                let name = self.mlx.name.trim().to_owned();
                let idle = self.mlx.idle_value();
                let java_review = self.mlx.java_review;
                if java_review
                    && matches!(action, Action::Configure)
                    && let Err(error) = java_models::require_review_policy()
                {
                    self.status = Some(error.to_string());
                    return Task::none();
                }
                runtime_package::work(move |cancelled| {
                    let notice = match action {
                        Action::Start => {
                            local_ai::start_mlx_model(&profile, &choice.id, cancelled)?;
                            "The managed MLX sidecar is ready."
                        }
                        Action::Test => {
                            test_model(&profile, &choice.id, cancelled)?;
                            "The managed MLX model replied to the test."
                        }
                        Action::Stop => {
                            runtime_package::with_stopped_mlx_model(
                                &profile,
                                &choice.id,
                                || Ok(()),
                            )
                            .map_err(|error| error.to_string())?;
                            "Stopped the managed MLX model."
                        }
                        Action::Configure => {
                            let idle =
                                idle.ok_or("Enter an MLX idle timeout from 1 to 1440 minutes or choose Never unload.")?;
                            runtime_package::with_stopped_mlx_model(&profile, &choice.id, || {
                                let registry = mlx_models::Registry::new(&profile);
                                if java_review {
                                    registry.configure_checked(&choice.id, &name, idle, |current| {
                                        java_models::require_review_policy()?;
                                        if current.name != choice.name
                                            || current.idle_minutes != choice.idle_minutes
                                            || current.bytes != choice.bytes
                                        {
                                            return Err(std::io::Error::other("The selected MLX model changed. Refresh and review it again."));
                                        }
                                        Ok(())
                                    })
                                } else {
                                    registry.configure(&choice.id, &name, idle)
                                }
                            })
                            .map_err(|error| error.to_string())?;
                            "Saved the MLX model settings."
                        }
                        _ => unreachable!("model lifecycle action"),
                    };
                    snapshot(&profile, notice.into())
                })
            }
            _ => return Task::none(),
        };
        self.run_managed(
            pending,
            "Working with managed MLX…",
            requires_ai,
            Output::Mlx,
        )
    }
}

fn snapshot(profile: &Path, notice: String) -> Result<ResultValue, String> {
    let models = mlx_models::Registry::new(profile)
        .list()
        .map_err(|error| error.to_string())?;
    let runtime = mlx_runtime::installed(profile).map_err(|error| error.to_string())?;
    Ok(ResultValue::Snapshot(models, runtime, notice))
}

fn test_profile(profile: &Path, id: &str) -> Result<local_ai::Profile, String> {
    let profile = local_ai::Profile::managed_mlx(
        profile.to_owned(),
        id,
        local_ai::ProfileOptions {
            timeout: Some(Duration::from_secs(300)),
            ..Default::default()
        },
    )?;
    Ok(profile.with_output_limit(32))
}

fn test_model(profile: &Path, id: &str, cancelled: &AtomicBool) -> Result<(), String> {
    let pending = local_ai::start(test_profile(profile, id)?, |client| {
        let answer = client
            .complete_text_before(
                "Reply briefly.",
                "This is a local MLX test. Reply OK.",
                false,
                std::time::Instant::now() + Duration::from_secs(300),
            )
            .map_err(|error| error.message())?;
        if answer.trim().is_empty() {
            return Err("The MLX model returned an empty reply".into());
        }
        Ok(())
    })?;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("MLX model test cancelled".into());
        }
        match pending.reply.try_recv() {
            Ok(result) => return result,
            Err(async_channel::TryRecvError::Closed) => {
                return Err("The MLX test worker stopped".into());
            }
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(25))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Request;
    use super::*;
    use crate::ai_manager::{Configuration, Dialog as Manager, Message as ManagerMessage, Page};
    use crate::private_files::Directory;

    #[test]
    fn mlx_search_focuses_local_fields_and_retains_the_active_review() {
        use iced::keyboard::Key;
        use iced::{Font, Renderer, Size};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::ai_manager::search::Target;
        use crate::settings_search::test_input;

        let profile = crate::snippets::tests::Profile::new();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            profile.0.clone(),
            Vec::new(),
            true,
            None,
        );
        let signature = Target::Setting(Setting::Mlx(MlxSetting::RuntimeSignature));
        let _ = manager.update(ManagerMessage::Search("MLX detached signature".into()));
        assert_eq!(
            manager.search_results().next(),
            mlx_runtime::supported().then_some(signature)
        );
        if !mlx_runtime::supported() {
            let hidden = manager.update(ManagerMessage::Reveal(signature));
            assert!(iced_runtime::task::into_stream(hidden).is_none());
            return;
        }

        let choice = Choice {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "Fixture".into(),
            bytes: 1024,
            idle_minutes: 10,
        };
        manager.local.mlx.choices.push(choice.clone());
        manager.local.mlx.selected = Some(choice.id.clone());
        manager.local.mlx.name.clone_from(&choice.name);
        manager.local.mlx.idle = choice.idle_minutes.to_string();
        for (query, setting) in [
            ("MLX detached signature", MlxSetting::RuntimeSignature),
            ("MLX model display name", MlxSetting::Name),
            ("MLX idle timeout", MlxSetting::Idle),
        ] {
            let _ = manager.update(ManagerMessage::Search(query.into()));
            let destination = Setting::Mlx(setting);
            assert_eq!(
                manager.search_results().next(),
                Some(Target::Setting(destination))
            );
            let task = manager.update(ManagerMessage::SearchChoose);
            assert_eq!(manager.page, Page::LocalModels);
            assert_eq!(manager.revealed.setting, Some(destination));
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                manager.view(Vec::new(), Vec::new()),
                Size::new(900.0, 480.0),
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(&mut interface, &renderer, task);
            let typed = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Character("7".into()),
                Some("7"),
            );
            assert!(match (setting, typed.as_slice()) {
                (
                    MlxSetting::RuntimeSignature,
                    [ManagerMessage::Local(Message::Mlx(Action::Edit(Field::Signature, value)))],
                )
                | (
                    MlxSetting::Name,
                    [ManagerMessage::Local(Message::Mlx(Action::Edit(Field::Name, value)))],
                )
                | (
                    MlxSetting::Idle,
                    [ManagerMessage::Local(Message::Mlx(Action::Edit(Field::Idle, value)))],
                ) => value.ends_with('7'),
                _ => false,
            });
            drop(interface);
            for message in typed {
                let _ = manager.update(message);
            }
        }
        assert_eq!(manager.local.mlx.paths[1], "7");
        assert_eq!(manager.local.mlx.name, "Fixture7");
        assert_eq!(manager.local.mlx.idle, "107");
        assert!(manager.local.mlx.choice() == Some(&choice));
        assert!(!manager.local.busy());

        let online = Target::Setting(Setting::Mlx(MlxSetting::CheckOnline));
        let _ = manager.update(ManagerMessage::Search("check signed MLX releases".into()));
        assert!(manager.search_results().any(|target| target == online));
        manager.set_runtime_downloads_allowed(false);
        assert!(!manager.search_results().any(|target| target == online));
        let denied = manager.update(ManagerMessage::Reveal(online));
        assert!(iced_runtime::task::into_stream(denied).is_none());
        assert!(!manager.local.busy());
        let _ = manager.update(ManagerMessage::SearchEscape);

        let _ = manager.update(ManagerMessage::Local(Message::Mlx(Action::ReviewRemoval)));
        let destination = Setting::Mlx(MlxSetting::ConfirmRemoval);
        let _ = manager.update(ManagerMessage::Search(
            "confirm managed MLX model removal".into(),
        ));
        assert_eq!(
            manager.search_results().next(),
            Some(Target::Setting(destination))
        );
        let _ = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(manager.revealed.setting, Some(destination));
        assert!(
            matches!(manager.local.mlx.review.as_ref(), Some(Review::Remove(model)) if model == &choice)
        );
        assert!(!manager.local.busy());
        let _ = manager.update(ManagerMessage::Local(Message::Mlx(Action::Discard)));
        assert!(manager.local.mlx.review.is_none());
        assert!(manager.local.mlx.choice() == Some(&choice));
    }

    #[test]
    fn mlx_never_unload_round_trips_through_model_settings() {
        use iced::futures::{StreamExt, executor::block_on};

        if !mlx_runtime::supported() {
            return;
        }
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let stop = AtomicBool::new(false);
        let source = mlx_models::test_fixture(&root);
        let review = mlx_models::Review::prepare(&source, &stop).unwrap();
        let registry = mlx_models::Registry::new(&root);
        let model = registry.import(&review, &stop).unwrap();
        let existing = registry.configure(&model.id, &model.name, 0).unwrap();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        manager.local.mlx.add_hub_model(existing, &root);
        assert!(manager.local.mlx.never_unload);
        assert_eq!(manager.local.mlx.idle_value(), Some(0));
        assert_eq!(
            manager.local.mlx.idle,
            mlx_models::DEFAULT_IDLE_MINUTES.to_string()
        );

        let send = |manager: &mut Manager, action| {
            manager.update(ManagerMessage::Local(Message::Mlx(action)))
        };
        let save = |manager: &mut Manager, expected| {
            let task = send(manager, Action::Configure);
            let mut events = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(event)) = block_on(events.next()) else {
                panic!("expected the MLX settings result");
            };
            let _ = manager.update(event);
            assert_eq!(
                manager.local.status.as_deref(),
                Some("Saved the MLX model settings.")
            );
            let stored = registry.open(&model.id).unwrap();
            assert_eq!(stored.model.idle_minutes, expected);
            assert_eq!(manager.local.mlx.idle_value(), Some(expected));
        };
        let _ = send(&mut manager, Action::NeverUnload(false));
        let _ = send(&mut manager, Action::Edit(Field::Idle, "1441".into()));
        assert_eq!(manager.local.mlx.idle_value(), None);
        let _ = send(&mut manager, Action::Edit(Field::Idle, "37".into()));
        let _ = send(&mut manager, Action::NeverUnload(true));
        let _ = send(&mut manager, Action::Edit(Field::Idle, "9".into()));
        let _ = send(&mut manager, Action::NeverUnload(false));
        assert_eq!(manager.local.mlx.idle_value(), Some(37));
        save(&mut manager, 37);

        let hint = Hint {
            id: "java-model".into(),
            name: model.name.clone(),
            published_at: None,
            metadata: Metadata::Mlx {
                context: 4096,
                idle_minutes: 0,
                quantization: None,
            },
        };
        assert!(manager.local.mlx.stage_java_settings(&hint));
        let idle = crate::ai_manager::search::Target::Setting(Setting::Mlx(MlxSetting::Idle));
        let _ = manager.update(ManagerMessage::Search("MLX idle timeout".into()));
        assert!(!manager.search_results().any(|target| target == idle));
        let never =
            crate::ai_manager::search::Target::Setting(Setting::Mlx(MlxSetting::NeverUnload));
        let _ = manager.update(ManagerMessage::Search("never unload MLX".into()));
        assert!(manager.search_results().any(|target| target == never));
        let _ = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(
            manager.revealed.setting,
            Some(Setting::Mlx(MlxSetting::NeverUnload))
        );
        save(&mut manager, 0);
    }

    #[test]
    fn mlx_connectivity_test_profile_uses_a_short_output_limit() {
        let profile = crate::snippets::tests::Profile::new();
        let id = "0123456789abcdef0123456789abcdef";
        let test = test_profile(&profile.0, id).unwrap();
        assert_eq!(test.usage_estimate_profile().0, id);
        assert_eq!(test.effective_output_limit(), 32);
    }

    #[test]
    fn mlx_runtime_removal_review_obeys_manager_escape_and_close() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let packages = Directory::open(&root, false)
            .unwrap()
            .private_child("mlx-runtime", true)
            .unwrap()
            .private_child("installations", true)
            .unwrap();
        let package = "package-0123456789abcdef0123456789abcdef";
        packages.private_child(package, true).unwrap();
        let plan = Arc::new(
            mlx_runtime::inspect_removal(&root, &AtomicBool::new(false))
                .unwrap()
                .unwrap(),
        );
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root,
            Vec::new(),
            true,
            None,
        );
        manager.page = Page::LocalModels;
        let _ = manager.update(ManagerMessage::Features(false));
        let completed = |generation| {
            ManagerMessage::Local(Message::Finished(
                generation,
                Ok(Output::Mlx(ResultValue::RuntimeRemovalReviewed(Some(
                    Arc::clone(&plan),
                )))),
            ))
        };
        let (cancel, requested) = runtime_package::Cancel::test_token();
        manager.local.request = Some(Request::RuntimeRemovalReview { _cancel: cancel });
        let generation = manager.local.generation;
        let _ = manager.update(manager.escape_message());
        assert!(requested.load(Ordering::Acquire));
        let _ = manager.update(completed(generation));
        assert!(manager.child_close_message().is_none());

        let (cancel, _) = runtime_package::Cancel::test_token();
        manager.local.request = Some(Request::RuntimeRemovalReview { _cancel: cancel });
        let _ = manager.update(completed(manager.local.generation));
        assert!(manager.local.mlx.runtime_removal_open());
        assert!(!Action::Confirm.requires_ai(&manager.local.mlx));
        let close = manager.child_close_message().expect("review owns close");
        let _ = manager.update(close);
        assert!(!manager.local.mlx.runtime_removal_open());
        assert!(manager.child_close_message().is_none());
        assert!(packages.private_child(package, false).is_ok());
    }
}
