//! Modeless terminal-agent runs bound to the original live stream.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

use iced::{Element, Task, window};
use serde_json::json;

use crate::ai_manager::{ProfileId, ProfileRef};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai;
use crate::session::{SessionId, Status as SessionStatus};
use crate::session_security::IsolationLevel;
use crate::terminal_agent::design::Layout as AgentLayout;
#[cfg(test)]
use crate::terminal_agent::design::Preset as AgentDesign;
use crate::terminal_agent::{
    self as engine, Action, Cancel, Command, Decision, Elevation, LocalDirectory, Mode, Outcome,
    Shell, Target,
};
use crate::terminal_logging::{Redactor, Secrets};

use super::terminal_ai::mask;
use super::{Message as Desktop, PaneSession, Tab, Workspace, dialog_placement};

use self::binding::{ApprovedExecution, Binding, CommandToRun, Live as LiveBinding, RunTarget};

mod activity_export;
mod binding;
mod cursor;
pub(super) mod design;
mod final_evidence;
mod flow;
mod handoff;
mod history;
mod inline_history;
pub(crate) mod knowledge_review;
pub(super) mod palette;
mod password;
mod planning;
mod policy;
mod request_review;
pub(super) mod shortcut;
mod swarm;
mod view;
mod workflow;

const MAX_RUNS: usize = 8;
const MAX_GOAL: usize = 8192;
const MAX_ACTIVITY: usize = 32;

#[derive(Clone, Eq, PartialEq)]
pub struct ProfileChoice {
    id: ProfileId,
    reference: ProfileRef,
    label: String,
    reviewed_https: Option<String>,
    reviewed_cli: Option<String>,
}

impl fmt::Display for ProfileChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

impl ProfileChoice {
    fn is_reviewed_outbound(&self) -> bool {
        self.reviewed_https.is_some() || self.reviewed_cli.is_some()
    }

    fn transport(
        &self,
        config: &crate::ai_manager::Configuration,
    ) -> Result<(local_ai::Profile, usize), String> {
        if config.resolve_profile_reference(&self.reference) != Some(self.id) {
            return Err("The agent profile changed. Start a new review.".into());
        }
        if let Some(destination) = &self.reviewed_cli {
            config.reviewed_agent_cli_transport(&self.reference, destination)
        } else if let Some(destination) = &self.reviewed_https {
            config.reviewed_agent_https_transport(&self.reference, destination)
        } else {
            config.agent_transport_for_reference(&self.reference)
        }
    }
}

#[derive(Clone)]
pub enum Message {
    Goal(String),
    DirectoryChoice(String),
    Profile(ProfileChoice),
    Start,
    StartFromShortcut,
    ReviewModel(request_review::Message),
    ConfirmDirectory(u64),
    Approve(u64),
    ApproveBatch(u64),
    Answer(u64, String),
    Implement(u64),
    Planning(planning::Message),
    History(history::Message),
    ActivityExport(activity_export::Message),
    Design(design::Message),
    ChatFollowUp(u64, String),
    ContinueInChat(u64),
    DismissChatHandoff(u64),
    ChatHandoffLoaded(Arc<handoff::Loaded>),
    InlineDetails,
    Pip(design::pip::Message),
    PipOwned(window::Id, Box<Message>),
    Password(password::Message),
    Swarm(swarm::Message),
    Workflow(workflow::Message),
    Pause,
    Cancel,
    Copy,
    Focus,
    Close,
    Directory(u64, Result<LocalDirectory, String>),
    SavedReady(
        u64,
        Result<Arc<engine::swarm::headless::Review>, &'static str>,
    ),
    ScriptRevision(u64, Result<(), &'static str>),
    Progress(u64, engine::usage::Progress),
    Decision(u64, Arc<engine::usage::Report<Decision>>),
    Command(u64, Arc<Result<Outcome, String>>),
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Terminal agent action")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Setup,
    ModelReview,
    DirectoryConfirmation,
    Directory,
    ReviewedDirectory { probe: bool },
    SavedPreflight,
    ProbeApproval,
    Probe,
    Thinking,
    FinalAdvice,
    Approval,
    Question,
    Running,
    Elevated,
    SudoChallenge,
    SudoPassword,
    SudoRunning,
    ScriptValidation,
    Paused,
    PlanReady,
    PlanQuestions,
    PlanOptions,
    PlanLibrary,
    PlanStorage,
    Finished,
    Failed,
    Blocked,
    Cancelled,
}

impl Phase {
    fn working(self) -> bool {
        matches!(
            self,
            Self::Directory
                | Self::ReviewedDirectory { .. }
                | Self::SavedPreflight
                | Self::Probe
                | Self::Thinking
                | Self::FinalAdvice
                | Self::Running
                | Self::Elevated
                | Self::SudoChallenge
                | Self::SudoPassword
                | Self::SudoRunning
                | Self::ScriptValidation
                | Self::PlanStorage
        )
    }

    fn deciding(self) -> bool {
        matches!(self, Self::Thinking | Self::FinalAdvice)
    }

    fn finished(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Blocked | Self::Cancelled
        )
    }

    fn notice(self, before: Self) -> Option<super::terminal_notifications::AiRunNotice> {
        use super::terminal_notifications::AiRunNotice;

        if self == before {
            return None;
        }
        match self {
            Self::ProbeApproval | Self::Approval | Self::PlanOptions => Some(AiRunNotice::Approval),
            Self::SudoPassword => Some(AiRunNotice::Password),
            Self::Finished => Some(AiRunNotice::Finished),
            Self::Failed | Self::Blocked if before != Self::Setup => Some(AiRunNotice::Stopped),
            _ => None,
        }
    }
}

struct Run {
    binding: Binding,
    mode: Mode,
    phase: Phase,
    label: String,
    target: Option<RunTarget>,
    manual_directory: Option<String>,
    goal: String,
    context: String,
    model_review: Option<request_review::Review>,
    plan: Option<String>,
    planning: Option<planning::Flow>,
    history: history::State,
    activity_export: activity_export::Export,
    question: Option<(String, Vec<String>)>,
    profiles: Vec<ProfileChoice>,
    profile_pending: bool,
    purpose: Purpose,
    swarm: bool,
    design: AgentLayout,
    presentation: design::Presentation,
    started_order: u64,
    chat_handoff_id: Option<String>,
    chat_follow_up: String,
    chat_handoff_dismissed: bool,
    chat_handoff_pending: bool,
    chat_handoff_enabled: bool,
    chat_handoff_notice: Option<String>,
    chat_handoff_elapsed_ms: Option<u64>,
    chat_journal: Option<super::journal_agent_chat::AgentJournal>,
    inline: Option<inline_history::History>,
    transport: Option<(local_ai::Profile, usize)>,
    redactor: Redactor,
    learned_sudo: Option<crate::terminal_logging::RedactionSecrets>,
    notice: String,
    activity: VecDeque<String>,
    swarm_conclusion: Option<String>,
    final_evidence: final_evidence::Evidence,
    commands: VecDeque<ReviewedCommand>,
    batch_commands: Option<Box<swarm::batch::Pending>>,
    epoch: u64,
    turns: usize,
    questions: usize,
    clarification_answers: Vec<String>,
    paused: bool,
    closing: bool,
    cancel: Cancel,
    ai_cancel: Option<local_ai::Cancel>,
    password: Option<password::Pending>,
}

struct ReviewedCommand {
    proposal: Command,
    sudo: Option<Sudo>,
}

enum Sudo {
    Passwordless(engine::PasswordlessSudo),
    Password(Box<password::Review>),
}

impl ReviewedCommand {
    fn text(&self) -> &str {
        match &self.sudo {
            Some(Sudo::Passwordless(sudo)) => sudo.command(),
            Some(Sudo::Password(sudo)) => sudo.wire(),
            None => &self.proposal.command,
        }
    }

    fn into_command(self) -> Result<CommandToRun, &'static str> {
        match self.sudo {
            Some(Sudo::Passwordless(sudo)) => Ok(CommandToRun::PasswordlessSudo(sudo)),
            Some(Sudo::Password(_)) => Err("Password sudo requires its owned approval workflow."),
            None => Ok(CommandToRun::Plain(self.proposal.command)),
        }
    }
}

enum Purpose {
    Agent(ProfileChoice),
    Script(swarm::snippet::Review),
}

struct RunSetup {
    binding: Binding,
    mode: Mode,
    label: String,
    context: String,
    manual_directory: Option<String>,
    profiles: Vec<ProfileChoice>,
    purpose: Purpose,
    swarm: bool,
    design: AgentLayout,
    redactor: Redactor,
}

fn profile_choices(config: &crate::ai_manager::Configuration) -> Vec<ProfileChoice> {
    config
        .agent_profiles()
        .filter_map(|(id, name, model, _)| {
            Some(ProfileChoice {
                id,
                reference: config.profile_reference(id)?,
                label: format!("{name} · {model}"),
                reviewed_https: None,
                reviewed_cli: None,
            })
        })
        .collect()
}

fn https_profile_choices(
    config: &crate::ai_manager::Configuration,
) -> impl Iterator<Item = ProfileChoice> {
    config
        .reviewed_agent_https_profiles()
        .filter_map(|(id, name, model, destination)| {
            Some(ProfileChoice {
                id,
                reference: config.profile_reference(id)?,
                label: format!("{name} · {model} · HTTPS"),
                reviewed_https: Some(destination),
                reviewed_cli: None,
            })
        })
}

fn cli_profile_choices(
    config: &crate::ai_manager::Configuration,
) -> impl Iterator<Item = ProfileChoice> {
    config
        .reviewed_agent_cli_profiles()
        .filter_map(|(id, name, model, destination)| {
            Some(ProfileChoice {
                id,
                reference: config.profile_reference(id)?,
                label: format!("{name} · {model} · CLI"),
                reviewed_https: None,
                reviewed_cli: Some(destination),
            })
        })
}

impl Run {
    fn from_setup(setup: RunSetup) -> Self {
        Self {
            binding: setup.binding,
            mode: setup.mode,
            phase: Phase::Setup,
            label: setup.label,
            target: None,
            manual_directory: setup.manual_directory,
            goal: String::new(),
            context: setup.context,
            model_review: None,
            plan: None,
            planning: None,
            history: history::State::default(),
            activity_export: activity_export::Export::default(),
            question: None,
            profiles: setup.profiles,
            profile_pending: false,
            purpose: setup.purpose,
            swarm: setup.swarm,
            design: setup.design,
            presentation: design::Presentation::default(),
            started_order: 0,
            chat_handoff_id: None,
            chat_follow_up: String::new(),
            chat_handoff_dismissed: false,
            chat_handoff_pending: false,
            chat_handoff_enabled: false,
            chat_handoff_notice: None,
            chat_handoff_elapsed_ms: None,
            chat_journal: None,
            inline: None,
            transport: None,
            redactor: setup.redactor,
            learned_sudo: None,
            notice: String::new(),
            activity: VecDeque::new(),
            swarm_conclusion: None,
            final_evidence: final_evidence::Evidence::default(),
            commands: VecDeque::new(),
            batch_commands: None,
            epoch: 0,
            turns: 0,
            questions: 0,
            clarification_answers: Vec::new(),
            paused: false,
            closing: false,
            cancel: Cancel::default(),
            ai_cancel: None,
            password: None,
        }
    }

    fn profile(&self) -> Option<&ProfileChoice> {
        match &self.purpose {
            Purpose::Agent(profile) if !self.profile_pending => Some(profile),
            _ => None,
        }
    }

    fn passwordless_sudo_target(&self) -> Option<&Target> {
        if self.swarm || self.mode != Mode::Run {
            return None;
        }
        match self.target.as_ref()? {
            RunTarget::Attached(target) if target.supports_passwordless_sudo() => Some(target),
            _ => None,
        }
    }

    fn reviewing_sudo(&self) -> bool {
        self.phase == Phase::Approval
            && self
                .commands
                .front()
                .is_some_and(|command| command.sudo.is_some())
    }

    fn active(&self) -> bool {
        self.phase != Phase::Setup && !self.phase.finished()
    }

    fn awaiting_input(&self) -> bool {
        !self.cancel.requested()
            && matches!(
                self.phase,
                Phase::ModelReview
                    | Phase::DirectoryConfirmation
                    | Phase::ProbeApproval
                    | Phase::Approval
                    | Phase::Question
                    | Phase::SudoPassword
                    | Phase::PlanReady
                    | Phase::PlanQuestions
                    | Phase::PlanOptions
                    | Phase::PlanLibrary
            )
    }

    fn next(&mut self, phase: Phase, notice: &str) -> u64 {
        self.epoch += 1;
        self.phase = phase;
        if phase.finished() {
            self.model_review = None;
            self.chat_handoff_notice = Some(notice.into());
        }
        self.presentation.review(phase);
        self.notice = notice.into();
        self.initialize_inline();
        self.epoch
    }

    fn record(&mut self, text: String) {
        if self.activity.len() == MAX_ACTIVITY {
            self.activity.pop_front();
            self.presentation.evict();
        }
        self.activity.push_back(text);
    }

    fn stop(&mut self, notice: &str) {
        self.cancel.request();
        self.ai_cancel = None;
        self.model_review = None;
        self.commands.clear();
        self.batch_commands = None;
        self.question = None;
        self.password = None;
        if self.phase.working() {
            self.notice = "Cancelling the active agent operation…".into();
        } else {
            self.phase = Phase::Cancelled;
            self.notice = notice.into();
            self.chat_handoff_notice = Some(notice.into());
        }
    }

    fn block(&mut self, error: String) {
        self.next(Phase::Blocked, &error);
        self.commands.clear();
        self.batch_commands = None;
        self.question = None;
        self.ai_cancel = None;
        self.password = None;
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.cancel.request();
    }
}

#[derive(Default)]
pub(super) struct State {
    runs: HashMap<window::Id, Run>,
    cursor_keys: u8,
    inline_details_held: bool,
    cursor_takeover: Option<cursor::Pending>,
    pip_selected: HashMap<SessionId, (u64, window::Id)>,
    next_run_order: u64,
    swarms: HashMap<window::Id, swarm::Group>,
    workflows: HashMap<window::Id, workflow::Dialog>,
    activity: activity_export::Archive,
    chat_handoffs: VecDeque<handoff::Snapshot>,
    pending_chat_handoffs: HashMap<String, handoff::Pending>,
}

impl State {
    pub(super) fn title(&self, id: window::Id) -> Option<&'static str> {
        if self.workflows.contains_key(&id) {
            return Some("kortty · Generate Workflow Script");
        }
        if self.pip_run_for_window(id).is_some() {
            return Some("kortty · Agent history");
        }
        if self.swarms.contains_key(&id) {
            return Some("kortty · AI Swarm");
        }
        let run = self.runs.get(&id).filter(|run| !run.swarm)?;
        Some(match run.mode {
            Mode::Run => "kortty · AI Agent",
            Mode::Ask => "kortty · Ask Agent",
            Mode::Plan => "kortty · AI Planning",
        })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.swarms.is_empty() && self.workflows.is_empty()
    }

    pub(super) fn owns_session(&self, session: SessionId) -> bool {
        self.runs
            .values()
            .any(|run| run.binding.session() == Some(session) && run.active())
    }

    pub(super) fn holds_mirrored_input(&self, session: SessionId) -> bool {
        self.runs.values().any(|run| {
            run.mode == Mode::Run && run.binding.session() == Some(session) && run.active()
        })
    }

    fn owns_saved(&self, connection: u64) -> bool {
        self.runs.values().any(|run| {
            run.active()
                && run
                    .binding
                    .saved()
                    .is_some_and(|source| source.reference.id == connection)
        })
    }

    pub(super) fn needs_tick(&self) -> bool {
        self.runs.values().any(Run::active)
            || self.swarms.values().any(swarm::Group::running)
            || !self.workflows.is_empty()
    }

    pub(super) fn running_window(&self) -> Option<window::Id> {
        self.swarms
            .iter()
            .find_map(|(&id, group)| group.running().then_some(id))
            .or_else(|| {
                self.runs.iter().find_map(|(&id, run)| {
                    (!run.swarm
                        && (run.phase.working()
                            || run.history.writing()
                            || run.activity_export.writing()))
                    .then_some(id)
                })
            })
            .or_else(|| {
                self.workflows
                    .iter()
                    .find_map(|(&id, dialog)| dialog.running().then_some(id))
            })
    }

    pub(super) fn invalidate_session(&mut self, session: SessionId) {
        self.activity.remove_session(session);
        for (&id, run) in self
            .runs
            .iter_mut()
            .filter(|(_, run)| run.binding.session() == Some(session))
        {
            run.activity_export.cancel();
            for dialog in self
                .workflows
                .values_mut()
                .filter(|dialog| dialog.belongs_to(id))
            {
                dialog.invalidate();
            }
            if !run.phase.finished() {
                run.stop("The source terminal disconnected.");
            }
        }
    }

    pub(super) fn request_cancel(&mut self, id: window::Id) {
        if let Some(run) = self.runs.get_mut(&id) {
            run.activity_export.cancel();
            if !run.phase.finished() {
                run.stop("Agent run cancelled.");
            }
        }
    }

    fn window_run(&self, id: window::Id) -> Option<&Run> {
        let run = self
            .workflows
            .get(&id)
            .map(workflow::Dialog::run_id)
            .or_else(|| self.pip_run_for_window(id))
            .unwrap_or(id);
        self.runs.get(&run).or_else(|| self.swarm_focused_run(id))
    }

    fn view(
        &self,
        id: window::Id,
        palette: Option<palette::Palette>,
        batch_allowed: bool,
    ) -> Option<Element<'_, Message>> {
        use iced::widget::container;

        let view = if let Some(dialog) = self.workflows.get(&id) {
            dialog.view().map(Message::Workflow)
        } else if let Some(run_id) = self.pip_run_for_window(id) {
            return Some(design::pip::detached_view(self, run_id, palette));
        } else if let Some(view) = self.swarm_view(id, palette, batch_allowed) {
            view.map(Message::Swarm)
        } else {
            self.runs.get(&id).filter(|run| !run.swarm)?.view(palette)
        };
        Some(if let Some(palette) = palette {
            palette::panel(
                Some(palette),
                container(view)
                    .width(iced::Length::Fill)
                    .height(iced::Length::Fill),
            )
        } else {
            view
        })
    }
}

impl Workspace {
    pub(super) fn agent_window_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        self.terminal_agents.view(
            id,
            self.agent_window_palette(id),
            self.clipboard.policy.ai_agent_execution
                == crate::clipboard_policy::AiAgentExecution::Allow,
        )
    }

    fn agent_window_palette(&self, id: window::Id) -> Option<palette::Palette> {
        let theme = self
            .terminal_agents
            .window_run(id)
            .and_then(|run| run.binding.live().ok())
            .filter(|source| {
                self.sessions
                    .get(&source.session)
                    .is_some_and(|pane| pane.stream_generation == source.stream)
            })
            .map_or_else(
                || self.window_settings.themes.selected(),
                |source| self.pane_theme_id(source.session),
            );
        self.agent_panel_palette(theme)
    }

    fn agent_panel_palette(
        &self,
        theme: Option<crate::terminal_theme::Id>,
    ) -> Option<palette::Palette> {
        let id = theme?;
        let themes = &self.window_settings.themes;
        Some(palette::Palette::new(
            themes.find(id)?.scheme,
            themes.agent_panel_colors(id)?,
        ))
    }

    fn agent_pane(&self, session: SessionId) -> Option<(window::Id, &PaneSession, &Tab)> {
        let pane = self.sessions.get(&session)?;
        let (owner, tab) = self.windows.iter().find_map(|(&owner, window)| {
            let (index, _) = window.pane_location(session)?;
            Some((owner, window.tabs.get(index)?.terminal()?))
        })?;
        Some((owner, pane, tab))
    }

    pub(super) fn terminal_agent_available(&self, session: SessionId, mode: Mode) -> bool {
        self.agent_pane(session)
            .is_some_and(|(_, pane, _)| self.agent_policy(pane, mode).is_ok())
    }

    fn agent_policy(&self, pane: &PaneSession, mode: Mode) -> Result<(), &'static str> {
        if !self.ai_features_enabled() || self.clipboard.policy.problem.is_some() {
            return Err("AI is unavailable under the current settings.");
        }
        policy::agent_mode(&self.clipboard.policy, mode)?;
        Self::agent_source_available(pane)?;
        if let Some(ssh) = &pane.ssh
            && (ssh.blocked_server(self).is_some()
                || ssh.isolation < self.clipboard.policy.minimum_isolation)
        {
            return Err("Managed policy blocks the source SSH route.");
        }
        if mode != Mode::Run {
            return Ok(());
        }
        if self
            .terminal_overrides(pane)
            .is_some_and(|settings| settings.ai_agent_disabled)
        {
            return Err("Agent command execution is disabled for this connection.");
        }
        self.agent_execution_target(pane)
    }

    fn agent_source_available(pane: &PaneSession) -> Result<(), &'static str> {
        if pane.frame.status != SessionStatus::Running
            || pane.frame.password_prompt
            || (pane.ssh.is_none() && pane.local_security.is_none())
        {
            return Err("The source shell is unavailable.");
        }
        Ok(())
    }

    fn agent_execution_target(&self, pane: &PaneSession) -> Result<(), &'static str> {
        if let Some(settings) = &pane.local_security {
            if settings
                .isolation
                .max(self.clipboard.policy.minimum_isolation)
                != IsolationLevel::None
                || settings.command.is_some()
            {
                return Err(
                    "The configured local shell or isolation level does not support the agent command runner.",
                );
            }
        } else if pane.session.exec_opener().is_none() {
            return Err("The authenticated SSH command channel is unavailable.");
        }
        Ok(())
    }

    fn swarm_script_policy(&self, pane: &PaneSession) -> Result<(), &'static str> {
        self.swarm_policy()?;
        Self::agent_source_available(pane)?;
        self.agent_execution_target(pane)
    }

    fn current_agent_pane(
        &self,
        binding: LiveBinding,
        mode: Mode,
    ) -> Result<(window::Id, &PaneSession), &'static str> {
        let (owner, pane, tab) = self
            .agent_pane(binding.session)
            .ok_or("The source terminal closed.")?;
        if pane.stream_generation != binding.stream || tab.incognito != binding.private {
            return Err("The source terminal changed. Open a new agent run.");
        }
        self.agent_policy(pane, mode)?;
        Ok((owner, pane))
    }

    fn current_run_pane(&self, run: &Run) -> Result<(window::Id, &PaneSession), &'static str> {
        let binding = run.binding.live()?;
        if matches!(run.purpose, Purpose::Agent(_)) {
            if run.swarm {
                self.swarm_ai_policy(run.mode)?;
            }
            return self.current_agent_pane(binding, run.mode);
        }
        let (owner, pane, tab) = self
            .agent_pane(binding.session)
            .ok_or("The source terminal closed.")?;
        if tab.incognito || pane.stream_generation != binding.stream {
            return Err("The selected terminal changed. Start a new review.");
        }
        self.swarm_script_policy(pane)?;
        Ok((owner, pane))
    }

    fn check_run_destination(&self, run: &Run) -> Result<(), String> {
        if let Some(source) = run.binding.saved() {
            self.saved_swarm_connection(source, run.mode, run.profile().is_some())?;
        } else {
            self.current_run_pane(run)?;
        }
        self.agent_dispatch_policy(run)?.check()
    }

    pub(super) fn open_terminal_agent(
        &mut self,
        owner: window::Id,
        session: SessionId,
        mode: Mode,
    ) -> Task<Desktop> {
        let result = self
            .build_agent(session, mode, None, None, true)
            .map(|run| (run, false));
        self.open_agent_result(owner, result)
    }

    fn open_agent_result(
        &mut self,
        owner: window::Id,
        result: Result<(Run, bool), String>,
    ) -> Task<Desktop> {
        match result {
            Ok((mut run, direct)) => {
                run.chat_handoff_enabled = self.ai_chat_enabled();
                let (kind, settings) = design::window(run.design);
                let (id, opened) = self.open_placed_dialog(kind, settings);
                self.terminal_agents.runs.insert(id, run);
                let opened = opened.map(|_| Desktop::Noop);
                if direct {
                    opened.chain(self.update_terminal_agent(id, Message::StartFromShortcut))
                } else {
                    opened
                }
            }
            Err(error) => {
                self.notice(owner, error);
                Task::none()
            }
        }
    }

    fn new_agent_with_purpose(
        &self,
        session: SessionId,
        mode: Mode,
        purpose: Purpose,
    ) -> Result<Run, String> {
        self.build_agent(session, mode, Some(purpose), None, false)
    }

    fn build_agent(
        &self,
        session: SessionId,
        mode: Mode,
        purpose: Option<Purpose>,
        profile_override: Option<ProfileId>,
        include_selection: bool,
    ) -> Result<Run, String> {
        if self.terminal_agents.runs.len() >= MAX_RUNS {
            return Err("Close an agent activity window before opening another.".into());
        }
        let (_, pane, tab) = self
            .agent_pane(session)
            .ok_or("The source terminal is unavailable.")?;
        let swarm = purpose.is_some();
        let mut profile_pending = false;
        let (purpose, profiles) = match purpose {
            Some(purpose @ Purpose::Script(_)) => {
                self.swarm_script_policy(pane)?;
                (purpose, Vec::new())
            }
            requested => {
                self.agent_policy(pane, mode)?;
                if swarm {
                    self.swarm_ai_policy(mode)?;
                }
                let policy = ClipboardPolicy::load();
                policy::agent_mode(&policy, mode)?;
                if pane
                    .ssh
                    .as_ref()
                    .is_some_and(|route| !route.workflow_route_allowed(&policy))
                {
                    return Err("Managed policy blocks the source SSH route.".into());
                }
                let config = self
                    .ai_configuration()
                    .ok_or("AI is unavailable under the current settings.")?;
                let mut profiles = profile_choices(&config);
                let connection = self
                    .terminal_overrides(pane)
                    .and_then(|settings| settings.ai_profile.as_ref());
                if !tab.incognito && matches!(mode, Mode::Ask | Mode::Run) {
                    profiles.extend(https_profile_choices(&config));
                    if local_ai::cli::allowed(&policy, false)
                        && local_ai::cli::allowed(&self.clipboard.policy, false)
                    {
                        profiles.extend(cli_profile_choices(&config));
                    }
                }
                let preferred = profile_override.or_else(|| {
                    let connection_id = connection
                        .and_then(|reference| config.resolve_profile_reference(reference));
                    if connection_id.is_some() || matches!(connection, Some(ProfileRef::Policy(_)))
                    {
                        connection_id
                    } else {
                        config
                            .agent_profile_id()
                            .or_else(|| config.preferred_agent_cli_id())
                    }
                });
                let cli_fallback = preferred.is_some_and(|id| config.is_terminal_cli_profile(id));
                let preferred = preferred.and_then(|id| {
                    profiles.iter().find(|profile| {
                        profile.id == id
                            && (profile.reviewed_https.is_none() || profile_override.is_some())
                    })
                });
                let fallback_allowed = profile_override.is_none()
                    && !matches!(connection, Some(ProfileRef::Policy(_)))
                    && !config.has_named_agent_profile();
                let profile = if let Some(Purpose::Agent(profile)) = requested {
                    if !profiles.contains(&profile) {
                        return Err("The selected agent profile changed. Review it again.".into());
                    }
                    profile
                } else if let Some(profile) = preferred {
                    profile.clone()
                } else if fallback_allowed
                    && cli_fallback
                    && let Some(profile) = profiles
                        .iter()
                        .find(|profile| !profile.is_reviewed_outbound())
                {
                    profile.clone()
                } else if fallback_allowed
                    && let Some(outbound) = profiles
                        .iter()
                        .find(|profile| profile.is_reviewed_outbound())
                {
                    profile_pending = true;
                    outbound.clone()
                } else {
                    return Err("Choose an available agent profile in AI Manager.".into());
                };
                (Purpose::Agent(profile), profiles)
            }
        };
        let redactor = self.agent_source_redactor(pane)?;
        let label = pane
            .ssh
            .as_ref()
            .map_or_else(|| "Local shell".into(), |ssh| ssh.label());
        let label = mask::prompt(&redactor, &label)?;
        let context = match pane
            .selection
            .filter(|_| mode == Mode::Ask && include_selection)
        {
            Some(selection) => {
                let text =
                    zeroize::Zeroizing::new(selection.visible_joined_text(&pane.frame.screen));
                if text.len() > 24 * 1024 {
                    return Err("Select at most 24 KiB of terminal context.".into());
                }
                mask::prompt(&redactor, &text)?
            }
            None => String::new(),
        };
        let manual_directory = if cfg!(windows) {
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
        let mut run = Run::from_setup(RunSetup {
            binding: Binding::Live(LiveBinding {
                session,
                stream: pane.stream_generation,
                private: tab.incognito,
            }),
            mode,
            label,
            manual_directory,
            context,
            profiles,
            purpose,
            swarm,
            design: self.window_settings.ai_agent_designs.resolve(
                self.terminal_overrides(pane)
                    .and_then(|settings| settings.ai_agent_design.as_deref()),
                self.window_settings.ai_agent_design,
            ),
            redactor,
        });
        run.profile_pending = profile_pending;
        Ok(run)
    }

    fn agent_source_redactor(&self, pane: &PaneSession) -> Result<Redactor, String> {
        let secrets = match pane.session.journal_redaction_secrets() {
            Some(secrets) => secrets,
            None if pane.ssh.is_some() => {
                return Err("Could not verify the source terminal's credential mask.".into());
            }
            None => Secrets::default()
                .seal()
                .map_err(|error| error.to_string())?,
        };
        Ok(Redactor::for_journal(
            secrets,
            self.clipboard.policy.terminal_logging.clone(),
        ))
    }

    pub(super) fn update_terminal_agent(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        if let Message::ChatHandoffLoaded(loaded) = message {
            return self.finish_agent_chat(loaded);
        }
        if let Message::PipOwned(run_id, message) = message {
            let current = self.terminal_agents.runs.get(&run_id).is_some_and(|run| {
                !run.closing
                    && run.binding.live().ok().is_some_and(|source| {
                        self.agent_pane(source.session)
                            .is_some_and(|(_, pane, _)| pane.stream_generation == source.stream)
                    })
            });
            return if current && self.terminal_agents.pip_run_for_window(id) == Some(run_id) {
                self.update_terminal_agent(run_id, *message)
            } else {
                Task::none()
            };
        }
        if self.terminal_agents.pip_run_for_window(id).is_some() {
            return if matches!(message, Message::Close) {
                self.close_agent_pip(id).unwrap_or_else(Task::none)
            } else {
                Task::none()
            };
        }
        if let Message::Pip(message) = message {
            return self.agent_pip_action(id, message);
        }
        if let Message::Swarm(message) = message {
            return self.swarm_action(id, message);
        }
        if let Message::Workflow(message) = message {
            return self.workflow_action(id, message);
        }
        if matches!(
            message,
            Message::ChatFollowUp(..) | Message::ContinueInChat(_) | Message::DismissChatHandoff(_)
        ) {
            return self.agent_chat_action(id, message);
        }
        let group = self.terminal_agents.swarm_for_member(id);
        if matches!(message, Message::Close) {
            return self.close_terminal_agent(id).unwrap_or_else(Task::none);
        }
        if matches!(message, Message::Focus) {
            let source = self
                .terminal_agents
                .runs
                .get(&id)
                .filter(|run| !run.swarm && !run.closing)
                .and_then(|run| run.binding.live().ok());
            return if source.is_some_and(|source| {
                self.agent_pane(source.session)
                    .is_some_and(|(_, pane, _)| pane.stream_generation == source.stream)
            }) {
                window::minimize(id, false).chain(window::gain_focus(id))
            } else {
                Task::none()
            };
        }
        if matches!(message, Message::Copy) {
            let transcript = self
                .terminal_agents
                .runs
                .get(&id)
                .filter(|run| !run.swarm)
                .map(|run| {
                    run.activity
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n\n")
                });
            return transcript.map_or_else(Task::none, |text| self.write_clipboard(text));
        }
        let Some(mut run) = self.terminal_agents.runs.remove(&id) else {
            return Task::none();
        };
        if run.swarm && group.is_none() {
            return Task::none();
        }
        let before = run.phase;
        let before_epoch = run.epoch;
        let start_requested = matches!(
            message,
            Message::Start | Message::StartFromShortcut | Message::Implement(_)
        );
        let task = self.agent_action(id, &mut run, message);
        let started = start_requested && run.epoch != before_epoch && run.active();
        if started {
            self.terminal_agents.next_run_order =
                self.terminal_agents.next_run_order.saturating_add(1);
            run.started_order = self.terminal_agents.next_run_order;
            if !run.swarm && run.binding.live().is_ok() {
                run.chat_handoff_id = crate::private_files::nonce().ok();
                run.chat_follow_up.clear();
                run.chat_handoff_dismissed = false;
                run.chat_handoff_pending = false;
                run.chat_handoff_elapsed_ms = None;
                run.chat_journal = None;
            }
        }
        if !run.swarm
            && !run.closing
            && (started
                || run.awaiting_input() && (before != run.phase || before_epoch != run.epoch))
            && let Ok(source) = run.binding.live()
            && self
                .agent_pane(source.session)
                .is_some_and(|(_, pane, _)| pane.stream_generation == source.stream)
        {
            self.terminal_agents.pip_selected.remove(&source.session);
        }
        self.publish_agent_inline_history(&run);
        if run.phase.finished() && !before.finished() && !run.closing {
            self.terminal_agents.activity.capture(&mut run);
        }
        if before != run.phase
            && (run.phase.finished() || run.phase == Phase::PlanReady)
            && let Ok(source) = run.binding.live()
            && self
                .agent_pane(source.session)
                .is_some_and(|(_, pane, tab)| {
                    pane.stream_generation == source.stream && tab.incognito == source.private
                })
        {
            run.chat_handoff_elapsed_ms = run.agent_history_elapsed_ms();
            self.remember_chat_handoff(&mut run);
        }
        if run.closing
            && !run.phase.working()
            && !run.history.writing()
            && !run.activity_export.writing()
        {
            let detached = run.presentation.pip.detached.take();
            let workflow = self.close_run_workflow(id);
            let closed = Task::batch([
                self.close_placed_dialog(id),
                detached.map_or_else(Task::none, |pip| self.close_placed_dialog(pip)),
                workflow,
            ]);
            return if self.should_exit_after_window_close() {
                Task::batch([closed, self.quit()])
            } else {
                closed
            };
        }
        let notice = (!run.swarm && !run.closing && !run.binding.private())
            .then(|| {
                let target = run.binding.live().ok()?;
                Some((target.session, target.stream, run.phase.notice(before)?))
            })
            .flatten();
        self.terminal_agents.runs.insert(id, run);
        let followed = self.reconcile_agent_pip();
        let task = if followed.units() == 0 {
            task
        } else {
            Task::batch([task, followed])
        };
        if let Some(group) = group {
            Task::batch([task, self.advance_swarm(group)])
        } else if let Some((session, stream, notice)) = notice {
            let notice = if notice == super::terminal_notifications::AiRunNotice::Password {
                let epoch = self.terminal_agents.runs[&id].epoch;
                self.notify_agent_password(id, epoch, session, stream)
            } else {
                self.notify_agent_run(id, session, stream, notice)
            };
            if notice.units() == 0 {
                task
            } else {
                Task::batch([task, notice])
            }
        } else {
            task
        }
    }

    pub(super) fn close_terminal_agent(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        if let Some(close) = self.close_workflow_window(id) {
            return Some(close);
        }
        if let Some(task) = self.close_agent_pip(id) {
            return Some(task);
        }
        if self.terminal_agents.swarms.contains_key(&id) {
            return Some(self.swarm_action(id, swarm::Message::Close));
        }
        let run = self
            .terminal_agents
            .runs
            .get_mut(&id)
            .filter(|run| !run.swarm)?;
        if run.activity_export.writing() {
            run.closing = true;
            run.activity_export.cancel();
            return Some(Task::none());
        }
        if run.phase.working() || run.history.writing() {
            run.closing = true;
            run.stop("Cancelling the owned agent operation before closing…");
            self.publish_agent_inline_history(&self.terminal_agents.runs[&id]);
            return Some(Task::none());
        }
        let detached = run.presentation.pip.detached.take();
        let workflow = self.close_run_workflow(id);
        if let Some(mut run) = self.terminal_agents.runs.remove(&id) {
            if run.active() {
                run.stop("Agent run cancelled.");
            }
            self.publish_agent_inline_history(&run);
        }
        let closed = Task::batch([
            self.close_placed_dialog(id),
            detached.map_or_else(Task::none, |pip| self.close_placed_dialog(pip)),
            workflow,
        ]);
        Some(if self.should_exit_after_window_close() {
            Task::batch([closed, self.quit()])
        } else {
            closed
        })
    }

    pub(super) fn cancel_terminal_agent_activity(&mut self, id: window::Id) -> Task<Desktop> {
        if self.terminal_agents.swarms.contains_key(&id) {
            self.swarm_action(id, swarm::Message::CancelAll)
        } else if self.terminal_agents.workflows.contains_key(&id) {
            self.workflow_action(id, workflow::Message::Cancel)
        } else {
            self.terminal_agents.request_cancel(id);
            if let Some(run) = self.terminal_agents.runs.get(&id) {
                self.publish_agent_inline_history(run);
            }
            Task::none()
        }
    }

    pub(super) fn reconcile_terminal_agents(&mut self) -> Task<Desktop> {
        let pip = self.reconcile_agent_pip();
        for run in self.terminal_agents.runs.values() {
            if run.activity_export.writing() && self.agent_export_pane(run).is_err() {
                run.activity_export.cancel();
            }
        }
        let invalid: Vec<_> = self
            .terminal_agents
            .runs
            .iter()
            .filter_map(|(&id, run)| {
                if run.phase.finished() || run.cancel.requested() {
                    return None;
                }
                let available = if let Some(password) = &run.password {
                    self.revalidate_password_sudo(run, password)
                } else if let Some(source) = run.binding.saved() {
                    self.saved_swarm_policy(source, run.mode, run.profile().is_some())
                        .map_err(str::to_owned)
                } else {
                    self.current_run_pane(run)
                        .map(|_| ())
                        .map_err(str::to_owned)
                };
                available.err().map(|error| (id, error))
            })
            .collect();
        for (id, error) in invalid {
            if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
                run.stop(&error);
            }
            if let Some(run) = self.terminal_agents.runs.get(&id) {
                self.publish_agent_inline_history(run);
            }
        }
        let groups: Vec<_> = self.terminal_agents.swarms.keys().copied().collect();
        self.tick_agent_inline_history();
        let workflows = self.reconcile_agent_workflows();
        Task::batch(
            [pip, workflows]
                .into_iter()
                .chain(groups.into_iter().map(|id| self.advance_swarm(id))),
        )
    }
}

#[cfg(test)]
#[track_caller]
fn assert_only_agent_notice_task(task: Task<Desktop>, owner: window::Id) {
    use iced::futures::{StreamExt as _, executor::block_on};
    use iced_runtime::Action;
    use iced_runtime::window::Action as WindowAction;

    let units = task.units();
    assert!(units <= usize::from(cfg!(any(windows, target_os = "macos"))));
    if units == 0 {
        assert!(iced_runtime::task::into_stream(task).is_none());
        return;
    }
    let mut stream = iced_runtime::task::into_stream(task).expect("native notification task");
    assert!(matches!(
        block_on(stream.next()),
        Some(Action::Window(WindowAction::Run(id, _))) if id == owner
    ));
    assert!(block_on(stream.next()).is_none());
}
