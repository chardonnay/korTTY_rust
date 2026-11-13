//! Native OpenPGP recipient management in a window or a tool tab.

use std::fmt;
use std::io;
use std::path::PathBuf;

use iced::{Element, Size, Task, window};

use crate::profile;
use crate::snippets::recipients::{
    Action, CertificateInfo, Choice, FILE_NAME, inspect_certificate,
};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace, tool_tabs, window_lifecycle};
use page::{Message as PageMessage, Page};

mod browser;
mod page;

pub(super) const TITLE: &str = "OpenPGP Recipients";

#[derive(Clone)]
pub enum Message {
    Focus,
    Escape,
    Page(PageMessage),
    Finished(u64, Result<Vec<Choice>, String>),
    CacheRefreshed(u64, Result<Vec<Choice>, String>),
    Previewed(u64, Result<CertificateInfo, String>),
    Browser(browser::Message),
    HostedClose(u64, bool),
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpenPGP recipient window action")
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CloseIntent {
    Window,
    Quit,
}

#[derive(Clone, Copy)]
enum Placement {
    Standalone { source: window::Id },
    Hosted,
}

pub(super) struct Window {
    id: window::Id,
    placement: Placement,
    path: PathBuf,
    page: Page,
    work: Option<u64>,
    closing: Option<CloseIntent>,
    hosted_close: Option<u64>,
    preview_work: Option<u64>,
    browser_work: Option<u64>,
    browser: Option<browser::Browser>,
}

impl Workspace {
    pub(super) fn show_openpgp(&mut self, source: window::Id) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.openpgp_source_live(source) {
            return Task::none();
        }
        let hosted =
            self.window_settings.open_tool_windows_as_tabs && self.windows.contains_key(&source);
        if hosted {
            if let Some(id) = self.hosted_tool_in(source, tool_tabs::Kind::OpenPgp) {
                return self.focus_openpgp(id);
            }
        } else if let Some((id, previous)) = self.standalone_openpgp() {
            if !self.openpgp_source_live(previous)
                && let Some(state) = self.openpgp.get_mut(&id)
            {
                state.placement = Placement::Standalone { source };
            }
            return window::gain_focus(id);
        }
        let path = match profile::root() {
            Ok(root) => root.join(FILE_NAME),
            Err(error) => {
                self.notice(
                    self.hosted_tool_owner(source).unwrap_or(source),
                    format!("Could not open OpenPGP recipients ({error})."),
                );
                return Task::none();
            }
        };
        let (id, opened, placement) = if hosted {
            (window::Id::unique(), Task::none(), Placement::Hosted)
        } else {
            let (id, opened) = self.open_placed_dialog(
                DialogKind::OpenPgpRecipients,
                window::Settings {
                    size: Size::new(730.0, 545.0),
                    min_size: Some(Size::new(510.0, 400.0)),
                    position: window::Position::Centered,
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
            );
            (
                id,
                opened.map(|_| Desktop::Noop),
                Placement::Standalone { source },
            )
        };
        let token = self.allocate();
        self.openpgp.insert(
            id,
            Window {
                id,
                placement,
                path: path.clone(),
                page: Page::loading(),
                work: Some(token),
                closing: None,
                hosted_close: None,
                preview_work: None,
                browser_work: None,
                browser: None,
            },
        );
        let opened = if hosted {
            let placed = self.host_tool(source, tool_tabs::Kind::OpenPgp, id);
            if self.hosted_tool_in(source, tool_tabs::Kind::OpenPgp) != Some(id) {
                self.openpgp.remove(&id);
                return placed;
            }
            placed
        } else {
            opened
        };
        Task::batch([opened, action_task(id, token, Action::List(path))])
    }

    pub(super) fn is_openpgp_standalone(&self, id: window::Id) -> bool {
        self.openpgp
            .get(&id)
            .is_some_and(|state| matches!(state.placement, Placement::Standalone { .. }))
    }

    #[cfg(any(target_os = "macos", test))]
    pub(super) fn has_standalone_openpgp(&self) -> bool {
        self.standalone_openpgp().is_some()
    }

    fn standalone_openpgp(&self) -> Option<(window::Id, window::Id)> {
        self.openpgp
            .values()
            .find_map(|state| match state.placement {
                Placement::Standalone { source } => Some((state.id, source)),
                Placement::Hosted => None,
            })
    }

    pub(super) fn openpgp_for_source(&self, source: window::Id) -> Option<window::Id> {
        let (id, owner) = self.standalone_openpgp()?;
        (owner == source).then_some(id)
    }

    fn openpgp_source_live(&self, source: window::Id) -> bool {
        self.windows.contains_key(&source)
            || self.settings.title(source).is_some()
            || self.is_snippet_surface(source)
    }

    pub(super) fn openpgp_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let state = self.openpgp.get(&id)?;
        if let Some(browser) = &state.browser {
            return Some(
                browser
                    .view()
                    .map(move |message| wrap(id, Message::Browser(message))),
            );
        }
        let hosted_close = state.hosted_close;
        Some(state.page.view().map(move |message| {
            let message = match (hosted_close, message) {
                (Some(token), PageMessage::KeepEditing) => Message::HostedClose(token, false),
                (Some(token), PageMessage::DiscardAndClose) => Message::HostedClose(token, true),
                (_, message) => Message::Page(message),
            };
            wrap(id, message)
        }))
    }

    pub(super) fn update_openpgp(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        if let Message::CacheRefreshed(token, result) = message {
            if self.openpgp_cache_refresh == Some(token) {
                self.openpgp_cache_refresh = None;
                if let Ok(choices) = result {
                    self.backup_recipients = choices;
                }
            }
            return Task::none();
        }
        let Some(state) = self.openpgp.get_mut(&id) else {
            return Task::none();
        };
        match message {
            Message::Focus => self.focus_openpgp(id),
            Message::Finished(token, result) if state.work == Some(token) => {
                state.work = None;
                let refresh_path = result.is_ok().then(|| state.path.clone());
                state
                    .page
                    .update(PageMessage::Finished(result), &state.path);
                let refresh = refresh_path
                    .map_or_else(Task::none, |path| self.refresh_openpgp_cache(id, path));
                Task::batch([refresh, self.advance_openpgp_close(id)])
            }
            Message::Previewed(token, result) if state.preview_work == Some(token) => {
                state.preview_work = None;
                state
                    .page
                    .update(PageMessage::PreviewFinished(token, result), &state.path);
                self.advance_openpgp_close(id)
            }
            Message::Browser(message @ browser::Message::Listed(..)) => {
                self.update_openpgp_browser(id, message)
            }
            Message::HostedClose(token, approve)
                if state.hosted_close == Some(token) && !state.page.busy() =>
            {
                self.hosted_tool_close_answer(id, token, approve)
            }
            Message::Escape if !state.page.busy() && state.hosted_close.is_some() => {
                let token = state.hosted_close.expect("checked hosted close prompt");
                self.hosted_tool_close_answer(id, token, false)
            }
            Message::HostedClose(_, _) => Task::none(),
            _ if state.hosted_close.is_some() => Task::none(),
            Message::Escape => {
                if state.browser.take().is_some() {
                    return Task::none();
                }
                if state.closing.is_some() && !state.page.busy() {
                    return self.update_openpgp(id, Message::Page(PageMessage::KeepEditing));
                }
                if state.page.cancel_inner() {
                    return Task::none();
                }
                self.request_openpgp_close(id)
            }
            Message::Browser(message) => self.update_openpgp_browser(id, message),
            Message::Page(_) if state.browser.is_some() => Task::none(),
            Message::Page(PageMessage::Done) => self.request_openpgp_close(id),
            Message::Page(PageMessage::KeepEditing) if !state.page.busy() => {
                state.page.update(PageMessage::KeepEditing, &state.path);
                if state.closing.take() == Some(CloseIntent::Quit) {
                    window_lifecycle::reply_to_native_quit(false);
                }
                Task::none()
            }
            Message::Page(PageMessage::DiscardAndClose)
                if state.closing.is_some() && state.page.discard_and_close() =>
            {
                self.finish_openpgp_close(id)
            }
            Message::Page(PageMessage::Finished(_) | PageMessage::PreviewFinished(_, _))
            | Message::Finished(_, _)
            | Message::CacheRefreshed(_, _)
            | Message::Previewed(_, _) => Task::none(),
            Message::Page(PageMessage::Browse) if !self.exiting && !self.scheduler_draining => {
                self.start_openpgp_browser(id)
            }
            Message::Page(PageMessage::PreviewCertificate)
                if !self.exiting && !self.scheduler_draining =>
            {
                state.preview_certificate()
            }
            Message::Page(message) if !self.exiting && !self.scheduler_draining => {
                let Some(action) = state.page.update(message, &state.path) else {
                    return Task::none();
                };
                self.start_openpgp_action(id, action)
            }
            Message::Page(_) => Task::none(),
        }
    }

    fn focus_openpgp(&mut self, id: window::Id) -> Task<Desktop> {
        if self.is_openpgp_standalone(id) {
            window::gain_focus(id)
        } else {
            self.focus_hosted_tool(id).unwrap_or_else(Task::none)
        }
    }

    fn refresh_openpgp_cache(&mut self, id: window::Id, path: PathBuf) -> Task<Desktop> {
        // Different windows may commit in a different order than their tasks
        // return. Read the current catalog and accept only the newest refresh.
        let token = self.allocate();
        self.openpgp_cache_refresh = Some(token);
        file_task(
            move || Action::List(path).run(),
            move |result| wrap(id, Message::CacheRefreshed(token, result)),
        )
    }

    fn advance_openpgp_close(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(state) = self.openpgp.get(&id) else {
            return Task::none();
        };
        match state.placement {
            Placement::Hosted => self.resume_hosted_tool_close(id),
            Placement::Standalone { .. } => {
                let intent = state.closing;
                intent.map_or_else(Task::none, |intent| self.queue_openpgp_close(id, intent))
            }
        }
    }

    fn start_openpgp_action(&mut self, id: window::Id, action: Action) -> Task<Desktop> {
        let token = self.allocate();
        let Some(state) = self.openpgp.get_mut(&id) else {
            return Task::none();
        };
        state.work = Some(token);
        action_task(id, token, action)
    }

    pub(super) fn request_openpgp_close(&mut self, id: window::Id) -> Task<Desktop> {
        if self.is_openpgp_standalone(id) {
            self.queue_openpgp_close(id, CloseIntent::Window)
        } else {
            self.request_hosted_tool_close(id)
        }
    }

    pub(super) fn request_openpgp_quit(&mut self) -> Option<Task<Desktop>> {
        let (id, _) = self.standalone_openpgp()?;
        let state = &self.openpgp[&id];
        if state.page.busy() {
            window_lifecycle::reply_to_native_quit(false);
            return Some(window::gain_focus(id));
        }
        Some(self.queue_openpgp_close(id, CloseIntent::Quit))
    }

    fn queue_openpgp_close(&mut self, id: window::Id, intent: CloseIntent) -> Task<Desktop> {
        let Some(state) = self.openpgp.get_mut(&id) else {
            return Task::none();
        };
        if state.closing != Some(CloseIntent::Quit) {
            state.closing = Some(intent);
        }
        state.browser = None;
        if state.page.request_close() {
            self.finish_openpgp_close(id)
        } else {
            window::gain_focus(id)
        }
    }

    fn finish_openpgp_close(&mut self, id: window::Id) -> Task<Desktop> {
        let Some((standalone, source)) = self
            .standalone_openpgp()
            .filter(|(actual, _)| *actual == id)
        else {
            return Task::none();
        };
        let state = self
            .openpgp
            .remove(&standalone)
            .expect("known OpenPGP window");
        let after =
            if state.closing == Some(CloseIntent::Quit) || self.should_exit_after_window_close() {
                self.quit()
            } else if let Some(focus) = self.focus_hosted_tool(source) {
                focus
            } else if self.openpgp_source_live(source) {
                window::gain_focus(source)
            } else {
                self.windows
                    .iter()
                    .max_by_key(|(_, window)| (window.focused, window.number))
                    .map_or_else(Task::none, |(&id, _)| window::gain_focus(id))
            };
        Task::batch([self.close_placed_dialog(id), after])
    }

    pub(super) fn hosted_openpgp_busy(&self, id: window::Id) -> bool {
        self.hosted_openpgp(id)
            .is_some_and(|state| state.page.busy())
    }

    pub(super) fn hosted_openpgp_dirty(&self, id: window::Id) -> bool {
        self.hosted_openpgp(id)
            .is_some_and(|state| state.page.dirty())
    }

    pub(super) fn prompt_hosted_openpgp_close(&mut self, id: window::Id, token: u64) {
        if let Some(state) = self
            .openpgp
            .get_mut(&id)
            .filter(|state| matches!(state.placement, Placement::Hosted))
        {
            state.hosted_close = Some(token);
            state.browser = None;
            state.page.request_close();
        }
    }

    pub(super) fn cancel_hosted_openpgp_close(&mut self, id: window::Id) {
        if let Some(state) = self
            .openpgp
            .get_mut(&id)
            .filter(|state| matches!(state.placement, Placement::Hosted))
        {
            state.hosted_close = None;
            state.page.update(PageMessage::KeepEditing, &state.path);
        }
    }

    pub(super) fn remove_hosted_openpgp(&mut self, id: window::Id) {
        if self.hosted_openpgp(id).is_some() {
            self.openpgp.remove(&id);
        }
    }

    fn hosted_openpgp(&self, id: window::Id) -> Option<&Window> {
        self.openpgp
            .get(&id)
            .filter(|state| matches!(state.placement, Placement::Hosted))
    }
}

impl Window {
    fn preview_certificate(&mut self) -> Task<Desktop> {
        if self.preview_work.is_some() {
            return Task::none();
        }
        let Some((token, path)) = self.page.begin_preview() else {
            return Task::none();
        };
        self.preview_work = Some(token);
        let id = self.id;
        file_task(
            move || inspect_certificate(&path),
            move |result| wrap(id, Message::Previewed(token, result)),
        )
    }
}

fn action_task(id: window::Id, token: u64, action: Action) -> Task<Desktop> {
    file_task(
        move || action.run(),
        move |result| wrap(id, Message::Finished(token, result)),
    )
}

fn file_task<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
    finished: impl Fn(Result<T, String>) -> Desktop + Send + 'static,
) -> Task<Desktop> {
    Task::perform(
        async move {
            let (send, receive) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name("kortty-openpgp-recipients".into())
                .spawn(move || {
                    let _ = send.send_blocking(work().map_err(|error| error.to_string()));
                })
                .map_err(|error| error.to_string())?;
            receive
                .recv()
                .await
                .map_err(|_| String::from("OpenPGP recipient worker stopped"))?
        },
        finished,
    )
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::OpenPgp(id, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippet_window::{self, SnippetWindow};

    fn choice(name: &str) -> Choice {
        // Test-only worker reply double. These tests exercise routing, and the
        // recipients backend separately verifies real certificates.
        let revision = [0_u8; 16];
        serde_json::from_value(serde_json::json!({
            "name": name,
            "fingerprint": "0000000000000000000000000000000000000000",
            "revision": revision,
            "revoked": false,
        }))
        .unwrap()
    }

    #[test]
    fn only_the_current_window_and_worker_can_refresh_backup_recipients() {
        let mut workspace = Workspace::default();
        let (source, _) = workspace.open_window(false);
        let _ = workspace.show_openpgp(source);
        let id = workspace.openpgp_for_source(source).unwrap();
        let work = workspace.openpgp[&id].work.unwrap();
        workspace.backup_recipients = vec![choice("Cached")];

        let _ = workspace.update_openpgp(
            window::Id::unique(),
            Message::Finished(work, Ok(vec![choice("Closed window")])),
        );
        let _ = workspace.update_openpgp(
            id,
            Message::Finished(work.wrapping_add(1), Ok(vec![choice("Obsolete work")])),
        );
        let _ = workspace.update_openpgp(
            id,
            Message::Page(PageMessage::Finished(Ok(vec![choice("Unstamped")]))),
        );
        assert_eq!(workspace.backup_recipients, [choice("Cached")]);

        let _ = workspace.update_openpgp(id, Message::Finished(work, Ok(vec![choice("Current")])));
        let refresh = workspace.openpgp_cache_refresh.unwrap();
        let _ =
            workspace.update_openpgp(id, Message::Finished(work, Ok(vec![choice("Duplicate")])));
        assert_eq!(workspace.openpgp_cache_refresh, Some(refresh));
        let _ = workspace.update_openpgp(
            id,
            Message::CacheRefreshed(refresh, Ok(vec![choice("Current")])),
        );
        assert_eq!(workspace.backup_recipients, [choice("Current")]);
        assert!(!workspace.openpgp[&id].page.busy());
    }

    #[test]
    fn hosted_managers_are_per_window_and_ignore_an_old_shared_cache_refresh() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (left, _) = workspace.open_window(false);
        let (right, _) = workspace.open_window(false);
        let _ = workspace.show_openpgp(left);
        let first = workspace
            .hosted_tool_in(left, tool_tabs::Kind::OpenPgp)
            .unwrap();
        let _ = workspace.show_openpgp(left);
        assert_eq!(
            workspace.hosted_tool_in(left, tool_tabs::Kind::OpenPgp),
            Some(first)
        );
        let _ = workspace.show_openpgp(right);
        let second = workspace
            .hosted_tool_in(right, tool_tabs::Kind::OpenPgp)
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(workspace.openpgp.len(), 2);
        assert!(workspace.sessions.is_empty());
        assert!(!workspace.has_standalone_openpgp());

        let first_work = workspace.openpgp[&first].work.unwrap();
        let second_work = workspace.openpgp[&second].work.unwrap();
        let _ = workspace.update_openpgp(
            first,
            Message::Finished(first_work, Ok(vec![choice("First view")])),
        );
        let stale_refresh = workspace.openpgp_cache_refresh.unwrap();
        let _ = workspace.update_openpgp(
            second,
            Message::Finished(second_work, Ok(vec![choice("Second view")])),
        );
        let current_refresh = workspace.openpgp_cache_refresh.unwrap();
        let _ = workspace.update_openpgp(
            second,
            Message::CacheRefreshed(current_refresh, Ok(vec![choice("Current catalog")])),
        );
        let _ = workspace.update_openpgp(
            first,
            Message::CacheRefreshed(stale_refresh, Ok(vec![choice("Old catalog")])),
        );
        assert_eq!(workspace.backup_recipients, [choice("Current catalog")]);
    }

    #[test]
    fn hosted_close_rejects_stale_approval_and_cancel_keeps_the_draft() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (source, _) = workspace.open_window(false);
        let _ = workspace.show_openpgp(source);
        let id = workspace
            .hosted_tool_in(source, tool_tabs::Kind::OpenPgp)
            .unwrap();
        let work = workspace.openpgp[&id].work.unwrap();
        let _ = workspace.update_openpgp(id, Message::Finished(work, Ok(Vec::new())));
        let _ = workspace.update_openpgp(id, Message::Page(PageMessage::New));
        let _ = workspace.update_openpgp(
            id,
            Message::Page(PageMessage::Name("Draft recipient".into())),
        );
        let _ = workspace.request_hosted_tool_close(id);
        let token = workspace.openpgp[&id].hosted_close.unwrap();

        let _ = workspace.update_openpgp(id, Message::HostedClose(token.wrapping_add(1), true));
        assert_eq!(workspace.openpgp[&id].hosted_close, Some(token));
        assert!(workspace.hosted_openpgp_dirty(id));
        let _ = workspace.update_openpgp(id, Message::HostedClose(token, false));
        assert!(workspace.openpgp[&id].hosted_close.is_none());
        assert!(workspace.hosted_openpgp_dirty(id));
    }

    #[test]
    fn canceling_the_second_hosted_quit_prompt_keeps_both_drafts_and_tabs() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (first_owner, _) = workspace.open_window(false);
        let (second_owner, _) = workspace.open_window(false);
        let [(first, first_tab), (second, second_tab)] =
            [(first_owner, "First draft"), (second_owner, "Second draft")].map(|(owner, name)| {
                let _ = workspace.show_openpgp(owner);
                let id = workspace
                    .hosted_tool_in(owner, tool_tabs::Kind::OpenPgp)
                    .unwrap();
                let tab = workspace.windows[&owner].tabs[0].id();
                let work = workspace.openpgp[&id].work.unwrap();
                let _ = workspace.update_openpgp(id, Message::Finished(work, Ok(Vec::new())));
                let _ = workspace.update_openpgp(id, Message::Page(PageMessage::New));
                let _ = workspace.update_openpgp(id, Message::Page(PageMessage::Name(name.into())));
                assert!(workspace.hosted_openpgp_dirty(id));
                (id, tab)
            });

        assert!(workspace.guard_hosted_quit(false).is_some());
        let token = workspace.openpgp[&first].hosted_close.unwrap();
        assert!(workspace.openpgp[&second].hosted_close.is_none());
        let _ = workspace.update_openpgp(first, Message::HostedClose(token, true));
        assert_eq!(workspace.openpgp[&second].hosted_close, Some(token));
        assert!(workspace.hosted_openpgp_dirty(first));

        let _ = workspace.update_openpgp(second, Message::HostedClose(token, false));
        for (owner, id, tab) in [
            (first_owner, first, first_tab),
            (second_owner, second, second_tab),
        ] {
            let tabs = &workspace.windows[&owner].tabs;
            assert_eq!(tabs.len(), 1);
            assert_eq!(tabs[0].id(), tab);
            assert_eq!(
                workspace.hosted_tool_in(owner, tool_tabs::Kind::OpenPgp),
                Some(id)
            );
            assert!(workspace.hosted_openpgp_dirty(id));
            assert!(workspace.openpgp[&id].hosted_close.is_none());
            assert!(!workspace.hosted_close_locks(id));
        }
        assert!(!workspace.exiting);
    }

    #[test]
    fn hosted_worker_completion_follows_the_tool_tab_to_its_current_window() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        let _ = workspace.show_openpgp(source);
        let id = workspace
            .hosted_tool_in(source, tool_tabs::Kind::OpenPgp)
            .unwrap();
        let tab = workspace.windows[&source].tabs[0].id();
        let work = workspace.openpgp[&id].work.unwrap();
        assert!(workspace.hosted_openpgp_busy(id));

        let _ = workspace.transfer(source, Some(destination), tab);
        assert_eq!(
            workspace.hosted_tool_in(source, tool_tabs::Kind::OpenPgp),
            None
        );
        assert_eq!(
            workspace.hosted_tool_in(destination, tool_tabs::Kind::OpenPgp),
            Some(id)
        );
        let _ = workspace.update_openpgp(id, Message::Finished(work, Ok(Vec::new())));
        assert!(!workspace.hosted_openpgp_busy(id));
        let _ = workspace.update_openpgp(id, Message::Page(PageMessage::New));
        let _ =
            workspace.update_openpgp(id, Message::Page(PageMessage::Name("Moved draft".into())));
        assert!(workspace.hosted_openpgp_dirty(id));
        assert!(workspace.sessions.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_close_window_closes_the_focused_manager_and_keeps_both_main_windows() {
        use crate::workspace::chrome::Action;

        let mut workspace = Workspace::default();
        let (source, _) = workspace.open_window(false);
        let (newer, _) = workspace.open_window(false);
        let _ = workspace.show_openpgp(source);
        let id = workspace.openpgp_for_source(source).unwrap();
        let work = workspace.openpgp[&id].work.unwrap();
        let _ = workspace.update_openpgp(id, Message::Finished(work, Ok(Vec::new())));
        let _ = workspace.update(Desktop::Window(id, window::Event::Focused));

        let _ = workspace.native_menu(Action::CloseWindow as u16);
        assert!(!workspace.has_standalone_openpgp());
        assert!(workspace.native_focused_openpgp().is_none());
        assert!(workspace.windows.contains_key(&source));
        assert!(workspace.windows.contains_key(&newer));
    }

    #[test]
    fn clean_manager_quit_unblocks_the_unsaved_snippet_confirmation() {
        let mut workspace = Workspace::default();
        let source = window::Id::unique();
        let absent = std::env::current_dir()
            .unwrap()
            .join("target/.scratch")
            .join(format!(
                "kortty-openpgp-window-{}",
                crate::private_files::nonce().unwrap(),
            ))
            .join("snippets.json");
        let mut snippets = SnippetWindow::open(absent.clone()).unwrap();
        snippets.update(snippet_window::Message::Name("Unsaved fixture".into()));
        workspace.snippets.insert(
            source,
            super::super::snippet_access::Surface::new(snippets, source),
        );
        let _ = workspace.show_openpgp(source);
        let id = workspace.openpgp_for_source(source).unwrap();
        let work = workspace.openpgp[&id].work.unwrap();
        let _ = workspace.update_openpgp(id, Message::Finished(work, Ok(Vec::new())));
        assert_eq!(workspace.owner_dialog(source).map(|(id, _)| id), Some(id));

        let _ = workspace.quit();
        assert!(!workspace.has_standalone_openpgp());
        assert!(workspace.owner_dialog(source).is_none());
        assert!(workspace.hosted_close_locks(source));
        assert!(!workspace.exiting);
        assert!(!absent.exists());
    }
}
