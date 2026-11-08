//! Live tool tabs share the tab strip with terminals and keep their own state.

use std::borrow::Cow;
use std::sync::Arc;

use iced::{Element, Task, window};

use crate::profile::WorkspaceLayout;
use crate::session::SessionId;

use super::{Message, TabEntry, Workspace, window_lifecycle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    OpenPgp,
    KnownHosts,
    SshKeys,
    SessionProcesses,
    RecordingManager,
    AiManager,
    LogfileManager,
    LogfileViewer,
    Snippets,
    Scheduler,
    SessionJournalManager,
    SessionJournalViewer,
    TerminalEffects,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct HostedTab {
    pub(super) id: u64,
    pub(super) kind: Kind,
    pub(super) instance: window::Id,
}

enum CloseScope {
    Tab,
    Window(window::Id, bool),
    Batch(window::Id, u64),
    Project(
        window::Id,
        super::project_access::OpenTarget,
        Arc<WorkspaceLayout>,
        Vec<(u64, SessionId)>,
    ),
    Quit(bool),
}

#[derive(Default)]
pub(super) struct CloseState(Option<PendingClose>);

struct PendingClose {
    token: u64,
    scope: CloseScope,
    targets: Vec<CloseTarget>,
    approved: Vec<window::Id>,
    awaiting: Option<window::Id>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseTarget {
    Hosted(HostedTab),
    NativeSnippet(window::Id),
    NativeScheduler(window::Id),
    NativeLogfile(window::Id),
    NativeJournal(window::Id),
    NativeAiManager(window::Id),
}

impl CloseTarget {
    fn instance(self) -> window::Id {
        match self {
            Self::Hosted(tool) => tool.instance,
            Self::NativeSnippet(id)
            | Self::NativeScheduler(id)
            | Self::NativeLogfile(id)
            | Self::NativeJournal(id)
            | Self::NativeAiManager(id) => id,
        }
    }
}

impl Workspace {
    pub(super) fn host_tool(
        &mut self,
        owner: window::Id,
        kind: Kind,
        instance: window::Id,
    ) -> Task<Message> {
        let id = self.allocate();
        let Some(window) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        window
            .tabs
            .push(TabEntry::Tool(HostedTab { id, kind, instance }));
        self.select_tab(owner, id, false);
        window::gain_focus(owner)
    }

    pub(super) fn hosted_tool_in(&self, owner: window::Id, kind: Kind) -> Option<window::Id> {
        self.windows.get(&owner)?.tabs.iter().find_map(|tab| {
            if let TabEntry::Tool(tool) = tab
                && tool.kind == kind
            {
                Some(tool.instance)
            } else {
                None
            }
        })
    }

    fn locate_hosted_tool(&self, instance: window::Id) -> Option<(window::Id, HostedTab)> {
        self.windows.iter().find_map(|(&owner, window)| {
            window.tabs.iter().find_map(|tab| match tab {
                TabEntry::Tool(tool) if tool.instance == instance => Some((owner, *tool)),
                _ => None,
            })
        })
    }

    pub(super) fn hosted_tool_owner(&self, instance: window::Id) -> Option<window::Id> {
        self.locate_hosted_tool(instance).map(|(owner, _)| owner)
    }

    pub(super) fn hosted_tab_at(&self, owner: window::Id, id: u64) -> Option<HostedTab> {
        self.windows
            .get(&owner)?
            .tabs
            .iter()
            .find_map(|entry| match entry {
                TabEntry::Tool(tool) if tool.id == id => Some(*tool),
                _ => None,
            })
    }

    pub(super) fn hosted_tools_in(&self, owner: window::Id) -> Vec<HostedTab> {
        self.windows
            .get(&owner)
            .into_iter()
            .flat_map(|window| &window.tabs)
            .filter_map(|entry| match entry {
                TabEntry::Tool(tool) => Some(*tool),
                TabEntry::Terminal(_) => None,
            })
            .collect()
    }

    pub(super) fn hosted_tool_view(&self, tool: &HostedTab) -> Option<Element<'_, Message>> {
        match tool.kind {
            Kind::OpenPgp => self.openpgp_view(tool.instance),
            Kind::KnownHosts => self.known_hosts_view(tool.instance),
            Kind::SshKeys => self.hosted_ssh_keys_view(tool.instance),
            Kind::SessionProcesses => self.session_process_view(tool.instance),
            Kind::RecordingManager => self.recording_view(tool.instance),
            Kind::AiManager => self.ai_manager_view(tool.instance),
            Kind::LogfileManager | Kind::LogfileViewer => self.logfile_view(tool.instance),
            Kind::Snippets => self.snippet_view(tool.instance),
            Kind::Scheduler => self.scheduler_view(tool.instance),
            Kind::TerminalEffects => self.terminal_effect_dialog_view(tool.instance),
            Kind::SessionJournalManager | Kind::SessionJournalViewer => {
                self.journal_view(tool.instance)
            }
        }
    }

    pub(super) fn hosted_tool_title(&self, tool: &HostedTab) -> Cow<'static, str> {
        Cow::Borrowed(match tool.kind {
            Kind::OpenPgp => super::openpgp_window::TITLE,
            Kind::KnownHosts => super::known_hosts::TITLE,
            Kind::SshKeys => super::ssh_keys_ui::TITLE,
            Kind::SessionProcesses => super::session_processes::TITLE,
            Kind::RecordingManager => super::recording_access::MANAGER_TITLE,
            Kind::AiManager => crate::ai_manager::TITLE,
            Kind::LogfileManager => super::logfile_access::MANAGER_TITLE,
            Kind::Snippets => return self.snippet_tool_title(tool.instance),
            Kind::Scheduler => super::scheduler_access::TITLE,
            Kind::TerminalEffects => super::terminal_effect_access::TITLE,
            Kind::SessionJournalManager => super::journal_access::TITLE,
            Kind::SessionJournalViewer => {
                return self
                    .journal_title(tool.instance)
                    .unwrap_or(Cow::Borrowed(super::journal_access::TITLE));
            }
            Kind::LogfileViewer => {
                if let Some(title) = self.logfiles.title(tool.instance) {
                    return Cow::Owned(title);
                }
                "Logfile Viewer"
            }
        })
    }

    pub(super) fn dismiss_hosted_tool(&mut self, tool: HostedTab) -> Task<Message> {
        match tool.kind {
            Kind::OpenPgp => {
                self.update_openpgp(tool.instance, super::openpgp_window::Message::Escape)
            }
            Kind::KnownHosts => self.dismiss_hosted_known_hosts(tool.instance),
            Kind::SshKeys => self.dismiss_hosted_ssh_keys(tool.instance),
            Kind::SessionProcesses => self.request_hosted_tool_close(tool.instance),
            Kind::RecordingManager => self.dismiss_hosted_recording_manager(tool.instance),
            Kind::AiManager => self.update_ai_manager(super::ai_access::Message::Dialog(
                tool.instance,
                crate::ai_manager::Message::Close,
            )),
            Kind::LogfileManager | Kind::LogfileViewer => {
                self.dismiss_hosted_logfile(tool.instance)
            }
            Kind::Snippets => self.dismiss_snippets(tool.instance),
            Kind::Scheduler => self.dismiss_hosted_scheduler(tool.instance),
            Kind::SessionJournalManager | Kind::SessionJournalViewer | Kind::TerminalEffects => {
                if self.dismiss_journal_menu(tool.instance) {
                    Task::none()
                } else {
                    self.request_hosted_tool_close(tool.instance)
                }
            }
        }
    }

    fn hosted_tool_busy_or_dirty(&self, tool: HostedTab) -> bool {
        match tool.kind {
            Kind::OpenPgp => {
                self.hosted_openpgp_busy(tool.instance) || self.hosted_openpgp_dirty(tool.instance)
            }
            Kind::KnownHosts => self.hosted_known_hosts_busy(tool.instance),
            Kind::SshKeys => {
                self.hosted_ssh_keys_busy(tool.instance)
                    || self.hosted_ssh_keys_dirty(tool.instance)
            }
            Kind::SessionProcesses => self.hosted_session_process_busy(tool.instance),
            Kind::RecordingManager => self.hosted_recording_manager_busy(tool.instance),
            Kind::AiManager => self.ai_manager_needs_close(tool.instance),
            Kind::LogfileManager | Kind::LogfileViewer => {
                self.hosted_logfile_busy(tool.instance) || self.logfile_badge_dirty(tool.instance)
            }
            Kind::Snippets => {
                self.hosted_snippets_busy(tool.instance)
                    || self.hosted_snippets_dirty(tool.instance)
            }
            Kind::Scheduler => self.hosted_scheduler_dirty(tool.instance),
            Kind::SessionJournalViewer => self.journal_entry_needs_close(tool.instance),
            Kind::SessionJournalManager | Kind::TerminalEffects => false,
        }
    }

    fn prompt_hosted_tool_close(&mut self, tool: HostedTab, token: u64) {
        match tool.kind {
            Kind::OpenPgp => self.prompt_hosted_openpgp_close(tool.instance, token),
            Kind::SshKeys => self.prompt_hosted_ssh_keys_close(tool.instance, token),
            Kind::Snippets => self.prompt_hosted_snippets_close(tool.instance, token),
            Kind::Scheduler => self.prompt_hosted_scheduler_close(tool.instance, token),
            Kind::LogfileManager => self.prompt_logfile_close(tool.instance, token),
            Kind::SessionJournalViewer => self.prompt_journal_entry_close(tool.instance, token),
            Kind::AiManager => self.prompt_ai_manager_close(tool.instance, Some(token)),
            Kind::KnownHosts
            | Kind::SessionProcesses
            | Kind::RecordingManager
            | Kind::LogfileViewer
            | Kind::SessionJournalManager
            | Kind::TerminalEffects => {}
        }
    }

    fn cancel_hosted_tool_prompt(&mut self, tool: HostedTab) {
        match tool.kind {
            Kind::OpenPgp => self.cancel_hosted_openpgp_close(tool.instance),
            Kind::SshKeys => self.cancel_hosted_ssh_keys_close(tool.instance),
            Kind::Snippets => self.cancel_hosted_snippets_close(tool.instance),
            Kind::Scheduler => self.cancel_hosted_scheduler_close(tool.instance),
            Kind::LogfileManager => self.cancel_logfile_close(tool.instance),
            Kind::SessionJournalViewer => self.cancel_journal_entry_close(tool.instance),
            Kind::AiManager => self.prompt_ai_manager_close(tool.instance, None),
            Kind::KnownHosts
            | Kind::SessionProcesses
            | Kind::RecordingManager
            | Kind::LogfileViewer
            | Kind::SessionJournalManager
            | Kind::TerminalEffects => {}
        }
    }

    pub(super) fn remove_hosted_tool(&mut self, tool: HostedTab) -> Task<Message> {
        match tool.kind {
            Kind::OpenPgp => self.remove_hosted_openpgp(tool.instance),
            Kind::KnownHosts => self.remove_hosted_known_hosts(tool.instance),
            Kind::SshKeys => self.remove_hosted_ssh_keys(tool.instance),
            Kind::SessionProcesses => self.remove_hosted_session_processes(tool.instance),
            Kind::RecordingManager => self.remove_hosted_recording_manager(tool.instance),
            Kind::AiManager => self.remove_ai_manager(tool.instance),
            Kind::LogfileManager | Kind::LogfileViewer => self.logfiles.remove(tool.instance),
            Kind::Snippets => return self.remove_hosted_snippets(tool.instance),
            Kind::Scheduler => self.remove_hosted_scheduler(tool.instance),
            Kind::TerminalEffects => {
                self.effect_dialogs.remove(&tool.instance);
            }
            Kind::SessionJournalManager | Kind::SessionJournalViewer => {
                self.journals.remove(&tool.instance);
            }
        }
        Task::none()
    }

    pub(super) fn focus_hosted_tool(&mut self, instance: window::Id) -> Option<Task<Message>> {
        let (owner, tool) = self.locate_hosted_tool(instance)?;
        self.select_tab(owner, tool.id, false);
        Some(window::gain_focus(owner))
    }

    pub(super) fn hosted_close_locks(&self, instance: window::Id) -> bool {
        self.tool_close.0.as_ref().is_some_and(|pending| {
            pending
                .targets
                .iter()
                .any(|target| target.instance() == instance)
        })
    }

    pub(super) fn hosted_close_blocks_selection(&self, owner: window::Id, tab: u64) -> bool {
        self.hosted_close_prompt(owner)
            .is_some_and(|tool| tool.id != tab)
    }

    pub(super) fn hosted_close_prompt(&self, owner: window::Id) -> Option<HostedTab> {
        let (waiting_owner, tool) =
            self.locate_hosted_tool(self.tool_close.0.as_ref()?.awaiting?)?;
        (waiting_owner == owner).then_some(tool)
    }

    pub(super) fn request_hosted_tool_close(&mut self, instance: window::Id) -> Task<Message> {
        if let Some(close) = self.close_journal_edit(instance) {
            return close;
        }
        let Some((_, tool)) = self.locate_hosted_tool(instance) else {
            return Task::none();
        };
        self.guard_tool_close(CloseScope::Tab, vec![CloseTarget::Hosted(tool)])
            .unwrap_or_else(Task::none)
    }

    pub(super) fn guard_native_ai_manager_close(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Message>> {
        if self.ai_manager_id() == Some(id) && self.hosted_tool_owner(id).is_none() {
            self.guard_tool_close(CloseScope::Tab, vec![CloseTarget::NativeAiManager(id)])
        } else {
            None
        }
    }

    pub(super) fn guard_native_logfile_close(&mut self, id: window::Id) -> Option<Task<Message>> {
        if self.hosted_tool_owner(id).is_some() || !self.logfile_badge_editor_open(id) {
            return None;
        }
        if self.logfile_badge_needs_close_decision(id) {
            return self.guard_tool_close(CloseScope::Tab, vec![CloseTarget::NativeLogfile(id)]);
        }
        Some(self.update_logfile(id, crate::logfile_window::Message::Dismiss))
    }

    pub(super) fn guard_native_journal_close(&mut self, id: window::Id) -> Option<Task<Message>> {
        if self.hosted_tool_owner(id).is_none() && self.journal_entry_needs_close(id) {
            self.guard_tool_close(CloseScope::Tab, vec![CloseTarget::NativeJournal(id)])
        } else {
            None
        }
    }

    pub(super) fn guard_hosted_window_close(
        &mut self,
        owner: window::Id,
        user: bool,
    ) -> Option<Task<Message>> {
        self.guard_tool_close(
            CloseScope::Window(owner, user),
            self.window_close_targets(owner),
        )
    }

    pub(super) fn guard_hosted_batch_close(
        &mut self,
        owner: window::Id,
        batch: u64,
    ) -> Option<Task<Message>> {
        let targets = self
            .tool_targets_for_batch(owner, batch)
            .into_iter()
            .filter_map(|id| self.hosted_tab_at(owner, id))
            .map(CloseTarget::Hosted)
            .collect();
        self.guard_tool_close(CloseScope::Batch(owner, batch), targets)
    }

    pub(super) fn guard_hosted_project_close(
        &mut self,
        owner: window::Id,
        target: super::project_access::OpenTarget,
        layout: Arc<WorkspaceLayout>,
        tabs: Vec<(u64, SessionId)>,
    ) -> Option<Task<Message>> {
        self.guard_tool_close(
            CloseScope::Project(owner, target, layout, tabs),
            self.hosted_tools_in(owner)
                .into_iter()
                .map(CloseTarget::Hosted)
                .collect(),
        )
    }

    pub(super) fn guard_hosted_quit(&mut self, serial_confirmed: bool) -> Option<Task<Message>> {
        self.guard_tool_close(
            CloseScope::Quit(serial_confirmed),
            self.quit_close_targets(),
        )
    }

    fn window_close_targets(&self, owner: window::Id) -> Vec<CloseTarget> {
        self.hosted_tools_in(owner)
            .into_iter()
            .map(CloseTarget::Hosted)
            .chain(self.native_close_targets(Some(owner)))
            .collect()
    }

    fn quit_close_targets(&self) -> Vec<CloseTarget> {
        let mut windows: Vec<_> = self.windows.iter().collect();
        windows.sort_by_key(|(_, window)| window.number);
        windows
            .into_iter()
            .flat_map(|(&id, _)| self.hosted_tools_in(id))
            .map(CloseTarget::Hosted)
            .chain(self.native_close_targets(None))
            .chain(
                self.ai_manager_id()
                    .filter(|&id| self.hosted_tool_owner(id).is_none())
                    .map(CloseTarget::NativeAiManager),
            )
            .chain(
                self.native_logfile_close_targets()
                    .into_iter()
                    .map(CloseTarget::NativeLogfile),
            )
            .collect()
    }

    fn native_close_targets(&self, owner: Option<window::Id>) -> impl Iterator<Item = CloseTarget> {
        self.native_snippet_close_targets(owner)
            .into_iter()
            .map(CloseTarget::NativeSnippet)
            .chain(
                self.native_scheduler_close_targets(owner)
                    .into_iter()
                    .map(CloseTarget::NativeScheduler),
            )
            .chain(
                self.native_journal_close_targets(owner)
                    .into_iter()
                    .map(CloseTarget::NativeJournal),
            )
    }

    fn focus_close_target(&mut self, instance: window::Id) -> Option<Task<Message>> {
        self.focus_hosted_tool(instance).or_else(|| {
            (self.is_snippet_window(instance)
                || self.is_scheduler_window(instance)
                || self.logfiles.contains(instance)
                || self.is_journal_window(instance)
                || self.ai_manager_id() == Some(instance))
            .then(|| window::gain_focus(instance))
        })
    }

    fn target_needs_close_decision(&self, target: CloseTarget) -> bool {
        match target {
            CloseTarget::Hosted(tool) => self.hosted_tool_busy_or_dirty(tool),
            CloseTarget::NativeSnippet(id) => {
                self.hosted_snippets_busy(id) || self.hosted_snippets_dirty(id)
            }
            CloseTarget::NativeScheduler(id) => self.hosted_scheduler_dirty(id),
            CloseTarget::NativeLogfile(id) => self.logfile_badge_needs_close_decision(id),
            CloseTarget::NativeJournal(id) => self.journal_entry_needs_close(id),
            CloseTarget::NativeAiManager(id) => self.ai_manager_needs_close(id),
        }
    }

    fn prompt_close_target(&mut self, target: CloseTarget, token: u64) {
        match target {
            CloseTarget::Hosted(tool) => self.prompt_hosted_tool_close(tool, token),
            CloseTarget::NativeSnippet(id) => self.prompt_hosted_snippets_close(id, token),
            CloseTarget::NativeScheduler(id) => self.prompt_hosted_scheduler_close(id, token),
            CloseTarget::NativeLogfile(id) => self.prompt_logfile_close(id, token),
            CloseTarget::NativeJournal(id) => self.prompt_journal_entry_close(id, token),
            CloseTarget::NativeAiManager(id) => self.prompt_ai_manager_close(id, Some(token)),
        }
    }

    fn cancel_close_target(&mut self, target: CloseTarget) {
        match target {
            CloseTarget::Hosted(tool) => self.cancel_hosted_tool_prompt(tool),
            CloseTarget::NativeSnippet(id) => self.cancel_hosted_snippets_close(id),
            CloseTarget::NativeScheduler(id) => self.cancel_hosted_scheduler_close(id),
            CloseTarget::NativeLogfile(id) => self.cancel_logfile_close(id),
            CloseTarget::NativeJournal(id) => self.cancel_journal_entry_close(id),
            CloseTarget::NativeAiManager(id) => self.prompt_ai_manager_close(id, None),
        }
    }

    fn guard_tool_close(
        &mut self,
        scope: CloseScope,
        targets: Vec<CloseTarget>,
    ) -> Option<Task<Message>> {
        if targets.is_empty() {
            return None;
        }
        for target in &targets {
            if let CloseTarget::Hosted(tool) = target
                && matches!(
                    tool.kind,
                    Kind::SessionJournalManager | Kind::SessionJournalViewer
                )
                && let Some(task) = self.close_journal_edit(tool.instance)
            {
                if matches!(scope, CloseScope::Quit(_)) {
                    window_lifecycle::reply_to_native_quit(false);
                }
                return Some(task);
            }
        }
        if let Some(pending) = &self.tool_close.0 {
            if matches!(scope, CloseScope::Quit(_)) && !matches!(pending.scope, CloseScope::Quit(_))
            {
                window_lifecycle::reply_to_native_quit(false);
            }
            let awaiting = pending.awaiting;
            return Some(
                awaiting
                    .and_then(|id| self.focus_close_target(id))
                    .unwrap_or_else(Task::none),
            );
        }
        self.tool_close.0 = Some(PendingClose {
            token: self.allocate(),
            scope,
            targets,
            approved: Vec::new(),
            awaiting: None,
        });
        Some(self.advance_tool_close())
    }

    pub(super) fn hosted_tool_close_answer(
        &mut self,
        instance: window::Id,
        token: u64,
        approve: bool,
    ) -> Task<Message> {
        let Some(pending) = self
            .tool_close
            .0
            .as_mut()
            .filter(|pending| pending.token == token && pending.awaiting == Some(instance))
        else {
            return Task::none();
        };
        if approve {
            pending.approved.push(instance);
            self.advance_tool_close()
        } else {
            let pending = self.tool_close.0.take().expect("checked close request");
            self.cancel_tool_close(pending)
        }
    }

    fn cancel_tool_close(&mut self, pending: PendingClose) -> Task<Message> {
        for target in pending.targets {
            self.cancel_close_target(target);
        }
        match pending.scope {
            CloseScope::Batch(owner, batch) => self.cancel_tab_batch_close(owner, batch),
            CloseScope::Quit(_) => {
                window_lifecycle::reply_to_native_quit(false);
            }
            _ => {}
        }
        pending
            .awaiting
            .and_then(|id| self.focus_close_target(id))
            .unwrap_or_else(Task::none)
    }

    pub(super) fn resume_hosted_tool_close(&mut self, instance: window::Id) -> Task<Message> {
        if self
            .tool_close
            .0
            .as_ref()
            .is_some_and(|pending| pending.awaiting == Some(instance))
        {
            self.advance_tool_close()
        } else {
            Task::none()
        }
    }

    fn advance_tool_close(&mut self) -> Task<Message> {
        let Some(pending) = self.tool_close.0.as_ref() else {
            return Task::none();
        };
        let current = match pending.scope {
            CloseScope::Window(owner, _) => self.window_close_targets(owner),
            CloseScope::Quit(_) => self.quit_close_targets(),
            _ => Vec::new(),
        };
        if !current.is_empty() {
            let pending = self.tool_close.0.as_mut().expect("checked close request");
            for target in current {
                if !pending.targets.contains(&target) {
                    pending.targets.push(target);
                }
            }
        }
        let pending = self.tool_close.0.as_ref().expect("checked close request");
        let token = pending.token;
        let waiting = pending.targets.iter().copied().find(|target| {
            !pending.approved.contains(&target.instance())
                && self.target_needs_close_decision(*target)
        });
        if let Some(target) = waiting {
            let instance = target.instance();
            self.tool_close
                .0
                .as_mut()
                .expect("checked close request")
                .awaiting = Some(instance);
            self.prompt_close_target(target, token);
            return self.focus_close_target(instance).unwrap_or_else(Task::none);
        }
        let pending = self.tool_close.0.take().expect("checked close request");
        if let CloseScope::Batch(owner, batch) = pending.scope
            && !self.tab_batch_still_applies(owner, batch)
        {
            return self.cancel_tool_close(pending);
        }
        if let CloseScope::Project(owner, target, layout, tabs) = pending.scope {
            let tools: Vec<_> = pending
                .targets
                .iter()
                .filter_map(|target| match target {
                    CloseTarget::Hosted(tool) => Some(*tool),
                    CloseTarget::NativeSnippet(_)
                    | CloseTarget::NativeScheduler(_)
                    | CloseTarget::NativeLogfile(_)
                    | CloseTarget::NativeJournal(_)
                    | CloseTarget::NativeAiManager(_) => None,
                })
                .collect();
            let task = self.resume_project_after_tools(owner, target, layout, tabs, &tools);
            // A failed staging attempt preserves the live tabs and their drafts.
            for target in pending.targets {
                self.cancel_close_target(target);
            }
            return task;
        }
        let mut tasks = Vec::new();
        for target in pending.targets {
            match target {
                CloseTarget::Hosted(tool) => {
                    if let Some((owner, _)) = self.locate_hosted_tool(tool.instance) {
                        self.detach_tab(owner, tool.id);
                    }
                    tasks.push(self.remove_hosted_tool(tool));
                }
                CloseTarget::NativeSnippet(id) => {
                    tasks.push(self.close_native_snippet_after_preflight(id))
                }
                CloseTarget::NativeScheduler(id) => {
                    tasks.push(self.close_native_scheduler_after_preflight(id))
                }
                CloseTarget::NativeLogfile(id) => {
                    self.logfiles.remove(id);
                    tasks.push(if matches!(pending.scope, CloseScope::Tab) {
                        self.close_window(id)
                    } else {
                        self.close_placed_dialog(id)
                    });
                }
                CloseTarget::NativeAiManager(id) => {
                    self.remove_ai_manager(id);
                    tasks.push(if matches!(pending.scope, CloseScope::Tab) {
                        self.close_window(id)
                    } else {
                        self.close_placed_dialog(id)
                    });
                }
                CloseTarget::NativeJournal(id) => {
                    tasks.push(self.remove_closed_journal_windows(id));
                    tasks.push(if matches!(pending.scope, CloseScope::Tab) {
                        self.close_window(id)
                    } else {
                        self.close_placed_dialog(id)
                    });
                }
            }
        }
        tasks.push(match pending.scope {
            CloseScope::Tab => Task::none(),
            CloseScope::Window(owner, user) => self.close_window_inner(owner, user),
            CloseScope::Batch(owner, batch) => self.apply_tab_batch_close(owner, batch),
            CloseScope::Quit(confirmed) => self.quit_confirmed(confirmed),
            CloseScope::Project(..) => unreachable!("project handled before removing tool tabs"),
        });
        Task::batch(tasks)
    }
}
