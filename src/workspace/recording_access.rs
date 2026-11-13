use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_channel::bounded;
use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::widget::{button, row, text};
use iced::{Element, Size, Subscription, Task, window};
use kortty_recording::FILE_EXTENSION;

use crate::recording_window::{self, RecordingWindow};
use crate::recording_window::{
    manager,
    preferences::{Preferences, Scope},
};
use crate::session::{RecordingStatus, Session, SessionId, TabRecording};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as WorkspaceMessage, Tab, TabEntry, Workspace, horizontal, tool_tabs::Kind};

pub(super) use crate::recording_window::MANAGER_TITLE;

#[derive(Clone, Debug)]
pub enum Message {
    Toggle(window::Id),
    ChooseScope(window::Id, u64, Option<Scope>),
    Open(window::Id),
    Window(window::Id, recording_window::Message),
    PreferencesSaved(window::Id, Result<manager::Settings, String>),
    Tick(Instant),
    TabUpdated,
}

#[derive(Default)]
pub(super) struct Video {
    settings: manager::Settings,
    saving: Option<window::Id>,
    notice: Option<String>,
    scope_prompts: HashMap<u64, SessionId>,
    tabs: HashMap<u64, TabCapture>,
    updates: Option<(async_channel::Sender<()>, async_channel::Receiver<()>)>,
}

struct TabUpdates(async_channel::Receiver<()>);

impl Hash for TabUpdates {
    fn hash<H: Hasher>(&self, _: &mut H) {
        // All tab writers share this receiver for the lifetime of the workspace.
    }
}

fn tab_updates(updates: &TabUpdates) -> BoxStream<'static, WorkspaceMessage> {
    updates
        .0
        .clone()
        .map(|()| wrap(Message::TabUpdated))
        .boxed()
}

struct TabCapture {
    recording: TabRecording,
    target: Option<SessionId>,
    members: HashSet<SessionId>,
}

impl Video {
    pub(super) fn load() -> Self {
        let mut video = Self::default();
        match Preferences::load() {
            Ok(preferences) => video.settings.preferences = preferences,
            Err(error) => {
                video.notice = Some(format!("Recording settings could not be loaded ({error})."))
            }
        }
        video
    }

    pub(super) fn preferences(&self) -> &Preferences {
        &self.settings.preferences
    }

    pub(super) fn save_pending(&self) -> bool {
        self.saving.is_some()
    }

    pub(super) fn finish_global_preferences(
        &mut self,
        id: window::Id,
        result: Result<Preferences, String>,
    ) -> Option<Result<(), String>> {
        if self.saving != Some(id) {
            return None;
        }
        self.saving = None;
        Some(result.map(|preferences| {
            self.settings.preferences = preferences;
            self.notice = None;
        }))
    }
}

impl Workspace {
    pub(super) fn save_global_video_preferences(
        &mut self,
        id: window::Id,
        preferences: Preferences,
    ) -> io::Result<Task<Result<Preferences, String>>> {
        if self.video.save_pending() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Recording settings are still being saved",
            ));
        }
        self.video.saving = Some(id);
        Ok(self.write_video_preferences(preferences, false))
    }

    fn write_video_preferences(
        &mut self,
        mut preferences: Preferences,
        approve_encoder: bool,
    ) -> Task<Result<Preferences, String>> {
        let (signal, done) = bounded(1);
        self.track_session_shutdown(done);
        Task::future(async move {
            let _signal = signal;
            if approve_encoder {
                let policy = crate::clipboard_policy::ClipboardPolicy::load();
                if policy.problem.is_some() || !policy.recording_allowed {
                    return Err("FFmpeg approval is disabled by managed recording policy.".into());
                }
                preferences
                    .approve_encoder()
                    .map_err(|error| error.to_string())?;
            }
            preferences
                .save()
                .map(|()| preferences)
                .map_err(|error| error.to_string())
        })
    }

    pub(super) fn update_recording(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::Toggle(id) => {
                self.toggle_recording(id);
                Task::none()
            }
            Message::ChooseScope(window, tab_id, scope) => {
                self.choose_recording_scope(window, tab_id, scope);
                Task::none()
            }
            Message::Open(id) => self.open_recordings(id),
            Message::Window(
                id,
                recording_window::Message::Close
                | recording_window::Message::Manager(manager::Message::Close),
            ) => self.close_recording_view(id),
            Message::Window(id, recording_window::Message::Manager(manager::Message::View)) => {
                self.open_selected_recording(id, false, false)
            }
            Message::Window(id, recording_window::Message::Manager(manager::Message::Export)) => {
                self.open_selected_recording(id, true, false)
            }
            Message::Window(
                id,
                recording_window::Message::Manager(manager::Message::OpenOther),
            ) => self.open_selected_recording(id, false, true),
            Message::Window(id, recording_window::Message::Manager(manager::Message::Save)) => {
                self.save_recording_settings(id)
            }
            Message::PreferencesSaved(id, result) => self.recording_settings_saved(id, result),
            Message::Window(id, message) => {
                let (update, job_done) = self.recording_windows.get_mut(&id).map_or_else(
                    || (Task::none(), None),
                    |state| {
                        let task = state
                            .update(message)
                            .map(move |message| wrap(Message::Window(id, message)));
                        let done = state.take_recording_job_done();
                        (task, done)
                    },
                );
                if let Some(done) = job_done {
                    self.track_session_shutdown(done);
                }
                if self.hosted_recording_manager_busy(id) {
                    update
                } else {
                    Task::batch([update, self.resume_hosted_tool_close(id)])
                }
            }
            Message::Tick(now) => Task::batch(
                self.recording_windows
                    .iter_mut()
                    .filter(|(_, state)| state.playing())
                    .map(|(&id, state)| {
                        state
                            .tick(now)
                            .map(move |message| wrap(Message::Window(id, message)))
                    }),
            ),
            Message::TabUpdated => Task::none(),
        }
    }

    pub(super) fn recording_subscription(&self) -> Subscription<WorkspaceMessage> {
        let replay = if self
            .recording_windows
            .values()
            .any(RecordingWindow::playing)
        {
            window::frames().map(|now| wrap(Message::Tick(now)))
        } else {
            Subscription::none()
        };
        let status = self
            .video
            .updates
            .as_ref()
            .map_or_else(Subscription::none, |(_, receiver)| {
                Subscription::run_with(TabUpdates(receiver.clone()), tab_updates)
            });
        Subscription::batch([replay, status])
    }

    pub(super) fn recording_view(&self, id: window::Id) -> Option<Element<'_, WorkspaceMessage>> {
        self.recording_windows.get(&id).map(|state| {
            state
                .view()
                .map(move |message| wrap(Message::Window(id, message)))
        })
    }

    fn recording_session(&self, id: window::Id) -> Option<&Session> {
        let tab = self.windows.get(&id)?.tab()?;
        if tab.incognito {
            return None;
        }
        let active = tab.focused_session()?;
        self.sessions.get(&active).map(|pane| &pane.session)
    }

    fn toggle_recording(&mut self, id: window::Id) {
        self.reconcile_tab_recordings();
        let Some(tab) = self
            .windows
            .get(&id)
            .and_then(|window| window.tab())
            .filter(|tab| !tab.incognito)
        else {
            return;
        };
        let tab_id = tab.id;
        if let Some(capture) = self.video.tabs.get(&tab_id)
            && capture.recording.status().active()
        {
            capture.recording.stop();
            return;
        }
        let stop = tab
            .grid
            .iter()
            .filter_map(|(_, pane)| self.sessions.get(pane).map(|pane| &pane.session))
            .find(|session| session.recording_status().active())
            .map(|session| {
                if matches!(session.recording_status(), RecordingStatus::Stopping(_)) {
                    Ok(())
                } else {
                    session.stop_recording()
                }
            });
        if let Some(result) = stop {
            if let Err(error) = result {
                self.notice(id, error.to_string());
            }
            return;
        }
        if !self.recording_allowed() {
            self.notice(id, "Terminal recording is disabled by policy.".into());
            return;
        }
        if !self.video.settings.recording_available() {
            self.notice(
                id,
                "Enable terminal recording in Video Manager or Settings → Video.".into(),
            );
            return;
        }
        let Some(focused) = tab.focused_session() else {
            return;
        };
        if tab.grid.iter().count() > 1 {
            self.video.scope_prompts.entry(tab_id).or_insert(focused);
        } else {
            self.start_scoped_recording(id, tab_id, focused, self.video.settings.preferences.scope);
        }
    }

    fn choose_recording_scope(&mut self, owner: window::Id, tab_id: u64, scope: Option<Scope>) {
        if self
            .windows
            .get(&owner)
            .and_then(|window| window.tab())
            .map(|tab| tab.id)
            != Some(tab_id)
        {
            return;
        }
        if let Some(focused) = self.video.scope_prompts.remove(&tab_id)
            && let Some(scope) = scope
        {
            self.start_scoped_recording(owner, tab_id, focused, scope);
        }
    }

    fn start_scoped_recording(
        &mut self,
        owner: window::Id,
        tab_id: u64,
        focused: SessionId,
        scope: Scope,
    ) {
        if !self.recording_allowed() || !self.video.settings.recording_available() {
            return;
        }
        let Some(tab) = self
            .windows
            .get(&owner)
            .and_then(|window| window.tab())
            .filter(|tab| tab.id == tab_id && !tab.incognito)
        else {
            return;
        };
        let target = (scope == Scope::ActiveSplit).then_some(focused);
        let eligible = |pane: SessionId| {
            target.is_none_or(|target| pane == target)
                && self
                    .sessions
                    .get(&pane)
                    .is_some_and(|pane| pane.session.can_record_in_tab())
        };
        if !tab.grid.iter().any(|(_, &pane)| eligible(pane)) {
            self.notice(owner, "Recording requires a live terminal.".into());
            return;
        }
        let preferences = &self.video.settings.preferences;
        let options = preferences.capture_options();
        let result = capture_path(preferences)
            .map_err(|error| error.to_string())
            .and_then(|path| {
                if let Some(target) = target
                    && let Some(pane) = self.sessions.get(&target)
                    && pane.session.recording_status() != RecordingStatus::Unavailable
                {
                    pane.session
                        .start_recording(path, options)
                        .map_err(|error| error.to_string())?;
                    self.video.tabs.remove(&tab_id);
                    Ok(())
                } else {
                    let (redraw, _) = self.video.updates.get_or_insert_with(|| bounded(1));
                    let (recording, done) = TabRecording::start(path, options, redraw.clone())
                        .map_err(|error| error.to_string())?;
                    self.track_session_shutdown(done);
                    self.video.tabs.insert(
                        tab_id,
                        TabCapture {
                            recording,
                            target,
                            members: HashSet::new(),
                        },
                    );
                    self.reconcile_tab_recordings();
                    Ok(())
                }
            });
        if let Err(error) = result {
            self.notice(owner, error);
        }
    }

    pub(super) fn reconcile_tab_recordings(&mut self) {
        if self.video.tabs.is_empty() && self.video.scope_prompts.is_empty() {
            return;
        }
        let policy_allows = self.recording_allowed() && !self.exiting;
        let windows = &self.windows;
        let sessions = &self.sessions;
        let tab_by_id = |tab_id| {
            windows
                .values()
                .flat_map(|window| window.tabs.iter())
                .filter_map(TabEntry::terminal)
                .find(|tab| tab.id == tab_id)
        };
        self.video.scope_prompts.retain(|&tab_id, focused| {
            policy_allows
                && tab_by_id(tab_id).is_some_and(|tab| {
                    !tab.incognito && tab.grid.iter().any(|(_, pane)| pane == focused)
                })
        });
        self.video.tabs.retain(|&tab_id, capture| {
            let tab = tab_by_id(tab_id).filter(|tab| !tab.incognito);
            let members: HashSet<_> = if let Some(tab) = tab
                && policy_allows
                && matches!(
                    capture.recording.status(),
                    RecordingStatus::Starting(_) | RecordingStatus::Capturing { .. }
                ) {
                tab.grid
                    .iter()
                    .filter_map(|(_, &id)| {
                        let pane = sessions.get(&id)?;
                        (capture.target.is_none_or(|target| id == target)
                            && pane.session.can_record_in_tab()
                            && !pane.session.recording_status().active())
                        .then_some(id)
                    })
                    .collect()
            } else {
                HashSet::new()
            };
            if members.is_empty() {
                capture.recording.stop();
            }
            if members != capture.members {
                capture.recording.set_members(&members);
                for removed in capture.members.difference(&members) {
                    if let Some(pane) = sessions.get(removed) {
                        pane.session.detach_tab_recording(&capture.recording);
                    }
                }
                for &added in members.difference(&capture.members) {
                    if let Some(pane) = sessions.get(&added) {
                        pane.session.attach_tab_recording(added, &capture.recording);
                    }
                }
                capture.members = members;
            }
            tab.is_some()
        });
    }

    pub(super) fn recording_bar(
        &self,
        owner: window::Id,
        tab: &Tab,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        if tab.incognito {
            return None;
        }
        if self.video.scope_prompts.contains_key(&tab.id) {
            let pick = |scope| wrap(Message::ChooseScope(owner, tab.id, scope));
            let action = |scope: Scope| {
                let button = button(text(scope.to_string())).on_press(pick(Some(scope)));
                if scope == self.video.settings.preferences.scope {
                    button
                } else {
                    button.style(button::secondary)
                }
            };
            return Some(horizontal(
                row![
                    text("Recording scope").size(13),
                    action(Scope::ActiveSplit),
                    action(Scope::WholeTab),
                    button("Cancel")
                        .style(button::secondary)
                        .on_press(pick(None)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            ));
        }
        let capture = self.video.tabs.get(&tab.id)?;
        let name = capture
            .target
            .map_or("Whole tab recording", |_| "Active split recording");
        let (status, stop) = match capture.recording.status() {
            RecordingStatus::Starting(_) => ("Starting recording…".into(), true),
            RecordingStatus::Capturing { paused: true, .. } => (format!("{name} paused"), true),
            RecordingStatus::Capturing { paused: false, .. } => (name.into(), true),
            RecordingStatus::Stopping(_) => ("Saving recording…".into(), false),
            RecordingStatus::Failed { error, .. } => (error, false),
            _ => return None,
        };
        let mut bar = row![text(status).size(13)]
            .spacing(6)
            .align_y(iced::Alignment::Center);
        if stop {
            bar = bar.push(button("Stop recording").on_press(wrap(Message::Toggle(owner))));
        }
        Some(horizontal(bar))
    }

    fn open_recordings(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        let hosted = self.window_settings.open_tool_windows_as_tabs;
        if hosted {
            if let Some(id) = self.hosted_tool_in(owner, Kind::RecordingManager) {
                return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            }
        } else if let Some(id) = self.native_recording_manager_for_owner(owner) {
            return window::minimize(id, false).chain(window::gain_focus(id));
        }
        if self.video.saving.is_some() {
            self.notice(owner, "Recording settings are still being saved.".into());
            return Task::none();
        }
        let selected = self
            .windows
            .get(&owner)
            .and_then(|window| window.tab())
            .filter(|tab| !tab.incognito)
            .and_then(|tab| self.video.tabs.get(&tab.id))
            .and_then(|capture| capture.recording.status().path().map(PathBuf::from))
            .or_else(|| {
                self.recording_session(owner)
                    .and_then(|session| session.recording_status().path().map(PathBuf::from))
            });
        let state = RecordingWindow::manager(
            &self.video.settings,
            self.recording_allowed(),
            self.video.notice.clone(),
            selected,
        );
        if hosted {
            let id = window::Id::unique();
            let focus = self.host_tool(owner, Kind::RecordingManager, id);
            Task::batch([focus, self.insert_recording_window(id, state)])
        } else {
            let (id, open) = self.open_modeless_placed_dialog(
                owner,
                DialogKind::VideoManager,
                window::Settings {
                    size: Size::new(760.0, 620.0),
                    min_size: Some(Size::new(620.0, 460.0)),
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
            );
            Task::batch([open, self.insert_recording_window(id, state)])
        }
    }

    fn native_recording_manager_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.recording_windows.iter().find_map(|(&id, state)| {
            (state.is_manager() && self.placed_dialog_owner(id) == Some(owner)).then_some(id)
        })
    }

    pub(super) fn close_native_recording_manager(
        &mut self,
        id: window::Id,
    ) -> Option<Task<WorkspaceMessage>> {
        let owner = self.placed_dialog_owner(id)?;
        if !self.recording_windows.get(&id)?.is_manager() {
            return None;
        }
        self.recording_windows.remove(&id);
        Some(self.close_placed_dialog_and_focus(id, owner))
    }

    pub(super) fn remove_native_recording_manager_for_owner(&mut self, owner: window::Id) {
        if let Some(id) = self.native_recording_manager_for_owner(owner) {
            self.recording_windows.remove(&id);
        }
    }

    pub(super) fn recording_allowed(&self) -> bool {
        self.clipboard.policy.recording_allowed && self.clipboard.policy.problem.is_none()
    }

    pub(super) fn cancel_recording_jobs(&mut self) {
        for state in self.recording_windows.values_mut() {
            state.cancel_recording_jobs();
        }
    }

    fn open_selected_recording(
        &mut self,
        id: window::Id,
        export: bool,
        external: bool,
    ) -> Task<WorkspaceMessage> {
        let path = self
            .recording_windows
            .get_mut(&id)
            .and_then(RecordingWindow::manager_mut)
            .and_then(|manager| manager.replay(external));
        path.map_or_else(Task::none, |path| {
            let mut state = RecordingWindow::replay(path, export);
            if self.recording_allowed() {
                state.set_video_encoder(self.video.settings.preferences.encoder());
            }
            self.show_recording_window(state)
        })
    }

    fn save_recording_settings(&mut self, id: window::Id) -> Task<WorkspaceMessage> {
        if self.video.saving.is_some() {
            return Task::none();
        }
        let Some((settings, approve_encoder)) = self
            .recording_windows
            .get_mut(&id)
            .and_then(RecordingWindow::manager_mut)
            .and_then(|manager| {
                let settings = manager.save_request(&self.video.settings)?;
                let approve =
                    settings.preferences.ffmpeg.is_some() && manager.encoder_approval_requested();
                Some((settings, approve))
            })
        else {
            return Task::none();
        };
        self.video.saving = Some(id);
        if !approve_encoder
            && settings.preferences == self.video.settings.preferences
            && self.video.notice.is_none()
        {
            return self.recording_settings_saved(id, Ok(settings));
        }
        let manager::Settings {
            preferences,
            enabled,
        } = settings;
        self.write_video_preferences(preferences, approve_encoder)
            .map(move |result| {
                wrap(Message::PreferencesSaved(
                    id,
                    result.map(|preferences| manager::Settings {
                        preferences,
                        enabled,
                    }),
                ))
            })
    }

    fn recording_settings_saved(
        &mut self,
        id: window::Id,
        result: Result<manager::Settings, String>,
    ) -> Task<WorkspaceMessage> {
        if self.video.saving != Some(id) {
            return Task::none();
        }
        self.video.saving = None;
        match result {
            Ok(mut settings) => {
                settings.enabled &= self.recording_allowed();
                if settings.preferences.encoder() != self.video.settings.preferences.encoder() {
                    let encoder = settings
                        .preferences
                        .encoder()
                        .filter(|_| self.recording_allowed());
                    for state in self.recording_windows.values_mut() {
                        state.set_video_encoder(encoder.clone());
                    }
                }
                self.video.settings = settings;
                self.video.notice = None;
                if let Some(manager) = self
                    .recording_windows
                    .get_mut(&id)
                    .and_then(RecordingWindow::manager_mut)
                {
                    manager.save_finished();
                    return if self.hosted_close_locks(id) {
                        self.resume_hosted_tool_close(id)
                    } else {
                        self.close_recording_view(id)
                    };
                }
            }
            Err(error) => {
                if let Some(manager) = self
                    .recording_windows
                    .get_mut(&id)
                    .and_then(RecordingWindow::manager_mut)
                {
                    manager.save_failed(error);
                }
                return self.resume_hosted_tool_close(id);
            }
        }
        Task::none()
    }

    pub(super) fn show_recording_window(
        &mut self,
        state: RecordingWindow,
    ) -> Task<WorkspaceMessage> {
        let (kind, size, min_size) = if state.is_export() {
            (
                DialogKind::VideoExport,
                Size::new(700.0, 315.0),
                Size::new(600.0, 280.0),
            )
        } else {
            (
                DialogKind::VideoReplay,
                Size::new(1040.0, 720.0),
                Size::new(620.0, 340.0),
            )
        };
        let (id, open) = self.open_placed_dialog(
            kind,
            window::Settings {
                size,
                min_size: Some(min_size),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let load = self.insert_recording_window(id, state);
        Task::batch([open.map(|_| WorkspaceMessage::Noop), load])
    }

    fn insert_recording_window(
        &mut self,
        id: window::Id,
        mut state: RecordingWindow,
    ) -> Task<WorkspaceMessage> {
        let load = state
            .initialize()
            .map(move |message| wrap(Message::Window(id, message)));
        self.recording_windows.insert(id, state);
        load
    }

    fn close_recording_view(&mut self, id: window::Id) -> Task<WorkspaceMessage> {
        if self.hosted_tool_owner(id).is_some() {
            self.request_hosted_tool_close(id)
        } else {
            self.close_window(id)
        }
    }

    pub(super) fn hosted_recording_manager_busy(&self, id: window::Id) -> bool {
        self.recording_windows
            .get(&id)
            .and_then(RecordingWindow::manager_state)
            .is_some_and(manager::Manager::mutating)
    }

    pub(super) fn remove_hosted_recording_manager(&mut self, id: window::Id) {
        self.recording_windows.remove(&id);
    }

    pub(super) fn dismiss_hosted_recording_manager(
        &mut self,
        id: window::Id,
    ) -> Task<WorkspaceMessage> {
        let Some(manager) = self
            .recording_windows
            .get(&id)
            .and_then(RecordingWindow::manager_state)
        else {
            return Task::none();
        };
        self.update_recording(Message::Window(
            id,
            recording_window::Message::Manager(manager.escape_action()),
        ))
    }
}

fn capture_path(preferences: &Preferences) -> io::Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let name = format!(
        "{:020}-{:09}-{:016x}.{FILE_EXTENSION}",
        now.as_secs(),
        now.subsec_nanos(),
        u64::from_le_bytes(random)
    );
    Ok(preferences.directory()?.join(name))
}

fn wrap(message: Message) -> WorkspaceMessage {
    WorkspaceMessage::Recording(message)
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    fn manager_message(id: window::Id, message: manager::Message) -> WorkspaceMessage {
        wrap(Message::Window(
            id,
            recording_window::Message::Manager(message),
        ))
    }

    #[test]
    fn native_recording_manager_remains_modeless_for_its_own_main_window() {
        use iced::futures::{StreamExt, executor::block_on, future::ready};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (first, _) = workspace.open_window(false);
        let (second, _) = workspace.open_window(false);
        let _ = workspace.update(wrap(Message::Open(first)));
        let _ = workspace.update(wrap(Message::Open(second)));
        let first_manager = workspace.native_recording_manager_for_owner(first).unwrap();
        let second_manager = workspace
            .native_recording_manager_for_owner(second)
            .unwrap();
        assert_ne!(first_manager, second_manager);
        assert!(workspace.owner_dialog(first).is_none());
        assert!(workspace.owner_dialog(second).is_none());

        let reopen = workspace.update(wrap(Message::Open(first)));
        let mut actions = task::into_stream(reopen).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Minimize(id, false))) if id == first_manager
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == first_manager
        ));
        assert_eq!(workspace.recording_windows.len(), 2);

        let close = workspace.update(WorkspaceMessage::Window(
            first,
            window::Event::CloseRequested,
        ));
        let closed: Vec<_> = block_on(
            task::into_stream(close)
                .unwrap()
                .filter_map(|action| {
                    ready(match action {
                        RuntimeAction::Window(WindowAction::Close(id)) => Some(id),
                        _ => None,
                    })
                })
                .collect(),
        );
        assert_eq!(closed, [first_manager, first]);
        assert!(!workspace.recording_windows.contains_key(&first_manager));
        assert_eq!(
            workspace.native_recording_manager_for_owner(second),
            Some(second_manager)
        );

        let _ = workspace.finish_owned_dialog_open(second_manager, second, None);
        let close = workspace.update(manager_message(second_manager, manager::Message::Close));
        let mut actions = task::into_stream(close).unwrap();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Run(id, _))) if id == second_manager
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == second_manager
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == second
        ));
        assert!(workspace.recording_windows.is_empty());
        assert!(workspace.windows.contains_key(&second));
    }

    #[test]
    fn hosted_manager_keeps_its_draft_after_transfer_and_waits_for_its_own_save() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let directory = std::env::current_dir().unwrap().join("recordings-fixture");
        workspace.video.settings.preferences = serde_json::from_value(serde_json::json!({
            "directory": &directory,
        }))
        .unwrap();
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);

        // Drop UI and profile tasks. Listed and PreferencesSaved below are test doubles
        // for asynchronous completions, addressed to the actual hosted instances.
        let _ = workspace.update(wrap(Message::Open(source)));
        let tool = workspace.hosted_tools_in(source)[0];
        let _ = workspace.update(wrap(Message::Open(source)));
        assert_eq!(
            workspace.hosted_tool_in(source, Kind::RecordingManager),
            Some(tool.instance)
        );
        assert_eq!(workspace.hosted_tools_in(source).len(), 1);
        assert_eq!(workspace.recording_windows.len(), 1);

        let _ = workspace.update(manager_message(
            tool.instance,
            manager::Message::Listed(Ok(Vec::new())),
        ));
        let _ = workspace.update(manager_message(
            tool.instance,
            manager::Message::CaptureColor(true),
        ));
        let _ = workspace.update(WorkspaceMessage::Transfer {
            from: source,
            to: Some(destination),
            tab: tool.id,
        });
        assert_eq!(
            workspace.hosted_tool_owner(tool.instance),
            Some(destination)
        );
        assert!(workspace.hosted_tools_in(source).is_empty());

        let _ = workspace.update(wrap(Message::Open(source)));
        let peer = workspace.hosted_tools_in(source)[0].instance;
        assert_ne!(tool.instance, peer);
        let _ = workspace.update(manager_message(
            peer,
            manager::Message::Listed(Ok(Vec::new())),
        ));
        let _ = workspace.update(manager_message(peer, manager::Message::AutoPause(false)));

        let _ = workspace.update(manager_message(tool.instance, manager::Message::Save));
        assert_eq!(workspace.video.saving, Some(tool.instance));
        assert!(workspace.hosted_recording_manager_busy(tool.instance));
        let _ = workspace.update(manager_message(peer, manager::Message::Save));
        assert!(!workspace.hosted_recording_manager_busy(peer));

        let _ = workspace.update(manager_message(tool.instance, manager::Message::Close));
        assert!(workspace.hosted_close_locks(tool.instance));
        assert!(!workspace.hosted_close_locks(peer));
        let _ = workspace.update(wrap(Message::PreferencesSaved(
            peer,
            Ok(manager::Settings::default()),
        )));
        assert_eq!(workspace.video.saving, Some(tool.instance));
        assert_eq!(
            workspace.hosted_tool_owner(tool.instance),
            Some(destination)
        );

        let saved = manager::Settings {
            preferences: serde_json::from_value(serde_json::json!({
                "directory": directory,
                "include_color": true,
            }))
            .unwrap(),
            enabled: false,
        };
        let _ = workspace.update(wrap(Message::PreferencesSaved(tool.instance, Ok(saved))));
        assert_eq!(workspace.video.saving, None);
        assert!(
            workspace
                .video
                .settings
                .preferences
                .capture_options()
                .include_style
        );
        assert!(workspace.hosted_tool_owner(tool.instance).is_none());
        assert!(!workspace.recording_windows.contains_key(&tool.instance));
        assert_eq!(workspace.hosted_tool_owner(peer), Some(source));
        assert!(workspace.recording_windows.contains_key(&peer));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::time::Duration;

    use kortty_recording::Replay;
    use kortty_terminal::{Color, TerminalProfile};

    use super::*;
    use crate::session::Status;

    #[test]
    fn manager_options_reach_real_pty_capture_and_incognito_is_still_excluded() {
        let mut suffix = [0; 8];
        getrandom::fill(&mut suffix).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-video-test-{:016x}",
            u64::from_ne_bytes(suffix)
        ));
        fs::create_dir(&path).unwrap();
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(path);

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let pane = workspace
            .start_session(owner, TerminalProfile::Xterm)
            .unwrap();
        workspace.add_tab(owner, pane);
        wait(|| workspace.sessions[&pane].session.frame().status == Status::Running);

        let _ = workspace.update(wrap(Message::Toggle(owner)));
        assert_eq!(
            workspace.sessions[&pane].session.recording_status(),
            RecordingStatus::Idle
        );

        let mut dialog = RecordingWindow::manager(&workspace.video.settings, true, None, None);
        for message in [
            manager::Message::Enable(true),
            manager::Message::Directory(scratch.0.to_str().unwrap().into()),
            manager::Message::CaptureColor(true),
            manager::Message::AutoPause(false),
        ] {
            let _ = dialog.update(recording_window::Message::Manager(message));
        }
        workspace.video.settings = dialog
            .manager_mut()
            .unwrap()
            .save_request(&workspace.video.settings)
            .unwrap();
        assert!(workspace.video.settings.recording_available());
        let preferences = scratch.0.join("video-settings.json");
        workspace.video.settings.preferences.enabled_after_restart = true;
        workspace
            .video
            .settings
            .preferences
            .save_to(&preferences)
            .unwrap();
        workspace.video.settings = manager::Settings {
            preferences: Preferences::load_from(&preferences).unwrap(),
            enabled: false,
        };
        assert_eq!(
            workspace.sessions[&pane].session.recording_status(),
            RecordingStatus::Idle
        );
        workspace.clipboard.policy.recording_allowed = false;
        let _ = workspace.update(wrap(Message::Toggle(owner)));
        assert_eq!(
            workspace.sessions[&pane].session.recording_status(),
            RecordingStatus::Idle
        );
        workspace.clipboard.policy.recording_allowed = true;

        let _ = workspace.update(wrap(Message::Toggle(owner)));
        wait(|| {
            matches!(
                workspace.sessions[&pane].session.recording_status(),
                RecordingStatus::Capturing { paused: false, .. }
            )
        });
        let session = &workspace.sessions[&pane].session;
        session
            .send(b"printf '\\033[31m%s%s\\033[0m\\n' 'video-' 'marker'\n".to_vec())
            .unwrap();
        wait(|| {
            session
                .frame()
                .screen
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains("video-marker")
        });
        let _ = workspace.update(wrap(Message::Toggle(owner)));
        wait(|| {
            matches!(
                workspace.sessions[&pane].session.recording_status(),
                RecordingStatus::Saved(_)
            )
        });
        let saved = workspace.sessions[&pane].session.recording_status();
        let path = saved.path().unwrap();
        assert_eq!(path.parent(), Some(scratch.0.as_path()));
        let mut replay = Replay::open(path).unwrap();
        assert!(replay.is_complete());
        assert!((0..replay.frame_count()).any(|index| {
            replay
                .frame(index)
                .unwrap()
                .unwrap()
                .screen
                .cells
                .windows("video-marker".len())
                .any(|cells| {
                    cells
                        .iter()
                        .all(|cell| cell.style.foreground == Color::Indexed(1))
                        && cells
                            .iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                            == "video-marker"
                })
        }));

        workspace
            .windows
            .get_mut(&owner)
            .unwrap()
            .tab_mut()
            .unwrap()
            .incognito = true;
        let _ = workspace.update(wrap(Message::Toggle(owner)));
        assert_eq!(workspace.sessions[&pane].session.recording_status(), saved);
        workspace.stop_session(pane);
        wait(|| {
            workspace
                .pending_shutdown
                .iter()
                .all(async_channel::Receiver::is_closed)
        });
    }

    fn wait(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the recording session"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
#[test]
fn whole_tab_recording_follows_a_live_tab_and_captures_a_late_split() {
    use std::time::Duration;

    use crate::session::Status;
    use crate::workspace::chrome::Action;
    use kortty_recording::Replay;

    fn wait(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "whole-tab recording timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn write_marker(workspace: &Workspace, pane: SessionId, suffix: &str) {
        #[cfg(unix)]
        let line = format!("printf '%s%s\\n' 'whole-' '{suffix}'\r");
        #[cfg(windows)]
        let line = format!("[Console]::WriteLine('whole-'+'{suffix}')\r");
        let session = &workspace.sessions[&pane].session;
        session.send(line.into_bytes()).unwrap();
        let marker = format!("whole-{suffix}");
        wait(|| {
            let frame = session.frame();
            frame
                .screen
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains(&marker)
        });
    }

    let scratch = crate::snippets::tests::Profile::new();
    let mut workspace = Workspace::default();
    workspace.clipboard.policy.recording_allowed = true;
    let mut manager = RecordingWindow::manager(&workspace.video.settings, true, None, None);
    for message in [
        manager::Message::Directory(scratch.0.to_str().unwrap().into()),
        manager::Message::Scope(Scope::WholeTab),
        manager::Message::Enable(true),
    ] {
        let _ = manager.update(recording_window::Message::Manager(message));
    }
    workspace.video.settings = manager
        .manager_mut()
        .unwrap()
        .save_request(&workspace.video.settings)
        .unwrap();
    let (from, _) = workspace.open_window(true);
    let (to, _) = workspace.open_window(false);
    let tab = workspace.windows[&from].tab().unwrap();
    let tab_id = tab.id;
    let first = tab.focused_session().unwrap();
    wait(|| workspace.sessions[&first].session.frame().status == Status::Running);

    let _ = workspace.invoke_chrome(from, Action::SplitRight);
    workspace.wait_for_test_split(from);
    let second = workspace.windows[&from]
        .tab()
        .unwrap()
        .focused_session()
        .unwrap();
    assert_ne!(first, second);

    let _ = workspace.invoke_chrome(from, Action::ToggleRecording);
    assert!(workspace.video.scope_prompts.contains_key(&tab_id));
    let _ = workspace.update(wrap(Message::ChooseScope(
        from,
        tab_id,
        Some(Scope::WholeTab),
    )));
    wait(|| {
        matches!(
            workspace.video.tabs[&tab_id].recording.status(),
            RecordingStatus::Capturing { .. }
        )
    });
    write_marker(&workspace, first, "before");
    let _ = workspace.update(WorkspaceMessage::Transfer {
        from,
        to: Some(to),
        tab: tab_id,
    });
    assert_eq!(workspace.windows[&to].tab().unwrap().id, tab_id);
    write_marker(&workspace, second, "moved");

    let _ = workspace.invoke_chrome(to, Action::SplitDown);
    workspace.wait_for_test_split(to);
    let late = workspace.windows[&to]
        .tab()
        .unwrap()
        .focused_session()
        .unwrap();
    assert_ne!(late, first);
    assert_ne!(late, second);
    assert_eq!(
        workspace.video.tabs[&tab_id].members,
        HashSet::from([first, second, late])
    );
    write_marker(&workspace, late, "joined");

    let _ = workspace.invoke_chrome(to, Action::ToggleRecording);
    wait(|| {
        matches!(
            workspace.video.tabs[&tab_id].recording.status(),
            RecordingStatus::Saved(_)
        )
    });
    let saved = workspace.video.tabs[&tab_id].recording.status();
    let mut replay = Replay::open(saved.path().unwrap()).unwrap();
    assert!(replay.is_complete());
    assert_eq!(replay.pane_count(), 3);
    let mut expected = HashMap::from([
        (first.0, "whole-before"),
        (second.0, "whole-moved"),
        (late.0, "whole-joined"),
    ]);
    for index in 0..replay.frame_count() {
        let frame = replay.frame(index).unwrap().unwrap();
        if let Some(&marker) = expected.get(&frame.pane)
            && frame
                .screen
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains(marker)
        {
            expected.remove(&frame.pane);
        }
    }
    assert!(expected.is_empty(), "missing pane markers {expected:?}");
    for pane in [first, second, late] {
        workspace.stop_session(pane);
    }
    wait(|| {
        workspace
            .pending_shutdown
            .iter()
            .all(async_channel::Receiver::is_closed)
    });
}
