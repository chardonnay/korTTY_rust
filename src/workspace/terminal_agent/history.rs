//! A modeless picker for masked, explicitly started agent prompts.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task, window};

use crate::terminal_agent::history::{self as store, Entry, Kind, Snapshot};

use super::{Desktop, Message as RunMessage, Mode, Phase, Run, Workspace};

#[derive(Clone)]
pub enum Message {
    Open,
    Dismiss,
    Use(u64, usize),
    Delete(u64, usize),
    Clear(bool),
    ConfirmClear(u64),
    Limit(String),
    ApplyLimit(u64),
    Finished(u64, Arc<Result<Snapshot, String>>),
    Recorded(u64, Result<(), String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Load,
    Write,
    Record,
}

#[derive(Default)]
pub(super) struct State {
    epoch: u64,
    visible: bool,
    pending: Option<Pending>,
    snapshot: Snapshot,
    limit: String,
    clearing: bool,
    pub(super) notice: String,
}

impl State {
    pub(super) fn visible(&self) -> bool {
        self.visible
    }

    pub(super) fn writing(&self) -> bool {
        matches!(self.pending, Some(Pending::Write | Pending::Record))
    }

    fn begin(&mut self, pending: Pending) -> u64 {
        self.epoch += 1;
        self.pending = Some(pending);
        self.notice.clear();
        self.epoch
    }

    fn dismiss(&mut self) {
        self.epoch += 1;
        self.visible = false;
        self.pending = None;
        self.clearing = false;
        self.snapshot = Snapshot::default();
        self.notice.clear();
    }

    fn limit(&self) -> Option<usize> {
        self.limit
            .parse()
            .ok()
            .filter(|value| (store::MIN_LIMIT..=store::MAX_LIMIT).contains(value))
    }

    pub(super) fn view(&self) -> Element<'_, RunMessage> {
        let action = RunMessage::History;
        if !self.visible {
            return button("Recent prompts")
                .on_press(action(Message::Open))
                .into();
        }
        let enabled = self.pending.is_none() && !self.clearing;
        let mut list = column![].spacing(5);
        for (index, entry) in self.snapshot.entries.iter().enumerate() {
            list = list.push(
                row![
                    button(text(label(entry)))
                        .on_press_maybe(enabled.then_some(action(Message::Use(self.epoch, index)))),
                    button("Delete").on_press_maybe(
                        enabled.then_some(action(Message::Delete(self.epoch, index)))
                    ),
                ]
                .spacing(6)
                .wrap(),
            );
        }
        let mut view = column![
            text("Recent agent prompts").size(17),
            scrollable(list).height(Length::Fixed(150.0)),
            row![
                text_input("Prompt limit (5–100)", &self.limit)
                    .on_input_maybe(enabled.then_some(move |value| action(Message::Limit(value)))),
                button("Save limit").on_press_maybe(
                    (enabled
                        && self
                            .limit()
                            .is_some_and(|limit| limit != self.snapshot.limit))
                    .then_some(action(Message::ApplyLimit(self.epoch)))
                ),
                button("Clear all").on_press_maybe(
                    (enabled && !self.snapshot.entries.is_empty())
                        .then_some(action(Message::Clear(true)))
                ),
                button("Close history")
                    .on_press_maybe((!self.writing()).then_some(action(Message::Dismiss))),
            ]
            .spacing(6)
            .wrap(),
        ]
        .spacing(6);
        if self.clearing {
            view = view.push(
                row![
                    text(format!(
                        "Delete all {} shown prompts?",
                        self.snapshot.entries.len()
                    )),
                    button("Delete all shown prompts")
                        .on_press(action(Message::ConfirmClear(self.epoch))),
                    button("Keep prompts").on_press(action(Message::Clear(false))),
                ]
                .spacing(6)
                .wrap(),
            );
        }
        view.into()
    }
}

impl Workspace {
    pub(super) fn agent_history_action(
        &self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Task<Desktop> {
        if run.binding.private() || run.swarm {
            return Task::none();
        }
        let history = &mut run.history;
        match message {
            Message::Recorded(epoch, result)
                if history.pending == Some(Pending::Record) && epoch == history.epoch =>
            {
                history.pending = None;
                if let Err(error) = result {
                    history.notice = error;
                }
                return Task::none();
            }
            Message::Finished(epoch, result)
                if matches!(history.pending, Some(Pending::Load | Pending::Write))
                    && epoch == history.epoch =>
            {
                history.pending = None;
                history.clearing = false;
                match result.as_ref() {
                    Ok(snapshot) => {
                        history.snapshot = snapshot.clone();
                        history.limit = snapshot.limit.to_string();
                        history.notice.clear();
                    }
                    Err(error) => history.notice.clone_from(error),
                }
                return Task::none();
            }
            _ => {}
        }
        if run.phase != Phase::Setup || history.writing() || run.closing {
            return Task::none();
        }
        let ready = history.visible && history.pending.is_none();
        match message {
            Message::Open if !history.visible => {
                let Some(root) = self.agent_history_root() else {
                    return Task::none();
                };
                history.visible = true;
                let epoch = history.begin(Pending::Load);
                history.notice = "Loading recent prompts…".into();
                return load(id, epoch, root, |_| Ok(()));
            }
            Message::Dismiss => history.dismiss(),
            Message::Use(epoch, index) if ready && !history.clearing && epoch == history.epoch => {
                if let Some(entry) = history.snapshot.entries.get(index) {
                    run.goal.clone_from(&entry.goal);
                    history.dismiss();
                }
            }
            Message::Delete(epoch, index)
                if ready && !history.clearing && epoch == history.epoch =>
            {
                if let Some(entry) = history.snapshot.entries.get(index).cloned()
                    && let Some(root) = self.agent_history_root()
                {
                    let epoch = history.begin(Pending::Write);
                    return load(id, epoch, root, move |path| {
                        store::delete(path, &entry, false)
                    });
                }
            }
            Message::Clear(value) if ready => history.clearing = value,
            Message::ConfirmClear(epoch) if ready && history.clearing && epoch == history.epoch => {
                if let Some(root) = self.agent_history_root() {
                    let before = history.snapshot.clone();
                    let epoch = history.begin(Pending::Write);
                    return load(id, epoch, root, move |path| {
                        store::clear(path, &before, false)
                    });
                }
            }
            Message::Limit(value) if ready && !history.clearing && value.len() <= 3 => {
                history.limit = value
            }
            Message::ApplyLimit(epoch) if ready && !history.clearing && epoch == history.epoch => {
                if let (Some(root), Some(limit)) = (self.agent_history_root(), history.limit()) {
                    let before = history.snapshot.clone();
                    let epoch = history.begin(Pending::Write);
                    return load(id, epoch, root, move |path| {
                        store::set_limit(path, &before, limit, false)
                    });
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn remember_agent_prompt(
        &self,
        id: window::Id,
        run: &mut Run,
        mode: Mode,
    ) -> Task<Desktop> {
        if run.binding.private() || run.swarm || run.history.writing() {
            return Task::none();
        }
        let Some(root) = self.agent_history_root() else {
            return Task::none();
        };
        let goal = run.goal.clone();
        run.history.dismiss();
        let epoch = run.history.begin(Pending::Record);
        file_task(
            move || store::remember(&root, goal, mode, false),
            move |result| {
                Desktop::TerminalAgent(id, RunMessage::History(Message::Recorded(epoch, result)))
            },
        )
    }

    pub(super) fn agent_history_root(&self) -> Option<PathBuf> {
        self.ai_configuration()?
            .saved_root()
            .map(std::path::Path::to_owned)
    }
}

fn load(
    id: window::Id,
    epoch: u64,
    root: PathBuf,
    operation: impl FnOnce(&std::path::Path) -> io::Result<()> + Send + 'static,
) -> Task<Desktop> {
    file_task(
        move || {
            operation(&root)?;
            store::list(&root, false)
        },
        move |result| {
            Desktop::TerminalAgent(
                id,
                RunMessage::History(Message::Finished(epoch, Arc::new(result))),
            )
        },
    )
}

pub(super) fn file_task<T: Send + 'static>(
    operation: impl FnOnce() -> io::Result<T> + Send + 'static,
    complete: impl Fn(Result<T, String>) -> Desktop + Send + 'static,
) -> Task<Desktop> {
    Task::perform(
        async move {
            let (send, receive) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name("kortty-agent-history".into())
                .spawn(move || {
                    let _ = send.send_blocking(operation().map_err(|error| error.to_string()));
                })
                .map_err(|_| "Could not start the prompt history worker.")?;
            receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The prompt history worker stopped.".into()))
        },
        complete,
    )
}

pub(super) fn label(entry: &Entry) -> String {
    let mut goal: String = entry
        .goal
        .chars()
        .take(60)
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .collect();
    if entry.goal.chars().count() > 60 {
        goal.push('…');
    }
    let kind = match entry.kind {
        Kind::Run => "Run",
        Kind::Ask => "Ask",
        Kind::Plan => "Plan",
    };
    let date = i64::try_from(entry.started_at)
        .ok()
        .and_then(|value| jiff::Timestamp::from_millisecond(value).ok());
    date.map_or_else(
        || format!("{kind} · {goal}"),
        |date| format!("{kind} · {goal} · {date}"),
    )
}
