use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::widget::{checkbox, column, pick_list, row, text};
use iced::{Element, Subscription, Task, window};
use kortty_control::Reply;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Message, Workspace};
use crate::session::SessionId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Preferences {
    pub(super) bell: bool,
    pub(super) command_finished: bool,
    pub(super) command_seconds: u16,
    pub(super) programs: bool,
    pub(super) ai_runs: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            bell: false,
            command_finished: true,
            command_seconds: 30,
            programs: true,
            ai_runs: true,
        }
    }
}

impl Preferences {
    pub(super) fn valid(&self) -> bool {
        (1..=3600).contains(&self.command_seconds)
    }
    pub(super) fn normalize(&mut self) {
        self.command_seconds = self.command_seconds.clamp(1, 3600);
    }
    pub(super) fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Bell(value) => self.bell = value,
            Edit::Commands(value) => self.command_finished = value,
            Edit::Programs(value) => self.programs = value,
            Edit::AiRuns(value) => self.ai_runs = value,
            Edit::Seconds(value) if (1..=3600).contains(&value) => self.command_seconds = value,
            Edit::Seconds(_) => {}
        }
    }
    pub(super) fn view(
        &self,
        owner: window::Id,
        allowed: bool,
        marks: bool,
    ) -> Element<'_, Message> {
        let send = move |edit| {
            Message::Settings(super::settings_window::Message::TerminalNotifications(
                owner, edit,
            ))
        };
        let mut seconds = vec![1, 5, 10, 30, 60, 120, 300, 600, 1800, 3600];
        if !seconds.contains(&self.command_seconds) {
            seconds.push(self.command_seconds);
            seconds.sort();
        }
        let duration: Element<'_, Message> = if allowed && marks {
            pick_list(seconds, Some(self.command_seconds), move |value| {
                send(Edit::Seconds(value))
            })
            .into()
        } else {
            text(self.command_seconds.to_string()).into()
        };
        column![
            checkbox(self.bell)
                .label("Notify when an unseen terminal rings its bell")
                .on_toggle_maybe(allowed.then_some(move |value| send(Edit::Bell(value)))),
            checkbox(self.programs)
                .label("Show desktop notifications from terminal programs (OSC 9 / 777)")
                .on_toggle_maybe(allowed.then_some(move |value| send(Edit::Programs(value)))),
            checkbox(self.command_finished)
                .label("Notify when a long command finishes in an unseen terminal")
                .on_toggle_maybe(
                    (allowed && marks).then_some(move |value| send(Edit::Commands(value)))
                ),
            row![text("Minimum command duration (seconds)"), duration,].spacing(8),
            checkbox(self.ai_runs)
                .label("Notify about AI run results and approval requests")
                .on_toggle_maybe(allowed.then_some(move |value| send(Edit::AiRuns(value)))),
        ]
        .spacing(8)
        .into()
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Bell(bool),
    Commands(bool),
    Programs(bool),
    AiRuns(bool),
    Seconds(u16),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Kind {
    Bell,
    Program,
    Command,
    Activity,
    Silence,
    Highlight,
    AiFinished,
    AiFailed,
    AiApproval,
    AiPassword,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AiRunNotice {
    Approval,
    Password,
    Finished,
    Stopped,
}

impl AiRunNotice {
    fn kind(self) -> Kind {
        match self {
            Self::Approval => Kind::AiApproval,
            Self::Password => Kind::AiPassword,
            Self::Finished => Kind::AiFinished,
            Self::Stopped => Kind::AiFailed,
        }
    }

    fn body(self) -> &'static str {
        match self {
            Self::Approval => "An AI run is waiting for approval.",
            Self::Password => "An AI run is waiting for a sudo password.",
            Self::Finished => "An AI run finished.",
            Self::Stopped => "An AI run stopped before completion.",
        }
    }
}

impl Kind {
    fn is_ai(self) -> bool {
        matches!(
            self,
            Self::AiFinished | Self::AiFailed | Self::AiApproval | Self::AiPassword
        )
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum Slot {
    Pane(SessionId),
    Tab(u64),
    MultiExec(u64),
    Swarm(window::Id, u64),
}

#[derive(Clone, Copy)]
struct Target {
    session: SessionId,
    stream: u64,
}

enum Destination {
    Pane(Target),
    AgentPassword {
        activity: window::Id,
        epoch: u64,
        pane: Target,
    },
    Swarm {
        group: window::Id,
        round: u64,
        pane: Option<Target>,
    },
}

#[derive(Clone)]
pub struct Activation {
    ticket: u64,
    generation: u64,
    token: Option<String>,
}
impl std::fmt::Debug for Activation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Terminal notification activation")
    }
}

pub(super) struct State {
    gate: Arc<AtomicU64>,
    sender: async_channel::Sender<Activation>,
    receiver: async_channel::Receiver<Activation>,
    next: u64,
    targets: VecDeque<(u64, Destination)>,
    delivered: HashMap<(Slot, Kind, String), Instant>,
    completions: HashMap<SessionId, (u64, u64)>,
    notified_runs: VecDeque<(u64, Instant)>,
}
impl Default for State {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(32);
        Self {
            gate: Arc::default(),
            sender,
            receiver,
            next: 0,
            targets: VecDeque::new(),
            delivered: HashMap::new(),
            completions: HashMap::new(),
            notified_runs: VecDeque::new(),
        }
    }
}

impl State {
    fn new_completion(&mut self, session: SessionId, stream: u64, sequence: u64) -> bool {
        self.completions.insert(session, (stream, sequence)) != Some((stream, sequence))
    }

    fn notified_run(&self, run: (u64, Instant)) -> bool {
        self.notified_runs.iter().any(|&(epoch, started)| {
            epoch == run.0
                && started.max(run.1).duration_since(started.min(run.1)) < Duration::from_secs(5)
        })
    }

    fn publish(
        &mut self,
        owner: window::Id,
        destination: Destination,
        key: (Slot, Kind, String),
        title: String,
        body: String,
        run: Option<(u64, Instant)>,
    ) -> Task<Message> {
        let now = Instant::now();
        let interval = Duration::from_secs(match key.1 {
            Kind::Program => 5,
            Kind::Highlight => 30,
            _ => 10,
        });
        if run.is_some_and(|run| self.notified_run(run)) {
            return Task::none();
        }
        self.delivered.retain(|_, previous| {
            now.saturating_duration_since(*previous) < Duration::from_secs(30)
        });
        if self.delivered.len() >= 4096 && !self.delivered.contains_key(&key) {
            return Task::none();
        }
        if self
            .delivered
            .get(&key)
            .is_some_and(|previous| now.saturating_duration_since(*previous) < interval)
        {
            return Task::none();
        }
        self.delivered.insert(key, now);
        if let Some(run) = run {
            if self.notified_runs.len() == 16 {
                self.notified_runs.pop_front();
            }
            self.notified_runs.push_back(run);
        }
        self.next = self.next.wrapping_add(1);
        let ticket = self.next;
        if self.targets.len() == 128 {
            self.targets.pop_front();
        }
        self.targets.push_back((ticket, destination));
        let generation = self.gate.load(Ordering::Relaxed);
        let sender = self.sender.clone();
        send_native(
            Some(owner),
            title,
            body,
            Arc::clone(&self.gate),
            move |token| {
                let _ = sender.try_send(Activation {
                    ticket,
                    generation,
                    token,
                });
            },
        )
    }
}

struct Feed(Arc<AtomicU64>, async_channel::Receiver<Activation>);
impl Hash for Feed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}
fn feed(input: &Feed) -> BoxStream<'static, Message> {
    input
        .1
        .clone()
        .map(Message::TerminalNotificationActivated)
        .boxed()
}

impl Workspace {
    pub(super) fn terminal_notifications_enabled(&self) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.clipboard.policy.problem.is_none()
            && self.clipboard.policy.desktop_notifications_allowed
    }
    pub(super) fn terminal_notification_subscription(&self) -> Subscription<Message> {
        if !self.terminal_notifications_enabled() {
            return Subscription::none();
        }
        Subscription::run_with(
            Feed(
                Arc::clone(&self.terminal_notifications.gate),
                self.terminal_notifications.receiver.clone(),
            ),
            feed,
        )
    }
    pub(super) fn revoke_terminal_notifications(&mut self) {
        self.terminal_notifications
            .gate
            .fetch_add(1, Ordering::Relaxed);
        self.terminal_notifications.targets.clear();
    }
    pub(super) fn notify_terminal(
        &mut self,
        session: SessionId,
        rule: &str,
        title: &str,
        body: &str,
    ) -> Task<Message> {
        self.deliver_terminal_notice(session, Kind::Highlight, rule, title, body, None)
    }

    fn external_notification_owner(&self) -> Option<window::Id> {
        self.windows
            .iter()
            .find(|(_, window)| window.focused)
            .or_else(|| self.windows.iter().next())
            .map(|(&id, _)| id)
    }

    pub(super) fn notify_control_activity(&self, title: &str, body: &str) -> Task<Message> {
        if !self.terminal_notifications_enabled() {
            return Task::none();
        }
        let Some(owner) = self.external_notification_owner() else {
            return Task::none();
        };
        let title = format!("kortty · {}", clean(title, 70));
        send_native(
            Some(owner),
            title,
            clean(body, 200),
            Arc::clone(&self.terminal_notifications.gate),
            |_| {},
        )
    }

    pub(super) fn notify_control_request(
        &self,
        title: String,
        body: String,
        reply: Reply,
    ) -> Task<Message> {
        let pending = reply.clone();
        submit_native(
            self.external_notification_owner(),
            title,
            body,
            Arc::clone(&self.terminal_notifications.gate),
            move || pending.active(),
            |_| {},
        )
        .collect()
        .map(move |results| {
            // Iced drops the action when its window has closed. An empty result
            // then reports that the admitted notification could not be submitted.
            reply.send(Ok(json!({
                "shown": true,
                "supported": results.first() == Some(&true),
            })));
        })
        .discard()
    }

    pub(super) fn notify_job_results(&mut self) -> Task<Message> {
        if self
            .scheduler
            .as_ref()
            .is_none_or(|scheduler| !scheduler.has_desktop_notices())
        {
            return Task::none();
        }
        let owner = self
            .windows
            .iter()
            .find(|(_, window)| window.focused)
            .or_else(|| self.windows.iter().next())
            .map(|(&id, _)| id)
            .or_else(|| self.scheduler_owners.keys().next().copied());
        let allowed = self.terminal_notifications_enabled() && (!cfg!(windows) || owner.is_some());
        let Some(scheduler) = &mut self.scheduler else {
            return Task::none();
        };
        let notices = scheduler.take_desktop_notices(Instant::now(), allowed);
        if notices.is_empty() {
            return Task::none();
        }
        Task::batch(notices.into_iter().map(|notice| {
            send_native(
                owner,
                format!("kortty · {}", clean(&notice.title, 80)),
                notice.body,
                Arc::clone(&self.terminal_notifications.gate),
                |_| {},
            )
        }))
    }
    pub(super) fn notify_terminal_kind(
        &mut self,
        session: SessionId,
        kind: Kind,
        title: &str,
        body: &str,
    ) -> Task<Message> {
        self.deliver_terminal_notice(session, kind, "", title, body, None)
    }

    pub(super) fn notify_agent_run(
        &mut self,
        activity: window::Id,
        session: SessionId,
        stream: u64,
        notice: AiRunNotice,
    ) -> Task<Message> {
        if self.native_focused_window == Some(activity)
            || self
                .sessions
                .get(&session)
                .is_none_or(|pane| pane.stream_generation != stream)
        {
            return Task::none();
        }
        self.notify_terminal_kind(session, notice.kind(), "kortty · AI Agent", notice.body())
    }

    pub(super) fn notify_swarm_run(
        &mut self,
        group: window::Id,
        round: u64,
        live: Option<(SessionId, u64)>,
        notice: AiRunNotice,
    ) -> Task<Message> {
        if !self.terminal_notifications_enabled()
            || !self.window_settings.terminal_notifications.ai_runs
            || self.native_focused_window == Some(group)
            || !self.swarm_notice_current(group, round)
        {
            return Task::none();
        }
        let pane = live.map(|(session, stream)| Target { session, stream });
        let owner = if let Some(pane) = pane {
            let Some((owner, tab, _)) = self.notification_pane(pane) else {
                return Task::none();
            };
            if self.windows[&owner].focused && self.windows[&owner].active_tab_id() == Some(tab) {
                return Task::none();
            }
            owner
        } else {
            group
        };
        self.terminal_notifications.publish(
            owner,
            Destination::Swarm { group, round, pane },
            (Slot::Swarm(group, round), notice.kind(), String::new()),
            "kortty · AI Swarm".into(),
            notice.body().into(),
            None,
        )
    }

    pub(super) fn notify_agent_password(
        &mut self,
        activity: window::Id,
        epoch: u64,
        session: SessionId,
        stream: u64,
    ) -> Task<Message> {
        if self.native_focused_window == Some(activity)
            || !self.agent_password_notice_current(activity, epoch, session, stream)
        {
            return Task::none();
        }
        let pane = Target { session, stream };
        let Some((owner, tab, _)) = self.notification_pane(pane) else {
            return Task::none();
        };
        let window = self
            .windows
            .get_mut(&owner)
            .expect("notification owner exists");
        if window.focused && window.active_tab_id() == Some(tab) {
            return Task::none();
        }
        let notice = AiRunNotice::Password;
        if let Some(target) = window.terminal_tabs_mut().find(|target| target.id == tab) {
            target.watch.mark_notification(notice.body());
        }
        if !self.terminal_notifications_enabled()
            || !self.window_settings.terminal_notifications.ai_runs
        {
            return Task::none();
        }
        self.terminal_notifications.publish(
            owner,
            Destination::AgentPassword {
                activity,
                epoch,
                pane,
            },
            (Slot::Tab(tab), notice.kind(), String::new()),
            "kortty · AI Agent".into(),
            notice.body().into(),
            None,
        )
    }

    fn deliver_terminal_notice(
        &mut self,
        session: SessionId,
        kind: Kind,
        scope: &str,
        title: &str,
        body: &str,
        run: Option<(u64, Instant)>,
    ) -> Task<Message> {
        let seen = self.windows.values().any(|window| {
            window.focused
                && window
                    .tab()
                    .is_some_and(|tab| tab.grid.iter().any(|(_, id)| *id == session))
        });
        let Some((owner, index, tab_id, stream, redacted_title, redacted_body)) =
            self.windows.iter().find_map(|(&owner, window)| {
                let (index, _) = window.pane_location(session)?;
                let tab = window.tabs.get(index)?.terminal()?;
                if tab.incognito {
                    return None;
                }
                let pane = self.sessions.get(&session)?;
                // AI notices contain only fixed application text.
                let (title, body) = if kind.is_ai() {
                    (title.to_owned().into(), body.to_owned().into())
                } else {
                    let title = pane
                        .session
                        .redact_journal_note(title, &self.clipboard.policy.terminal_logging)
                        .unwrap_or_else(|| "kortty · Terminal notification".to_owned().into());
                    let body = pane
                        .session
                        .redact_journal_note(body, &self.clipboard.policy.terminal_logging)
                        .unwrap_or_else(|| {
                            "The terminal requested your attention.".to_owned().into()
                        });
                    (title, body)
                };
                Some((owner, index, tab.id, pane.stream_generation, title, body))
            })
        else {
            return Task::none();
        };
        if seen {
            return Task::none();
        }
        if matches!(kind, Kind::Bell | Kind::Activity | Kind::Highlight)
            && self
                .sessions
                .get(&session)
                .and_then(|pane| pane.session.last_mirrored_input())
                .is_some_and(|time| time.elapsed() < Duration::from_secs(2))
        {
            return Task::none();
        }
        let body = clean(&redacted_body, 200);
        if !matches!(kind, Kind::Activity | Kind::Silence)
            && let Some(tab) = self
                .windows
                .get_mut(&owner)
                .and_then(|window| window.tabs.get_mut(index))
                .and_then(|tab| tab.terminal_mut())
        {
            tab.watch.mark_notification(&body);
        }
        if !self.terminal_notifications_enabled() {
            return Task::none();
        }
        let prefs = &self.window_settings.terminal_notifications;
        if !match kind {
            Kind::Bell => prefs.bell,
            Kind::Program => prefs.programs,
            Kind::Command => {
                prefs.command_finished && self.window_settings.prompt_navigation_enabled
            }
            Kind::AiFinished | Kind::AiFailed | Kind::AiApproval | Kind::AiPassword => {
                prefs.ai_runs
            }
            Kind::Activity | Kind::Silence | Kind::Highlight => true,
        } {
            return Task::none();
        }
        if matches!(kind, Kind::Bell | Kind::Program)
            && self.coding_agent_notifications_enabled()
            && self
                .sessions
                .get(&session)
                .is_some_and(|pane| pane.frame.coding_agent().is_some())
        {
            return Task::none();
        }
        let slot = if kind.is_ai() {
            Slot::Tab(tab_id)
        } else if kind != Kind::Program && self.multi_exec.contains(session) {
            Slot::MultiExec(self.multi_exec.epoch())
        } else if kind == Kind::Command {
            Slot::Tab(tab_id)
        } else {
            Slot::Pane(session)
        };
        self.terminal_notifications.publish(
            owner,
            Destination::Pane(Target { session, stream }),
            (slot, kind, scope.to_owned()),
            clean(&redacted_title, 80),
            body,
            run,
        )
    }
    pub(super) fn observe_terminal_notifications(
        &mut self,
        session: SessionId,
        bells: u64,
    ) -> Task<Message> {
        let Some(pane) = self.sessions.get(&session) else {
            return Task::none();
        };
        let programs = pane.session.take_program_notifications();
        let activity = pane.frame.shell_activity();
        let completion = activity.command_exit.and_then(|exit| {
            if !self.terminal_notifications.new_completion(
                session,
                pane.stream_generation,
                exit.sequence,
            ) {
                return None;
            }
            Some((exit.status, activity.completed_runtime?, exit.started))
        });
        let mut tasks = Vec::new();
        if bells != 0 {
            tasks.push(self.notify_terminal_kind(
                session,
                Kind::Bell,
                "kortty · Terminal bell",
                "The terminal requested your attention.",
            ));
        }
        for program in programs {
            let body = if program.title.is_empty() {
                program.body
            } else {
                format!("{} — {}", program.title, program.body)
            };
            tasks.push(self.notify_terminal_kind(
                session,
                Kind::Program,
                "kortty · Terminal program",
                &body,
            ));
        }
        if let Some((status, elapsed, started)) = completion
            && elapsed
                >= Duration::from_secs(u64::from(
                    self.window_settings.terminal_notifications.command_seconds,
                ))
        {
            let body = match status {
                Some(code) => format!(
                    "Command finished with exit status {code} after {} seconds.",
                    elapsed.as_secs()
                ),
                None => format!("Command finished after {} seconds.", elapsed.as_secs()),
            };
            let run = started
                .filter(|_| self.multi_exec.contains(session))
                .map(|started| (self.multi_exec.epoch(), started));
            tasks.push(self.deliver_terminal_notice(
                session,
                Kind::Command,
                "",
                "kortty · Command finished",
                &body,
                run,
            ));
        }
        self.terminal_notifications
            .completions
            .retain(|id, _| self.sessions.contains_key(id));
        Task::batch(tasks)
    }
    pub(super) fn activate_terminal_notification(
        &mut self,
        activation: Activation,
    ) -> Task<Message> {
        if !self.terminal_notifications_enabled()
            || self.terminal_notifications.gate.load(Ordering::Relaxed) != activation.generation
        {
            return Task::none();
        }
        let Some(index) = self
            .terminal_notifications
            .targets
            .iter()
            .position(|(ticket, _)| *ticket == activation.ticket)
        else {
            return Task::none();
        };
        let (_, destination) = self
            .terminal_notifications
            .targets
            .remove(index)
            .expect("notification ticket exists");
        let (target, activity) = match destination {
            Destination::Pane(target) => (target, None),
            Destination::AgentPassword {
                activity,
                epoch,
                pane,
            } => {
                if !self.agent_password_notice_current(activity, epoch, pane.session, pane.stream) {
                    return Task::none();
                }
                (pane, Some(activity))
            }
            Destination::Swarm { group, round, pane } => {
                if !self.swarm_notice_current(group, round) {
                    return Task::none();
                }
                let Some(pane) = pane else {
                    return raise_window(group, activation.token);
                };
                (pane, None)
            }
        };
        let Some((owner, tab_id, pane)) = self.notification_pane(target) else {
            return Task::none();
        };
        if let Some((dialog, focus)) = self.owner_dialog(owner) {
            return Task::batch([raise_window(dialog, activation.token), Task::done(focus)]);
        }
        self.select_tab(owner, tab_id, false);
        if self.windows[&owner].active_tab_id() != Some(tab_id) {
            return Task::none();
        }
        let Some(tab) = self.active_tab_mut(owner) else {
            return Task::none();
        };
        if tab
            .grid
            .maximized()
            .is_some_and(|maximized| maximized != pane)
        {
            tab.grid.restore();
        }
        tab.focused = pane;
        tab.remote_files.release_keyboard();
        self.session_restore.changed(false);
        raise_window(activity.unwrap_or(owner), activation.token)
    }

    fn notification_pane(
        &self,
        target: Target,
    ) -> Option<(window::Id, u64, iced::widget::pane_grid::Pane)> {
        let source = self.sessions.get(&target.session)?;
        if source.stream_generation != target.stream {
            return None;
        }
        self.windows.iter().find_map(|(&owner, window)| {
            let (index, pane) = window.pane_location(target.session)?;
            let tab = window.tabs.get(index)?.terminal()?;
            (!tab.incognito).then_some((owner, tab.id, pane))
        })
    }
}

fn clean(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    *character,
                    '\u{061c}'
                        | '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2060}'..='\u{206f}'
                        | '\u{feff}'
                )
        })
        .take(limit)
        .collect()
}

pub(super) fn send_native(
    owner: Option<window::Id>,
    title: String,
    body: String,
    gate: Arc<AtomicU64>,
    on_activate: impl FnOnce(Option<String>) + Send + 'static,
) -> Task<Message> {
    let submission = submit_native(owner, title, body, gate, || true, on_activate);
    if cfg!(any(target_os = "macos", windows)) && owner.is_some() {
        submission.discard()
    } else {
        Task::none()
    }
}

fn submit_native(
    _owner: Option<window::Id>,
    title: String,
    body: String,
    gate: Arc<AtomicU64>,
    requested: impl FnOnce() -> bool + Send + 'static,
    on_activate: impl FnOnce(Option<String>) + Send + 'static,
) -> Task<bool> {
    let generation = gate.load(Ordering::Relaxed);
    let callback_gate = Arc::clone(&gate);
    let callback = move |token| {
        if callback_gate.load(Ordering::Relaxed) == generation {
            on_activate(token);
        }
    };
    let still_allowed = move || gate.load(Ordering::Relaxed) == generation;
    #[cfg(target_os = "linux")]
    {
        Task::done(
            requested()
                && kortty_windowing::send_desktop_notification(
                    title,
                    body,
                    still_allowed,
                    callback,
                ),
        )
    }
    #[cfg(target_os = "macos")]
    {
        let send = move || {
            requested()
                && still_allowed()
                && kortty_macos::send_desktop_notification(title, body, still_allowed, move || {
                    callback(None)
                })
        };
        match _owner {
            Some(owner) => window::run(owner, move |_| send()),
            None => Task::done(send()),
        }
    }
    #[cfg(windows)]
    {
        let Some(owner) = _owner else {
            return Task::done(false);
        };
        window::run(owner, move |native| {
            if requested()
                && still_allowed()
                && let Ok(handle) = native.window_handle()
            {
                return kortty_windowing::send_desktop_notification(
                    handle,
                    title,
                    body,
                    move || callback(None),
                );
            }
            false
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (title, body, still_allowed, callback, requested);
        Task::done(false)
    }
}

pub(super) fn raise_window(owner: window::Id, _token: Option<String>) -> Task<Message> {
    #[cfg(target_os = "linux")]
    if let Some(token) = _token {
        return window::run(owner, move |native| {
            let (Ok(window), Ok(display)) = (native.window_handle(), native.display_handle())
            else {
                return false;
            };
            kortty_windowing::activate_notification_window(window, display, &token)
        })
        .then(move |raised| {
            if raised {
                Task::none()
            } else {
                window::gain_focus(owner)
            }
        });
    }
    window::gain_focus(owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_commands_are_bound_to_streams_and_multi_exec_start_times() {
        let mut state = State::default();
        let session = SessionId(1);
        assert!(state.new_completion(session, 4, 12));
        assert!(!state.new_completion(session, 4, 12));
        assert!(state.new_completion(session, 5, 12));
        let started = Instant::now();
        state.notified_runs.push_back((2, started));
        assert!(state.notified_run((2, started + Duration::from_secs(4))));
        assert!(!state.notified_run((2, started + Duration::from_secs(5))));
        assert!(!state.notified_run((3, started)));
    }

    #[test]
    fn separate_rules_notify_and_late_multi_exec_completions_only_mark_attention() {
        use crate::session::{IbmKind, Session};
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            Default::default(),
            14,
        );
        workspace.add_tab(owner, session);
        let _ = workspace.notify_terminal(session, "rule-one", "Matched", "First rule");
        let _ = workspace.notify_terminal(session, "rule-two", "Matched", "Second rule");
        let _ = workspace.notify_terminal(session, "rule-one", "Matched", "First rule again");
        assert_eq!(workspace.terminal_notifications.targets.len(), 2);

        workspace.multi_exec.set_panes(&[session], true, true);
        let started = Instant::now();
        let run = Some((workspace.multi_exec.epoch(), started));
        let _ = workspace.deliver_terminal_notice(session, Kind::Command, "", "Done", "Done", run);
        assert_eq!(workspace.terminal_notifications.targets.len(), 3);
        workspace.terminal_notifications.delivered.clear();
        let _ = workspace.deliver_terminal_notice(
            session,
            Kind::Command,
            "",
            "Done",
            "Late member",
            Some((
                workspace.multi_exec.epoch(),
                started + Duration::from_secs(1),
            )),
        );
        assert_eq!(workspace.terminal_notifications.targets.len(), 3);
    }
    #[test]
    fn terminal_notification_defaults_and_text_are_bounded() {
        let prefs = Preferences::default();
        assert!(!prefs.bell && prefs.command_finished && prefs.programs && prefs.ai_runs);
        assert!(prefs.valid());
        assert_eq!(clean("\x1bReady\u{202e} text\n", 8), "Ready te");
        assert_eq!(clean(&"日".repeat(1000), 80).chars().count(), 80);
    }

    #[test]
    fn terminal_notification_click_follows_the_target_and_rejects_replay_or_replaced_stream() {
        use crate::session::{IbmKind, Session};
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let first = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            Default::default(),
            14,
        );
        let target = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            Default::default(),
            14,
        );
        workspace.add_tab(owner, first);
        workspace.add_tab(owner, target);
        let target_tab = workspace.windows[&owner].tab().unwrap().id;
        let (destination, _) = workspace.open_window(false);
        let _ = workspace.transfer(owner, Some(destination), target_tab);
        workspace.terminal_notifications.targets.push_back((
            1,
            Destination::Pane(Target {
                session: target,
                stream: 0,
            }),
        ));
        let click = Activation {
            ticket: 1,
            generation: 0,
            token: None,
        };
        let _ = workspace.activate_terminal_notification(click.clone());
        assert_eq!(
            workspace.windows[&destination]
                .tab()
                .unwrap()
                .focused_session(),
            Some(target)
        );
        assert!(workspace.terminal_notifications.targets.is_empty());
        let _ = workspace.activate_terminal_notification(click);
        workspace.terminal_notifications.targets.push_back((
            2,
            Destination::Pane(Target {
                session: target,
                stream: 0,
            }),
        ));
        workspace
            .sessions
            .get_mut(&target)
            .unwrap()
            .stream_generation = 1;
        let _ = workspace.activate_terminal_notification(Activation {
            ticket: 2,
            generation: 0,
            token: None,
        });
        assert!(workspace.terminal_notifications.targets.is_empty());
        workspace.terminal_notifications.targets.push_back((
            3,
            Destination::Pane(Target {
                session: target,
                stream: 1,
            }),
        ));
        workspace.revoke_terminal_notifications();
        assert!(workspace.terminal_notifications.targets.is_empty());
    }

    #[test]
    fn agent_notices_respect_activity_focus_and_the_reviewed_stream() {
        use crate::session::{IbmKind, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            Default::default(),
            14,
        );
        workspace.add_tab(owner, session);
        let activity = window::Id::unique();
        workspace.native_focused_window = Some(activity);
        assert_eq!(
            workspace
                .notify_agent_run(activity, session, 0, AiRunNotice::Approval)
                .units(),
            0
        );
        workspace.native_focused_window = None;
        assert_eq!(
            workspace
                .notify_agent_run(activity, session, 1, AiRunNotice::Approval)
                .units(),
            0
        );
        let _ = workspace.notify_agent_run(activity, session, 0, AiRunNotice::Approval);
        let _ = workspace.notify_agent_run(activity, session, 0, AiRunNotice::Approval);
        let _ = workspace.notify_agent_run(activity, session, 0, AiRunNotice::Finished);
        assert_eq!(workspace.terminal_notifications.targets.len(), 2);
        assert!(workspace.terminal_notifications.targets.iter().all(|(_, destination)| {
            matches!(destination, Destination::Pane(target) if target.session == session && target.stream == 0)
        }));
        workspace.window_settings.terminal_notifications.ai_runs = false;
        let _ = workspace.notify_agent_run(activity, session, 0, AiRunNotice::Stopped);
        assert_eq!(workspace.terminal_notifications.targets.len(), 2);
        assert_eq!(
            AiRunNotice::Stopped.body(),
            "An AI run stopped before completion."
        );
    }

    #[test]
    fn password_notice_click_requires_the_owned_live_challenge() {
        use crate::session::{IbmKind, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            Default::default(),
            14,
        );
        workspace.add_tab(owner, session);
        let activity = window::Id::unique();
        workspace.terminal_notifications.targets.push_back((
            1,
            Destination::AgentPassword {
                activity,
                epoch: 3,
                pane: Target { session, stream: 0 },
            },
        ));
        let task = workspace.activate_terminal_notification(Activation {
            ticket: 1,
            generation: 0,
            token: None,
        });
        assert_eq!(task.units(), 0);
        assert!(workspace.terminal_notifications.targets.is_empty());
    }

    #[test]
    fn terminal_notification_policy_private_tabs_and_cooldown_are_enforced() {
        use crate::session::{IbmKind, Session};
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            Default::default(),
            14,
        );
        workspace.add_tab(owner, session);
        let _ = workspace.notify_terminal_kind(session, Kind::Bell, "Bell", "Attention");
        assert!(workspace.terminal_notifications.targets.is_empty());
        assert!(
            workspace.windows[&owner]
                .tab()
                .unwrap()
                .watch
                .attention_text()
                .is_some()
        );
        workspace.window_settings.terminal_notifications.bell = true;
        let _ = workspace.notify_terminal_kind(session, Kind::Bell, "Bell", "Attention");
        assert_eq!(workspace.terminal_notifications.targets.len(), 1);
        let _ = workspace.notify_terminal_kind(session, Kind::Bell, "Bell", "Attention");
        assert_eq!(workspace.terminal_notifications.targets.len(), 1);
        workspace
            .windows
            .get_mut(&owner)
            .unwrap()
            .tab_mut()
            .unwrap()
            .incognito = true;
        let _ = workspace.notify_terminal_kind(session, Kind::Program, "Program", "Attention");
        assert_eq!(workspace.terminal_notifications.targets.len(), 1);
        workspace
            .windows
            .get_mut(&owner)
            .unwrap()
            .tab_mut()
            .unwrap()
            .incognito = false;
        workspace.clipboard.policy.desktop_notifications_allowed = false;
        let _ = workspace.notify_terminal_kind(session, Kind::Program, "Program", "Attention");
        assert_eq!(workspace.terminal_notifications.targets.len(), 1);
    }
}
