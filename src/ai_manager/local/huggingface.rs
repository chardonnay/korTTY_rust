//! Explicit Hub search, file review and managed-model download controls.

mod browser;

pub(crate) use browser::{FormatFilter, SortColumn};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Weak};

use iced::Element;
use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};

use crate::clipboard_policy::AI_MODEL_DOWNLOADS_DISABLED;
use crate::connection_window::CredentialChoice;
use crate::local_ai::managed_models::Purpose;
use crate::local_ai::managed_models::huggingface::{
    self as hub, Continuation, Downloaded, Format, GIB, MlxDownload, MlxDownloaded,
    MlxPackageReview, ModelListing, Pause, Plan, Progress, Recommendation, RecommendationRole,
    Repository, Review, SearchPage, SearchPages, Selection, recommendations,
};
use crate::local_ai::runtime_package;
use crate::vault_window::VaultWindow;
use iced::Task;

use super::{Dialog, Message};
use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{HubSetting, Reveal, Setting};

#[derive(Clone)]
pub(crate) enum Action {
    Query(String),
    Token(Token),
    Search,
    LoadMore(Vec<Continuation>),
    Format(FormatFilter),
    Sort(SortColumn),
    ShowAllModels(bool),
    Repository(Repository),
    Review,
    RecommendationRole(RecommendationRole),
    Recommendation(Arc<Recommendation>),
    ReviewRecommendation,
    Artifact(Selection),
    Purpose(Purpose),
    Download,
    DownloadMlx,
    ReadPausedMlx,
    ResumeMlx,
    DiscardPausedMlx,
    ReadPaused,
    Resume,
    DiscardPaused,
    CancelDiscard,
    Pause,
    Progress(u64, Progress),
}

impl Action {
    fn needs_token(&self) -> bool {
        matches!(
            self,
            Self::Search
                | Self::Format(_)
                | Self::LoadMore(_)
                | Self::Review
                | Self::ReviewRecommendation
                | Self::Download
                | Self::DownloadMlx
                | Self::ResumeMlx
                | Self::Resume
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Output {
    Search(u64, Vec<Continuation>, SearchPages),
    Review(Arc<Review>),
    MlxReview(u64, Option<Weak<()>>, Arc<MlxPackageReview>),
    MlxDownload(MlxDownloaded),
    MlxPaused(Option<Arc<MlxPackageReview>>),
    MlxDiscarded(bool),
    Recommendation(RecommendationRole, Arc<Recommendation>, Arc<Review>),
    Download(Downloaded),
    Paused(Option<Plan>),
    Discarded(bool),
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum Token {
    Anonymous,
    Saved(CredentialChoice),
}

impl fmt::Display for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anonymous => formatter.write_str("Anonymous"),
            Self::Saved(choice) => formatter.write_str(&choice.label),
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    pub(super) query: String,
    pub(super) token: Option<String>,
    repositories: Vec<ModelListing>,
    sort: browser::Sort,
    show_all_models: bool,
    sizing: Sizing,
    next_pages: Vec<Continuation>,
    format_filter: FormatFilter,
    loaded_pages: HashSet<Continuation>,
    search_revision: u64,
    pub(super) repository: Option<Repository>,
    pub(super) review: Option<Arc<Review>>,
    mlx_review: Option<(Arc<MlxPackageReview>, Option<Weak<()>>)>,
    mlx_paused: Option<Arc<MlxPackageReview>>,
    mlx_discard_confirmed: bool,
    pub(super) file: Option<Selection>,
    pub(super) purpose: Purpose,
    pub(super) paused: Option<Plan>,
    pub(super) pause: Option<Pause>,
    pub(super) progress: Option<Progress>,
    pub(super) discard_confirmed: bool,
    recommendations: Recommendations,
    reviewed_role: Option<RecommendationRole>,
}

const SIZE_LOOKUPS: usize = 4;

#[derive(Default)]
struct Sizing {
    tried: HashSet<Repository>,
    pending: HashMap<Repository, runtime_package::Cancel>,
    credential_revision: Option<Weak<()>>,
}

struct Recommendations {
    catalog: Option<Arc<crate::local_ai::model_catalog::Catalog>>,
    memory: Option<u64>,
    role: RecommendationRole,
    preferred: Option<Arc<Recommendation>>,
}

impl Default for Recommendations {
    fn default() -> Self {
        Self {
            catalog: None,
            memory: crate::host_memory::physical_bytes(),
            role: RecommendationRole::default(),
            preferred: None,
        }
    }
}

impl Recommendations {
    fn choices(&self) -> Vec<Arc<Recommendation>> {
        recommendations(self.catalog.as_deref(), self.role, self.memory)
    }

    fn selected(&self) -> Option<Arc<Recommendation>> {
        self.preferred
            .clone()
            .or_else(|| self.choices().into_iter().next())
    }

    fn view(&self, enabled: bool, access_selected: bool, reveal: &Reveal) -> Element<'_, Message> {
        let memory = self.memory.map_or_else(
            || "System memory could not be detected. Showing baseline models.".to_owned(),
            |bytes| {
                format!(
                    "Detected system memory · {:.1} GiB",
                    bytes as f64 / GIB as f64
                )
            },
        );
        let selected = self.selected();
        let role: Element<'_, Message> = if enabled {
            pick_list(RecommendationRole::ALL, Some(self.role), |role| {
                Message::HuggingFace(Action::RecommendationRole(role))
            })
            .dismiss_on_escape()
        } else {
            text(self.role.to_string()).into()
        };
        let model: Option<Element<'_, Message>> = selected.clone().map(|recommendation| {
            if enabled {
                pick_list(self.choices(), Some(recommendation), |recommendation| {
                    Message::HuggingFace(Action::Recommendation(recommendation))
                })
                .dismiss_on_escape()
            } else {
                text(recommendation.to_string()).into()
            }
        });
        column![
            text("Recommended GGUF models").size(16),
            text(memory).size(13),
            row![
                reveal.action(Setting::Hub(HubSetting::RecommendationRole), role),
                model.map(|model| reveal.action(Setting::Hub(HubSetting::RecommendedModel), model)),
            ]
            .spacing(8)
            .wrap(),
            selected.as_ref().map(|recommendation| column![
                (recommendation.minimum_system_memory_bytes != 0).then(|| text(format!(
                    "Recommended for {:.1} GiB or more",
                    recommendation.minimum_system_memory_bytes as f64 / GIB as f64
                ))
                .size(13)),
                text(recommendation.revision.as_ref().map_or_else(
                    || "The exact commit is resolved during review.".into(),
                    |revision| format!("Pinned commit · {revision}"),
                ))
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(4)),
            reveal.action(
                Setting::Hub(HubSetting::ReviewRecommendation),
                button("Review recommended model").on_press_maybe(
                    (enabled && access_selected && selected.is_some())
                        .then_some(Message::HuggingFace(Action::ReviewRecommendation))
                )
            ),
        ]
        .spacing(6)
        .into()
    }
}

impl State {
    fn reset_search(&mut self) {
        self.search_revision = self.search_revision.wrapping_add(1);
        self.sizing = Sizing::default();
        self.repositories.clear();
        self.next_pages.clear();
        self.loaded_pages.clear();
        self.repository = None;
        self.clear_review();
    }

    pub(super) fn suspend(&mut self) {
        *self = Self {
            search_revision: self.search_revision.wrapping_add(1),
            ..Self::default()
        };
    }

    fn apply_search_page(&mut self, requested: Option<Continuation>, page: SearchPage) {
        if let Some(requested) = requested {
            self.loaded_pages.insert(requested);
        }
        if let Some(next) = page.next.filter(|next| !self.loaded_pages.contains(next)) {
            self.next_pages.push(next);
        }
        let mut seen: HashSet<_> = self
            .repositories
            .iter()
            .map(|model| model.repository.id().to_ascii_lowercase())
            .collect();
        self.repositories.extend(
            page.repositories
                .into_iter()
                .filter(|model| seen.insert(model.repository.id().to_ascii_lowercase())),
        );
    }

    fn selected_listing(&self) -> Option<&ModelListing> {
        let selected = self.repository.as_ref()?;
        self.repositories
            .iter()
            .find(|listing| &listing.repository == selected)
    }

    fn selected_mlx(&self) -> bool {
        self.selected_listing()
            .is_some_and(|model| model.format == Format::Mlx)
    }

    pub(super) fn install_model_catalog(
        &mut self,
        catalog: Option<Arc<crate::local_ai::model_catalog::Catalog>>,
    ) {
        if self.recommendations.catalog == catalog {
            return;
        }
        self.recommendations.catalog = catalog;
        self.recommendations.preferred = None;
        if self.reviewed_role.is_some() {
            self.clear_review();
        }
    }

    pub(super) fn clear_review(&mut self) {
        self.review = None;
        self.mlx_review = None;
        self.file = None;
        self.reviewed_role = None;
    }

    fn install_purpose(&self) -> Purpose {
        self.reviewed_role
            .map_or(self.purpose, RecommendationRole::purpose)
    }

    pub(super) fn install_plan(&self) -> Result<Plan, String> {
        let review = self
            .review
            .as_ref()
            .ok_or("Review a Hugging Face repository first.")?;
        let file = self.file.as_ref().ok_or("Choose a verified GGUF file.")?;
        review.select(file, self.install_purpose())
    }

    pub(super) fn view(
        &self,
        enabled: bool,
        model_downloads_allowed: bool,
        credentials: Vec<CredentialChoice>,
        reveal: &Reveal,
    ) -> Element<'_, Message> {
        if !model_downloads_allowed {
            return column![
                text("Hugging Face Models").size(17),
                text(AI_MODEL_DOWNLOADS_DISABLED).size(13),
                self.paused_actions(enabled, reveal),
            ]
            .spacing(8)
            .into();
        }
        let tokens = std::iter::once(Token::Anonymous)
            .chain(credentials.into_iter().map(Token::Saved))
            .collect::<Vec<_>>();
        let selected = tokens
            .iter()
            .find(|choice| match choice {
                Token::Anonymous => self.token.is_none(),
                Token::Saved(choice) => Some(choice.id.as_str()) == self.token.as_deref(),
            })
            .cloned();
        let authentication: Element<'_, Message> = if enabled {
            pick_list(tokens, selected.clone(), |token| {
                Message::HuggingFace(Action::Token(token))
            })
            .placeholder("Select Hugging Face access")
            .dismiss_on_escape()
        } else {
            text(selected.as_ref().map_or_else(
                || "Unlock Vault to use the selected token".into(),
                ToString::to_string,
            ))
            .into()
        };
        let review = self.review.as_ref().map(|review| {
            let file: Element<'_, Message> = if enabled && !review.files.is_empty() {
                pick_list(review.files.as_slice(), self.file.as_ref(), |artifact| {
                    Message::HuggingFace(Action::Artifact(artifact))
                })
                .placeholder("Choose a reviewed GGUF file")
                .dismiss_on_escape()
            } else {
                text(self.file.as_ref().map_or_else(
                    || "No complete GGUF with a published SHA-256 for each file.".into(),
                    ToString::to_string,
                ))
                .into()
            };
            let purpose: Element<'_, Message> = if enabled && self.reviewed_role.is_none() {
                pick_list(Purpose::ALL, Some(self.purpose), |purpose| {
                    Message::HuggingFace(Action::Purpose(purpose))
                })
                .dismiss_on_escape()
            } else {
                text(self.install_purpose().to_string()).into()
            };
            column![
                text(format!("Reviewed commit · {}", review.revision)).size(13),
                reveal.wrap(Setting::Hub(HubSetting::Artifact), file),
                reveal.action(
                    Setting::Hub(HubSetting::Purpose),
                    row![text("Model purpose"), purpose].spacing(8)
                ),
                self.install_plan().ok().map(|plan| plan_summary(&plan)),
                reveal.action(
                    Setting::Hub(HubSetting::Download),
                    button("Download and register reviewed model").on_press_maybe(
                        (enabled
                            && self.file.is_some()
                            && self.paused.is_none()
                            && selected.is_some())
                        .then_some(Message::HuggingFace(Action::Download))
                    )
                ),
            ]
            .spacing(8)
        });
        let query_setting = Setting::Hub(HubSetting::Query);
        let query = reveal.input(
            query_setting,
            enabled,
            text_input("Search Hugging Face models", &self.query)
                .on_input_maybe(
                    enabled.then_some(|query| Message::HuggingFace(Action::Query(query))),
                )
                .on_submit_maybe(
                    (enabled && selected.is_some()).then_some(Message::HuggingFace(Action::Search)),
                ),
        );
        let format_picker = crate::local_ai::mlx_runtime::supported().then(|| {
            let picker: Element<'_, Message> = if enabled {
                pick_list(FormatFilter::ALL, Some(self.format_filter), |filter| {
                    Message::HuggingFace(Action::Format(filter))
                })
                .dismiss_on_escape()
            } else {
                text(self.format_filter.to_string()).into()
            };
            reveal.action(Setting::Hub(HubSetting::Format), picker)
        });
        column![
            text("Hugging Face Models").size(17),
            text("Search the Hub over HTTPS and review an immutable commit, file size, and SHA-256 before downloading. A selected vault token goes to huggingface.co.").size(13),
            reveal.action(Setting::Hub(HubSetting::Access), row![text("Hub access"), authentication].spacing(8)),
            self.recommendations.view(enabled, selected.is_some(), reveal),
            row![
                reveal.wrap(query_setting, query),
                reveal.action(Setting::Hub(HubSetting::Search), button("Search Hugging Face").on_press_maybe((enabled && selected.is_some()).then_some(Message::HuggingFace(Action::Search)))),
                format_picker,
                (!self.next_pages.is_empty()).then(|| reveal.action(Setting::Hub(HubSetting::LoadMore),
                    button("Load more results").on_press_maybe((enabled && selected.is_some())
                        .then(|| Message::HuggingFace(Action::LoadMore(self.next_pages.clone())))))),
            ].spacing(8).wrap(),
            (!self.repositories.is_empty()).then(|| reveal.action(
                Setting::Hub(HubSetting::ShowAllModels),
                checkbox(self.show_all_models).label("Show all models")
                    .on_toggle_maybe(enabled.then_some(|show| Message::HuggingFace(Action::ShowAllModels(show)))),
            )),
            (!self.repositories.is_empty()).then(|| reveal.wrap(Setting::Hub(HubSetting::Repository), self.browser_view(enabled))),
            (!self.selected_mlx()).then(|| reveal.action(Setting::Hub(HubSetting::ReviewRepository), button("Review repository").on_press_maybe((enabled && self.repository.is_some() && selected.is_some()).then_some(Message::HuggingFace(Action::Review))))),
            self.selected_mlx().then(|| reveal.action(Setting::Hub(HubSetting::ReviewMlx),
                button("Review MLX package").on_press_maybe((enabled && selected.is_some()).then_some(Message::HuggingFace(Action::Review))))),
            review,
            self.mlx_review.as_ref().map(|(review, _)| column![
                reveal.wrap(Setting::Hub(HubSetting::MlxFiles), mlx_package_view(review)),
                reveal.action(Setting::Hub(HubSetting::DownloadMlx),
                    button("Download and register MLX model").on_press_maybe((enabled && selected.is_some() && self.mlx_paused.is_none()).then_some(Message::HuggingFace(Action::DownloadMlx)))),
            ].spacing(6)),
            self.paused_actions(enabled, reveal),
            self.paused.as_ref().map(|plan| column![
                text("Paused download").size(15),
                plan_summary(plan),
                reveal.action(Setting::Hub(HubSetting::Resume), button("Resume reviewed download").on_press_maybe((enabled && selected.is_some()).then_some(Message::HuggingFace(Action::Resume)))),
            ].spacing(6)),
            self.mlx_paused.as_ref().map(|review| column![
                text("Paused MLX download").size(15),
                mlx_package_view(review),
                reveal.action(Setting::Hub(HubSetting::ResumeMlx),
                    button("Resume reviewed MLX download").on_press_maybe((enabled && selected.is_some()).then_some(Message::HuggingFace(Action::ResumeMlx)))),
            ].spacing(6)),
            self.progress.map(|progress| row![
                text(format!("{} / {} MiB", progress.received / (1024 * 1024), progress.total.div_ceil(1024 * 1024))).size(13),
                reveal.action(Setting::Hub(HubSetting::Pause), button("Pause download").on_press_maybe((progress.resumable && self.pause.is_some()).then_some(Message::HuggingFace(Action::Pause)))),
            ].spacing(8)),
        ].spacing(8).into()
    }

    fn paused_actions(&self, enabled: bool, reveal: &Reveal) -> Element<'_, Message> {
        let action = |setting, button| reveal.action(Setting::Hub(setting), button);
        let gguf = row![
            action(
                HubSetting::FindPaused,
                button("Find paused download")
                    .style(button::secondary)
                    .on_press_maybe(enabled.then_some(Message::HuggingFace(Action::ReadPaused)))
            ),
            action(
                HubSetting::DiscardPaused,
                button(if self.discard_confirmed {
                    "Confirm discard"
                } else {
                    "Discard paused download…"
                })
                .style(button::secondary)
                .on_press_maybe(enabled.then_some(Message::HuggingFace(Action::DiscardPaused)))
            ),
            (enabled && self.discard_confirmed).then(|| action(
                HubSetting::KeepPaused,
                button("Keep download")
                    .style(button::secondary)
                    .on_press(Message::HuggingFace(Action::CancelDiscard))
            )),
        ]
        .spacing(8)
        .wrap();
        let mlx = crate::local_ai::mlx_runtime::supported().then(|| {
            row![
                action(
                    HubSetting::FindPausedMlx,
                    button("Find paused MLX download")
                        .style(button::secondary)
                        .on_press_maybe(
                            enabled.then_some(Message::HuggingFace(Action::ReadPausedMlx))
                        )
                ),
                action(
                    HubSetting::DiscardPausedMlx,
                    button(if self.mlx_discard_confirmed {
                        "Confirm MLX discard"
                    } else {
                        "Discard paused MLX download…"
                    })
                    .style(button::secondary)
                    .on_press_maybe(
                        enabled.then_some(Message::HuggingFace(Action::DiscardPausedMlx))
                    )
                ),
                (enabled && self.mlx_discard_confirmed).then(|| button("Keep MLX download")
                    .style(button::secondary)
                    .on_press(Message::HuggingFace(Action::CancelDiscard))),
            ]
            .spacing(8)
            .wrap()
        });
        column![gguf, mlx].spacing(8).into()
    }
}

impl Dialog {
    pub(in crate::ai_manager) fn can_reveal_hub_setting(
        &self,
        setting: HubSetting,
        editable: bool,
    ) -> bool {
        let enabled = editable && self.request.is_none() && self.runtime_removal.is_none();
        match setting {
            HubSetting::FindPaused | HubSetting::DiscardPaused => true,
            HubSetting::FindPausedMlx | HubSetting::DiscardPausedMlx => {
                crate::local_ai::mlx_runtime::supported()
            }
            HubSetting::KeepPaused => enabled && self.hf.discard_confirmed,
            _ if !self.model_downloads_allowed => false,
            HubSetting::RecommendedModel => self.hf.recommendations.selected().is_some(),
            HubSetting::LoadMore => !self.hf.next_pages.is_empty(),
            HubSetting::Format => crate::local_ai::mlx_runtime::supported(),
            HubSetting::ReviewRepository => !self.hf.selected_mlx(),
            HubSetting::ReviewMlx => self.hf.selected_mlx(),
            HubSetting::MlxFiles => self.hf.mlx_review.is_some(),
            HubSetting::DownloadMlx => self.hf.mlx_review.is_some(),
            HubSetting::ShowAllModels => !self.hf.repositories.is_empty(),
            HubSetting::Repository => !self.hf.repositories.is_empty(),
            HubSetting::Artifact | HubSetting::Purpose | HubSetting::Download => {
                self.hf.review.is_some()
            }
            HubSetting::Resume => self.hf.paused.is_some(),
            HubSetting::ResumeMlx => self.hf.mlx_paused.is_some(),
            HubSetting::Pause => self.hf.progress.is_some(),
            _ => true,
        }
    }

    pub(in crate::ai_manager) fn set_model_downloads_allowed(&mut self, allowed: bool) {
        if self.model_downloads_allowed && !allowed {
            if let Some(pause) = self.hf.pause.take() {
                pause.request();
            }
            self.hf.reset_search();
        }
        self.model_downloads_allowed = allowed;
    }

    pub(in crate::ai_manager) fn update_huggingface(
        &mut self,
        action: Action,
        ai_allowed: bool,
        vault: Option<&VaultWindow>,
    ) -> Task<Message> {
        match action {
            Action::Progress(generation, progress) => {
                if ai_allowed && self.generation == generation && self.hf.pause.is_some() {
                    self.hf.progress = Some(progress);
                }
                return Task::none();
            }
            Action::Pause => {
                if ai_allowed
                    && self.hf.progress.is_some_and(|progress| progress.resumable)
                    && let Some(pause) = self.hf.pause.take()
                {
                    pause.request();
                    self.status = Some("Pausing the Hugging Face download…".into());
                }
                return Task::none();
            }
            _ => {}
        }
        if !ai_allowed
            || self.request.is_some()
            || self.backend_repair.is_some()
            || self.runtime_removal.is_some()
        {
            return Task::none();
        }
        if !self.model_downloads_allowed
            && !matches!(
                action,
                Action::ReadPaused
                    | Action::DiscardPaused
                    | Action::ReadPausedMlx
                    | Action::DiscardPausedMlx
                    | Action::CancelDiscard
            )
        {
            self.status = Some(AI_MODEL_DOWNLOADS_DISABLED.into());
            return Task::none();
        }
        if matches!(&action, Action::LoadMore(continuations) if continuations.is_empty() || continuations != &self.hf.next_pages)
            || matches!(action, Action::Format(format) if !crate::local_ai::mlx_runtime::supported() || format == self.hf.format_filter)
        {
            return Task::none();
        }
        if !matches!(action, Action::DiscardPaused) {
            self.hf.discard_confirmed = false;
        }
        if !matches!(action, Action::DiscardPausedMlx) {
            self.hf.mlx_discard_confirmed = false;
        }
        let token = match self.hf.token.as_deref().filter(|_| action.needs_token()) {
            None => None,
            Some(id) => match vault
                .ok_or("Unlock the vault to use the selected Hugging Face token.")
                .and_then(|vault| vault.huggingface_token(id))
            {
                Ok(token) => Some(token),
                Err(error) => {
                    self.status = Some(error.into());
                    return Task::none();
                }
            },
        };
        match action {
            Action::Query(query) => {
                let query = super::limited(&query, 256);
                if self.hf.query != query {
                    self.hf.query = query.into();
                    self.hf.reset_search();
                }
            }
            Action::Token(token) => {
                self.hf.token = match token {
                    Token::Anonymous => None,
                    Token::Saved(choice) => Some(choice.id),
                };
                self.hf.reset_search();
            }
            Action::Search | Action::Format(_) => {
                if let Action::Format(format) = action {
                    self.hf.format_filter = format;
                }
                self.hf.reset_search();
                if self.hf.token.is_some() {
                    self.hf.sizing.credential_revision =
                        vault.and_then(VaultWindow::credential_revision);
                }
                let revision = self.hf.search_revision;
                return self.run_huggingface(
                    hub::search(
                        self.hf.query.clone(),
                        self.hf.format_filter.formats(),
                        token,
                    ),
                    "Searching Hugging Face…",
                    move |pages| Output::Search(revision, Vec::new(), pages),
                );
            }
            Action::LoadMore(continuations) => {
                let revision = self.hf.search_revision;
                return self.run_huggingface(
                    hub::load_more(continuations.clone(), token),
                    "Loading more Hugging Face results…",
                    move |pages| Output::Search(revision, continuations, pages),
                );
            }
            Action::Sort(column) if !self.hf.repositories.is_empty() => self.hf.sort.select(column),
            Action::ShowAllModels(show) => self.hf.show_all_models = show,
            Action::Repository(repository)
                if self.hf.repository.as_ref() != Some(&repository)
                    && self
                        .hf
                        .repositories
                        .iter()
                        .any(|model| model.repository == repository) =>
            {
                self.hf.repository = Some(repository);
                self.hf.clear_review();
            }
            Action::Review => {
                if let Some(model) = self.hf.selected_listing() {
                    let repository = model.repository.clone();
                    let format = model.format;
                    self.hf.clear_review();
                    let revision = self.hf.search_revision;
                    return match format {
                        Format::Gguf => self.run_huggingface(
                            hub::review(repository, token),
                            "Reading immutable Hugging Face file metadata…",
                            Output::Review,
                        ),
                        Format::Mlx => {
                            let credentials = if let Some(token) = token {
                                let Some(vault_revision) =
                                    vault.and_then(VaultWindow::credential_revision)
                                else {
                                    self.status =
                                        Some("Unlock the vault to review this MLX package.".into());
                                    return Task::none();
                                };
                                Some((token, vault_revision))
                            } else {
                                None
                            };
                            let vault_revision =
                                credentials.as_ref().map(|(_, revision)| revision.clone());
                            self.run_huggingface(
                                hub::review_mlx(repository, credentials),
                                "Reviewing the immutable MLX asset list and checksums…",
                                move |review| Output::MlxReview(revision, vault_revision, review),
                            )
                        }
                    };
                }
            }
            Action::RecommendationRole(role) if self.hf.recommendations.role != role => {
                self.hf.recommendations.role = role;
                self.hf.recommendations.preferred = None;
                self.hf.clear_review();
            }
            Action::Recommendation(recommendation)
                if self.hf.recommendations.choices().contains(&recommendation)
                    && self.hf.recommendations.selected().as_ref() != Some(&recommendation) =>
            {
                self.hf.recommendations.preferred = Some(recommendation);
                self.hf.clear_review();
            }
            Action::ReviewRecommendation => {
                if let Some(recommendation) = self.hf.recommendations.selected() {
                    self.hf.clear_review();
                    let role = self.hf.recommendations.role;
                    return self.run_huggingface(
                        recommendation.review(token),
                        "Reading the pinned Hugging Face model metadata…",
                        move |review| Output::Recommendation(role, recommendation, review),
                    );
                }
            }
            Action::Artifact(artifact)
                if self
                    .hf
                    .review
                    .as_ref()
                    .is_some_and(|review| review.files.contains(&artifact)) =>
            {
                self.hf.file = Some(artifact);
            }
            Action::Purpose(purpose) if self.hf.reviewed_role.is_none() => {
                self.hf.purpose = purpose
            }
            Action::Download if self.hf.paused.is_none() => match self.hf.install_plan() {
                Ok(plan) => return self.download_huggingface(plan, token),
                Err(error) => self.status = Some(error),
            },
            Action::DownloadMlx if self.hf.mlx_paused.is_none() => {
                if let Some((review, reviewed_revision)) = self.hf.mlx_review.clone() {
                    let credentials = match (token, reviewed_revision) {
                        (None, None) => None,
                        (Some(token), Some(reviewed))
                            if vault
                                .and_then(VaultWindow::credential_revision)
                                .is_some_and(|current| Weak::ptr_eq(&reviewed, &current)) =>
                        {
                            Some((token, reviewed))
                        }
                        _ => {
                            self.hf.clear_review();
                            self.status = Some("The Hugging Face credential changed. Review the MLX package again.".into());
                            return Task::none();
                        }
                    };
                    return self.download_mlx(review, false, credentials);
                }
            }
            Action::ReadPausedMlx => {
                return self.run_huggingface(
                    hub::paused_mlx(self.profile_dir.clone()),
                    "Checking for a paused MLX download…",
                    Output::MlxPaused,
                );
            }
            Action::ResumeMlx => {
                if let Some(review) = self.hf.mlx_paused.clone() {
                    let credentials = if let Some(token) = token {
                        let Some(revision) = vault.and_then(VaultWindow::credential_revision)
                        else {
                            self.status =
                                Some("Unlock the vault to resume the MLX package.".into());
                            return Task::none();
                        };
                        Some((token, revision))
                    } else {
                        None
                    };
                    return self.download_mlx(review, true, credentials);
                }
            }
            Action::DiscardPausedMlx => {
                if self.hf.mlx_discard_confirmed {
                    self.hf.mlx_discard_confirmed = false;
                    return self.run_huggingface(
                        hub::discard_paused_mlx(self.profile_dir.clone()),
                        "Discarding the paused MLX download…",
                        Output::MlxDiscarded,
                    );
                }
                self.hf.mlx_discard_confirmed = true;
            }
            Action::ReadPaused => {
                return self.run_huggingface(
                    hub::paused(self.profile_dir.clone()),
                    "Checking for a paused Hugging Face download…",
                    Output::Paused,
                );
            }
            Action::Resume => {
                if let Some(plan) = self.hf.paused.clone() {
                    return self.download_huggingface(plan, token);
                }
            }
            Action::DiscardPaused => {
                if self.hf.discard_confirmed {
                    self.hf.discard_confirmed = false;
                    return self.run_huggingface(
                        hub::discard_paused(self.profile_dir.clone()),
                        "Discarding the paused Hugging Face download…",
                        Output::Discarded,
                    );
                }
                self.hf.discard_confirmed = true;
            }
            _ => {}
        }
        Task::none()
    }

    fn run_huggingface<T: Send + 'static>(
        &mut self,
        pending: Result<runtime_package::Pending<T>, String>,
        status: &'static str,
        output: impl FnOnce(T) -> Output + Send + 'static,
    ) -> Task<Message> {
        self.run_managed(pending, status, true, move |value| {
            super::Output::HuggingFace(output(value))
        })
    }

    pub(super) fn fill_hub_sizes(&mut self, vault: Option<&VaultWindow>) -> Task<Message> {
        if !self.model_downloads_allowed {
            return Task::none();
        }
        let revision = self.hf.search_revision;
        let slots = SIZE_LOOKUPS.saturating_sub(self.hf.sizing.pending.len());
        let candidates: Vec<_> = self
            .hf
            .repositories
            .iter()
            .filter(|model| {
                model.bytes.is_none() && !self.hf.sizing.tried.contains(&model.repository)
            })
            .take(slots)
            .map(|model| (model.repository.clone(), model.format))
            .collect();
        let mut tasks = Vec::new();
        for (repository, format) in candidates {
            let credentials = if let Some(id) = &self.hf.token {
                let Some(vault) = vault else {
                    break;
                };
                let Some(revision) = vault.credential_revision().filter(|current| {
                    self.hf
                        .sizing
                        .credential_revision
                        .as_ref()
                        .is_some_and(|reviewed| Weak::ptr_eq(reviewed, current))
                }) else {
                    break;
                };
                let Ok(token) = vault.huggingface_token(id) else {
                    break;
                };
                Some((token, revision))
            } else {
                None
            };
            self.hf.sizing.tried.insert(repository.clone());
            if let Ok(runtime_package::Pending { reply, cancel }) =
                hub::inspect_size(repository.clone(), format, credentials)
            {
                self.hf.sizing.pending.insert(repository.clone(), cancel);
                tasks.push(Task::perform(
                    async move { reply.recv().await.ok().and_then(Result::ok).flatten() },
                    move |bytes| Message::HubSizeResolved(revision, repository, bytes),
                ));
            }
        }
        Task::batch(tasks)
    }

    pub(super) fn resolve_hub_size(
        &mut self,
        revision: u64,
        repository: Repository,
        bytes: Option<u64>,
        vault: Option<&VaultWindow>,
    ) -> Task<Message> {
        if revision != self.hf.search_revision
            || self.hf.sizing.pending.remove(&repository).is_none()
        {
            return Task::none();
        }
        if self
            .hf
            .sizing
            .credential_revision
            .as_ref()
            .is_some_and(|revision| revision.strong_count() == 0)
        {
            self.hf.sizing.pending.clear();
            return Task::none();
        }
        if let Some(bytes) = bytes
            && let Some(model) = self
                .hf
                .repositories
                .iter_mut()
                .find(|model| model.repository == repository)
        {
            model.bytes.get_or_insert(bytes);
        }
        self.fill_hub_sizes(vault)
    }

    fn download_mlx(
        &mut self,
        review: Arc<MlxPackageReview>,
        resume: bool,
        credentials: Option<(kortty_vault::Secret, Weak<()>)>,
    ) -> Task<Message> {
        let download: MlxDownload =
            match hub::download_mlx(self.profile_dir.clone(), review, resume, credentials) {
                Ok(download) => download,
                Err(error) => {
                    self.status = Some(error);
                    return Task::none();
                }
            };
        self.hf.pause = Some(download.pause);
        self.hf.progress = None;
        let run = self.run_huggingface(
            Ok(download.pending),
            "Downloading the reviewed MLX package…",
            Output::MlxDownload,
        );
        let generation = self.generation;
        Task::batch([
            run,
            Task::run(download.progress, move |progress| {
                Message::HuggingFace(Action::Progress(generation, progress))
            }),
        ])
    }

    fn download_huggingface(
        &mut self,
        plan: Plan,
        token: Option<kortty_vault::Secret>,
    ) -> Task<Message> {
        let download = match hub::download(self.profile_dir.clone(), plan, token) {
            Ok(download) => download,
            Err(error) => {
                self.status = Some(error);
                return Task::none();
            }
        };
        self.hf.pause = Some(download.pause);
        self.hf.progress = None;
        let result = self.run_huggingface(
            Ok(download.pending),
            "Downloading the reviewed Hugging Face model…",
            Output::Download,
        );
        let generation = self.generation;
        Task::batch([
            result,
            Task::run(download.progress, move |progress| {
                Message::HuggingFace(Action::Progress(generation, progress))
            }),
        ])
    }

    pub(super) fn finish_huggingface(&mut self, output: Output) -> String {
        match output {
            Output::Search(..)
            | Output::Review(_)
            | Output::MlxReview(..)
            | Output::Recommendation(..)
                if !self.model_downloads_allowed =>
            {
                AI_MODEL_DOWNLOADS_DISABLED.into()
            }
            Output::Search(revision, requested, pages) => {
                if self.hf.search_revision != revision || self.hf.next_pages != requested {
                    return "The Hugging Face search changed. Search again for current results."
                        .into();
                }
                let mut errors = Vec::new();
                for (format, page) in pages {
                    match page {
                        Ok(page) => {
                            let cursor =
                                requested.iter().find(|next| next.format == format).cloned();
                            self.hf.next_pages.retain(|next| next.format != format);
                            self.hf.apply_search_page(cursor, page);
                        }
                        Err(error) => errors.push(format!("{format}: {error}")),
                    }
                }
                let count = self.hf.repositories.len();
                let mut status = format!("Found {count} Hugging Face repositories.");
                if !errors.is_empty() {
                    status.push(' ');
                    status.push_str(&errors.join(" · "));
                }
                status
            }
            Output::Review(review) => {
                let count = review.files.len();
                if let Some(bytes) = review.files.iter().map(Selection::bytes).min()
                    && let Some(model) = self
                        .hf
                        .repositories
                        .iter_mut()
                        .find(|model| model.repository == review.repository)
                {
                    model.bytes = Some(bytes);
                }
                self.hf.review = Some(review);
                format!("Reviewed {count} complete GGUF choices with published SHA-256 values.")
            }
            Output::MlxReview(revision, vault_revision, review) => {
                if self.hf.search_revision != revision
                    || !self.hf.selected_mlx()
                    || self.hf.repository.as_ref() != Some(&review.repository)
                {
                    return "The MLX selection changed. Review the current repository.".into();
                }
                if vault_revision
                    .as_ref()
                    .is_some_and(|revision| revision.strong_count() == 0)
                {
                    return "The Hugging Face credential changed. Review the MLX package again."
                        .into();
                }
                if let Some(model) = self
                    .hf
                    .repositories
                    .iter_mut()
                    .find(|model| model.repository == review.repository)
                {
                    model.bytes = Some(review.bytes);
                }
                let count = review.files.len();
                self.hf.mlx_review = Some((review, vault_revision));
                format!("Reviewed {count} MLX files at the selected commit.")
            }
            Output::Recommendation(role, recommendation, review) => {
                if self.hf.recommendations.role != role
                    || self.hf.recommendations.selected().as_ref() != Some(&recommendation)
                    || !recommendation.matches_review(&review)
                {
                    return "The recommendation changed. Review the model again.".into();
                }
                self.hf.file = review.files.first().cloned();
                self.hf.review = Some(review);
                self.hf.reviewed_role = Some(role);
                "Review the pinned file size and SHA-256 before downloading.".into()
            }
            Output::MlxDownload(MlxDownloaded::Installed {
                model,
                cleanup_warning,
                sync_warning,
            }) => {
                let mut notice = format!("Registered {}.", model.name);
                self.mlx.add_hub_model(model, &self.profile_dir);
                self.hf.mlx_paused = None;
                self.hf.clear_review();
                if cleanup_warning {
                    notice.push_str(" Could not clean up the private download stage.");
                }
                if sync_warning {
                    notice.push_str(" Could not synchronize the model registry directory.");
                }
                notice
            }
            Output::MlxDownload(MlxDownloaded::Paused {
                review,
                interrupted,
            }) => {
                self.hf.mlx_paused = Some(review);
                if interrupted {
                    "The MLX transfer was interrupted. Reviewed files and bytes are available to resume.".into()
                } else {
                    "The MLX download is paused. Review the saved package before resuming.".into()
                }
            }
            Output::MlxPaused(review) => {
                self.hf.mlx_paused = review;
                if self.hf.mlx_paused.is_some() {
                    "Found the paused MLX package.".into()
                } else {
                    "There is no paused MLX download.".into()
                }
            }
            Output::MlxDiscarded(removed) => {
                self.hf.mlx_paused = None;
                if removed {
                    "Discarded the paused MLX download.".into()
                } else {
                    "There is no paused MLX download.".into()
                }
            }
            Output::Download(Downloaded::Installed(model, sync_failed)) => {
                let notice = format!("Registered {}.", model.name);
                self.managed.add_model(model, &self.profile_dir);
                self.hf.paused = None;
                self.hf.clear_review();
                if sync_failed {
                    format!("{notice} Could not sync the model registry directory.")
                } else {
                    notice
                }
            }
            Output::Download(Downloaded::Paused { plan, interrupted }) => {
                self.hf.paused = Some(plan);
                if interrupted {
                    "The download was interrupted. The verified prefix is available to resume."
                        .into()
                } else {
                    "The download is paused. The verified prefix is available to resume.".into()
                }
            }
            Output::Paused(plan) => {
                self.hf.paused = plan;
                if self.hf.paused.is_some() {
                    if self.model_downloads_allowed {
                        "Review the saved download before resuming.".into()
                    } else {
                        "A paused Hugging Face download can be discarded.".into()
                    }
                } else {
                    "There is no paused Hugging Face download.".into()
                }
            }
            Output::Discarded(removed) => {
                self.hf.paused = None;
                if removed {
                    "Discarded the paused Hugging Face download.".into()
                } else {
                    "There is no paused Hugging Face download.".into()
                }
            }
        }
    }
}

fn mlx_package_view(review: &MlxPackageReview) -> Element<'_, Message> {
    let files = iced::widget::Column::with_children(review.files.iter().map(|file| {
        text(format!(
            "{} · {} bytes\nSHA-256 {}",
            file.path(),
            file.size(),
            file.digest()
        ))
        .size(13)
        .wrapping(text::Wrapping::WordOrGlyph)
        .into()
    }))
    .spacing(6);
    column![
        text(format!("{} · {} bytes", review.repository, review.bytes)).size(14),
        text(format!("Reviewed commit · {}", review.revision)).size(13),
        scrollable(files).height(164),
    ]
    .spacing(6)
    .into()
}

fn plan_summary(plan: &Plan) -> Element<'static, Message> {
    let mut summary = column![
        text(format!(
            "{} files · {} bytes · {}",
            plan.count(),
            plan.total(),
            plan.purpose
        ))
        .size(13)
    ]
    .spacing(6);
    for file in plan.files() {
        summary = summary.push(
            column![
                text(plan.destination(file))
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph),
                text(format!("{} bytes · SHA-256 {}", file.size(), file.digest()))
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(2),
        );
    }
    summary.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_mlx_package_is_bound_to_the_selection_search_and_vault_revision() {
        let profile = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(profile.0.clone());
        let repository: Repository = serde_json::from_value("example/mlx".into()).unwrap();
        let mut model = ModelListing::for_test(repository.clone());
        model.format = Format::Mlx;
        dialog.hf.repositories.push(model);
        dialog.hf.repository = Some(repository.clone());
        let revision = dialog.hf.search_revision;
        let review = Arc::new(MlxPackageReview {
            repository,
            revision: "a".repeat(40),
            files: Vec::new(),
            bytes: 123,
        });
        let credential = Arc::new(());
        let revoked = Arc::downgrade(&credential);
        drop(credential);
        dialog.finish_huggingface(Output::MlxReview(
            revision,
            Some(revoked),
            Arc::clone(&review),
        ));
        assert!(dialog.hf.mlx_review.is_none());
        dialog.finish_huggingface(Output::MlxReview(
            revision.wrapping_add(1),
            None,
            Arc::clone(&review),
        ));
        assert!(dialog.hf.mlx_review.is_none());
        dialog.finish_huggingface(Output::MlxReview(revision, None, review));
        assert!(dialog.hf.mlx_review.is_some());
        assert!(dialog.hf.review.is_none());
        assert_eq!(dialog.hf.repositories[0].bytes, Some(123));
        dialog.hf.reset_search();
        assert!(dialog.hf.mlx_review.is_none());
    }

    #[test]
    fn mixed_hub_pages_keep_successful_results_and_retry_only_failed_continuations() {
        assert_eq!(FormatFilter::All.formats(), [Format::Gguf, Format::Mlx]);
        let profile = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(profile.0.clone());
        let gguf = Continuation::for_test("gguf-next");
        let mut mlx = Continuation::for_test("mlx-next");
        mlx.format = Format::Mlx;
        let requested = vec![gguf, mlx.clone()];
        dialog.hf.next_pages.clone_from(&requested);
        let mut model =
            ModelListing::for_test(serde_json::from_value("example/fixture".into()).unwrap());
        let status = dialog.finish_huggingface(Output::Search(
            dialog.hf.search_revision,
            requested,
            vec![
                (
                    Format::Gguf,
                    Ok(SearchPage {
                        repositories: vec![model.clone()],
                        next: None,
                    }),
                ),
                (Format::Mlx, Err("fixture failure".into())),
            ],
        ));
        assert!(status.contains("MLX: fixture failure"));
        assert_eq!(dialog.hf.next_pages, [mlx.clone()]);
        assert_eq!(dialog.hf.repositories.len(), 1);

        model.format = Format::Mlx;
        model.repository = serde_json::from_value("example/mlx".into()).unwrap();
        dialog.finish_huggingface(Output::Search(
            dialog.hf.search_revision,
            vec![mlx],
            vec![(
                Format::Mlx,
                Ok(SearchPage {
                    repositories: vec![model.clone()],
                    next: None,
                }),
            )],
        ));
        dialog.hf.repository = Some(model.repository);
        assert!(dialog.hf.selected_mlx());
        assert!(dialog.hf.install_plan().is_err());
        assert!(!dialog.busy());
        assert!(dialog.hf.next_pages.is_empty());
        assert_eq!(dialog.hf.repositories.len(), 2);
    }

    #[test]
    fn size_completion_requires_the_live_search_and_keeps_artifact_review_separate() {
        let profile = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(profile.0.clone());
        let repository: Repository = serde_json::from_value("example/fixture".into()).unwrap();
        let listing = ModelListing::for_test(repository.clone());
        let (cancel, cancelled) = runtime_package::Cancel::test_token();
        dialog.hf.repositories.push(listing.clone());
        dialog.hf.sizing.tried.insert(repository.clone());
        dialog.hf.sizing.pending.insert(repository.clone(), cancel);
        let revision = dialog.hf.search_revision;
        let _ = dialog.resolve_hub_size(revision, repository.clone(), Some(4 * GIB), None);
        assert_eq!(dialog.hf.repositories[0].bytes, Some(4 * GIB));
        assert!(dialog.hf.review.is_none());
        assert!(dialog.hf.file.is_none());
        assert!(!dialog.busy());
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));

        let (cancel, _) = runtime_package::Cancel::test_token();
        dialog.hf.sizing.pending.insert(repository.clone(), cancel);
        let _ = dialog.resolve_hub_size(revision, repository.clone(), Some(8 * GIB), None);
        assert_eq!(dialog.hf.repositories[0].bytes, Some(4 * GIB));

        let (cancel, cancelled) = runtime_package::Cancel::test_token();
        dialog.hf.sizing.pending.insert(repository.clone(), cancel);
        dialog.hf.suspend();
        dialog.hf.repositories.push(listing);
        let _ = dialog.resolve_hub_size(revision, repository, Some(8 * GIB), None);
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
        assert!(dialog.hf.repositories[0].bytes.is_none());
    }

    #[test]
    fn search_pages_preserve_selection_and_ignore_duplicates_cycles_and_stale_results() {
        let profile = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(profile.0.clone());
        let repository = |id: &str| serde_json::from_value::<Repository>(id.into()).unwrap();
        let first = repository("example/first");
        let second = repository("example/second");
        let cursor = Continuation::for_test("next");
        let page = SearchPage {
            repositories: vec![ModelListing::for_test(first.clone())],
            next: Some(cursor.clone()),
        };
        let revision = dialog.hf.search_revision;
        dialog.finish_huggingface(Output::Search(
            revision,
            Vec::new(),
            vec![(Format::Gguf, Ok(page.clone()))],
        ));
        dialog.hf.repository = Some(first.clone());
        dialog.finish_huggingface(Output::Search(
            revision,
            vec![cursor.clone()],
            vec![(
                Format::Gguf,
                Ok(SearchPage {
                    repositories: vec![
                        ModelListing::for_test(repository("Example/FIRST")),
                        ModelListing::for_test(second.clone()),
                    ],
                    next: Some(cursor.clone()),
                }),
            )],
        ));
        assert_eq!(
            dialog
                .hf
                .repositories
                .iter()
                .map(|model| &model.repository)
                .collect::<Vec<_>>(),
            [&first, &second]
        );
        let sorted = dialog.update_huggingface(Action::Sort(SortColumn::Size), true, None);
        assert!(iced_runtime::task::into_stream(sorted).is_none());
        assert_eq!(dialog.hf.repository, Some(first));
        assert!(dialog.hf.next_pages.is_empty());

        let _ = dialog.update_huggingface(Action::Query("other".into()), true, None);
        dialog.finish_huggingface(Output::Search(
            revision,
            Vec::new(),
            vec![(Format::Gguf, Ok(page))],
        ));
        assert!(dialog.hf.repositories.is_empty());
        assert!(dialog.hf.repository.is_none());
        assert!(dialog.hf.next_pages.is_empty());
        let stale_click = dialog.update_huggingface(Action::LoadMore(vec![cursor]), true, None);
        assert!(iced_runtime::task::into_stream(stale_click).is_none());
        assert!(!dialog.busy());
    }

    #[test]
    fn ai_model_download_policy_blocks_hub_actions_and_allows_local_discard_review() {
        let profile = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(profile.0.clone());
        dialog.hf.token = Some("must-not-resolve".into());
        dialog.set_model_downloads_allowed(false);
        for action in [
            Action::Search,
            Action::Review,
            Action::ReviewRecommendation,
            Action::Download,
            Action::Resume,
        ] {
            assert!(
                iced_runtime::task::into_stream(dialog.update_huggingface(action, true, None))
                    .is_none()
            );
            assert_eq!(dialog.status.as_deref(), Some(AI_MODEL_DOWNLOADS_DISABLED));
            assert!(!dialog.busy());
        }
        assert!(
            iced_runtime::task::into_stream(dialog.update_huggingface(
                Action::DiscardPaused,
                true,
                None
            ))
            .is_none()
        );
        assert!(dialog.hf.discard_confirmed);
        let _ = dialog.update_huggingface(Action::CancelDiscard, true, None);
        assert!(!dialog.hf.discard_confirmed);
    }

    #[test]
    fn choosing_a_recommendation_is_local_and_new_selections_invalidate_its_review() {
        let profile = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(profile.0.clone());
        dialog.hf.recommendations.memory = Some(24 * GIB);
        let text = dialog.hf.recommendations.selected().unwrap();
        assert_eq!(text.id, "qwen3-8b-q4");
        let coding = recommendations(None, RecommendationRole::Coding, Some(24 * GIB))[0].clone();
        let choose = |dialog: &mut Dialog, action| {
            assert!(
                iced_runtime::task::into_stream(dialog.update_huggingface(action, true, None))
                    .is_none()
            );
            assert!(!dialog.busy());
        };
        choose(&mut dialog, Action::Recommendation(coding.clone()));
        assert_eq!(dialog.hf.recommendations.selected(), Some(text));

        choose(
            &mut dialog,
            Action::RecommendationRole(RecommendationRole::Embedding),
        );
        let embedding = dialog.hf.recommendations.selected().unwrap();
        assert_eq!(embedding.id, "qwen3-embedding-0.6b-q8");
        dialog.hf.reviewed_role = Some(RecommendationRole::Embedding);
        choose(&mut dialog, Action::Purpose(Purpose::Chat));
        assert_eq!(dialog.hf.install_purpose(), Purpose::Embedding);
        assert!(dialog.hf.install_plan().is_err());

        let alternate = dialog.hf.recommendations.choices()[1].clone();
        choose(&mut dialog, Action::Recommendation(alternate.clone()));
        assert_eq!(dialog.hf.recommendations.selected(), Some(alternate));
        assert!(dialog.hf.reviewed_role.is_none());
        choose(
            &mut dialog,
            Action::RecommendationRole(RecommendationRole::Coding),
        );
        assert_eq!(dialog.hf.recommendations.selected(), Some(coding));
        assert!(dialog.hf.install_plan().is_err());
    }
}
