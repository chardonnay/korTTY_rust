use std::sync::Arc;

use iced::widget::{button, column, row, text, text_editor};
use iced::{Element, Font, Length, Task, window};

use crate::ai_manager::Configuration;
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai;
use crate::workspace::skill_classifier;

use super::{Choice, Desktop, Message, quota};

pub(super) struct Review {
    request: skill_classifier::Request,
    config: Arc<Configuration>,
    assigned: Vec<u64>,
    pub(super) preview: text_editor::Content,
    pub(super) quota: quota::State,
    showing: bool,
    pending: Option<(u64, local_ai::Cancel)>,
}

impl Review {
    pub(super) fn new(
        request: skill_classifier::Request,
        config: &Arc<Configuration>,
        assigned: &[u64],
    ) -> Option<Self> {
        let preview = request.preview().ok()?;
        Some(Self {
            request,
            config: Arc::clone(config),
            assigned: assigned.to_vec(),
            preview,
            quota: quota::State::default(),
            showing: false,
            pending: None,
        })
    }

    pub(super) fn matches(&self, config: Option<&Arc<Configuration>>, assigned: &[u64]) -> bool {
        config.is_some_and(|config| Arc::ptr_eq(config, &self.config)) && assigned == self.assigned
    }

    pub(super) fn showing(&self) -> bool {
        self.showing
    }

    pub(super) fn reveal(
        &mut self,
        id: window::Id,
        token: u64,
        profile: &local_ai::Profile,
    ) -> Task<Desktop> {
        self.showing = true;
        self.refresh_quota(id, token, profile)
    }

    fn refresh_quota(
        &mut self,
        id: window::Id,
        token: u64,
        profile: &local_ai::Profile,
    ) -> Task<Desktop> {
        self.quota.start_messages(
            token,
            &skill_classifier::transport(profile),
            self.request.messages(),
            move |forecast| Desktop::TerminalAi(id, Message::SkillQuotaFinished(token, forecast)),
        )
    }

    pub(super) fn start(
        &mut self,
        id: window::Id,
        token: u64,
        profile: &local_ai::Profile,
    ) -> Result<Task<Desktop>, String> {
        if !self.showing || self.pending.is_some() || self.quota.loading() {
            return Ok(Task::none());
        }
        if self.quota.stale() {
            return Ok(self.refresh_quota(id, token, profile));
        }
        let config = Arc::clone(&self.config);
        let worker = self
            .request
            .start(profile, move || require_current(&config))?;
        self.pending = Some((token, worker.cancel));
        Ok(Task::perform(
            async move {
                worker.reply.recv().await.unwrap_or_else(|_| {
                    Err("The skill classification worker ended without a response.".into())
                })
            },
            move |result| {
                Desktop::TerminalAi(id, Message::SkillClassificationFinished(token, result))
            },
        ))
    }

    pub(super) fn completed(&self, token: u64) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(current, _)| *current == token)
    }

    pub(super) fn cancel(&mut self) -> bool {
        self.showing = false;
        self.quota = quota::State::default();
        self.pending.take().is_some()
    }

    pub(super) fn view(
        &self,
        choice: &Choice,
        font_size: f32,
        allowed: bool,
    ) -> Element<'_, Message> {
        column![
            text("Review skill classification").size(19),
            text(format!(
                "{} · {} · {}",
                choice.name, choice.model, choice.destination
            ))
            .size(14),
            text(
                "The exact request below contains the masked context and eligible skill metadata."
            )
            .size(13),
            self.quota.label().map(|forecast| text(forecast).size(13)),
            text_editor(&self.preview)
                .font(Font::MONOSPACE)
                .size(font_size)
                .on_action(Message::ReadSkillPreview)
                .height(Length::Fill),
            row![
                button(if self.pending.is_some() {
                    "Cancel classification"
                } else {
                    "Back to chat review"
                })
                .on_press(Message::CancelSkillClassification),
                self.pending.is_some().then(|| text("Classifying skills…")),
                self.pending.is_none().then(|| {
                    button("Classify skills").on_press_maybe(
                        (allowed && !self.quota.loading()).then_some(Message::ClassifySkills),
                    )
                }),
            ]
            .spacing(8),
        ]
        .padding(14)
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

pub(super) fn require_current(config: &Configuration) -> Result<(), String> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some() || !policy.ai_allowed || !policy.ai_chat_allowed {
        return Err("Managed policy blocks AI chat requests.".into());
    }
    if config.saved_root().is_some()
        && !config
            .reload_saved()
            .is_ok_and(|current| current == *config)
    {
        return Err("The saved AI settings changed. Review the request again.".into());
    }
    Ok(())
}
