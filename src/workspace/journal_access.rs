//! Native journal views and metadata editing.

use std::borrow::Cow;
use std::io;
use std::ops::RangeInclusive;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use iced::widget::{
    Column, button, checkbox, column, container, mouse_area, pick_list, responsive, rich_text, row,
    rule, scrollable, space, span, text, text_editor, text_input, tooltip,
};
use iced::{Alignment, Color, Element, Font, Length, Size, Task, window};
use jiff::Timestamp;

use crate::session_journal::export_output::Format as ExportFormat;
use crate::session_journal::keywords::MAX_QUERY_BYTES as MAX_QUERY;
use crate::session_journal::{
    self, CaptureKind, CapturePage, Catalog, Cursor, Journal, LinkField, LinkTarget, Summary,
    TimelineEntry, tree, viewer::Source as ViewerSource,
};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::journal_appearance::{Content, controls};
use super::journal_appearance_access::{
    Page as AppearancePage, Target as AppearanceTarget, page_text,
};
use super::{Message as Desktop, Workspace, tool_tabs::Kind};

pub(super) mod actions;
mod content_search;
pub(super) mod dialog_geometry;
mod entry_edit_access;
pub(in crate::workspace) mod entry_editor;
use entry_edit_access::deletion as entry_delete;
pub(super) use entry_edit_access::deletion::Dialogs as EntryDeleteDialogs;
pub(super) mod export_dialog;
mod export_options;
mod keyword_chips;
mod link_text;
mod manager_tree;
pub(super) mod marker_dialog;
pub(super) mod metadata;
pub(super) mod replace_dialog;
pub(in crate::workspace) mod screenshot_access;
pub(in crate::workspace) mod screenshot_ai_access;
pub(super) mod screenshot_editor;
pub(in crate::workspace) mod screenshot_export;
mod timeline_find;
mod viewer_keys;
mod viewer_navigation;
mod viewer_refresh;
mod viewer_thumbnail;
pub(super) use link_text::journal_rich_text;
use viewer_navigation::{Event as Navigation, Projection, projection::row_field};

pub(super) const TITLE: &str = "Session Journals";
const PAGE_ROWS: usize = 64;
const MAX_DETAIL: usize = 8192;
const COMPACT_HEIGHT: f32 = 360.0;

#[derive(Clone, Debug)]
pub enum Message {
    Metadata(metadata::Message),
    Actions(actions::Message),
    ContentSearch(content_search::Message),
    Export(export_dialog::Message),
    ScreenshotExport(screenshot_export::Event),
    Annotate(screenshot_editor::Event),
    Open,
    Options,
    Folder(String),
    Refresh,
    CatalogLoaded(u64, PathBuf, Result<Arc<Catalog>, String>),
    Search(String),
    Keyword(u64, String),
    SearchContents(bool),
    Filter(tree::Filter),
    Tree(manager_tree::Event),
    #[cfg(test)]
    Select(u64, PathBuf),
    OpenSelected,
    DocumentLoaded(Result<Arc<Journal>, String>),
    ViewerNavigation(Navigation),
    ViewerRefresh(viewer_refresh::Event),
    Appearance(controls::Event),
    EntryEditor(entry_editor::Event),
    EntryEdit(entry_edit_access::Event),
    EntryDelete(entry_delete::Event),
    Replace(replace_dialog::Event),
    Markers(marker_dialog::Event),
    ShowCapture(bool),
    SelectEntry(usize, String),
    CaptureForEntry,
    CaptureQuery(String),
    SearchCapture,
    ResetRange,
    CaptureLoaded(u64, Result<Arc<CapturePage>, String>),
    ShowScreenshot(usize, String),
    OpenThumbnail(usize, String, String),
    ScreenshotThumbnails(Arc<super::journal_thumbnail::Loaded>),
    ScreenshotLoaded(u64, Result<Arc<screenshot_export::Displayed>, String>),
    ScreenshotAction(screenshot_access::Event),
    ScreenshotAnalysis(screenshot_ai_access::Event),
    HideScreenshot,
    OpenLink(usize, String, LinkField, LinkTarget),
    LinkOpened(bool),
    Page(bool),
    Close,
}

pub(super) struct Surface {
    // Native windows belong to the main window that opened them. Hosted
    // instances always resolve their current owner through the tab strip.
    native_owner: Option<window::Id>,
    mode: Mode,
}

enum Mode {
    Manager(Box<Manager>),
    Viewer(Box<Viewer>),
}

struct Manager {
    folder: String,
    inherited: bool,
    locked_root: Option<PathBuf>,
    root: Option<PathBuf>,
    revision: u64,
    reading: Option<(u64, PathBuf)>,
    pending: Option<PathBuf>,
    catalog: Option<Arc<Catalog>>,
    query: String,
    query_input: iced::widget::Id,
    content_search: content_search::Search,
    tree: manager_tree::State,
    filter: tree::Filter,
    modifiers: iced::keyboard::Modifiers,
    loading: bool,
    error: Option<String>,
    description: text_editor::Content,
    description_saved: String,
    description_target: Option<(PathBuf, String)>,
    description_status: Option<String>,
    action_report: Option<Arc<actions::Report>>,
    live_aliases: Option<Vec<metadata::LiveAlias>>,
}

struct Viewer {
    source: ViewerSource,
    name: String,
    document: Option<Arc<Journal>>,
    deferred_title: Option<(String, String)>,
    deferred_description: Option<(String, String)>,
    capture: Option<Arc<CapturePage>>,
    page_revision: u64,
    capture_shown: bool,
    selected_entry: Option<usize>,
    export_range: Option<export_dialog::ranges::Picker>,
    timeline_page: usize,
    navigation: viewer_navigation::State,
    refresh: viewer_refresh::State,
    entry_editor: entry_editor::State,
    entry_flow: entry_edit_access::Flow,
    query: String,
    search: String,
    range: Option<RangeInclusive<u64>>,
    cursors: Vec<Option<Cursor>>,
    loading: bool,
    error: Option<String>,
    screenshot: Option<Arc<screenshot_export::Displayed>>,
    screenshot_request: Option<String>,
    screenshot_menu: Option<iced::Point>,
    screenshot_analysis_notice: Option<String>,
    screenshot_menu_choice: super::journal_image_menu::Choice,
    screenshot_thumbnails: super::journal_thumbnail::Cache,
    thumbnail_menu: Option<screenshot_access::ThumbnailMenu>,
    title_menu: bool,
}

impl Surface {
    fn title(&self) -> Cow<'static, str> {
        match &self.mode {
            Mode::Manager(_) => Cow::Borrowed(TITLE),
            Mode::Viewer(viewer) => Cow::Owned(format!(
                "{TITLE} — {}",
                label(
                    viewer
                        .document
                        .as_ref()
                        .map_or(&viewer.name, |journal| &journal.summary.title),
                    100
                )
            )),
        }
    }
}

impl Workspace {
    pub(super) fn journal_allowed(&self) -> bool {
        self.clipboard.policy.session_journal_allowed && self.clipboard.policy.problem.is_none()
    }

    pub(super) fn journal_links_allowed(&self) -> bool {
        self.journal_allowed()
            && self.window_settings.session_journal.clickable_links
            && self
                .clipboard
                .policy
                .terminal_logging
                .journal_clickable_links_allowed()
    }

    fn visible_journal_link(
        &self,
        id: window::Id,
        index: usize,
        entry_id: &str,
        field: LinkField,
        target: &LinkTarget,
    ) -> bool {
        if !self.journal_links_allowed() || self.exiting || self.scheduler_draining {
            return false;
        }
        let Some(surface) = self.journals.get(&id) else {
            return false;
        };
        let Mode::Viewer(viewer) = &surface.mode else {
            return false;
        };
        let Some(owner) = self.journal_owner(id, surface) else {
            return false;
        };
        let active = surface.native_owner.is_some()
            || matches!(self.windows.get(&owner).and_then(super::Window::active_entry),
                Some(super::TabEntry::Tool(tool)) if tool.instance == id);
        active
            && self.palette_can_open(owner)
            && !self.palette.is_open(owner)
            && viewer.has_link(index, entry_id, field, target)
    }

    pub(super) fn open_journal_link(
        &mut self,
        id: window::Id,
        target: LinkTarget,
    ) -> Task<Desktop> {
        if !self.journal_links_allowed() || self.exiting || self.scheduler_draining {
            return Task::none();
        }
        #[cfg(target_os = "macos")]
        {
            let opened = kortty_windowing::open_desktop_uri(target.uri());
            self.update_session_journal(id, Message::LinkOpened(opened))
        }
        #[cfg(not(target_os = "macos"))]
        Task::perform(
            async move { kortty_windowing::open_desktop_uri(target.uri()) },
            move |opened| Desktop::SessionJournal(id, Message::LinkOpened(opened)),
        )
    }

    fn journal_link_opened(&mut self, id: window::Id, opened: bool) {
        if !opened {
            let problem = "The desktop could not open the journal link.".to_owned();
            if let Some(Mode::Viewer(viewer)) = self.journals.get_mut(&id).map(|s| &mut s.mode) {
                viewer.error = Some(problem);
            } else {
                self.notice(id, problem);
            }
        }
    }

    pub(super) fn journal_directory(&self) -> io::Result<PathBuf> {
        if !self.journal_allowed() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Session Journals are disabled by policy",
            ));
        }
        if let Some(root) = self.clipboard.policy.terminal_logging.journal_directory() {
            return Ok(root.to_owned());
        }
        self.window_settings
            .session_journal
            .directory(&crate::profile::root()?)
    }

    pub(super) fn journal_manager_focused(&mut self, id: window::Id) -> Task<Desktop> {
        let instance = if self.journals.contains_key(&id) {
            id
        } else if let Some(super::TabEntry::Tool(tool)) =
            self.windows.get(&id).and_then(super::Window::active_entry)
            && tool.kind == Kind::SessionJournalManager
        {
            tool.instance
        } else {
            return Task::none();
        };
        self.refresh_journal_manager(instance, true)
    }

    pub(super) fn journal_modifiers(
        &mut self,
        id: window::Id,
        modifiers: iced::keyboard::Modifiers,
    ) {
        if let Some(Mode::Manager(manager)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        {
            manager.modifiers = modifiers;
        }
        // Clear modifiers on blur as well: a tab can be selected while a shortcut
        // key is still held, without another ModifiersChanged event.
        if let Some(window) = self.windows.get_mut(&id) {
            window.remote_modifiers = modifiers;
        }
    }

    fn journal_manager_interactive(&self, id: window::Id) -> bool {
        if self.exiting || self.scheduler_draining || self.journal_edit_for_owner(id).is_some() {
            return false;
        }
        let Some(owner) = self.visible_journal_manager_owner(id) else {
            return false;
        };
        self.palette_can_open(owner) && !self.palette.is_open(owner)
    }

    fn visible_journal_manager_owner(&self, id: window::Id) -> Option<window::Id> {
        if !matches!(&self.journals.get(&id)?.mode, Mode::Manager(_)) {
            return None;
        }
        self.visible_journal_surface_owner(id)
    }

    fn visible_journal_surface_owner(&self, id: window::Id) -> Option<window::Id> {
        let surface = self.journals.get(&id)?;
        let owner = self.journal_owner(id, surface)?;
        (surface.native_owner.is_some()
            || matches!(self.windows.get(&owner).and_then(super::Window::active_entry),
                Some(super::TabEntry::Tool(tool)) if tool.instance == id))
        .then_some(owner)
    }

    fn viewer_navigation_interactive(&self, id: window::Id) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.journal_allowed()
            && self.journal_edit_for_owner(id).is_none()
            && self
                .visible_journal_surface_owner(id)
                .is_some_and(|owner| self.palette_can_open(owner) && !self.palette.is_open(owner))
    }

    pub(super) fn journal_viewer_appearance_owner(&self, id: window::Id) -> Option<window::Id> {
        let surface = self.journals.get(&id)?;
        match &surface.mode {
            Mode::Viewer(viewer)
                if viewer.screenshot.is_none()
                    && viewer.export_range.is_none()
                    && self.viewer_navigation_interactive(id) =>
            {
                self.journal_owner(id, surface)
            }
            _ => None,
        }
    }

    pub(super) fn resize_journal_timelines(&mut self, scale: u16) -> Task<Desktop> {
        Task::batch(
            self.journals
                .values_mut()
                .filter_map(|surface| match &mut surface.mode {
                    Mode::Viewer(viewer) => Some(viewer.resize_timeline(scale)),
                    Mode::Manager(_) => None,
                }),
        )
    }

    fn journal_manager_instance(&self, physical: window::Id) -> Option<window::Id> {
        if self.journals.get(&physical).is_some_and(|surface| {
            surface.native_owner.is_some() && matches!(surface.mode, Mode::Manager(_))
        }) {
            return Some(physical);
        }
        match self
            .windows
            .get(&physical)
            .and_then(super::Window::active_entry)
        {
            Some(super::TabEntry::Tool(tool)) if tool.kind == Kind::SessionJournalManager => {
                Some(tool.instance)
            }
            _ => None,
        }
    }

    pub(super) fn dismiss_journal_context_for_window(&mut self, physical: window::Id) {
        if let Some(id) = self.journal_manager_instance(physical) {
            self.dismiss_journal_menu(id);
        }
        let viewer = match self
            .windows
            .get(&physical)
            .and_then(super::Window::active_entry)
        {
            Some(super::TabEntry::Tool(tool)) if tool.kind == Kind::SessionJournalViewer => {
                tool.instance
            }
            _ => physical,
        };
        if let Some(Mode::Viewer(viewer)) = self
            .journals
            .get_mut(&viewer)
            .map(|surface| &mut surface.mode)
        {
            viewer.thumbnail_menu = None;
        }
    }

    pub(super) fn journal_context_view<'a>(
        &'a self,
        physical: window::Id,
        element: Element<'a, Desktop>,
    ) -> Element<'a, Desktop> {
        let Some(id) = self.journal_manager_instance(physical) else {
            return element;
        };
        let Mode::Manager(manager) = &self.journals[&id].mode else {
            return element;
        };
        let visible = self.journal_allowed()
            && self.journal_manager_interactive(id)
            && !self.journal_edit_busy(id);
        manager.tree.context_view(id, element, true, visible)
    }

    pub(super) fn refresh_inherited_journal_managers(&mut self) -> Task<Desktop> {
        let managers: Vec<_> = self
            .journals
            .iter()
            .filter_map(|(&id, surface)| {
                matches!(&surface.mode, Mode::Manager(manager) if manager.inherited).then_some(id)
            })
            .collect();
        Task::batch(
            managers
                .into_iter()
                .map(|id| self.refresh_journal_manager(id, true)),
        )
    }

    fn refresh_journal_manager(&mut self, id: window::Id, only_changed: bool) -> Task<Desktop> {
        if !self.journal_allowed() || self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let Some(Mode::Manager(manager)) = self.journals.get(&id).map(|surface| &surface.mode)
        else {
            return Task::none();
        };
        let locked_root = self
            .clipboard
            .policy
            .terminal_logging
            .journal_directory()
            .map(Path::to_owned);
        if only_changed && !manager.inherited && manager.locked_root == locked_root {
            return Task::none();
        }
        let root = if manager.inherited || locked_root.is_some() {
            self.journal_directory()
        } else {
            let root = PathBuf::from(manager.folder.trim());
            if !root.is_absolute() || root.components().any(|part| part == Component::ParentDir) {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Enter an absolute journal folder path.",
                ))
            } else {
                Ok(root)
            }
        };
        if only_changed
            && root.as_ref().ok() == manager.root.as_ref()
            && manager.locked_root == locked_root
        {
            return Task::none();
        }
        let Some(Mode::Manager(manager)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        manager.locked_root = locked_root;
        match root {
            Ok(root) => {
                if manager.inherited {
                    manager.folder = root.to_string_lossy().into_owned();
                }
                manager.request(id, root)
            }
            Err(error) => {
                manager.invalidate();
                manager.error = Some(error.to_string());
                Task::none()
            }
        }
    }

    pub(super) fn is_journal_window(&self, id: window::Id) -> bool {
        self.journal_edits.dialogs.contains_key(&id)
            || self.journal_actions.dialogs.contains_key(&id)
            || self.journal_entry_deletes.dialogs.contains_key(&id)
            || self.journal_replacements.dialogs.contains_key(&id)
            || self.journal_markers.contains(id)
            || self.journal_exports.dialogs.contains_key(&id)
            || self.journal_screenshot_exports.dialogs.contains_key(&id)
            || self.journal_screenshot_editors.title(id).is_some()
            || self
                .journals
                .get(&id)
                .is_some_and(|surface| surface.native_owner.is_some())
    }

    pub(super) fn journal_edit_focus_message(&self, id: window::Id) -> Desktop {
        let message = if self.journal_screenshot_editors.title(id).is_some() {
            Message::Annotate(screenshot_editor::Event::Focus)
        } else if self.journal_exports.dialogs.contains_key(&id) {
            Message::Export(export_dialog::Message::Focus)
        } else if self.journal_actions.dialogs.contains_key(&id) {
            Message::Actions(actions::Message::Focus)
        } else if self.journal_entry_deletes.dialogs.contains_key(&id) {
            Message::EntryDelete(entry_delete::Event::Focus)
        } else if self.journal_replacements.dialogs.contains_key(&id) {
            Message::Replace(replace_dialog::Event::Focus)
        } else if self.journal_markers.contains(id) {
            Message::Markers(marker_dialog::Event::Focus)
        } else if self.journal_screenshot_exports.dialogs.contains_key(&id) {
            Message::ScreenshotExport(screenshot_export::Event::Focus)
        } else {
            Message::Metadata(metadata::Message::Focus)
        };
        Desktop::SessionJournal(id, message)
    }

    pub(super) fn journal_title(&self, id: window::Id) -> Option<Cow<'static, str>> {
        if let Some(title) = self.journal_screenshot_editors.title(id) {
            return Some(Cow::Borrowed(title));
        }
        if let Some(dialog) = self.journal_exports.dialogs.get(&id) {
            return Some(Cow::Borrowed(dialog.title()));
        }
        if self.journal_actions.dialogs.contains_key(&id) {
            return Some(Cow::Borrowed(actions::TITLE));
        }
        if self.journal_entry_deletes.dialogs.contains_key(&id) {
            return Some(Cow::Borrowed(entry_delete::TITLE));
        }
        if self.journal_replacements.dialogs.contains_key(&id) {
            return Some(Cow::Borrowed(replace_dialog::TITLE));
        }
        if self.journal_markers.contains(id) {
            return Some(Cow::Borrowed(marker_dialog::TITLE));
        }
        if self.journal_screenshot_exports.dialogs.contains_key(&id) {
            return Some(Cow::Borrowed(screenshot_export::TITLE));
        }
        if self.journal_edits.dialogs.contains_key(&id) {
            return Some(Cow::Borrowed(metadata::TITLE));
        }
        self.journals.get(&id).map(Surface::title)
    }

    pub(super) fn journal_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        if let Some(editor) = self.journal_screenshot_editor_view(id) {
            return Some(editor);
        }
        if let Some(dialog) = self.journal_marker_view(id) {
            return Some(dialog);
        }
        if self.journal_screenshot_exports.dialogs.contains_key(&id) {
            return Some(self.journal_screenshot_export_view(id));
        }
        if let Some(dialog) = self.journal_exports.dialogs.get(&id) {
            return Some(
                dialog
                    .view()
                    .map(move |message| Desktop::SessionJournal(id, Message::Export(message))),
            );
        }
        if let Some(dialog) = self.journal_actions.dialogs.get(&id) {
            return Some(
                dialog
                    .view()
                    .map(move |message| Desktop::SessionJournal(id, Message::Actions(message))),
            );
        }
        if let Some(dialog) = self.journal_entry_deletes.dialogs.get(&id) {
            return Some(
                dialog
                    .view()
                    .map(move |event| Desktop::SessionJournal(id, Message::EntryDelete(event))),
            );
        }
        if let Some(dialog) = self.journal_replacements.dialogs.get(&id) {
            let blocked = self.journal_replacement_blocker(id, dialog);
            return Some(
                dialog
                    .view(blocked)
                    .map(move |event| Desktop::SessionJournal(id, Message::Replace(event))),
            );
        }
        if let Some(dialog) = self.journal_edits.dialogs.get(&id) {
            return Some(
                dialog
                    .view()
                    .map(move |message| Desktop::SessionJournal(id, Message::Metadata(message))),
            );
        }
        let surface = self.journals.get(&id)?;
        if !self.journal_allowed() {
            return Some(
                column![
                    text("Session Journals are disabled by policy."),
                    button("Close").on_press(Desktop::SessionJournal(id, Message::Close)),
                ]
                .padding(14)
                .spacing(8)
                .into(),
            );
        }
        let view = match &surface.mode {
            Mode::Manager(manager) => manager.view(
                self.journal_rename_allowed()
                    && manager.live_aliases.is_some()
                    && manager
                        .selected()
                        .is_some_and(|row| !self.journal_metadata_is_live(manager, row)),
                self.journal_edit_busy(id),
                !self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_rename_allowed(),
                self.journal_delete_available(id),
                !self.journal_delete_allowed(),
            ),
            Mode::Viewer(viewer) => {
                let appearance = self.journal_appearance_page(AppearanceTarget::Viewer(id));
                let keyboard = self.viewer_navigation_interactive(id) && !appearance.open;
                let screenshot_interactive = keyboard && !self.hosted_close_locks(id);
                let entry_enabled = appearance.enabled
                    && viewer.can_edit_entries()
                    && viewer.entry_flow.input_enabled();
                let entry_controls = entry_editor::Controls {
                    enabled: entry_enabled,
                    delete_allowed: self.journal_delete_allowed(),
                    translation: self.journal_note_translation_view(
                        entry_enabled
                            && screenshot_interactive
                            && viewer.entry_editor.can_translate(),
                    ),
                };
                viewer_keys::wrap(
                    viewer.view(
                        self.journal_links_allowed(),
                        self.journal_rename_allowed() && !self.journal_edit_busy(id),
                        entry_controls,
                        screenshot_interactive,
                        self.journal_screenshot_analysis_available(),
                        appearance,
                    ),
                    viewer,
                    keyboard,
                )
            }
        };
        Some(view.map(move |message| Desktop::SessionJournal(id, message)))
    }

    pub(super) fn remove_closed_journal_windows(&mut self, owner: window::Id) -> Task<Desktop> {
        let dialogs = self.remove_journal_edit_owner(owner);
        self.journals.remove(&owner);
        let mut closed = Vec::new();
        self.journals.retain(|&id, surface| {
            if surface.native_owner != Some(owner) {
                return true;
            }
            closed.push(id);
            false
        });
        Task::batch(
            closed
                .into_iter()
                .map(|id| self.close_placed_dialog(id))
                .chain([dialogs]),
        )
    }

    fn journal_owner(&self, id: window::Id, surface: &Surface) -> Option<window::Id> {
        surface.native_owner.or_else(|| self.hosted_tool_owner(id))
    }

    fn open_journal_manager(&mut self, owner: window::Id) -> Task<Desktop> {
        if !self.windows.contains_key(&owner) || self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let hosted = self.window_settings.open_tool_windows_as_tabs;
        if let Some(id) = if hosted {
            self.hosted_tool_in(owner, Kind::SessionJournalManager)
        } else {
            self.journals.iter().find_map(|(&id, surface)| {
                (surface.native_owner == Some(owner) && matches!(surface.mode, Mode::Manager(_)))
                    .then_some(id)
            })
        } {
            let refresh = self.refresh_journal_manager(id, false);
            let focus = self
                .focus_hosted_tool(id)
                .unwrap_or_else(|| window::gain_focus(id));
            return Task::batch([refresh, focus]);
        }
        let locked_root = self
            .clipboard
            .policy
            .terminal_logging
            .journal_directory()
            .map(Path::to_owned);
        let root = self.journal_directory();
        let root = match root {
            Ok(root) => root,
            Err(error) => {
                self.notice(owner, format!("Journal folder unavailable ({error})."));
                return Task::none();
            }
        };
        let (id, open) = self.open_journal_surface(
            owner,
            Kind::SessionJournalManager,
            Mode::Manager(Box::new(Manager {
                folder: root.to_string_lossy().into_owned(),
                inherited: true,
                locked_root,
                root: None,
                revision: 0,
                reading: None,
                pending: None,
                catalog: None,
                query: String::new(),
                query_input: iced::widget::Id::unique(),
                content_search: content_search::Search::default(),
                tree: manager_tree::State::default(),
                filter: tree::Filter::default(),
                modifiers: iced::keyboard::Modifiers::empty(),
                loading: false,
                error: None,
                description: text_editor::Content::new(),
                description_saved: String::new(),
                description_target: None,
                description_status: None,
                action_report: None,
                live_aliases: None,
            })),
            Size::new(940.0, 620.0),
        );
        Task::batch([open, self.refresh_journal_manager(id, false)])
    }

    fn open_selected_journal(&mut self, manager_id: window::Id) -> Task<Desktop> {
        let Some(surface) = self.journals.get(&manager_id) else {
            return Task::none();
        };
        let Mode::Manager(manager) = &surface.mode else {
            return Task::none();
        };
        let Some((summary, owner)) = manager
            .selected()
            .zip(self.journal_owner(manager_id, surface))
        else {
            return Task::none();
        };
        self.open_catalog_journal_viewer(
            owner,
            ViewerSource::catalog(summary),
            summary.title.clone(),
        )
    }

    fn open_catalog_journal_viewer(
        &mut self,
        owner: window::Id,
        source: io::Result<ViewerSource>,
        name: String,
    ) -> Task<Desktop> {
        match source {
            Ok(source) => self.open_journal_viewer(owner, source, name),
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not open the selected journal ({error})."),
                );
                Task::none()
            }
        }
    }

    pub(super) fn open_journal_viewer(
        &mut self,
        owner: window::Id,
        source: ViewerSource,
        name: String,
    ) -> Task<Desktop> {
        if !self.journal_allowed() || self.exiting || self.scheduler_draining {
            return Task::none();
        }
        if let Some(id) = self.journals.iter().find_map(|(&id, surface)| {
            (matches!(&surface.mode, Mode::Viewer(viewer) if viewer.source.same_journal(&source))
                && self.journal_owner(id, surface) == Some(owner))
            .then_some(id)
        }) {
            return self
                .focus_hosted_tool(id)
                .unwrap_or_else(|| window::gain_focus(id));
        }
        let (id, open) = self.open_journal_surface(
            owner,
            Kind::SessionJournalViewer,
            Mode::Viewer(Box::new(Viewer {
                source: source.clone(),
                name,
                document: None,
                deferred_title: None,
                deferred_description: None,
                capture: None,
                page_revision: 0,
                capture_shown: false,
                selected_entry: None,
                export_range: None,
                timeline_page: 0,
                navigation: viewer_navigation::State::default(),
                refresh: viewer_refresh::State::default(),
                entry_editor: entry_editor::State::default(),
                entry_flow: entry_edit_access::Flow::default(),
                query: String::new(),
                search: String::new(),
                range: None,
                cursors: vec![None],
                loading: true,
                error: None,
                screenshot: None,
                screenshot_request: None,
                screenshot_menu: None,
                screenshot_analysis_notice: None,
                screenshot_menu_choice: Default::default(),
                screenshot_thumbnails: super::journal_thumbnail::Cache::default(),
                thumbnail_menu: None,
                title_menu: false,
            })),
            Size::new(1000.0, 680.0),
        );
        let scale = self.window_settings.journal_appearance.scale;
        let replacing = self.journal_replacement_in_progress(&source);
        let applying_markers = self.journal_marker_apply_in_progress(&source);
        let screenshot_saving = self.journal_screenshot_edit_in_progress(&source);
        if let Some(Mode::Viewer(viewer)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        {
            viewer.entry_flow.set_replacing(replacing);
            viewer.entry_flow.set_applying_markers(applying_markers);
            viewer.entry_flow.set_screenshot_saving(screenshot_saving);
            let _ = viewer.resize_timeline(scale);
            viewer
                .entry_editor
                .set_split(self.window_settings.journal_edit_split);
        }
        Task::batch([
            open,
            read_task(
                id,
                move || {
                    source
                        .read(None)?
                        .ok_or_else(|| io::Error::other("The journal reader returned no document."))
                },
                Message::DocumentLoaded,
            ),
        ])
    }

    fn open_journal_surface(
        &mut self,
        owner: window::Id,
        kind: Kind,
        mode: Mode,
        size: Size,
    ) -> (window::Id, Task<Desktop>) {
        let (id, task, native_owner) = if self.window_settings.open_tool_windows_as_tabs {
            let id = window::Id::unique();
            (id, self.host_tool(owner, kind, id), None)
        } else {
            let dialog_kind = if kind == Kind::SessionJournalManager {
                DialogKind::JournalManager
            } else {
                DialogKind::JournalViewer
            };
            let (id, opened) = self.open_placed_dialog(
                dialog_kind,
                window::Settings {
                    size,
                    min_size: Some(Size::new(620.0, 400.0)),
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
            );
            (id, opened.map(|_| Desktop::Noop), Some(owner))
        };
        self.journals.insert(id, Surface { native_owner, mode });
        (id, task)
    }

    pub(super) fn update_session_journal(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Metadata(message) => return self.update_journal_metadata(id, message),
            Message::Actions(message) => return self.update_journal_actions(id, message),
            Message::Export(message) => return self.update_journal_export(id, message),
            Message::ScreenshotExport(event) => {
                return self.update_journal_screenshot_export(id, event);
            }
            Message::Annotate(event) => return self.update_journal_screenshot_editor(id, event),
            Message::ScreenshotAction(event) => {
                return self.update_journal_screenshot_access(id, event);
            }
            Message::ScreenshotAnalysis(event) => return self.update_screenshot_analysis(event),
            Message::ViewerRefresh(event) => return self.update_viewer_refresh(id, event),
            Message::EntryEditor(event) => return self.update_journal_entry_editor(id, event),
            Message::EntryEdit(event) => return self.update_journal_entry_edit(id, event),
            Message::EntryDelete(event) => return self.update_journal_entry_delete(id, event),
            Message::Replace(event) => return self.update_journal_replacement(id, event),
            Message::Markers(event) => return self.update_journal_markers(id, event),
            Message::Appearance(event) => {
                return self.update_journal_appearance(AppearanceTarget::Viewer(id), event);
            }
            Message::ViewerNavigation(event) => {
                let interactive = self.viewer_navigation_interactive(id);
                return match self.journals.get_mut(&id).map(|surface| &mut surface.mode) {
                    Some(Mode::Viewer(viewer)) => {
                        let updated = viewer.update_navigation(id, event, interactive);
                        let thumbnails = if interactive {
                            viewer.request_screenshot_thumbnails(id)
                        } else {
                            Task::none()
                        };
                        Task::batch([updated, thumbnails])
                    }
                    _ => Task::none(),
                };
            }
            Message::ScreenshotThumbnails(loaded) => {
                return self.integrate_viewer_thumbnails(id, &loaded);
            }
            Message::OpenThumbnail(..)
                if !self.viewer_navigation_interactive(id)
                    || self.hosted_close_locks(id)
                    || self
                        .journal_appearance_page(AppearanceTarget::Viewer(id))
                        .open =>
            {
                return Task::none();
            }
            Message::ContentSearch(content_search::Message::Finished(ticket, result)) => {
                let refresh = self.refresh_journal_manager(id, true);
                let allowed = self.journal_allowed() && !self.exiting && !self.scheduler_draining;
                let complete = match self.journals.get_mut(&id).map(|surface| &mut surface.mode) {
                    Some(Mode::Manager(manager)) => {
                        manager.finish_content_search(id, ticket, result, allowed)
                    }
                    _ => Task::none(),
                };
                return Task::batch([refresh, complete]);
            }
            Message::OpenLink(index, entry, field, target) => {
                return if self.visible_journal_link(id, index, &entry, field, &target) {
                    self.open_journal_link(id, target)
                } else {
                    Task::none()
                };
            }
            Message::LinkOpened(opened) => {
                self.journal_link_opened(id, opened);
                return Task::none();
            }
            Message::Close if self.hosted_tool_owner(id).is_some() => {
                return self.request_hosted_tool_close(id);
            }
            Message::Close if self.is_journal_window(id) => return self.close_window(id),
            Message::CatalogLoaded(revision, root, result) => {
                let refresh = self.journal_manager_focused(id);
                let allowed = self.journal_allowed();
                let complete = match self.journals.get_mut(&id).map(|surface| &mut surface.mode) {
                    Some(Mode::Manager(manager)) => {
                        manager.loaded(id, revision, root, result, allowed)
                    }
                    _ => Task::none(),
                };
                let live = self.refresh_journal_live_aliases(id);
                return Task::batch([refresh, complete, live]);
            }
            _ if !self.journal_allowed() => return Task::none(),
            Message::Tree(_)
            | Message::Filter(_)
            | Message::Search(_)
            | Message::Keyword(_, _)
            | Message::SearchContents(_)
            | Message::OpenSelected
            | Message::Folder(_)
            | Message::Refresh
                if !self.journal_manager_interactive(id) =>
            {
                return Task::none();
            }
            #[cfg(test)]
            Message::Select(_, _) if !self.journal_manager_interactive(id) => return Task::none(),
            Message::Keyword(revision, keyword) => {
                let Some(Mode::Manager(manager)) =
                    self.journals.get(&id).map(|surface| &surface.mode)
                else {
                    return Task::none();
                };
                if manager.revision != revision
                    || !manager.keyword_labels().any(|word| word == keyword)
                {
                    return Task::none();
                }
                let input = manager.query_input.clone();
                let search = self.update_session_journal(id, Message::Search(keyword));
                let focus = iced::widget::operation::focus(input.clone())
                    .chain(iced::widget::operation::move_cursor_to_end(input));
                return Task::batch([search, focus]);
            }
            Message::Open => return self.open_journal_manager(id),
            Message::Options => {
                if self.journal_manager_interactive(id)
                    && let Some(owner) = self.visible_journal_manager_owner(id)
                {
                    return self.open_journal_options(owner);
                }
                return Task::none();
            }
            Message::Refresh => return self.refresh_journal_manager(id, false),
            Message::Folder(_)
                if self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_directory()
                    .is_some() =>
            {
                return Task::none();
            }
            Message::OpenSelected if !self.exiting && !self.scheduler_draining => {
                let refresh = self.refresh_journal_manager(id, true);
                let open = self.open_selected_journal(id);
                return Task::batch([refresh, open]);
            }
            Message::Tree(event) => {
                let refresh = self.refresh_journal_manager(id, true);
                let busy = self.journal_edit_busy(id);
                let Some(surface) = self.journals.get(&id) else {
                    return refresh;
                };
                let owner = self.journal_owner(id, surface);
                let hosted_modifiers = surface
                    .native_owner
                    .is_none()
                    .then(|| {
                        owner
                            .and_then(|owner| self.windows.get(&owner))
                            .map(|window| window.remote_modifiers)
                    })
                    .flatten();
                let Some(Mode::Manager(manager)) =
                    self.journals.get_mut(&id).map(|surface| &mut surface.mode)
                else {
                    return refresh;
                };
                let modifiers = hosted_modifiers.unwrap_or(manager.modifiers);
                if let manager_tree::Event::Context(event) = event {
                    if busy {
                        return refresh;
                    }
                    let intent = manager.tree.update_context(event, modifiers, true);
                    manager.sync_description();
                    let action = match intent {
                        Some(manager_tree::ContextIntent::Pin { indices, pinned }) => {
                            self.pin_journal_rows(id, indices, pinned)
                        }
                        Some(manager_tree::ContextIntent::OpenJob(job)) => owner
                            .map_or_else(Task::none, |owner| self.show_scheduler_job(owner, job)),
                        None => Task::none(),
                    };
                    return Task::batch([refresh, action]);
                }
                let index = manager.tree.update(event, modifiers);
                manager.sync_description();
                let scroll = manager
                    .tree
                    .take_scroll_task()
                    .map(move |event| Desktop::SessionJournal(id, Message::Tree(event)));
                let target = index
                    .and_then(|index| manager.catalog.as_ref()?.journals.get(index))
                    .map(|summary| (ViewerSource::catalog(summary), summary.title.clone()));
                let open = match owner.zip(target) {
                    Some((owner, (source, title))) => {
                        self.open_catalog_journal_viewer(owner, source, title)
                    }
                    None => Task::none(),
                };
                return Task::batch([refresh, scroll, open]);
            }
            _ => {}
        }
        let thumbnails = self.viewer_navigation_interactive(id);
        match self.journals.get_mut(&id).map(|surface| &mut surface.mode) {
            Some(Mode::Manager(manager)) => {
                manager.update(message);
                manager.sync_description();
                let scroll = manager
                    .tree
                    .take_scroll_task()
                    .map(move |event| Desktop::SessionJournal(id, Message::Tree(event)));
                Task::batch([scroll, manager.start_content_search(id)])
            }
            Some(Mode::Viewer(viewer)) => {
                let updated = viewer.update(id, message);
                let thumbnails = if thumbnails {
                    viewer.request_screenshot_thumbnails(id)
                } else {
                    Task::none()
                };
                Task::batch([updated, thumbnails])
            }
            None => Task::none(),
        }
    }
}

impl Manager {
    fn sync_description(&mut self) {
        if self.loading && self.root.is_some() {
            return;
        }
        let selected = self.selected().map(|row| {
            (
                (row.directory.clone(), row.id.clone()),
                row.description.clone(),
            )
        });
        if selected.is_none()
            && self.tree.has_selection()
            && self
                .content_search
                .pending(&tree::prepare_query(&self.query))
        {
            return;
        }
        let target = selected.as_ref().map(|(target, _)| target.clone());
        let value = selected.map_or_else(String::new, |(_, value)| value);
        if self.description_target != target
            || (self.description_saved != value
                && self.description.text() == self.description_saved)
        {
            self.description_target = target;
            self.description = text_editor::Content::with_text(&value);
            self.description_status = None;
        }
        self.description_saved = value;
    }
    fn invalidate_for(&mut self, root: Option<&Path>) {
        let preserve_description = root.is_some() && self.root.as_deref() == root;
        self.tree.invalidate(root);
        self.content_search.invalidate();
        self.revision = self.revision.wrapping_add(1);
        self.root = None;
        self.pending = None;
        self.catalog = None;
        self.loading = self.reading.is_some();
        self.error = None;
        self.live_aliases = None;
        if !preserve_description {
            self.sync_description();
        }
    }

    fn invalidate(&mut self) {
        self.invalidate_for(None);
    }

    fn request(&mut self, id: window::Id, root: PathBuf) -> Task<Desktop> {
        self.invalidate_for(Some(&root));
        self.root = Some(root.clone());
        self.loading = true;
        if self.reading.is_some() {
            self.pending = Some(root);
            return Task::none();
        }
        self.reading = Some((self.revision, root.clone()));
        catalog_task(id, self.revision, root)
    }

    fn loaded(
        &mut self,
        id: window::Id,
        revision: u64,
        root: PathBuf,
        result: Result<Arc<Catalog>, String>,
        allowed: bool,
    ) -> Task<Desktop> {
        if self.reading.as_ref() != Some(&(revision, root)) {
            return Task::none();
        }
        self.reading = None;
        self.loading = false;
        if !allowed {
            self.invalidate();
            return Task::none();
        }
        if revision == self.revision {
            match result {
                Ok(catalog) => {
                    self.content_search.invalidate();
                    self.catalog = Some(catalog);
                    self.rebuild_tree();
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(root) = self.pending.take() {
            self.reading = Some((self.revision, root.clone()));
            self.loading = true;
            return catalog_task(id, self.revision, root);
        }
        self.sync_description();
        let scroll = self
            .tree
            .take_scroll_task()
            .map(move |event| Desktop::SessionJournal(id, Message::Tree(event)));
        Task::batch([scroll, self.start_content_search(id)])
    }

    fn rebuild_tree(&mut self) {
        if let Some((root, catalog)) = self.root.as_ref().zip(self.catalog.as_ref()) {
            let query = tree::prepare_query(&self.query);
            self.tree.rebuild_matching(
                root,
                catalog,
                self.content_search.pending(&query),
                |index, journal| {
                    tree::matches_query(
                        journal,
                        self.filter,
                        &query,
                        self.content_search.has_match(index),
                    )
                },
            );
        }
    }

    fn results(&self) -> impl Iterator<Item = &Summary> {
        let query = tree::prepare_query(&self.query);
        self.catalog
            .iter()
            .flat_map(|catalog| &catalog.journals)
            .enumerate()
            .filter(move |(index, journal)| {
                tree::matches_query(
                    journal,
                    self.filter,
                    &query,
                    self.content_search.has_match(*index),
                )
            })
            .map(|(_, journal)| journal)
    }

    fn selected(&self) -> Option<&Summary> {
        if self.loading {
            return None;
        }
        let catalog = self.catalog.as_ref()?;
        catalog.journals.get(self.tree.single_selected(catalog)?)
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Folder(value)
                if self.locked_root.is_none()
                    && value.len() <= 4096
                    && !value.chars().any(char::is_control) =>
            {
                self.inherited = false;
                self.folder = value;
                self.action_report = None;
                self.invalidate();
            }
            Message::Search(query) if query.len() <= MAX_QUERY && query != self.query => {
                self.query = query;
                self.content_search.invalidate();
                self.rebuild_tree();
            }
            Message::SearchContents(enabled) if enabled != self.content_search.enabled => {
                self.content_search.enabled = enabled;
                self.content_search.invalidate();
                self.rebuild_tree();
            }
            Message::Filter(filter) if filter != self.filter => {
                self.filter = filter;
                self.content_search.invalidate();
                self.rebuild_tree();
            }
            #[cfg(test)]
            Message::Select(revision, path) if !self.loading && revision == self.revision => {
                self.tree.select_path(&path);
            }
            _ => {}
        }
    }

    fn view(
        &self,
        rename_allowed: bool,
        saving: bool,
        rename_managed: bool,
        delete_available: bool,
        delete_managed: bool,
    ) -> Element<'_, Message> {
        responsive(move |size| {
            self.catalog_view(
                size.height < COMPACT_HEIGHT,
                rename_allowed,
                saving,
                rename_managed,
                delete_available,
                delete_managed,
            )
        })
        .into()
    }

    fn catalog_view(
        &self,
        compact: bool,
        rename_allowed: bool,
        saving: bool,
        rename_managed: bool,
        delete_available: bool,
        delete_managed: bool,
    ) -> Element<'_, Message> {
        let count = self.results().count();
        let rows: Element<'_, Message> = if let Some(catalog) = &self.catalog {
            self.tree.view(catalog, !self.loading).map(Message::Tree)
        } else {
            container(
                text(if self.loading {
                    "Reading journal folder…"
                } else {
                    "No matching session journals."
                })
                .size(13),
            )
            .height(if compact {
                Length::Fixed(200.0)
            } else {
                Length::Fill
            })
            .into()
        };
        let folder = self.locked_root.as_ref().map(|path| path.to_string_lossy());
        let mut path = text_input(
            "Absolute journal folder",
            folder.as_deref().unwrap_or(&self.folder),
        );
        if self.locked_root.is_none() {
            path = path.on_input(Message::Folder).on_submit(Message::Refresh);
        }
        let status = self.error.clone().or_else(|| {
            self.catalog.as_ref().and_then(|catalog| {
                (catalog.skipped > 0 || catalog.limited).then(|| {
                    format!(
                        "{} unreadable journal(s){}",
                        catalog.skipped,
                        if catalog.limited {
                            " · catalog limit reached"
                        } else {
                            ""
                        },
                    )
                })
            })
        });
        let rename = button("Rename").on_press_maybe(
            self.selected()
                .filter(|_| rename_allowed && !saving)
                .map(|row| {
                    Message::Metadata(metadata::Message::RenameTarget((
                        row.directory.clone(),
                        row.id.clone(),
                    )))
                }),
        );
        let rename: Element<'_, Message> = if rename_managed {
            tooltip(
                rename,
                "This option is managed by your organization's policy.",
                tooltip::Position::Top,
            )
            .into()
        } else {
            rename.into()
        };
        let delete = button("Delete").on_press_maybe(
            delete_available.then_some(Message::Actions(actions::Message::DeleteSelected)),
        );
        let delete: Element<'_, Message> = if delete_managed {
            tooltip(
                delete,
                "This option is managed by your organization's policy.",
                tooltip::Position::Top,
            )
            .into()
        } else {
            delete.into()
        };
        let footer = row![
            button("Open").on_press_maybe(self.selected().map(|_| Message::OpenSelected)),
            rename,
            export_menu(
                self.catalog
                    .as_ref()
                    .filter(|_| !self.loading)
                    .is_some_and(|catalog| !self.tree.selected_indices(catalog).is_empty())
            ),
            delete,
            text(format!("{count} journals")).size(13),
            space::horizontal(),
            button("Options…")
                .style(button::secondary)
                .on_press(Message::Options),
            button("Close").on_press(Message::Close)
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let mut body = column![
            text(TITLE).size(21),
            row![path, button("Refresh").on_press(Message::Refresh)].spacing(8),
            row![
                text_input("Filter by title, connection or host", &self.query)
                    .id(self.query_input.clone())
                    .on_input(Message::Search),
                pick_list(tree::Filter::ALL, Some(self.filter), Message::Filter),
                checkbox(self.content_search.enabled)
                    .label("Search contents")
                    .on_toggle(Message::SearchContents),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            self.keyword_chips(),
            container(rows).height(if compact {
                Length::Fixed(220.0)
            } else {
                Length::Fill
            }),
        ]
        .spacing(8)
        .height(body_height(compact));
        if let Some(status) = status {
            body = body.push(text(status).size(13));
        }
        if self.catalog.is_some()
            && !self.loading
            && let Some(status) = self
                .content_search
                .status(&tree::prepare_query(&self.query))
        {
            body = body.push(text(status).size(13));
        }
        if let Some(report) = &self.action_report {
            body = body.push(report.pin_view());
        }
        let editable = self.selected().is_some() && !saving;
        let mut description = text_editor(&self.description)
            .placeholder("Description")
            .height(64);
        if editable && let Some(target) = self.description_target.clone() {
            description = description.on_action(move |action| {
                Message::Metadata(metadata::Message::Description(target.clone(), action))
            });
        }
        body =
            body.push(
                column![
                    text("Description").size(13),
                    description,
                    row![
                        button("Save description").on_press_maybe(
                            self.description_target
                                .clone()
                                .filter(|_| editable)
                                .map(|target| Message::Metadata(
                                    metadata::Message::SaveDescription(target)
                                ))
                        ),
                        self.description_status
                            .as_deref()
                            .map(|status| text(status).size(12)),
                    ]
                    .spacing(8),
                ]
                .spacing(4),
            );
        if compact {
            container(column![compact_scroll(body), footer].spacing(8))
                .padding(14)
                .into()
        } else {
            container(body.push(footer)).padding(14).into()
        }
    }
}

impl Viewer {
    fn can_pick_export_range(&self) -> bool {
        !self.entry_flow.saving()
            && self.document.as_ref().is_some_and(|document| {
                document
                    .timeline
                    .iter()
                    .any(|entry| entry.created_at.is_some())
            })
    }

    fn export_summary(&self) -> Option<&Summary> {
        self.document
            .as_ref()
            .filter(|document| {
                !self.loading
                    && !self.entry_flow.saving()
                    && self.screenshot.is_none()
                    && document.summary.deletion_identity.is_some()
            })
            .map(|document| &document.summary)
    }

    fn has_link(
        &self,
        index: usize,
        entry_id: &str,
        field: LinkField,
        target: &LinkTarget,
    ) -> bool {
        if self.loading
            || self.capture_shown
            || self.screenshot.is_some()
            || self.export_range.is_some()
            || self.selected_entry != Some(index)
        {
            return false;
        }
        self.document
            .as_ref()
            .and_then(|document| document.timeline.get(index))
            .filter(|entry| entry.id == entry_id)
            .is_some_and(|entry| Projection::new(entry).has_link(field, target))
    }

    fn update(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        match message {
            Message::DocumentLoaded(result) if self.loading && self.document.is_none() => {
                self.loading = false;
                match result {
                    Ok(mut document) if self.source.id() == document.summary.id => {
                        if let Some((id, title)) = self.deferred_title.take()
                            && id == document.summary.id
                        {
                            self.name.clone_from(&title);
                            Arc::make_mut(&mut document).summary.title = title;
                        }
                        if let Some((id, description)) = self.deferred_description.take()
                            && id == document.summary.id
                        {
                            Arc::make_mut(&mut document).summary.description = description;
                        }
                        self.navigation.load_markers(&document);
                        self.document = Some(document);
                    }
                    Ok(_) => {
                        self.error = Some("The journal changed. Refresh the journal list.".into())
                    }
                    Err(error) => self.error = Some(error),
                }
                self.deferred_title = None;
                self.deferred_description = None;
            }
            Message::CaptureLoaded(revision, result)
                if revision == self.page_revision && self.loading && self.document.is_some() =>
            {
                self.loading = false;
                match result {
                    Ok(page) => self.capture = Some(page),
                    Err(error) => self.error = Some(error),
                }
                return self.resume_timeline_navigation(true);
            }
            Message::ScreenshotLoaded(revision, result)
                if revision == self.page_revision && self.screenshot_request.is_some() =>
            {
                self.loading = false;
                self.screenshot_request = None;
                self.screenshot_menu = None;
                match result {
                    Ok(image) => self.screenshot = Some(image),
                    Err(error) => self.error = Some(error),
                }
                return self.resume_timeline_navigation(true);
            }
            Message::HideScreenshot => {
                self.screenshot = None;
                self.screenshot_menu = None;
                if self.screenshot_request.take().is_some() {
                    self.loading = false;
                }
                return self.resume_timeline_navigation(true);
            }
            Message::ShowCapture(shown)
                if self.export_range.is_none() && !self.entry_flow.saving() =>
            {
                self.thumbnail_menu = None;
                self.capture_shown = shown;
                if shown && self.document.is_some() && self.capture.is_none() && !self.loading {
                    return self.load_capture(id);
                }
                if !shown {
                    return self.resume_timeline_navigation(true);
                }
            }
            _ if self.loading || self.document.is_none() => {}
            Message::SelectEntry(index, id) if self.export_range.is_none() => {
                self.thumbnail_menu = None;
                self.selected_entry = self
                    .document
                    .as_ref()
                    .and_then(|document| document.timeline.get(index))
                    .filter(|entry| entry.id == id)
                    .map(|_| index);
            }
            Message::CaptureForEntry
                if self.export_range.is_none() && !self.entry_flow.saving() =>
            {
                let Some(range) = self.selected_range() else {
                    return Task::none();
                };
                self.capture_shown = true;
                self.range = Some(range);
                return self.restart_capture(id);
            }
            Message::ShowScreenshot(index, entry_id)
                if self.selected_entry == Some(index)
                    && self.export_range.is_none()
                    && !self.entry_flow.saving() =>
            {
                return self.show_screenshot(id, index, entry_id);
            }
            Message::OpenThumbnail(index, entry_id, reference)
                if self.can_navigate_timeline()
                    && !self.entry_flow.saving()
                    && self.thumbnail_is_current(index, &entry_id, &reference) =>
            {
                return self.show_screenshot(id, index, entry_id);
            }
            Message::CaptureQuery(query) if query.len() <= MAX_QUERY => self.query = query,
            Message::SearchCapture => {
                self.search = self.query.trim().to_owned();
                return self.restart_capture(id);
            }
            Message::ResetRange => {
                self.range = None;
                return self.restart_capture(id);
            }
            Message::Page(forward) if self.capture_shown => {
                if forward {
                    let Some(next) = self.capture.as_ref().and_then(|page| page.next) else {
                        return Task::none();
                    };
                    if self.cursors.len() == 128 {
                        self.cursors.remove(0);
                    }
                    self.cursors.push(Some(next));
                } else if self.cursors.len() > 1 {
                    self.cursors.pop();
                } else {
                    return Task::none();
                }
                return self.load_capture(id);
            }
            Message::Page(forward) if !forward || self.timeline_has_next() => {
                self.thumbnail_menu = None;
                let page = self
                    .export_range
                    .as_mut()
                    .map_or(&mut self.timeline_page, |picker| &mut picker.page);
                *page = if forward {
                    *page + 1
                } else {
                    page.saturating_sub(1)
                };
                if self.export_range.is_none() {
                    return self.navigation.scroll_to(self.timeline_page, 0.0);
                }
                return iced::widget::operation::scroll_to(
                    self.navigation.scroll_id(),
                    scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
                );
            }
            _ => {}
        }
        Task::none()
    }

    fn selected_range(&self) -> Option<RangeInclusive<u64>> {
        let selected = self.document.as_ref()?.timeline.get(self.selected_entry?)?;
        let (start, end) = (selected.log_start?, selected.log_end?);
        (start <= end).then_some(start..=end)
    }

    fn timeline_has_next(&self) -> bool {
        self.document.as_ref().is_some_and(|doc| {
            (self.displayed_timeline_page() + 1) * PAGE_ROWS < self.timeline_len(doc)
        })
    }

    fn displayed_timeline_page(&self) -> usize {
        self.export_range
            .as_ref()
            .map_or(self.timeline_page, |picker| picker.page)
    }

    fn timeline_len(&self, document: &Journal) -> usize {
        self.timeline_filter()
            .map_or(document.timeline.len(), <[usize]>::len)
    }

    fn timeline_filter(&self) -> Option<&[usize]> {
        self.export_range
            .is_none()
            .then(|| self.navigation.filtered())
            .flatten()
    }

    fn restart_capture(&mut self, id: window::Id) -> Task<Desktop> {
        self.cursors.clear();
        self.cursors.push(None);
        self.load_capture(id)
    }

    fn load_capture(&mut self, id: window::Id) -> Task<Desktop> {
        self.page_revision = self.page_revision.wrapping_add(1);
        self.screenshot_request = None;
        let revision = self.page_revision;
        self.loading = true;
        self.error = None;
        self.capture = None;
        let source = self.source.clone();
        let cursor = self.cursors.last().copied().flatten();
        let range = self.range.clone();
        let query = self.search.clone();
        read_task(
            id,
            move || {
                let query = (!query.is_empty()).then_some(query.as_str());
                source.capture_page(cursor, range, query)
            },
            move |result| Message::CaptureLoaded(revision, result),
        )
    }

    fn view<'a>(
        &'a self,
        links_allowed: bool,
        rename_allowed: bool,
        entry_controls: entry_editor::Controls<'a>,
        screenshot_interactive: bool,
        screenshot_analysis: bool,
        appearance: AppearancePage<'a>,
    ) -> Element<'a, Message> {
        if let Some(screenshot) = &self.screenshot {
            let analysis = self.document.as_ref().and_then(|document| {
                document
                    .timeline
                    .iter()
                    .find(|entry| entry.id == screenshot.entry_id())
                    .and_then(TimelineEntry::screenshot_analysis)
            });
            let lightbox = container(
                column![
                    button("Back to timeline").on_press(Message::HideScreenshot),
                    self.entry_flow.status_view(),
                    self.error.as_ref().map(|error| text(error).size(13)),
                    self.screenshot_analysis_notice
                        .as_ref()
                        .map(|notice| text(notice).size(13)),
                    analysis.map(|rows| scrollable(screenshot_ai_access::analysis_card(
                        rows,
                        appearance.content
                    ))
                    .height(98)),
                    screenshot_access::image_view(
                        screenshot,
                        self.page_revision,
                        self.screenshot_menu
                            .map(|point| (point, self.screenshot_menu_choice)),
                        screenshot_interactive,
                        screenshot_analysis,
                    ),
                ]
                .spacing(8)
                .height(Length::Fill),
            )
            .padding(14)
            .into();
            return if screenshot_interactive && self.screenshot_menu.is_none() {
                viewer_keys::lightbox(lightbox)
            } else {
                lightbox
            };
        }
        responsive(move |size| {
            self.document_view(
                size,
                links_allowed,
                rename_allowed,
                entry_controls,
                screenshot_analysis,
                &appearance,
            )
        })
        .into()
    }

    fn document_view<'a>(
        &'a self,
        viewport: Size,
        links_allowed: bool,
        rename_allowed: bool,
        entry_controls: entry_editor::Controls<'a>,
        screenshot_analysis: bool,
        appearance: &AppearancePage<'a>,
    ) -> Element<'a, Message> {
        let content = appearance.content;
        let compact = viewport.height < COMPACT_HEIGHT * content.scale.max(1.0);
        let toolbar = row![
            button("Timeline")
                .on_press_maybe((!self.entry_flow.saving()).then_some(Message::ShowCapture(false))),
            button("Capture log").on_press_maybe(
                (self.document.is_some()
                    && self.export_range.is_none()
                    && !self.entry_flow.saving())
                .then_some(Message::ShowCapture(true))
            ),
            export_menu(self.export_range.is_none() && self.export_summary().is_some()),
            button("Pick an export time range").on_press_maybe(
                (self.export_range.is_none()
                    && self.export_summary().is_some()
                    && self.can_pick_export_range())
                .then_some(Message::Export(export_dialog::Message::BeginRange)),
            ),
            button("Edit")
                .style(if self.entry_editor.is_open() {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press_maybe(
                    entry_controls
                        .enabled
                        .then_some(Message::EntryEditor(entry_editor::Event::Toggle))
                ),
            appearance.appearance_button(Message::Appearance),
            self.refresh_button(),
            space::horizontal(),
            button("Close").on_press(Message::Close)
        ]
        .spacing(8)
        .wrap();
        let viewport = Size::new(
            (viewport.width - if compact { 42.0 } else { 28.0 }).max(1.0),
            viewport.height,
        );
        let page = |size| {
            self.document_page(
                size,
                compact,
                links_allowed,
                rename_allowed,
                screenshot_analysis,
                appearance,
            )
        };
        let center = match &self.document {
            Some(document) if self.entry_editor.is_open() => {
                self.entry_editor
                    .view(document, viewport, compact, entry_controls, page)
            }
            _ => page(viewport),
        };
        let body = column![
            toolbar,
            self.entry_flow.status_view(),
            self.screenshot_analysis_notice
                .as_ref()
                .map(|notice| text(notice).size(13)),
            center
        ]
        .spacing(8)
        .height(body_height(compact));
        if compact {
            container(compact_scroll(body)).padding(14).into()
        } else {
            container(body).padding(14).into()
        }
    }

    fn document_page<'a>(
        &'a self,
        viewport: Size,
        compact: bool,
        links_allowed: bool,
        rename_allowed: bool,
        screenshot_analysis: bool,
        appearance: &AppearancePage<'a>,
    ) -> Element<'a, Message> {
        let content = appearance.content;
        let title = self
            .document
            .as_ref()
            .map_or(&self.name, |doc| &doc.summary.title);
        let title: Element<'a, Message> =
            if rename_allowed && !self.loading && self.document.is_some() {
                tooltip(
                    mouse_area(page_text(content, label(title, 140), 21.0))
                        .on_double_click(Message::Metadata(metadata::Message::Rename))
                        .on_right_press(Message::Metadata(metadata::Message::TitleMenu)),
                    "Double-click to rename the journal",
                    tooltip::Position::Top,
                )
                .into()
            } else {
                page_text(content, label(title, 140), 21.0).into()
            };
        let mut body = column![
            row![
                title,
                space::horizontal(),
                appearance.page_controls(Message::Appearance)
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap()
        ]
        .spacing(10)
        .height(body_height(compact));
        if self.title_menu && rename_allowed {
            body = body.push(
                row![
                    button("Rename journal…")
                        .on_press(Message::Metadata(metadata::Message::Rename)),
                    button("Cancel").on_press(Message::Metadata(metadata::Message::DismissMenu)),
                ]
                .spacing(8),
            );
        }
        if let Some((picker, document)) = self.export_range.as_ref().zip(self.document.as_ref()) {
            body = body.push(picker.view(document));
        }
        if self.capture_shown {
            body = body.push(self.capture_view(compact, content));
        } else if let Some(document) = &self.document {
            let timeline_size = Size::new((viewport.width - 24.0).max(1.0), viewport.height);
            body = body.push(self.timeline(
                document,
                timeline_size,
                links_allowed,
                screenshot_analysis,
                appearance,
            ));
        } else if self.loading {
            body = body.push(page_text(content, "Reading journal…", 13.0));
        }
        if let Some(error) = &self.error {
            body = body.push(page_text(content, error, 13.0));
        }
        if let Some(problem) = self.refresh.problem() {
            body = body.push(page_text(content, problem, 13.0));
        }
        content.wrap(container(body).padding(12).width(Length::Fill).into())
    }

    fn timeline<'a>(
        &'a self,
        document: &'a Journal,
        viewport: Size,
        links_allowed: bool,
        screenshot_analysis: bool,
        appearance: &AppearancePage<'a>,
    ) -> Element<'a, Message> {
        let content = appearance.content;
        let compact = viewport.height < COMPACT_HEIGHT * content.scale.max(1.0);
        let mut rows = column![].spacing(self.navigation.row_spacing());
        let date_width = content.size(155.0).min((viewport.width * 0.30).max(88.0));
        let marker_width = content.size(105.0).min((viewport.width * 0.24).max(62.0));
        let filtered = self.timeline_filter();
        let total = self.timeline_len(document);
        let page = self.displayed_timeline_page();
        let start = page * PAGE_ROWS;
        let searching = self.export_range.is_none();
        let active = searching.then(|| self.navigation.active_hit()).flatten();
        for ordinal in start..(start + PAGE_ROWS).min(total) {
            let index = filtered.map_or(ordinal, |rows| rows[ordinal]);
            let entry = &document.timeline[index];
            let (highlighted, select) = match &self.export_range {
                Some(picker) => (
                    entry.created_at.is_some() && picker.highlights(index),
                    entry.created_at.map(|_| {
                        picker.event(export_dialog::ranges::Event::Select(
                            index,
                            entry.id.clone(),
                        ))
                    }),
                ),
                None => (
                    self.selected_entry == Some(index),
                    Some(Message::SelectEntry(index, entry.id.clone())),
                ),
            };
            let hits = if searching {
                self.navigation.hits(index)
            } else {
                &[]
            };
            let [date, marker, title] = Projection::row_fields(entry);
            let trigger = button(
                row![
                    row_field(date, Length::Fixed(date_width), 0, hits, active, content),
                    row![
                        entry.marker_color.map(|[r, g, b]| {
                            container(space().width(8).height(8)).style(move |_| {
                                container::Style::default().background(Color::from_rgb8(r, g, b))
                            })
                        }),
                        row_field(marker, Length::Fill, 1, hits, active, content),
                    ]
                    .width(marker_width)
                    .spacing(4)
                    .align_y(Alignment::Center),
                    row_field(title, Length::Fill, 2, hits, active, content),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .height(self.navigation.row_height())
            .style(if highlighted {
                button::primary
            } else if searching && self.navigation.is_marker_target(index) {
                button::secondary
            } else {
                button::text
            })
            .on_press_maybe(select)
            .into();
            let header = self.entry_flow.timeline_row(
                trigger,
                index,
                &entry.id,
                searching
                    && appearance.enabled
                    && self.can_edit_entries()
                    && self.entry_flow.context_enabled()
                    && !self.entry_editor.is_dirty(),
            );
            rows = rows.push(
                if let Some(thumbnail) = searching
                    .then(|| {
                        self.thumbnail(
                            index,
                            entry,
                            appearance.enabled
                                && self.can_navigate_timeline()
                                && !self.entry_flow.saving(),
                            screenshot_analysis,
                        )
                    })
                    .flatten()
                {
                    if let Some(analysis) = entry
                        .screenshot_analysis()
                        .filter(|_| viewport.width >= 540.0)
                    {
                        let details =
                            scrollable(screenshot_ai_access::analysis_card(analysis, content))
                                .height(self.navigation.thumbnail_height())
                                .width(Length::Fill);
                        column![header, row![thumbnail, details].spacing(8)].into()
                    } else {
                        column![header, thumbnail].into()
                    }
                } else {
                    header
                },
            );
        }
        if total == 0 {
            rows = rows.push(page_text(
                content,
                if filtered.is_some() {
                    "No matching timeline entries."
                } else {
                    "No timeline entries. The capture log may still contain activity."
                },
                13.0,
            ));
        }
        let summary = &document.summary;
        let endpoint = format!(
            "{}{}{}{}",
            label(&summary.username, 60),
            if summary.username.is_empty() { "" } else { "@" },
            label(&summary.host, 80),
            summary
                .port
                .map_or_else(String::new, |port| format!(":{port}")),
        );
        let mut body = column![
            page_text(
                content,
                format!(
                    "{} · {} · {}",
                    label(&summary.connection_name, 80),
                    endpoint,
                    timestamp(summary.started_at)
                ),
                13.0
            ),
            page_text(
                content,
                format!(
                    "{} · Ended {} · {} entries · {} log parts",
                    label(&summary.source, 40),
                    timestamp(summary.ended_at),
                    summary.entry_count,
                    summary.log_parts
                ),
                12.0
            )
            .color(content.palette.colors.muted.color()),
        ]
        .spacing(8)
        .height(body_height(compact));
        if searching && !self.loading {
            body = body.push(
                row![
                    button(page_text(content, "Search journal", 13.0))
                        .on_press(viewer_navigation::message(Navigation::OpenFind)),
                    button(page_text(content, "Markers", 13.0)).on_press_maybe(
                        self.navigation
                            .has_markers()
                            .then(|| { viewer_navigation::message(Navigation::ToggleMarkers) })
                    ),
                ]
                .spacing(8),
            );
            if self.navigation.find_open() {
                body = body.push(self.navigation.find_view(
                    content.ui_font,
                    appearance.preferences.scale,
                    appearance.enabled,
                ));
            }
            if self.navigation.markers_open() {
                body = body.push(
                    self.navigation
                        .marker_view(content.ui_font, appearance.preferences.scale),
                );
            }
            if let Some(hit) = active
                && let Some(entry) = document.timeline.get(hit.entry)
            {
                body = body.push(
                    rich_text(Projection::new(entry).context(hit, content))
                        .font(content.ui_font)
                        .size(content.size(13.0)),
                );
            }
        }
        body = body
            .push(rule::horizontal(1))
            .push(
                scrollable(rows)
                    .id(self.navigation.scroll_id())
                    .on_scroll(move |viewport| self.navigation.scroll_event(page, viewport))
                    .height(if compact {
                        Length::Fixed(176.0)
                    } else {
                        Length::Fill
                    }),
            )
            .push(page_buttons(page > 0, self.timeline_has_next(), content));
        if searching
            && let Some(index) = self.selected_entry
            && let Some(entry) = document.timeline.get(index)
        {
            let projection = Projection::new(entry);
            let hits = self.navigation.hits(index);
            let chat = self
                .navigation
                .chat_open(index, projection.first_chat_field());
            if !entry.chat_details.is_empty() {
                body = body.push(
                    button(page_text(
                        content,
                        if chat { "Hide AI chat" } else { "Show AI chat" },
                        13.0,
                    ))
                    .on_press(viewer_navigation::message(Navigation::Chat(
                        index,
                        entry.id.clone(),
                    ))),
                );
            }
            let (spans, limited) = projection.detail_spans(
                hits,
                active,
                links_allowed && !self.loading,
                chat,
                content,
            );
            if limited {
                body = body.push(page_text(
                    content,
                    "Detail highlights are limited to the current area.",
                    12.0,
                ));
            }
            if !spans.is_empty() {
                let detail: Element<'_, Message> = if spans.iter().any(|span| span.link.is_some()) {
                    journal_rich_text(
                        index,
                        spans,
                        content.ui_font,
                        content.size(13.0),
                        move |(field, target)| {
                            Message::OpenLink(index, entry.id.clone(), field, target)
                        },
                    )
                } else {
                    rich_text(spans)
                        .font(content.ui_font)
                        .size(content.size(13.0))
                        .into()
                };
                body = if compact {
                    body.push(detail)
                } else {
                    body.push(scrollable(detail).height(Length::Fixed(
                        content.size(96.0).min(viewport.height * 0.33),
                    )))
                };
            }
            if self.selected_range().is_some() {
                let title = entry.log_part.map_or_else(
                    || "View captured activity".into(),
                    |part| format!("View captured activity from part {part}"),
                );
                body = body.push(
                    button(page_text(content, title, 13.0)).on_press(Message::CaptureForEntry),
                );
            }
            if screenshot_access::reference(entry).is_some() {
                body = body.push(
                    button(page_text(content, "View screenshot", 13.0)).on_press_maybe(
                        (!self.loading).then(|| Message::ShowScreenshot(index, entry.id.clone())),
                    ),
                );
            }
        }
        if document.timeline_limited {
            body = body.push(page_text(
                content,
                "The timeline reached its display limit.",
                13.0,
            ));
        }
        body.into()
    }

    fn capture_view(&self, compact: bool, content: Content) -> Element<'_, Message> {
        let mut rows = column![].spacing(8);
        if let Some(page) = &self.capture {
            for entry in &page.records {
                let kind = match entry.kind {
                    CaptureKind::Output => "Output",
                    CaptureKind::Input => "Input",
                    CaptureKind::Seed => "History",
                    CaptureKind::Screenshot => "Screenshot",
                    CaptureKind::Note => "Note",
                };
                rows = rows.push(column![
                    page_text(
                        content,
                        format!(
                            "#{} · {} · {kind}{}{}{}",
                            entry.sequence,
                            timestamp(Some(entry.timestamp)),
                            if entry.redacted { " · redacted" } else { "" },
                            if entry.partial { " · partial" } else { "" },
                            if entry.repeat > 1 {
                                format!(" · ×{}", entry.repeat)
                            } else {
                                String::new()
                            }
                        ),
                        12.0
                    ),
                    text(display(&entry.text, 8192))
                        .font(content.mono_font)
                        .size(content.size(13.0))
                        .color(match entry.kind {
                            CaptureKind::Input => content.palette.colors.input.color(),
                            CaptureKind::Output | CaptureKind::Seed =>
                                content.palette.colors.output.color(),
                            CaptureKind::Screenshot | CaptureKind::Note =>
                                content.palette.colors.muted.color(),
                        }),
                ]);
            }
            if page.records.is_empty() {
                rows = rows.push(page_text(content, "No matching capture entries.", 13.0));
            }
        } else if self.loading {
            rows = rows.push(page_text(content, "Reading capture log…", 13.0));
        }
        let query = text_input("Search the capture log", &self.query)
            .font(content.ui_font)
            .size(content.size(13.0));
        let query = if self.loading {
            query
        } else {
            query
                .on_input(Message::CaptureQuery)
                .on_submit(Message::SearchCapture)
        };
        let mut body = column![
            row![
                query,
                button(page_text(content, "Find", 13.0))
                    .on_press_maybe((!self.loading).then_some(Message::SearchCapture)),
                button(page_text(content, "Show entire log", 13.0)).on_press_maybe(
                    (self.range.is_some() && !self.loading).then_some(Message::ResetRange)
                ),
            ]
            .spacing(8),
            journal_rows(rows, compact),
            page_buttons(
                !self.loading && self.cursors.len() > 1,
                !self.loading
                    && self
                        .capture
                        .as_ref()
                        .is_some_and(|page| page.next.is_some()),
                content,
            ),
        ]
        .spacing(8)
        .height(body_height(compact));
        if let Some(notice) = self.capture.as_ref().and_then(|page| page.status.notice()) {
            body = body.push(page_text(content, notice, 13.0));
        }
        body.into()
    }
}

fn body_height(compact: bool) -> Length {
    if compact {
        Length::Shrink
    } else {
        Length::Fill
    }
}

fn export_menu(available: bool) -> Element<'static, Message> {
    if available {
        pick_list(ExportFormat::ALL, None::<ExportFormat>, |format| {
            Message::Export(export_dialog::Message::Open(format))
        })
        .placeholder("Export")
        .into()
    } else {
        button("Export").into()
    }
}

fn journal_rows(rows: Column<'_, Message>, compact: bool) -> Element<'_, Message> {
    if compact {
        rows.into()
    } else {
        scrollable(rows).height(Length::Fill).into()
    }
}

fn compact_scroll(body: Column<'_, Message>) -> Element<'_, Message> {
    scrollable(container(body).padding(iced::padding::right(14)))
        .height(Length::Fill)
        .into()
}

fn catalog_task(id: window::Id, revision: u64, root: PathBuf) -> Task<Desktop> {
    let source = root.clone();
    read_task(
        id,
        move || session_journal::catalog(&root),
        move |result| Message::CatalogLoaded(revision, source, result),
    )
}

fn read_task<T: Send + Sync + 'static>(
    id: window::Id,
    read: impl FnOnce() -> io::Result<T> + Send + 'static,
    result: impl FnOnce(Result<Arc<T>, String>) -> Message + Send + 'static,
) -> Task<Desktop> {
    Task::future(async move {
        let (send, receive) = async_channel::bounded(1);
        let spawn = std::thread::Builder::new()
            .name("kortty-journal-reader".into())
            .spawn(move || {
                let _ = send.try_send(read().map(Arc::new).map_err(|error| error.to_string()));
            });
        let response = match spawn {
            Ok(_) => receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Journal reader stopped.".into())),
            Err(error) => Err(error.to_string()),
        };
        Desktop::SessionJournal(id, result(response))
    })
}

fn page_buttons<'a>(previous: bool, next: bool, content: Content) -> Element<'a, Message> {
    row![
        button(page_text(content, "Previous", 13.0))
            .on_press_maybe(previous.then_some(Message::Page(false))),
        button(page_text(content, "Next", 13.0))
            .on_press_maybe(next.then_some(Message::Page(true))),
    ]
    .spacing(8)
    .into()
}

fn timestamp(value: Option<Timestamp>) -> String {
    value.map_or_else(
        || "—".into(),
        |timestamp| timestamp.strftime("%Y-%m-%d %H:%M:%S UTC").to_string(),
    )
}

pub(super) fn journal_field_spans<'a, L>(
    entry: &'a TimelineEntry,
    field: LinkField,
    limit: usize,
    link: impl Fn(&LinkTarget) -> L,
) -> Vec<text::Span<'a, L, Font>> {
    entry
        .link_spans(field, limit)
        .into_iter()
        .map(|segment| {
            let part = span(segment.text);
            match segment.target {
                Some(target) => part.underline(true).link(link(target)),
                None => part,
            }
        })
        .collect()
}

fn display(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let mut result: String = characters
        .by_ref()
        .take(limit)
        .map(|c| match c {
            '\n' | '\t' => c,
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '�',
            c if c.is_control() => '�',
            c => c,
        })
        .collect();
    if characters.next().is_some() {
        result.push('…');
    }
    result
}

fn label(value: &str, limit: usize) -> String {
    display(value, limit).replace(['\n', '\t'], " ")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod manager_tree_tests;
