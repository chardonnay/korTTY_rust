use std::collections::HashMap;

use iced::{Element, Size, Task, keyboard::Modifiers, window};

use crate::logfile_window::{Message, Window};
use crate::terminal_theme::Rgb;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace, tool_tabs::Kind};

pub(super) use crate::logfile_window::MANAGER_TITLE;

#[derive(Default)]
pub(super) struct Windows(HashMap<window::Id, Window>);

impl Windows {
    pub(super) fn title(&self, id: window::Id) -> Option<String> {
        self.0.get(&id).map(Window::title)
    }

    pub(super) fn remove(&mut self, id: window::Id) {
        self.0.remove(&id);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn contains(&self, id: window::Id) -> bool {
        self.0.contains_key(&id)
    }
}

impl Workspace {
    pub(super) fn open_logfiles(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.window_settings.open_tool_windows_as_tabs && self.windows.contains_key(&owner) {
            if let Some(id) = self.hosted_tool_in(owner, Kind::LogfileManager) {
                return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            }
            return self.open_hosted_logfile(owner, Kind::LogfileManager, Window::manager());
        }
        if let Some((&id, _)) = self
            .logfiles
            .0
            .iter()
            .find(|(id, tool)| tool.is_manager() && self.hosted_tool_owner(**id).is_none())
        {
            return window::gain_focus(id);
        }
        self.open_logfile_window(Window::manager())
    }

    fn open_logfile_window(&mut self, tool: Window) -> Task<Desktop> {
        let (kind, size, minimum) = if tool.is_manager() {
            (
                DialogKind::LogfileManager,
                Size::new(1400.0, 620.0),
                Size::new(760.0, 420.0),
            )
        } else {
            (
                DialogKind::LogfileViewer,
                Size::new(1100.0, 700.0),
                Size::new(700.0, 440.0),
            )
        };
        let (id, opened) = self.open_placed_dialog(
            kind,
            window::Settings {
                size,
                min_size: Some(minimum),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let initialized = self.insert_logfile(id, tool);
        Task::batch([opened.map(|_| Desktop::Noop), initialized])
    }

    fn open_hosted_logfile(
        &mut self,
        owner: window::Id,
        kind: Kind,
        tool: Window,
    ) -> Task<Desktop> {
        let id = window::Id::unique();
        let focus = self.host_tool(owner, kind, id);
        Task::batch([focus, self.insert_logfile(id, tool)])
    }

    fn insert_logfile(&mut self, id: window::Id, mut tool: Window) -> Task<Desktop> {
        let initialized = tool
            .initialize()
            .map(move |message| Desktop::Logfile(id, message));
        self.logfiles.0.insert(id, tool);
        initialized
    }

    fn open_logfile_viewer(&mut self, manager: window::Id, viewer: Window) -> Task<Desktop> {
        let owner = self
            .hosted_tool_owner(manager)
            .filter(|_| self.window_settings.open_tool_windows_as_tabs);
        if let Some(owner) = owner {
            let path = viewer.viewer_path();
            if let Some(instance) = self
                .hosted_tools_in(owner)
                .into_iter()
                .filter(|tool| tool.kind == Kind::LogfileViewer)
                .map(|tool| tool.instance)
                .find(|id| self.logfiles.0.get(id).and_then(Window::viewer_path) == path)
            {
                return self.focus_hosted_tool(instance).unwrap_or_else(Task::none);
            }
            return self.open_hosted_logfile(owner, Kind::LogfileViewer, viewer);
        }
        self.open_logfile_window(viewer)
    }

    pub(super) fn logfile_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        self.logfiles.0.get(&id).map(|tool| {
            let owner = self.hosted_tool_owner(id).unwrap_or(id);
            let [r, g, b, _] = self.window_theme(owner).palette().text.into_rgba8();
            tool.view(Rgb::from_bytes([r, g, b]))
                .map(move |message| Desktop::Logfile(id, message))
        })
    }

    pub(super) fn logfile_modifiers(&mut self, window: window::Id, modifiers: Modifiers) {
        if let Some(tool) = self.logfiles.0.get_mut(&window) {
            tool.modifiers(modifiers);
        }
        for hosted in self.hosted_tools_in(window) {
            if let Some(tool) = self.logfiles.0.get_mut(&hosted.instance) {
                tool.modifiers(modifiers);
            }
        }
    }

    pub(super) fn escape_logfile_window(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let action = self.logfiles.0.get(&id)?.escape_action();
        Some(self.update_logfile(id, action))
    }

    pub(super) fn update_logfile(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        if !self.logfiles.0.contains_key(&id) {
            return Task::none();
        }
        match message {
            Message::FolderPrepared(prepared) => self.open_logfile_folder(id, prepared),
            Message::View => {
                let viewer = self
                    .logfiles
                    .0
                    .get(&id)
                    .and_then(Window::viewer_for_selection);
                viewer.map_or_else(Task::none, |viewer| self.open_logfile_viewer(id, viewer))
            }
            Message::Close if self.hosted_tool_owner(id).is_some() => {
                self.request_hosted_tool_close(id)
            }
            Message::Close => self.close_window(id),
            Message::BadgeClose(token, approve) => {
                if self.logfiles.0[&id].badge_close_token() == Some(token) {
                    self.hosted_tool_close_answer(id, token, approve)
                } else {
                    Task::none()
                }
            }
            message => {
                let tool = self
                    .logfiles
                    .0
                    .get_mut(&id)
                    .expect("checked logfile window");
                let update = tool
                    .update(message)
                    .map(move |message| Desktop::Logfile(id, message));
                if tool.busy() {
                    update
                } else {
                    Task::batch([update, self.resume_hosted_tool_close(id)])
                }
            }
        }
    }

    fn open_logfile_folder(
        &mut self,
        id: window::Id,
        prepared: Result<std::path::PathBuf, String>,
    ) -> Task<Desktop> {
        if !self
            .logfiles
            .0
            .get(&id)
            .is_some_and(Window::operation_active)
        {
            return self.update_logfile(
                id,
                Message::FolderOpened(Err("Opening the logfile folder was cancelled.".into())),
            );
        }
        let path = match prepared {
            Ok(path) => path,
            Err(error) => return self.update_logfile(id, Message::FolderOpened(Err(error))),
        };
        let launch = move || {
            kortty_windowing::open_desktop_directory(&path)
                .then_some(())
                .ok_or_else(|| "The desktop could not open the logfile folder.".into())
        };
        #[cfg(target_os = "macos")]
        return self.update_logfile(id, Message::FolderOpened(launch()));
        #[cfg(not(target_os = "macos"))]
        Task::perform(async move { launch() }, move |result| {
            Desktop::Logfile(id, Message::FolderOpened(result))
        })
    }

    pub(super) fn hosted_logfile_busy(&self, id: window::Id) -> bool {
        self.logfiles.0.get(&id).is_some_and(Window::busy)
    }

    pub(super) fn logfile_badge_dirty(&self, id: window::Id) -> bool {
        self.logfiles.0.get(&id).is_some_and(Window::badge_dirty)
    }

    pub(super) fn logfile_badge_editor_open(&self, id: window::Id) -> bool {
        self.logfiles
            .0
            .get(&id)
            .is_some_and(Window::badge_editor_open)
    }

    pub(super) fn logfile_badge_needs_close_decision(&self, id: window::Id) -> bool {
        self.logfiles
            .0
            .get(&id)
            .is_some_and(Window::badge_needs_close_decision)
    }

    pub(super) fn native_logfile_close_targets(&self) -> Vec<window::Id> {
        let mut targets: Vec<_> = self
            .logfiles
            .0
            .iter()
            .filter(|(id, tool)| {
                tool.badge_needs_close_decision() && self.hosted_tool_owner(**id).is_none()
            })
            .map(|(&id, _)| id)
            .collect();
        targets.sort_unstable();
        targets
    }

    pub(super) fn prompt_logfile_close(&mut self, id: window::Id, token: u64) {
        if let Some(tool) = self.logfiles.0.get_mut(&id) {
            tool.prompt_badge_close(token);
        }
    }

    pub(super) fn cancel_logfile_close(&mut self, id: window::Id) {
        if let Some(tool) = self.logfiles.0.get_mut(&id) {
            tool.cancel_badge_close();
        }
    }

    pub(super) fn dismiss_hosted_logfile(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(tool) = self.logfiles.0.get(&id) else {
            return Task::none();
        };
        self.update_logfile(id, tool.escape_action())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::logfile_window::catalog;

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "kortty-hosted-logfile-{:032x}",
                u128::from_le_bytes(nonce)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn two_logs(&self) -> [PathBuf; 2] {
            let [default, additional] = [self.0.join("terminal-logs"), self.0.join("extra")];
            for directory in [&default, &additional] {
                fs::create_dir(directory).unwrap();
            }
            catalog::add_folder(&self.0, &additional).unwrap();
            let paths =
                [default, additional].map(|folder| folder.join("2026-11-06-13-00-00-local_1.log"));
            fs::write(&paths[0], b"first logfile\n").unwrap();
            fs::write(&paths[1], b"second logfile\n").unwrap();
            paths
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn active_viewer(workspace: &Workspace, owner: window::Id, path: &Path) -> window::Id {
        let active = workspace.windows[&owner].active_tab_id().unwrap();
        let viewer = workspace.hosted_tab_at(owner, active).unwrap();
        assert_eq!(viewer.kind, Kind::LogfileViewer);
        assert_eq!(
            workspace.logfiles.0[&viewer.instance].viewer_path(),
            Some(path)
        );
        viewer.instance
    }

    #[test]
    fn hosted_logfile_viewers_follow_the_manager_and_dedupe_by_path() {
        let fixture = Fixture::new();
        let [first, second] = fixture.two_logs();
        let report = catalog::scan(&fixture.0, || false).unwrap();
        assert_eq!(report.entries.len(), 2);
        let mut workspace = Workspace::default();
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        workspace.window_settings.open_tool_windows_as_tabs = true;

        // Supply the actual fixture catalog through the scan completion message
        // so test setup never opens the desktop user's profile.
        let manager = window::Id::unique();
        let _ = workspace.host_tool(source, Kind::LogfileManager, manager);
        let manager_tab = workspace.windows[&source].active_tab_id().unwrap();
        workspace.logfiles.0.insert(manager, Window::manager());
        let _ = workspace.update(Desktop::Logfile(manager, Message::Scanned(Ok(report))));
        let view = |workspace: &mut Workspace, path: &Path| {
            let _ = workspace.update(Desktop::Logfile(manager, Message::Select(path.to_owned())));
            let _ = workspace.update(Desktop::Logfile(manager, Message::View));
        };

        view(&mut workspace, &first);
        let original = active_viewer(&workspace, source, &first);
        let _ = workspace.update(Desktop::SelectTab(source, manager_tab));
        let _ = workspace.update(Desktop::Transfer {
            from: source,
            to: Some(destination),
            tab: manager_tab,
        });
        assert_eq!(workspace.hosted_tool_owner(manager), Some(destination));
        view(&mut workspace, &first);
        let moved_owner_viewer = active_viewer(&workspace, destination, &first);
        assert_ne!(moved_owner_viewer, original);
        assert_eq!(workspace.hosted_tool_owner(original), Some(source));

        view(&mut workspace, &second);
        let other = active_viewer(&workspace, destination, &second);
        assert_ne!(other, moved_owner_viewer);
        view(&mut workspace, &first);
        assert_eq!(
            active_viewer(&workspace, destination, &first),
            moved_owner_viewer
        );
        assert_eq!(workspace.hosted_tools_in(source).len(), 1);
        assert_eq!(workspace.hosted_tools_in(destination).len(), 3);
    }

    #[test]
    fn logfile_badges_draft_guards_tab_owner_native_window_and_quit_close() {
        use crate::logfile_window::BadgeMessage;

        let fixture = Fixture::new();
        let [path, _] = fixture.two_logs();
        let entry = crate::logfile_window::catalog::scan(&fixture.0, || false)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap();
        for scope in ["tab", "owner", "native", "quit-hosted", "quit-native"] {
            let mut workspace = Workspace::default();
            let (owner, _) = workspace.open_window(false);
            let id = window::Id::unique();
            let hosted = matches!(scope, "tab" | "owner" | "quit-hosted");
            if hosted {
                let _ = workspace.host_tool(owner, Kind::LogfileManager, id);
            }
            let mut manager = Window::manager();
            manager
                .open_badge_editor_for_test(&fixture.0, &entry)
                .unwrap();
            assert!(!manager.badge_dirty());
            workspace.logfiles.0.insert(id, manager);
            let _ =
                workspace.update_logfile(id, Message::Badge(BadgeMessage::Name("Draft".into())));
            assert!(workspace.logfile_badge_dirty(id));

            let _ = match scope {
                "tab" => workspace.request_hosted_tool_close(id),
                "owner" => workspace.close_window(owner),
                "native" => workspace.close_window(id),
                _ => workspace.quit(),
            };
            let token = workspace.logfiles.0[&id].badge_close_token().expect(scope);
            let _ = workspace.update_logfile(id, Message::BadgeClose(token.wrapping_add(1), true));
            assert_eq!(workspace.logfiles.0[&id].badge_close_token(), Some(token));
            let _ = if hosted {
                workspace.dismiss_hosted_logfile(id)
            } else {
                workspace.escape_logfile_window(id).unwrap()
            };
            assert_eq!(workspace.logfiles.0[&id].badge_close_token(), None);
            assert!(workspace.logfile_badge_dirty(id));
            assert!(workspace.windows.contains_key(&owner));

            let _ = workspace.update_logfile(id, Message::Dismiss);
            assert!(!workspace.logfile_badge_editor_open(id));
            assert!(workspace.logfiles.contains(id));

            if matches!(scope, "tab" | "native") {
                workspace
                    .logfiles
                    .0
                    .get_mut(&id)
                    .unwrap()
                    .open_badge_editor_for_test(&fixture.0, &entry)
                    .unwrap();
                let _ = workspace.update_logfile(
                    id,
                    Message::Badge(BadgeMessage::Name("Discard this draft".into())),
                );
                let _ = if hosted {
                    workspace.request_hosted_tool_close(id)
                } else {
                    workspace.close_window(id)
                };
                let token = workspace.logfiles.0[&id].badge_close_token().unwrap();
                let _ = workspace.update_logfile(id, Message::BadgeClose(token, true));
                assert!(!workspace.logfiles.contains(id));
            }
        }
    }
}
