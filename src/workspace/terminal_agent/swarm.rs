//! A native swarm activity window over independently guarded terminal and saved targets.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use iced::widget::{
    Column, button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Length, Size, Task, window};

use crate::ai_manager::ProfileRef;
use crate::clipboard_policy::{AiAgentExecution, ClipboardPolicy};
use crate::scheduler_window::{AutomationEstimate, SwarmHandoff, SwarmJournalEdit};
use crate::terminal_agent::swarm::{self as engine, Key, Queue, Status, Target, Ticket};

use super::{
    Desktop, Message as RunMessage, Mode, Phase, ProfileChoice, Run, State, Workspace,
    dialog_placement, mask, palette::Palette, request_review,
};
use crate::workspace::saved_chats::swarm::Record;

pub(super) mod batch;
mod comparison;
mod conversation;
pub(super) mod headless;
mod journal;
mod notice;
mod restart;
pub(super) mod snippet;
#[cfg(test)]
mod tests;
mod workflow;

const SAVED_PROFILE_UNAVAILABLE: &str =
    "The saved AI profile is unavailable. Choose an available profile to continue.";

fn swarm_profiles(config: &crate::ai_manager::Configuration) -> Vec<ProfileChoice> {
    let mut profiles = super::profile_choices(config);
    profiles.extend(super::https_profile_choices(config));
    if crate::local_ai::cli::allowed(&ClipboardPolicy::load(), false) {
        profiles.extend(super::cli_profile_choices(config));
    }
    profiles
}

fn swarm_profile_entries(
    config: &crate::ai_manager::Configuration,
) -> impl Iterator<Item = (crate::ai_manager::ProfileId, &str, &str, String)> {
    config
        .agent_profiles()
        .chain(config.reviewed_agent_https_profiles())
        .chain(config.reviewed_agent_cli_profiles())
}

#[derive(Clone)]
pub enum Message {
    Goal(String),
    Profile(ProfileChoice),
    Toggle(Key, bool),
    Directory(Key, String),
    Mode(SwarmMode),
    Snippet(snippet::Choice),
    ScriptArguments(text_editor::Action),
    Snippets(Result<Vec<snippet::Choice>, &'static str>),
    SavedTargets(Result<Vec<headless::Source>, &'static str>),
    SnippetPrepared(u64, Result<snippet::Review, &'static str>),
    Journal(SwarmJournalEdit),
    SaveJournalDefaults,
    EstimateJournal,
    JournalEstimate(u64, Result<AutomationEstimate, &'static str>),
    OpenJournal(usize),
    Start,
    RefreshSavedTargets,
    ConnectionGroup(String),
    AddConnectionGroup,
    SaveConversation,
    SaveDraft,
    RenameConversation,
    ConversationTitle(String),
    ConfirmConversationSave,
    CancelConversationSave,
    ConversationSaved(u64, PathBuf, FollowUpResult),
    SearchHistory(String),
    MoveHistorySearch(bool),
    ReviewFollowUp,
    StartFollowUp(u64),
    FollowUpSaved(u64, FollowUpResult),
    Comparison(comparison::Message),
    Batch(batch::Message),
    Focus(Key),
    Member(window::Id, MemberAction),
    #[cfg(test)]
    Cancel(Key),
    StopTarget(Ticket),
    CancelAll,
    Pause(bool),
    PauseTarget(Ticket, bool),
    RestartTarget(Ticket),
    RestartRound(u64),
    CopyReport,
    ReportPath(String),
    ReportFormat(engine::report::Format),
    SaveReport,
    ReportWritten(u64, Result<(), &'static str>),
    ShowWorkflow,
    ScheduleDraft,
    Workflow(u64, workflow::Message),
    CloseWorkflow,
    Close,
    JournalFinished(Result<Option<crate::session_journal::automation::Artifact>, String>),
}

#[derive(Clone)]
pub struct FollowUpResult(Result<Record, &'static str>);

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AI Swarm action")
    }
}

#[derive(Clone)]
pub enum MemberAction {
    Approve(u64),
    Answer(u64, String),
    ReviewModel(request_review::Message),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum SwarmMode {
    Advisory,
    Agent,
    Snippet,
}

impl fmt::Display for SwarmMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Advisory => "AI advisory",
            Self::Agent => "AI command review",
            Self::Snippet => "Saved script review",
        })
    }
}

#[derive(Clone)]
struct Choice {
    target: Target,
    directory: Option<String>,
    saved: Option<headless::Source>,
}

struct Launch<'a> {
    mode: SwarmMode,
    profile: Option<&'a ProfileChoice>,
    prepared: Option<&'a snippet::Review>,
    goal: &'a str,
    continuation: Option<&'a conversation::Reviewed>,
}

#[derive(Clone, Copy)]
enum SaveKind {
    Conversation,
    Draft,
    Rename,
}

struct SaveName {
    kind: SaveKind,
    title: String,
    input: iced::widget::Id,
}

enum ConversationSave {
    Completed(Box<conversation::Initial>, Option<String>),
    Draft(Box<conversation::Draft>, String),
    Rename(Box<conversation::Rename>, String),
}

impl ConversationSave {
    fn write(self, root: &Path) -> Result<Record, &'static str> {
        match self {
            Self::Completed(snapshot, title) => snapshot.save(root, title),
            Self::Draft(snapshot, title) => snapshot.save(root, title),
            Self::Rename(snapshot, title) => snapshot.save(root, title),
        }
    }
}

pub(super) struct Group {
    owner: Option<window::Id>,
    choices: Vec<Choice>,
    live_routes: HashMap<Key, headless::Source>,
    selected: HashSet<Key>,
    connection_group: Option<String>,
    modes: Vec<SwarmMode>,
    profiles: Vec<ProfileChoice>,
    profile: Option<ProfileChoice>,
    goal: String,
    mode: SwarmMode,
    snippets: Vec<snippet::Choice>,
    snippet: Option<snippet::Choice>,
    arguments: text_editor::Content,
    prepared: Option<snippet::Review>,
    preparing: bool,
    discard_preparation: bool,
    journal: journal::State,
    journal_run: Option<(
        crate::session_journal::automation::CaptureSettings,
        crate::session_journal::automation::Run,
    )>,
    journal_allowed: bool,
    journals_pending: usize,
    report_path: String,
    report_format: engine::report::Format,
    report_writing: bool,
    workflow: Option<workflow::Form>,
    workflow_generation: u64,
    queue: Option<Queue>,
    members: HashMap<window::Id, Ticket>,
    restarts: restart::State,
    batch_review: Option<std::sync::Arc<batch::Review>>,
    focused: Option<Key>,
    round: u64,
    closing: bool,
    notifications: notice::State,
    notice: String,
    conversation: Option<conversation::Conversation>,
    save_name: Option<SaveName>,
    save_generation: u64,
    saving_conversation: bool,
    saved_round: Option<u64>,
    comparison: comparison::State,
}

impl Group {
    fn with_comparison(&self, report: String) -> String {
        match self.comparison.result() {
            Some(result) => format!("{report}\n\n## AI comparison\n\n{result}\n"),
            None => report,
        }
    }

    fn new(
        modes: Vec<SwarmMode>,
        profiles: Vec<ProfileChoice>,
        profile: Option<ProfileChoice>,
        journal_allowed: bool,
    ) -> Self {
        Self {
            owner: None,
            choices: Vec::new(),
            live_routes: HashMap::new(),
            selected: HashSet::new(),
            connection_group: None,
            mode: modes.first().copied().unwrap_or(SwarmMode::Advisory),
            modes,
            profiles,
            profile,
            goal: String::new(),
            snippets: Vec::new(),
            snippet: None,
            arguments: text_editor::Content::new(),
            prepared: None,
            preparing: false,
            discard_preparation: false,
            journal: journal::State::default(),
            journal_run: None,
            journal_allowed,
            journals_pending: 0,
            report_path: String::new(),
            report_format: engine::report::Format::default(),
            report_writing: false,
            workflow: None,
            workflow_generation: 0,
            queue: None,
            members: HashMap::new(),
            restarts: restart::State::default(),
            batch_review: None,
            focused: None,
            round: 0,
            closing: false,
            notifications: notice::State::default(),
            notice: String::new(),
            conversation: None,
            save_name: None,
            save_generation: 0,
            saving_conversation: false,
            saved_round: None,
            comparison: comparison::State::default(),
        }
    }

    pub(super) fn running(&self) -> bool {
        self.preparing
            || self.comparison.busy()
            || self.queue.as_ref().is_some_and(|queue| !queue.complete())
            || self.journals_pending != 0
            || self.report_writing
            || self.saving_conversation
            || self.conversation.as_ref().is_some_and(|chat| chat.saving)
            || self.workflow.as_ref().is_some_and(workflow::Form::running)
    }

    fn set_journal_allowed(&mut self, allowed: bool) {
        self.journal_allowed = allowed;
        if !allowed {
            self.journal.clear_estimate();
            self.journal_run = None;
        }
    }

    fn start_queue(&mut self) -> bool {
        let targets = self
            .choices
            .iter()
            .filter(|choice| self.selected.contains(&choice.target.key))
            .map(|choice| choice.target.clone())
            .collect();
        match Queue::new(self.round, targets, engine::MAX_PARALLEL) {
            Ok(queue) => {
                self.focused = queue.rows().first().map(|row| row.target.key);
                self.queue = Some(queue);
                self.batch_review = None;
                self.notifications = notice::State::new(self.round);
                self.notice.clear();
                true
            }
            Err(error) => {
                self.notice = error.into();
                false
            }
        }
    }

    fn setup_view(&self, running: bool) -> Column<'_, Message> {
        let conversation = self.conversation.as_ref();
        let draft = conversation.is_some_and(|chat| chat.record.is_draft());
        let mut targets = column![].spacing(6);
        for choice in &self.choices {
            let key = choice.target.key;
            let checked = self.selected.contains(&key);
            targets = targets.push(
                checkbox(checked)
                    .label(
                        choice
                            .saved
                            .as_ref()
                            .map_or(choice.target.label.as_str(), |source| source.label.as_str()),
                    )
                    .on_toggle_maybe(
                        (!running).then_some(move |value| Message::Toggle(key, value)),
                    ),
            );
            if checked
                && (self.mode != SwarmMode::Advisory || choice.saved.is_some())
                && let Some(directory) = &choice.directory
            {
                targets = targets.push(
                    text_input(
                        if choice.saved.is_some() {
                            "Absolute remote directory"
                        } else {
                            "Absolute local directory"
                        },
                        directory,
                    )
                    .on_input_maybe(
                        (!running).then_some(move |value| Message::Directory(key, value)),
                    ),
                );
            }
        }
        let ready = !running
            && self.modes.contains(&self.mode)
            && !self.selected.is_empty()
            && match self.mode {
                SwarmMode::Snippet => self.snippet.is_some(),
                _ => self.profile.is_some() && !self.goal.trim().is_empty(),
            }
            && self
                .choices
                .iter()
                .filter(|choice| self.selected.contains(&choice.target.key))
                .all(|choice| {
                    if conversation.is_some() {
                        choice.saved.is_some()
                            && choice
                                .directory
                                .as_deref()
                                .is_some_and(|directory| directory.starts_with('/'))
                    } else {
                        self.mode == SwarmMode::Advisory
                            || choice
                                .directory
                                .as_ref()
                                .is_none_or(|directory| !directory.trim().is_empty())
                    }
                });
        let mut view = column![].spacing(8).padding(12);
        view = if let Some(chat) = conversation {
            view.push(chat.history()).push(row![
                (!chat.record.is_draft()).then(|| button("Rename…")
                    .on_press_maybe((!running && !self.closing && !chat.loading).then_some(Message::RenameConversation))),
                text("Choose up to eight current saved SSH connections. Enter an absolute directory for each target.").size(13),
                button("Refresh connections and profiles").on_press_maybe((!running && !chat.loading).then_some(Message::RefreshSavedTargets)),
            ].spacing(6).wrap())
        } else {
            view.push(text("AI Swarm").size(22))
                .push(text("Choose up to eight live panes or saved SSH connections. Review model requests and commands for each target.").size(13))
        };
        let groups = self.connection_groups();
        if !groups.is_empty() {
            view = view.push(
                row![
                    pick_list(
                        groups,
                        self.connection_group.clone(),
                        Message::ConnectionGroup
                    )
                    .placeholder("Connection group"),
                    button("Add group targets").on_press_maybe(
                        (!running && self.connection_group.is_some())
                            .then_some(Message::AddConnectionGroup),
                    ),
                ]
                .spacing(6)
                .wrap(),
            );
        }
        view = view
            .push(scrollable(targets).height(Length::Fixed(150.0)))
            .push(pick_list(
                self.modes.as_slice(),
                self.modes.contains(&self.mode).then_some(self.mode),
                Message::Mode,
            ));
        if self.mode == SwarmMode::Snippet {
            view = view
                .push(pick_list(
                    self.snippets.as_slice(),
                    self.snippet.clone(),
                    Message::Snippet,
                ))
                .push(
                    text_editor(&self.arguments)
                        .height(70)
                        .placeholder("Optional literal arguments, one per line")
                        .on_action(Message::ScriptArguments),
                );
        } else {
            view = view
                .push(
                    pick_list(
                        self.profiles.as_slice(),
                        self.profile.clone(),
                        Message::Profile,
                    )
                    .placeholder("AI profile"),
                )
                .push(
                    text_input(
                        if draft {
                            "Task for the first run"
                        } else if conversation.is_some() {
                            "Fresh question for the selected targets"
                        } else {
                            "Task for the selected targets"
                        },
                        &self.goal,
                    )
                    .on_input_maybe((!running).then_some(Message::Goal)),
                );
        }
        if !running && self.journal_allowed {
            view = view.push(self.journal.view(self.selected.len()));
        }
        if let Some(chat) = conversation {
            view = view.push(
                row![
                    button(if draft {
                        "Review first run…"
                    } else {
                        "Review follow-up…"
                    })
                    .on_press_maybe((ready && !chat.loading).then_some(Message::ReviewFollowUp)),
                    draft.then(|| button("Save draft changes…").on_press_maybe(
                        (!running && self.can_save_draft()).then_some(Message::SaveDraft)
                    )),
                ]
                .spacing(8)
                .wrap(),
            );
            if let Some(review) = &chat.review {
                view = view.push(review.view(draft));
            }
        } else {
            view = view.push(
                row![
                    button("Start swarm").on_press_maybe(ready.then_some(Message::Start)),
                    button("Save Swarm draft…").on_press_maybe(
                        (!running && self.can_save_draft()).then_some(Message::SaveDraft)
                    ),
                    button("Generate workflow…").on_press_maybe(
                        (!running
                            && self.modes.contains(&SwarmMode::Advisory)
                            && !self.goal.trim().is_empty())
                        .then_some(Message::ShowWorkflow)
                    ),
                ]
                .spacing(8)
                .wrap(),
            );
        }
        if self.mode != SwarmMode::Snippet {
            view = view.push(button("Schedule…").on_press_maybe(
                (!running && self.can_schedule()).then_some(Message::ScheduleDraft),
            ));
        }
        view
    }

    fn schedule_prompt(&self) -> Option<&str> {
        let composer = self.goal.trim();
        if !composer.is_empty() {
            return Some(composer);
        }
        self.conversation
            .as_ref()?
            .record
            .turns
            .iter()
            .rev()
            .map(|turn| turn.question.trim())
            .find(|question| !question.is_empty())
    }

    fn can_schedule(&self) -> bool {
        self.mode != SwarmMode::Snippet
            && self.profile.is_some()
            && self.schedule_prompt().is_some()
            && self.conversation.as_ref().is_none_or(|chat| !chat.loading)
            && self.choices.iter().any(|choice| {
                self.selected.contains(&choice.target.key)
                    && !choice.target.private
                    && self.catalog_source(choice).is_some()
            })
    }

    fn connection_groups(&self) -> Vec<String> {
        let mut groups = BTreeSet::new();
        for path in self
            .choices
            .iter()
            .filter_map(|choice| self.catalog_source(choice)?.group.as_deref())
        {
            let mut path = path;
            loop {
                groups.insert(path.to_owned());
                let Some((parent, _)) = path.rsplit_once('/') else {
                    break;
                };
                path = parent;
            }
        }
        groups.into_iter().collect()
    }

    fn add_connection_group(&mut self) {
        let Some(group) = &self.connection_group else {
            return;
        };
        let mut selected = self.selected.clone();
        selected.extend(self.choices.iter().filter_map(|choice| {
            let path = self.catalog_source(choice)?.group.as_deref()?;
            crate::connection_store::group_suffix(path, group).map(|_| choice.target.key)
        }));
        if selected.len() > engine::MAX_TARGETS {
            self.notice = "That connection group exceeds the eight-target limit.".into();
        } else {
            self.selected = selected;
            self.notice.clear();
        }
    }

    fn catalog_source<'a>(&'a self, choice: &'a Choice) -> Option<&'a headless::Source> {
        choice
            .saved
            .as_ref()
            .or_else(|| self.live_routes.get(&choice.target.key))
    }

    fn can_save_draft(&self) -> bool {
        let can_edit = match &self.conversation {
            Some(chat) => chat.record.is_draft() && !chat.loading,
            None => self
                .queue
                .as_ref()
                .is_none_or(|queue| queue.complete() && !self.comparison.can_save_initial(queue)),
        };
        can_edit
            && self.mode != SwarmMode::Snippet
            && self.profile.is_some()
            && !self.selected.is_empty()
            && self
                .choices
                .iter()
                .filter(|choice| self.selected.contains(&choice.target.key))
                .all(|choice| !choice.target.private && self.catalog_source(choice).is_some())
    }

    fn focused_member<'a>(&self, state: &'a State) -> Option<(window::Id, &'a Run)> {
        let key = self.focused?;
        self.members.iter().find_map(|(&id, ticket)| {
            (ticket.key() == key)
                .then(|| state.runs.get(&id).map(|run| (id, run)))
                .flatten()
        })
    }

    fn view<'a>(
        &'a self,
        state: &'a State,
        palette: Option<Palette>,
        batch_allowed: bool,
    ) -> Element<'a, Message> {
        let muted = palette.map(|palette| palette.muted.color());
        if let Some(save) = &self.save_name {
            let editing = !self.saving_conversation && !self.closing;
            let valid = editing && save.valid();
            return column![
                text(match save.kind {
                    SaveKind::Conversation => "Name the Swarm conversation",
                    SaveKind::Draft => "Name the Swarm draft",
                    SaveKind::Rename => "Rename the Swarm conversation",
                }).size(20),
                text(match save.kind {
                    SaveKind::Draft => "The draft stores the selected saved SSH connections and profile. The task is saved after the first completed run.",
                    SaveKind::Conversation => "Leave the title blank to use the first line of the question.",
                    SaveKind::Rename => "Enter a title for this saved conversation.",
                }).size(13),
                text_input("Title", &save.title)
                    .id(save.input.clone())
                    .on_input_maybe(editing.then_some(Message::ConversationTitle))
                    .on_submit_maybe(valid.then_some(Message::ConfirmConversationSave)),
                row![
                    button(if self.saving_conversation { "Saving…" } else if matches!(save.kind, SaveKind::Rename) { "Rename" } else { "Save" })
                        .on_press_maybe(valid.then_some(Message::ConfirmConversationSave)),
                    button("Cancel").on_press_maybe(editing.then_some(Message::CancelConversationSave)),
                ].spacing(8),
                text(&self.notice).size(13).style(move |_| text::Style { color: muted }),
            ].spacing(10).padding(12).into();
        }
        if let Some(workflow) = &self.workflow {
            let generation = self.workflow_generation;
            return column![
                workflow
                    .view()
                    .map(move |message| Message::Workflow(generation, message)),
                text(&self.notice)
                    .size(13)
                    .style(move |_| text::Style { color: muted }),
                button("Back to swarm").on_press(Message::CloseWorkflow),
            ]
            .spacing(8)
            .padding(12)
            .into();
        }
        let running = self.running();
        let mut view = if self.comparison.busy()
            || self.queue.as_ref().is_some_and(|queue| !queue.complete())
        {
            column![text("AI Swarm").size(22)].spacing(8).padding(12)
        } else {
            self.setup_view(running)
        };
        if let Some(queue) = &self.queue {
            let mut rows = column![].spacing(6);
            for item in queue.rows() {
                let key = item.target.key;
                let ticket = queue.ticket(key).expect("visible Swarm target");
                let held = queue.held(ticket);
                let restarting = self.restarts.target_pending(key);
                let status = if restarting {
                    "Restarting"
                } else if held && item.status == Status::Queued {
                    "Paused · queued"
                } else {
                    item.status.label()
                };
                let status_color = palette.map(|palette| match item.status {
                    Status::Failed | Status::Blocked => palette.error.color(),
                    Status::Review | Status::Running => palette.accent.color(),
                    _ => palette.muted.color(),
                });
                rows = rows.push(
                    row![
                        button(text(&item.target.label)).on_press(Message::Focus(key)),
                        text(status).style(move |_| text::Style {
                            color: status_color
                        }),
                        text(&item.detail)
                            .size(13)
                            .style(move |_| text::Style { color: muted }),
                        button("Stop target").on_press_maybe(
                            ((!item.status.finished() && item.status != Status::Cancelling)
                                || restarting)
                                .then_some(Message::StopTarget(ticket))
                        ),
                        (self.mode != SwarmMode::Snippet).then(|| button(if held {
                            "Resume target"
                        } else {
                            "Pause target"
                        })
                        .on_press_maybe(
                            self.can_hold_target(ticket)
                                .then_some(Message::PauseTarget(ticket, !held))
                        )),
                        (self.mode != SwarmMode::Snippet).then(|| button("Restart target")
                            .on_press_maybe(
                                self.can_restart_target(ticket)
                                    .then_some(Message::RestartTarget(ticket))
                            )),
                    ]
                    .spacing(8)
                    .wrap(),
                );
            }
            view = view.push(scrollable(rows).height(Length::Fixed(150.0)));
            view = view.push(
                self.batch_view(state, batch_allowed)
                    .map(|view| view.map(Message::Batch)),
            );
            if let Some(key) = self.focused {
                let item = queue.rows().iter().find(|item| item.target.key == key);
                let member = self.focused_member(state);
                if let Some(item) = item {
                    view = view.push(text(&item.target.label));
                }
                if let Some((id, run)) = member.filter(|(_, run)| !run.cancel.requested())
                    && let Some(review) = run.model_review_view(Length::Fixed(190.0))
                {
                    view = view.push(review.map(move |message| {
                        Message::Member(id, MemberAction::ReviewModel(message))
                    }));
                }
                if let Some(item) = item.filter(|item| !item.transcript.is_empty()) {
                    view =
                        view.push(scrollable(text(&item.transcript)).height(Length::Fixed(150.0)));
                }
                if let Some((id, run)) = member {
                    if let Some((purpose, command)) = run.reviewed_command() {
                        if let Some(super::RunTarget::Saved(review)) = &run.target {
                            for (endpoint, fingerprint) in review.fingerprints() {
                                view =
                                    view.push(text(format!("{endpoint} · {fingerprint}")).size(12));
                            }
                        }
                        view = view.push(
                            column![
                                text(format!(
                                    "{} · {}",
                                    purpose,
                                    run.target.as_ref().map_or("", |target| target.directory())
                                )),
                                scrollable(text(command).font(iced::Font::MONOSPACE))
                                    .height(Length::Fixed(75.0)),
                                button("Approve for this target once").on_press_maybe(
                                    (!run.paused && !run.cancel.requested()).then_some(
                                        Message::Member(id, MemberAction::Approve(run.epoch))
                                    )
                                ),
                            ]
                            .spacing(5),
                        );
                    }
                    if let Some((question, answers)) = &run.question {
                        let mut controls = row![].spacing(6);
                        for answer in answers {
                            controls = controls.push(button(text(answer)).on_press_maybe(
                                (!run.paused && !run.cancel.requested()).then(|| {
                                    Message::Member(
                                        id,
                                        MemberAction::Answer(run.epoch, answer.clone()),
                                    )
                                }),
                            ));
                        }
                        view = view.push(column![text(question), controls.wrap()].spacing(6));
                    }
                }
            }
        }
        if self.conversation.as_ref().map_or_else(
            || self.queue.as_ref().is_some_and(Queue::complete),
            |chat| !chat.record.is_draft(),
        ) {
            view = view.push(
                row![
                    pick_list(
                        &engine::report::Format::ALL[..],
                        Some(self.report_format),
                        Message::ReportFormat
                    ),
                    text_input("Absolute report path", &self.report_path)
                        .on_input_maybe((!running).then_some(Message::ReportPath)),
                    button(if self.conversation.is_some() {
                        "Export conversation"
                    } else {
                        "Save report"
                    })
                    .on_press_maybe(
                        (!running && !self.report_path.trim().is_empty())
                            .then_some(Message::SaveReport)
                    ),
                ]
                .spacing(6),
            );
        }
        if self.conversation.is_none()
            && self.saved_round != Some(self.round)
            && self
                .queue
                .as_ref()
                .is_some_and(|queue| self.comparison.can_save_initial(queue))
        {
            view = view.push(
                button("Save conversation…")
                    .on_press_maybe((!running).then_some(Message::SaveConversation)),
            );
        }
        if !self.journal.links.is_empty() {
            let mut journals = row![text("Session journals")].spacing(6);
            for (index, _) in self.journal.links.iter().enumerate() {
                journals = journals.push(
                    button(text(format!("Open journal {}", index + 1)))
                        .on_press(Message::OpenJournal(index)),
                );
            }
            view = view.push(journals.wrap());
        }
        column![
            self.queue
                .as_ref()
                .filter(|queue| queue.complete())
                .and_then(|_| {
                    self.comparison
                        .view(!running && !self.closing)
                        .map(|view| view.map(Message::Comparison))
                }),
            scrollable(view).height(Length::Fill),
            column![
                text(&self.notice)
                    .size(13)
                    .style(move |_| text::Style { color: muted }),
                row![
                    button("Pause all")
                        .on_press_maybe(self.can_pause_round().then_some(Message::Pause(true))),
                    button("Resume all")
                        .on_press_maybe(self.can_pause_round().then_some(Message::Pause(false))),
                    button("Stop all").on_press_maybe(running.then_some(Message::CancelAll)),
                    button("Restart Swarm").on_press_maybe(
                        self.can_restart_round()
                            .then_some(Message::RestartRound(self.round))
                    ),
                    button("Copy comparison")
                        .on_press_maybe(self.queue.is_some().then_some(Message::CopyReport)),
                    button("Close").on_press(Message::Close),
                ]
                .spacing(6)
                .wrap(),
            ]
            .spacing(6)
            .padding(12),
        ]
        .into()
    }
}

impl SaveName {
    fn new(kind: SaveKind, title: String) -> Self {
        Self {
            kind,
            title,
            input: iced::widget::Id::unique(),
        }
    }

    fn focus(&self) -> Task<Desktop> {
        iced::widget::operation::focus(self.input.clone())
            .chain(iced::widget::operation::select_all(self.input.clone()))
    }

    fn valid(&self) -> bool {
        let title = self.title.trim();
        (matches!(self.kind, SaveKind::Conversation) && title.is_empty())
            || crate::workspace::saved_chats::swarm::valid_title(title)
    }
}

impl State {
    pub(super) fn swarm_view(
        &self,
        id: window::Id,
        palette: Option<Palette>,
        batch_allowed: bool,
    ) -> Option<Element<'_, Message>> {
        self.swarms
            .get(&id)
            .map(|group| group.view(self, palette, batch_allowed))
    }

    pub(super) fn swarm_focused_run(&self, id: window::Id) -> Option<&Run> {
        self.swarms
            .get(&id)?
            .focused_member(self)
            .map(|(_, run)| run)
    }

    pub(super) fn swarm_for_member(&self, id: window::Id) -> Option<window::Id> {
        self.swarms
            .iter()
            .find_map(|(&owner, group)| group.members.contains_key(&id).then_some(owner))
    }
}

impl Workspace {
    pub(in crate::workspace) fn agent_swarm_available(&self) -> bool {
        self.swarm_policy().is_ok()
    }

    pub(in crate::workspace) fn open_agent_swarm(&mut self, owner: window::Id) -> Task<Desktop> {
        if let Err(error) = self.swarm_policy() {
            self.notice(owner, error.into());
            return Task::none();
        }
        let config = self.ai_configuration();
        let mut profiles = config.as_deref().map(swarm_profiles).unwrap_or_default();
        if !crate::local_ai::cli::allowed(&self.clipboard.policy, false) {
            profiles.retain(|profile| profile.reviewed_cli.is_none());
        }
        let profile = config
            .as_ref()
            .and_then(|config| {
                config
                    .agent_profile_id()
                    .or_else(|| config.preferred_agent_cli_id())
            })
            .and_then(|id| profiles.iter().find(|profile| profile.id == id))
            .or_else(|| {
                if config
                    .as_ref()
                    .is_some_and(|config| config.has_named_agent_profile())
                {
                    None
                } else {
                    profiles
                        .iter()
                        .find(|profile| !profile.is_reviewed_outbound())
                }
            })
            .cloned();
        let modes = self.available_swarm_modes(!profiles.is_empty());
        let mut choices = self
            .sessions
            .keys()
            .filter_map(|&session| {
                let (_, pane, tab) = self.agent_pane(session)?;
                if tab.incognito || Self::agent_source_available(pane).is_err() {
                    return None;
                }
                let label = pane
                    .ssh
                    .as_ref()
                    .map_or_else(|| "Local shell".into(), |ssh| ssh.label());
                let redactor = self.agent_source_redactor(pane).ok()?;
                let label = format!(
                    "{} · pane {}",
                    mask::prompt(&redactor, &label).ok()?,
                    session.0
                );
                let directory = if cfg!(windows) {
                    pane.local_security.as_ref().map(|settings| {
                        settings
                            .working_directory
                            .as_deref()
                            .and_then(|path| path.to_str())
                            .unwrap_or_default()
                            .to_owned()
                    })
                } else {
                    None
                };
                Some(Choice {
                    target: Target {
                        key: Key::Live {
                            session,
                            stream: pane.stream_generation,
                        },
                        label,
                        private: false,
                    },
                    directory,
                    saved: None,
                })
            })
            .collect::<Vec<_>>();
        choices.sort_by_key(|choice| (choice.target.label.clone(), choice.target.key.stable()));
        let mut group = Group::new(
            modes,
            profiles,
            profile,
            self.swarm_journal_capture_allowed(),
        );
        group.choices = choices;
        let (id, opened) = self.install_swarm_group(owner, group);
        opened.chain(Task::batch([
            snippet::catalog(id),
            headless::catalog(id, None),
        ]))
    }

    fn install_swarm_group(
        &mut self,
        owner: window::Id,
        mut group: Group,
    ) -> (window::Id, Task<Desktop>) {
        group.owner = Some(owner);
        if let Err(error) = group.journal.load_defaults() {
            group.notice = format!("Could not load Swarm journal defaults ({error}).");
        }
        let (id, opened) = self.open_placed_dialog(
            dialog_placement::Kind::AgentSwarm,
            window::Settings {
                size: Size::new(920.0, 720.0),
                min_size: Some(Size::new(610.0, 500.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.terminal_agents.swarms.insert(id, group);
        (id, opened.map(|_| Desktop::Noop))
    }

    pub(in crate::workspace) fn open_saved_swarm(
        &mut self,
        owner: window::Id,
        root: PathBuf,
        record: Record,
    ) -> Task<Desktop> {
        let policy = ClipboardPolicy::load();
        if self.exiting
            || self.scheduler_draining
            || !self.windows.contains_key(&owner)
            || !self.ai_chat_enabled()
            || !self.clipboard.policy.ai_swarm_allowed()
            || !policy.ai_chat_allowed
            || !policy.ai_swarm_allowed()
            || !crate::profile::root().is_ok_and(|current| current == root)
        {
            self.notice(
                owner,
                "Saved AI Swarm conversations are unavailable for the current settings.".into(),
            );
            return Task::none();
        }
        if !crate::workspace::saved_chats::swarm::load(&root, &record.id)
            .is_ok_and(|current| current.same_revision(&record))
        {
            self.notice(
                owner,
                "The saved Swarm conversation changed or is unavailable. Refresh Saved Chats."
                    .into(),
            );
            return Task::none();
        }
        if let Some((&id, group)) = self.terminal_agents.swarms.iter_mut().find(|(_, group)| {
            group
                .conversation
                .as_ref()
                .is_some_and(|chat| chat.root == root && chat.record.id == record.id)
        }) {
            let chat = group.conversation.as_mut().expect("saved conversation");
            if !chat.record.same_revision(&record) {
                chat.review = None;
                group.notice =
                    "The saved Swarm conversation changed. Close and reopen it before continuing."
                        .into();
            }
            return window::minimize(id, false).chain(window::gain_focus(id));
        }
        let (profiles, profile) = self.saved_swarm_profiles(&record.profile_id);
        let modes = self.saved_swarm_modes(!profiles.is_empty());
        let prior = record.target_ids().collect::<Vec<_>>();
        let mut group = Group::new(
            modes,
            profiles,
            profile,
            self.swarm_journal_capture_allowed(),
        );
        if group.profile.is_none() {
            group.notice = SAVED_PROFILE_UNAVAILABLE.into();
        }
        group.choices = prior
            .iter()
            .map(|&id| conversation::saved_choice(id, None))
            .collect();
        if prior.len() <= engine::MAX_TARGETS {
            group.selected = prior.iter().copied().map(Key::Saved).collect();
        } else if group.profile.is_some() {
            group.notice = "Select up to eight targets for an interactive follow-up.".into();
        }
        group.conversation = Some(conversation::Conversation::new(
            root,
            record,
            group.profile.as_ref(),
        ));
        let (id, opened) = self.install_swarm_group(owner, group);
        opened.chain(headless::catalog(id, Some(prior)))
    }

    pub(in crate::workspace) fn revoke_saved_swarm(
        &mut self,
        root: &Path,
        record: &str,
    ) -> Task<Desktop> {
        let id = self.terminal_agents.swarms.iter().find_map(|(&id, group)| {
            group
                .conversation
                .as_ref()
                .is_some_and(|chat| chat.root == root && chat.record.id == record)
                .then_some(id)
        });
        id.map_or_else(Task::none, |id| self.swarm_action(id, Message::Close))
    }

    pub(super) fn swarm_policy(&self) -> Result<(), &'static str> {
        self.swarm_policy_for(SwarmMode::Snippet).map(drop)
    }

    pub(super) fn swarm_ai_policy(&self, mode: Mode) -> Result<(), &'static str> {
        self.swarm_policy_for(ai_mode(mode)).map(drop)
    }

    fn swarm_policy_for(&self, mode: SwarmMode) -> Result<ClipboardPolicy, &'static str> {
        require_swarm_policy(&self.clipboard.policy, mode)?;
        let policy = ClipboardPolicy::load();
        require_swarm_policy(&policy, mode)?;
        if mode != SwarmMode::Snippet && !self.ai_features_enabled() {
            return Err("AI is unavailable under the current settings.");
        }
        Ok(policy)
    }

    fn available_swarm_modes(&self, has_profile: bool) -> Vec<SwarmMode> {
        let mut modes = Vec::new();
        if has_profile && let Ok(policy) = self.swarm_policy_for(SwarmMode::Advisory) {
            modes.push(SwarmMode::Advisory);
            if require_swarm_policy(&self.clipboard.policy, SwarmMode::Agent).is_ok()
                && require_swarm_policy(&policy, SwarmMode::Agent).is_ok()
            {
                modes.push(SwarmMode::Agent);
            }
        }
        modes.push(SwarmMode::Snippet);
        modes
    }

    fn saved_swarm_modes(&self, has_profile: bool) -> Vec<SwarmMode> {
        if !self.ai_chat_enabled() || !ClipboardPolicy::load().ai_chat_allowed {
            return Vec::new();
        }
        self.available_swarm_modes(has_profile)
            .into_iter()
            .filter(|mode| *mode != SwarmMode::Snippet)
            .collect()
    }

    fn saved_swarm_profiles(
        &self,
        selection: &ProfileRef,
    ) -> (Vec<ProfileChoice>, Option<ProfileChoice>) {
        let Some(config) = self.ai_configuration() else {
            return (Vec::new(), None);
        };
        let profiles = swarm_profiles(&config);
        let selected = config
            .resolve_profile_reference(selection)
            .and_then(|id| profiles.iter().find(|profile| profile.id == id))
            .cloned();
        (profiles, selected)
    }

    pub(super) fn swarm_action(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        let Some(mut group) = self.terminal_agents.swarms.remove(&id) else {
            return Task::none();
        };
        if let Message::Batch(message) = message {
            return self.swarm_batch_action(id, group, message);
        }
        if matches!(
            message,
            Message::Journal(_) | Message::SaveJournalDefaults | Message::EstimateJournal
        ) {
            group.set_journal_allowed(self.swarm_journal_capture_allowed());
        }
        if matches!(message, Message::Toggle(_, _) | Message::AddConnectionGroup) {
            group.journal.clear_estimate();
        }
        let running = group.running();
        let setup = !running && group.workflow.is_none() && group.save_name.is_none();
        if setup
            && let Some(chat) = &mut group.conversation
            && matches!(
                &message,
                Message::Goal(_)
                    | Message::Profile(_)
                    | Message::Toggle(_, _)
                    | Message::Directory(_, _)
                    | Message::Mode(_)
                    | Message::AddConnectionGroup
                    | Message::RefreshSavedTargets
            )
        {
            chat.review = None;
        }
        match message {
            Message::Goal(goal) if setup && goal.len() <= super::MAX_GOAL => group.goal = goal,
            Message::Profile(profile) if setup && group.profiles.contains(&profile) => {
                if let Some(chat) = &mut group.conversation {
                    chat.profile_selection = profile.reference.clone();
                    if group.notice == SAVED_PROFILE_UNAVAILABLE {
                        group.notice.clear();
                    }
                }
                group.profile = Some(profile)
            }
            Message::Toggle(key, true)
                if setup
                    && group.selected.len() < engine::MAX_TARGETS
                    && group.choices.iter().any(|choice| choice.target.key == key) =>
            {
                group.selected.insert(key);
            }
            Message::Toggle(key, false) if setup => {
                group.selected.remove(&key);
            }
            Message::ConnectionGroup(path)
                if setup && group.connection_groups().contains(&path) =>
            {
                group.connection_group = Some(path);
            }
            Message::AddConnectionGroup if setup => group.add_connection_group(),
            Message::Directory(key, directory) if setup && directory.len() <= 4096 => {
                if let Some(choice) = group
                    .choices
                    .iter_mut()
                    .find(|choice| choice.target.key == key && choice.directory.is_some())
                {
                    choice.directory = Some(directory);
                }
            }
            Message::Mode(mode) if setup && group.modes.contains(&mode) => {
                match self.swarm_policy_for(mode) {
                    Ok(_) => group.mode = mode,
                    Err(error) => group.notice = error.into(),
                }
            }
            Message::Snippets(result) => match result {
                Ok(choices) => group.snippets = choices,
                Err(error) => group.notice = error.into(),
            },
            Message::RefreshSavedTargets if setup => {
                if let Some(chat) = &mut group.conversation
                    && !chat.loading
                {
                    chat.loading = true;
                    (group.profiles, group.profile) =
                        self.saved_swarm_profiles(&chat.profile_selection);
                    if let Some(profile) = &group.profile {
                        chat.profile_selection = profile.reference.clone();
                        if group.notice == SAVED_PROFILE_UNAVAILABLE {
                            group.notice.clear();
                        }
                    } else {
                        group.notice = SAVED_PROFILE_UNAVAILABLE.into();
                    }
                    group.modes = self.saved_swarm_modes(!group.profiles.is_empty());
                    let prior = group
                        .choices
                        .iter()
                        .filter_map(|choice| match choice.target.key {
                            Key::Saved(id) if group.selected.contains(&choice.target.key) => {
                                Some(id)
                            }
                            _ => None,
                        })
                        .collect();
                    self.terminal_agents.swarms.insert(id, group);
                    return headless::catalog(id, Some(prior));
                }
            }
            Message::SavedTargets(result) if group.conversation.is_some() => {
                let chat = group.conversation.as_mut().expect("saved conversation");
                if chat.loading {
                    chat.loading = false;
                    match result {
                        Ok(sources) => {
                            chat.catalog_result(&mut group.choices, &group.selected, sources)
                        }
                        Err(error) => group.notice = error.into(),
                    }
                }
            }
            Message::SavedTargets(result) => match result {
                Ok(sources) => {
                    for source in sources {
                        let id = source.reference.id;
                        let duplicates = group
                            .choices
                            .iter()
                            .filter_map(|choice| {
                                let (session, stream) = choice.target.key.pane()?;
                                let (_, pane, _) = self.agent_pane(session)?;
                                let ssh = pane.ssh.as_ref()?;
                                (pane.stream_generation == stream
                                    && ssh.saved_id() == Some(id)
                                    && ssh.matches_endpoints(
                                        &source.reference.target,
                                        source.reference.jump.as_ref(),
                                    ))
                                .then_some(choice.target.key)
                            })
                            .collect::<Vec<_>>();
                        if duplicates.is_empty() {
                            group.choices.push(Choice {
                                target: Target {
                                    key: Key::Saved(id),
                                    label: format!("Saved SSH connection {id}"),
                                    private: false,
                                },
                                directory: Some(String::new()),
                                saved: Some(source),
                            });
                        } else {
                            group
                                .live_routes
                                .extend(duplicates.into_iter().map(|key| (key, source.clone())));
                        }
                    }
                }
                Err(error) => group.notice = error.into(),
            },
            Message::Snippet(choice) if setup && group.snippets.contains(&choice) => {
                group.snippet = Some(choice);
            }
            Message::ScriptArguments(action) if setup => {
                edit_bounded(&mut group.arguments, action, snippet::MAX_ARGUMENT_BYTES);
            }
            Message::Journal(edit) if setup && group.journal_allowed => {
                group.journal.edit(edit);
            }
            Message::SaveJournalDefaults if setup && group.journal_allowed => {
                group.notice = match group.journal.save_defaults() {
                    Ok(true) => "Swarm journal defaults saved.".into(),
                    Ok(false) => {
                        "Swarm journal defaults saved. Could not confirm durable storage.".into()
                    }
                    Err(error) => format!("Could not save Swarm journal defaults ({error})."),
                };
            }
            Message::EstimateJournal if setup && group.journal_allowed => {
                let targets = group.selected.len();
                let task = group.journal.estimate(self, id, targets);
                self.terminal_agents.swarms.insert(id, group);
                return task;
            }
            Message::JournalEstimate(epoch, result) => group.journal.finish_estimate(epoch, result),
            Message::OpenJournal(index) => {
                if let Some((source, owner)) =
                    group.journal.links.get(index).cloned().zip(group.owner)
                    && self.windows.contains_key(&owner)
                    && self
                        .journal_directory()
                        .is_ok_and(|root| source.directory().parent() == Some(root.as_path()))
                {
                    self.terminal_agents.swarms.insert(id, group);
                    return self.open_journal_viewer(owner, source, "AI Swarm journal".into());
                }
            }
            Message::SearchHistory(query) => {
                if let Some(chat) = &mut group.conversation {
                    chat.set_search(query);
                }
            }
            Message::MoveHistorySearch(forward) => {
                if let Some(chat) = &mut group.conversation {
                    chat.move_search(forward);
                }
            }
            Message::SaveConversation
                if setup
                    && !group.closing
                    && group.conversation.is_none()
                    && group.saved_round != Some(group.round)
                    && group
                        .queue
                        .as_ref()
                        .is_some_and(|queue| group.comparison.can_save_initial(queue)) =>
            {
                let save = SaveName::new(SaveKind::Conversation, String::new());
                let focus = save.focus();
                group.save_name = Some(save);
                group.notice.clear();
                self.terminal_agents.swarms.insert(id, group);
                return focus;
            }
            Message::SaveDraft if setup && !group.closing && group.can_save_draft() => {
                let title = group.conversation.as_ref().map_or_else(
                    || "AI Swarm".to_owned(),
                    |chat| chat.record.title().to_owned(),
                );
                let save = SaveName::new(SaveKind::Draft, title);
                let focus = save.focus();
                group.save_name = Some(save);
                group.notice.clear();
                self.terminal_agents.swarms.insert(id, group);
                return focus;
            }
            Message::RenameConversation if setup && !group.closing => {
                if let Some(chat) = group
                    .conversation
                    .as_ref()
                    .filter(|chat| !chat.loading && !chat.record.is_draft())
                    && self.swarm_chat_access_allowed()
                {
                    let save = SaveName::new(SaveKind::Rename, chat.record.title().to_owned());
                    let focus = save.focus();
                    group.save_name = Some(save);
                    group.notice.clear();
                    self.terminal_agents.swarms.insert(id, group);
                    return focus;
                }
            }
            Message::ConversationTitle(title)
                if !running && !group.closing && title.len() <= 480 =>
            {
                if let Some(save) = &mut group.save_name {
                    save.title = title;
                    group.notice.clear();
                }
            }
            Message::CancelConversationSave if !running => {
                if group.save_name.take().is_some() {
                    group.notice.clear();
                }
            }
            Message::ConfirmConversationSave if !running && !group.closing => {
                if let Some(save) = group.save_name.as_ref().filter(|save| save.valid()) {
                    match self.swarm_conversation_save(&group, save) {
                        Ok((root, snapshot)) => {
                            group.saving_conversation = true;
                            group.notice.clear();
                            group.save_generation += 1;
                            let generation = group.save_generation;
                            let target = root.clone();
                            self.terminal_agents.swarms.insert(id, group);
                            return snippet::work(
                                "kortty-swarm-conversation",
                                move || snapshot.write(&target),
                                move |result| {
                                    Desktop::TerminalAgent(
                                        id,
                                        RunMessage::Swarm(Message::ConversationSaved(
                                            generation,
                                            root.clone(),
                                            FollowUpResult(result),
                                        )),
                                    )
                                },
                            );
                        }
                        Err(error) => group.notice = error,
                    }
                }
            }
            Message::ConversationSaved(generation, root, result)
                if group.saving_conversation && generation == group.save_generation =>
            {
                group.saving_conversation = false;
                match result.0 {
                    Ok(record) => {
                        let renamed = group
                            .save_name
                            .take()
                            .is_some_and(|save| matches!(save.kind, SaveKind::Rename));
                        self.record_saved_swarm(&root, &record);
                        if renamed {
                            if let Some(chat) = &mut group.conversation {
                                if !chat.record.same_revision(&record) {
                                    chat.review = None;
                                    group.comparison.conversation_renamed(&chat.record, &record);
                                }
                                chat.record = record;
                                group.notice = if chat.record.sync_failed {
                                    "Conversation renamed. Could not confirm that it reached durable storage."
                                } else {
                                    "Conversation renamed."
                                }.into();
                            }
                        } else if record.is_draft() {
                            group.notice = if record.sync_failed {
                                "Draft saved. Could not confirm that it reached durable storage."
                            } else {
                                "Draft saved in Saved Chats."
                            }
                            .into();
                            if let Some(chat) = &mut group.conversation {
                                chat.profile_selection = record.profile_id.clone();
                                chat.record = record;
                                chat.review = None;
                            } else if !group.closing {
                                let prior = record.target_ids().collect::<Vec<_>>();
                                group.choices = prior
                                    .iter()
                                    .map(|&id| conversation::saved_choice(id, None))
                                    .collect();
                                group.selected = prior.iter().copied().map(Key::Saved).collect();
                                group.live_routes.clear();
                                group.connection_group = None;
                                group.queue = None;
                                group.focused = None;
                                group.comparison.clear_round();
                                group.modes = self.saved_swarm_modes(!group.profiles.is_empty());
                                group.conversation = Some(conversation::Conversation::new(
                                    root,
                                    record,
                                    group.profile.as_ref(),
                                ));
                                self.terminal_agents.swarms.insert(id, group);
                                return headless::catalog(id, Some(prior));
                            }
                        } else {
                            group.saved_round = Some(group.round);
                            group.notice = if record.sync_failed {
                                "Conversation saved. Could not confirm that it reached durable storage."
                            } else {
                                "Conversation saved. Open it in Saved Chats to continue."
                            }
                            .into();
                        }
                    }
                    Err(error) => group.notice = error.into(),
                }
            }
            Message::ReviewFollowUp if setup && !group.closing => {
                if let Err(error) = self.review_saved_swarm_follow_up(&mut group) {
                    group.notice = error;
                }
            }
            Message::StartFollowUp(epoch) if setup && !group.closing => {
                if let Err(error) = self.start_saved_swarm_follow_up(&mut group, epoch) {
                    group.notice = error;
                }
            }
            Message::FollowUpSaved(round, result) if round == group.round => {
                if let Some(chat) = &mut group.conversation
                    && chat.saving
                {
                    chat.saving = false;
                    match result.0 {
                        Ok(record) => {
                            self.record_saved_swarm(&chat.root, &record);
                            group.comparison.follow_up_saved(&record);
                            group.notice = if record.sync_failed {
                                "Follow-up saved. Could not confirm that it reached durable storage."
                            } else if chat.record.is_draft() {
                                "First Swarm run saved."
                            } else {
                                "Follow-up saved."
                            }.into();
                            chat.profile_selection = record.profile_id.clone();
                            chat.record = record;
                        }
                        Err(error) => group.notice = error.into(),
                    }
                }
            }
            Message::Comparison(message) => {
                let task = self.swarm_comparison_action(id, &mut group, message);
                self.terminal_agents.swarms.insert(id, group);
                return Task::batch([task, self.advance_swarm(id)]);
            }
            Message::Start
                if setup
                    && group.conversation.is_none()
                    && !group.selected.is_empty()
                    && !group.closing =>
            {
                group.round += 1;
                group.comparison.clear_round();
                group.journal_run = None;
                group.prepared = None;
                group.discard_preparation = false;
                if let Err(error) = self.swarm_policy_for(group.mode) {
                    group.notice = error.into();
                } else if group.mode == SwarmMode::Snippet {
                    if let Some(choice) = &group.snippet {
                        let task =
                            snippet::prepare(id, group.round, choice.id, group.arguments.text());
                        group.preparing = true;
                        group.notice = "Preparing the exact saved script for review…".into();
                        self.terminal_agents.swarms.insert(id, group);
                        return task;
                    }
                } else if group.profile.is_some() && !group.goal.trim().is_empty() {
                    self.start_reviewed_swarm_queue(&mut group);
                }
            }
            Message::SnippetPrepared(round, result) if group.preparing && round == group.round => {
                group.preparing = false;
                if group.discard_preparation {
                    group.notice = "Script preparation cancelled.".into();
                } else if let Err(error) = self.swarm_policy() {
                    group.notice = error.into();
                } else {
                    match result {
                        Ok(review) => {
                            group.prepared = Some(review);
                            self.start_reviewed_swarm_queue(&mut group);
                        }
                        Err(error) => group.notice = error.into(),
                    }
                }
            }
            Message::Focus(key) => group.focused = Some(key),
            Message::Member(member, action)
                if group.members.contains_key(&member) && !group.closing =>
            {
                self.terminal_agents.swarms.insert(id, group);
                let message = match action {
                    MemberAction::Approve(epoch) => RunMessage::Approve(epoch),
                    MemberAction::Answer(epoch, answer) => RunMessage::Answer(epoch, answer),
                    MemberAction::ReviewModel(message) => RunMessage::ReviewModel(message),
                };
                return self.update_terminal_agent(member, message);
            }
            Message::StopTarget(ticket)
                if group
                    .queue
                    .as_ref()
                    .is_some_and(|queue| queue.current(ticket).is_some()) =>
            {
                group.restarts.cancel_target(ticket.key());
                self.cancel_swarm_target(&mut group, ticket.key());
            }
            #[cfg(test)]
            Message::Cancel(key) => {
                group.restarts.cancel_target(key);
                self.cancel_swarm_target(&mut group, key);
            }
            Message::CancelAll | Message::Close => {
                group.restarts.cancel();
                group.batch_review = None;
                group.closing |= matches!(message, Message::Close);
                if let Some(chat) = &mut group.conversation {
                    chat.review = None;
                    chat.active = None;
                }
                group.notifications.stop();
                group.workflow = None;
                group.comparison.cancel();
                if group.queue.as_ref().is_some_and(|queue| !queue.complete()) {
                    group.comparison.clear_round();
                }
                group.discard_preparation |= group.preparing;
                if let Some(queue) = &mut group.queue {
                    queue.set_paused(true);
                }
                let keys = group.queue.as_ref().map_or_else(Vec::new, |queue| {
                    queue.rows().iter().map(|row| row.target.key).collect()
                });
                for key in keys {
                    self.cancel_swarm_target(&mut group, key);
                }
            }
            Message::Pause(paused) if group.can_pause_round() => {
                group.batch_review = None;
                if let Some(queue) = &mut group.queue {
                    queue.set_paused(paused);
                }
                let members: Vec<_> = group.members.keys().copied().collect();
                self.terminal_agents.swarms.insert(id, group);
                let mut tasks = Vec::new();
                for member in members {
                    if self
                        .terminal_agents
                        .runs
                        .get(&member)
                        .is_some_and(|run| run.paused != paused)
                    {
                        tasks.push(self.update_terminal_agent(member, RunMessage::Pause));
                    }
                }
                tasks.push(self.advance_swarm(id));
                return Task::batch(tasks);
            }
            Message::PauseTarget(ticket, paused) if group.can_hold_target(ticket) => {
                group.batch_review = None;
                if group
                    .queue
                    .as_mut()
                    .is_some_and(|queue| queue.hold(ticket, paused))
                    && let Some(member) = group.member_for(ticket)
                    && self
                        .terminal_agents
                        .runs
                        .get(&member)
                        .is_some_and(|run| run.paused != paused)
                {
                    self.terminal_agents.swarms.insert(id, group);
                    return self.update_terminal_agent(member, RunMessage::Pause);
                }
            }
            Message::RestartTarget(ticket) if group.can_restart_target(ticket) => {
                self.request_swarm_target_restart(&mut group, ticket);
            }
            Message::RestartRound(round) if group.round == round && group.can_restart_round() => {
                self.request_swarm_round_restart(&mut group);
            }
            Message::CopyReport => {
                let task = match group
                    .queue
                    .as_ref()
                    .map(|queue| group.with_comparison(queue.report()))
                {
                    Some(report) => match self.copy_swarm_report(report) {
                        Ok(task) => task,
                        Err(error) => {
                            group.notice = error;
                            Task::none()
                        }
                    },
                    None => Task::none(),
                };
                self.terminal_agents.swarms.insert(id, group);
                return task;
            }
            Message::ReportPath(path) if !running && path.len() <= 4096 => group.report_path = path,
            Message::ReportFormat(format) if !running => group.report_format = format,
            Message::SaveReport if setup && !group.closing => {
                let format = group.report_format;
                let export = if let Some(chat) = &group.conversation {
                    if chat.record.is_draft() {
                        None
                    } else if self.ai_chat_enabled() {
                        Some(Ok((
                            chat.export(format),
                            Some((chat.root.clone(), chat.record.clone())),
                        )))
                    } else {
                        Some(Err(
                            "Exporting saved AI conversations is unavailable under the current settings.",
                        ))
                    }
                } else {
                    group.queue.as_ref().map(|queue| {
                        engine::report::round(queue, format, group.comparison.result())
                            .map(|report| (report, None))
                    })
                };
                if let Some(export) = export {
                    match export {
                        Ok((report, source)) => {
                            let path = PathBuf::from(group.report_path.trim());
                            let round = group.round;
                            group.report_writing = true;
                            group.notice = "Saving the swarm report…".into();
                            self.terminal_agents.swarms.insert(id, group);
                            return snippet::work(
                                "kortty-swarm-report",
                                move || {
                                    engine::report::publish(&path, format, &report, || conversation::check_export(source.as_ref())).map_err(|error| {
                                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                                        "The report file already exists. Choose a new filename."
                                    } else {
                                        "Could not save the Swarm report. Check the conversation, policy and new absolute path."
                                    }
                                })
                                },
                                move |result| {
                                    Desktop::TerminalAgent(
                                        id,
                                        RunMessage::Swarm(Message::ReportWritten(round, result)),
                                    )
                                },
                            );
                        }
                        Err(error) => group.notice = error.into(),
                    }
                }
            }
            Message::ReportWritten(round, result)
                if group.report_writing && round == group.round =>
            {
                group.report_writing = false;
                group.notice = result.err().unwrap_or("Swarm report saved.").into();
            }
            Message::ScheduleDraft if setup && !group.closing && group.can_schedule() => match self
                .swarm_scheduler_draft(&group)
                .and_then(|(owner, draft)| self.show_scheduler_swarm_handoff(owner, draft))
            {
                Ok(task) => {
                    self.terminal_agents.swarms.insert(id, group);
                    return task;
                }
                Err(error) => group.notice = error,
            },
            Message::ShowWorkflow
                if setup
                    && group.conversation.is_none()
                    && group.profile.is_some()
                    && !group.goal.trim().is_empty() =>
            {
                match self.swarm_ai_policy(Mode::Ask) {
                    Ok(()) => {
                        group.workflow_generation += 1;
                        group.workflow = Some(workflow::Form::new(group.workflow_generation));
                        group.notice.clear();
                    }
                    Err(error) => group.notice = error.into(),
                }
            }
            Message::Workflow(generation, message) => {
                let task = if !group.closing && generation == group.workflow_generation {
                    self.swarm_workflow_action(id, &mut group, message)
                } else {
                    Task::none()
                };
                self.terminal_agents.swarms.insert(id, group);
                return task;
            }
            Message::CloseWorkflow => {
                group.workflow = None;
                group.notice.clear();
            }
            Message::JournalFinished(result) => {
                group.journals_pending = group.journals_pending.saturating_sub(1);
                match result {
                    Ok(Some(artifact)) => {
                        if artifact.complete
                            && group
                                .journal_run
                                .as_ref()
                                .is_some_and(|(_, run)| run.options().keep(artifact.outcome))
                        {
                            group.journal.links.push(artifact.source.clone());
                        }
                        let _ = self.automation_journals_completed(vec![artifact]);
                    }
                    Ok(None) => {}
                    Err(error) => group.notice = error,
                }
            }
            _ => {}
        }
        self.terminal_agents.swarms.insert(id, group);
        self.advance_swarm(id)
    }

    fn swarm_scheduler_draft(&self, group: &Group) -> Result<(window::Id, SwarmHandoff), String> {
        use crate::terminal_logging::{Redactor, Secrets};

        let owner = group.owner.ok_or("The Swarm owner is unavailable.")?;
        let installed = self.swarm_policy_for(group.mode)?;
        let local = &self.clipboard.policy;
        if !local
            .terminal_logging
            .equivalent_redactions(&installed.terminal_logging)
        {
            return Err("Managed masking rules changed. Reopen Swarm before scheduling.".into());
        }
        let config = self
            .ai_configuration()
            .ok_or("AI features are unavailable.")?;
        let profile = group
            .profile
            .as_ref()
            .ok_or("Choose a current AI profile.")?;
        let (transport, _) = profile.transport(&config)?;
        let goal = group.schedule_prompt().ok_or("Enter a Swarm task.")?;
        let title = group
            .conversation
            .as_ref()
            .map_or("AI Swarm", |chat| chat.record.title());
        let name: String = title
            .trim()
            .chars()
            .take(kortty_diagram::MAX_LABEL_CHARS)
            .collect();
        let name = name.trim();
        let credential = transport.completion_secret();
        let redactor = Redactor::for_journal(
            Secrets::default()
                .seal()
                .map_err(|error| error.to_string())?,
            local.terminal_logging.clone(),
        );
        let clean = |value: &str| -> Result<bool, String> {
            let masked = super::request_review::mask_https_text(
                value,
                credential.as_deref().map(String::as_str),
                self.vault.as_ref(),
            )?;
            Ok(mask::prompt(&redactor, &masked)? == value)
        };
        if !clean(goal)? || !clean(name)? {
            return Err(
                "Use a task and title without private or redacted text before scheduling.".into(),
            );
        }
        let mode = if group.mode == SwarmMode::Advisory {
            Mode::Ask
        } else {
            Mode::Run
        };
        let mut targets = Vec::new();
        let mut ids = HashSet::new();
        for choice in group
            .choices
            .iter()
            .filter(|choice| group.selected.contains(&choice.target.key))
        {
            let Some(source) = group.catalog_source(choice) else {
                if matches!(choice.target.key, Key::Saved(_)) {
                    return Err(
                        "A selected saved connection is unavailable. Refresh the Swarm targets."
                            .into(),
                    );
                }
                continue;
            };
            if choice.target.private {
                continue;
            }
            self.saved_swarm_connection(source, mode, true)?;
            if let Some((session, stream)) = choice.target.key.pane() {
                let (_, pane, _) = self
                    .agent_pane(session)
                    .filter(|(_, pane, tab)| pane.stream_generation == stream && !tab.incognito)
                    .filter(|(_, pane, _)| {
                        pane.ssh.as_ref().is_some_and(|ssh| {
                            ssh.saved_id() == Some(source.reference.id)
                                && ssh.matches_endpoints(
                                    &source.reference.target,
                                    source.reference.jump.as_ref(),
                                )
                        })
                    })
                    .ok_or("A selected SSH pane changed. Refresh the Swarm targets.")?;
                let redactor = self.agent_source_redactor(pane)?;
                if mask::prompt(&redactor, goal)? != goal || mask::prompt(&redactor, name)? != name
                {
                    return Err(
                        "Use a task and title without private or redacted text before scheduling."
                            .into(),
                    );
                }
            }
            if !ids.insert(source.reference.id) {
                return Err(
                    "Choose each saved SSH connection only once for the Scheduler draft.".into(),
                );
            }
            targets.push(source.reference.clone());
        }
        let journal = self
            .automation_journal_capture_allowed()
            .then(|| group.journal.draft.build())
            .transpose()?;
        Ok((
            owner,
            SwarmHandoff {
                name: name.to_owned(),
                goal: goal.to_owned(),
                profile: profile.reference.clone(),
                targets,
                read_only: group.mode == SwarmMode::Advisory,
                journal: journal.flatten(),
            },
        ))
    }

    fn copy_swarm_report(&self, mut report: String) -> Result<Task<Desktop>, String> {
        use crate::terminal_logging::{Redactor, Secrets};

        let live = &self.clipboard.policy;
        let fresh = ClipboardPolicy::load();
        if live.problem.is_some()
            || fresh.problem.is_some()
            || !live.write_allowed
            || !fresh.write_allowed
            || (fresh.internal && !live.internal)
        {
            return Err("Managed policy blocks copying the swarm report.".into());
        }
        let secrets = Secrets::default()
            .seal()
            .map_err(|error| error.to_string())?;
        let current = &live.terminal_logging;
        if !current.equivalent_redactions(&fresh.terminal_logging) {
            let fresh = Redactor::for_journal(secrets.clone(), fresh.terminal_logging);
            report = mask::prompt(&fresh, &report)?;
        }
        let redactor = Redactor::for_journal(secrets, current.clone());
        Ok(self.write_clipboard(mask::prompt(&redactor, &report)?))
    }

    fn swarm_conversation_save(
        &self,
        group: &Group,
        save: &SaveName,
    ) -> Result<(PathBuf, ConversationSave), String> {
        let root = crate::profile::root().map_err(|error| error.to_string())?;
        let title = save.title.trim();
        let snapshot = match save.kind {
            SaveKind::Draft if group.can_save_draft() => ConversationSave::Draft(
                Box::new(self.swarm_draft_snapshot(group, &root, title)?),
                title.to_owned(),
            ),
            SaveKind::Rename => ConversationSave::Rename(
                Box::new(self.swarm_rename_snapshot(group, &root, title)?),
                title.to_owned(),
            ),
            SaveKind::Conversation
                if group.conversation.is_none() && group.saved_round != Some(group.round) =>
            {
                let queue = group
                    .queue
                    .as_ref()
                    .ok_or("Finish an AI Swarm round first.")?;
                let snapshot = group.comparison.initial_snapshot(self, group, queue)?;
                let title = if title.is_empty() {
                    None
                } else {
                    if mask::prompt(&snapshot.redactor, title)? != title
                        || request_review::mask_https_text(title, None, self.vault.as_ref())?
                            != title
                    {
                        return Err(
                            "Choose a title that contains no private or redacted text.".into()
                        );
                    }
                    Some(title.to_owned())
                };
                ConversationSave::Completed(Box::new(snapshot), title)
            }
            _ => return Err("The Swarm selection changed. Open the save form again.".into()),
        };
        Ok((root, snapshot))
    }

    fn cancel_swarm_target(&mut self, group: &mut Group, key: Key) {
        if let Some(ticket) = group.queue.as_mut().and_then(|queue| queue.cancel(key))
            && let Some((&id, _)) = group
                .members
                .iter()
                .find(|(_, candidate)| **candidate == ticket)
        {
            self.terminal_agents.request_cancel(id);
        }
    }

    pub(in crate::workspace) fn swarm_windows_are_open(&self) -> bool {
        !self.terminal_agents.swarms.is_empty()
    }

    pub(in crate::workspace) fn revalidate_swarm_automation_journals(&mut self) {
        let allowed = self.automation_journal_capture_allowed();
        for group in self.terminal_agents.swarms.values_mut() {
            group.set_journal_allowed(allowed);
        }
    }

    fn swarm_journal_capture_allowed(&mut self) -> bool {
        self.refresh_automation_journal_capture_policy();
        self.automation_journal_capture_allowed()
    }

    pub(in crate::workspace) fn swarm_journals_are_writing(&self) -> bool {
        self.terminal_agents.swarms.values().any(|group| {
            group.journals_pending != 0
                || (group.journal_run.is_some()
                    && group.queue.as_ref().is_some_and(|queue| !queue.complete()))
        })
    }

    fn start_reviewed_swarm_queue(&mut self, group: &mut Group) {
        use crate::session_journal::automation::Kind;
        if !group.start_queue() {
            return;
        }
        group.remember_round();
        self.begin_swarm_comparison(group);
        group.set_journal_allowed(self.swarm_journal_capture_allowed());
        group.journal.links.clear();
        if group.journal_allowed {
            let options = match group.journal.draft.build() {
                Ok(Some(options)) => options,
                Ok(None) => return,
                Err(error) => {
                    group.notice = format!("Could not start swarm journals ({error}).");
                    return;
                }
            };
            let action = if group.mode == SwarmMode::Snippet {
                "SNIPPET_SCRIPT"
            } else {
                "AI_SWARM"
            };
            match self.automation_capture(
                Kind::Swarm,
                journal::SOURCE.into(),
                "AI Swarm".into(),
                action,
                jiff::Timestamp::now(),
                options,
            ) {
                Ok(run) => group.journal_run = Some(run),
                Err(error) => group.notice = format!("Could not start swarm journals ({error})."),
            }
        }
    }

    pub(super) fn advance_swarm(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(mut group) = self.terminal_agents.swarms.remove(&id) else {
            return Task::none();
        };
        group.invalidate_batch(&self.terminal_agents);
        if group.journal_run.is_some() && !self.automation_journal_capture_allowed() {
            group.set_journal_allowed(false);
        }
        if group.workflow.is_some()
            && let Err(error) = self.swarm_ai_policy(Mode::Ask)
        {
            group.workflow = None;
            group.notice = error.into();
        }
        let mut tasks = Vec::new();
        if let Some(queue) = &mut group.queue {
            group.members.retain(|&id_member, ticket| {
                let Some(run) = self.terminal_agents.runs.get(&id_member) else {
                    queue.change(
                        *ticket,
                        Status::Blocked,
                        "The target operation is unavailable.",
                        "",
                    );
                    return false;
                };
                let status = member_status(run);
                let transcript = run
                    .activity
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n\n");
                queue.change(*ticket, status, &run.notice, &transcript);
                if status.finished() {
                    if !group.restarts.round_pending()
                        && !group.restarts.target_pending(ticket.key())
                    {
                        group.comparison.record(self, ticket.key(), run);
                        if let Some(active) = group
                            .conversation
                            .as_mut()
                            .and_then(|chat| chat.active.as_mut())
                            && let Some(conclusion) = &run.swarm_conclusion
                        {
                            active.conclusion(ticket.key(), conclusion);
                        }
                    }
                    self.terminal_agents.runs.remove(&id_member);
                    if let Some((settings, run)) = &group.journal_run
                        && let Some(row) = queue
                            .rows()
                            .iter()
                            .find(|row| row.target.key == ticket.key() && !row.target.private)
                    {
                        group.journals_pending += 1;
                        tasks.push(journal_task(
                            id,
                            settings.clone(),
                            run.clone(),
                            row,
                            self.clipboard.policy.terminal_logging.clone(),
                        ));
                    }
                    return false;
                }
                true
            });
        }
        group.retry_finished_targets();
        self.restart_drained_swarm_round(&mut group);
        if let Some(save) = self.save_completed_swarm_follow_up(id, &mut group) {
            tasks.push(save);
        }
        if group.closing && !group.running() {
            tasks.push(self.close_placed_dialog(id));
            if self.should_exit_after_window_close() {
                tasks.push(self.quit());
            }
            return Task::batch(tasks);
        }
        if !group.closing
            && let Some(queue) = &mut group.queue
        {
            let launch = Launch {
                mode: group.mode,
                profile: group.profile.as_ref(),
                prepared: group.prepared.as_ref(),
                goal: &group.goal,
                continuation: group
                    .conversation
                    .as_ref()
                    .and_then(|chat| chat.active.as_ref())
                    .map(|active| active.reviewed.as_ref()),
            };
            let original = group
                .restarts
                .last(group.round)
                .filter(|_| launch.continuation.is_none());
            let original_launch = original.map(restart::Round::launch);
            loop {
                let reservations = queue.reserve();
                if reservations.is_empty() {
                    break;
                }
                for ticket in reservations {
                    let choice = match original {
                        Some(round) => round.choice(ticket.key()),
                        None => group
                            .choices
                            .iter()
                            .find(|choice| choice.target.key == ticket.key()),
                    };
                    match self.new_swarm_member(choice, original_launch.as_ref().unwrap_or(&launch))
                    {
                        Ok(mut run) => {
                            if run.binding.saved().is_some() {
                                queue.set_label(ticket, run.label.clone());
                            }
                            let member = window::Id::unique();
                            let work = self.agent_action(member, &mut run, RunMessage::Start);
                            let status = member_status(&run);
                            queue.change(ticket, status, &run.notice, "");
                            if status.finished() {
                                group.comparison.record(self, ticket.key(), &run);
                            } else {
                                self.terminal_agents.runs.insert(member, run);
                                group.members.insert(member, ticket);
                            }
                            tasks.push(work);
                        }
                        Err(error) => {
                            queue.change(ticket, Status::Blocked, &error, "");
                        }
                    }
                }
            }
        }
        if !group.running() {
            group.modes = if group.conversation.is_some() {
                self.saved_swarm_modes(!group.profiles.is_empty())
            } else {
                self.available_swarm_modes(!group.profiles.is_empty())
            };
        }
        let notices = group.take_notifications(self);
        let round = group.round;
        self.terminal_agents.swarms.insert(id, group);
        tasks.extend(
            notices
                .into_iter()
                .map(|(key, kind)| self.notify_swarm_run(id, round, key.pane(), kind)),
        );
        Task::batch(tasks)
    }

    fn new_swarm_member(
        &self,
        choice: Option<&Choice>,
        launch: &Launch<'_>,
    ) -> Result<Run, String> {
        self.swarm_policy_for(launch.mode)?;
        let choice = choice.ok_or("The selected target is unavailable.")?;
        let (mode, purpose) = match launch.mode {
            SwarmMode::Advisory => (
                Mode::Ask,
                super::Purpose::Agent(
                    launch
                        .profile
                        .cloned()
                        .ok_or("The agent profile is unavailable.")?,
                ),
            ),
            SwarmMode::Agent => (
                Mode::Run,
                super::Purpose::Agent(
                    launch
                        .profile
                        .cloned()
                        .ok_or("The agent profile is unavailable.")?,
                ),
            ),
            SwarmMode::Snippet => (
                Mode::Run,
                super::Purpose::Script(
                    launch
                        .prepared
                        .cloned()
                        .ok_or("The reviewed snippet is unavailable.")?,
                ),
            ),
        };
        let mut run = if let Some(review) = launch.continuation {
            let source = review
                .source(choice.target.key)
                .ok_or("The reviewed saved SSH target changed.")?;
            self.new_saved_member(source.clone(), mode, purpose)?
        } else if let Some(mut source) = choice.saved.clone() {
            source.directory = choice.directory.clone().unwrap_or_default();
            self.new_saved_member(source, mode, purpose)?
        } else {
            let (session, stream) = choice
                .target
                .key
                .pane()
                .ok_or("The selected terminal is unavailable.")?;
            let mut run = self.new_agent_with_purpose(session, mode, purpose)?;
            let binding = run.binding.live()?;
            if binding.stream != stream || binding.private {
                return Err("The selected terminal changed before the swarm could start.".into());
            }
            run.manual_directory.clone_from(&choice.directory);
            run
        };
        run.goal = if let Some(review) = launch.continuation {
            review.question.clone()
        } else {
            launch
                .prepared
                .map_or_else(|| launch.goal.into(), |review| review.prepared.name.clone())
        };
        run.context = format!("Selected target {}", run.label);
        Ok(run)
    }
}

fn ai_mode(mode: Mode) -> SwarmMode {
    if mode == Mode::Run {
        SwarmMode::Agent
    } else {
        SwarmMode::Advisory
    }
}

fn require_swarm_policy(policy: &ClipboardPolicy, mode: SwarmMode) -> Result<(), &'static str> {
    if policy.problem.is_some() || !policy.multi_exec_allowed {
        return Err("Managed policy blocks multi-target execution.");
    }
    if mode != SwarmMode::Snippet && (!policy.ai_swarm_allowed() || !policy.ai_agent_allowed()) {
        return Err("Managed policy blocks AI Swarm.");
    }
    if mode == SwarmMode::Agent && policy.ai_agent_execution == AiAgentExecution::ReadOnly {
        return Err("Managed policy blocks AI Swarm command execution.");
    }
    Ok(())
}

pub(super) fn edit_bounded(
    content: &mut text_editor::Content,
    action: text_editor::Action,
    limit: usize,
) {
    if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > limit)
    {
        return;
    }
    if !action.is_edit() {
        content.perform(action);
        return;
    }
    let old = content.text();
    let cursor = content.cursor();
    content.perform(action);
    if content.text().len() > limit {
        *content = text_editor::Content::with_text(&old);
        content.move_to(cursor);
    }
}

fn member_status(run: &Run) -> Status {
    match run.phase {
        Phase::Finished => Status::Succeeded,
        Phase::Failed => Status::Failed,
        Phase::Blocked => Status::Blocked,
        Phase::Cancelled => Status::Cancelled,
        _ if run.cancel.requested() => Status::Cancelling,
        _ if run.paused => Status::Paused,
        Phase::ModelReview | Phase::ProbeApproval | Phase::Approval | Phase::Question => {
            Status::Review
        }
        Phase::Setup | Phase::Directory | Phase::SavedPreflight => Status::Starting,
        _ => Status::Running,
    }
}

fn journal_task(
    id: window::Id,
    settings: crate::session_journal::automation::CaptureSettings,
    run: crate::session_journal::automation::Run,
    row: &engine::Row,
    policy: crate::terminal_logging::Policy,
) -> Task<Desktop> {
    let row = row.clone();
    Task::perform(
        async move {
            let (send, receive) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name("kortty-swarm-journal".into())
                .spawn(move || {
                    let result = if settings.stopped.load(Ordering::Acquire)
                        || !ClipboardPolicy::load().automation_journal_allowed()
                    {
                        Ok(None)
                    } else {
                        match engine::journal::publish(&settings, run, &row, policy) {
                            Ok(artifact) => Ok(Some(artifact)),
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                Ok(None)
                            }
                            Err(error) => Err(error.to_string()),
                        }
                    };
                    let _ = send.send_blocking(result);
                })
                .map_err(|_| "Could not start the swarm journal worker.")?;
            receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The swarm journal worker stopped.".into()))
        },
        move |result| {
            Desktop::TerminalAgent(id, RunMessage::Swarm(Message::JournalFinished(result)))
        },
    )
}
