//! Explicit Hub search, file review and managed-model download controls.

use std::fmt;
use std::sync::Arc;

use iced::Element;
use iced::widget::{button, column, pick_list, row, text, text_input};

use crate::clipboard_policy::AI_MODEL_DOWNLOADS_DISABLED;
use crate::connection_window::CredentialChoice;
use crate::local_ai::managed_models::Purpose;
use crate::local_ai::managed_models::huggingface::{
    self as hub, Downloaded, GIB, Pause, Plan, Progress, Recommendation, RecommendationRole,
    Repository, Review, Selection, recommendations,
};
use crate::local_ai::runtime_package;
use crate::vault_window::VaultWindow;
use iced::Task;

use super::{Dialog, Message};
use crate::ai_manager::profiles::picker::DismissOnEscape as _;

#[derive(Clone)]
pub(crate) enum Action {
    Query(String),
    Token(Token),
    Search,
    Repository(Repository),
    Review,
    RecommendationRole(RecommendationRole),
    Recommendation(Arc<Recommendation>),
    ReviewRecommendation,
    Artifact(Selection),
    Purpose(Purpose),
    Download,
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
                | Self::Review
                | Self::ReviewRecommendation
                | Self::Download
                | Self::Resume
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Output {
    Search(Vec<Repository>),
    Review(Arc<Review>),
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
    pub(super) repositories: Vec<Repository>,
    pub(super) repository: Option<Repository>,
    pub(super) review: Option<Arc<Review>>,
    pub(super) file: Option<Selection>,
    pub(super) purpose: Purpose,
    pub(super) paused: Option<Plan>,
    pub(super) pause: Option<Pause>,
    pub(super) progress: Option<Progress>,
    pub(super) discard_confirmed: bool,
    recommendations: Recommendations,
    reviewed_role: Option<RecommendationRole>,
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

    fn view(&self, enabled: bool, access_selected: bool) -> Element<'_, Message> {
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
            row![role, model].spacing(8).wrap(),
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
            button("Review recommended model").on_press_maybe(
                (enabled && access_selected && selected.is_some())
                    .then_some(Message::HuggingFace(Action::ReviewRecommendation))
            ),
        ]
        .spacing(6)
        .into()
    }
}

impl State {
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
    ) -> Element<'_, Message> {
        if !model_downloads_allowed {
            return column![
                text("Hugging Face GGUF Models").size(17),
                text(AI_MODEL_DOWNLOADS_DISABLED).size(13),
                self.paused_actions(enabled),
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
        let repository_picker: Option<Element<'_, Message>> =
            if enabled && !self.repositories.is_empty() {
                Some(
                    pick_list(
                        self.repositories.as_slice(),
                        self.repository.as_ref(),
                        |repository| Message::HuggingFace(Action::Repository(repository)),
                    )
                    .placeholder("Choose a GGUF repository")
                    .dismiss_on_escape(),
                )
            } else {
                self.repository
                    .as_ref()
                    .map(|repository| text(repository.to_string()).into())
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
                file,
                row![text("Model purpose"), purpose].spacing(8),
                self.install_plan().ok().map(|plan| plan_summary(&plan)),
                button("Download and register reviewed model").on_press_maybe(
                    (enabled && self.file.is_some() && self.paused.is_none() && selected.is_some())
                        .then_some(Message::HuggingFace(Action::Download))
                ),
            ]
            .spacing(8)
        });
        column![
            text("Hugging Face GGUF Models").size(17),
            text("Search the Hub over HTTPS and review an immutable commit, file size, and SHA-256 before downloading. A selected vault token goes to huggingface.co.").size(13),
            row![text("Hub access"), authentication].spacing(8),
            self.recommendations.view(enabled, selected.is_some()),
            row![
                text_input("Search GGUF models", &self.query)
                    .on_input_maybe(enabled.then_some(|query| Message::HuggingFace(Action::Query(query))))
                    .on_submit_maybe((enabled && selected.is_some()).then_some(Message::HuggingFace(Action::Search))),
                button("Search Hugging Face").on_press_maybe((enabled && selected.is_some()).then_some(Message::HuggingFace(Action::Search))),
            ].spacing(8).wrap(),
            row![
                repository_picker,
                button("Review repository").on_press_maybe((enabled && self.repository.is_some() && selected.is_some()).then_some(Message::HuggingFace(Action::Review))),
            ].spacing(8).wrap(),
            review,
            self.paused_actions(enabled),
            self.paused.as_ref().map(|plan| column![
                text("Paused download").size(15),
                plan_summary(plan),
                button("Resume reviewed download").on_press_maybe((enabled && selected.is_some()).then_some(Message::HuggingFace(Action::Resume))),
            ].spacing(6)),
            self.progress.map(|progress| row![
                text(format!("{} / {} MiB", progress.received / (1024 * 1024), progress.total.div_ceil(1024 * 1024))).size(13),
                button("Pause download").on_press_maybe((progress.resumable && self.pause.is_some()).then_some(Message::HuggingFace(Action::Pause))),
            ].spacing(8)),
        ].spacing(8).into()
    }

    fn paused_actions(&self, enabled: bool) -> Element<'_, Message> {
        row![
            button("Find paused download")
                .style(button::secondary)
                .on_press_maybe(enabled.then_some(Message::HuggingFace(Action::ReadPaused))),
            button(if self.discard_confirmed {
                "Confirm discard"
            } else {
                "Discard paused download…"
            })
            .style(button::secondary)
            .on_press_maybe(enabled.then_some(Message::HuggingFace(Action::DiscardPaused))),
            (enabled && self.discard_confirmed).then(|| button("Keep download")
                .style(button::secondary)
                .on_press(Message::HuggingFace(Action::CancelDiscard))),
        ]
        .spacing(8)
        .wrap()
        .into()
    }
}

impl Dialog {
    pub(in crate::ai_manager) fn set_model_downloads_allowed(&mut self, allowed: bool) {
        if self.model_downloads_allowed && !allowed {
            if let Some(pause) = self.hf.pause.take() {
                pause.request();
            }
            self.hf.repositories.clear();
            self.hf.repository = None;
            self.hf.clear_review();
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
                Action::ReadPaused | Action::DiscardPaused | Action::CancelDiscard
            )
        {
            self.status = Some(AI_MODEL_DOWNLOADS_DISABLED.into());
            return Task::none();
        }
        if !matches!(action, Action::DiscardPaused) {
            self.hf.discard_confirmed = false;
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
            Action::Query(query) => self.hf.query = super::limited(&query, 256).into(),
            Action::Token(token) => {
                self.hf.token = match token {
                    Token::Anonymous => None,
                    Token::Saved(choice) => Some(choice.id),
                };
                self.hf.repositories.clear();
                self.hf.repository = None;
                self.hf.clear_review();
            }
            Action::Search => {
                self.hf.repositories.clear();
                self.hf.repository = None;
                self.hf.clear_review();
                return self.run_huggingface(
                    hub::search(self.hf.query.clone(), token),
                    "Searching Hugging Face…",
                    Output::Search,
                );
            }
            Action::Repository(repository) if self.hf.repositories.contains(&repository) => {
                self.hf.repository = Some(repository);
                self.hf.clear_review();
            }
            Action::Review => {
                if let Some(repository) = self.hf.repository.clone() {
                    self.hf.clear_review();
                    return self.run_huggingface(
                        hub::review(repository, token),
                        "Reading immutable Hugging Face file metadata…",
                        Output::Review,
                    );
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
            Output::Search(_) | Output::Review(_) | Output::Recommendation(..)
                if !self.model_downloads_allowed =>
            {
                AI_MODEL_DOWNLOADS_DISABLED.into()
            }
            Output::Search(repositories) => {
                let count = repositories.len();
                self.hf.repositories = repositories;
                format!("Found {count} Hugging Face GGUF repositories.")
            }
            Output::Review(review) => {
                let count = review.files.len();
                self.hf.review = Some(review);
                format!("Reviewed {count} complete GGUF choices with published SHA-256 values.")
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
