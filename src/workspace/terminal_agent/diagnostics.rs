//! Optional command diagnostics stay separate from model and journal evidence.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::terminal_agent::{Cancel, OutputStream};

use iced::widget::{column, scrollable, text};
use iced::{Element, Length};

use super::{Message, Mode, Redactor, mask};

const MAX_ENTRIES: usize = 32;
const MAX_CHARS: usize = 4000;

#[derive(Default)]
pub(super) struct State {
    pub(super) debug_enabled: bool,
    pub(super) runtime_enabled: bool,
    entries: VecDeque<String>,
    live: Option<Live>,
}

struct Live {
    epoch: u64,
    text: [String; 2],
}

#[derive(Clone)]
pub struct Update {
    stream: OutputStream,
    text: String,
}

impl State {
    pub(super) fn configured(config: &crate::ai_manager::Configuration, mode: Mode) -> Self {
        Self {
            debug_enabled: config.agent_debug_messages(),
            runtime_enabled: mode != Mode::Ask && config.agent_runtime_messages(),
            ..Self::default()
        }
    }

    pub(super) fn start_output(
        &mut self,
        epoch: u64,
        cancel: Cancel,
        redactor: Redactor,
    ) -> (Cancel, Option<async_channel::Receiver<Arc<Update>>>) {
        if !self.debug_enabled {
            return (cancel, None);
        }
        self.live = Some(Live {
            epoch,
            text: Default::default(),
        });
        let (send, receive) = async_channel::bounded(8);
        let versions = Mutex::new([None, None]);
        let observed = cancel.with_output(move |stream, captured, finished| {
            let version = (captured.progress_revision(), finished);
            let Ok(mut previous) = versions.lock() else {
                return;
            };
            if previous[stream.index()] == Some(version) {
                return;
            }
            previous[stream.index()] = Some(version);
            drop(previous);
            let raw = captured.text();
            let masked = if finished {
                mask::captured_output(&redactor, &raw)
            } else {
                mask::streaming_captured(&redactor, &raw)
            };
            let text = match masked {
                Ok(text) if text.is_empty() => return,
                Ok(text) => {
                    let start = text
                        .char_indices()
                        .rev()
                        .nth(MAX_CHARS - 1)
                        .map_or(0, |(start, _)| start);
                    if start == 0 {
                        text
                    } else {
                        format!("Earlier debug output omitted.\n{}", &text[start..])
                    }
                }
                Err(_) => "Debug output withheld because it could not be safely masked.".into(),
            };
            // Full snapshots let a slow view skip frames without blocking a command.
            let _ = send.try_send(Arc::new(Update { stream, text }));
        });
        (observed, Some(receive))
    }

    pub(super) fn update_output(&mut self, epoch: u64, update: &Update) {
        if self.debug_enabled
            && let Some(live) = &mut self.live
            && live.epoch == epoch
        {
            let label = match update.stream {
                OutputStream::Stdout => "stdout",
                OutputStream::Stderr => "stderr",
            };
            live.text[update.stream.index()] = format!("{label}\n{}", update.text);
        }
    }

    pub(super) fn clear_output(&mut self) {
        self.live = None;
    }

    pub(super) fn freeze_output(&mut self) {
        if let Some(live) = self.live.take() {
            for text in live.text {
                self.record(&text);
            }
        }
    }

    // Callers supply the same masked preview/output used by canonical activity.
    pub(super) fn record(&mut self, masked: &str) {
        if !self.debug_enabled || masked.trim().is_empty() {
            return;
        }
        if self.entries.len() == MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.entries
            .push_back(masked.chars().take(MAX_CHARS).collect());
    }

    pub(super) fn snapshot(&self, redactor: &Redactor) -> Result<Self, String> {
        Ok(Self {
            debug_enabled: self.debug_enabled,
            runtime_enabled: self.runtime_enabled,
            live: None,
            entries: self
                .entries
                .iter()
                .map(|entry| mask::prompt(redactor, entry))
                .collect::<Result<_, _>>()?,
        })
    }

    pub(super) fn visible(&self) -> bool {
        self.debug_enabled
            && (!self.entries.is_empty()
                || self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.text.iter().any(|text| !text.is_empty())))
    }

    pub(super) fn view(&self, font: f32, height: f32) -> Option<Element<'_, Message>> {
        if !self.visible() || height < 40.0 {
            return None;
        }
        let messages = self
            .entries
            .iter()
            .chain(
                self.live
                    .iter()
                    .flat_map(|live| live.text.iter())
                    .filter(|text| !text.is_empty()),
            )
            .fold(column![].spacing(6), |column, entry| {
                column.push(text(entry.trim_end()).size(font).width(Length::Fill))
            });
        Some(
            column![
                text("Debug messages").size(font),
                scrollable(messages).anchor_bottom().height(Length::Fill),
            ]
            .spacing(4)
            .height(Length::Fixed(height))
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    mod openssh;

    #[test]
    fn diagnostic_epoch_freeze_and_snapshot_keep_late_or_newly_secret_text_private() {
        let mut state = State {
            debug_enabled: true,
            ..State::default()
        };
        state.live = Some(Live {
            epoch: 7,
            text: Default::default(),
        });
        let update = Update {
            stream: OutputStream::Stdout,
            text: "newly-learned-fixture".into(),
        };
        state.update_output(8, &update);
        assert!(
            state
                .live
                .as_ref()
                .unwrap()
                .text
                .iter()
                .all(String::is_empty)
        );
        state.update_output(7, &update);
        assert!(state.live.as_ref().unwrap().text[0].contains("newly-learned-fixture"));
        state.freeze_output();
        assert!(state.live.is_none());
        let count = state.entries.len();
        state.update_output(7, &update);
        assert_eq!(state.entries.len(), count);
        let secrets = crate::terminal_logging::Secrets::default();
        secrets.add("newly-learned-fixture");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Default::default());
        let snapshot = state.snapshot(&redactor).unwrap();
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| !entry.contains("newly-learned-fixture"))
        );
        assert!(snapshot.entries.iter().any(|entry| entry.contains("***")));
    }

    #[test]
    fn diagnostic_tail_is_bounded_and_off_collects_nothing() {
        let mut state = State::default();
        state.record("Off message");
        assert!(state.entries.is_empty());
        state.debug_enabled = true;
        for index in 0..40 {
            state.record(&format!("{index} {}", "界".repeat(5000)));
        }
        assert_eq!(state.entries.len(), MAX_ENTRIES);
        assert!(state.entries[0].starts_with("8 "));
        assert!(
            state
                .entries
                .iter()
                .all(|entry| entry.chars().count() == MAX_CHARS)
        );
        state.debug_enabled = false;
        state.record("Must not replace retained messages");
        assert_eq!(state.entries.len(), MAX_ENTRIES);
        assert!(state.view(13.0, 100.0).is_none());
    }
}
