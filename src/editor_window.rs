//! Native UTF-8 editor for local and SFTP source files.

#[cfg(any(unix, test))]
use std::fs;
use std::{
    collections::VecDeque,
    fmt,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iced::{
    Alignment, Element, Font, Length, Task, Theme,
    widget::{
        self, button, checkbox, column, container, pick_list, row, scrollable, text, text_editor,
        text_input,
    },
};
use kortty_ssh::sftp::{MAX_EDIT_BYTES as MAX_FILE_BYTES, RemoteText};

use crate::profile::{MAX_TERMINAL_FONT as MAX_FONT_SIZE, MIN_TERMINAL_FONT as MIN_FONT_SIZE};
use crate::sftp_window::RemoteEditor;
use crate::terminal_theme::Rgb;

pub(crate) mod caret;
mod line_endings;
pub(crate) mod lua;
mod search;
pub(crate) mod structure;
pub(crate) mod tools;

use line_endings::Conversion;
pub(crate) use search::{MAX_QUERY_BYTES, literal_cursor, source_selection};

const MAX_OUTLINE_ITEMS: usize = 256;
const MAX_HISTORY_STEPS: usize = 32;
const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;
const EDIT_GROUP_TIMEOUT: Duration = Duration::from_millis(650);
const FONT_SIZE_STEP: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Appearance {
    pub(crate) font: Font,
    pub(crate) size: u8,
    pub(crate) foreground: Rgb,
    pub(crate) background: Rgb,
}

impl Appearance {
    pub(crate) fn style(self, theme: &Theme, status: text_editor::Status) -> text_editor::Style {
        text_editor::Style {
            background: self.background.color().into(),
            value: self.foreground.color(),
            ..text_editor::default(theme, status)
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    RequestClose,
    RequestQuit,
    Edit(text_editor::Action),
    Cut,
    SetPath(String),
    Open,
    Save,
    SaveAndClose,
    Opened(Result<Document, String>),
    Saved(Result<Document, String>),
    DecideDiscard(bool),
    RefreshOutline,
    OutlineReady(u64, Outline),
    Jump { revision: u64, item: usize },
    CompleteLua,
    ChooseLua(&'static str),
    DismissLua,
    Tools(tools::Message),
    ShowFind,
    HideFind,
    SetFind(String),
    SetReplacement(String),
    FindRegex(bool),
    FindCaseSensitive(bool),
    FindSelectionOnly(bool),
    Find(SearchDirection),
    ReplaceOne,
    ReplaceAll,
    ConvertLineEndings(Conversion),
    Undo,
    Redo,
    ZoomIn(u8),
    ZoomOut(u8),
    ResetZoom,
}

#[derive(Debug, Clone, Copy)]
pub enum SearchDirection {
    Next,
    Previous,
}

#[derive(Clone)]
pub struct Document {
    source: DocumentSource,
    original: Arc<str>,
    local_read_only: bool,
}

#[derive(Debug, Clone)]
enum DocumentSource {
    Local(PathBuf),
    Remote {
        document: RemoteText,
        editor: RemoteEditor,
        label: String,
    },
}

impl std::fmt::Debug for Document {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Document")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl Document {
    fn local(path: PathBuf, original: Arc<str>) -> Self {
        Self {
            source: DocumentSource::Local(path),
            original,
            local_read_only: false,
        }
    }

    pub(crate) fn remote(document: RemoteText, editor: RemoteEditor, label: String) -> Self {
        Self {
            original: Arc::clone(document.text()),
            local_read_only: false,
            source: DocumentSource::Remote {
                document,
                editor,
                label,
            },
        }
    }

    pub(crate) fn remote_editor(&self) -> Option<&RemoteEditor> {
        match &self.source {
            DocumentSource::Local(_) => None,
            DocumentSource::Remote { editor, .. } => Some(editor),
        }
    }
}

#[derive(Debug, Clone)]
enum Discard {
    Open(PathBuf),
    Close,
}

#[derive(Debug, Clone, Default)]
pub struct Outline {
    items: Vec<OutlineItem>,
    limited: bool,
}

#[derive(Debug, Clone)]
struct OutlineItem {
    line: usize,
    column: usize,
    end_column: usize,
    kind: &'static str,
    name: String,
}

pub struct EditorWindow {
    content: text_editor::Content,
    syntax_source: u64,
    endings: line_endings::State,
    font_override: Option<u8>,
    editor_id: widget::Id,
    find_id: widget::Id,
    find_visible: bool,
    search: search::Search,
    undo: Snapshots,
    redo: Snapshots,
    edit_group: Option<(EditGroup, Instant)>,
    path: String,
    document: Option<Document>,
    status: Option<String>,
    dirty: bool,
    busy: bool,
    discard: Option<Discard>,
    close_ready: bool,
    revision: u64,
    outline: Outline,
    outline_revision: Option<u64>,
    lua_completion: Option<lua::Completion>,
    tools: tools::Panel,
}

impl Default for EditorWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorWindow {
    pub(crate) fn with_unsaved_draft(text: &str) -> (Self, Task<Message>) {
        let mut editor = Self::new();
        editor.load_text(text);
        editor.mark_edited();
        editor.status =
            Some("Review the generated draft and enter a new local path to save it.".into());
        let outline = editor.refresh_outline();
        (editor, outline)
    }

    pub(crate) fn with_file(path: PathBuf) -> (Self, Task<Message>) {
        let mut editor = Self::new();
        let opened = editor.open(path);
        (editor, opened)
    }

    pub(crate) fn with_project_file(path: PathBuf) -> (Self, Task<Message>) {
        let mut editor = Self::new();
        editor.busy = true;
        editor.status = Some("Opening…".into());
        let opened = Task::perform(
            async move {
                let open = || crate::profile::projects::open_local_file(&path);
                let original = read_opened_file(open)
                    .map_err(|error| format!("Could not open project file ({error})"))?;
                let path = canonical_target(&path).map_err(|error| error.to_string())?;
                Ok(Document::local(path, original))
            },
            Message::Opened,
        );
        (editor, opened)
    }

    pub(crate) fn local_document_path(&self) -> Option<&Path> {
        match &self.document.as_ref()?.source {
            DocumentSource::Local(path) => Some(path),
            DocumentSource::Remote { .. } => None,
        }
    }

    pub(crate) fn needs_close_decision(&self) -> bool {
        self.dirty || self.busy
    }

    pub(crate) fn cancel_hosted_close(&mut self) {
        if matches!(self.discard, Some(Discard::Close)) {
            self.discard = None;
        }
        self.close_ready = false;
    }

    pub(crate) fn with_remote(
        document: RemoteText,
        editor: RemoteEditor,
        label: String,
    ) -> (Self, Task<Message>) {
        Self::with_document(Document::remote(document, editor, label), None, None)
    }

    pub(crate) fn with_document(
        document: Document,
        line: Option<usize>,
        column: Option<usize>,
    ) -> (Self, Task<Message>) {
        let mut window = Self::new();
        window.load_text(&document.original);
        if let Some(line) = line {
            let line = line
                .saturating_sub(1)
                .min(window.content.line_count().saturating_sub(1));
            let column = column.unwrap_or(1).saturating_sub(1);
            let column = window.content.line(line).map_or(0, |text| {
                text.text
                    .char_indices()
                    .nth(column)
                    .map_or(text.text.len(), |(byte, _)| byte)
            });
            window.content.move_to(text_editor::Cursor {
                position: text_editor::Position { line, column },
                selection: None,
            });
        }
        window.install(document, "Opened");
        if !window.writable() {
            window.status = Some(format!("Read-only {}", window.path));
        }
        let outline = window.refresh_outline();
        (window, outline)
    }

    pub fn new() -> Self {
        Self {
            content: text_editor::Content::new(),
            syntax_source: lua::source_id(),
            endings: line_endings::State::default(),
            font_override: None,
            editor_id: widget::Id::unique(),
            find_id: widget::Id::unique(),
            find_visible: false,
            search: search::Search::default(),
            undo: Snapshots::default(),
            redo: Snapshots::default(),
            edit_group: None,
            path: String::new(),
            document: None,
            status: None,
            dirty: false,
            busy: false,
            discard: None,
            close_ready: false,
            revision: 0,
            outline: Outline::default(),
            outline_revision: Some(0),
            lua_completion: None,
            tools: tools::Panel::default(),
        }
    }

    fn font_size(&self, inherited: u8) -> u8 {
        self.font_override.unwrap_or(inherited)
    }

    /// Returns whether the window can close immediately.
    pub fn request_close(&mut self) -> bool {
        self.tools.invalidate(None);
        if self.busy {
            self.status = Some("A file operation is in progress.".into());
            return false;
        }
        if self.dirty {
            self.discard = Some(Discard::Close);
            return false;
        }
        true
    }

    /// Reports an explicitly confirmed close to the owning workspace.
    pub fn may_close(&self) -> bool {
        self.close_ready
    }

    pub(crate) fn close_decision_available(&self) -> bool {
        !self.busy && matches!(self.discard, Some(Discard::Close))
    }

    pub(crate) fn remote_editor(&self) -> Option<&RemoteEditor> {
        self.document.as_ref()?.remote_editor()
    }

    pub(crate) fn can_cut(&self) -> bool {
        self.available() && self.writable()
    }

    pub(crate) fn cut_selection(&mut self) -> Option<String> {
        if !self.can_cut() {
            return None;
        }
        let selection = self.content.selection().filter(|text| !text.is_empty())?;
        self.perform(text_editor::Action::Edit(text_editor::Edit::Delete));
        Some(selection)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.update_inner(message);
        if self.tools.active() {
            let context = self.tool_context();
            self.tools.invalidate(context);
        }
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Edit(action) if self.available() && (self.writable() || !action.is_edit()) => {
                if let text_editor::Action::Edit(text_editor::Edit::Paste(text)) = &action
                    && text.len() > MAX_FILE_BYTES
                {
                    self.status = Some(file_too_large().to_string());
                    return Task::none();
                }
                self.perform(action);
            }
            Message::SetPath(path) if self.can_set_path() => {
                self.lua_completion = None;
                self.path = path;
                self.revision = self.revision.wrapping_add(1);
            }
            Message::Open if self.available() && !self.is_remote() && self.writable() => {
                if let Some(path) = self.typed_path() {
                    if self.dirty {
                        self.discard = Some(Discard::Open(path));
                    } else {
                        return self.open(path);
                    }
                }
            }
            Message::Save if self.available() && self.writable() => {
                if self.is_remote() {
                    return self.save_remote();
                } else if let Some(path) = self.typed_path() {
                    return self.save(path);
                }
            }
            Message::SaveAndClose if self.close_decision_available() && self.writable() => {
                return self.save_before_close();
            }
            Message::Opened(result) => {
                self.busy = false;
                match result {
                    Ok(document) => {
                        self.load_text(&document.original);
                        self.search.invalidate_scope();
                        self.undo = Snapshots::default();
                        self.redo = Snapshots::default();
                        self.install(document, "Opened");
                        return self.refresh_outline();
                    }
                    Err(error) => self.status = Some(error),
                }
            }
            Message::Saved(result) => {
                self.busy = false;
                match result {
                    Ok(document) => {
                        self.install(document, "Saved");
                        if matches!(self.discard, Some(Discard::Close)) {
                            self.discard = None;
                            self.close_ready = true;
                            return Task::none();
                        }
                        return self.refresh_outline();
                    }
                    Err(error) => self.status = Some(error),
                }
            }
            Message::DecideDiscard(accept) if !self.busy => {
                if let Some(discard) = self.discard.take().filter(|_| accept) {
                    match discard {
                        Discard::Close => self.close_ready = true,
                        Discard::Open(path) => return self.open(path),
                    }
                }
            }
            Message::RefreshOutline if self.available() => return self.refresh_outline(),
            Message::CompleteLua if self.available() && self.writable() && self.is_lua() => {
                self.lua_completion = lua::Completion::open(&self.content);
                if self.lua_completion.is_none() {
                    self.status = Some("No Lua completions at the caret.".into());
                }
            }
            Message::ChooseLua(item) if self.available() && self.writable() && self.is_lua() => {
                if let Some(action) = self
                    .lua_completion
                    .take()
                    .and_then(|completion| completion.edit(&mut self.content, item))
                {
                    self.perform(action);
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::DismissLua => self.lua_completion = None,
            Message::Tools(message) => return self.update_tools(message),
            Message::OutlineReady(revision, outline) if revision == self.revision => {
                self.outline = outline;
                self.outline_revision = Some(revision);
            }
            Message::Jump { revision, item }
                if self.available() && self.outline_is_current() && revision == self.revision =>
            {
                let Some(item) = self.outline.items.get(item) else {
                    return Task::none();
                };
                if !structure::select_name(
                    &mut self.content,
                    item.line,
                    item.column..item.end_column,
                ) {
                    return Task::none();
                }
                self.edit_group = None;
                return widget::operation::focus(self.editor_id.clone());
            }
            Message::ShowFind if self.available() => {
                self.find_visible = true;
                return widget::operation::focus(self.find_id.clone());
            }
            Message::HideFind if self.available() => {
                self.find_visible = false;
                return widget::operation::focus(self.editor_id.clone());
            }
            Message::SetFind(query) if self.available() => {
                if let Err(error) = self.search.set_query(query) {
                    self.status = Some(error.into());
                }
            }
            Message::SetReplacement(replacement) if self.available() && self.writable() => {
                if let Err(error) = self.search.set_replacement(replacement) {
                    self.status = Some(error.into());
                }
            }
            Message::FindRegex(enabled) if self.available() => self.search.regex = enabled,
            Message::FindCaseSensitive(enabled) if self.available() => {
                self.search.case_sensitive = enabled
            }
            Message::FindSelectionOnly(enabled) if self.available() => {
                if let Err(error) = self.search.set_scope(enabled, &self.content) {
                    self.status = Some(error.into());
                }
            }
            Message::Find(direction) if self.available() => {
                if self.find(direction) {
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::ReplaceOne if self.available() && self.writable() => {
                if self.replace(false) {
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::ReplaceAll if self.available() && self.writable() => {
                if self.replace(true) {
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::ConvertLineEndings(conversion) if self.available() && self.writable() => {
                if self.convert_line_endings(conversion) {
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::Undo if self.available() && self.writable() => {
                if let Some(snapshot) = self.undo.pop() {
                    self.redo
                        .push(Snapshot::capture(&self.content, self.endings));
                    self.restore(snapshot);
                    self.status = Some("Undid last change.".into());
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::Redo if self.available() && self.writable() => {
                if let Some(snapshot) = self.redo.pop() {
                    self.undo
                        .push(Snapshot::capture(&self.content, self.endings));
                    self.restore(snapshot);
                    self.status = Some("Redid change.".into());
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::ZoomIn(inherited) if self.available() => {
                let size = self.font_size(inherited).saturating_add(FONT_SIZE_STEP);
                let size = size.min(MAX_FONT_SIZE);
                self.font_override = (size != inherited).then_some(size);
            }
            Message::ZoomOut(inherited) if self.available() => {
                let size = self.font_size(inherited).saturating_sub(FONT_SIZE_STEP);
                let size = size.max(MIN_FONT_SIZE);
                self.font_override = (size != inherited).then_some(size);
            }
            Message::ResetZoom if self.available() => self.font_override = None,
            _ => {}
        }
        Task::none()
    }

    fn available(&self) -> bool {
        !self.busy && self.discard.is_none() && !self.close_ready
    }

    fn is_remote(&self) -> bool {
        matches!(
            self.document.as_ref().map(|document| &document.source),
            Some(DocumentSource::Remote { .. })
        )
    }

    fn writable(&self) -> bool {
        !self
            .document
            .as_ref()
            .is_some_and(|document| document.local_read_only)
            && self.remote_editor().is_none_or(RemoteEditor::writable)
    }

    fn can_set_path(&self) -> bool {
        !self.is_remote()
            && self.writable()
            && (self.available() || (self.document.is_none() && self.close_decision_available()))
    }

    fn perform(&mut self, action: text_editor::Action) {
        self.lua_completion = None;
        if action.is_edit() {
            let pasted_endings = matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(text))
                if text.contains(['\r', '\n']));
            let action = match self.endings.prepare_edit(action) {
                Ok(action) => action,
                Err(error) => {
                    self.status = Some(error.into());
                    return;
                }
            };
            let old_count = self.content.line_count();
            let cursor = self.content.cursor();
            let selected_lines = cursor
                .selection
                .is_some_and(|anchor| anchor.line != cursor.position.line);
            let group = match action {
                text_editor::Action::Edit(text_editor::Edit::Insert(_)) => Some(EditGroup::Typing),
                text_editor::Action::Edit(text_editor::Edit::Backspace) => {
                    Some(EditGroup::Backspace)
                }
                text_editor::Action::Edit(text_editor::Edit::Delete) => Some(EditGroup::Delete),
                _ => None,
            };
            let group = group.filter(|_| cursor.selection.is_none());
            self.remember_edit(group);
            self.search.invalidate_scope();
            self.mark_edited();
            self.content.perform(action);
            self.endings
                .observe_edit(&self.content, old_count, selected_lines, pasted_endings);
        } else {
            self.edit_group = None;
            self.content.perform(action);
        }
    }

    fn remember_edit(&mut self, group: Option<EditGroup>) {
        let now = Instant::now();
        let grouped = group.is_some_and(|group| {
            self.edit_group.is_some_and(|(previous, since)| {
                previous == group && now.duration_since(since) < EDIT_GROUP_TIMEOUT
            })
        });
        if !grouped {
            self.undo
                .push(Snapshot::capture(&self.content, self.endings));
        }
        self.redo = Snapshots::default();
        self.edit_group = group.map(|group| (group, now));
    }

    fn mark_edited(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.lua_completion = None;
        self.replace_content(text_editor::Content::with_text(&snapshot.text));
        self.content.move_to(text_editor::Cursor {
            position: clamp_position(&self.content, snapshot.cursor.position),
            selection: snapshot
                .cursor
                .selection
                .map(|position| clamp_position(&self.content, position)),
        });
        self.endings = snapshot.endings;
        self.endings.update_from_text(&snapshot.text);
        self.search.invalidate_scope();
        self.dirty = self.document.as_ref().map_or_else(
            || !snapshot.text.is_empty(),
            |document| snapshot.text.as_str() != &*document.original,
        );
        self.edit_group = None;
        self.revision = self.revision.wrapping_add(1);
    }

    fn find(&mut self, direction: SearchDirection) -> bool {
        let result = self.search.cursor(&self.content, direction);
        self.edit_group = None;
        match result {
            Ok(cursor) => {
                self.content.move_to(cursor);
                let line = cursor
                    .selection
                    .map_or(cursor.position.line, |start| start.line);
                self.status = Some(format!("Found on line {}", line + 1));
                true
            }
            Err(error) => {
                self.status = Some(error.into());
                false
            }
        }
    }

    fn replace(&mut self, all: bool) -> bool {
        let result = search::Text::read(&self.content).and_then(|source| {
            let plan = self.search.plan(&source)?;
            let replacement = self.search.replacement_for_edit()?;
            let converted = self.endings.convert_inserted(replacement)?;
            let replacement = converted.as_deref().unwrap_or(replacement);
            let rewritten = if all {
                source.rewrite(plan.hits(&source), replacement)?
            } else {
                let hit = plan
                    .hit(&source, SearchDirection::Next, true)?
                    .ok_or("No matches.")?;
                source.rewrite(std::iter::once(Ok(hit)), replacement)?
            };
            if rewritten.count == 0 {
                return Err("No changes to replace.");
            }
            Ok((source.raw, rewritten))
        });
        let (original, rewritten) = match result {
            Ok(change) => change,
            Err(error) => {
                self.status = Some(error.into());
                return false;
            }
        };
        let cursor = self.content.cursor();
        self.search
            .update_scope(original.len(), rewritten.output.len());
        self.undo.push(Snapshot {
            text: original,
            cursor,
            endings: self.endings,
        });
        self.redo = Snapshots::default();
        self.edit_group = None;
        self.replace_content(text_editor::Content::with_text(&rewritten.output));
        self.endings.update_from_text(&rewritten.output);
        let position = if all {
            clamp_position(&self.content, cursor.position)
        } else {
            search::position_after_edit(&self.content, rewritten.position)
        };
        self.content.move_to(text_editor::Cursor {
            position,
            selection: None,
        });
        self.mark_edited();
        self.status = Some(if rewritten.count == 1 {
            "Replaced one match.".into()
        } else {
            format!("Replaced {} matches.", rewritten.count)
        });
        true
    }

    fn load_text(&mut self, text: &str) {
        self.replace_content(text_editor::Content::with_text(text));
        self.endings = line_endings::State::from_text(text);
    }

    fn replace_content(&mut self, replacement: text_editor::Content) {
        self.content = replacement;
        self.syntax_source = lua::source_id();
        self.lua_completion = None;
    }

    fn convert_line_endings(&mut self, conversion: Conversion) -> bool {
        let converted = match line_endings::convert_content(&self.content, conversion) {
            Ok(converted) => converted,
            Err(error) => {
                self.status = Some(error.into());
                return false;
            }
        };
        let Some(converted) = converted else {
            self.endings.choose(conversion);
            self.status = Some(format!("Line endings use {conversion}."));
            return false;
        };
        let cursor = self.content.cursor();
        self.undo.push(Snapshot {
            text: converted.original,
            cursor,
            endings: self.endings,
        });
        self.redo = Snapshots::default();
        self.edit_group = None;
        self.search.invalidate_scope();
        self.endings.choose(conversion);
        self.endings.update_from_text(&converted.text);
        self.replace_content(text_editor::Content::with_text(&converted.text));
        self.content.move_to(cursor);
        self.mark_edited();
        self.status = Some(format!("Converted line endings to {conversion}."));
        true
    }

    fn typed_path(&mut self) -> Option<PathBuf> {
        if self.path.is_empty() {
            self.status = Some("Enter a local file path.".into());
            None
        } else {
            Some(PathBuf::from(&self.path))
        }
    }

    fn open(&mut self, path: PathBuf) -> Task<Message> {
        self.busy = true;
        self.status = Some("Opening…".into());
        Task::perform(
            async move { open_document(&path).map_err(|error| format!("Could not open file: {error}")) },
            Message::Opened,
        )
    }

    fn save(&mut self, path: PathBuf) -> Task<Message> {
        let original = self.document.clone();
        let contents = if self.dirty || original.is_none() {
            Arc::<str>::from(self.content.text())
        } else {
            original
                .as_ref()
                .map_or_else(|| Arc::from(""), |file| Arc::clone(&file.original))
        };
        self.busy = true;
        self.status = Some("Saving…".into());
        Task::perform(
            async move {
                save_document(&path, contents, original.as_ref())
                    .map_err(|error| format!("Could not save file: {error}"))
            },
            Message::Saved,
        )
    }

    fn save_before_close(&mut self) -> Task<Message> {
        let path = match self.document.as_ref().map(|document| &document.source) {
            Some(DocumentSource::Remote { .. }) => return self.save_remote(),
            Some(DocumentSource::Local(path)) => path.clone(),
            None => match self.typed_path() {
                Some(path) => path,
                None => return Task::none(),
            },
        };
        self.save(path)
    }

    fn save_remote(&mut self) -> Task<Message> {
        let Some(DocumentSource::Remote {
            document,
            editor,
            label,
        }) = self.document.as_ref().map(|document| &document.source)
        else {
            return Task::none();
        };
        if !self.dirty {
            return Task::none();
        }
        let (document, editor, label) = (document.clone(), editor.clone(), label.clone());
        let text = Arc::<str>::from(self.content.text());
        self.busy = true;
        self.status = Some("Saving remote file…".into());
        Task::perform(
            async move {
                editor
                    .save(document, text)
                    .await
                    .map(|document| Document::remote(document, editor, label))
            },
            Message::Saved,
        )
    }

    fn install(&mut self, document: Document, action: &str) {
        self.lua_completion = None;
        self.path = match &document.source {
            DocumentSource::Local(path) => path.to_string_lossy().into_owned(),
            DocumentSource::Remote { label, .. } => label.clone(),
        };
        self.status = Some(format!("{action} {}", self.path));
        self.document = Some(document);
        self.dirty = false;
        self.edit_group = None;
        self.revision = self.revision.wrapping_add(1);
        self.outline = Outline::default();
        self.outline_revision = None;
    }

    fn outline_is_current(&self) -> bool {
        self.outline_revision == Some(self.revision)
    }

    fn source_path(&self) -> &Path {
        let path = match self.document.as_ref().map(|document| &document.source) {
            Some(DocumentSource::Remote { document, .. }) => {
                std::str::from_utf8(document.path()).unwrap_or(&self.path)
            }
            _ => &self.path,
        };
        Path::new(path)
    }

    fn is_lua(&self) -> bool {
        lua::detects(self.source_path(), &self.content)
    }

    fn tool_context(&self) -> Option<tools::Context> {
        if !self.available() || self.remote_editor().is_some_and(RemoteEditor::is_closed) {
            return None;
        }
        let path = self.source_path();
        let language = if self.is_lua() {
            tools::Language::Lua
        } else if let Some(language) = tools::Language::extension(path) {
            language
        } else {
            let first = self.content.line(0)?;
            if Language::detect(path, &first.text) != Language::Shell {
                return None;
            }
            tools::Language::Shell
        };
        Some(tools::Context {
            revision: self.revision,
            language,
            limit: MAX_FILE_BYTES,
            remote: self.is_remote(),
            writable: self.writable(),
        })
    }

    fn update_tools(&mut self, message: tools::Message) -> Task<Message> {
        let context = self.tool_context();
        let remote = self.document.as_ref().and_then(Document::remote_editor);
        match self
            .tools
            .update(message, context, remote, || self.content.text())
        {
            tools::Update::None => {}
            tools::Update::Notice(notice) => self.status = Some(notice),
            tools::Update::Job(job) => return job.task().map(Message::Tools),
            tools::Update::Apply(formatted) => {
                let converted = match self
                    .endings
                    .convert_formatted(&formatted, self.content.line_count())
                {
                    Ok(converted) => converted,
                    Err(error) => {
                        self.status = Some(error.into());
                        return Task::none();
                    }
                };
                let formatted = converted.as_deref().unwrap_or(&formatted);
                if self.content.text() == formatted {
                    self.status = Some("The source is already formatted.".into());
                    return Task::none();
                }
                let replacement = text_editor::Content::with_text(formatted);
                if replacement.text() != formatted {
                    self.status = Some("The editor could not represent the formatted text.".into());
                    return Task::none();
                }
                let cursor = self.content.cursor();
                self.remember_edit(None);
                self.replace_content(replacement);
                self.content.move_to(text_editor::Cursor {
                    position: clamp_position(&self.content, cursor.position),
                    selection: None,
                });
                self.endings.update_from_text(formatted);
                self.search.invalidate_scope();
                self.lua_completion = None;
                self.mark_edited();
                self.status = Some("Formatted the editor buffer.".into());
                return Task::batch([
                    self.refresh_outline(),
                    widget::operation::focus(self.editor_id.clone()),
                ]);
            }
        }
        Task::none()
    }

    fn refresh_outline(&mut self) -> Task<Message> {
        let source = self.content.text();
        let language = Language::detect(self.source_path(), &source);
        let revision = self.revision;
        Task::perform(
            async move { scan_outline(&source, language) },
            move |outline| Message::OutlineReady(revision, outline),
        )
    }

    pub(crate) fn view(
        &self,
        content_theme: Theme,
        appearance: Appearance,
        caret_preferences: caret::Preferences,
    ) -> Element<'_, Message> {
        let available = self.available();
        let inherited = appearance.size;
        let font_size = self.font_size(inherited);
        let remote = self.is_remote();
        let writable = self.writable();
        let is_lua = self.is_lua();
        let path: Element<'_, Message> = if remote {
            container(text(&self.path).wrapping(text::Wrapping::None))
                .width(Length::Fill)
                .clip(true)
                .into()
        } else {
            let path = text_input("Local file path", &self.path).width(Length::Fill);
            if self.can_set_path() {
                path.on_input(Message::SetPath)
                    .on_submit(if available {
                        Message::Open
                    } else {
                        Message::SaveAndClose
                    })
                    .into()
            } else {
                path.into()
            }
        };
        let mut editor = text_editor(&self.content)
            .id(self.editor_id.clone())
            .height(Length::Fill)
            .placeholder("Start typing or open a UTF-8 source file");
        if available {
            editor = editor.on_action(Message::Edit).key_binding(move |event| {
                if is_lua && writable && lua::completion_key(&event) {
                    return Some(text_editor::Binding::Custom(Message::CompleteLua));
                }
                editor_key_binding(event, inherited, writable)
            });
        }
        let editor = caret::view(
            editor.highlight_with::<lua::Lua>(
                lua::Settings::new(is_lua, appearance, self.syntax_source),
                lua::format,
            ),
            content_theme,
            appearance,
            font_size,
            caret_preferences,
        );
        let zoom = row![
            button("−").on_press_maybe(
                (available && font_size > MIN_FONT_SIZE).then_some(Message::ZoomOut(inherited))
            ),
            text(format!("{font_size} pt"))
                .width(42)
                .align_x(Alignment::Center),
            button("+").on_press_maybe(
                (available && font_size < MAX_FONT_SIZE).then_some(Message::ZoomIn(inherited))
            ),
            button("Reset zoom").on_press_maybe(
                (available && self.font_override.is_some()).then_some(Message::ResetZoom),
            ),
        ]
        .spacing(5)
        .align_y(Alignment::Center);
        let mut toolbar = row![];
        if !remote && writable {
            toolbar =
                toolbar.push(button("Open").on_press_maybe(available.then_some(Message::Open)));
        }
        if is_lua && writable {
            toolbar = toolbar.push(
                button("Complete Lua…").on_press_maybe(available.then_some(Message::CompleteLua)),
            );
        }
        for button in self.tools.actions(self.tool_context()) {
            toolbar = toolbar.push(Element::new(button).map(Message::Tools));
        }
        let toolbar = toolbar
            .push(
                button(if remote { "Save to server" } else { "Save" }).on_press_maybe(
                    (available && writable && (!remote || self.dirty)).then_some(Message::Save),
                ),
            )
            .push(
                button(if writable { "Find / Replace" } else { "Find" })
                    .on_press_maybe(available.then_some(Message::ShowFind)),
            )
            .push(button("Undo").on_press_maybe(
                (available && writable && !self.undo.items.is_empty()).then_some(Message::Undo),
            ))
            .push(button("Redo").on_press_maybe(
                (available && writable && !self.redo.items.is_empty()).then_some(Message::Redo),
            ))
            .push(zoom)
            .push(button("Close editor").on_press_maybe(available.then_some(Message::RequestClose)))
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap();

        let mut content = column![path, toolbar].spacing(10);
        if available
            && is_lua
            && let Some(completion) = &self.lua_completion
        {
            content = content.push(completion.view(Message::ChooseLua, Message::DismissLua));
        }
        if let Some(tools) = self.tools.view() {
            content = content.push(tools.map(Message::Tools));
        }
        if self.find_visible {
            content = content.push(self.find_view());
        }
        if let Some(discard) = &self.discard {
            let (question, action) = match discard {
                Discard::Open(_) => (
                    "Discard unsaved changes and open the selected file?",
                    "Discard and open",
                ),
                Discard::Close => (
                    "Save changes before closing the editor?",
                    "Discard and close",
                ),
            };
            let save = (matches!(discard, Discard::Close) && writable).then(|| {
                button("Save and close").on_press_maybe(
                    (!self.busy && (self.document.is_some() || !self.path.is_empty()))
                        .then_some(Message::SaveAndClose),
                )
            });
            let mut choices = row![
                text(question).width(Length::Fill),
                button("Keep editing")
                    .on_press_maybe((!self.busy).then_some(Message::DecideDiscard(false))),
                button(action).on_press_maybe((!self.busy).then_some(Message::DecideDiscard(true))),
            ];
            if let Some(save) = save {
                choices = choices.push(save);
            }
            content = content.push(choices.spacing(8).align_y(Alignment::Center));
        }
        content = content.push(
            row![editor, self.outline_view()]
                .height(Length::Fill)
                .spacing(12),
        );
        let cursor = self.content.cursor().position;
        let column = self
            .content
            .line(cursor.line)
            .map_or(cursor.column, |line| {
                line.text
                    .get(..cursor.column)
                    .map_or(cursor.column, |prefix| prefix.chars().count())
            });
        let mut state = format!("Ln {}  Col {}", cursor.line + 1, column + 1);
        if self.dirty {
            state.push_str("   Unsaved changes");
        }
        let line_count = self.content.line_count();
        let ending_label = self.endings.label(line_count);
        let ending: Element<'_, Message> = if available && writable {
            pick_list(
                Conversion::ALL,
                self.endings.selected(line_count),
                Message::ConvertLineEndings,
            )
            .placeholder(ending_label)
            .padding([3, 7])
            .into()
        } else {
            text(ending_label).size(13).into()
        };
        content = content.push(
            row![
                text(self.status.as_deref().unwrap_or("UTF-8"))
                    .size(13)
                    .width(Length::Fill),
                text("Line endings").size(13),
                ending,
                text(state).size(13),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        container(content)
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn outline_view(&self) -> Element<'_, Message> {
        let current = self.outline_is_current();
        let mut outline = column![].spacing(4);
        if !current {
            outline = outline.push(text("Outline is out of date.").size(13));
        } else if self.outline.items.is_empty() {
            outline = outline.push(text("No supported declarations or placeholders.").size(13));
        } else {
            for (index, item) in self.outline.items.iter().enumerate() {
                let label = format!("{}  {}  {}", item.line + 1, item.kind, item.name);
                outline = outline.push(
                    button(text(label).size(13))
                        .width(Length::Fill)
                        .on_press_maybe(self.available().then_some(Message::Jump {
                            revision: self.revision,
                            item: index,
                        })),
                );
            }
        }
        if self.outline.limited && current {
            outline = outline.push(text("Outline limit reached.").size(13));
        }
        column![
            row![
                text("Structure").width(Length::Fill),
                button("Refresh")
                    .on_press_maybe(self.available().then_some(Message::RefreshOutline)),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
            scrollable(outline).height(Length::Fill),
        ]
        .spacing(8)
        .width(Length::Fixed(230.0))
        .height(Length::Fill)
        .into()
    }

    fn find_view(&self) -> Element<'_, Message> {
        let available = self.available();
        let writable = self.writable();
        let mut find = text_input(
            if self.search.regex {
                "Find regular expression"
            } else {
                "Find text"
            },
            &self.search.query,
        )
        .id(self.find_id.clone())
        .width(Length::Fill);
        let mut replacement =
            text_input("Replacement text", &self.search.replacement).width(Length::Fill);
        if available {
            find = find
                .on_input(Message::SetFind)
                .on_submit(Message::Find(SearchDirection::Next));
        }
        if available && writable {
            replacement = replacement
                .on_input(Message::SetReplacement)
                .on_submit(Message::ReplaceOne);
        }
        let mut controls = column![
            row![
                find,
                button("Previous")
                    .on_press_maybe(available.then_some(Message::Find(SearchDirection::Previous))),
                button("Next")
                    .on_press_maybe(available.then_some(Message::Find(SearchDirection::Next))),
                button("Close").on_press_maybe(available.then_some(Message::HideFind)),
            ]
            .spacing(8),
            row![
                checkbox(self.search.regex)
                    .label("Regex")
                    .on_toggle_maybe(available.then_some(Message::FindRegex)),
                checkbox(self.search.case_sensitive)
                    .label("Case sensitive")
                    .on_toggle_maybe(available.then_some(Message::FindCaseSensitive)),
                checkbox(self.search.selection_only())
                    .label("Selection only")
                    .on_toggle_maybe(available.then_some(Message::FindSelectionOnly)),
            ]
            .spacing(12),
        ];
        if writable {
            controls = controls.push(
                row![
                    replacement,
                    button("Replace").on_press_maybe(available.then_some(Message::ReplaceOne)),
                    button("Replace All").on_press_maybe(available.then_some(Message::ReplaceAll)),
                ]
                .spacing(8),
            );
        }
        controls.spacing(6).into()
    }
}

fn editor_key_binding(
    event: text_editor::KeyPress,
    inherited: u8,
    writable: bool,
) -> Option<text_editor::Binding<Message>> {
    if matches!(event.status, text_editor::Status::Focused { .. })
        && event.modifiers.command()
        && !event.modifiers.alt()
    {
        let shifted = event.modifiers.shift();
        let message = match event.key.to_latin(event.physical_key) {
            Some('w') if !shifted => Some(Message::RequestClose),
            Some('q') if !shifted => Some(Message::RequestQuit),
            Some('f') if !shifted => Some(Message::ShowFind),
            Some('g') => Some(Message::Find(if shifted {
                SearchDirection::Previous
            } else {
                SearchDirection::Next
            })),
            Some('z') => Some(if shifted {
                Message::Redo
            } else {
                Message::Undo
            }),
            Some('y') if !shifted => Some(Message::Redo),
            Some('+' | '=') => Some(Message::ZoomIn(inherited)),
            Some('-') if !shifted => Some(Message::ZoomOut(inherited)),
            Some('0') if !shifted => Some(Message::ResetZoom),
            _ => None,
        };
        if let Some(message) = message {
            return Some(text_editor::Binding::Custom(message));
        }
    }
    let command = event.modifiers.logo() || (event.modifiers.command() && !event.modifiers.alt());
    match text_editor::Binding::from_key_press(event) {
        Some(text_editor::Binding::Cut) => {
            writable.then_some(text_editor::Binding::Custom(Message::Cut))
        }
        binding => binding
            .filter(|binding| !command || !matches!(binding, text_editor::Binding::Insert(_))),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditGroup {
    Typing,
    Backspace,
    Delete,
}

struct Snapshot {
    text: String,
    cursor: text_editor::Cursor,
    endings: line_endings::State,
}

impl Snapshot {
    fn capture(content: &text_editor::Content, endings: line_endings::State) -> Self {
        Self {
            text: content.text(),
            cursor: content.cursor(),
            endings,
        }
    }
}

#[derive(Default)]
struct Snapshots {
    items: VecDeque<Snapshot>,
    bytes: usize,
}

impl Snapshots {
    fn push(&mut self, snapshot: Snapshot) {
        let size = snapshot.text.len();
        if size > MAX_HISTORY_BYTES {
            *self = Self::default();
            return;
        }
        while self.items.len() == MAX_HISTORY_STEPS || self.bytes + size > MAX_HISTORY_BYTES {
            if let Some(removed) = self.items.pop_front() {
                self.bytes -= removed.text.len();
            }
        }
        self.bytes += size;
        self.items.push_back(snapshot);
    }

    fn pop(&mut self) -> Option<Snapshot> {
        let snapshot = self.items.pop_back()?;
        self.bytes -= snapshot.text.len();
        Some(snapshot)
    }
}

fn clamp_position(
    content: &text_editor::Content,
    position: text_editor::Position,
) -> text_editor::Position {
    let line = position.line.min(content.line_count().saturating_sub(1));
    let mut column = 0;
    if let Some(text) = content.line(line) {
        column = position.column.min(text.text.len());
        while !text.text.is_char_boundary(column) {
            column -= 1;
        }
    }
    text_editor::Position { line, column }
}

fn canonical_target(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("enter a file name"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(parent.canonicalize()?.join(name))
}

fn open_document(path: &Path) -> io::Result<Document> {
    let path = canonical_target(path)?;
    let original = read_regular_file(&path)?;
    Ok(Document::local(path, original))
}

pub(crate) fn open_terminal_document(path: &Path, writable: bool) -> io::Result<Document> {
    #[cfg(unix)]
    let mut document = open_document(path)?;
    #[cfg(windows)]
    let mut document = {
        let lease = kortty_files::tree::ExternalLease::open_terminal_path(path)?;
        let path = canonical_target(lease.path())?;
        let (original, stamp) = read_opened_contents(lease.file())?;
        stamp.require_unchanged(&open_regular_editor_file(&path)?)?;
        Document::local(path, original)
    };
    document.local_read_only = !writable;
    Ok(document)
}

fn read_regular_file(path: &Path) -> io::Result<Arc<str>> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("only regular files can be edited"));
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(file_too_large());
    }
    read_opened_file(|| open_regular_editor_file(path))
}

fn read_opened_file(open: impl Fn() -> io::Result<File>) -> io::Result<Arc<str>> {
    let file = open()?;
    let (text, stamp) = read_opened_contents(&file)?;
    stamp.require_unchanged(&open()?)?;
    Ok(text)
}

fn read_opened_contents(file: &File) -> io::Result<(Arc<str>, crate::private_files::FileStamp)> {
    let stamp = crate::private_files::FileStamp::read(file)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(file_too_large());
    }
    stamp.require_unchanged(file)?;
    let text = String::from_utf8(bytes).map_err(|_| io::Error::other("the file is not UTF-8"))?;
    if text.contains('\0') {
        return Err(io::Error::other("the file contains binary data"));
    }
    Ok((Arc::from(text), stamp))
}

fn open_regular_editor_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, open};
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        Ok(File::from(open(path, flags, Mode::empty())?))
    }
    #[cfg(windows)]
    {
        kortty_files::open_regular_without_reparse(path)
    }
}

fn file_too_large() -> io::Error {
    io::Error::other("editor file limit is 2 MiB")
}

#[derive(Debug)]
struct SaveError {
    stage: &'static str,
    source: io::Error,
}

impl fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed ({})", self.stage, self.source)
    }
}

fn save_stage<T>(stage: &'static str, result: io::Result<T>) -> Result<T, SaveError> {
    result.map_err(|source| SaveError { stage, source })
}

fn save_document(
    path: &Path,
    contents: Arc<str>,
    previous: Option<&Document>,
) -> Result<Document, SaveError> {
    if previous.is_some_and(|document| document.local_read_only) {
        return save_stage(
            "checking write access",
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the file is read-only",
            )),
        );
    }
    if contents.len() > MAX_FILE_BYTES {
        return save_stage("checking the document size", Err(file_too_large()));
    }
    if contents.contains('\0') {
        return save_stage(
            "checking the document contents",
            Err(io::Error::other("the document contains binary data")),
        );
    }
    let path = save_stage("resolving the destination", canonical_target(path))?;
    let previous = previous.filter(
        |previous| matches!(&previous.source, DocumentSource::Local(existing) if *existing == path),
    );
    if let Some(previous) = previous {
        save_stage("checking the original file", check_current_file(previous))?;
        if contents == previous.original {
            return Ok(Document::local(path, contents));
        }
    } else {
        let available = match path.symlink_metadata() {
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "open an existing file before replacing it",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        save_stage("checking the new destination", available)?;
    }
    let directory = save_stage(
        "finding the destination folder",
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent directory")),
    )?;
    let mut temporary = save_stage("creating the temporary file", TemporarySave::new(directory))?;
    save_stage(
        "writing the temporary file",
        temporary.file_mut().write_all(contents.as_bytes()),
    )?;
    if previous.is_some() {
        #[cfg(unix)]
        let metadata = path.symlink_metadata();
        #[cfg(windows)]
        let metadata = temporary
            .open_destination(&path)
            .and_then(|file| file.metadata());
        let permissions = save_stage("reading destination attributes", metadata)?.permissions();
        #[cfg(windows)]
        if permissions.readonly() {
            return save_stage(
                "checking write access",
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the file is read-only",
                )),
            );
        }
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            fs::Permissions::from_mode(permissions.mode() & 0o777)
        };
        save_stage(
            "setting temporary file attributes",
            temporary.file_mut().set_permissions(permissions),
        )?;
    }
    save_stage(
        "synchronizing the temporary file",
        temporary.file_mut().sync_all(),
    )?;
    #[cfg(unix)]
    drop(temporary.file.take());
    if let Some(previous) = previous {
        // Several editor windows can save the same path concurrently. Keep
        // the last content check and publication together within this process.
        static SAVE_COMMIT: Mutex<()> = Mutex::new(());
        let _commit = save_stage(
            "acquiring the editor save lock",
            SAVE_COMMIT
                .lock()
                .map_err(|_| io::Error::other("the lock was poisoned")),
        )?;
        temporary.replace(&path, previous)?;
    } else {
        // Publish new files atomically without overwriting another writer.
        save_stage("publishing the new file", temporary.publish_new(&path))?;
    }
    Ok(Document::local(path, contents))
}

#[cfg(windows)]
fn retry_windows_replacement(
    mut replace: impl FnMut() -> Result<(), SaveError>,
) -> Result<(), SaveError> {
    const ACCESS_DENIED: i32 = 5;
    const SHARING_VIOLATION: i32 = 32;
    // Each retry rechecks the original after a transient sharing denial.
    for delay in [10, 25, 50, 100, 200] {
        match replace() {
            Err(error)
                if matches!(
                    error.source.raw_os_error(),
                    Some(ACCESS_DENIED | SHARING_VIOLATION)
                ) =>
            {
                std::thread::sleep(Duration::from_millis(delay));
            }
            result => return result,
        }
    }
    replace()
}

fn check_current_file(previous: &Document) -> io::Result<()> {
    let DocumentSource::Local(path) = &previous.source else {
        return Err(io::Error::other("expected a local file"));
    };
    check_original_contents(read_regular_file(path)?, previous)
}

fn check_original_contents(contents: Arc<str>, previous: &Document) -> io::Result<()> {
    if contents == previous.original {
        Ok(())
    } else {
        Err(io::Error::other(
            "the file changed on disk after it was opened",
        ))
    }
}

struct TemporarySave {
    file: Option<File>,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    directory: kortty_files::tree::Directory,
}

impl TemporarySave {
    fn new(directory: &Path) -> io::Result<Self> {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        #[cfg(unix)]
        let options = {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            options
        };
        #[cfg(windows)]
        let directory = kortty_files::tree::Directory::open_publish_parent(directory)?;
        for _ in 0..32 {
            let number = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let name = format!(".kortty-editor-{}-{number}.tmp", std::process::id());
            #[cfg(unix)]
            let path = directory.join(name);
            #[cfg(unix)]
            let opened = options.open(&path);
            #[cfg(windows)]
            let opened = directory.create_staging_file(std::ffi::OsStr::new(&name));
            match opened {
                Ok(file) => {
                    return Ok(Self {
                        file: Some(file),
                        #[cfg(unix)]
                        path,
                        #[cfg(windows)]
                        directory,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("could not allocate a temporary save file"))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("open temporary save file")
    }

    #[cfg(unix)]
    fn replace(&mut self, path: &Path, previous: &Document) -> Result<(), SaveError> {
        save_stage("rechecking the original file", check_current_file(previous))?;
        save_stage(
            "replacing the destination file",
            fs::rename(&self.path, path),
        )
    }

    #[cfg(windows)]
    fn replace(&mut self, path: &Path, previous: &Document) -> Result<(), SaveError> {
        retry_windows_replacement(|| self.replace_once(path, previous))
    }

    #[cfg(windows)]
    fn replace_once(&mut self, path: &Path, previous: &Document) -> Result<(), SaveError> {
        save_stage(
            "rechecking the original file",
            (|| {
                let contents = read_opened_file(|| self.open_destination(path))?;
                check_original_contents(contents, previous)?;
                self.require_current_parent(path)
            })(),
        )?;
        save_stage(
            "replacing the destination file",
            self.directory.replace_file_with_shared_readers(
                self.file.as_ref().expect("open temporary save file"),
                path.file_name().expect("canonical editor destination"),
            ),
        )?;
        drop(self.file.take());
        Ok(())
    }

    #[cfg(unix)]
    fn publish_new(&mut self, path: &Path) -> io::Result<()> {
        fs::hard_link(&self.path, path)
    }

    #[cfg(windows)]
    fn publish_new(&mut self, path: &Path) -> io::Result<()> {
        self.require_current_parent(path)?;
        self.directory.publish_file(
            self.file.as_ref().expect("open temporary save file"),
            path.file_name().expect("canonical editor destination"),
        )?;
        drop(self.file.take());
        Ok(())
    }

    #[cfg(windows)]
    fn open_destination(&self, path: &Path) -> io::Result<File> {
        self.directory
            .read_child(path.file_name().expect("canonical editor destination"))?
            .ok_or_else(|| io::Error::other("only regular files can be edited"))
    }

    #[cfg(windows)]
    fn require_current_parent(&self, path: &Path) -> io::Result<()> {
        let parent = path.parent().expect("canonical editor destination");
        let current = kortty_files::tree::Directory::open_publish_parent(parent)?;
        if !self.directory.same_as(&current)? {
            return Err(io::Error::other(
                "the destination folder changed during the save",
            ));
        }
        Ok(())
    }
}

impl Drop for TemporarySave {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::remove_file(&self.path);
        #[cfg(windows)]
        if let Some(file) = self.file.take() {
            let _ = kortty_files::tree::remove(file);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Language {
    Rust,
    Python,
    Shell,
    Dockerfile,
    Sql,
    Extended(&'static str),
    Other,
}

impl Language {
    fn detect(path: &Path, source: &str) -> Self {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name == "dockerfile" || name.ends_with(".dockerfile")
            })
        {
            return Self::Dockerfile;
        }
        let extension = path.extension().and_then(|extension| extension.to_str());
        match extension.map(str::to_ascii_lowercase).as_deref() {
            Some("rs") => Self::Rust,
            Some("py" | "pyw") => Self::Python,
            Some("sh" | "bash" | "zsh" | "ksh") => Self::Shell,
            Some("sql") => Self::Sql,
            Some("md" | "markdown") => Self::Extended("markdown"),
            Some("adoc" | "asciidoc") => Self::Extended("asciidoc"),
            _ => {
                if let Some(language) = structure::additional_language(path, source) {
                    return Self::Extended(language);
                }
                let shebang = source
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("#!"));
                for part in shebang.into_iter().flat_map(str::split_whitespace) {
                    match Path::new(part).file_name().and_then(|name| name.to_str()) {
                        Some("python" | "python3") => return Self::Python,
                        Some("sh" | "bash" | "zsh" | "ksh") => return Self::Shell,
                        _ => {}
                    }
                }
                Self::Other
            }
        }
    }
}

fn scan_outline(source: &str, language: Language) -> Outline {
    match language {
        Language::Shell => return structured_outline(source, structure::shell_outline(source)),
        Language::Python => return structured_outline(source, structure::python_outline(source)),
        Language::Dockerfile => {
            return structured_outline(source, structure::analyze(source, "dockerfile"));
        }
        Language::Sql => return structured_outline(source, structure::analyze(source, "sql")),
        Language::Extended(language) => {
            return structured_outline(source, structure::analyze(source, language));
        }
        _ => {}
    }
    let mut outline = Outline::default();
    let mut state = SourceState::default();
    for (line, text) in structure::lines(source).enumerate() {
        if state.line_starts_in_code(text, language)
            && let Some((kind, name)) = declaration(text.trim_start(), language)
        {
            outline.items.push(outline_item(line, text, kind, name));
        }
        for name in placeholders(text) {
            if outline.items.len() >= MAX_OUTLINE_ITEMS {
                outline.limited = true;
                return outline;
            }
            outline
                .items
                .push(outline_item(line, text, "placeholder", name));
        }
        if outline.items.len() >= MAX_OUTLINE_ITEMS {
            outline.limited = true;
            break;
        }
    }
    outline
}

fn structured_outline(source: &str, analysis: structure::Analysis) -> Outline {
    if analysis.status == structure::Status::TooLarge {
        return Outline {
            limited: true,
            ..Outline::default()
        };
    }
    let mut items: Vec<_> = analysis
        .nodes
        .into_iter()
        .map(|node| OutlineItem {
            line: node.line,
            column: node.column,
            end_column: node.end_column,
            kind: node.kind,
            name: node.name,
        })
        .chain(analysis.variables.into_iter().map(|variable| OutlineItem {
            line: variable.line,
            column: variable.column,
            end_column: variable.end_column,
            kind: variable.kind,
            name: variable.name,
        }))
        .collect();
    items.extend(
        structure::lines(source)
            .enumerate()
            .flat_map(|(line, text)| {
                placeholders(text).map(move |name| outline_item(line, text, "placeholder", name))
            })
            .take(MAX_OUTLINE_ITEMS + 1),
    );
    items.sort_by_key(|item| item.line);
    let limited = analysis.limited || items.len() > MAX_OUTLINE_ITEMS;
    items.truncate(MAX_OUTLINE_ITEMS);
    Outline { items, limited }
}

fn outline_item(line: usize, text: &str, kind: &'static str, name: &str) -> OutlineItem {
    let column = structure::column_of(text, name);
    OutlineItem {
        line,
        column,
        end_column: column + name.len(),
        kind,
        name: name.to_owned(),
    }
}

fn declaration(mut text: &str, language: Language) -> Option<(&'static str, &str)> {
    match language {
        Language::Rust => {
            if let Some(visible) = text.strip_prefix("pub(") {
                text = visible.split_once(')')?.1.trim_start();
            } else if let Some(visible) = text.strip_prefix("pub ") {
                text = visible.trim_start();
            }
            if let Some((kind, body)) = leading_keyword(text, &["let", "const", "static"]) {
                let mut body = body;
                while let Some((_, rest)) = leading_keyword(body, &["mut", "ref"]) {
                    body = rest;
                }
                if let Some((name, tail)) = identifier(body) {
                    let tail = tail.trim_start();
                    if name != "_"
                        && (tail.starts_with(':') && !tail.starts_with("::")
                            || kind == "let" && tail.starts_with(['=', ';']))
                    {
                        return Some(("variable", name));
                    }
                }
            }
            while let Some(qualifier) = ["async ", "unsafe ", "const "]
                .into_iter()
                .find_map(|prefix| text.strip_prefix(prefix))
            {
                text = qualifier.trim_start();
            }
            let (kind, body) = ["fn", "struct", "enum", "trait", "mod", "type"]
                .into_iter()
                .find_map(|kind| {
                    text.strip_prefix(kind)
                        .and_then(after_space)
                        .map(|body| (kind, body))
                })?;
            let (name, tail) = identifier(body)?;
            let expected: &[char] = match kind {
                "fn" => &['(', '<'],
                "struct" => &['{', '(', '<', ';'],
                "enum" | "trait" => &['{', '<'],
                "mod" => &['{', ';'],
                "type" => &['=', '<', ';'],
                _ => return None,
            };
            let tail = tail.trim_start();
            (tail.starts_with(expected) || kind != "fn" && tail.is_empty()).then_some((kind, name))
        }
        Language::Python => {
            if let Some((name, tail)) = identifier(text) {
                let tail = tail.trim_start();
                let annotated = tail.strip_prefix(':').is_some_and(|annotation| {
                    let annotation = annotation.trim_start();
                    identifier(annotation).is_some() || annotation.starts_with(['\'', '"', '('])
                });
                if annotated || tail.starts_with('=') && !tail.starts_with("==") {
                    return Some(("variable", name));
                }
            }
            text = text.strip_prefix("async ").unwrap_or(text).trim_start();
            let (kind, body) = ["def", "class"].into_iter().find_map(|kind| {
                text.strip_prefix(kind)
                    .and_then(after_space)
                    .map(|body| (kind, body))
            })?;
            let (name, tail) = identifier(body)?;
            let tail = tail.trim_start();
            (tail.starts_with('(') || kind == "class" && tail.starts_with(':'))
                .then_some((kind, name))
        }
        Language::Shell
        | Language::Dockerfile
        | Language::Sql
        | Language::Extended(_)
        | Language::Other => None,
    }
}

fn leading_keyword<'a, 'b>(text: &'a str, keywords: &'b [&str]) -> Option<(&'b str, &'a str)> {
    keywords.iter().find_map(|&keyword| {
        text.strip_prefix(keyword)
            .and_then(after_space)
            .map(|body| (keyword, body))
    })
}

fn after_space(text: &str) -> Option<&str> {
    text.starts_with(char::is_whitespace)
        .then_some(text.trim_start())
}

fn identifier(text: &str) -> Option<(&str, &str)> {
    let first = text.chars().next()?;
    if first != '_' && !first.is_alphabetic() {
        return None;
    }
    let end = text
        .char_indices()
        .find(|&(_, character)| character != '_' && !character.is_alphanumeric())
        .map_or(text.len(), |(offset, _)| offset);
    Some(text.split_at(end))
}

fn placeholders(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        loop {
            let (prefix, tail) = text.split_once("${")?;
            let (name, rest) = tail.split_once('}')?;
            let token = &text[prefix.len()..text.len() - rest.len()];
            text = rest;
            if identifier(name).is_some_and(|(_, rest)| rest.is_empty()) {
                return Some(token);
            }
        }
    })
}

#[derive(Default)]
struct SourceState {
    block_comments: usize,
    literal: Option<Literal>,
    shell_arithmetic_depth: usize,
    python_group_depth: usize,
    python_continuation: bool,
}

#[derive(Clone, Copy)]
enum Literal {
    Quote(u8),
    TripleQuote(u8),
    RustRaw(usize),
}

impl SourceState {
    fn line_starts_in_code(&mut self, line: &str, language: Language) -> bool {
        self.scan_line(line, language, |_, _| {})
    }

    // The second argument to the visitor identifies shell double-quoted expansions.
    fn scan_line(
        &mut self,
        line: &str,
        language: Language,
        mut visit: impl FnMut(usize, bool),
    ) -> bool {
        if language == Language::Other {
            return false;
        }
        let code = self.block_comments == 0
            && self.literal.is_none()
            && self.shell_arithmetic_depth == 0
            && self.python_group_depth == 0
            && !self.python_continuation;
        self.python_continuation = false;
        let bytes = line.as_bytes();
        let dollar_expansions = matches!(language, Language::Shell | Language::Dockerfile);
        let mut offset = 0;
        while offset < bytes.len() {
            let rest = &bytes[offset..];
            if self.block_comments != 0 {
                if language != Language::Sql && rest.starts_with(b"/*") {
                    self.block_comments += 1;
                    offset += 2;
                } else if rest.starts_with(b"*/") {
                    self.block_comments -= 1;
                    offset += 2;
                } else {
                    offset += 1;
                }
                continue;
            }
            if let Some(literal) = self.literal {
                let closing = closing_literal(literal, rest);
                if closing > 0 {
                    if language == Language::Sql {
                        if matches!(literal, Literal::Quote(b'\'')) && rest.starts_with(b"''") {
                            offset += 2;
                            continue;
                        }
                        visit(offset, false);
                    }
                    self.literal = None;
                    offset += closing;
                } else if rest[0] == b'\\'
                    && language != Language::Sql
                    && !matches!(literal, Literal::RustRaw(_))
                    && !(dollar_expansions && matches!(literal, Literal::Quote(b'\'')))
                {
                    offset += 2;
                } else {
                    if dollar_expansions && matches!(literal, Literal::Quote(b'"')) {
                        visit(offset, true);
                    }
                    offset += 1;
                }
                continue;
            }
            match language {
                Language::Rust if rest.starts_with(b"//") => break,
                Language::Sql if rest.starts_with(b"--") => break,
                Language::Rust | Language::Sql if rest.starts_with(b"/*") => {
                    self.block_comments = 1;
                    offset += 2;
                    continue;
                }
                Language::Rust if rest[0] == b'r' && raw_prefix_boundary(bytes, offset) => {
                    let hashes = rest[1..].iter().take_while(|&&byte| byte == b'#').count();
                    if rest.get(hashes + 1) == Some(&b'"') {
                        self.literal = Some(Literal::RustRaw(hashes));
                        offset += hashes + 2;
                        continue;
                    }
                }
                Language::Rust if rest[0] == b'\'' => {
                    if let Some(end) = rust_character_length(rest) {
                        offset += end;
                        continue;
                    }
                }
                Language::Rust if rest[0] == b'"' => self.literal = Some(Literal::Quote(b'"')),
                Language::Python if rest[0] == b'#' => break,
                Language::Shell if rest[0] == b'#' && shell_comment_start(bytes, offset) => break,
                Language::Dockerfile
                    if rest[0] == b'#'
                        && (offset == 0
                            || bytes[offset - 1].is_ascii_whitespace()
                            || b";|&()".contains(&bytes[offset - 1])) =>
                {
                    break;
                }
                Language::Python if matches!(rest[0], b'\'' | b'"') => {
                    if rest.starts_with(&[rest[0]; 3]) {
                        self.literal = Some(Literal::TripleQuote(rest[0]));
                        offset += 3;
                        continue;
                    }
                    self.literal = Some(Literal::Quote(rest[0]));
                }
                Language::Python if matches!(rest[0], b'(' | b'[' | b'{') => {
                    self.python_group_depth += 1;
                }
                Language::Python if matches!(rest[0], b')' | b']' | b'}') => {
                    self.python_group_depth = self.python_group_depth.saturating_sub(1);
                }
                Language::Python if rest == b"\\" => self.python_continuation = true,
                Language::Shell if matches!(rest[0], b'\'' | b'"' | b'`') => {
                    self.literal = Some(Literal::Quote(rest[0]));
                }
                Language::Dockerfile if matches!(rest[0], b'\'' | b'"') => {
                    self.literal = Some(Literal::Quote(rest[0]));
                }
                Language::Sql if matches!(rest[0], b'\'' | b'"') => {
                    visit(offset, false);
                    self.literal = Some(Literal::Quote(rest[0]));
                }
                Language::Shell | Language::Dockerfile if rest[0] == b'\\' => {
                    offset += 2;
                    continue;
                }
                Language::Shell if self.shell_arithmetic_depth == 0 && rest.starts_with(b"((") => {
                    self.shell_arithmetic_depth = 2;
                    visit(offset, false);
                    visit(offset + 1, false);
                    offset += 2;
                    continue;
                }
                Language::Shell if self.shell_arithmetic_depth > 0 => match rest[0] {
                    b'(' => self.shell_arithmetic_depth += 1,
                    b')' => self.shell_arithmetic_depth -= 1,
                    b'<' if rest.starts_with(b"<<") => {
                        // A shift cannot introduce a here-document.
                        offset += 2;
                        continue;
                    }
                    _ => {}
                },
                Language::Shell if rest.starts_with(b"<<<") => {
                    offset += 3;
                    continue;
                }
                _ => {}
            }
            if self.literal.is_none() {
                visit(offset, false);
            }
            offset += 1;
        }
        if matches!(self.literal, Some(Literal::Quote(_)))
            && (language == Language::Dockerfile
                || language == Language::Python && !line.ends_with('\\'))
        {
            self.literal = None;
        }
        code
    }
}

fn raw_prefix_boundary(text: &[u8], offset: usize) -> bool {
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    if offset == 0 || !is_word(text[offset - 1]) {
        return true;
    }
    matches!(text[offset - 1], b'b' | b'c') && (offset == 1 || !is_word(text[offset - 2]))
}

fn rust_character_length(text: &[u8]) -> Option<usize> {
    if text.get(1) == Some(&b'\\') {
        return text
            .get(3..text.len().min(13))?
            .iter()
            .position(|&byte| byte == b'\'')
            .map(|index| index + 4);
    }
    let first = *text.get(1)?;
    let width = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return None,
    };
    (text.get(1 + width) == Some(&b'\'')).then_some(width + 2)
}

fn closing_literal(literal: Literal, rest: &[u8]) -> usize {
    match literal {
        Literal::Quote(quote) if rest[0] == quote => 1,
        Literal::TripleQuote(quote) if rest.starts_with(&[quote; 3]) => 3,
        Literal::RustRaw(hashes)
            if rest[0] == b'"'
                && rest
                    .get(1..=hashes)
                    .is_some_and(|tail| tail.iter().all(|&byte| byte == b'#')) =>
        {
            hashes + 1
        }
        _ => 0,
    }
}

fn shell_comment_start(bytes: &[u8], offset: usize) -> bool {
    let Some(previous) = offset.checked_sub(1) else {
        return true;
    };
    let byte = bytes[previous];
    (byte.is_ascii_whitespace() || b";|&()<>".contains(&byte))
        && bytes[..previous]
            .iter()
            .rev()
            .take_while(|&&byte| byte == b'\\')
            .count()
            .is_multiple_of(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_zoom_preserves_dirty_text_selection_and_undo() {
        let mut editor = EditorWindow::new();
        let inherited = 21;
        assert_eq!(editor.font_size(inherited), inherited);
        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Paste(Arc::new(String::from("edited text"))),
        )));
        let cursor = text_editor::Cursor {
            position: text_editor::Position { line: 0, column: 6 },
            selection: Some(text_editor::Position { line: 0, column: 0 }),
        };
        editor.content.move_to(cursor);
        let text = editor.content.text();
        let revision = editor.revision;
        assert!(editor.dirty);

        let _ = editor.update(Message::ZoomIn(inherited));
        assert_eq!(editor.font_size(26), 23);
        let _ = editor.update(Message::ZoomOut(inherited));
        assert_eq!(editor.font_size(26), 26);
        for _ in 0..24 {
            let _ = editor.update(Message::ZoomOut(inherited));
        }
        assert_eq!(editor.font_size(inherited), MIN_FONT_SIZE);
        for _ in 0..MAX_FONT_SIZE {
            let _ = editor.update(Message::ZoomIn(inherited));
        }
        assert_eq!(editor.font_size(inherited), MAX_FONT_SIZE);
        let _ = editor.update(Message::ResetZoom);
        assert_eq!(editor.font_size(26), 26);

        assert_eq!(editor.content.text(), text);
        assert_eq!(editor.content.cursor(), cursor);
        assert_eq!(editor.revision, revision);
        assert!(editor.dirty);
        let _ = editor.update(Message::Undo);
        assert!(editor.content.text().trim().is_empty());
        assert_eq!(editor.font_size(26), 26);
    }

    #[test]
    fn editor_zoom_shortcuts_accept_primary_and_keypad_keys() {
        use iced::keyboard::{
            Key, Modifiers,
            key::{Code, Physical},
        };

        let event = |key: &str, code, modifiers| text_editor::KeyPress {
            key: Key::Character(key.into()),
            modified_key: Key::Character(key.into()),
            physical_key: Physical::Code(code),
            modifiers,
            text: None,
            status: text_editor::Status::Focused { is_hovered: false },
        };
        let command = Modifiers::COMMAND;
        for (key, code, modifiers) in [
            ("=", Code::Equal, command),
            ("=", Code::Equal, command | Modifiers::SHIFT),
            ("+", Code::NumpadAdd, command),
        ] {
            assert!(matches!(
                editor_key_binding(event(key, code, modifiers), 22, true),
                Some(text_editor::Binding::Custom(Message::ZoomIn(22)))
            ));
        }
        for code in [Code::Minus, Code::NumpadSubtract] {
            assert!(matches!(
                editor_key_binding(event("-", code, command), 22, true),
                Some(text_editor::Binding::Custom(Message::ZoomOut(22)))
            ));
        }
        for code in [Code::Digit0, Code::Numpad0] {
            assert!(matches!(
                editor_key_binding(event("0", code, command), 22, true),
                Some(text_editor::Binding::Custom(Message::ResetZoom))
            ));
        }
        assert!(!matches!(
            editor_key_binding(event("=", Code::Equal, command | Modifiers::ALT), 22, true),
            Some(text_editor::Binding::Custom(Message::ZoomIn(22)))
        ));
    }

    #[test]
    fn editor_cut_key_requires_focus_and_a_writable_document() {
        use iced::keyboard::{
            Key, Modifiers,
            key::{Code, Physical},
        };

        let mut event = text_editor::KeyPress {
            key: Key::Character("x".into()),
            modified_key: Key::Character("x".into()),
            physical_key: Physical::Code(Code::KeyX),
            modifiers: Modifiers::COMMAND,
            text: None,
            status: text_editor::Status::Focused { is_hovered: false },
        };
        assert!(matches!(
            editor_key_binding(event.clone(), 16, true),
            Some(text_editor::Binding::Custom(Message::Cut))
        ));
        assert!(editor_key_binding(event.clone(), 16, false).is_none());
        event.status = text_editor::Status::Hovered;
        assert!(editor_key_binding(event, 16, true).is_none());
    }

    #[test]
    fn editor_zoom_controls_fit_the_minimum_window() {
        use iced::advanced::{Layout, layout, widget::Tree};
        use iced::{Renderer, Size, Theme};

        let editor = EditorWindow::new();
        let mut view = editor.view(
            Theme::Dark,
            Appearance {
                font: Font::MONOSPACE,
                size: MAX_FONT_SIZE,
                foreground: Rgb::from_hex(0xffffff),
                background: Rgb::from_hex(0x000000),
            },
            caret::Preferences::default(),
        );
        let mut tree = Tree::new(view.as_widget());
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let limits = layout::Limits::new(Size::ZERO, Size::new(600.0, 320.0));
        let node = view.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let content = Layout::new(&node).children().next().unwrap();
        let toolbar = content.children().nth(1).unwrap();
        let controls: Vec<_> = toolbar.children().collect();
        assert_eq!(controls.len(), 7);
        assert_eq!(controls[5].children().count(), 4);
        for control in controls {
            let bounds = control.bounds();
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
            assert!(bounds.x >= content.bounds().x);
            assert!(bounds.x + bounds.width <= content.bounds().x + content.bounds().width);
            assert!(bounds.y + bounds.height <= toolbar.bounds().y + toolbar.bounds().height);
        }
        assert!(content.children().nth(2).unwrap().bounds().height >= 100.0);
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
            let number = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kortty-editor-test-{}-{number}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn find_wraps_and_selects_utf8_matches_with_byte_columns() {
        let mut editor = EditorWindow::new();
        editor.content = text_editor::Content::with_text("ä漢 needle\r\n🙂 needle");
        let _ = editor.update(Message::SetFind("needle".into()));
        let _ = editor.update(Message::Find(SearchDirection::Next));
        assert_eq!(editor.content.selection().as_deref(), Some("needle"));
        assert_eq!(
            editor.content.cursor().selection.unwrap(),
            text_editor::Position { line: 0, column: 6 }
        );
        let _ = editor.update(Message::Find(SearchDirection::Next));
        assert_eq!(
            editor.content.cursor().selection.unwrap(),
            text_editor::Position { line: 1, column: 5 }
        );
        let _ = editor.update(Message::Find(SearchDirection::Next));
        assert_eq!(editor.content.cursor().position.line, 0);
        let _ = editor.update(Message::Find(SearchDirection::Previous));
        assert_eq!(editor.content.cursor().position.line, 1);
        assert!(!editor.dirty);
    }

    #[test]
    fn replacements_preserve_line_endings_and_undo_to_the_saved_state() {
        let mut editor = EditorWindow::new();
        let original = "a漢 ä\r\n漢 Ä\n";
        editor.content = text_editor::Content::with_text(original);
        editor.document = Some(Document::local(
            PathBuf::from("test.txt"),
            Arc::from(original),
        ));
        let _ = editor.update(Message::SetFind("漢".into()));
        let _ = editor.update(Message::Find(SearchDirection::Next));
        let _ = editor.update(Message::SetReplacement("é".into()));
        let _ = editor.update(Message::ReplaceOne);
        assert_eq!(editor.content.text(), "aé ä\r\n漢 Ä\n");
        let _ = editor.update(Message::SetFind("ä".into()));
        let _ = editor.update(Message::FindCaseSensitive(true));
        let _ = editor.update(Message::SetReplacement(String::new()));
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), "aé \r\n漢 Ä\n");
        assert!(editor.dirty);
        let _ = editor.update(Message::Undo);
        assert_eq!(editor.content.text(), "aé ä\r\n漢 Ä\n");
        let _ = editor.update(Message::Undo);
        assert_eq!(editor.content.text(), original);
        assert!(!editor.dirty);
        let _ = editor.update(Message::Redo);
        assert_eq!(editor.content.text(), "aé ä\r\n漢 Ä\n");
        assert!(editor.dirty);
    }

    #[test]
    fn editor_regex_replaces_only_the_original_selection_across_mixed_line_endings() {
        let original = "outer Ä99\r\nx ä12\r\nNEXT Ä3\nouter Ä99\n";
        let mut editor = EditorWindow::new();
        editor.content = text_editor::Content::with_text(original);
        editor.content.move_to(text_editor::Cursor {
            position: text_editor::Position {
                line: 2,
                column: "NEXT Ä3".len(),
            },
            selection: Some(text_editor::Position { line: 1, column: 2 }),
        });
        let _ = editor.update(Message::FindSelectionOnly(true));
        let _ = editor.update(Message::FindRegex(true));
        let _ = editor.update(Message::SetFind(r"ä\d+\nnext".into()));
        let _ = editor.update(Message::Find(SearchDirection::Next));
        let hit = editor.content.cursor();
        assert_eq!(
            hit.selection,
            Some(text_editor::Position { line: 1, column: 2 })
        );
        assert_eq!(hit.position, text_editor::Position { line: 2, column: 4 });

        let _ = editor.update(Message::SetReplacement("done".into()));
        let _ = editor.update(Message::ReplaceOne);
        assert_eq!(editor.content.text(), "outer Ä99\r\nx done Ä3\nouter Ä99\n");
        let _ = editor.update(Message::SetFind(r"ä\d+".into()));
        let _ = editor.update(Message::SetReplacement("$1".into()));
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), "outer Ä99\r\nx done $1\nouter Ä99\n");

        let _ = editor.update(Message::Undo);
        let previous = editor.content.text();
        assert_eq!(previous, "outer Ä99\r\nx done Ä3\nouter Ä99\n");
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), previous);
        assert_eq!(
            editor.status.as_deref(),
            Some("Select text, then enable Selection only again.")
        );
    }

    #[test]
    fn editor_regex_navigates_empty_matches_and_rejects_invalid_patterns_without_editing() {
        let mut editor = EditorWindow::new();
        let original = "ä\r\n漢\n🙂";
        editor.content = text_editor::Content::with_text(original);
        let _ = editor.update(Message::FindRegex(true));
        let _ = editor.update(Message::SetFind("(?m)^".into()));
        for line in [0, 1, 2, 0] {
            let _ = editor.update(Message::Find(SearchDirection::Next));
            assert_eq!(
                editor.content.cursor().position,
                text_editor::Position { line, column: 0 }
            );
        }
        let _ = editor.update(Message::SetFind("(".into()));
        let _ = editor.update(Message::SetReplacement("changed".into()));
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), original);
        assert_eq!(
            editor.status.as_deref(),
            Some("Invalid or unsupported search expression.")
        );
        assert!(!editor.dirty);
        assert!(editor.undo.items.is_empty());
    }

    #[test]
    fn deleting_one_match_and_editing_after_undo_clears_redo() {
        let mut editor = EditorWindow::new();
        editor.content = text_editor::Content::with_text("x🙂y");
        let _ = editor.update(Message::SetFind("🙂".into()));
        let _ = editor.update(Message::ReplaceOne);
        assert_eq!(editor.content.text(), "xy");
        let _ = editor.update(Message::Undo);
        assert_eq!(editor.content.text(), "x🙂y");
        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('!'),
        )));
        assert!(editor.redo.items.is_empty());
        let _ = editor.update(Message::Redo);
        assert_eq!(editor.content.text(), "!x🙂y");
    }

    #[test]
    fn replace_all_checks_the_output_budget_before_editing() {
        let mut editor = EditorWindow::new();
        editor.content = text_editor::Content::with_text("aa");
        let _ = editor.update(Message::SetFind("a".into()));
        let _ = editor.update(Message::SetReplacement("x".repeat(MAX_FILE_BYTES / 2 + 1)));
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), "aa");
        assert!(!editor.dirty);
        assert!(editor.undo.items.is_empty());
    }

    #[test]
    fn editor_search_does_not_apply_a_previous_value_after_rejecting_an_oversized_paste() {
        let mut editor = EditorWindow::new();
        editor.content = text_editor::Content::with_text("one");
        let _ = editor.update(Message::SetFind("one".into()));
        let _ = editor.update(Message::SetReplacement("two".into()));
        let _ = editor.update(Message::SetReplacement("x".repeat(MAX_FILE_BYTES + 1)));
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), "one");
        assert!(!editor.dirty);

        let _ = editor.update(Message::SetReplacement("two".into()));
        let _ = editor.update(Message::SetFind("x".repeat(4097)));
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), "one");
        assert!(!editor.dirty);

        let _ = editor.update(Message::SetFind("one".into()));
        let _ = editor.update(Message::ReplaceAll);
        assert_eq!(editor.content.text(), "two");
    }

    #[test]
    fn a_dirty_close_requires_explicit_discard() {
        let mut window = EditorWindow::new();
        let _ = window.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('a'),
        )));
        assert!(!window.request_close());
        let _ = window.update(Message::DecideDiscard(false));
        assert!(!window.may_close());
        assert_eq!(window.content.text(), "a");
        assert!(!window.request_close());
        let _ = window.update(Message::DecideDiscard(true));
        assert!(window.may_close());
    }

    #[test]
    fn terminal_file_editor_preserves_the_unicode_position_and_read_only_policy() {
        let directory = TestDirectory::new();
        let path = directory.0.join("position.txt");
        let source = "first\nαβ third\nlast\n";
        fs::write(&path, source).unwrap();
        let document = open_terminal_document(&path, false).unwrap();
        assert!(save_document(&path, Arc::from("replacement"), Some(&document)).is_err());
        let (mut editor, _) = EditorWindow::with_document(document, Some(2), Some(3));
        assert_eq!(
            editor.content.cursor().position,
            text_editor::Position { line: 1, column: 4 }
        );
        assert!(!editor.writable());
        let _ = editor.update(Message::SetPath("other.txt".into()));
        let _ = editor.update(Message::Open);
        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('x'),
        )));
        let _ = editor.update(Message::ConvertLineEndings(Conversion::CrLf));
        let _ = editor.update(Message::Edit(text_editor::Action::SelectAll));
        assert!(editor.cut_selection().is_none());
        assert_eq!(editor.content.text(), source);
        #[cfg(unix)]
        assert_eq!(Path::new(&editor.path), canonical_target(&path).unwrap());
        #[cfg(windows)]
        assert_eq!(
            crate::private_files::FileStamp::read(
                &open_regular_editor_file(Path::new(&editor.path)).unwrap()
            )
            .unwrap(),
            crate::private_files::FileStamp::read(&File::open(&path).unwrap()).unwrap()
        );
        assert!(!editor.dirty);
    }

    #[test]
    fn editor_save_and_close_waits_for_success_and_keeps_the_draft_after_conflict() {
        let directory = TestDirectory::new();
        let file = directory.0.join("edit.txt");
        fs::write(&file, "saved").unwrap();
        let (mut window, _) = EditorWindow::with_file(file.clone());
        let _ = window.update(Message::Opened(
            open_document(&file).map_err(|error| error.to_string()),
        ));
        let _ = window.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('!'),
        )));
        let draft = Arc::<str>::from(window.content.text());
        assert!(!window.request_close());
        let _ = window.update(Message::SaveAndClose);
        assert!(window.busy);
        assert!(!window.close_decision_available());
        let _ = window.update(Message::DecideDiscard(true));
        assert!(!window.may_close());

        fs::write(&file, "another writer").unwrap();
        let failed =
            save_document(&file, Arc::clone(&draft), window.document.as_ref()).unwrap_err();
        let _ = window.update(Message::Saved(Err(failed.to_string())));
        assert!(window.close_decision_available());
        assert!(!window.may_close());
        assert!(window.dirty);
        assert_eq!(window.content.text(), draft.as_ref());
        assert_eq!(fs::read_to_string(&file).unwrap(), "another writer");

        fs::write(&file, "saved").unwrap();
        let _ = window.update(Message::SaveAndClose);
        let saved = save_document(&file, Arc::clone(&draft), window.document.as_ref()).unwrap();
        let _ = window.update(Message::Saved(Ok(saved)));
        assert!(window.may_close());
        assert!(!window.dirty);
        assert_eq!(fs::read_to_string(&file).unwrap(), draft.as_ref());
    }

    #[test]
    fn editor_save_and_close_accepts_a_new_destination_for_an_untitled_draft() {
        let directory = TestDirectory::new();
        let file = directory.0.join("untitled.txt");
        let mut window = EditorWindow::new();
        let _ = window.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('x'),
        )));
        assert!(!window.request_close());
        let _ = window.update(Message::SaveAndClose);
        assert!(window.close_decision_available());
        let _ = window.update(Message::SetPath(file.to_string_lossy().into_owned()));
        assert!(window.can_set_path());
        let _ = window.update(Message::SaveAndClose);
        assert!(window.busy);
        let saved = save_document(&file, Arc::from(window.content.text()), None).unwrap();
        let _ = window.update(Message::Saved(Ok(saved)));
        assert!(window.may_close());
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
    }

    #[cfg(unix)]
    #[test]
    fn editor_reader_does_not_follow_a_leaf_symlink() {
        let directory = TestDirectory::new();
        let real = directory.0.join("real.txt");
        let alias = directory.0.join("alias.txt");
        fs::write(&real, "other file").unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert!(open_document(&alias).is_err());
        assert!(open_regular_editor_file(&alias).is_err());
    }

    #[test]
    fn editor_retains_file_line_endings_while_editing() {
        for ending in ["\r\n", "\n"] {
            let original = format!("alpha{ending}beta{ending}");
            let mut content = text_editor::Content::<iced::Renderer>::with_text(&original);
            assert_eq!(content.text(), original);
            content.perform(text_editor::Action::Edit(text_editor::Edit::Insert('x')));
            assert_eq!(content.text(), format!("x{original}"));
        }
    }

    #[test]
    fn editor_converts_mixed_endings_as_one_undoable_local_file_edit() {
        let directory = TestDirectory::new();
        let file = directory.0.join("mixed.txt");
        let original = "ä\r\nβ\rc\nlast";
        fs::write(&file, original).unwrap();
        let (mut editor, _) =
            EditorWindow::with_document(open_document(&file).unwrap(), None, None);
        assert_eq!(editor.endings.label(editor.content.line_count()), "Mixed");
        let selection = text_editor::Cursor {
            position: text_editor::Position { line: 2, column: 1 },
            selection: Some(text_editor::Position {
                line: 0,
                column: "ä".len(),
            }),
        };
        editor.content.move_to(selection);
        let _ = editor.update(Message::ConvertLineEndings(Conversion::CrLf));
        assert_eq!(editor.content.text(), "ä\r\nβ\r\nc\r\nlast");
        assert_eq!(editor.content.cursor(), selection);
        assert_eq!(
            editor.endings.label(editor.content.line_count()),
            "CRLF (Windows)"
        );
        assert!(editor.dirty);
        assert_eq!(fs::read_to_string(&file).unwrap(), original);
        let _ = editor.update(Message::Undo);
        assert_eq!(editor.content.text(), original);
        assert_eq!(editor.content.cursor(), selection);
        assert_eq!(editor.endings.label(editor.content.line_count()), "Mixed");
        assert!(!editor.dirty);
        let _ = editor.update(Message::Redo);
        let _ = editor.update(Message::Edit(text_editor::Action::Move(
            text_editor::Motion::DocumentEnd,
        )));
        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Enter,
        )));
        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Paste(Arc::new("new\nlast\r".into())),
        )));
        let contents = editor.content.text();
        assert_eq!(contents, "ä\r\nβ\r\nc\r\nlast\r\nnew\r\nlast\r\n");
        save_document(
            &file,
            Arc::from(contents.as_str()),
            editor.document.as_ref(),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(file).unwrap(), contents);
    }

    #[test]
    fn editor_selects_lf_or_crlf_for_new_lines_without_changing_empty_file_bytes() {
        let mut editor = EditorWindow::new();
        let _ = editor.update(Message::ConvertLineEndings(Conversion::CrLf));
        assert_eq!(
            editor.endings.label(editor.content.line_count()),
            "CRLF (Windows)"
        );
        assert!(!editor.dirty);
        assert!(editor.undo.items.is_empty());
        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Enter,
        )));
        assert_eq!(editor.content.text(), "\r\n");
        let _ = editor.update(Message::ConvertLineEndings(Conversion::Lf));
        assert_eq!(editor.content.text(), "\n");
    }

    #[test]
    fn file_save_refuses_unknown_or_changed_destinations() {
        let directory = TestDirectory::new();
        let file = directory.0.join("source.rs");
        fs::write(&file, "fn before() {}\r\n").unwrap();
        let document = open_document(&file).unwrap();
        assert_eq!(&*document.original, "fn before() {}\r\n");
        assert_eq!(
            save_document(&file, Arc::from("new"), None)
                .unwrap_err()
                .source
                .kind(),
            io::ErrorKind::AlreadyExists,
        );
        fs::write(&file, "external changes").unwrap();
        let error = save_document(&file, Arc::from("new"), Some(&document)).unwrap_err();
        assert_eq!(error.stage, "checking the original file");
        assert_eq!(fs::read_to_string(&file).unwrap(), "external changes");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn save_creates_and_replaces_regular_files() {
        let directory = TestDirectory::new();
        let file = directory.0.join("source.py");
        let document = save_document(&file, Arc::from("def first():\n    pass\n"), None).unwrap();
        assert_eq!(&*document.original, fs::read_to_string(&file).unwrap());
        let modified = "def second():\n    pass\n";
        save_document(&file, Arc::from(modified), Some(&document)).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), modified);
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn simultaneous_saves_from_the_same_snapshot_do_not_overwrite_each_other() {
        let directory = TestDirectory::new();
        let file = directory.0.join("source.rs");
        let previous = save_document(&file, Arc::from("original"), None).unwrap();
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| save_document(&file, Arc::from("first"), Some(&previous)));
            let second = scope.spawn(|| save_document(&file, Arc::from("second"), Some(&previous)));
            (first.join().unwrap(), second.join().unwrap())
        });
        let winner = match (first, second) {
            (Ok(document), Err(_)) | (Err(_), Ok(document)) => document,
            other => panic!("expected one saved document, got {other:?}"),
        };
        assert_eq!(fs::read_to_string(&file).unwrap(), &*winner.original);
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[cfg(windows)]
    const SHARE_READ_WRITE: u32 = 0x1 | 0x2;

    #[cfg(windows)]
    #[test]
    fn windows_staging_reserves_delete_access_and_keeps_new_publication_no_clobber() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let directory = TestDirectory::new();
        let mut staged = TemporarySave::new(&directory.0).unwrap();
        staged.file_mut().write_all(b"staged").unwrap();
        staged.file_mut().sync_all().unwrap();
        let staging_path = fs::read_dir(&directory.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let blocked = fs::OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ_WRITE)
            .open(&staging_path)
            .unwrap_err();
        assert_eq!(blocked.raw_os_error(), Some(32));

        let path = directory.0.join("source.rs");
        fs::write(&path, "other writer").unwrap();
        assert_eq!(
            staged.publish_new(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(staged);
        assert_eq!(fs::read_to_string(&path).unwrap(), "other writer");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_replacement_preserves_delete_shared_readers_of_the_original() {
        let directory = TestDirectory::new();
        let path = directory.0.join("source.rs");
        let previous = save_document(&path, Arc::from("original"), None).unwrap();
        // File::open shares read, write and delete on Windows.
        let mut reader = File::open(&path).unwrap();
        save_document(&path, Arc::from("updated"), Some(&previous)).unwrap();
        let mut opened = String::new();
        reader.read_to_string(&mut opened).unwrap();
        assert_eq!(opened, "original");
        assert_eq!(fs::read_to_string(&path).unwrap(), "updated");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_replacement_rechecks_the_snapshot_after_destination_sharing_denial() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let directory = TestDirectory::new();
        let path = directory.0.join("source.rs");
        let previous = save_document(&path, Arc::from("original"), None).unwrap();
        let mut blocker = Some(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(SHARE_READ_WRITE)
                .open(&path)
                .unwrap(),
        );
        let mut staged = TemporarySave::new(&directory.0).unwrap();
        staged.file_mut().write_all(b"staged").unwrap();
        staged.file_mut().sync_all().unwrap();
        let mut attempts = 0;
        let error = retry_windows_replacement(|| {
            attempts += 1;
            let result = staged.replace_once(&path, &previous);
            if let Some(blocker) = blocker.take() {
                let denied = result.as_ref().unwrap_err();
                assert_eq!(denied.stage, "replacing the destination file");
                assert!(matches!(denied.source.raw_os_error(), Some(5 | 32)));
                drop(blocker);
                fs::write(&path, "external change").unwrap();
            }
            result
        })
        .unwrap_err();
        assert_eq!(attempts, 2);
        assert_eq!(error.stage, "rechecking the original file");
        drop(staged);
        assert_eq!(fs::read_to_string(&path).unwrap(), "external change");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn open_rejects_non_utf8_or_oversized_input() {
        let directory = TestDirectory::new();
        let file = directory.0.join("source");
        fs::write(&file, [0xff]).unwrap();
        assert!(open_document(&file).is_err());
        File::create(&file)
            .unwrap()
            .set_len(MAX_FILE_BYTES as u64 + 1)
            .unwrap();
        assert!(open_document(&file).is_err());
    }

    #[test]
    fn outline_skips_declarations_inside_comments_and_literals() {
        let rust = r##"/* comment
fn fake() {}
*/
const EXAMPLE: &str = r#"
fn also_fake() {}
"#;
pub(crate) async fn real_name() { let _ = '"'; }
struct Struct;
let placeholder = "${workspace}";"##;
        let outline = scan_outline(rust, Language::Rust);
        let names: Vec<_> = outline
            .items
            .iter()
            .map(|item| (item.line + 1, item.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                (4, "EXAMPLE"),
                (7, "real_name"),
                (8, "Struct"),
                (9, "placeholder"),
                (9, "${workspace}")
            ]
        );

        let python =
            "'''\ndef fake():\n'''\nclass Real:\n    async def method(self):\n        pass";
        let outline = scan_outline(python, Language::Python);
        let names: Vec<_> = outline
            .items
            .iter()
            .map(|item| (item.line + 1, item.name.as_str()))
            .collect();
        assert_eq!(names, [(4, "Real"), (5, "method")]);

        let shell = "cat <<'EOF'\nfake() {\nEOF\ncat <<<HERE\nreal() {\n  echo ${HOME}\n}";
        let outline = scan_outline(shell, Language::Shell);
        let names: Vec<_> = outline
            .items
            .iter()
            .map(|item| (item.line + 1, item.name.as_str()))
            .collect();
        assert_eq!(names, [(5, "real"), (6, "${HOME}")]);
    }

    #[test]
    fn dockerfile_outline_refresh_and_jump_work_in_the_native_editor() {
        use iced::futures::{StreamExt, executor::block_on};

        let source = "ARG BASE=alpine\nFROM ${BASE} AS build\nENV APP_HOME=/app\nFROM alpine\n";
        assert!(matches!(
            Language::detect(Path::new("Dockerfile"), source),
            Language::Dockerfile
        ));
        let mut editor = EditorWindow::new();
        editor.path = "build.Dockerfile".into();
        editor.content = text_editor::Content::with_text(source);
        let mut actions =
            iced_runtime::task::into_stream(editor.update(Message::RefreshOutline)).unwrap();
        let Some(iced_runtime::Action::Output(result)) = block_on(actions.next()) else {
            panic!("native Editor did not refresh its Dockerfile outline");
        };
        let _ = editor.update(result);
        assert_eq!(
            editor
                .outline
                .items
                .iter()
                .map(|item| (item.kind, item.name.as_str(), item.line))
                .collect::<Vec<_>>(),
            [
                ("parameter", "BASE", 0),
                ("stage", "build", 1),
                ("placeholder", "${BASE}", 1),
                ("exported", "APP_HOME", 2),
                ("stage", "alpine", 3)
            ]
        );
        let _ = editor.update(Message::Jump {
            revision: editor.revision,
            item: 1,
        });
        assert_eq!(editor.content.selection().as_deref(), Some("build"));
    }

    #[test]
    fn sql_outline_refresh_and_jump_work_in_the_native_editor() {
        use iced::futures::{StreamExt, executor::block_on};

        let source = "-- Heading\nCREATE TABLE users (id INT);\nDECLARE @limit INT = 5;\n";
        let mut editor = EditorWindow::new();
        editor.path = "setup.SQL".into();
        editor.content = text_editor::Content::with_text(source);
        let mut actions =
            iced_runtime::task::into_stream(editor.update(Message::RefreshOutline)).unwrap();
        let Some(iced_runtime::Action::Output(result)) = block_on(actions.next()) else {
            panic!("native Editor did not refresh its SQL outline");
        };
        let _ = editor.update(result);
        assert_eq!(
            editor
                .outline
                .items
                .iter()
                .map(|item| (item.kind, item.name.as_str(), item.line))
                .collect::<Vec<_>>(),
            [("sql table", "users", 1), ("variable", "@limit", 2)]
        );
        let _ = editor.update(Message::Jump {
            revision: editor.revision,
            item: 0,
        });
        assert_eq!(editor.content.selection().as_deref(), Some("users"));
    }

    #[test]
    fn outline_lists_variable_bindings_and_selects_their_source_name() {
        let variables = |source, language| {
            scan_outline(source, language)
                .items
                .into_iter()
                .filter(|item| item.kind == "variable")
                .map(|item| (item.line, item.name))
                .collect::<Vec<_>>()
        };
        let rust = "pub const LIMIT: usize = 3;\nstatic mut ACTIVE: bool = false;\nconst fn bound() {}\n    let mut count = LIMIT;\n    let unused: usize;\nlet _ = unused;\nlet Some(value) = maybe else { return };";
        assert_eq!(
            variables(rust, Language::Rust),
            [(0, "LIMIT"), (1, "ACTIVE"), (3, "count"), (4, "unused")]
                .map(|(line, name)| (line, name.to_owned()))
        );
        let python = "title: str = 'β'\nvalue == 4\nself.field = 'attribute'\nrequest(\n    timeout=5,\n)\nthreshold = 7\nresult = (\n    threshold == 1\n)\nannotation: int";
        assert_eq!(
            variables(python, Language::Python),
            [
                (0, "title"),
                (6, "threshold"),
                (7, "result"),
                (10, "annotation")
            ]
            .map(|(line, name)| (line, name.to_owned()))
        );
        let shell = "MODE=fast\nrun() {\n    local first=1\n    export PATH\n    echo ignored=1\n    DATA+=more\n    cat <<'EOF'\n    HIDDEN=yes\nEOF\n}";
        assert_eq!(
            variables(shell, Language::Shell),
            [(0, "MODE"), (2, "first"), (5, "DATA")].map(|(line, name)| (line, name.to_owned()))
        );
        assert!(
            scan_outline(shell, Language::Shell)
                .items
                .iter()
                .any(|item| {
                    (item.line, item.kind, item.name.as_str()) == (3, "exported", "PATH")
                })
        );

        let mut editor = EditorWindow::new();
        editor.content = text_editor::Content::with_text(python);
        let outline = scan_outline(python, Language::Python);
        let item = outline
            .items
            .iter()
            .position(|item| item.name == "threshold")
            .unwrap();
        let _ = editor.update(Message::OutlineReady(editor.revision, outline));
        let _ = editor.update(Message::Jump {
            revision: editor.revision,
            item,
        });
        assert_eq!(editor.content.selection().as_deref(), Some("threshold"));
        assert_eq!(
            editor.content.cursor().position,
            text_editor::Position { line: 6, column: 9 }
        );
    }

    #[test]
    fn python_file_outline_selects_a_parameter_on_a_continuation_line() {
        use iced::futures::{StreamExt, executor::block_on};

        let source = "def render(\r\n    prefix: str,\r\n    größe: int = 1,\r\n):\r\n    return prefix * größe\r\n";
        let mut editor = EditorWindow::new();
        editor.path = "layout.py".into();
        editor.content = text_editor::Content::with_text(source);
        let mut actions =
            iced_runtime::task::into_stream(editor.update(Message::RefreshOutline)).unwrap();
        let Some(iced_runtime::Action::Output(result)) = block_on(actions.next()) else {
            panic!("native Editor did not refresh its Python outline");
        };
        let _ = editor.update(result);
        let item = editor
            .outline
            .items
            .iter()
            .position(|item| item.kind == "parameter" && item.name == "größe" && item.line == 2)
            .unwrap();
        let _ = editor.update(Message::Jump {
            revision: editor.revision,
            item,
        });
        assert_eq!(editor.content.selection().as_deref(), Some("größe"));
    }

    #[test]
    fn markup_outline_selects_exact_unicode_names_and_rejects_old_clicks() {
        use iced::futures::{StreamExt, executor::block_on};

        fn refresh(editor: &mut EditorWindow) {
            let mut actions =
                iced_runtime::task::into_stream(editor.update(Message::RefreshOutline)).unwrap();
            let Some(iced_runtime::Action::Output(result)) = block_on(actions.next()) else {
                panic!("native Editor did not refresh its markup outline");
            };
            let _ = editor.update(result);
        }

        let mut editor = EditorWindow::new();
        editor.path = "manual.MD".into();
        editor.content = text_editor::Content::with_text(concat!(
            "# Overview\r\n",
            "~~~md\r\n",
            "## Hidden\r\n",
            "~~~\r\n",
            "## Größe 🐚 ###\r\n",
            "ä ${δ} ${δ}\r\n",
        ));
        refresh(&mut editor);
        assert_eq!(
            editor
                .outline
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Overview", "Größe 🐚", "${δ}", "${δ}"],
        );
        let heading = Message::Jump {
            revision: editor.revision,
            item: 1,
        };
        let _ = editor.update(heading.clone());
        assert_eq!(editor.content.selection().as_deref(), Some("Größe 🐚"));
        let _ = editor.update(Message::Jump {
            revision: editor.revision,
            item: 3,
        });
        assert_eq!(editor.content.selection().as_deref(), Some("${δ}"));
        assert_eq!(
            editor.content.cursor().selection,
            Some(text_editor::Position {
                line: 5,
                column: "ä ${δ} ".len(),
            })
        );

        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('x'),
        )));
        refresh(&mut editor);
        let after_edit = editor.content.cursor();
        assert_ne!(editor.revision, 0);
        assert!(iced_runtime::task::into_stream(editor.update(heading)).is_none());
        assert_eq!(editor.content.cursor(), after_edit);
        let _ = editor.update(Message::Jump {
            revision: editor.revision,
            item: 1,
        });
        assert_eq!(editor.content.selection().as_deref(), Some("Größe 🐚"));

        let adoc = "= Erste\n----\n== Hidden\n----\n== Größe\n";
        let outline = scan_outline(adoc, Language::detect(Path::new("guide.adoc"), adoc));
        assert_eq!(
            outline
                .items
                .iter()
                .map(|item| (item.name.as_str(), item.line, item.column))
                .collect::<Vec<_>>(),
            [("Erste", 0, 2), ("Größe", 4, 3)]
        );
    }
}
