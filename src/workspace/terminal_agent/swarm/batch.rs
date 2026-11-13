//! One approval for the exact ordinary commands currently awaiting review.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, row, text};
use iced::{Element, Task, window};

use crate::clipboard_policy::{AiAgentExecution, ClipboardPolicy};
use crate::ssh_connection::ExecOpener;
use crate::terminal_agent::swarm::headless::Review as SavedRoute;
use crate::terminal_agent::{Cancel, Command};
use crate::workspace::SshTarget;

use super::super::{Elevation, LiveBinding, LocalDirectory, Purpose, RunTarget, Shell, Target};
use super::{
    Desktop, Group, Key, Phase, Run, RunMessage, State, SwarmMode, Ticket, Workspace, mask,
};

const CHANGED: &str =
    "A reviewed command or destination changed. Review the current commands again.";
const NOT_READY: &str = "At least two ordinary commands must be ready to review together.";
const POLICY: &str = "Managed policy requires a separate approval for each command.";
const ELEVATION_TOOLS: &[&str] = &["sudo", "doas", "pkexec", "runas", "gsudo", "su"];
const MAX_COMMANDS_PER_TARGET: usize = 3;

#[derive(Clone)]
pub enum Message {
    Review,
    Confirm(Arc<Review>),
    Cancel(Arc<Review>),
}

pub struct Review {
    round: u64,
    entries: Vec<Entry>,
}

struct Entry {
    member: window::Id,
    ticket: Ticket,
    epoch: u64,
    commands: Vec<Step>,
    destination: Arc<Destination>,
    profile: Arc<ProfileAuthority>,
    label: String,
    route: String,
    directory: String,
    fingerprints: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
enum ExactCommand {
    Probe { purpose: String, command: String },
    Model(Command),
}

impl ExactCommand {
    fn text(&self) -> &str {
        match self {
            Self::Probe { command, .. } => command,
            Self::Model(command) => &command.command,
        }
    }

    fn purpose(&self) -> &str {
        match self {
            Self::Probe { purpose, .. } => purpose,
            Self::Model(command) => &command.purpose,
        }
    }
}

struct Step {
    exact: ExactCommand,
    display_purpose: String,
}

pub(in crate::workspace::terminal_agent) struct Pending {
    destination: Arc<Destination>,
    profile: Arc<ProfileAuthority>,
    ticket: Ticket,
    commands: VecDeque<ExactCommand>,
    phase: DispatchPhase,
    grant: Arc<AtomicBool>,
}

pub(in crate::workspace::terminal_agent) struct Dispatch {
    profile: Arc<ProfileAuthority>,
    cancel: Cancel,
    destination: Arc<Destination>,
    grant: Arc<AtomicBool>,
}

struct ProfileAuthority {
    configuration: Arc<crate::ai_manager::Configuration>,
    reference: crate::ai_manager::ProfileRef,
    revision: [u8; 32],
}

impl ProfileAuthority {
    fn capture(workspace: &Workspace, run: &Run) -> Result<Self, String> {
        let configuration = workspace.ai_configuration().ok_or(CHANGED)?;
        let reference = run.profile().ok_or(CHANGED)?.reference.clone();
        let revision = configuration
            .agent_profile_reference_revision(&reference)
            .ok_or(CHANGED)?;
        let profile = Self {
            configuration,
            reference,
            revision,
        };
        profile.check()?;
        Ok(profile)
    }

    fn check(&self) -> Result<(), String> {
        if self
            .configuration
            .reload_saved()
            .ok()
            .filter(crate::ai_manager::Configuration::enabled)
            .and_then(|config| config.agent_profile_reference_revision(&self.reference))
            == Some(self.revision)
        {
            Ok(())
        } else {
            Err(CHANGED.into())
        }
    }

    fn check_current(&self, workspace: &Workspace) -> Result<(), String> {
        if workspace.ai_configuration().is_some_and(|config| {
            config.agent_profile_reference_revision(&self.reference) == Some(self.revision)
        }) {
            self.check()
        } else {
            Err(CHANGED.into())
        }
    }
}

impl Dispatch {
    pub(in crate::workspace::terminal_agent) fn check(&self) -> Result<(), String> {
        let policy = ClipboardPolicy::load();
        if policy.ai_agent_execution != AiAgentExecution::Allow {
            return Err(POLICY.into());
        }
        if !self.grant.load(Ordering::Acquire) || self.cancel.requested() {
            return Err(CHANGED.into());
        }
        self.profile.check()?;
        match self.destination.as_ref() {
            Destination::Local { directory, .. } => directory.require_current(),
            Destination::Ssh { opener, .. } if opener.is_closed() => Err(CHANGED.into()),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DispatchPhase {
    AwaitingApproval,
    CheckingDirectory,
    Running,
}

fn require_batch_policy(workspace: &Workspace) -> Result<(), String> {
    let installed = workspace.swarm_policy_for(SwarmMode::Agent)?;
    if workspace.clipboard.policy.ai_agent_execution != AiAgentExecution::Allow
        || installed.ai_agent_execution != AiAgentExecution::Allow
    {
        return Err(POLICY.into());
    }
    Ok(())
}

impl Workspace {
    pub(in crate::workspace::terminal_agent) fn start_batch_approval(&self, run: &mut Run) -> bool {
        let Some(pending) = run.batch_commands.as_ref() else {
            return false;
        };
        if pending.phase != DispatchPhase::AwaitingApproval
            || pending
                .check(self, run, run.phase == Phase::ProbeApproval)
                .is_err()
        {
            run.batch_commands = None;
            return false;
        }
        run.batch_commands
            .as_mut()
            .expect("checked batch approval")
            .phase = DispatchPhase::CheckingDirectory;
        true
    }

    pub(in crate::workspace::terminal_agent) fn take_batch_command(
        &self,
        run: &mut Run,
        probe: bool,
    ) -> Result<Option<Dispatch>, String> {
        let Some(pending) = run.batch_commands.as_ref() else {
            return Ok(None);
        };
        if pending.phase != DispatchPhase::CheckingDirectory {
            return Err(CHANGED.into());
        }
        pending.check(self, run, probe)?;
        let authority = Dispatch {
            profile: Arc::clone(&pending.profile),
            cancel: run.cancel.clone(),
            destination: Arc::clone(&pending.destination),
            grant: Arc::clone(&pending.grant),
        };
        authority.check()?;
        let pending = run.batch_commands.as_mut().expect("checked batch command");
        pending.commands.pop_front();
        pending.phase = DispatchPhase::Running;
        Ok(Some(authority))
    }
}

impl Run {
    pub(in crate::workspace::terminal_agent) fn finish_batch_command(
        &mut self,
        success: bool,
    ) -> bool {
        let Some(pending) = self.batch_commands.as_mut() else {
            return false;
        };
        if !success || pending.phase != DispatchPhase::Running || pending.commands.is_empty() {
            self.batch_commands = None;
            return false;
        }
        pending.phase = DispatchPhase::AwaitingApproval;
        true
    }
}

impl Pending {
    fn check(&self, workspace: &Workspace, run: &Run, probe: bool) -> Result<(), String> {
        require_batch_policy(workspace)?;
        self.profile.check_current(workspace)?;
        if !run.swarm
            || run.binding.private()
            || run.paused
            || run.closing
            || run.cancel.requested()
            || !self.destination.matches(run)
        {
            return Err(CHANGED.into());
        }
        let exact = self.commands.front().ok_or(CHANGED)?;
        let sequence_matches = match exact {
            ExactCommand::Probe { command, .. } if probe => run
                .target
                .as_ref()
                .is_some_and(|target| target.probe_command() == command),
            ExactCommand::Model(_) if !probe => run
                .commands
                .iter()
                .take(self.commands.len())
                .map_while(|command| {
                    (command.sudo.is_none() && ordinary(&command.proposal))
                        .then_some(&command.proposal)
                })
                .eq(self.commands.iter().filter_map(|item| match item {
                    ExactCommand::Model(proposal) => Some(proposal),
                    _ => None,
                })),
            _ => false,
        };
        if !sequence_matches || mask::prompt(&run.redactor, exact.text())? != exact.text() {
            return Err(CHANGED.into());
        }
        self.destination.check(workspace, run, self.ticket.key())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.grant.store(false, Ordering::Release);
    }
}

enum Destination {
    Local {
        binding: LiveBinding,
        shell: Shell,
        directory: Box<LocalDirectory>,
    },
    Ssh {
        binding: LiveBinding,
        opener: Box<ExecOpener>,
        directory: String,
        target: Box<SshTarget>,
    },
    Saved(Arc<SavedRoute>),
}

impl Group {
    fn command_reviews<'a>(
        &'a self,
        state: &'a State,
    ) -> impl Iterator<Item = (window::Id, Ticket, &'a Run, &'a str, Vec<ExactCommand>)> {
        self.queue.iter().flat_map(move |queue| {
            queue.rows().iter().filter_map(move |row| {
                if self.closing || self.mode != SwarmMode::Agent || row.target.private {
                    return None;
                }
                let (&id, &ticket) = self
                    .members
                    .iter()
                    .find(|(_, ticket)| ticket.key() == row.target.key)?;
                let run = state.runs.get(&id)?;
                if run.batch_commands.is_some() {
                    return None;
                }
                let exact = commands(run);
                (!exact.is_empty()).then_some((id, ticket, run, row.target.label.as_str(), exact))
            })
        })
    }

    pub(super) fn batch_view<'a>(
        &'a self,
        state: &'a State,
        allowed: bool,
    ) -> Option<Element<'a, Message>> {
        if !allowed {
            return None;
        }
        if let Some(review) = &self.batch_review {
            return Some(review.view());
        }
        (self
            .command_reviews(state)
            .map(|(_, _, _, _, commands)| commands.len())
            .sum::<usize>()
            >= 2)
            .then(|| {
                button("Review commands together…")
                    .on_press(Message::Review)
                    .into()
            })
    }

    pub(super) fn invalidate_batch(&mut self, state: &State) {
        if self.batch_review.as_ref().is_some_and(|review| {
            self.round != review.round
                || self.closing
                || review
                    .entries
                    .iter()
                    .any(|entry| entry.current(self, state).is_none())
        }) {
            self.batch_review = None;
            self.notice = CHANGED.into();
        }
    }
}

fn commands(run: &Run) -> Vec<ExactCommand> {
    if !run.swarm
        || run.mode != super::Mode::Run
        || run.binding.private()
        || run.paused
        || run.closing
        || run.cancel.requested()
        || !matches!(&run.purpose, Purpose::Agent(_))
    {
        return Vec::new();
    }
    if run.phase == Phase::ProbeApproval {
        return run
            .reviewed_command()
            .map(|(purpose, command)| ExactCommand::Probe {
                purpose: purpose.to_owned(),
                command: command.to_owned(),
            })
            .into_iter()
            .collect();
    }
    if run.phase != Phase::Approval {
        return Vec::new();
    }
    run.commands
        .iter()
        .take(MAX_COMMANDS_PER_TARGET)
        .map_while(|command| {
            (command.sudo.is_none() && ordinary(&command.proposal))
                .then(|| ExactCommand::Model(command.proposal.clone()))
        })
        .collect()
}

fn ordinary(command: &Command) -> bool {
    // Keep explicit elevation invocations in the single-target review even when
    // the model supplied them as plain text instead of using the elevation field.
    let elevated = command
        .command
        .split(|value: char| !value.is_ascii_alphanumeric() && value != '_' && value != '-')
        .any(|word| {
            ELEVATION_TOOLS
                .iter()
                .any(|tool| word.eq_ignore_ascii_case(tool))
        });
    command.elevation == Elevation::None && !elevated
}

fn same_pane(left: LiveBinding, right: LiveBinding) -> bool {
    left.session == right.session && left.stream == right.stream && left.private == right.private
}

impl Entry {
    fn capture(
        workspace: &Workspace,
        member: window::Id,
        ticket: Ticket,
        run: &Run,
        label: &str,
        commands: Vec<ExactCommand>,
    ) -> Result<Self, String> {
        let target = run.target.as_ref().ok_or(CHANGED)?;
        let destination = match target {
            RunTarget::Attached(Target::Local { shell, directory }) => Destination::Local {
                binding: run.binding.live()?,
                shell: shell.clone(),
                directory: Box::new(directory.clone()),
            },
            RunTarget::Attached(Target::Ssh { opener, directory }) => {
                let (_, pane) = workspace.current_run_pane(run)?;
                let target = pane.ssh.as_ref().ok_or(CHANGED)?;
                Destination::Ssh {
                    binding: run.binding.live()?,
                    opener: Box::new(opener.clone()),
                    directory: directory.clone(),
                    target: Box::new(target.clone()),
                }
            }
            RunTarget::Saved(route) => Destination::Saved(Arc::clone(route)),
        };
        destination.check(workspace, run, ticket.key())?;
        let profile = ProfileAuthority::capture(workspace, run)?;
        let mask_display = |value: &str| mask::prompt(&run.redactor, value);
        if mask_display(target.directory())? != target.directory() {
            return Err("A command or directory contains text that requires masking. Review targets individually.".into());
        }
        let commands = commands.into_iter().map(|exact| {
            if mask_display(exact.text())? != exact.text() {
                return Err("A command or directory contains text that requires masking. Review targets individually.".into());
            }
            Ok(Step { display_purpose: mask_display(exact.purpose())?, exact })
        }).collect::<Result<Vec<_>, String>>()?;
        let (route, fingerprints) = destination.presentation();
        Ok(Self {
            member,
            ticket,
            epoch: run.epoch,
            commands,
            destination: Arc::new(destination),
            profile: Arc::new(profile),
            label: mask_display(label)?,
            route: mask_display(&route)?,
            directory: target.directory().into(),
            fingerprints: fingerprints
                .iter()
                .map(|value| mask_display(value))
                .collect::<Result<_, _>>()?,
        })
    }

    fn current<'a>(&self, group: &Group, state: &'a State) -> Option<&'a Run> {
        if group.members.get(&self.member) != Some(&self.ticket) {
            return None;
        }
        let run = state.runs.get(&self.member)?;
        (run.epoch == self.epoch
            && run.batch_commands.is_none()
            && commands(run)
                .iter()
                .eq(self.commands.iter().map(|step| &step.exact))
            && self.destination.matches(run))
        .then_some(run)
    }

    fn check(&self, workspace: &Workspace, group: &Group) -> Result<(), String> {
        let run = self
            .current(group, &workspace.terminal_agents)
            .ok_or(CHANGED)?;
        self.profile.check_current(workspace)?;
        if mask::prompt(&run.redactor, &self.directory)? != self.directory {
            return Err(CHANGED.into());
        }
        for step in &self.commands {
            if mask::prompt(&run.redactor, step.exact.text())? != step.exact.text() {
                return Err(CHANGED.into());
            }
        }
        self.destination.check(workspace, run, self.ticket.key())
    }

    fn pending(&self) -> Pending {
        Pending {
            destination: Arc::clone(&self.destination),
            profile: Arc::clone(&self.profile),
            ticket: self.ticket,
            commands: self
                .commands
                .iter()
                .map(|step| step.exact.clone())
                .collect(),
            phase: DispatchPhase::AwaitingApproval,
            grant: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Destination {
    fn matches(&self, run: &Run) -> bool {
        match (self, run.target.as_ref()) {
            (
                Self::Local {
                    binding,
                    shell,
                    directory,
                },
                Some(RunTarget::Attached(Target::Local {
                    shell: current_shell,
                    directory: current_directory,
                })),
            ) => {
                run.binding
                    .live()
                    .is_ok_and(|current| same_pane(*binding, current))
                    && shell == current_shell
                    && directory.as_ref() == current_directory
            }
            (
                Self::Ssh {
                    binding,
                    opener,
                    directory,
                    ..
                },
                Some(RunTarget::Attached(Target::Ssh {
                    opener: current_opener,
                    directory: current_directory,
                })),
            ) => {
                run.binding
                    .live()
                    .is_ok_and(|current| same_pane(*binding, current))
                    && !opener.is_closed()
                    && opener.same_connection(current_opener)
                    && directory == current_directory
            }
            (Self::Saved(review), Some(RunTarget::Saved(current))) => Arc::ptr_eq(review, current),
            _ => false,
        }
    }

    fn check(&self, workspace: &Workspace, run: &Run, key: Key) -> Result<(), String> {
        let policy = workspace.agent_dispatch_policy(run)?;
        policy.check_with(&workspace.clipboard.policy)?;
        policy.check()?;
        workspace.agent_transport(run)?;
        match self {
            Self::Local {
                binding,
                shell,
                directory,
            } => {
                if key.pane() != Some((binding.session, binding.stream)) {
                    return Err(CHANGED.into());
                }
                let (_, pane) = workspace.current_run_pane(run)?;
                if pane.ssh.is_some()
                    || pane
                        .local_security
                        .as_ref()
                        .and_then(|settings| kortty_pty::shell_program(settings.shell).ok())
                        .and_then(|program| super::super::flow::active_shell(program).ok())
                        .as_ref()
                        != Some(shell)
                {
                    return Err(CHANGED.into());
                }
                directory.require_current()?;
            }
            Self::Ssh {
                binding,
                opener,
                directory,
                target,
            } => {
                let (owner, pane) = workspace.current_run_pane(run)?;
                if key.pane() != Some((binding.session, binding.stream))
                    || pane.ssh.as_ref() != Some(target.as_ref())
                    || opener.is_closed()
                    || opener.verified_host_key().is_none()
                    || pane.session.exec_opener().is_none_or(|current| {
                        current.is_closed() || !opener.same_connection(&current)
                    })
                    || &workspace.agent_remote_path(owner, binding.session)? != directory
                {
                    return Err(CHANGED.into());
                }
            }
            Self::Saved(review) => {
                let source = run.binding.saved().ok_or(CHANGED)?;
                let current = workspace.saved_swarm_connection(source, run.mode, true)?;
                if key != Key::Saved(current.id) || source.directory != review.directory() {
                    return Err(CHANGED.into());
                }
                let trust = crate::ssh_connection::trust_store().map_err(|_| CHANGED)?;
                review
                    .trust_snapshot(
                        &crate::scheduler_window::ConnectionRef::from(&current),
                        &trust,
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn presentation(&self) -> (String, Vec<String>) {
        match self {
            Self::Local { .. } => ("Local terminal".into(), Vec::new()),
            Self::Ssh { opener, target, .. } => (
                target.project_route_label(),
                opener
                    .verified_host_key()
                    .map(|key| key.fingerprint())
                    .into_iter()
                    .collect(),
            ),
            Self::Saved(review) => {
                let route = review.reference();
                let label = route.jump.as_ref().map_or_else(
                    || route.target.label(),
                    |jump| format!("{} via {}", route.target.label(), jump.label()),
                );
                (
                    label,
                    review
                        .fingerprints()
                        .map(|(endpoint, fingerprint)| format!("{endpoint} · {fingerprint}"))
                        .collect(),
                )
            }
        }
    }
}

impl Review {
    fn view(self: &Arc<Self>) -> Element<'_, Message> {
        let mut view = column![text("Review commands together").size(17),
            text("This approval covers the commands shown below once. Later commands require a new approval.").size(13),
        ].spacing(8);
        for entry in &self.entries {
            view = view
                .push(text(format!("{} · {}", entry.label, entry.route)).size(15))
                .push(
                    text(&entry.directory)
                        .size(13)
                        .wrapping(text::Wrapping::WordOrGlyph),
                );
            for fingerprint in &entry.fingerprints {
                view = view.push(
                    text(fingerprint)
                        .size(12)
                        .wrapping(text::Wrapping::WordOrGlyph),
                );
            }
            for (index, step) in entry.commands.iter().enumerate() {
                view = view
                    .push(text(format!("{}. {}", index + 1, step.display_purpose)).size(13))
                    .push(
                        text(step.exact.text())
                            .font(iced::Font::MONOSPACE)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    );
            }
        }
        view.push(
            row![
                button(text(format!(
                    "Approve {} commands once",
                    self.entries
                        .iter()
                        .map(|entry| entry.commands.len())
                        .sum::<usize>()
                )))
                .on_press(Message::Confirm(Arc::clone(self))),
                button("Cancel review")
                    .style(button::secondary)
                    .on_press(Message::Cancel(Arc::clone(self))),
            ]
            .spacing(6)
            .wrap(),
        )
        .into()
    }
}

impl Workspace {
    pub(super) fn swarm_batch_action(
        &mut self,
        id: window::Id,
        mut group: Group,
        message: Message,
    ) -> Task<Desktop> {
        let members = match message {
            Message::Review if group.batch_review.is_none() => {
                let review = self.review_swarm_batch(&group);
                match review {
                    Ok(review) => {
                        group.batch_review = Some(Arc::new(review));
                        group.notice.clear();
                    }
                    Err(error) => group.notice = error,
                }
                Vec::new()
            }
            Message::Confirm(review)
                if group
                    .batch_review
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, &review)) =>
            {
                group.batch_review = None;
                match self.confirm_swarm_batch(&group, &review) {
                    Ok(members) => {
                        group.notice = format!(
                            "Approved {} reviewed commands.",
                            review
                                .entries
                                .iter()
                                .map(|entry| entry.commands.len())
                                .sum::<usize>()
                        );
                        for entry in &review.entries {
                            let run = self
                                .terminal_agents
                                .runs
                                .get_mut(&entry.member)
                                .expect("reviewed current Swarm member");
                            run.batch_commands = Some(Box::new(entry.pending()));
                        }
                        members
                    }
                    Err(error) => {
                        group.notice = error;
                        Vec::new()
                    }
                }
            }
            Message::Cancel(review)
                if group
                    .batch_review
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, &review)) =>
            {
                group.batch_review = None;
                Vec::new()
            }
            _ => Vec::new(),
        };
        self.terminal_agents.swarms.insert(id, group);
        Task::batch(members.into_iter().map(|(member, epoch)| {
            self.update_terminal_agent(member, RunMessage::ApproveBatch(epoch))
        }))
    }

    fn review_swarm_batch(&self, group: &Group) -> Result<Review, String> {
        require_batch_policy(self)?;
        let entries = group
            .command_reviews(&self.terminal_agents)
            .map(|(member, ticket, run, label, exact)| {
                Entry::capture(self, member, ticket, run, label, exact)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if entries
            .iter()
            .map(|entry| entry.commands.len())
            .sum::<usize>()
            < 2
        {
            return Err(NOT_READY.into());
        }
        Ok(Review {
            round: group.round,
            entries,
        })
    }

    fn confirm_swarm_batch(
        &self,
        group: &Group,
        review: &Review,
    ) -> Result<Vec<(window::Id, u64)>, String> {
        require_batch_policy(self)?;
        if group.round != review.round || group.closing || group.mode != SwarmMode::Agent {
            return Err(CHANGED.into());
        }
        for entry in &review.entries {
            entry.check(self, group)?;
        }
        Ok(review
            .entries
            .iter()
            .map(|entry| (entry.member, entry.epoch))
            .collect())
    }
}
