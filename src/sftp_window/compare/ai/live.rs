//! Bounded display snapshots of an answer that is still arriving.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_channel::Sender;
use zeroize::Zeroizing;

use crate::local_ai::{StreamDelta, reply};
use crate::vault_window::JournalQuestionSecrets;

use super::{Authority, MAX_RESULT};

const PREVIEW_BYTES: usize = 4 * 1024;
const UPDATE_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Default)]
pub struct Preview {
    pub(super) answer: String,
    pub(super) reasoning: String,
}

pub(super) struct Reporter<'a> {
    authority: &'a Authority,
    vault: &'a JournalQuestionSecrets,
    updates: Sender<Arc<Preview>>,
    enabled: bool,
    last_update: Option<Instant>,
    answer: Zeroizing<String>,
    reasoning: Zeroizing<String>,
}

impl<'a> Reporter<'a> {
    pub(super) fn new(
        authority: &'a Authority,
        vault: &'a JournalQuestionSecrets,
        updates: Sender<Arc<Preview>>,
    ) -> Self {
        Self {
            authority,
            vault,
            updates,
            // A rule or credential may match only after a subsequent line arrives.
            enabled: !authority.policy.has_redactions()
                && !vault.has_multiline_values()
                && authority
                    .terminal_secrets
                    .as_ref()
                    .is_none_or(|secrets| !secrets.has_multiline_values())
                && authority
                    .credential
                    .as_ref()
                    .is_none_or(|credential| !credential.contains(['\r', '\n'])),
            last_update: None,
            answer: Zeroizing::default(),
            reasoning: Zeroizing::default(),
        }
    }

    pub(super) fn push(&mut self, delta: StreamDelta) {
        let (output, text) = match delta {
            StreamDelta::Text(text) => (&mut self.answer, Zeroizing::new(text)),
            StreamDelta::Reasoning(text) => (&mut self.reasoning, Zeroizing::new(text)),
        };
        if !self.enabled {
            return;
        }
        if text.len() > MAX_RESULT - output.len() {
            self.enabled = false;
            return;
        }
        output.push_str(&text);

        let now = Instant::now();
        if self.updates.is_full()
            || self
                .last_update
                .is_some_and(|last| now.duration_since(last) < UPDATE_INTERVAL)
        {
            return;
        }
        let display = |input: &str| self.authority.mask(self.vault, input);
        let answer = complete_lines(&self.answer)
            .and_then(|answer| display(answer).ok())
            .map(|masked| tail(reply::sanitize_reply(&masked).to_string()));
        let reasoning =
            complete_lines(&self.reasoning).and_then(|input| display(input).ok().map(tail));
        if answer.as_deref().is_none_or(str::is_empty)
            && reasoning.as_deref().is_none_or(str::is_empty)
        {
            return;
        }
        let preview = Arc::new(Preview {
            answer: answer.unwrap_or_default(),
            reasoning: reasoning.unwrap_or_default(),
        });
        if self.updates.try_send(preview).is_ok() {
            self.last_update = Some(now);
        }
    }
}

fn complete_lines(input: &str) -> Option<&str> {
    // A credential's unfinished prefix is never displayed.
    input.rfind('\n').map(|end| &input[..=end])
}

fn tail(mut text: String) -> String {
    if text.len() > PREVIEW_BYTES {
        let from = text.ceil_char_boundary(text.len() - PREVIEW_BYTES);
        text.drain(..from);
        text.insert_str(0, "…\n");
    }
    text
}
