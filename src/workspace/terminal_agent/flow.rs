use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Task, window};

use super::{request_review::Review, *};

mod final_advice;
#[cfg(test)]
pub(in crate::workspace::terminal_agent) mod tests;

const TURN_LIMIT_REACHED: &str =
    "The run reached its eight-turn limit. Review the transcript and continue manually.";

impl Workspace {
    pub(super) fn agent_action(
        &mut self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Task<Desktop> {
        if let Message::Design(message) = message {
            use design::log_table::Message as LogMessage;
            if let design::Message::LogTable(
                navigation @ (LogMessage::Navigate(..) | LogMessage::KeyboardMenu(..)),
            ) = message
            {
                return run
                    .navigate_agent_log(navigation, !self.window_settings.agent_animations());
            }
            let len = match message {
                design::Message::BrowseCards(_) | design::Message::ScrollCards { .. } => {
                    design::card_stack::run_entry_count(run)
                }
                _ => run.activity.len(),
            };
            run.presentation.update(message, len);
            return Task::none();
        }
        if matches!(message, Message::InlineDetails) {
            run.toggle_inline_details();
            return Task::none();
        }
        if let Message::SelectLogDetail(revision, action) = message {
            run.select_log_detail(revision, action);
            return Task::none();
        }
        if let Message::History(history) = message {
            return self.agent_history_action(id, run, history);
        }
        if let Message::ActivityExport(export) = message {
            return self.agent_activity_export(id, run, export);
        }
        let remember = matches!(
            message,
            Message::Start | Message::StartFromShortcut | Message::StartPlanWithoutProbe
        ) && run.phase == Phase::Setup
            && run.plan.is_none()
            && !run.swarm
            && !run.binding.private();
        let mode = run.mode;
        let result = self.agent_transition(id, run, message);
        match result {
            Ok(task) if remember && run.active() => {
                Task::batch([task, self.remember_agent_prompt(id, run, mode)])
            }
            Ok(task) => task,
            Err(error) => {
                run.block(error);
                Task::none()
            }
        }
    }

    fn agent_transition(
        &self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        let completion = matches!(message, Message::AskAttachment(epoch, _) | Message::Directory(epoch, _) | Message::Decision(epoch, _) | Message::Command(epoch, _) | Message::ScriptRevision(epoch, _) | Message::SavedReady(epoch, _) if epoch == run.epoch);
        if run.cancel.requested() && completion {
            if let Message::Decision(epoch, report) = &message
                && run.phase.deciding()
                && run.mode != Mode::Plan
            {
                if self.agent_model_current(run).is_ok() {
                    run.record_model_attempts(*epoch, &report.attempts);
                }
                run.finish_model_repair(*epoch, true);
            }
            if let Message::Command(_, result) = message
                && let Ok(outcome) = &*result
            {
                run.record_result(outcome)?;
            }
            run.next(
                Phase::Cancelled,
                "Run cancelled. Commands already started may have had effects.",
            );
            return Ok(Task::none());
        }
        match message {
            Message::DebugMessages(enabled) if run.phase == Phase::Setup => {
                run.diagnostics.debug_enabled = enabled;
            }
            Message::RuntimeMessages(enabled)
                if run.phase == Phase::Setup && run.mode != Mode::Ask =>
            {
                run.diagnostics.runtime_enabled = enabled;
            }
            Message::AutoApproveRoot(enabled) if run.phase == Phase::Setup => {
                run.auto_approve_root = enabled;
            }
            Message::ConfirmEveryCommand(enabled) if run.phase == Phase::Setup => {
                run.confirm_every_command = enabled;
            }
            Message::Goal(goal) if run.phase == Phase::Setup && goal.len() <= MAX_GOAL => {
                run.goal = goal
            }
            Message::DirectoryChoice(value)
                if value.len() <= 4096
                    && (run.phase == Phase::Setup
                        && matches!(run.mode, Mode::Run | Mode::Plan)
                        || run.phase == Phase::DirectoryConfirmation
                        || run.target.is_none()
                            && matches!(
                                run.phase,
                                Phase::PlanReady | Phase::PlanImplementationReview
                            )) =>
            {
                if let Some(directory) = &mut run.manual_directory {
                    *directory = value;
                }
            }
            Message::Profile(profile)
                if run.phase == Phase::Setup && run.profiles.contains(&profile) =>
            {
                run.purpose = Purpose::Agent(profile);
                run.profile_pending = false;
            }
            Message::Start | Message::StartFromShortcut | Message::StartPlanWithoutProbe
                if run.phase == Phase::Setup
                    && !run.profile_pending
                    && !run.history.visible()
                    && (!matches!(message, Message::StartPlanWithoutProbe)
                        || run.mode == Mode::Plan)
                    && !run.goal.trim().is_empty() =>
            {
                return self.start_agent(
                    id,
                    run,
                    matches!(message, Message::StartFromShortcut),
                    !matches!(message, Message::StartPlanWithoutProbe),
                );
            }
            Message::SkipPlanProbe(epoch)
                if run.mode == Mode::Plan
                    && epoch == run.epoch
                    && !run.paused
                    && matches!(
                        run.phase,
                        Phase::DirectoryConfirmation | Phase::ProbeApproval
                    ) =>
            {
                run.target = None;
                return self.planning_questions(id, run);
            }
            Message::ConfirmDirectory(epoch)
                if epoch == run.epoch
                    && run.phase == Phase::DirectoryConfirmation
                    && !run.paused
                    && run
                        .manual_directory
                        .as_ref()
                        .is_some_and(|path| !path.trim().is_empty()) =>
            {
                self.agent_transport(run)?;
                return self.agent_directory(id, run, Phase::Directory);
            }
            Message::Planning(message) => return self.planning_action(id, run, message),
            Message::AskAttachment(epoch, result)
                if run.phase == Phase::AskAttachment && epoch == run.epoch =>
            {
                return self.finish_ask_attachment(id, run, result);
            }
            Message::Triage(message) => return self.triage_action(id, run, message),
            Message::ReviewModel(message) => return self.model_review_action(id, run, message),
            Message::Password(message) => return self.password_sudo_action(id, run, message),
            Message::Approve(epoch)
            | Message::ApproveBatch(epoch)
            | Message::ApproveRemaining(epoch)
                if epoch == run.epoch
                    && !run.paused
                    && (run.mode != Mode::Plan || matches!(message, Message::Approve(_)))
                    && run.reviewed_password_sudo().is_none()
                    && matches!(run.phase, Phase::ProbeApproval | Phase::Approval) =>
            {
                if matches!(message, Message::ApproveRemaining(_)) {
                    let approval = self.approve_remaining_agent_commands(run)?;
                    run.revoke_command_approval();
                    run.run_approval = Some(approval);
                } else if run.run_approval.is_some() && !self.automatic_agent_command(run) {
                    run.revoke_command_approval();
                }
                if matches!(message, Message::ApproveBatch(_)) {
                    if !self.start_batch_approval(run) {
                        run.notice =
                            "The batch approval changed. Review this command again.".into();
                        return Ok(Task::none());
                    }
                } else {
                    run.batch_commands = None;
                }
                let probe = run.phase == Phase::ProbeApproval;
                match run
                    .target
                    .as_ref()
                    .ok_or("The approved target is unavailable.")?
                {
                    RunTarget::Attached(Target::Local { .. }) => {
                        return self.agent_directory(id, run, Phase::ReviewedDirectory { probe });
                    }
                    RunTarget::Attached(Target::Ssh { directory, opener }) => {
                        let (owner, pane) = self.current_run_pane(run)?;
                        if run.reviewing_sudo()
                            && (opener.is_closed()
                                || opener.verified_host_key().is_none()
                                || pane.session.exec_opener().is_none_or(|current| {
                                    current.is_closed()
                                        || current.verified_host_key() != opener.verified_host_key()
                                }))
                        {
                            return Err("The SSH connection changed after the sudo review.".into());
                        }
                        if &self.agent_remote_path(owner, run.binding.live()?.session)? != directory
                        {
                            return Err(
                                "The SSH directory changed after review. Start a new run.".into()
                            );
                        }
                    }
                    RunTarget::Saved(_) => self.check_run_destination(run)?,
                }
                return self.accept_target_review(id, run, probe);
            }
            Message::Clarification(message) => return self.clarify_agent(id, run, message),
            Message::Pause if !run.phase.finished() && !run.cancel.requested() => {
                run.auto_approve_root = false;
                let had_approval = run.revoke_command_approval();
                run.paused = !run.paused;
                if run.paused
                    && (run.batch_commands.take().is_some() || had_approval)
                    && let Phase::ReviewedDirectory { probe } = run.phase
                {
                    run.next(
                        if probe {
                            Phase::ProbeApproval
                        } else {
                            Phase::Approval
                        },
                        "Resume to review the command again.",
                    );
                }
                if !run.paused && run.phase == Phase::Paused {
                    return self.agent_turn(id, run);
                }
            }
            Message::Cancel if !run.phase.finished() => run.stop("Agent run cancelled."),
            Message::Directory(epoch, result) if epoch == run.epoch => {
                let directory = match result {
                    Ok(directory) => directory,
                    Err(error) if run.mode == Mode::Plan && run.phase == Phase::Directory => {
                        return self.plan_without_probe_after_failure(id, run, &error);
                    }
                    Err(error) => return Err(error),
                };
                let (_, pane) = self.current_run_pane(run)?;
                match run.phase {
                    Phase::Directory => {
                        let shell = match local_agent_shell(pane) {
                            Ok(shell) => shell,
                            Err(error) if run.mode == Mode::Plan => {
                                return self.plan_without_probe_after_failure(id, run, &error);
                            }
                            Err(error) => return Err(error),
                        };
                        if run.mode == Mode::Run
                            && run.plan.is_some()
                            && let Some(RunTarget::Attached(Target::Local {
                                shell: expected_shell,
                                directory: expected_directory,
                            })) = &run.target
                            && (&shell != expected_shell || &directory != expected_directory)
                        {
                            return Err(
                                "The planned local shell or directory changed. Open a new plan."
                                    .into(),
                            );
                        }
                        run.target = Some(RunTarget::Attached(Target::Local { shell, directory }));
                        run.review_initial_command()?;
                    }
                    Phase::ReviewedDirectory { probe } => {
                        let Some(RunTarget::Attached(Target::Local {
                            shell,
                            directory: before,
                        })) = &run.target
                        else {
                            return Err("The reviewed local shell is unavailable.".into());
                        };
                        if before != &directory {
                            return Err(
                                "The shell directory changed after review. Start a new run.".into(),
                            );
                        }
                        if local_agent_shell(pane).as_ref() != Ok(shell) {
                            return Err("The shell changed after review. Start a new run.".into());
                        }
                        return self.accept_target_review(id, run, probe);
                    }
                    _ => {}
                }
            }
            Message::SavedReady(epoch, result)
                if epoch == run.epoch && run.phase == Phase::SavedPreflight =>
            {
                self.check_run_destination(run)?;
                let source = run
                    .binding
                    .saved()
                    .ok_or("The saved SSH destination is unavailable.")?;
                let review = result?;
                if review.reference() != &source.reference || review.directory() != source.directory
                {
                    return Err("The saved SSH destination changed. Start a new review.".into());
                }
                run.target = Some(RunTarget::Saved(review));
                run.review_initial_command()?;
            }
            Message::Diagnostic(epoch, update)
                if epoch == run.epoch
                    && !run.cancel.requested()
                    && matches!(run.phase, Phase::Probe | Phase::Running | Phase::Elevated) =>
            {
                let dispatch = self.agent_dispatch_policy(run)?;
                dispatch.check_with(&self.clipboard.policy)?;
                dispatch.check()?;
                run.diagnostics.update_output(epoch, &update);
            }
            Message::Progress(epoch, progress) => self.agent_model_progress(run, epoch, progress),
            Message::Decision(epoch, report)
                if epoch == run.epoch && run.phase.deciding() && run.mode != Mode::Plan =>
            {
                if let Err(error) = self.agent_model_current(run) {
                    run.finish_model_repair(epoch, false);
                    return Err(error);
                }
                run.ai_cancel = None;
                run.record_model_response(epoch, &report);
                let skill_usage = run.model_review.take().and_then(Review::into_skill_usage);
                run.finish_model_repair(epoch, false);
                let review_next = if run.phase == Phase::FinalAdvice {
                    run.accept_final_advice(self, &report.result)?;
                    false
                } else {
                    run.accept_decision(self, report.result.as_ref().map_err(Clone::clone)?)?
                };
                if let Some(usage) = skill_usage {
                    usage.record();
                }
                if review_next {
                    return self.agent_turn(id, run);
                }
                if let Some(task) = self.continue_agent_commands(id, run)? {
                    return Ok(task);
                }
            }
            Message::ScriptRevision(epoch, result)
                if epoch == run.epoch && run.phase == Phase::ScriptValidation =>
            {
                result?;
                if let Some(RunTarget::Attached(Target::Ssh { directory, .. })) = &run.target {
                    let (owner, _) = self.current_run_pane(run)?;
                    if self.agent_remote_path(owner, run.binding.live()?.session)? != *directory {
                        return Err(
                            "The SSH directory changed after review. Start a new run.".into()
                        );
                    }
                }
                return self.agent_launch(id, run, false);
            }
            Message::Command(epoch, result)
                if epoch == run.epoch
                    && matches!(run.phase, Phase::Probe | Phase::Running | Phase::Elevated) =>
            {
                if run.mode == Mode::Plan && run.phase == Phase::Probe {
                    return self.finish_planning_probe(id, run, &result);
                }
                if run.phase == Phase::Probe
                    && result
                        .as_ref()
                        .as_ref()
                        .is_ok_and(|outcome| outcome.status == engine::Status::Exited(0))
                {
                    run.probe_reviewed = true;
                }
                let result = result.as_ref().as_ref().map_err(Clone::clone)?;
                run.record_result(result)?;
                if run.phase == Phase::Elevated && result.status != engine::Status::Exited(0) {
                    return Err("The passwordless sudo command did not complete successfully. Review its result.".into());
                }
                if matches!(run.purpose, Purpose::Script(_)) {
                    if result.status == engine::Status::Exited(0) {
                        run.next(Phase::Finished, "The approved script completed.");
                    } else {
                        run.next(
                            Phase::Failed,
                            "The script did not complete successfully. Review its output.",
                        );
                    }
                    return Ok(Task::none());
                }
                if !matches!(result.status, engine::Status::Exited(_))
                    || (run.phase == Phase::Probe && result.status != engine::Status::Exited(0))
                {
                    return Err("The command did not complete. Review its partial result.".into());
                }
                if run.phase == Phase::Probe && self.offer_plan_sudo_preflight(run, result)? {
                    return Ok(Task::none());
                }
                if run.commands.is_empty() {
                    if result.status != engine::Status::Exited(0) {
                        run.auto_approve_root = false;
                        run.revoke_command_approval();
                    }
                    run.batch_commands = None;
                    return self.agent_turn(id, run);
                }
                let epoch = run.next(
                    Phase::Approval,
                    "Review the next exact command and its destination.",
                );
                if result.status != engine::Status::Exited(0) {
                    run.auto_approve_root = false;
                    run.revoke_command_approval();
                }
                if let Some(task) = self.continue_agent_commands(id, run)? {
                    return Ok(task);
                }
                if run.finish_batch_command(result.status == engine::Status::Exited(0)) {
                    return self.agent_transition(id, run, Message::ApproveBatch(epoch));
                }
            }
            _ => {}
        }
        Ok(Task::none())
    }

    fn start_agent(
        &self,
        id: window::Id,
        run: &mut Run,
        direct: bool,
        plan_with_probe: bool,
    ) -> Result<Task<Desktop>, String> {
        if run.binding.saved().is_some() {
            return self.start_saved_swarm_member(id, run);
        }
        self.agent_dispatch_policy(run)?.check()?;
        let live = run.binding.live()?;
        self.terminal_agents.check_live_run_slot(live, run.swarm)?;
        self.current_run_pane(run)?;
        run.goal = mask::prompt(&run.redactor, &run.goal)?;
        self.mask_https_agent_input(run)?;
        if !run.swarm && !live.private {
            run.activity_export.start();
        }
        match run.mode {
            Mode::Ask => return self.start_ask_review(id, run),
            Mode::Plan => return self.planning_start(id, run, direct, plan_with_probe),
            Mode::Run => {}
        }
        if matches!(run.purpose, Purpose::Agent(_)) {
            run.transport = Some(self.agent_transport(run)?);
        }
        self.prepare_attached_probe(id, run, direct)
    }

    pub(super) fn prepare_attached_probe(
        &self,
        id: window::Id,
        run: &mut Run,
        direct: bool,
    ) -> Result<Task<Desktop>, String> {
        let live = run.binding.live()?;
        let (owner, pane) = self.current_run_pane(run)?;
        if pane.ssh.is_some() {
            if run.mode == Mode::Run && run.plan.is_some() && run.target.is_some() {
                self.planning_probe_target(run)?;
            } else {
                run.target = Some(RunTarget::Attached(Target::Ssh {
                    opener: pane
                        .session
                        .exec_opener()
                        .ok_or("The authenticated command channel closed.")?,
                    directory: self.agent_remote_path(owner, live.session)?,
                }));
            }
            run.review_initial_command()?;
            return Ok(Task::none());
        }
        if run.manual_directory.as_ref().is_some_and(|directory| {
            direct || run.mode == Mode::Plan && directory.trim().is_empty()
        }) {
            run.next(
                Phase::DirectoryConfirmation,
                "Confirm the working directory before reviewing the environment probe.",
            );
            return Ok(Task::none());
        }
        self.agent_directory(id, run, Phase::Directory)
    }

    pub(super) fn agent_remote_path(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> Result<String, String> {
        let context = self
            .remote_shell_file_context(session)
            .ok_or("The pane no longer shows its authenticated SSH shell.")?;
        context.or_else(|| self.tracked_remote_directory(owner, session))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|path| path.starts_with('/') && engine::valid_target_label(path))
            .ok_or_else(|| "The SSH pane has no verified current directory. Enable shell directory reporting and try again.".into())
    }

    fn agent_directory(
        &self,
        id: window::Id,
        run: &mut Run,
        phase: Phase,
    ) -> Result<Task<Desktop>, String> {
        let (_, pane) = self.current_run_pane(run)?;
        self.agent_dispatch_policy(run)?.check()?;
        let chosen = run.manual_directory.as_ref().map(PathBuf::from);
        let response = if chosen.is_none() {
            Some(
                pane.session
                    .local_shell_directory()
                    .map_err(|_| "Could not read the current directory.")?,
            )
        } else {
            None
        };
        let epoch = run.next(phase, "Checking the source shell's directory…");
        let cancel = run.cancel.clone();
        Ok(Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                std::thread::Builder::new()
                    .name("kortty-agent-directory".into())
                    .spawn(move || {
                        let deadline = Instant::now() + Duration::from_secs(5);
                        let path = if let Some(response) = response {
                            loop {
                                if cancel.requested() || Instant::now() >= deadline {
                                    break None;
                                }
                                match response.try_recv() {
                                    Ok(path) => break path.ok(),
                                    Err(async_channel::TryRecvError::Closed) => break None,
                                    Err(async_channel::TryRecvError::Empty) => {
                                        std::thread::sleep(Duration::from_millis(25))
                                    }
                                }
                            }
                        } else {
                            chosen
                        };
                        let directory = path
                            .ok_or_else(|| "Could not verify the current directory.".into())
                            .and_then(LocalDirectory::capture);
                        let _ = send.send_blocking(directory);
                    })
                    .map_err(|_| "Could not start the directory check.")?;
                receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The directory worker stopped.".into()))
            },
            move |result| Desktop::TerminalAgent(id, Message::Directory(epoch, result)),
        ))
    }

    pub(super) fn continue_agent_commands(
        &self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Option<Task<Desktop>>, String> {
        if !self.automatic_agent_command(run) {
            return Ok(None);
        }
        let message = if run.reviewed_password_sudo().is_some() {
            if run.sudo_terminal.is_none() {
                run.notice = "This command is covered by the run's root approval. Choose a sudo authentication source.".into();
                return Ok(None);
            }
            Message::Password(password::Message::Approve(
                run.epoch,
                password::Source::Ticket,
            ))
        } else {
            Message::Approve(run.epoch)
        };
        self.agent_transition(id, run, message).map(Some)
    }

    fn accept_target_review(
        &self,
        id: window::Id,
        run: &mut Run,
        probe: bool,
    ) -> Result<Task<Desktop>, String> {
        let Purpose::Script(review) = &run.purpose else {
            return self.agent_launch(id, run, probe);
        };
        let review = review.clone();
        let epoch = run.next(
            Phase::ScriptValidation,
            "Checking the reviewed snippet revision…",
        );
        Ok(swarm::snippet::verify(id, epoch, review))
    }

    fn agent_launch(
        &self,
        id: window::Id,
        run: &mut Run,
        probe: bool,
    ) -> Result<Task<Desktop>, String> {
        self.check_run_destination(run)?;
        let plan_probe = if run.mode == Mode::Plan {
            if !probe {
                return Err("Planning can run only the reviewed environment probe.".into());
            }
            self.require_plan_review_route(run)?;
            self.current_agent_pane(run.binding.live()?, Mode::Run)?;
            let policy = self.planning_probe_dispatch_policy(run)?;
            policy.check()?;
            Some((policy, self.planning_probe_target(run)?))
        } else {
            None
        };
        let implementation_probe = (probe && run.uses_plan_sudo_probe())
            .then(|| self.planning_probe_target(run))
            .transpose()?;
        if matches!(run.purpose, Purpose::Agent(_)) {
            let _ = self.agent_transport(run)?;
        }
        if run.paused {
            run.next(
                if probe {
                    Phase::ProbeApproval
                } else {
                    Phase::Approval
                },
                "Resume to review the command again.",
            );
            return Ok(Task::none());
        }
        let Some(slot) = self.terminal_agents.shell_slots.reserve(run) else {
            run.return_to_busy_shell_review(if probe {
                Phase::ProbeApproval
            } else {
                Phase::Approval
            });
            return Ok(Task::none());
        };
        let batch = match self.take_batch_command(run, probe) {
            Ok(batch) => batch,
            Err(error) => {
                run.batch_commands = None;
                run.next(
                    if probe {
                        Phase::ProbeApproval
                    } else {
                        Phase::Approval
                    },
                    &error,
                );
                return Ok(Task::none());
            }
        };
        let target = run
            .target
            .clone()
            .ok_or("The approved target is unavailable.")?;
        let execution = match &target {
            RunTarget::Attached(target) => ApprovedExecution::Attached(target.clone()),
            RunTarget::Saved(review) => {
                let source = run
                    .binding
                    .saved()
                    .ok_or("The saved SSH destination is unavailable.")?;
                let (execution, redactor) = self.saved_swarm_execution(
                    source,
                    Arc::clone(review),
                    run.mode,
                    run.profile().is_some(),
                )?;
                run.redactor = redactor;
                ApprovedExecution::Saved(Box::new(execution))
            }
        };
        run.validate_script(&target)?;
        let (command, proposal) = if let Purpose::Script(review) = &run.purpose {
            (CommandToRun::Plain(review.prepared.command.clone()), None)
        } else if probe {
            (
                CommandToRun::Plain(run.environment_probe(&target).to_owned()),
                None,
            )
        } else {
            let reviewed = run
                .commands
                .pop_front()
                .ok_or("The approved command is unavailable.")?;
            let proposal = reviewed.proposal.clone();
            (reviewed.into_command()?, Some(proposal))
        };
        if mask::prompt(&run.redactor, command.text())? != command.text() {
            return Err("The proposed command contains text that requires masking.".into());
        }
        let policy = self.agent_dispatch_policy(run)?;
        let preview = match &run.purpose {
            Purpose::Script(review) => review.prepared.preview(),
            Purpose::Agent(_) => command.text(),
        };
        let journal_dispatch = run.record_command(format!("$ {preview}"), probe);
        let step = run.inline_command(command.text());
        run.bind_activity_step(step);
        let epoch = run.next(
            if probe {
                Phase::Probe
            } else if matches!(command, CommandToRun::PasswordlessSudo(_)) {
                Phase::Elevated
            } else {
                Phase::Running
            },
            "Running the approved command…",
        );
        if let Some(proposal) = proposal {
            run.final_evidence.start(epoch, &proposal);
        }
        let (cancel, updates) =
            run.diagnostics
                .start_output(epoch, run.cancel.clone(), run.redactor.clone());
        let completion = Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                let worker = std::thread::Builder::new()
                    .name("kortty-agent-command".into())
                    .spawn(move || {
                        let result = execution.run(
                            command,
                            &cancel,
                            || {
                                policy.check()?;
                                if let Some((policy, target)) = &plan_probe {
                                    policy.check()?;
                                    target.check_worker()?;
                                }
                                if let Some(target) = &implementation_probe {
                                    target.check_worker()?;
                                }
                                if let Some(batch) = &batch {
                                    batch.check()?;
                                }
                                Ok(())
                            },
                            || {
                                if let Some(dispatch) = &journal_dispatch {
                                    dispatch.store(true, std::sync::atomic::Ordering::Release);
                                }
                            },
                        );
                        drop(slot);
                        let _ = send.send_blocking(result);
                    });
                Arc::new(match worker {
                    Ok(_) => receive
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The agent command worker stopped.".into())),
                    Err(_) => Err("The agent command worker could not start.".into()),
                })
            },
            move |result| Desktop::TerminalAgent(id, Message::Command(epoch, result)),
        );
        Ok(match updates {
            Some(updates) => Task::batch([
                completion,
                Task::run(updates, move |update| {
                    Desktop::TerminalAgent(id, Message::Diagnostic(epoch, update))
                }),
            ]),
            None => completion,
        })
    }

    pub(super) fn mask_https_agent_input(&self, run: &mut Run) -> Result<(), String> {
        if run
            .profile()
            .is_some_and(ProfileChoice::is_reviewed_outbound)
        {
            let credential = self.agent_transport(run)?.0.completion_secret();
            let credential = credential.as_deref().map(String::as_str);
            let vault = self.vault.as_ref();
            run.goal = request_review::mask_https_text(&run.goal, credential, vault)?;
            run.context = request_review::mask_https_text(&run.context, credential, vault)?;
        }
        Ok(())
    }

    pub(super) fn agent_transport(&self, run: &Run) -> Result<(local_ai::Profile, usize), String> {
        if !run
            .redactor
            .uses_redactions(&self.clipboard.policy.terminal_logging)
        {
            return Err("The managed AI redaction rules changed. Start a new run.".into());
        }
        let config = self
            .ai_configuration()
            .ok_or("AI is unavailable under the current settings.")?;
        let profile = run.profile().ok_or("The agent profile is unavailable.")?;
        if run.mode == Mode::Run && !config.agent_execution_enabled() {
            return Err("Agent command execution is disabled in AI Manager.".into());
        }
        if profile.server_default.is_some()
            && (run.binding.private() || run.binding.saved().is_some() || run.swarm)
        {
            return Err("Server-selected agent profiles require a standalone, non-private terminal session.".into());
        }
        let planning_https = run.mode == Mode::Plan
            && profile.server_default.is_some()
            && profile.reviewed_https.is_some();
        if profile.is_reviewed_outbound()
            && (run.binding.private() || (run.mode == Mode::Plan && !planning_https))
        {
            return Err(
                "Reviewed HTTPS and CLI agent profiles require a non-private Ask or Run session."
                    .into(),
            );
        }
        if profile.reviewed_cli.is_some()
            && !local_ai::cli::allowed(&ClipboardPolicy::load(), false)
        {
            return Err("Managed policy blocks the configured CLI.".into());
        }
        let (mut transport, limit) = profile.transport_for_mode(&config, run.mode)?;
        if run.binding.private() {
            transport = transport.without_usage_account();
        }
        if run
            .transport
            .as_ref()
            .is_some_and(|(old, old_limit)| old != &transport || *old_limit != limit)
        {
            return Err("The agent profile changed. Start a new run.".into());
        }
        Ok((transport, limit))
    }

    fn agent_model_progress(&self, run: &mut Run, epoch: u64, progress: engine::usage::Progress) {
        use engine::usage::{AttemptKind, Progress};

        let current = epoch == run.epoch
            && run.phase.deciding()
            && run.mode != Mode::Plan
            && !run.closing
            && !run.cancel.requested()
            && run.ai_cancel.is_some()
            && self.agent_model_current(run).is_ok();
        let (permit, notice) = match progress {
            Progress::Repairing(initial, permit) => (
                permit,
                (current && run.model_repair_started(epoch, &initial))
                    .then_some("Repairing the AI response format…"),
            ),
            Progress::FormatFallback(kind, permit) => {
                let repairing = run.presentation.model_usage.repairing(epoch);
                (
                    permit,
                    (current && repairing == (kind == AttemptKind::Repair))
                        .then_some("Retrying without the provider's JSON response format…"),
                )
            }
            Progress::WebTool(proposal) => {
                self.model_tool_progress(run, proposal, current);
                return;
            }
        };
        if let Some(notice) = notice
            && permit.try_send(()).is_ok()
        {
            run.notice = notice.into();
        }
        permit.close();
    }

    fn agent_model_current(&self, run: &Run) -> Result<(), String> {
        if let Some(review) = &run.model_review {
            self.require_model_review_authority(run, review)
        } else {
            self.check_run_destination(run)?;
            self.agent_transport(run).map(|_| ())
        }
    }

    pub(super) fn agent_turn(
        &self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Task<Desktop>, String> {
        self.check_run_destination(run)?;
        if run.paused {
            run.next(Phase::Paused, "Paused between agent steps.");
            return Ok(Task::none());
        }
        if !run.triage.checked && self.triage_mode(run).is_some() {
            run.triage.checked = true;
            let triage = self.agent_transport(run).and_then(|(_, limit)| {
                let input = run.decision_input(false, limit, None)?;
                self.prepare_model_review(
                    id,
                    run,
                    request_review::Draft::Plan {
                        stage: engine::planning::Stage::Triage,
                        input,
                        evidence: run.context.clone(),
                    },
                )
            });
            if let Ok(task) = triage {
                return Ok(task);
            }
            run.record("The complexity check could not be prepared.".into());
        }
        let draft = match (run.mode, &run.purpose) {
            (Mode::Ask, Purpose::Agent(_)) => request_review::Draft::Ask,
            (Mode::Run, Purpose::Agent(_)) => {
                let kind = if run.turns < engine::MAX_TURNS {
                    engine::RunModel::Decision
                } else if !run.final_evidence.is_empty() {
                    engine::RunModel::FinalAdvice
                } else {
                    return Err(TURN_LIMIT_REACHED.into());
                };
                request_review::Draft::Run(kind)
            }
            _ => return Err("A model decision requires an Ask or Run agent.".into()),
        };
        self.prepare_model_review(id, run, draft)
    }
}

impl Run {
    #[cfg(unix)]
    pub(super) fn local_sudo_route(&self) -> Option<engine::password::LocalRoute> {
        use engine::password::LocalRoute;

        if self.mode != Mode::Run || self.swarm {
            return None;
        }
        let route = match self.target.as_ref() {
            Some(RunTarget::Attached(Target::Local {
                shell: Shell::Posix(_),
                ..
            })) => LocalRoute::Native,
            #[cfg(target_os = "linux")]
            Some(RunTarget::Attached(Target::Local {
                shell: Shell::FlatpakHost,
                ..
            })) => LocalRoute::FlatpakHost,
            _ => return None,
        };
        route.available().then_some(route)
    }

    pub(super) fn supports_local_sudo(&self) -> bool {
        #[cfg(unix)]
        {
            self.local_sudo_route().is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub(super) fn uses_plan_sudo_probe(&self) -> bool {
        self.mode == Mode::Run
            && !self.swarm
            && self.plan.is_some()
            && (matches!(self.target, Some(RunTarget::Attached(Target::Ssh { .. })))
                || self.supports_local_sudo())
    }

    pub(super) fn environment_probe(&self, target: &RunTarget) -> &'static str {
        if (self.mode == Mode::Plan || self.uses_plan_sudo_probe())
            && let RunTarget::Attached(target) = target
        {
            return engine::planning_probe::command(target);
        }
        target.probe_command()
    }
    pub(super) fn await_model(
        &mut self,
        id: window::Id,
        pending: local_ai::Pending<engine::usage::Report<Decision>>,
        updates: async_channel::Receiver<engine::usage::Progress>,
        phase: Phase,
        notice: &str,
    ) -> Task<Desktop> {
        self.ai_cancel = Some(pending.cancel);
        let epoch = self.next(phase, notice);
        self.begin_model_decision();
        let completion = Task::perform(
            async move {
                Arc::new(match pending.reply.recv().await {
                    Ok(Ok(report)) => report,
                    Ok(Err(error)) => Err(error).into(),
                    Err(_) => Err("The AI worker stopped.".into()).into(),
                })
            },
            move |result| Desktop::TerminalAgent(id, Message::Decision(epoch, result)),
        );
        // The owner must settle each authorization before the worker can complete.
        let progress = Task::run(updates, move |progress| {
            Desktop::TerminalAgent(id, Message::Progress(epoch, progress))
        });
        progress.chain(completion)
    }

    pub(super) fn decision_input(
        &self,
        password_sudo: bool,
        limit: usize,
        web_evidence: Option<&str>,
    ) -> Result<String, String> {
        let target = self
            .target
            .as_ref()
            .map(|target| {
                Ok::<_, String>(json!({
                    "shell": self.decision_text(target.shell())?,
                    "directory": self.decision_text(target.directory())?,
                    "passwordlessSudo": self.passwordless_sudo_target().is_some(),
                    "passwordSudo": password_sudo,
                }))
            })
            .transpose()?;
        let goal = self.decision_text(&self.goal)?;
        let context = self.decision_text(&self.context)?;
        let context = if let Some(snapshot) = &self.ask_file.snapshot {
            Cow::Owned(snapshot.append_to(&context))
        } else {
            context
        };
        let plan = self
            .plan
            .as_deref()
            .map(|value| self.decision_text(value))
            .transpose()?;
        let clarifications =
            self.clarification_input(|value| self.decision_text(value).map(Cow::into_owned))?;
        let mut steps = self
            .activity
            .iter()
            .rev()
            .take(8)
            .map(|value| self.decision_text(value))
            .collect::<Result<Vec<_>, _>>()?;
        steps.reverse();
        let data = |steps: &[Cow<'_, str>]| {
            let mut request = json!({"goal":goal,"target":target,"context":context,"acceptedPlan":plan,"steps":steps,
                "turn":self.turns + 1,"turnLimit":engine::MAX_TURNS});
            if !clarifications.is_empty() {
                request["clarifications"] = json!(clarifications);
            }
            if let Some(evidence) = web_evidence {
                request["web_evidence"] = json!(evidence);
            }
            request.to_string()
        };
        let mut input = data(&steps);
        while input.chars().count() > limit && !steps.is_empty() {
            steps.remove(0);
            input = data(&steps);
        }
        if input.chars().count() > limit {
            return Err(
                "The reviewed agent input exceeds the selected profile's context limit.".into(),
            );
        }
        if self.mask_decision_fields() {
            Ok(input)
        } else {
            mask::prompt(&self.redactor, &input)
        }
    }

    fn decision_text<'a>(&self, value: &'a str) -> Result<Cow<'a, str>, String> {
        // A newly learned password may appear in older evidence. Match the
        // original bytes before JSON escaping changes quotes and backslashes.
        if self.mask_decision_fields() {
            mask::prompt(&self.redactor, value).map(Cow::Owned)
        } else {
            Ok(Cow::Borrowed(value))
        }
    }

    fn mask_decision_fields(&self) -> bool {
        self.learned_sudo.is_some()
            || self.ask_file.snapshot.is_some()
            || self
                .profile()
                .is_some_and(ProfileChoice::is_reviewed_outbound)
    }

    fn review_initial_command(&mut self) -> Result<(), String> {
        if matches!(self.purpose, Purpose::Script(_)) {
            let target = self
                .target
                .as_ref()
                .ok_or("The approved target is unavailable.")?;
            self.validate_script(target)?;
            self.next(
                Phase::Approval,
                "Review the exact saved script and its destination.",
            );
        } else {
            self.next(
                Phase::ProbeApproval,
                "Review the fixed environment probe before running it.",
            );
        }
        Ok(())
    }

    fn validate_script(&self, target: &RunTarget) -> Result<(), String> {
        if let Purpose::Script(review) = &self.purpose {
            if !target.supports_script(&review.prepared) {
                return Err(
                    "The saved script requires a different shell than this target provides.".into(),
                );
            }
            for value in std::iter::once(review.prepared.command.as_str())
                .chain(review.prepared.raw_inputs())
            {
                if mask::prompt(&self.redactor, value)? != value {
                    return Err(
                        "The saved script or an argument contains text that requires masking."
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn accept_decision(
        &mut self,
        workspace: &Workspace,
        decision: &Decision,
    ) -> Result<bool, String> {
        let outbound = self
            .profile()
            .is_some_and(ProfileChoice::is_reviewed_outbound);
        let credential = if outbound {
            self.transport
                .as_ref()
                .and_then(|(profile, _)| profile.completion_secret())
        } else {
            None
        };
        let redactor = self.redactor.clone();
        let mask_response = |value: &str| {
            let value = mask::prompt(&redactor, value)?;
            if outbound {
                request_review::mask_https_text(
                    &value,
                    credential.as_deref().map(String::as_str),
                    workspace.vault.as_ref(),
                )
            } else {
                Ok(value)
            }
        };
        let summary = mask_response(&decision.summary)?;
        let detail = mask_response(&decision.message)?;
        let step = self.inline_decision(&decision.action, &summary, &detail);
        if self.swarm && matches!(decision.action, Action::Done | Action::Blocked) {
            self.swarm_conclusion = Some(detail.clone());
        }
        self.record(format!("{summary}\n{detail}"));
        self.bind_activity_step(step);
        match &decision.action {
            Action::Commands(commands) if self.mode == Mode::Run => {
                for command in commands {
                    if mask_response(&command.command)? != command.command {
                        return Err(
                            "A proposed command contained a masked value and was blocked.".into(),
                        );
                    }
                    let sudo = match command.elevation {
                        Elevation::None => None,
                        Elevation::PasswordlessSudo => Some(Sudo::Passwordless(
                            self.passwordless_sudo_target()
                                .ok_or("Passwordless sudo requires an attached SSH agent run.")?
                                .prepare_passwordless_sudo(&command.command)?,
                        )),
                        Elevation::PasswordSudo => Some(Sudo::Password(Box::new(
                            workspace.prepare_password_sudo(self, &command.command)?,
                        ))),
                    };
                    let mut proposal = command.clone();
                    proposal.purpose = mask_response(&command.purpose)?;
                    let reviewed = ReviewedCommand { proposal, sudo };
                    if reviewed.sudo.is_some() && mask_response(reviewed.text())? != reviewed.text()
                    {
                        return Err(
                            "The effective sudo command contains text that requires masking."
                                .into(),
                        );
                    }
                    self.commands.push_back(reviewed);
                }
                self.next(
                    Phase::Approval,
                    "Review the exact command and its destination.",
                );
            }
            Action::Input { question, answers } if self.mode == Mode::Run => {
                let question = mask_response(question)?;
                if self.questions >= engine::MAX_QUESTIONS {
                    self.clarification_answers
                        .push(clarification::Answer::unanswered(question));
                    self.record(
                        "The clarification limit was reached. Review the next request.".into(),
                    );
                    return Ok(true);
                }
                let answers = answers
                    .iter()
                    .map(|answer| mask_response(answer))
                    .collect::<Result<Vec<_>, _>>()?;
                self.questions += 1;
                self.question = Some(clarification::Question::new(question, answers, detail));
                self.next(Phase::Question, "The agent needs your input.");
            }
            Action::Done => {
                self.next(Phase::Finished, "Agent run finished.");
            }
            Action::Blocked => {
                self.next(Phase::Blocked, "The agent could not complete the request.");
            }
            _ => return Err("The decision is unavailable in this agent mode.".into()),
        }
        Ok(false)
    }

    fn record_result(&mut self, outcome: &Outcome) -> Result<(), String> {
        let stdout = mask::captured_output(&self.redactor, &outcome.stdout.text())?;
        let stderr = mask::captured_output(&self.redactor, &outcome.stderr.text())?;
        let step = self.inline_result(outcome.status, &stdout, &stderr);
        let status = match outcome.status {
            engine::Status::Exited(code) => format!("Exit status {code}"),
            engine::Status::Cancelled => "Command cancelled".into(),
            engine::Status::TimedOut => "Command timed out".into(),
            engine::Status::TransportFailed => "Command output transport failed".into(),
            engine::Status::MissingExitCode => "Command exit status unavailable".into(),
        };
        let limited = if outcome.stdout.truncated() || outcome.stderr.truncated() {
            "\nOutput limit reached."
        } else {
            ""
        };
        let entry = format!(
            "{status}\nstdout\n{}\nstderr\n{}{limited}",
            bounded(&stdout),
            bounded(&stderr)
        );
        self.diagnostics.clear_output();
        self.diagnostics.record(&entry);
        self.record(entry);
        self.bind_activity_step(step);
        if matches!(self.purpose, Purpose::Agent(_)) && !self.cancel.requested() {
            self.final_evidence
                .record_output(self.epoch, self.phase, outcome, stdout, stderr);
        }
        Ok(())
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(4000).collect()
}

pub(super) fn local_agent_shell(pane: &crate::workspace::PaneSession) -> Result<Shell, String> {
    let settings = pane
        .local_security
        .as_ref()
        .ok_or("The source shell changed.")?;
    let custom = settings
        .command
        .as_deref()
        .map(|command| custom_agent_shell(settings.shell, command))
        .transpose()?;
    #[cfg(target_os = "linux")]
    if pane.session.flatpak_host_shell() {
        return Ok(Shell::FlatpakHost);
    }
    if let Some(shell) = custom {
        return Ok(shell);
    }
    let program = kortty_pty::shell_program(settings.shell)
        .map_err(|_| "The selected shell is unavailable.")?;
    active_shell(program)
}

fn custom_agent_shell(preset: kortty_pty::LocalShell, command: &[String]) -> Result<Shell, String> {
    use kortty_pty::ShellIntegrationSnippet as Snippet;

    const UNSUPPORTED: &str = "The custom program has no supported agent command mode.";
    kortty_pty::validate_shell_integration(preset, Some(command)).map_err(|_| UNSUPPORTED)?;
    match Snippet::from_program(&command[0]) {
        #[cfg(unix)]
        Some(Snippet::Bash | Snippet::Zsh | Snippet::Fish) => {
            // Run reviewed POSIX commands independently of the interactive startup argv.
            Ok(Shell::Posix(PathBuf::from("/bin/sh")))
        }
        #[cfg(windows)]
        Some(snippet @ (Snippet::PowerShell | Snippet::CommandPrompt)) => {
            let preset = if snippet == Snippet::PowerShell {
                kortty_pty::LocalShell::PowerShell
            } else {
                kortty_pty::LocalShell::CommandPrompt
            };
            let program = kortty_pty::shell_program(preset)
                .map_err(|_| "The selected shell is unavailable.")?;
            active_shell(program)
        }
        _ => Err(UNSUPPORTED.into()),
    }
}

fn active_shell(path: PathBuf) -> Result<Shell, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    #[cfg(unix)]
    if matches!(name, "sh" | "bash" | "dash" | "zsh" | "ksh" | "ash") {
        return Ok(Shell::Posix(path));
    }
    #[cfg(unix)]
    if name == "fish" {
        return Ok(Shell::Posix(PathBuf::from("/bin/sh")));
    }
    #[cfg(windows)]
    {
        if name.eq_ignore_ascii_case("powershell.exe") {
            return Ok(Shell::PowerShell(path));
        }
        if name.eq_ignore_ascii_case("cmd.exe") {
            return Ok(Shell::CommandPrompt(path));
        }
    }
    Err("The selected shell has no supported agent command mode.".into())
}
