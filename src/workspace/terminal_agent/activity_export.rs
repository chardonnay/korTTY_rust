//! Reviews a fixed selection of retained runs before publishing a new local file.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use iced::Element;
use iced::widget::{button, column, pick_list, row, text, text_input};

use crate::terminal_agent::activity_report::{self as report, Format};
use crate::terminal_logging::Redactor;

use super::{
    Cancel, Desktop, LiveBinding, Message as RunMessage, Mode, PaneSession, Phase, Run, SessionId,
    Task, Workspace, history, mask, window,
};

const MAX_ARCHIVE: usize = 32;
const MAX_PATH: usize = 4096;
const MAX_FIELD: usize = 16 * 1024;

#[derive(Clone)]
pub enum Message {
    Open,
    Scope(Scope),
    Format(Format),
    Path(String),
    Confirm,
    Close,
    Rendered(u64, Result<Arc<Prepared>, String>),
    Written(u64, Result<(), String>),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Scope {
    This,
    All,
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::This => "This run",
            Self::All => "All retained runs of this terminal",
        })
    }
}

#[derive(Default)]
pub(super) struct Archive(VecDeque<(LiveBinding, Arc<report::Run>)>);

impl Archive {
    pub(super) fn capture(&mut self, run: &mut Run) {
        let (Some((began, started_at)), Ok(source)) =
            (run.activity_export.started.take(), run.binding.live())
        else {
            return;
        };
        if run.swarm || source.private {
            return;
        }
        let status = match run.phase {
            Phase::Finished => "Completed",
            Phase::Failed => "Failed",
            Phase::Blocked => "Blocked",
            Phase::Cancelled => "Cancelled",
            _ => return,
        };
        let usage = &run.presentation.model_usage;
        let snapshot = Arc::new(report::Run {
            mode: match run.mode {
                Mode::Run => "Run",
                Mode::Ask => "Ask",
                Mode::Plan => "Plan",
            },
            status,
            destination: run.label.clone(),
            profile_and_model: run
                .profile()
                .map_or_else(String::new, |profile| profile.label.clone()),
            prompt: run.goal.clone(),
            started_at: started_at.to_string(),
            finished_at: jiff::Timestamp::now().to_string(),
            elapsed_seconds: began.elapsed().as_secs(),
            activity: run.activity.iter().cloned().collect(),
            model_usage: usage.summary().map(|summary| report::ModelUsage {
                attempts: summary.attempts,
                prompt_tokens: report::TokenCount::new(summary.input.sum, summary.input.known),
                completion_tokens: report::TokenCount::new(
                    summary.output.sum,
                    summary.output.known,
                ),
                total_tokens: report::TokenCount::new(summary.total.sum, summary.total.known),
            }),
            model_attempts: usage.attempts().map(report::ModelAttempt::from).collect(),
        });
        let mut recent = 0;
        self.0.retain(|(old, _)| {
            if same_stream(*old, source) {
                recent += 1;
                recent < report::MAX_RUNS
            } else {
                true
            }
        });
        if self.0.len() >= MAX_ARCHIVE {
            self.0.pop_back();
        }
        self.0.push_front((source, Arc::clone(&snapshot)));
        run.activity_export.snapshot = Some(snapshot);
    }

    pub(super) fn remove_session(&mut self, session: SessionId) {
        self.0.retain(|(source, _)| source.session != session);
    }

    fn matching(&self, source: LiveBinding) -> Vec<Arc<report::Run>> {
        self.0
            .iter()
            .rev()
            .filter(|(old, _)| same_stream(*old, source))
            .map(|(_, run)| Arc::clone(run))
            .collect()
    }
}

fn same_stream(left: LiveBinding, right: LiveBinding) -> bool {
    left.session == right.session && left.stream == right.stream && !left.private && !right.private
}

struct Form {
    all: Vec<Arc<report::Run>>,
    scope: Scope,
    format: Format,
    path: String,
}

struct Pending {
    token: u64,
    publishing: bool,
    cancel: Cancel,
}

pub struct Prepared {
    destination: report::Destination,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub(super) struct Export {
    started: Option<(Instant, jiff::Timestamp)>,
    snapshot: Option<Arc<report::Run>>,
    form: Option<Form>,
    pending: Option<Pending>,
    notice: String,
}

impl Export {
    pub(super) fn start(&mut self) {
        self.started
            .get_or_insert_with(|| (Instant::now(), jiff::Timestamp::now()));
    }

    pub(super) fn writing(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn cancel(&self) {
        if let Some(pending) = &self.pending {
            pending.cancel.request();
        }
    }

    pub(super) fn view(&self) -> Option<Element<'_, RunMessage>> {
        self.snapshot.as_ref()?;
        let view: Element<'_, Message> = if let Some(form) = &self.form {
            let editable = !self.writing();
            column![
                text("Export masked activity to a new local file. The export includes the task and retained transcript."),
                row![
                    pick_list([Scope::This, Scope::All], Some(form.scope), Message::Scope),
                    pick_list(Format::ALL, Some(form.format), Message::Format),
                ].spacing(8).wrap(),
                text(format!("{} completed run(s) in this selection", if form.scope == Scope::All { form.all.len() } else { 1 })).size(13),
                text_input("Absolute path to a new export file", &form.path)
                    .on_input_maybe(editable.then_some(Message::Path)),
                row![
                    button("Export activity").on_press_maybe(
                        (editable && report::valid_path(Path::new(&form.path), form.format))
                            .then_some(Message::Confirm),
                    ),
                    button("Close export").on_press_maybe(editable.then_some(Message::Close)),
                ].spacing(8),
                text(&self.notice).size(13),
            ].spacing(7).into()
        } else {
            column![
                button("Export activity…").on_press(Message::Open),
                text(&self.notice).size(13),
            ]
            .spacing(4)
            .into()
        };
        Some(view.map(RunMessage::ActivityExport))
    }
}

impl Workspace {
    pub(super) fn agent_export_pane<'a>(
        &'a self,
        run: &Run,
    ) -> Result<&'a PaneSession, &'static str> {
        if run.swarm
            || run.binding.private()
            || !run.phase.finished()
            || run.activity_export.snapshot.is_none()
        {
            return Err("Only completed, non-private terminal agent activity can be exported.");
        }
        self.current_run_pane(run).map(|(_, pane)| pane)
    }

    pub(super) fn agent_activity_export(
        &mut self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Open if !run.closing && !run.activity_export.writing() => {
                let result = self.agent_export_pane(run).and_then(|_| run.binding.live());
                match result {
                    Ok(source) => {
                        let mut all = self.terminal_agents.activity.matching(source);
                        if all.is_empty()
                            && let Some(snapshot) = &run.activity_export.snapshot
                        {
                            all.push(Arc::clone(snapshot));
                        }
                        run.activity_export.form = Some(Form {
                            all,
                            scope: Scope::This,
                            format: Format::default(),
                            path: String::new(),
                        });
                        run.activity_export.notice.clear();
                    }
                    Err(error) => run.activity_export.notice = error.into(),
                }
            }
            Message::Confirm if !run.closing && !run.activity_export.writing() => {
                match self.prepare_activity_export(id, run) {
                    Ok(task) => return task,
                    Err(error) => run.activity_export.notice = error,
                }
            }
            Message::Rendered(token, result)
                if run
                    .activity_export
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.token == token && !pending.publishing) =>
            {
                let result = result.and_then(|prepared| {
                    self.agent_export_pane(run)?;
                    let pending = run
                        .activity_export
                        .pending
                        .as_ref()
                        .ok_or("The export request expired.")?;
                    if run.closing || pending.cancel.requested() {
                        return Err("The agent activity export was cancelled.".into());
                    }
                    Ok((prepared, pending.cancel.clone()))
                });
                match result {
                    Ok((prepared, cancel)) => {
                        if let Some(pending) = &mut run.activity_export.pending {
                            pending.publishing = true;
                        }
                        run.activity_export.notice = "Saving agent activity…".into();
                        return history::file_task(
                            move || {
                                prepared
                                    .destination
                                    .publish(&prepared.bytes, &|| cancel.requested())
                            },
                            move |result| event(id, Message::Written(token, result)),
                        );
                    }
                    Err(error) => finish(&mut run.activity_export, Err(error)),
                }
            }
            Message::Written(token, result)
                if run
                    .activity_export
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.token == token && pending.publishing) =>
            {
                finish(&mut run.activity_export, result);
            }
            _ if !run.closing && !run.activity_export.writing() => {
                let export = &mut run.activity_export;
                if let Some(form) = &mut export.form {
                    match message {
                        Message::Scope(scope) => form.scope = scope,
                        Message::Format(format) => form.format = format,
                        Message::Path(path) if path.len() <= MAX_PATH => form.path = path,
                        Message::Close => export.form = None,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn prepare_activity_export(
        &mut self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Task<Desktop>, String> {
        let pane = self.agent_export_pane(run)?;
        let redactor = self.agent_source_redactor(pane)?;
        let export = &mut run.activity_export;
        let form = export
            .form
            .as_ref()
            .ok_or("Open the agent activity export review first.")?;
        if !report::valid_path(Path::new(&form.path), form.format) {
            return Err(
                "Choose an absolute new export path with the selected format's extension.".into(),
            );
        }
        let selected = match form.scope {
            Scope::This => vec![Arc::clone(
                export
                    .snapshot
                    .as_ref()
                    .ok_or("The selected run is unavailable.")?,
            )],
            Scope::All => form.all.clone(),
        };
        let format = form.format;
        let path = PathBuf::from(&form.path);
        let token = self.allocate();
        let cancel = Cancel::default();
        export.pending = Some(Pending {
            token,
            publishing: false,
            cancel: cancel.clone(),
        });
        export.notice = "Preparing masked agent activity…".into();
        Ok(history::file_task(
            move || {
                let destination = report::Destination::review(path, format)?;
                let runs = selected
                    .iter()
                    .map(|run| remask(run, &redactor, &cancel))
                    .collect::<io::Result<Vec<_>>>()?;
                let bytes = report::render(format, jiff::Timestamp::now(), &runs, &|| {
                    cancel.requested()
                })?;
                Ok(Arc::new(Prepared { destination, bytes }))
            },
            move |result| event(id, Message::Rendered(token, result)),
        ))
    }
}

fn remask(run: &report::Run, redactor: &Redactor, cancel: &Cancel) -> io::Result<report::Run> {
    let masked = |value: &str| {
        if cancel.requested() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "The agent activity export was cancelled.",
            ));
        }
        let mut value = mask::prompt(redactor, value).map_err(io::Error::other)?;
        if value.len() > MAX_FIELD {
            value.truncate(value.floor_char_boundary(MAX_FIELD));
            value.push_str("\n[Activity clipped for export]");
        }
        Ok(value)
    };
    Ok(report::Run {
        destination: masked(&run.destination)?,
        profile_and_model: masked(&run.profile_and_model)?,
        prompt: masked(&run.prompt)?,
        activity: run
            .activity
            .iter()
            .map(|value| masked(value))
            .collect::<io::Result<Vec<_>>>()?,
        ..run.clone()
    })
}

fn event(id: window::Id, message: Message) -> Desktop {
    Desktop::TerminalAgent(id, RunMessage::ActivityExport(message))
}

fn finish(export: &mut Export, result: Result<(), String>) {
    export.pending = None;
    export.notice = match result {
        Ok(()) => {
            export.form = None;
            "Agent activity exported.".into()
        }
        Err(error) => error,
    };
}

#[cfg(test)]
mod tests;
