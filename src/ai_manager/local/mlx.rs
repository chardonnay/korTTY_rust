//! Explicit local installation and lifecycle controls for Apple MLX.

use iced::widget::{button, column, pick_list, row, text, text_input};
use iced::{Element, Length, Task};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::local_ai::{self, mlx_models, mlx_runtime, runtime_package};
use mlx_runtime::{InstallationState, online};

use super::{Dialog, Message, Output, limited};
use crate::ai_manager::profiles::picker::DismissOnEscape as _;

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
    Refresh,
    ReviewRuntime,
    CheckOnline,
    SelectOnline(online::Choice),
    ReviewOnline,
    Unpin(String),
    ReviewRuntimeRemoval,
    ReviewModel,
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
    Model(Arc<mlx_models::Review>),
    Remove(Choice),
    RemoveRuntime(Arc<mlx_runtime::RemovalPlan>),
}

impl Review {
    fn removal(&self) -> bool {
        matches!(self, Self::Remove(_) | Self::RemoveRuntime(_))
    }
}

#[derive(Clone)]
pub(crate) enum ResultValue {
    Runtime(Arc<mlx_runtime::Review>),
    OnlineCatalog(Arc<online::Catalog>),
    OnlineInstalled(online::InstallOutcome),
    Unpinned(InstallationState),
    Catalog(Arc<mlx_runtime::CatalogReview>),
    Model(Arc<mlx_models::Review>),
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
    review: Option<Review>,
    choices: Vec<Choice>,
    selected: Option<String>,
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

    pub(super) fn refresh_status(&mut self, profile: &Path) {
        self.status = self
            .selected
            .as_deref()
            .map(|id| runtime_package::mlx_model_status(profile, id));
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
            ResultValue::Model(review) => {
                self.review = Some(Review::Model(review));
                "Review the local files before importing the MLX model.".into()
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
                notice
            }
        }
    }

    fn select(&mut self, choice: Option<Choice>) {
        self.selected = choice.as_ref().map(|choice| choice.id.clone());
        self.name = choice
            .as_ref()
            .map(|choice| choice.name.clone())
            .unwrap_or_default();
        self.idle = choice
            .as_ref()
            .map(|choice| choice.idle_minutes.to_string())
            .unwrap_or_default();
    }

    pub(super) fn view(
        &self,
        can_edit: bool,
        ai_allowed: bool,
        can_review_runtime_removal: bool,
        runtime_downloads_allowed: bool,
    ) -> Option<Element<'_, Message>> {
        if !mlx_runtime::supported() {
            return None;
        }
        let editing = can_edit && self.review.is_none();
        let can_request = editing && ai_allowed;
        let can_download = can_request && runtime_downloads_allowed;
        let field = |placeholder, value, field| {
            text_input(placeholder, value).on_input_maybe(
                editing.then_some(move |value| Message::Mlx(Action::Edit(field, value))),
            )
        };
        let action = |label, action, enabled: bool| {
            button(label).on_press_maybe(enabled.then_some(Message::Mlx(action)))
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
                action("Follow stable MLX releases", Action::Unpin(pin.to_owned()), editing),
            ].spacing(8).wrap()),
            text("The signed stable MLX catalog is hosted in the korTTY runtime release repository on GitHub.").size(13),
            (!runtime_downloads_allowed).then(|| text("Managed policy blocks runtime downloads.").size(13)),
            action("Check signed MLX releases", Action::CheckOnline, can_download),
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
                    picker,
                    action("Review MLX download…", Action::ReviewOnline, can_download && self.online_choice.is_some()),
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
                        action("Cancel", Action::Discard, can_edit),
                        action("Download and install MLX runtime", Action::ConfirmOnline(*ticket), can_edit && ai_allowed && runtime_downloads_allowed),
                    ].spacing(8).wrap(),
                ].spacing(8))
            }),
            field("Absolute path to mlx-runtime-index-v1.json", &self.paths[0], Field::Index),
            field("Absolute path to the detached signature", &self.paths[1], Field::Signature),
            field("Absolute path to the signed MLX package ZIP (optional for catalog review)", &self.paths[2], Field::Archive),
            row![
                action("Refresh MLX", Action::Refresh, editing),
                action("Review signed runtime…", Action::ReviewRuntime, can_request && self.runtime_paths_ready()),
                action("Remove installed MLX runtime…", Action::ReviewRuntimeRemoval, editing && can_review_runtime_removal),
            ].spacing(8).wrap(),
            field("Absolute path to a local MLX model directory", &self.source, Field::Source),
            action("Review model import…", Action::ReviewModel, can_request && Path::new(self.source.trim()).is_absolute()),
            model_pick,
            selected.map(|choice| column![
                text(format!("{} bytes · {}", choice.bytes, self.status.map_or_else(|| "Checking status".into(), |status| status.to_string()))).size(13),
                row![
                    action("Start", Action::Start, can_request),
                    action("Test model", Action::Test, can_request),
                    action("Stop", Action::Stop, editing),
                    action("Remove model…", Action::ReviewRemoval, editing),
                ].spacing(8).wrap(),
                field("Model display name", &self.name, Field::Name),
                field("Idle minutes (0 keeps the process running)", &self.idle, Field::Idle),
                action("Save MLX model settings", Action::Configure, can_request && !self.name.trim().is_empty() && self.idle_value().is_some()),
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
                    Review::Model(review) => (
                        "Import a private copy of these MLX files?",
                        format!("{} · {} bytes\n{}\n{}", review.model().name, review.model().bytes, review.source().display(), review.asset_names().collect::<Vec<_>>().join("\n")),
                        "Import MLX model",
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
                    button("Cancel").on_press_maybe(can_edit.then_some(cancel)),
                    action(confirm, Action::Confirm, can_edit && (ai_allowed || review.removal())),
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
        self.idle
            .trim()
            .parse()
            .ok()
            .filter(|minutes| *minutes <= 1_440)
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
                Some(Review::Model(review)) => runtime_package::work(move |cancelled| {
                    let model = mlx_models::Registry::new(&profile)
                        .import(&review, cancelled)
                        .map_err(|error| error.to_string())?;
                    snapshot(&profile, format!("Imported MLX model {}.", model.name))
                }),
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
                        .map(|review| ResultValue::Model(Arc::new(review)))
                        .map_err(|error| error.to_string())
                })
            }
            Action::Start | Action::Test | Action::Stop | Action::Configure => {
                let Some(choice) = self.mlx.choice().cloned() else {
                    return Task::none();
                };
                let name = self.mlx.name.trim().to_owned();
                let idle = self.mlx.idle_value();
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
                                idle.ok_or("Enter an MLX idle timeout from 0 to 1440 minutes.")?;
                            runtime_package::with_stopped_mlx_model(&profile, &choice.id, || {
                                mlx_models::Registry::new(&profile)
                                    .configure(&choice.id, &name, idle)
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
