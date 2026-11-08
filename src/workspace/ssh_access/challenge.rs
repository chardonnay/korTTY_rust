use std::fmt;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::widget::{self, button, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task, window};
use kortty_ssh::{KeyboardChallenge, KeyboardResponse};

use crate::ssh_dialog::Password;
use crate::ssh_worker::DialCancellation;

use super::{Message, WorkspaceMessage, wrap};

const MAX_ANSWER: usize = 4096;
const MAX_TOTAL: usize = 16 * 1024;
type Answers = Vec<KeyboardResponse>;

#[derive(Clone)]
pub(super) struct Forwarder {
    pub(super) events: async_channel::Sender<Message>,
    pub(super) window: window::Id,
    pub(super) dial: u64,
    pub(super) cancellation: DialCancellation,
    pub(super) rounds: Arc<AtomicU64>,
    pub(super) secrets: Option<crate::terminal_logging::Secrets>,
}

impl Forwarder {
    pub(super) fn request(&self, challenge: &KeyboardChallenge) -> io::Result<Answers> {
        if self.cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let began = Instant::now();
        let (reply, responses) = mpsc::sync_channel(1);
        let ticket = Ticket {
            challenge: Arc::new(challenge.clone()),
            reply: Arc::new(Mutex::new(Some(reply))),
            round: self.rounds.fetch_add(1, Ordering::Relaxed),
        };
        self.events
            .try_send(Message::Challenge(self.window, self.dial, ticket.clone()))
            .map_err(|_| cancelled())?;
        let result = loop {
            if self.cancellation.is_cancelled() {
                break Err(cancelled());
            }
            if self.cancellation.worker_exited() {
                break Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "SSH worker exited during authentication",
                ));
            }
            let Some(remaining) = challenge.response_timeout.checked_sub(began.elapsed()) else {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "SSH prompt timed out",
                ));
            };
            match responses.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(answers) => break Ok(answers),
                Err(RecvTimeoutError::Disconnected) => break Err(cancelled()),
                Err(RecvTimeoutError::Timeout) => {}
            }
        };
        ticket.cancel();
        if let (Ok(answers), Some(secrets)) = (&result, &self.secrets) {
            for answer in answers {
                secrets.add(answer.as_str());
            }
        }
        result
    }
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "SSH prompt cancelled")
}

#[derive(Clone)]
pub struct Ticket {
    challenge: Arc<KeyboardChallenge>,
    reply: Arc<Mutex<Option<SyncSender<Answers>>>>,
    pub(super) round: u64,
}

impl fmt::Debug for Ticket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KeyboardInteractive")
            .field("round", &self.round)
            .finish_non_exhaustive()
    }
}

impl Ticket {
    fn take_reply(&self) -> Option<SyncSender<Answers>> {
        self.reply
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub(super) fn cancel(&self) {
        self.take_reply();
    }

    pub(super) fn active(&self) -> bool {
        self.reply
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }
}

struct Answer {
    field: widget::Id,
    value: Password,
    too_long: bool,
}

pub(super) struct Form {
    ticket: Ticket,
    answers: Vec<Answer>,
}

impl Form {
    pub(super) fn new(ticket: Ticket) -> Self {
        let answers = ticket
            .challenge
            .prompts
            .iter()
            .map(|_| Answer {
                field: widget::Id::unique(),
                value: Password::default(),
                too_long: false,
            })
            .collect();
        Self { ticket, answers }
    }

    pub(super) fn round(&self) -> u64 {
        self.ticket.round
    }

    pub(super) fn focus(&self) -> Task<WorkspaceMessage> {
        self.answers.first().map_or_else(Task::none, |answer| {
            widget::operation::focus(answer.field.clone())
        })
    }

    pub(super) fn answer(&mut self, index: usize, value: Password) {
        if let Some(answer) = self.answers.get_mut(index) {
            answer.too_long = value.as_str().len() > MAX_ANSWER;
            answer.value = if answer.too_long {
                Password::default()
            } else {
                value
            };
        }
    }

    fn valid(&self) -> bool {
        self.ticket.active()
            && self.answers.iter().all(|answer| !answer.too_long)
            && self
                .answers
                .iter()
                .map(|answer| answer.value.as_str().len())
                .sum::<usize>()
                <= MAX_TOTAL
    }

    pub(super) fn submit(&mut self) -> bool {
        if !self.valid() {
            return false;
        }
        if let Some(reply) = self.ticket.take_reply() {
            let responses = self
                .answers
                .iter_mut()
                .map(|answer| {
                    KeyboardResponse::new(std::mem::take(&mut answer.value).into_string())
                })
                .collect();
            let _ = reply.try_send(responses);
        }
        true
    }

    pub(super) fn view(
        &self,
        window: window::Id,
        dial: u64,
        cancel: WorkspaceMessage,
    ) -> Element<'_, WorkspaceMessage> {
        let challenge = &self.ticket.challenge;
        let round = self.round();
        let submit = self
            .valid()
            .then(|| wrap(Message::PromptSubmit(window, dial, round)));
        let mut fields = column![
            text("SSH keyboard-interactive").size(20),
            text(format!(
                "Verified host {} · user {}",
                challenge.endpoint, challenge.username
            )),
        ]
        .spacing(8);
        for description in [&challenge.name, &challenge.instruction] {
            if !description.is_empty() {
                fields = fields.push(text(description));
            }
        }
        for (index, (prompt, answer)) in challenge.prompts.iter().zip(&self.answers).enumerate() {
            let on_submit = (index + 1 == self.answers.len())
                .then(|| submit.clone())
                .flatten();
            fields = fields.push(text(&prompt.text)).push(
                text_input("Response", answer.value.as_str())
                    .id(answer.field.clone())
                    .secure(!prompt.echo)
                    .on_input(move |value| {
                        wrap(Message::PromptAnswer(
                            window,
                            dial,
                            round,
                            index,
                            Password::new(value),
                        ))
                    })
                    .on_submit_maybe(on_submit)
                    .width(Length::Fill),
            );
        }
        if self.answers.iter().any(|answer| answer.too_long) {
            fields = fields.push(text("One response exceeded the size limit."));
        }
        if self
            .answers
            .iter()
            .map(|answer| answer.value.as_str().len())
            .sum::<usize>()
            > MAX_TOTAL
        {
            fields = fields.push(text("The combined responses exceeded the size limit."));
        }
        scrollable(
            fields
                .push(
                    row![
                        button("Continue").on_press_maybe(submit),
                        button("Cancel").on_press(cancel),
                    ]
                    .spacing(8),
                )
                .padding(16)
                .max_width(650),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Drop for Form {
    fn drop(&mut self) {
        self.ticket.cancel();
    }
}
