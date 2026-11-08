use std::io;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_channel::bounded;
use iced::{Element, Size, Subscription, Task, window};
use kortty_recording::FILE_EXTENSION;

use crate::recording_window::{self, RecordingWindow};
use crate::recording_window::{manager, preferences::Preferences};
use crate::session::{RecordingStatus, Session};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as WorkspaceMessage, Workspace, tool_tabs::Kind};

pub(super) use crate::recording_window::MANAGER_TITLE;

#[derive(Clone, Debug)]
pub enum Message {
    Toggle(window::Id),
    Open(window::Id),
    Window(window::Id, recording_window::Message),
    PreferencesSaved(window::Id, Result<manager::Settings, String>),
    Tick(Instant),
}

#[derive(Default)]
pub(super) struct Video {
    settings: manager::Settings,
    saving: Option<window::Id>,
    notice: Option<String>,
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
        Ok(self.write_video_preferences(preferences))
    }

    fn write_video_preferences(
        &mut self,
        preferences: Preferences,
    ) -> Task<Result<Preferences, String>> {
        let (signal, done) = bounded(1);
        self.track_session_shutdown(done);
        Task::future(async move {
            let _signal = signal;
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
                let update = self
                    .recording_windows
                    .get_mut(&id)
                    .map_or_else(Task::none, |state| {
                        state
                            .update(message)
                            .map(move |message| wrap(Message::Window(id, message)))
                    });
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
        }
    }

    pub(super) fn recording_subscription(&self) -> Subscription<WorkspaceMessage> {
        if self
            .recording_windows
            .values()
            .any(RecordingWindow::playing)
        {
            window::frames().map(|now| wrap(Message::Tick(now)))
        } else {
            Subscription::none()
        }
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
        let Some(session) = self.recording_session(id) else {
            return;
        };
        let result = match session.recording_status() {
            RecordingStatus::Capturing { .. } => {
                session.stop_recording().map_err(|error| error.to_string())
            }
            RecordingStatus::Idle | RecordingStatus::Saved(_) | RecordingStatus::Failed { .. } => {
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
                let preferences = &self.video.settings.preferences;
                capture_path(preferences)
                    .map_err(|error| error.to_string())
                    .and_then(|path| {
                        session
                            .start_recording(path, preferences.capture_options())
                            .map_err(|error| error.to_string())
                    })
            }
            _ => return,
        };
        if let Err(error) = result {
            self.notice(id, error);
        }
    }

    fn open_recordings(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        let hosted =
            self.window_settings.open_tool_windows_as_tabs && self.windows.contains_key(&owner);
        if hosted {
            if let Some(id) = self.hosted_tool_in(owner, Kind::RecordingManager) {
                return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            }
        } else if let Some((&id, _)) = self
            .recording_windows
            .iter()
            .find(|(id, state)| state.is_manager() && self.hosted_tool_owner(**id).is_none())
        {
            return window::gain_focus(id);
        }
        if self.video.saving.is_some() {
            self.notice(owner, "Recording settings are still being saved.".into());
            return Task::none();
        }
        let selected = self
            .recording_session(owner)
            .and_then(|session| session.recording_status().path().map(PathBuf::from));
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
            self.show_recording_window(state)
        }
    }

    pub(super) fn recording_allowed(&self) -> bool {
        self.clipboard.policy.recording_allowed && self.clipboard.policy.problem.is_none()
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
            self.show_recording_window(RecordingWindow::replay(path, export))
        })
    }

    fn save_recording_settings(&mut self, id: window::Id) -> Task<WorkspaceMessage> {
        if self.video.saving.is_some() {
            return Task::none();
        }
        let Some(settings) = self
            .recording_windows
            .get_mut(&id)
            .and_then(RecordingWindow::manager_mut)
            .and_then(|manager| manager.save_request(&self.video.settings))
        else {
            return Task::none();
        };
        self.video.saving = Some(id);
        if settings.preferences == self.video.settings.preferences && self.video.notice.is_none() {
            return self.recording_settings_saved(id, Ok(settings));
        }
        let manager::Settings {
            preferences,
            enabled,
        } = settings;
        self.write_video_preferences(preferences)
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
        let (kind, size, min_size) = if state.is_manager() {
            (
                DialogKind::VideoManager,
                Size::new(760.0, 620.0),
                Size::new(620.0, 460.0),
            )
        } else if state.is_export() {
            (
                DialogKind::VideoExport,
                Size::new(700.0, 235.0),
                Size::new(600.0, 200.0),
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
mod hosted_tests {
    use super::*;

    fn manager_message(id: window::Id, message: manager::Message) -> WorkspaceMessage {
        wrap(Message::Window(
            id,
            recording_window::Message::Manager(message),
        ))
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
