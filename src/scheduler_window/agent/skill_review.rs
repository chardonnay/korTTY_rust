//! Interactive skill classification and model request quota previews.

use std::sync::Arc;

use iced::Task;
use zeroize::Zeroizing;

use crate::local_ai::Cancel;
use crate::workspace::{ai_quota, skill_classifier};

use super::{Message, ProposalSkillSetup, proposal::Prompt};

pub(in crate::scheduler_window) struct SkillReview {
    pub(in crate::scheduler_window) preview: String,
    pub(in crate::scheduler_window) quota: ai_quota::State,
    pub(in crate::scheduler_window) classification: Classification,
    pub(super) setup: Option<ProposalSkillSetup>,
    quota_token: u64,
}

pub(in crate::scheduler_window) enum Classification {
    Closed,
    Preview {
        text: Zeroizing<String>,
        quota: ai_quota::State,
    },
    Pending {
        ticket: String,
        _cancel: Cancel,
    },
}

impl SkillReview {
    pub(super) fn new() -> Result<Self, String> {
        Ok(Self {
            preview: preview_nonce()?,
            quota: ai_quota::State::default(),
            classification: Classification::Closed,
            setup: None,
            quota_token: 0,
        })
    }

    pub(in crate::scheduler_window) fn has_classifier(&self) -> bool {
        self.classifier().is_some()
    }

    pub(in crate::scheduler_window) fn ready(&self) -> bool {
        matches!(self.classification, Classification::Closed) && !self.quota.loading()
    }

    fn classifier(&self) -> Option<&skill_classifier::Request> {
        self.setup.as_ref()?.classifier.as_ref()
    }

    pub(super) fn renew_preview(&mut self) -> Result<(), String> {
        self.preview = preview_nonce()?;
        Ok(())
    }

    fn next_quota_token(&mut self) -> u64 {
        self.quota_token = self.quota_token.wrapping_add(1);
        self.quota_token
    }

    pub(super) fn proposal_quota(
        &mut self,
        prompts: &[Arc<Prompt>],
        standing: bool,
    ) -> Result<Task<Message>, String> {
        let prompt = prompts.first().ok_or("The model request is unavailable.")?;
        let token = self.next_quota_token();
        let preview = self.preview.clone();
        let finish = move |forecast| Message::ProposalQuotaFinished(preview, token, forecast);
        let transport = prompt.quota_transport();
        if !standing {
            return Ok(self
                .quota
                .start_messages(token, &transport, prompt.messages(), finish));
        }
        if !transport.has_quota() {
            self.quota = ai_quota::State::Omitted;
            return Ok(Task::none());
        }
        let mut requests = Vec::with_capacity(prompts.len());
        let mut output = 0u64;
        for prompt in prompts {
            let target = prompt.quota_transport();
            output += u64::from(target.effective_output_limit());
            requests.push((
                target,
                [
                    Zeroizing::new(prompt.firing_system().to_owned()),
                    Zeroizing::new(prompt.firing_input_preview()?),
                ],
            ));
        }
        Ok(self.quota.start_forecast_with(
            token,
            &transport,
            move |transport| {
                let input = requests.iter().fold(0u64, |sum, (target, parts)| {
                    sum.saturating_add(target.count_input(parts.iter().map(|part| part.as_str())))
                });
                transport.quota_projection(input, output)
            },
            finish,
        ))
    }

    pub(super) fn open(&mut self, prompt: &Prompt) -> Result<Task<Message>, String> {
        let Some(classifier) = self.classifier() else {
            return Ok(Task::none());
        };
        let text = classifier.preview_text()?;
        self.classification = Classification::Preview {
            text,
            quota: ai_quota::State::default(),
        };
        Ok(self.classifier_quota(prompt))
    }

    fn classifier_quota(&mut self, prompt: &Prompt) -> Task<Message> {
        let token = self.next_quota_token();
        let preview = self.preview.clone();
        let Some(classifier) = self
            .setup
            .as_ref()
            .and_then(|setup| setup.classifier.as_ref())
        else {
            return Task::none();
        };
        let Classification::Preview { quota, .. } = &mut self.classification else {
            return Task::none();
        };
        quota.start_messages(
            token,
            &skill_classifier::transport(&prompt.transport),
            classifier.messages(),
            move |forecast| Message::ProposalSkillQuotaFinished(preview, token, forecast),
        )
    }

    pub(super) fn request(
        &mut self,
        prompt: &Prompt,
        before_dispatch: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<Task<Message>, String> {
        let Classification::Preview { quota, .. } = &self.classification else {
            return Ok(Task::none());
        };
        if quota.loading() {
            return Ok(Task::none());
        }
        if quota.stale() {
            self.renew_preview()?;
            return Ok(self.classifier_quota(prompt));
        }
        let Some(classifier) = self.classifier() else {
            return Ok(Task::none());
        };
        let ticket = preview_nonce()?;
        let pending = classifier.start(&prompt.transport, before_dispatch)?;
        self.preview = ticket.clone();
        self.classification = Classification::Pending {
            ticket: ticket.clone(),
            _cancel: pending.cancel,
        };
        Ok(Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Skill classification stopped.".into()))
            },
            move |result| Message::ProposalSkillsFinished(ticket, result),
        ))
    }

    pub(super) fn close(&mut self, consume: bool) -> Result<(), String> {
        self.renew_preview()?;
        self.classification = Classification::Closed;
        if consume && let Some(setup) = &mut self.setup {
            setup.classifier = None;
        }
        Ok(())
    }
}

fn preview_nonce() -> Result<String, String> {
    crate::private_files::nonce().map_err(|_| "Could not identify the model request review.".into())
}
