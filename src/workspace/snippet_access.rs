use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{borrow::Cow, io};

use iced::{Element, Size, Subscription, Task, window};

use crate::snippet_window::compare;
use crate::snippet_window::{self, SnippetWindow, Update};
use crate::snippets;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message, Workspace, tool_tabs::Kind};

mod analysis;
pub(super) use analysis::Event as AnalysisEvent;
pub(crate) use analysis::SnippetReportAction;
#[cfg(test)]
mod folder_tests;
pub(super) mod recovery;
mod run;
mod transfer;
pub(super) use run::{PaletteTarget, RunMessage, RunPrompt};

pub(super) const TITLE: &str = "Snippet Manager";
const MAX_COMPARISONS: usize = 4;
const STRUCTURE_PREFERENCE_DELAY: Duration = Duration::from_millis(350);

pub(super) struct ComparisonSurface {
    source: window::Id,
    view: compare::View,
}

pub(super) struct Surface {
    pub(super) window: SnippetWindow,
    pub(super) transfer: Option<transfer::State>,
    analysis: analysis::Tabs,
    placement: Placement,
    focused_at: u64,
    close_after_work: bool,
}

enum Placement {
    Native { source: window::Id },
    Hosted,
}

impl Surface {
    pub(super) fn new(window: SnippetWindow, source: window::Id) -> Self {
        Self {
            window,
            transfer: None,
            analysis: analysis::Tabs::default(),
            placement: Placement::Native { source },
            focused_at: 0,
            close_after_work: false,
        }
    }

    pub(super) fn cancel_analysis(&mut self) {
        self.analysis = analysis::Tabs::default();
    }
}

impl Workspace {
    pub(super) fn show_snippets(&mut self, source: window::Id) -> Task<Message> {
        self.show_snippet_manager(source, None)
    }

    pub(super) fn open_snippet_window(&mut self, source: window::Id) -> Task<Message> {
        self.create_snippet_surface(source, None, false)
    }

    fn show_snippet_manager(
        &mut self,
        source: window::Id,
        select: Option<snippets::SnippetId>,
    ) -> Task<Message> {
        if let Some(selected) = select
            && let Some(owner) = self
                .snippets
                .iter()
                .find_map(|(&id, surface)| surface.window.owns_editor(selected).then_some(id))
        {
            let open = self.update_snippets(owner, snippet_window::Message::Open(selected));
            let focus = self
                .focus_hosted_tool(owner)
                .unwrap_or_else(|| window::gain_focus(owner));
            return Task::batch([open, focus]);
        }
        if let Some(id) = self.snippet_manager_for(source) {
            let snippets = &mut self
                .snippets
                .get_mut(&id)
                .expect("registered snippet surface")
                .window;
            let can_open = select.is_some_and(|select| snippets.reveal_saved(select));
            if select.is_some() && !can_open {
                self.notice(
                    source,
                    "Finish the open Snippet Manager dialog before selecting another snippet."
                        .into(),
                );
            }
            let search = if select.is_none() {
                self.focus_snippet_search(id)
            } else {
                Task::none()
            };
            let focused = self
                .focus_hosted_tool(id)
                .unwrap_or_else(|| window::gain_focus(id));
            let open = select
                .filter(|_| can_open)
                .map_or_else(Task::none, |select| {
                    self.update_snippets(id, snippet_window::Message::Open(select))
                });
            return Task::batch([focused, search, open]);
        }
        let hosted =
            self.window_settings.open_tool_windows_as_tabs && self.windows.contains_key(&source);
        self.create_snippet_surface(source, select, hosted)
    }

    fn snippet_manager_for(&self, source: window::Id) -> Option<window::Id> {
        self.snippets
            .iter()
            .filter(|(_, surface)| matches!(surface.placement, Placement::Native { source: owner } if owner == source))
            .max_by_key(|(id, surface)| (surface.focused_at, **id))
            .map(|(&id, _)| id)
            .or_else(|| self.hosted_tool_in(source, Kind::Snippets))
    }

    fn create_snippet_surface(
        &mut self,
        source: window::Id,
        select: Option<snippets::SnippetId>,
        hosted: bool,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let mut snippets = match snippets::profile_path().and_then(SnippetWindow::open) {
            Ok(snippets) => snippets,
            Err(_) => {
                self.notice(source, "Could not open the snippet library".into());
                return Task::none();
            }
        };
        let occupied: Vec<_> = self
            .snippets
            .values()
            .filter(|surface| surface.window.same_library(&snippets))
            .flat_map(|surface| surface.window.tab_session().ids)
            .collect();
        if let Some(saved) = &self.snippet_remembered {
            snippets.restore_tab_state(saved.clone(), &occupied);
        } else {
            snippets.restore_tabs(&occupied);
        }
        if let Some(select) = select {
            snippets.reveal_saved(select);
            if !occupied.contains(&select) {
                snippets.update(snippet_window::Message::Open(select));
            }
        }
        let (id, opened) = self.install_snippet_surface(source, snippets, hosted);
        if select.is_none() {
            opened.chain(self.focus_snippet_search(id))
        } else {
            opened
        }
    }

    fn install_snippet_surface(
        &mut self,
        source: window::Id,
        snippets: SnippetWindow,
        hosted: bool,
    ) -> (window::Id, Task<Message>) {
        let (id, opened) = if hosted {
            let id = window::Id::unique();
            (id, self.host_tool(source, Kind::Snippets, id))
        } else {
            let (id, opened) = self.open_placed_dialog(
                DialogKind::SnippetManager,
                window::Settings {
                    size: Size::new(1280.0, 820.0),
                    min_size: Some(Size::new(620.0, 380.0)),
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
            );
            (id, opened.map(|_| Message::Noop))
        };
        let mut surface = Surface::new(snippets, source);
        surface
            .window
            .apply_structure_preferences(self.window_settings.snippet_structure);
        if hosted {
            surface.placement = Placement::Hosted;
        }
        surface.focused_at = self.allocate();
        self.snippets.insert(id, surface);
        self.remember_snippet_tabs(id);
        self.refresh_snippet_recovery();
        (
            id,
            Task::batch([opened, self.poll_snippet_structure(id, Instant::now())]),
        )
    }

    fn focus_snippet_search(&self, id: window::Id) -> Task<Message> {
        self.snippets.get(&id).map_or_else(Task::none, |surface| {
            surface
                .window
                .focus_search()
                .map(move |message| Message::Snippets(id, message))
        })
    }

    pub(super) fn snippet_window_focused(&mut self, id: window::Id) {
        if self.is_snippet_window(id) {
            let focused_at = self.allocate();
            if let Some(surface) = self.snippets.get_mut(&id) {
                surface.focused_at = focused_at;
            }
        }
    }

    pub(super) fn is_snippet_window(&self, id: window::Id) -> bool {
        self.snippets
            .get(&id)
            .is_some_and(|surface| matches!(surface.placement, Placement::Native { .. }))
    }

    pub(super) fn is_snippet_surface(&self, id: window::Id) -> bool {
        self.snippets.contains_key(&id)
    }

    pub(super) fn native_snippet_close_targets(
        &self,
        owner: Option<window::Id>,
    ) -> Vec<window::Id> {
        let mut targets: Vec<_> = self
            .snippets
            .iter()
            .filter_map(|(&id, surface)| match surface.placement {
                Placement::Native { source } if owner.is_none_or(|owner| owner == source) => {
                    Some((surface.focused_at, id))
                }
                _ => None,
            })
            .collect();
        targets.sort_unstable();
        targets.into_iter().map(|(_, id)| id).collect()
    }

    pub(super) fn native_snippet_modal_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.snippets.iter().find_map(|(&id, surface)| {
            if matches!(surface.placement, Placement::Native { source } if source == owner) {
                self.openpgp_for_source(id)
            } else {
                None
            }
        })
    }

    pub(super) fn snippet_tool_title(&self, id: window::Id) -> Cow<'static, str> {
        Cow::Borrowed(if self.hosted_snippets_dirty(id) {
            "Snippet Manager *"
        } else {
            TITLE
        })
    }

    pub(super) fn snippet_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        if let Some(comparison) = self.snippet_comparisons.get(&id) {
            return Some(
                comparison
                    .view
                    .view()
                    .map(move |message| Message::SnippetComparison(id, message)),
            );
        }
        self.snippets.get(&id).map(|surface| {
            surface
                .window
                .view(
                    self.editor_content_theme(),
                    surface.window.selected_id().is_some_and(|snippet| {
                        self.snippet_editor_elsewhere(id, snippet).is_some()
                    }),
                    snippet_window::transfer::Context {
                        enabled: self.snippet_transfer_enabled(id),
                        page: surface.transfer.as_ref().map(|state| &state.view),
                    },
                    surface.analysis.view(),
                )
                .map(move |message| Message::Snippets(id, message))
        })
    }

    fn open_snippet_comparison(
        &mut self,
        source: window::Id,
        request: compare::Request,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        self.snippet_comparison_workers
            .retain(|done| !done.is_closed());
        if self.snippet_comparisons.len() >= MAX_COMPARISONS
            || self.snippet_comparison_workers.len() >= MAX_COMPARISONS
        {
            let owner = self.hosted_tool_owner(source).unwrap_or(source);
            self.notice(
                owner,
                "Close a snippet comparison before opening another.".into(),
            );
            return Task::none();
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::SnippetComparison,
            window::Settings {
                size: Size::new(1120.0, 720.0),
                min_size: Some(Size::new(580.0, 320.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.snippet_comparisons.insert(
            id,
            ComparisonSurface {
                source,
                view: compare::View::default(),
            },
        );
        let (send, reply) = async_channel::bounded(1);
        let (signal, done) = async_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("kortty-snippet-compare".into())
            .spawn(move || {
                let _signal = signal;
                let _ = send.try_send(request.compare().map(Arc::new));
            });
        if worker.is_ok() {
            self.snippet_comparison_workers.push(done.clone());
            self.track_session_shutdown(done);
        }
        let read = Task::future(async move {
            let result = reply
                .recv()
                .await
                .unwrap_or(Err("The snippet comparison worker stopped."));
            Message::SnippetComparison(id, compare::Message::Loaded(result))
        });
        Task::batch([opened.map(|_| Message::Noop), read])
    }

    pub(super) fn update_snippet_comparison(
        &mut self,
        id: window::Id,
        message: compare::Message,
    ) -> Task<Message> {
        if matches!(message, compare::Message::Close) {
            return self.close_snippet_comparison(id).unwrap_or_else(Task::none);
        }
        if let Some(comparison) = self.snippet_comparisons.get_mut(&id) {
            return comparison
                .view
                .update(message)
                .map(move |message| Message::SnippetComparison(id, message));
        }
        Task::none()
    }

    pub(super) fn close_snippet_comparison(&mut self, id: window::Id) -> Option<Task<Message>> {
        self.snippet_comparisons
            .remove(&id)
            .map(|_| self.close_placed_dialog(id))
    }

    fn close_snippet_comparisons_for(&mut self, source: window::Id) -> Task<Message> {
        let ids: Vec<_> = self
            .snippet_comparisons
            .iter()
            .filter_map(|(&id, window)| (window.source == source).then_some(id))
            .collect();
        Task::batch(
            ids.into_iter()
                .filter_map(|id| self.close_snippet_comparison(id)),
        )
    }

    pub(super) fn update_snippets(
        &mut self,
        id: window::Id,
        message: snippet_window::Message,
    ) -> Task<Message> {
        let message = match message {
            snippet_window::Message::Analyze(subject) => {
                return self.open_snippet_analysis(id, subject);
            }
            snippet_window::Message::Analysis(action) => {
                return self.update_snippet_analysis(id, action);
            }
            snippet_window::Message::AnalysisReports(subject) => {
                return self.open_snippet_analysis_reports(id, subject);
            }
            message => message,
        };
        if let snippet_window::Message::Transfer(action) = message {
            if self
                .snippets
                .get(&id)
                .is_some_and(|surface| surface.analysis.export_open())
            {
                return Task::none();
            }
            return self.update_snippet_transfer(id, action);
        }
        if self
            .snippets
            .get(&id)
            .is_some_and(|surface| surface.analysis.export_open())
            && !message.is_async_completion()
            && !matches!(
                message,
                snippet_window::Message::Close
                    | snippet_window::Message::HostedClose(..)
                    | snippet_window::Message::HostedSave(_)
                    | snippet_window::Message::SaveBeforeClose
                    | snippet_window::Message::Cancel
                    | snippet_window::Message::Confirm
            )
        {
            return Task::none();
        }
        if self
            .snippets
            .get(&id)
            .is_some_and(|surface| surface.transfer.is_some())
            && !message.is_async_completion()
            && !matches!(
                message,
                snippet_window::Message::Close
                    | snippet_window::Message::HostedClose(..)
                    | snippet_window::Message::HostedSave(_)
                    | snippet_window::Message::SaveBeforeClose
            )
        {
            return Task::none();
        }
        if let snippet_window::Message::Recovery(action) = message {
            return self.update_snippet_recovery_action(id, action);
        }
        if matches!(message, snippet_window::Message::Close) {
            return self.request_snippet_close(id);
        }
        if let Some(snippet) = self
            .snippets
            .get(&id)
            .and_then(|surface| surface.window.edit_target(&message))
            && let Some(owner) = self.snippet_editor_elsewhere(id, snippet)
        {
            if let Some(surface) = self.snippets.get_mut(&owner) {
                surface
                    .window
                    .update(snippet_window::Message::Open(snippet));
            }
            return self
                .focus_hosted_tool(owner)
                .unwrap_or_else(|| window::gain_focus(owner));
        }
        let locked = self.hosted_close_locks(id);
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if matches!(
            message,
            snippet_window::Message::Select(_)
                | snippet_window::Message::Open(_)
                | snippet_window::Message::SelectTab(_)
                | snippet_window::Message::New
        ) {
            surface.analysis.editor();
        }
        if locked {
            surface.close_after_work = false;
        }
        let snippets = &mut surface.window;
        let possible_removal = matches!(
            message,
            snippet_window::Message::Confirm
                | snippet_window::Message::Folders(snippet_window::folders::Message::KeepContents)
        )
        .then(|| analysis::subjects(snippets));
        if locked && snippets.hosted_close().is_none() && !message.is_async_completion() {
            return Task::none();
        }
        let update = match message {
            snippet_window::Message::HostedClose(token, approve)
                if snippets.hosted_close() == Some(token) && !snippets.busy() =>
            {
                if approve {
                    snippets.set_hosted_close(None);
                }
                return self.hosted_tool_close_answer(id, token, approve);
            }
            snippet_window::Message::HostedSave(token)
                if snippets.hosted_close() == Some(token) =>
            {
                if !snippets.save_before_close() {
                    self.snippet_library_changed(id);
                    self.remember_snippet_tabs(id);
                    return Task::none();
                }
                snippets.set_hosted_close(None);
                self.snippet_library_changed(id);
                self.remember_snippet_tabs(id);
                return self.hosted_tool_close_answer(id, token, true);
            }
            snippet_window::Message::SaveBeforeClose
                if snippets.hosted_close().is_none() && snippets.native_close_prompt() =>
            {
                if !snippets.save_before_close() {
                    self.snippet_library_changed(id);
                    self.remember_snippet_tabs(id);
                    return Task::none();
                }
                self.snippet_library_changed(id);
                self.remember_snippet_tabs(id);
                return self.close_snippets(id);
            }
            message => snippets.update(message),
        };
        if !matches!(update, Update::Changed | Update::DeleteFolder(..)) {
            snippets.apply_pending_library();
        }
        let task = match update {
            Update::DeleteFolder(folder, snippets) => {
                return self.delete_snippet_folder(id, folder, &snippets);
            }
            Update::FocusEditor => self.snippets[&id]
                .window
                .focus_editor()
                .map(move |message| Message::Snippets(id, message)),
            Update::FocusFolderName(input) => iced::widget::operation::focus(input.clone())
                .chain(iced::widget::operation::select_all(input)),
            Update::Changed => {
                self.snippet_library_changed(id);
                possible_removal.map_or_else(Task::none, |before| {
                    self.cleanup_snippet_analyses(id, before)
                })
            }
            Update::CloseConfirmed => {
                self.remember_snippet_tabs(id);
                return self.close_snippets(id);
            }
            Update::MoveEditor { tab, snippet } => {
                return self.move_snippet_editor(id, tab, snippet);
            }
            Update::RevealLibrary => self.snippets[&id]
                .window
                .reveal_library_selection()
                .map(move |message| Message::Snippets(id, message)),
            Update::ReadClipboard(token) => self.read_clipboard().map(move |value| {
                let message = snippet_window::copy::Message::ClipboardLoaded(
                    token,
                    value.map(zeroize::Zeroizing::new),
                );
                Message::Snippets(id, snippet_window::Message::Copy(message))
            }),
            Update::CopyResolved(value) => self.write_clipboard(value),
            Update::Diagram {
                source,
                render_locally,
            } => self.open_diagram(Some((&source, render_locally))),
            Update::Compare(request) => self.open_snippet_comparison(id, request),
            Update::ExchangeVariables(action) => file_task(
                move || action.run(),
                move |result| {
                    Message::Snippets(
                        id,
                        snippet_window::Message::Variables(
                            snippet_window::variables::Message::ExchangeFinished(result),
                        ),
                    )
                },
            ),
            Update::ExchangeArchive(action) => file_task(
                move || action.run(),
                move |result| {
                    Message::Snippets(
                        id,
                        snippet_window::Message::Archive(
                            snippet_window::archive::Message::Finished(result),
                        ),
                    )
                },
            ),
            Update::OpenRecipients => self.show_openpgp(id),
            Update::LoadExportRecipients(action) => file_task(
                move || action.run(),
                move |result| {
                    Message::Snippets(id, snippet_window::Message::ExportRecipients(result))
                },
            ),
            Update::None | Update::CloseCancelled => Task::none(),
        };
        let resume = if self.hosted_close_locks(id) {
            if self.hosted_snippets_busy(id) {
                Task::none()
            } else {
                self.resume_hosted_tool_close(id)
            }
        } else if self
            .snippets
            .get(&id)
            .is_some_and(|surface| surface.close_after_work && !surface.window.busy())
        {
            self.request_snippet_close(id)
        } else {
            Task::none()
        };
        self.remember_snippet_tabs(id);
        self.refresh_snippet_recovery();
        self.remember_snippet_structure(id);
        Task::batch([
            task,
            resume,
            self.poll_snippet_structure(id, Instant::now()),
        ])
    }

    fn delete_snippet_folder(
        &mut self,
        source: window::Id,
        folder: snippets::folders::Id,
        expected: &[snippets::SnippetId],
    ) -> Task<Message> {
        let Some(origin) = self.snippets.get(&source) else {
            return Task::none();
        };
        let blocker = self
            .snippets
            .iter()
            .filter(|(_, surface)| surface.window.same_library(&origin.window))
            .find_map(|(&id, surface)| {
                surface
                    .window
                    .folder_delete_blocker(expected)
                    .map(|snippet| (id, snippet))
            });
        if let Some((owner, snippet)) = blocker {
            if let Some(surface) = self.snippets.get_mut(&owner) {
                surface.window.folder_delete_blocked(snippet);
            }
            return self
                .focus_hosted_tool(owner)
                .unwrap_or_else(|| window::gain_focus(owner));
        }
        let Some(surface) = self.snippets.get_mut(&source) else {
            return Task::none();
        };
        let before = analysis::subjects(&surface.window);
        let mut cleanup = Task::none();
        if surface.window.remove_snippet_folder(folder, expected) == Update::Changed {
            self.snippet_library_changed(source);
            cleanup = self.cleanup_snippet_analyses(source, before);
        } else {
            surface.window.apply_pending_library();
        }
        self.remember_snippet_tabs(source);
        self.refresh_snippet_recovery();
        Task::batch([cleanup, self.poll_snippet_structure(source, Instant::now())])
    }

    fn remember_snippet_structure(&mut self, id: window::Id) {
        let Some(surface) = self.snippets.get(&id) else {
            return;
        };
        let preferences = surface.window.structure_preferences();
        if self.window_settings.snippet_structure == preferences {
            return;
        }
        self.window_settings.snippet_structure = preferences;
        for surface in self.snippets.values_mut() {
            surface.window.apply_structure_preferences(preferences);
        }
        self.snippet_structure_save_at = Some(Instant::now() + STRUCTURE_PREFERENCE_DELAY);
    }

    pub(super) fn flush_snippet_structure_preferences(&mut self) {
        if self.snippet_structure_save_at.take().is_some() {
            self.save_window_settings(None);
        }
    }

    fn snippet_structure_surface_visible(&self, id: window::Id) -> bool {
        self.is_snippet_window(id)
            || self
                .hosted_tool_owner(id)
                .is_some_and(|owner| self.focused_snippet_surface(owner) == Some(id))
    }

    pub(super) fn snippet_structure_subscription(&self) -> Subscription<Message> {
        let pending = self.snippet_structure_save_at.is_some()
            || self.snippets.iter().any(|(&id, surface)| {
                surface.window.structure_needs_tick() && self.snippet_structure_surface_visible(id)
            });
        if pending && !self.exiting && !self.scheduler_draining {
            super::scheduler_access::desktop_clock(Duration::from_millis(50))
                .map(|()| Message::SnippetStructureTick)
        } else {
            Subscription::none()
        }
    }

    pub(super) fn poll_snippet_structures(&mut self, now: Instant) -> Task<Message> {
        if self.snippet_structure_save_at.is_some_and(|due| now >= due) {
            self.flush_snippet_structure_preferences();
        }
        let ids: Vec<_> = self.snippets.keys().copied().collect();
        Task::batch(
            ids.into_iter()
                .map(|id| self.poll_snippet_structure(id, now)),
        )
    }

    fn poll_snippet_structure(&mut self, id: window::Id, now: Instant) -> Task<Message> {
        if !self.snippet_structure_surface_visible(id) {
            return Task::none();
        }
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        surface
            .window
            .apply_structure_preferences(self.window_settings.snippet_structure);
        surface
            .window
            .poll_structure(now)
            .map(move |message| Message::Snippets(id, message))
    }

    fn remember_snippet_tabs(&mut self, id: window::Id) {
        let Some(surface) = self.snippets.get(&id) else {
            return;
        };
        let state = surface.window.tab_session();
        if self.snippet_remembered.as_ref() != Some(&state) {
            if let Some(writer) = &self.snippet_session {
                writer.write(state.clone());
            }
            self.snippet_remembered = Some(state);
        }
    }

    fn move_snippet_editor(
        &mut self,
        from: window::Id,
        tab: snippet_window::tabs::Id,
        snippet: snippets::SnippetId,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get(&from) else {
            return Task::none();
        };
        let source = match surface.placement {
            Placement::Native { source } => Some(source),
            Placement::Hosted => self.hosted_tool_owner(from),
        };
        let Some(source) = source.filter(|id| self.windows.contains_key(id)) else {
            return Task::none();
        };
        let target = match surface.window.open_sibling(snippet) {
            Ok(target) => target,
            Err(error) => {
                if let Some(surface) = self.snippets.get_mut(&from) {
                    surface.window.move_failed(&error);
                }
                return Task::none();
            }
        };
        self.snippet_library_changed(from);
        if let Some(surface) = self.snippets.get_mut(&from) {
            surface.window.finish_move(tab);
        }
        self.remember_snippet_tabs(from);
        let (_, opened) = self.install_snippet_surface(source, target, false);
        opened
    }

    fn snippet_library_changed(&mut self, id: window::Id) {
        let Some(surface) = self.snippets.get_mut(&id) else {
            return;
        };
        let library = surface.window.shared_library();
        for (&peer, surface) in &mut self.snippets {
            if peer != id {
                surface.window.refresh_shared(library.clone());
            }
        }
        if let Some(scheduler) = self.scheduler.as_mut() {
            scheduler.refresh_snippets();
        }
        self.refresh_snippet_recovery();
    }

    fn snippet_editor_elsewhere(
        &self,
        origin: window::Id,
        snippet: snippets::SnippetId,
    ) -> Option<window::Id> {
        let source = &self.snippets.get(&origin)?.window;
        self.snippets.iter().find_map(|(&id, surface)| {
            (id != origin
                && surface.window.same_library(source)
                && surface.window.owns_editor(snippet))
            .then_some(id)
        })
    }

    pub(super) fn snippet_shortcut(
        &mut self,
        id: window::Id,
        key: &iced::keyboard::Key,
        modifiers: iced::keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        use iced::keyboard::{Key, key::Named};
        let surface = self.focused_snippet_surface(id)?;
        let message = if modifiers.control() && !modifiers.alt() && *key == Key::Named(Named::Tab) {
            snippet_window::Message::CycleTab(modifiers.shift())
        } else if modifiers.command() && !modifiers.alt() && !modifiers.shift() {
            match key {
                Key::Character(value) if value.eq_ignore_ascii_case("w") => {
                    snippet_window::Message::CloseActiveTab
                }
                Key::Character(value) if value.eq_ignore_ascii_case("s") => {
                    snippet_window::Message::Save
                }
                _ => return None,
            }
        } else if modifiers.is_empty() {
            self.snippets.get(&surface)?.window.library_key(key)?
        } else {
            return None;
        };
        Some(self.update_snippets(surface, message))
    }

    pub(super) fn focused_snippet_surface(&self, id: window::Id) -> Option<window::Id> {
        if self.is_snippet_window(id) {
            return Some(id);
        }
        let tab = self.windows.get(&id)?.active_tab_id()?;
        let tool = self.hosted_tab_at(id, tab)?;
        (tool.kind == Kind::Snippets).then_some(tool.instance)
    }

    pub(super) fn request_snippet_close(&mut self, id: window::Id) -> Task<Message> {
        if let Some(dialog) = self.openpgp_for_source(id) {
            return window::gain_focus(dialog);
        }
        if self.hosted_tool_owner(id).is_some() {
            return self.request_hosted_tool_close(id);
        }
        if self.hosted_close_locks(id) {
            return if self.hosted_snippets_busy(id) {
                window::gain_focus(id)
            } else {
                self.resume_hosted_tool_close(id)
            };
        }
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if let Some(transfer) = &surface.transfer
            && transfer.active()
        {
            transfer.cancel();
            surface.close_after_work = true;
            return Task::none();
        }
        surface.transfer = None;
        surface.close_after_work = surface.window.busy();
        if surface.window.request_close() {
            self.close_snippets(id)
        } else {
            Task::none()
        }
    }

    fn close_snippets(&mut self, id: window::Id) -> Task<Message> {
        if !self.is_snippet_window(id) || self.hosted_close_locks(id) {
            return Task::none();
        }
        let closed = self.close_native_snippet_after_preflight(id);
        if self.should_exit_after_window_close() {
            Task::batch([closed, self.quit()])
        } else {
            closed
        }
    }

    pub(super) fn close_native_snippet_after_preflight(&mut self, id: window::Id) -> Task<Message> {
        if !self.is_snippet_window(id) {
            return Task::none();
        }
        if let Some(surface) = self.snippets.get_mut(&id) {
            surface.window.discard_owned_recovery();
        }
        self.flush_snippet_recovery();
        self.snippets.remove(&id);
        Task::batch([
            self.close_snippet_comparisons_for(id),
            self.close_placed_dialog(id),
        ])
    }

    pub(super) fn dismiss_snippets(&mut self, id: window::Id) -> Task<Message> {
        let Some(surface) = self.snippets.get(&id) else {
            return Task::none();
        };
        if surface.window.hosted_close().is_none()
            && let Some(action) = surface.analysis.escape()
        {
            return self.update_snippet_analysis(id, action);
        }
        if let Some(transfer) = &surface.transfer
            && surface.window.hosted_close().is_none()
        {
            let action = snippet_window::transfer::escape(&transfer.view);
            return self.update_snippet_transfer(id, action);
        }
        let mut message = surface.window.escape();
        if matches!(surface.placement, Placement::Native { .. })
            && matches!(message, snippet_window::Message::Close)
        {
            message = snippet_window::Message::Cancel;
        }
        self.update_snippets(id, message)
    }

    pub(super) fn hosted_snippets_busy(&self, id: window::Id) -> bool {
        self.snippets.get(&id).is_some_and(|surface| {
            surface.window.busy()
                || surface
                    .transfer
                    .as_ref()
                    .is_some_and(transfer::State::active)
        })
    }

    pub(super) fn hosted_snippets_dirty(&self, id: window::Id) -> bool {
        self.snippets
            .get(&id)
            .is_some_and(|surface| surface.window.dirty())
    }

    pub(super) fn prompt_hosted_snippets_close(&mut self, id: window::Id, token: u64) {
        if let Some(surface) = self.snippets.get_mut(&id) {
            surface.close_after_work = false;
            if let Some(transfer) = &surface.transfer {
                if transfer.active() {
                    transfer.cancel();
                } else {
                    surface.transfer = None;
                }
            }
            surface.window.set_hosted_close(Some(token));
        }
    }

    pub(super) fn cancel_hosted_snippets_close(&mut self, id: window::Id) {
        if let Some(surface) = self.snippets.get_mut(&id) {
            surface.window.set_hosted_close(None);
        }
    }

    pub(super) fn remove_hosted_snippets(&mut self, id: window::Id) -> Task<Message> {
        if let Some(surface) = self.snippets.get_mut(&id) {
            surface.window.discard_owned_recovery();
        }
        self.flush_snippet_recovery();
        self.snippets.remove(&id);
        self.close_snippet_comparisons_for(id)
    }
}

fn file_task<T: Send + 'static>(
    run: impl FnOnce() -> io::Result<T> + Send + 'static,
    finished: impl Fn(Result<T, String>) -> Message + Send + 'static,
) -> Task<Message> {
    Task::perform(
        async move {
            let (send, receive) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name("kortty-snippet-exchange".into())
                .spawn(move || {
                    let _ = send.send_blocking(run().map_err(|error| error.to_string()));
                })
                .map_err(|error| error.to_string())?;
            receive
                .recv()
                .await
                .map_err(|_| String::from("Snippet exchange worker stopped"))?
        },
        finished,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_manager_preserves_the_last_saved_editor_session() {
        let profile = snippets::tests::Profile::new();
        let mut library = snippets::Library::open(profile.path()).unwrap();
        let mut save = |name: &str| {
            library
                .save(
                    None,
                    snippets::Input {
                        name: name.into(),
                        language: "plain".into(),
                        category: String::new(),
                        content: name.into(),
                    },
                )
                .unwrap()
        };
        let first = save("First");
        let second = save("Second");
        let path = profile
            .path()
            .with_file_name(snippet_window::session::FILE_NAME);
        let mut workspace = Workspace {
            snippet_session: Some(snippet_window::session::Writer::start(path.clone()).unwrap()),
            ..Workspace::default()
        };
        let (owner, _) = workspace.open_window(false);
        let (manager, _) = workspace.install_snippet_surface(
            owner,
            SnippetWindow::open(profile.path()).unwrap(),
            false,
        );
        for id in [second, first, second] {
            let _ = workspace.update_snippets(manager, snippet_window::Message::Open(id));
        }
        let _ = workspace.close_native_snippet_after_preflight(manager);
        workspace
            .snippet_session
            .take()
            .unwrap()
            .finish()
            .recv_blocking()
            .unwrap();
        assert_eq!(
            snippet_window::session::load(&path).unwrap(),
            snippet_window::session::State {
                ids: vec![second, first],
                active: Some(second)
            }
        );
        let mut reopened = SnippetWindow::open(profile.path()).unwrap();
        reopened.restore_tabs(&[]);
        assert_eq!(reopened.selected_id(), Some(second));
        assert_eq!(reopened.tab_session().ids, vec![second, first]);
    }

    #[test]
    fn a_partly_failed_save_all_remembers_newly_saved_editor_ids() {
        use iced::widget::text_editor::{Action, Edit};
        let profile = snippets::tests::Profile::new();
        let path = profile
            .path()
            .with_file_name(snippet_window::session::FILE_NAME);
        let mut workspace = Workspace {
            snippet_session: Some(snippet_window::session::Writer::start(path.clone()).unwrap()),
            ..Workspace::default()
        };
        let (owner, _) = workspace.open_window(false);
        let (manager, _) = workspace.install_snippet_surface(
            owner,
            SnippetWindow::open(profile.path()).unwrap(),
            false,
        );
        let messages = [
            snippet_window::Message::Name("Saved before failure".into()),
            snippet_window::Message::Edit(Action::Edit(Edit::Insert('A'))),
            snippet_window::Message::New,
            snippet_window::Message::Edit(Action::Edit(Edit::Insert('B'))),
            snippet_window::Message::CycleTab(true),
        ];
        for message in messages {
            let _ = workspace.update_snippets(manager, message);
        }
        let _ = workspace.request_snippet_close(manager);
        let _ = workspace.update_snippets(manager, snippet_window::Message::SaveBeforeClose);
        assert!(workspace.is_snippet_window(manager));
        assert!(workspace.snippets[&manager].window.dirty());
        let library = snippets::Library::open(profile.path()).unwrap();
        let saved = library
            .entries()
            .iter()
            .find(|snippet| snippet.name() == "Saved before failure")
            .unwrap()
            .id();
        assert_eq!(
            workspace.snippet_remembered.as_ref().unwrap().ids,
            vec![saved]
        );
        let _ = workspace.update_snippets(manager, snippet_window::Message::Cancel);
        let _ = workspace.request_snippet_close(manager);
        let _ = workspace.update_snippets(manager, snippet_window::Message::Confirm);
        workspace
            .snippet_session
            .take()
            .unwrap()
            .finish()
            .recv_blocking()
            .unwrap();
        assert_eq!(
            snippet_window::session::load(&path).unwrap().ids,
            vec![saved]
        );
    }

    #[test]
    fn moving_saved_snippets_respects_clean_and_dirty_close_choices_and_owner() {
        use iced::widget::text_editor::{Action, Edit};

        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        enum Choice {
            Clean,
            Cancel,
            Discard,
            Save,
        }

        for hosted in [false, true] {
            for choice in [Choice::Clean, Choice::Cancel, Choice::Discard, Choice::Save] {
                let profile = snippets::tests::Profile::new();
                let mut library = snippets::Library::open(profile.path()).unwrap();
                let saved = library
                    .save(
                        None,
                        snippets::Input {
                            name: "Original name".into(),
                            language: "plain".into(),
                            category: String::new(),
                            content: "original content".into(),
                        },
                    )
                    .unwrap();
                let mut workspace = Workspace::default();
                let (owner, _) = workspace.open_window(false);
                let (other, _) = workspace.open_window(false);
                workspace.add_tab(owner, crate::session::SessionId(42));
                workspace.add_tab(other, crate::session::SessionId(43));
                let source = workspace
                    .install_snippet_surface(
                        owner,
                        SnippetWindow::open(profile.path()).unwrap(),
                        hosted,
                    )
                    .0;
                let _ = workspace.update(Message::Snippets(
                    source,
                    snippet_window::Message::Open(saved),
                ));
                if choice != Choice::Clean {
                    let _ = workspace.update(Message::Snippets(
                        source,
                        snippet_window::Message::Name("Draft name".into()),
                    ));
                    let _ = workspace.update(Message::Snippets(
                        source,
                        snippet_window::Message::Edit(Action::Edit(Edit::Insert('!'))),
                    ));
                }
                let snippet_window::Message::CloseTab(tab) =
                    workspace.snippets[&source].window.close_active_tab()
                else {
                    panic!("expected the active inner tab");
                };
                let _ = workspace.update(Message::Snippets(
                    source,
                    snippet_window::Message::MoveTab(tab),
                ));
                if choice != Choice::Clean {
                    assert_eq!(workspace.snippets.len(), 1, "{choice:?}");
                    assert!(workspace.snippets[&source].window.owns_editor(saved));
                    assert!(workspace.hosted_snippets_dirty(source));
                    let answer = match choice {
                        Choice::Cancel => snippet_window::Message::Cancel,
                        Choice::Discard => snippet_window::Message::Confirm,
                        Choice::Save => snippet_window::Message::SaveTabBeforeClose,
                        Choice::Clean => unreachable!(),
                    };
                    let _ = workspace.update(Message::Snippets(source, answer));
                }
                if choice == Choice::Cancel {
                    assert_eq!(workspace.snippets.len(), 1);
                    assert!(workspace.snippets[&source].window.owns_editor(saved));
                    assert!(workspace.hosted_snippets_dirty(source));
                    let _ =
                        workspace.update(Message::Snippets(source, snippet_window::Message::Save));
                    assert_eq!(workspace.snippets.len(), 1);
                    let stored = snippets::Library::open(profile.path()).unwrap();
                    assert_eq!(stored.get(saved).unwrap().name(), "Draft name");
                    assert_eq!(stored.get(saved).unwrap().content(), "!original content");
                    continue;
                }

                assert_eq!(workspace.snippets.len(), 2, "{choice:?}");
                assert!(workspace.is_snippet_surface(source));
                assert!(!workspace.snippets[&source].window.owns_editor(saved));
                let (&target, destination) = workspace
                    .snippets
                    .iter()
                    .find(|(id, _)| **id != source)
                    .unwrap();
                assert!(matches!(
                    destination.placement,
                    Placement::Native { source: target_owner } if target_owner == owner
                ));
                assert!(workspace.is_snippet_window(target));
                assert_eq!(destination.window.selected_id(), Some(saved));
                assert!(destination.window.owns_editor(saved));
                assert!(!destination.window.dirty());
                assert!(
                    workspace
                        .native_snippet_close_targets(Some(other))
                        .is_empty()
                );
                assert_eq!(workspace.windows[&other].tabs.len(), 1);
                assert_eq!(
                    workspace.windows[&owner].tabs.len(),
                    if hosted { 2 } else { 1 }
                );
                let stored = snippets::Library::open(profile.path()).unwrap();
                let (name, content) = if choice == Choice::Save {
                    ("Draft name", "!original content")
                } else {
                    ("Original name", "original content")
                };
                assert_eq!(stored.get(saved).unwrap().name(), name);
                assert_eq!(stored.get(saved).unwrap().content(), content);
            }
        }
    }

    #[test]
    fn failed_move_keeps_the_source_editor_and_can_retry_after_profile_repair() {
        for dirty in [false, true] {
            let profile = snippets::tests::Profile::new();
            let mut library = snippets::Library::open(profile.path()).unwrap();
            let saved = library
                .save(
                    None,
                    snippets::Input {
                        name: "Original name".into(),
                        language: "plain".into(),
                        category: String::new(),
                        content: "original content".into(),
                    },
                )
                .unwrap();
            let before = std::fs::read(profile.path()).unwrap();
            let mut workspace = Workspace::default();
            let (owner, _) = workspace.open_window(false);
            let source = workspace
                .install_snippet_surface(owner, SnippetWindow::open(profile.path()).unwrap(), false)
                .0;
            let _ = workspace.update(Message::Snippets(
                source,
                snippet_window::Message::Open(saved),
            ));
            if dirty {
                let _ = workspace.update(Message::Snippets(
                    source,
                    snippet_window::Message::Name("Draft survives failed move".into()),
                ));
            }
            let snippet_window::Message::CloseTab(tab) =
                workspace.snippets[&source].window.close_active_tab()
            else {
                panic!("expected the active inner tab");
            };
            // Real malformed storage makes the destination's normal library open fail.
            std::fs::write(profile.path(), b"{invalid library").unwrap();
            let _ = workspace.update(Message::Snippets(
                source,
                snippet_window::Message::MoveTab(tab),
            ));
            if dirty {
                let _ =
                    workspace.update(Message::Snippets(source, snippet_window::Message::Confirm));
            }
            assert_eq!(workspace.snippets.len(), 1);
            assert!(workspace.is_snippet_window(source));
            assert_eq!(
                workspace.snippets[&source].window.selected_id(),
                Some(saved)
            );
            assert!(workspace.snippets[&source].window.owns_editor(saved));
            assert_eq!(workspace.hosted_snippets_dirty(source), dirty);
            assert_eq!(std::fs::read(profile.path()).unwrap(), b"{invalid library");

            std::fs::write(profile.path(), before).unwrap();
            let _ = workspace.update(Message::Snippets(
                source,
                snippet_window::Message::MoveTab(tab),
            ));
            if dirty {
                let _ = workspace.update(Message::Snippets(
                    source,
                    snippet_window::Message::SaveTabBeforeClose,
                ));
            }
            assert_eq!(workspace.snippets.len(), 2);
            assert!(!workspace.snippets[&source].window.owns_editor(saved));
            let destination = workspace
                .snippets
                .iter()
                .find(|(id, _)| **id != source)
                .unwrap()
                .1;
            assert!(matches!(
                destination.placement,
                Placement::Native { source: target_owner } if target_owner == owner
            ));
            assert!(destination.window.owns_editor(saved));
            let stored = snippets::Library::open(profile.path()).unwrap();
            assert_eq!(
                stored.get(saved).unwrap().name(),
                if dirty {
                    "Draft survives failed move"
                } else {
                    "Original name"
                }
            );
        }
    }

    #[test]
    fn saved_snippet_has_one_editor_across_native_and_hosted_workspaces() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced::widget::text_editor::{Action, Edit};

        for editor_hosted in [false, true] {
            let profile = snippets::tests::Profile::new();
            let mut library = snippets::Library::open(profile.path()).unwrap();
            let mut save = |name: &str| {
                library
                    .save(
                        None,
                        snippets::Input {
                            name: name.into(),
                            language: "plain".into(),
                            category: String::new(),
                            content: "original text".into(),
                        },
                    )
                    .unwrap()
            };
            let saved = save("Shared snippet");
            let other = save("Other snippet");
            let mut workspace = Workspace::default();
            let (editor_owner, _) = workspace.open_window(false);
            let (browser_owner, _) = workspace.open_window(false);
            workspace.add_tab(editor_owner, crate::session::SessionId(42));
            let terminal_tab = workspace.windows[&editor_owner].active_tab_id().unwrap();
            let editor = workspace
                .install_snippet_surface(
                    editor_owner,
                    SnippetWindow::open(profile.path()).unwrap(),
                    editor_hosted,
                )
                .0;
            let browser = workspace
                .install_snippet_surface(
                    browser_owner,
                    SnippetWindow::open(profile.path()).unwrap(),
                    !editor_hosted,
                )
                .0;
            let _ = workspace.update(Message::Snippets(
                editor,
                snippet_window::Message::Open(saved),
            ));
            let _ = workspace.update(Message::Snippets(
                editor,
                snippet_window::Message::Name("Edited by original owner".into()),
            ));
            let _ = workspace.update(Message::Snippets(
                browser,
                snippet_window::Message::Select(saved),
            ));
            assert_eq!(
                workspace.snippets[&browser].window.selected_id(),
                Some(saved)
            );
            assert!(!workspace.snippets[&browser].window.owns_editor(saved));
            assert!(!workspace.hosted_snippets_dirty(browser));

            for attempt in [
                snippet_window::Message::Name("Must not overwrite the owner".into()),
                snippet_window::Message::Edit(Action::Edit(Edit::Insert('!'))),
                snippet_window::Message::Open(saved),
            ] {
                let _ = workspace.update(Message::Snippets(
                    editor,
                    snippet_window::Message::Select(other),
                ));
                if editor_hosted {
                    let _ = workspace.update(Message::SelectTab(editor_owner, terminal_tab));
                }
                let task = workspace.update(Message::Snippets(browser, attempt));
                let mut actions = iced_runtime::task::into_stream(task).unwrap();
                let action = block_on(actions.next());
                let expected_window = if editor_hosted { editor_owner } else { editor };
                assert!(matches!(
                    action,
                    Some(iced_runtime::Action::Window(
                        iced_runtime::window::Action::GainFocus(id)
                    )) if id == expected_window
                ));
                assert_eq!(
                    workspace.snippets[&editor].window.selected_id(),
                    Some(saved)
                );
                assert!(workspace.snippets[&editor].window.owns_editor(saved));
                assert!(workspace.hosted_snippets_dirty(editor));
                assert_eq!(
                    workspace.snippets[&browser].window.selected_id(),
                    Some(saved)
                );
                assert!(!workspace.snippets[&browser].window.owns_editor(saved));
                assert!(!workspace.hosted_snippets_dirty(browser));
                if editor_hosted {
                    assert_eq!(
                        workspace.hosted_tool_in(editor_owner, Kind::Snippets),
                        Some(editor)
                    );
                    let active = workspace.windows[&editor_owner].active_tab_id().unwrap();
                    assert_eq!(
                        workspace
                            .hosted_tab_at(editor_owner, active)
                            .unwrap()
                            .instance,
                        editor
                    );
                }
            }

            let _ = workspace.update(Message::Snippets(editor, snippet_window::Message::Save));
            let stored = snippets::Library::open(profile.path()).unwrap();
            assert_eq!(
                stored.get(saved).unwrap().name(),
                "Edited by original owner"
            );
            assert_eq!(stored.get(saved).unwrap().content(), "original text");
            let _ = workspace.update(Message::Snippets(
                editor,
                snippet_window::Message::CloseActiveTab,
            ));
            assert!(!workspace.snippets[&editor].window.owns_editor(saved));

            let _ = workspace.update(Message::Snippets(
                browser,
                snippet_window::Message::Name("Edited by new owner".into()),
            ));
            let _ = workspace.update(Message::Snippets(
                browser,
                snippet_window::Message::Edit(Action::Edit(Edit::Insert('!'))),
            ));
            assert!(workspace.snippets[&browser].window.owns_editor(saved));
            assert!(workspace.hosted_snippets_dirty(browser));
            let _ = workspace.update(Message::Snippets(browser, snippet_window::Message::Save));
            let stored = snippets::Library::open(profile.path()).unwrap();
            assert_eq!(stored.get(saved).unwrap().name(), "Edited by new owner");
            assert_eq!(stored.get(saved).unwrap().content(), "!original text");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_close_tab_closes_only_the_focused_snippet_inner_tab() {
        use crate::session::SessionId;
        use crate::workspace::chrome::Action::CloseTab;

        let profile = snippets::tests::Profile::new();
        let mut library = snippets::Library::open(profile.path()).unwrap();
        let saved = library
            .save(
                None,
                snippets::Input {
                    name: "Snippet to close".into(),
                    language: "plain".into(),
                    category: String::new(),
                    content: "keep stored content".into(),
                },
            )
            .unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        workspace.add_tab(owner, SessionId(42));
        workspace.add_tab(other, SessionId(43));
        let manager = workspace
            .install_snippet_surface(owner, SnippetWindow::open(profile.path()).unwrap(), false)
            .0;
        let _ = workspace.update(Message::Snippets(
            manager,
            snippet_window::Message::Open(saved),
        ));
        assert!(workspace.snippets[&manager].window.owns_editor(saved));
        let _ = workspace.update(Message::Window(manager, window::Event::Focused));

        let _ = workspace.native_menu(CloseTab as u16);
        assert!(workspace.is_snippet_window(manager));
        assert!(!workspace.snippets[&manager].window.owns_editor(saved));
        assert_eq!(workspace.snippets[&manager].window.selected_id(), None);
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);
        assert_eq!(workspace.windows[&other].tabs.len(), 1);
        assert_eq!(
            snippets::Library::open(profile.path())
                .unwrap()
                .get(saved)
                .unwrap()
                .content(),
            "keep stored content"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_close_window_targets_the_focused_snippet_comparison() {
        use crate::session::SessionId;
        use crate::workspace::chrome::Action::{CloseTab, CloseWindow};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        workspace.add_tab(owner, SessionId(42));
        workspace.add_tab(other, SessionId(43));
        let comparison = window::Id::unique();
        // Test double for an already opened native comparison surface.
        workspace.snippet_comparisons.insert(
            comparison,
            ComparisonSurface {
                source: window::Id::unique(),
                view: compare::View::default(),
            },
        );
        let _ = workspace.update(Message::Window(comparison, window::Event::Focused));

        let _ = workspace.native_menu(CloseTab as u16);
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);
        assert_eq!(workspace.windows[&other].tabs.len(), 1);
        assert!(workspace.snippet_comparisons.contains_key(&comparison));

        let _ = workspace.native_menu(CloseWindow as u16);
        assert!(!workspace.snippet_comparisons.contains_key(&comparison));
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);
        assert_eq!(workspace.windows[&other].tabs.len(), 1);
    }

    #[test]
    fn native_snippet_manager_reuse_is_per_owner_and_most_recently_focused() {
        let profile = snippets::tests::Profile::new();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let install = |workspace: &mut Workspace, source, hosted| {
            workspace
                .install_snippet_surface(
                    source,
                    SnippetWindow::open(profile.path()).unwrap(),
                    hosted,
                )
                .0
        };
        let tab = install(&mut workspace, owner, true);
        assert_eq!(workspace.snippet_manager_for(owner), Some(tab));
        assert!(workspace.is_snippet_surface(tab));
        assert!(!workspace.is_snippet_window(tab));

        let first = install(&mut workspace, owner, false);
        let second = install(&mut workspace, owner, false);
        let foreign = install(&mut workspace, other, false);
        assert_eq!(workspace.snippet_manager_for(owner), Some(second));
        workspace.snippet_window_focused(first);
        assert_eq!(workspace.snippet_manager_for(owner), Some(first));
        assert_eq!(workspace.snippet_manager_for(other), Some(foreign));

        let _ = workspace.request_snippet_close(first);
        assert!(!workspace.is_snippet_surface(first));
        assert!(workspace.is_snippet_window(second));
        assert_eq!(workspace.snippet_manager_for(owner), Some(second));
    }

    #[test]
    fn moved_snippet_tab_keeps_draft_and_batch_close_cancellation_preserves_both() {
        let profile = snippets::tests::Profile::new();
        let mut workspace = Workspace::default();
        let (first_owner, _) = workspace.open_window(false);
        let (second_owner, _) = workspace.open_window(false);
        let first = workspace
            .install_snippet_surface(
                first_owner,
                SnippetWindow::open(profile.path()).unwrap(),
                true,
            )
            .0;
        let tab = workspace.hosted_tools_in(first_owner)[0];
        let _ = workspace.update(Message::Snippets(
            first,
            snippet_window::Message::Name("draft before move".into()),
        ));
        let _ = workspace.update(Message::Transfer {
            from: first_owner,
            to: Some(second_owner),
            tab: tab.id,
        });
        assert_eq!(workspace.hosted_tool_owner(first), Some(second_owner));
        assert!(workspace.hosted_snippets_dirty(first));
        let second = workspace
            .install_snippet_surface(
                first_owner,
                SnippetWindow::open(profile.path()).unwrap(),
                true,
            )
            .0;
        let _ = workspace.update(Message::Snippets(
            second,
            snippet_window::Message::Name("another draft".into()),
        ));

        let _ = workspace.update(Message::Quit);
        let token = workspace.snippets[&second].window.hosted_close().unwrap();
        let _ = workspace.update(Message::Snippets(
            second,
            snippet_window::Message::HostedClose(token, true),
        ));
        assert!(workspace.is_snippet_surface(second));
        assert!(workspace.hosted_snippets_dirty(second));
        assert_eq!(
            workspace.snippets[&first].window.hosted_close(),
            Some(token)
        );
        let _ = workspace.update(Message::Snippets(
            first,
            snippet_window::Message::HostedClose(token, false),
        ));
        assert!(!workspace.exiting);
        assert!(workspace.hosted_snippets_dirty(first));
        assert!(workspace.hosted_snippets_dirty(second));
        assert!(
            workspace
                .snippets
                .values()
                .all(|surface| surface.window.hosted_close().is_none())
        );
    }

    #[test]
    fn open_diagram_uses_the_current_snippet_draft_and_leaves_the_library_intact() {
        use iced::widget::text_editor::{Action, Edit};
        use std::sync::Arc;

        let profile = snippets::tests::Profile::new();
        let mut library = snippets::Library::open(profile.path()).unwrap();
        let saved = library
            .save(
                None,
                snippets::Input {
                    name: "Backup".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: "echo saved".into(),
                },
            )
            .unwrap();
        let (id, _) = window::open(window::Settings::default());
        let mut workspace = Workspace {
            snippets: [(
                id,
                Surface::new(SnippetWindow::open(profile.path()).unwrap(), id),
            )]
            .into(),
            ..Workspace::default()
        };
        let _ = workspace.update_snippets(
            id,
            snippet_window::Message::Edit(Action::Edit(Edit::Paste(Arc::new(
                "echo draft\n".into(),
            )))),
        );
        let _ = workspace.update_snippets(id, snippet_window::Message::OpenDiagram);
        let mut diagrams = workspace.diagrams.values();
        let diagram = diagrams.next().unwrap();
        assert_eq!(diagram.source(), "echo draft\necho saved");
        assert!(diagrams.next().is_none());
        let library = snippets::Library::open(profile.path()).unwrap();
        assert_eq!(library.get(saved).unwrap().content(), "echo saved");
        assert!(
            !workspace
                .snippets
                .get_mut(&id)
                .unwrap()
                .window
                .request_close()
        );
    }

    #[test]
    fn dirty_snippet_window_can_cancel_and_resume_app_quit() {
        let profile = snippets::tests::Profile::new();
        let (id, _) = window::open(window::Settings::default());
        let mut workspace = Workspace {
            snippets: [(
                id,
                Surface::new(SnippetWindow::open(profile.path()).unwrap(), id),
            )]
            .into(),
            ..Workspace::default()
        };
        let _ = workspace.update(Message::Snippets(
            id,
            snippet_window::Message::Name("Unsaved".into()),
        ));
        let _ = workspace.update(Message::Quit);
        let token = workspace.snippets[&id].window.hosted_close().unwrap();
        assert!(!workspace.exiting);
        let _ = workspace.dismiss_snippets(id);
        assert!(workspace.is_snippet_window(id));
        assert!(!workspace.hosted_close_locks(id));

        let _ = workspace.update(Message::Quit);
        let current = workspace.snippets[&id].window.hosted_close().unwrap();
        assert_ne!(current, token);
        let _ = workspace.update(Message::Snippets(
            id,
            snippet_window::Message::HostedClose(token, true),
        ));
        assert!(workspace.is_snippet_window(id));
        let _ = workspace.update(Message::Snippets(
            id,
            snippet_window::Message::HostedClose(current, true),
        ));
        assert!(!workspace.is_snippet_window(id));
        assert!(workspace.exiting);
        assert!(!profile.path().exists());
    }

    #[test]
    fn main_window_close_preflights_its_hosted_and_native_snippet_drafts_together() {
        let profile = snippets::tests::Profile::new();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let install = |workspace: &mut Workspace, source, hosted, name: &str| {
            let (id, _) = workspace.install_snippet_surface(
                source,
                SnippetWindow::open(profile.path()).unwrap(),
                hosted,
            );
            let _ = workspace.update(Message::Snippets(
                id,
                snippet_window::Message::Name(name.into()),
            ));
            id
        };
        let hosted = install(&mut workspace, owner, true, "Hosted draft");
        let first = install(&mut workspace, owner, false, "First native draft");
        let second = install(&mut workspace, owner, false, "Second native draft");
        let foreign = install(&mut workspace, other, false, "Other owner's draft");
        let _ = workspace.update(Message::Snippets(
            first,
            snippet_window::Message::Edit(iced::widget::text_editor::Action::Edit(
                iced::widget::text_editor::Edit::Paste(std::sync::Arc::new("echo first".into())),
            )),
        ));
        assert_eq!(
            workspace.native_snippet_close_targets(Some(owner)),
            [first, second]
        );
        let close = Message::Window(owner, window::Event::CloseRequested);

        let _ = workspace.update(close.clone());
        let token = workspace.snippets[&hosted].window.hosted_close().unwrap();
        let _ = workspace.update(Message::Snippets(
            hosted,
            snippet_window::Message::HostedClose(token, true),
        ));
        assert_eq!(
            workspace.snippets[&first].window.hosted_close(),
            Some(token)
        );
        let _ = workspace.update(Message::Snippets(
            first,
            snippet_window::Message::HostedClose(token, true),
        ));
        assert_eq!(
            workspace.snippets[&second].window.hosted_close(),
            Some(token)
        );
        // A direct native close during the pending shared transaction cannot tear down an
        // already approved draft or make its earlier Save button active again.
        let _ = workspace.update(Message::Window(first, window::Event::CloseRequested));
        let _ = workspace.update(Message::Snippets(
            first,
            snippet_window::Message::HostedSave(token),
        ));
        assert!(!profile.path().exists());
        let _ = workspace.dismiss_snippets(second);
        assert!(workspace.windows.contains_key(&owner));
        for id in [hosted, first, second, foreign] {
            assert!(workspace.hosted_snippets_dirty(id));
            assert!(!workspace.hosted_close_locks(id));
        }

        let _ = workspace.update(close);
        let token = workspace.snippets[&hosted].window.hosted_close().unwrap();
        for id in [hosted, first, second] {
            let _ = workspace.update(Message::Snippets(
                id,
                snippet_window::Message::HostedClose(token, true),
            ));
        }
        assert!(!workspace.windows.contains_key(&owner));
        assert!(workspace.windows.contains_key(&other));
        assert_eq!(workspace.native_snippet_close_targets(None), [foreign]);
        assert!(workspace.hosted_snippets_dirty(foreign));
        assert!(!profile.path().exists());
    }
}
