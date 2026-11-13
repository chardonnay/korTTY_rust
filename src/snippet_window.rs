//! Native snippet library and editor window.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{
    self, button, checkbox, column, mouse_area, row, scrollable, text, text_editor, text_input,
    tooltip,
};
use iced::{Alignment, Element, Length, Task};

use crate::clipboard_policy::{
    ClipboardPolicy, ManagedHeaderId, ManagedScriptHeader, SnippetPolicy,
};
use crate::editor_window::{Appearance, caret, lua, tools};
use crate::snippet_analysis::model::Subject as AnalysisSubject;
use crate::snippets::{self, Library, Snippet, SnippetId};

pub(crate) mod ai_assist;
pub(crate) mod analysis;
pub(crate) mod appearance;
pub(super) mod archive;
pub(crate) mod compare;
pub(super) mod copy;
mod details;
pub(crate) mod diagrams;
pub(crate) mod draft_store;
pub(crate) mod folders;
mod managed_headers;
pub(crate) mod recovery;
pub(crate) mod session;
pub(crate) mod structure;
mod tab_context;
pub(super) mod tabs;
pub(crate) mod transfer;
pub(super) mod variables;

#[derive(Clone)]
pub enum Message {
    Close,
    SaveBeforeClose,
    HostedClose(u64, bool),
    HostedSave(u64),
    Search(String),
    New,
    Select(SnippetId),
    Open(SnippetId),
    SelectTab(tabs::Id),
    CloseTab(tabs::Id),
    CloseActiveTab,
    CycleTab(bool),
    SaveTabBeforeClose,
    Recovery(recovery::Message),
    RecoveryReady,
    Folders(folders::Message),
    Structure(structure::Message),
    Analyze(AnalysisSubject),
    Assist(ai_assist::Message),
    Analysis(analysis::Message),
    AnalysisReports(Option<AnalysisSubject>),
    Transfer(transfer::Message),
    CloseOtherTabs(tabs::Id),
    CloseAllTabs,
    RevealTab(tabs::Id),
    MoveTab(tabs::Id),
    TabContext(tab_context::Message),
    MarkSnippet(SnippetId, bool),
    Compare,
    Reload,
    Name(String),
    Language(String),
    Category(String),
    Edit(text_editor::Action),
    CompleteLua,
    ChooseLua(&'static str),
    DismissLua,
    Tools(tools::Message),
    Save,
    SaveAsNew,
    Delete,
    Confirm,
    Cancel,
    ShowVariables,
    Variables(variables::Message),
    Copy(copy::Message),
    CopyCommand(snippets::one_liner::Mode),
    Import,
    Export,
    Archive(archive::Message),
    ShowDetails,
    Details(details::Message),
    ShowManagedHeaders,
    SelectManagedHeader(ManagedHeaderId),
    CloseManagedHeaders,
    OpenDiagram,
    DiagramLibrary(diagrams::Message),
    ShowRecipients,
    ExportRecipients(Result<Vec<snippets::recipients::Choice>, String>),
}

pub(crate) struct AnalysisPane<'a> {
    pub(crate) tabs: Vec<(AnalysisSubject, &'a str)>,
    pub(crate) active: Option<AnalysisSubject>,
    pub(crate) page: Option<Element<'a, Message>>,
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Snippet library action")
    }
}

impl Message {
    pub(crate) fn is_async_completion(&self) -> bool {
        if let Self::Tools(message) = self {
            return message.is_completion();
        }
        matches!(
            self,
            Self::Variables(variables::Message::ExchangeFinished(_))
                | Self::Archive(archive::Message::Finished(_))
                | Self::ExportRecipients(_)
                | Self::RecoveryReady
                | Self::Structure(structure::Message::Ready(..))
                | Self::Assist(
                    ai_assist::Message::Ready(..) | ai_assist::Message::QuotaFinished(..)
                )
                | Self::DiagramLibrary(diagrams::Message::DeleteFinished(_))
        )
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum Update {
    None,
    EditorTool(tools::Job),
    FocusEditor,
    FocusFolderName(widget::Id),
    DeleteFolder(snippets::folders::Id, Vec<SnippetId>),
    Changed,
    CloseConfirmed,
    CloseCancelled,
    ReadClipboard(u64),
    CopyResolved(String),
    ReviewCompactCommand,
    Diagram {
        source: String,
        render_locally: bool,
    },
    DiagramLibrary(diagrams::Request),
    Compare(compare::Request),
    ExchangeVariables(snippets::exchange::Action),
    ExchangeArchive(snippets::archive::Action),
    OpenRecipients,
    LoadExportRecipients(snippets::recipients::Action),
    RevealLibrary,
    MoveEditor {
        tab: tabs::Id,
        snippet: SnippetId,
    },
}

impl fmt::Debug for Update {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "None",
            Self::EditorTool(_) => "EditorTool",
            Self::FocusEditor => "FocusEditor",
            Self::FocusFolderName(_) => "FocusFolderName",
            Self::DeleteFolder(..) => "DeleteFolder",
            Self::Changed => "Changed",
            Self::CloseConfirmed => "CloseConfirmed",
            Self::CloseCancelled => "CloseCancelled",
            Self::ReadClipboard(_) => "ReadClipboard",
            Self::CopyResolved(_) => "CopyResolved",
            Self::ReviewCompactCommand => "ReviewCompactCommand",
            Self::Diagram { .. } => "Diagram",
            Self::DiagramLibrary(_) => "DiagramLibrary",
            Self::Compare(_) => "Compare",
            Self::ExchangeVariables(_) => "ExchangeVariables",
            Self::ExchangeArchive(_) => "ExchangeArchive",
            Self::OpenRecipients => "OpenRecipients",
            Self::LoadExportRecipients(_) => "LoadExportRecipients",
            Self::MoveEditor { .. } => "MoveEditor",
            Self::RevealLibrary => "RevealLibrary",
        })
    }
}

#[derive(Clone, Copy)]
enum Intent {
    New,
    Select(SnippetId),
    Reveal(SnippetId),
    Reload,
    Close,
    CloseTab(tabs::Id),
    MoveTab(tabs::Id),
    Archive(archive::Mode),
}

enum Prompt {
    Discard(Intent),
    Delete(SnippetId),
}

struct Draft {
    id: Option<SnippetId>,
    diagram_session: Arc<()>,
    analysis_revision: Arc<()>,
    base: Option<Snippet>,
    name: String,
    language: String,
    category: String,
    content: text_editor::Content,
    syntax_source: u64,
    content_changed: bool,
    metadata: snippets::Metadata,
    metadata_changed: bool,
    dirty: bool,
    recovery: recovery::State,
    structure: structure::Editor,
    lua_completion: Option<lua::Completion>,
    last_ai: Option<Arc<ai_assist::Change>>,
}

impl Draft {
    fn new(entry: Option<&Snippet>) -> Self {
        Self {
            id: entry.map(Snippet::id),
            diagram_session: Arc::new(()),
            analysis_revision: Arc::new(()),
            base: entry.cloned(),
            name: entry.map_or_else(String::new, |entry| entry.name().to_owned()),
            language: entry.map_or_else(|| "plain".into(), |entry| entry.language().to_owned()),
            category: entry.map_or_else(String::new, |entry| entry.category().to_owned()),
            content: entry.map_or_else(text_editor::Content::new, |entry| {
                text_editor::Content::with_text(entry.content())
            }),
            syntax_source: lua::source_id(),
            content_changed: false,
            metadata: entry
                .map(|entry| entry.metadata().clone())
                .unwrap_or_default(),
            metadata_changed: false,
            dirty: false,
            recovery: recovery::State::new(entry.map(Snippet::id)),
            structure: structure::Editor::default(),
            lua_completion: None,
            last_ai: None,
        }
    }

    fn edit(&mut self, action: text_editor::Action) {
        self.lua_completion = None;
        if !action.is_edit() {
            let line = self.content.cursor().position.line;
            self.content.perform(action);
            if line != self.content.cursor().position.line {
                self.structure.caret_moved();
            }
            return;
        }
        if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(text)) if text.len() > snippets::MAX_CONTENT_BYTES)
        {
            return;
        }
        let previous = self.content.text();
        let cursor = self.content.cursor();
        self.content.perform(action);
        let current = self.content.text();
        if current.len() > snippets::MAX_CONTENT_BYTES {
            self.replace_content(text_editor::Content::with_text(&previous));
            self.content.move_to(cursor);
        } else if current != previous {
            self.analysis_revision = Arc::new(());
            self.recovery.changed();
            self.structure.changed(Some(std::time::Instant::now()));
            self.content_changed = true;
            self.dirty = true;
        }
    }

    fn is_lua(&self) -> bool {
        lua::detects_snippet(&self.name, &self.language, &self.content)
    }

    fn replace_content(&mut self, replacement: text_editor::Content) {
        self.analysis_revision = Arc::new(());
        self.content = replacement;
        self.syntax_source = lua::source_id();
        self.lua_completion = None;
    }

    fn replace_saved(&mut self, entry: &Snippet) {
        let recovery =
            std::mem::replace(&mut self.recovery, recovery::State::new(Some(entry.id())));
        let mut replacement = Self::new(Some(entry));
        replacement.diagram_session = Arc::clone(&self.diagram_session);
        replacement.last_ai = self.last_ai.take();
        std::mem::swap(&mut replacement.structure, &mut self.structure);
        replacement.structure.changed(None);
        *self = replacement;
        self.recovery = recovery;
        self.recovery.update_base(Some(entry.content()));
    }
}

pub(crate) struct SnippetWindow {
    library: Library,
    pending_library: Option<Library>,
    draft: Draft,
    tabs: tabs::Tabs,
    tab_context: Option<tab_context::Menu>,
    closing_tabs: VecDeque<tabs::Id>,
    query: String,
    library_selection: Option<SnippetId>,
    search_input: widget::Id,
    library_scroll: widget::Id,
    matches: Vec<usize>,
    marked_snippets: Vec<SnippetId>,
    folders: folders::State,
    prompt: Option<Prompt>,
    hosted_close: Option<u64>,
    status: Option<String>,
    variables: Option<variables::Page>,
    copy: Option<copy::Pending>,
    archive: Option<archive::Page>,
    details: Option<details::Form>,
    snippet_policy: SnippetPolicy,
    managed_header: Option<ManagedHeaderId>,
    saved_diagrams: diagrams::State,
    recipient_path: PathBuf,
    next_copy_token: u64,
    recovery_removals: Vec<recovery::Remove>,
    orphan_drafts: Vec<draft_store::Summary>,
    recovery_busy: bool,
    analysis_apply: Option<text_editor::Content>,
    structure: structure::State,
    tools: tools::Panel,
    editor_id: widget::Id,
}

impl SnippetWindow {
    pub(crate) fn seed_workflow(&mut self, input: snippets::Input, metadata: snippets::Metadata) {
        self.tabs.new_draft(&mut self.draft);
        self.draft.name = input.name;
        self.draft.language = input.language;
        self.draft.category = input.category;
        self.draft
            .replace_content(text_editor::Content::with_text(&input.content));
        self.draft.metadata = metadata;
        self.draft.content_changed = true;
        self.draft.metadata_changed = true;
        self.draft.dirty = true;
        self.draft.recovery = recovery::State::memory_only();
        self.draft.structure.changed(None);
        self.library_selection = None;
    }

    pub(crate) fn open(path: PathBuf) -> io::Result<Self> {
        let recipient_path = path.with_file_name(snippets::recipients::FILE_NAME);
        let library = Library::open(path)?;
        let draft = Draft::new(library.entries().first());
        let library_selection = draft.id;
        let mut window = Self {
            library,
            pending_library: None,
            saved_diagrams: diagrams::State::default(),
            draft,
            tabs: tabs::Tabs::default(),
            tab_context: None,
            closing_tabs: VecDeque::new(),
            query: String::new(),
            library_selection,
            search_input: widget::Id::unique(),
            library_scroll: widget::Id::unique(),
            matches: Vec::new(),
            marked_snippets: Vec::new(),
            folders: folders::State::default(),
            prompt: None,
            hosted_close: None,
            status: None,
            variables: None,
            copy: None,
            archive: None,
            details: None,
            snippet_policy: ClipboardPolicy::load().snippets,
            managed_header: None,
            recipient_path,
            next_copy_token: 0,
            recovery_removals: Vec::new(),
            orphan_drafts: Vec::new(),
            recovery_busy: false,
            analysis_apply: None,
            structure: structure::State::default(),
            tools: tools::Panel::default(),
            editor_id: widget::Id::unique(),
        };
        window
            .structure
            .declarations_changed(window.library.variables());
        window.search();
        Ok(window)
    }

    pub(crate) fn managed_script_headers(&self) -> &[ManagedScriptHeader] {
        &self.snippet_policy.script_headers
    }

    fn custom_header_save_blocked(&self, editing: Option<SnippetId>) -> bool {
        !self.snippet_policy.allow_custom_script_headers
            && snippets::is_script_header_category(&self.draft.category)
            && !editing
                .and_then(|id| self.latest_library().get(id))
                .is_some_and(|snippet| snippets::is_script_header_category(snippet.category()))
    }

    pub(crate) fn dirty(&self) -> bool {
        self.tabs.dirty(&self.draft)
            || self.variables.as_ref().is_some_and(|page| page.dirty)
            || self
                .details
                .as_ref()
                .is_some_and(|page| page.changed(&self.draft.metadata))
    }

    pub(crate) fn restore_tabs(&mut self, occupied: &[SnippetId]) {
        let saved = match session::load(&self.library.path().with_file_name(session::FILE_NAME)) {
            Ok(saved) => saved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => {
                self.status = Some(format!("Could not restore snippet tabs ({error})"));
                return;
            }
        };
        self.restore_tab_state(saved, occupied);
    }

    pub(crate) fn restore_tab_state(&mut self, saved: session::State, occupied: &[SnippetId]) {
        let caller = self
            .tabs
            .labels(&self.draft)
            .iter()
            .any(|tab| !tab.preview)
            .then_some(self.tabs.active());
        if caller.is_none() {
            self.tabs.clear(&mut self.draft);
        }
        let mut restored = 0;
        for id in saved.ids {
            if occupied.contains(&id) || self.owns_editor(id) {
                continue;
            }
            if let Some(entry) = self.library.get(id) {
                self.tabs.browse(&mut self.draft, entry);
                self.tabs.pin();
                restored += 1;
                if restored == session::MAX_RESTORED {
                    break;
                }
            }
        }
        if let Some(caller) = caller {
            self.tabs.select(&mut self.draft, caller);
        } else if let Some(id) = saved.active {
            self.tabs.select_saved(&mut self.draft, id);
        }
        self.library_selection = self.draft.id;
    }

    pub(crate) fn tab_session(&self) -> session::State {
        self.tabs.session(&self.draft)
    }

    pub(crate) fn open_sibling(&self, id: SnippetId) -> io::Result<Self> {
        let mut window = Self::open(self.library.path().to_path_buf())?;
        if window.library.get(id).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "The snippet no longer exists",
            ));
        }
        window.update(Message::Open(id));
        Ok(window)
    }

    pub(crate) fn finish_move(&mut self, id: tabs::Id) {
        self.close_recovery_tab(id);
        self.library_selection = self.draft.id;
    }

    pub(crate) fn move_failed(&mut self, error: &io::Error) {
        self.status = Some(format!(
            "Could not open the destination Snippet Manager ({error})"
        ));
    }

    pub(crate) fn busy(&self) -> bool {
        self.recovery_busy
            || self.diagram_deleting()
            || self.analysis_apply_locked()
            || matches!(self.archive, Some(archive::Page::Running))
            || self.variables.as_ref().is_some_and(variables::Page::busy)
    }

    pub(crate) fn request_close(&mut self) -> bool {
        self.tools.invalidate(None);
        if self.busy() {
            false
        } else if self.dirty() {
            self.prompt = Some(Prompt::Discard(Intent::Close));
            false
        } else {
            true
        }
    }

    pub(crate) fn hosted_close(&self) -> Option<u64> {
        self.hosted_close
    }

    pub(crate) fn set_hosted_close(&mut self, token: Option<u64>) {
        self.hosted_close = token;
    }

    pub(crate) fn escape(&self) -> Message {
        if self.folders.menu_open() {
            return Message::Folders(folders::Message::Context(folders::Context::Dismiss));
        }
        if self.tab_context.is_some() {
            return Message::TabContext(tab_context::Message::Dismiss);
        }
        if let Some(token) = self.hosted_close {
            Message::HostedClose(token, false)
        } else if self.prompt.is_some()
            || self.variables.is_some()
            || self.copy.is_some()
            || self.archive.is_some()
            || self.details.is_some()
            || self.folders.modal()
        {
            Message::Cancel
        } else if self.managed_header.is_some() {
            Message::CloseManagedHeaders
        } else {
            Message::Close
        }
    }

    pub(crate) fn focus_search(&self) -> Task<Message> {
        widget::operation::focus(self.search_input.clone())
    }

    pub(crate) fn reveal_library_selection(&self) -> Task<Message> {
        let index = self
            .matches
            .iter()
            .position(|&index| Some(self.library.entries()[index].id()) == self.library_selection)
            .unwrap_or(0);
        widget::operation::scroll_to(
            self.library_scroll.clone(),
            widget::operation::AbsoluteOffset {
                x: 0.0,
                y: index as f32 * 36.0,
            },
        )
    }

    pub(crate) fn dismiss_tab_context(&mut self) {
        self.tab_context = None;
        self.folders.dismiss_menu();
    }

    fn can_save_before_close(&self) -> bool {
        self.tabs.dirty(&self.draft)
            && self.variables.is_none()
            && self.details.is_none()
            && !self.busy()
    }

    pub(crate) fn save_before_close(&mut self) -> bool {
        if !self.can_save_before_close() {
            return false;
        }
        while let Some(id) = self.tabs.first_dirty(&self.draft) {
            self.tabs.select(&mut self.draft, id);
            self.library_selection = self.draft.id;
            if self.save(self.draft.id) != Update::Changed {
                return false;
            }
        }
        true
    }

    pub(crate) fn native_close_prompt(&self) -> bool {
        matches!(self.prompt, Some(Prompt::Discard(Intent::Close)))
    }

    pub(crate) fn shared_library(&mut self) -> Library {
        self.pending_library = None;
        self.structure
            .declarations_changed(self.library.variables());
        self.library.clone()
    }

    pub(crate) fn refresh_shared(&mut self, library: Library) {
        if self.library.shares_store(&library) {
            self.pending_library = Some(library);
            self.apply_pending_library();
        }
    }

    pub(crate) fn latest_library(&self) -> &Library {
        self.pending_library.as_ref().unwrap_or(&self.library)
    }

    pub(crate) fn apply_pending_library(&mut self) {
        // Nested operations keep the snapshot against which the user made choices.
        // A conflicting write is rejected by Library's disk comparison.
        if self.analysis_apply_locked()
            || self.variables.is_some()
            || self.copy.is_some()
            || self.archive.is_some()
            || self.folders.modal()
        {
            return;
        }
        let Some(library) = self.pending_library.take() else {
            return;
        };
        self.library = library;
        self.structure
            .declarations_changed(self.library.variables());
        self.rebase_open_folder_changes();
        if self.prompt.is_none() && self.details.is_none() {
            let active = self.draft.id;
            self.tabs.refresh(&mut self.draft, &self.library);
            if active != self.draft.id
                || self
                    .library_selection
                    .is_some_and(|id| self.library.get(id).is_none())
            {
                self.library_selection = self.draft.id.filter(|id| self.library.get(*id).is_some());
            }
        }
        self.search();
    }

    pub(crate) fn reveal_saved(&mut self, id: SnippetId) -> bool {
        if self.analysis_apply_locked()
            || self.hosted_close.is_some()
            || self.prompt.is_some()
            || self.variables.is_some()
            || self.copy.is_some()
            || self.archive.is_some()
            || self.details.is_some()
            || self.folders.modal()
        {
            return false;
        }
        if self.draft.id == Some(id) && self.draft.dirty {
            self.library_selection = self.draft.id;
            self.query.clear();
            self.folders.show_snippet(&self.library, id);
            self.search();
        } else {
            self.change(Intent::Reveal(id));
        }
        true
    }

    pub(crate) fn owns_editor(&self, id: SnippetId) -> bool {
        self.tabs.pinned(&self.draft, id)
    }

    pub(crate) fn selected_id(&self) -> Option<SnippetId> {
        self.draft.id
    }

    pub(crate) fn analysis_library(&self) -> &Library {
        &self.library
    }

    pub(crate) fn analysis_notice(&mut self, notice: String) {
        self.status = Some(notice);
    }

    pub(crate) fn analysis_language(&self, id: SnippetId) -> Option<&str> {
        if self.draft.id == Some(id) && self.draft.dirty {
            Some(&self.draft.language)
        } else {
            self.library.get(id).map(Snippet::language)
        }
    }

    pub(crate) fn analysis_snippet(&self, id: SnippetId) -> Option<(String, String, String)> {
        let snippet = self.library.get(id)?;
        if self.draft.id == Some(id) && self.draft.dirty {
            Some((
                self.draft.name.clone(),
                self.draft.language.clone(),
                self.draft.content.text(),
            ))
        } else {
            Some((
                snippet.name().into(),
                snippet.language().into(),
                snippet.content().into(),
            ))
        }
    }

    pub(crate) fn analysis_editor_revision(&self, id: SnippetId) -> Option<std::sync::Weak<()>> {
        (self.draft.id == Some(id)).then(|| Arc::downgrade(&self.draft.analysis_revision))
    }

    pub(crate) fn analysis_selection(&self, id: SnippetId) -> Vec<SnippetId> {
        self.marked_move_ids(id)
    }

    pub(crate) fn analysis_apply_locked(&self) -> bool {
        self.analysis_apply.is_some()
    }

    pub(crate) fn can_apply_analysis_to(&self, id: SnippetId) -> bool {
        self.draft.id == Some(id)
            && self.library.get(id).is_some()
            && !self.tabs.is_preview()
            && self.draft_input_available()
    }

    fn draft_input_available(&self) -> bool {
        !self.busy()
            && self.hosted_close.is_none()
            && self.prompt.is_none()
            && self.variables.is_none()
            && self.copy.is_none()
            && self.archive.is_none()
            && self.details.is_none()
            && !self.folders.modal()
    }

    pub(crate) fn analysis_apply_matches(&self, id: SnippetId, before: &str) -> bool {
        self.can_apply_analysis_to(id)
            && self
                .analysis_snippet(id)
                .is_some_and(|(_, _, content)| content == before)
    }

    pub(crate) fn folder_apply_blocked(&self, ids: &[SnippetId]) -> bool {
        self.folder_delete_blocker(ids).is_some()
            || (self.draft.id.is_some_and(|id| ids.contains(&id))
                && (self.analysis_apply_locked()
                    || self.prompt.is_some()
                    || self.details.is_some()))
    }

    pub(crate) fn replace_reviewed_contents<'a>(
        &mut self,
        changes: impl IntoIterator<Item = (SnippetId, &'a str, &'a str)>,
    ) -> io::Result<()> {
        if self.analysis_apply_locked()
            || self.busy()
            || self.hosted_close.is_some()
            || self.prompt.is_some()
            || self.variables.is_some()
            || self.copy.is_some()
            || self.archive.is_some()
            || self.details.is_some()
            || self.folders.modal()
        {
            return Err(io::Error::other(
                "Close the pending editor operation before saving changes.",
            ));
        }
        let mut library = self.library.clone();
        library.replace_contents(changes)?;
        self.refresh_shared(library);
        Ok(())
    }

    pub(crate) fn lock_analysis_apply(
        &mut self,
        id: SnippetId,
        before: &str,
        after: &str,
    ) -> Result<(), String> {
        if !self.analysis_apply_matches(id, before) {
            return Err(
                "Open the original editable snippet at the exact reviewed revision.".into(),
            );
        }
        let replacement = text_editor::Content::with_text(after);
        if replacement.text() != after {
            return Err("The editor could not represent the exact proposed text.".into());
        }
        self.analysis_apply = Some(replacement);
        Ok(())
    }

    pub(crate) fn finish_analysis_apply(&mut self, accept: bool) {
        if let Some(replacement) = self.analysis_apply.take()
            && accept
        {
            self.draft.replace_content(replacement);
            self.draft.recovery.changed();
            self.draft
                .structure
                .changed(Some(std::time::Instant::now()));
            self.draft.content_changed = true;
            self.draft.dirty = true;
        }
        self.apply_pending_library();
    }

    pub(crate) fn jump_to_analysis_line(&mut self, id: SnippetId, line: usize) -> bool {
        self.jump_to_analysis_range(id, line, line)
    }

    pub(crate) fn jump_to_analysis_range(
        &mut self,
        id: SnippetId,
        start: usize,
        end: usize,
    ) -> bool {
        if self.draft.id != Some(id) {
            return false;
        }
        let Some(first) = start.checked_sub(1) else {
            return false;
        };
        let Some(last) = end.checked_sub(1).filter(|&last| last >= first) else {
            return false;
        };
        let Some(row) = self.draft.content.line(last) else {
            return false;
        };
        let column = row.text.len();
        self.draft.content.move_to(text_editor::Cursor {
            position: text_editor::Position { line: last, column },
            selection: (first != last || column > 0).then_some(text_editor::Position {
                line: first,
                column: 0,
            }),
        });
        true
    }

    fn search_target(&self) -> Option<SnippetId> {
        let mut visible = self
            .matches
            .iter()
            .map(|&index| self.library.entries()[index].id());
        let first = visible.next()?;
        if self.library_selection == Some(first)
            || visible.any(|id| Some(id) == self.library_selection)
        {
            self.library_selection
        } else {
            Some(first)
        }
    }

    pub(crate) fn library_key(&self, key: &iced::keyboard::Key) -> Option<Message> {
        use iced::keyboard::{Key, key::Named};
        if self.prompt.is_some()
            || self.variables.is_some()
            || self.copy.is_some()
            || self.archive.is_some()
            || self.details.is_some()
            || self.hosted_close.is_some()
            || self.folders.modal()
        {
            return None;
        }
        if *key == Key::Named(Named::Enter) {
            return self.search_target().map(Message::Open);
        }
        let backwards = match key {
            Key::Named(Named::ArrowUp) => true,
            Key::Named(Named::ArrowDown) => false,
            _ => return None,
        };
        let selected = self
            .matches
            .iter()
            .position(|&index| Some(self.library.entries()[index].id()) == self.library_selection);
        let index = match selected {
            Some(index) if backwards => index.saturating_sub(1),
            Some(index) => (index + 1).min(self.matches.len().saturating_sub(1)),
            None => 0,
        };
        self.matches
            .get(index)
            .map(|&index| Message::Select(self.library.entries()[index].id()))
    }

    pub(crate) fn same_library(&self, other: &Self) -> bool {
        self.library.shares_store(&other.library)
    }

    pub(crate) fn edit_target(&self, message: &Message) -> Option<SnippetId> {
        match message {
            Message::Open(id) => Some(*id),
            Message::Name(_)
            | Message::Language(_)
            | Message::Category(_)
            | Message::Save
            | Message::Delete
            | Message::ShowDetails => self.draft.id,
            Message::Edit(action) if action.is_edit() => self.draft.id,
            _ => None,
        }
    }

    pub(crate) fn close_active_tab(&self) -> Message {
        Message::CloseTab(self.tabs.active())
    }

    pub(crate) fn update(&mut self, message: Message) -> Update {
        let update = self.update_inner(message);
        if self.tools.active() {
            let context = self.tool_context();
            self.tools.invalidate(context);
        }
        update
    }

    fn update_inner(&mut self, message: Message) -> Update {
        if self.analysis_apply_locked() && !message.is_async_completion() {
            return Update::None;
        }
        if !matches!(message, Message::Folders(_)) && !message.is_async_completion() {
            self.folders.dismiss_menu();
        }
        if !matches!(message, Message::TabContext(_)) && !message.is_async_completion() {
            self.tab_context = None;
        }
        match message {
            _ if self.hosted_close.is_some() && !message.is_async_completion() => {}
            _ if self.recovery_busy && !message.is_async_completion() => {}
            Message::Confirm => return self.confirm(),
            Message::SaveTabBeforeClose => {
                if let Some(Prompt::Discard(intent @ (Intent::CloseTab(_) | Intent::MoveTab(_)))) =
                    self.prompt
                    && self.save(self.draft.id) == Update::Changed
                {
                    self.prompt = None;
                    let update = self.apply(intent);
                    return if update == Update::None {
                        Update::Changed
                    } else {
                        update
                    };
                }
            }
            Message::Cancel => {
                if let Some(prompt) = self.prompt.take() {
                    self.closing_tabs.clear();
                    return if matches!(prompt, Prompt::Discard(Intent::Close)) {
                        Update::CloseCancelled
                    } else {
                        Update::None
                    };
                }
                if self.copy.is_some() {
                    return self.update_copy(copy::Message::Cancel);
                }
                if self.archive.is_some() {
                    return self.update_archive(archive::Message::Cancel);
                }
                if self.details.is_some() {
                    self.details = None;
                    return Update::None;
                }
                if self.folders.modal() {
                    self.folders.cancel();
                    return Update::None;
                }
                return self.update_variables(variables::Message::Cancel);
            }
            Message::Variables(message @ variables::Message::ExchangeFinished(_)) => {
                return self.update_variables(message);
            }
            Message::Archive(message @ archive::Message::Finished(_)) => {
                return self.update_archive(message);
            }
            Message::ExportRecipients(result) => {
                if let Some(archive::Page::Export(page)) = self.archive.as_mut() {
                    page.set_recipients(result);
                }
            }
            Message::Structure(message @ structure::Message::Ready(..)) => {
                return self.update_structure(message);
            }
            Message::Tools(message) if message.is_completion() => {
                return self.update_tools(message);
            }
            Message::DiagramLibrary(message @ diagrams::Message::DeleteFinished(_)) => {
                return self.update_diagrams(message);
            }
            _ if self.prompt.is_some() => {}
            Message::Close => {
                if self.request_close() {
                    return Update::CloseConfirmed;
                }
            }
            Message::SelectManagedHeader(id)
                if self.managed_header.is_some()
                    && self
                        .managed_script_headers()
                        .iter()
                        .any(|header| header.id == id) =>
            {
                self.managed_header = Some(id);
            }
            Message::CloseManagedHeaders => self.managed_header = None,
            _ if self.managed_header.is_some() && !message.is_async_completion() => {}
            Message::Archive(message) if self.archive.is_some() => {
                return self.update_archive(message);
            }
            _ if self.archive.is_some() => {}
            Message::Details(message) if self.details.is_some() => self.update_details(message),
            _ if self.details.is_some() => {}
            Message::Copy(message) if self.variables.is_none() => return self.update_copy(message),
            Message::Variables(message) if self.copy.is_none() => {
                return self.update_variables(message);
            }
            _ if self.copy.is_some() || self.variables.is_some() => {}
            Message::Folders(message) => return self.update_folders(message),
            _ if self.folders.modal() => {}
            Message::CopyCommand(mode) if !self.tabs.is_empty() && !self.busy() => {
                return self.copy_snippet_command(mode);
            }
            Message::ShowManagedHeaders => {
                self.snippet_policy = ClipboardPolicy::load().snippets;
                self.managed_header = self
                    .managed_script_headers()
                    .first()
                    .map(|header| header.id);
            }
            Message::Structure(message) => return self.update_structure(message),
            Message::TabContext(message) => {
                if let Some(action) = self.update_tab_context(message) {
                    return self.update(action);
                }
            }
            Message::CloseOtherTabs(keep) if self.tabs.contains(keep) => {
                self.closing_tabs = self
                    .tabs
                    .labels(&self.draft)
                    .iter()
                    .filter(|tab| tab.id != keep)
                    .map(|tab| tab.id)
                    .collect();
                return self.continue_closing_tabs();
            }
            Message::CloseAllTabs => {
                self.closing_tabs = self
                    .tabs
                    .labels(&self.draft)
                    .iter()
                    .map(|tab| tab.id)
                    .collect();
                return self.continue_closing_tabs();
            }
            Message::RevealTab(id) => {
                if let Some(saved) = self.tabs.entry(&self.draft, id).and_then(|draft| draft.id) {
                    self.query.clear();
                    self.folders.show_snippet(&self.library, saved);
                    self.search();
                    self.library_selection = Some(saved);
                    return Update::RevealLibrary;
                }
            }
            Message::MoveTab(id)
                if self
                    .tabs
                    .labels(&self.draft)
                    .iter()
                    .any(|tab| tab.id == id && !tab.preview) =>
            {
                self.closing_tabs.clear();
                self.tabs.select(&mut self.draft, id);
                self.library_selection = self.draft.id;
                if self.draft.id.is_some() {
                    return self.change(Intent::MoveTab(id));
                }
            }
            Message::SelectTab(id) => {
                self.tabs.select(&mut self.draft, id);
                self.library_selection = self.draft.id;
                self.status = None;
            }
            Message::CycleTab(backwards) => {
                let labels = self.tabs.labels(&self.draft);
                if let Some(index) = labels.iter().position(|tab| tab.active) {
                    let step = if backwards { labels.len() - 1 } else { 1 };
                    let id = labels[(index + step) % labels.len()].id;
                    self.tabs.select(&mut self.draft, id);
                    self.library_selection = self.draft.id;
                }
            }
            Message::CloseActiveTab => return self.update(self.close_active_tab()),
            Message::CloseTab(id) if self.tabs.contains(id) => {
                self.closing_tabs.clear();
                self.tabs.select(&mut self.draft, id);
                self.library_selection = self.draft.id;
                return self.change(Intent::CloseTab(id));
            }
            Message::ShowVariables => self.variables = Some(variables::Page::new()),
            Message::ShowDetails => {
                self.tabs.pin();
                self.details = Some(details::Form::new(&self.draft.metadata));
            }
            Message::ShowRecipients => return Update::OpenRecipients,
            Message::Search(query) if query.len() <= snippets::MAX_NAME_BYTES => {
                self.query = query;
                self.search();
            }
            Message::New => return self.change(Intent::New),
            Message::Select(id) => {
                self.library_selection = Some(id);
                return self.change(Intent::Select(id));
            }
            Message::Open(id) => {
                if let Some(entry) = self.library.get(id) {
                    self.tabs.browse(&mut self.draft, entry);
                    self.tabs.pin();
                    self.library_selection = self.draft.id;
                }
            }
            Message::MarkSnippet(id, marked) => self.mark_snippet(id, marked),
            Message::Compare => {
                if let Some((left, right)) = self.comparison_pair() {
                    return Update::Compare(compare::Request::new(left, right));
                }
            }
            Message::Reload => return self.change(Intent::Reload),
            Message::Import => return self.change(Intent::Archive(archive::Mode::Import)),
            Message::Export => return self.change(Intent::Archive(archive::Mode::Export)),
            Message::OpenDiagram => return self.open_diagram(),
            Message::DiagramLibrary(message) => return self.update_diagrams(message),
            Message::Name(name) if name.len() <= snippets::MAX_NAME_BYTES => {
                self.draft.lua_completion = None;
                self.tabs.pin();
                if self.draft.name != name {
                    self.draft.analysis_revision = Arc::new(());
                    self.tools.invalidate(None);
                    self.draft.recovery.changed();
                }
                self.draft.dirty |= self.draft.name != name;
                self.draft.name = name;
            }
            Message::Language(language) if language.len() <= snippets::MAX_LANGUAGE_BYTES => {
                self.draft.lua_completion = None;
                self.tabs.pin();
                if self.draft.language != language {
                    self.draft.analysis_revision = Arc::new(());
                    self.draft.recovery.changed();
                    self.draft.structure.changed(None);
                }
                self.draft.dirty |= self.draft.language != language;
                self.draft.language = language;
            }
            Message::Category(category) if category.len() <= snippets::MAX_CATEGORY_BYTES => {
                self.tabs.pin();
                if self.draft.category != category {
                    self.draft.recovery.changed();
                }
                self.draft.dirty |= self.draft.category != category;
                self.draft.category = category;
            }
            Message::Edit(action) => {
                if action.is_edit() {
                    self.tabs.pin();
                }
                self.draft.edit(action);
            }
            Message::CompleteLua if !self.tabs.is_empty() && self.draft.is_lua() => {
                self.draft.lua_completion = lua::Completion::open(&self.draft.content);
                if self.draft.lua_completion.is_none() {
                    self.status = Some("No Lua completions at the caret.".into());
                }
            }
            Message::ChooseLua(item) if !self.tabs.is_empty() && self.draft.is_lua() => {
                if let Some(action) = self
                    .draft
                    .lua_completion
                    .take()
                    .and_then(|completion| completion.edit(&mut self.draft.content, item))
                {
                    self.tabs.pin();
                    self.draft.edit(action);
                    return Update::FocusEditor;
                }
            }
            Message::DismissLua => self.draft.lua_completion = None,
            Message::Tools(message) => return self.update_tools(message),
            Message::Save => return self.save(self.draft.id),
            Message::SaveAsNew => return self.save(None),
            Message::Delete if !self.draft.dirty => {
                self.prompt = self.draft.id.map(Prompt::Delete);
            }
            _ => {}
        }
        Update::None
    }

    fn tool_context(&self) -> Option<tools::Context> {
        if self.tabs.is_empty()
            || self.analysis_apply.is_some()
            || self.prompt.is_some()
            || self.archive.is_some()
            || self.details.is_some()
            || self.managed_header.is_some()
            || self.copy.is_some()
            || self.variables.is_some()
            || self.folders.modal()
        {
            return None;
        }
        let language = if self.draft.is_lua() {
            tools::Language::Lua
        } else {
            tools::Language::named(&self.draft.language).or_else(|| {
                let language = self.draft.language.trim();
                (language.is_empty() || language.eq_ignore_ascii_case("plain"))
                    .then(|| tools::Language::extension(std::path::Path::new(&self.draft.name)))
                    .flatten()
            })?
        };
        Some(tools::Context {
            revision: self.draft.structure.generation(),
            language,
            limit: snippets::MAX_CONTENT_BYTES,
            remote: false,
            writable: true,
        })
    }

    fn update_tools(&mut self, message: tools::Message) -> Update {
        let context = self.tool_context();
        match self
            .tools
            .update(message, context, None, || self.draft.content.text())
        {
            tools::Update::None => {}
            tools::Update::Notice(notice) => self.status = Some(notice),
            tools::Update::Job(job) => return Update::EditorTool(job),
            tools::Update::Apply(formatted) => {
                let replacement = text_editor::Content::with_text(&formatted);
                if replacement.text() != *formatted {
                    self.status = Some("The editor could not represent the formatted text.".into());
                    return Update::None;
                }
                let cursor = self.draft.content.cursor().position;
                let line = cursor.line.min(replacement.line_count().saturating_sub(1));
                let column = replacement.line(line).map_or(0, |row| {
                    row.text
                        .floor_char_boundary(cursor.column.min(row.text.len()))
                });
                self.tabs.pin();
                self.draft.replace_content(replacement);
                self.draft.content.move_to(text_editor::Cursor {
                    position: text_editor::Position { line, column },
                    selection: None,
                });
                self.draft.lua_completion = None;
                self.draft.recovery.changed();
                self.draft
                    .structure
                    .changed(Some(std::time::Instant::now()));
                self.draft.content_changed = true;
                self.draft.dirty = true;
                self.status = Some("Formatted the snippet buffer.".into());
                return Update::FocusEditor;
            }
        }
        Update::None
    }

    fn open_diagram(&mut self) -> Update {
        if self.draft.content.line_count() > kortty_diagram::MAX_SCRIPT_LINES {
            self.status = Some("The diagram source exceeds 100,000 lines".into());
            return Update::None;
        }
        let source = self.draft.content.text();
        if source.len() > kortty_diagram::MAX_SCRIPT_BYTES || source.trim().is_empty() {
            self.status = Some("Enter source code up to 2 MiB for the diagram".into());
            return Update::None;
        }
        let language = self.draft.language.trim().to_ascii_lowercase();
        let render_locally = matches!(
            language.as_str(),
            "sh" | "bash" | "shell" | "shellscript" | "dash" | "ash" | "zsh" | "ksh"
        );
        Update::Diagram {
            source,
            render_locally,
        }
    }

    fn update_variables(&mut self, message: variables::Message) -> Update {
        let Some(page) = self.variables.as_mut() else {
            return Update::None;
        };
        match page.update(message, &mut self.library) {
            variables::Update::None => Update::None,
            variables::Update::Back => {
                self.variables = None;
                Update::None
            }
            variables::Update::Changed => Update::Changed,
            variables::Update::Exchange(action) => Update::ExchangeVariables(action),
        }
    }

    fn update_details(&mut self, message: details::Message) {
        if matches!(message, details::Message::Cancel) {
            self.details = None;
            return;
        }
        if let Some(metadata) = self.details.as_mut().and_then(|page| page.update(message)) {
            let changed = metadata != self.draft.metadata;
            if changed {
                self.draft.recovery.changed();
            }
            self.draft.dirty |= changed;
            self.draft.metadata_changed |= changed;
            self.draft.metadata = metadata;
            self.details = None;
            self.status = None;
        }
    }

    fn update_archive(&mut self, message: archive::Message) -> Update {
        use snippets::archive::Outcome;
        let Some(page) = &mut self.archive else {
            return Update::None;
        };
        match message {
            archive::Message::Finished(result) if matches!(page, archive::Page::Running) => {
                self.archive = None;
                match result {
                    Ok(Outcome::Imported(imports)) => {
                        let before = self.library.entries().len();
                        let before_folders = self.library.folders().len();
                        let java_folders = imports.java_folder_count();
                        let rejected = imports.rejected_text;
                        let skipped_diagrams = imports.skipped_diagrams;
                        let skipped_references = imports.skipped_references;
                        match self
                            .library
                            .import_snippets(std::sync::Arc::unwrap_or_clone(imports))
                        {
                            Ok(first) => {
                                let count = self.library.entries().len() - before;
                                let folders = self.library.folders().len() - before_folders;
                                let mut status = format!("Imported {count} snippets");
                                if java_folders > 0 && folders > 0 {
                                    status.push_str(&format!(" · added {folders} folders"));
                                }
                                if rejected > 0 {
                                    status.push_str(&format!(
                                        " · skipped {rejected} binary or oversized text files"
                                    ));
                                }
                                if skipped_diagrams > 0 {
                                    status.push_str(&format!(" · skipped {skipped_diagrams} invalid or unsupported diagrams"));
                                }
                                if skipped_references > 0 {
                                    status.push_str(&format!(
                                        " · skipped {skipped_references} invalid diagram references"
                                    ));
                                }
                                self.status = Some(status);
                                if let Some(first) = first
                                    && let Some(entry) = self.library.get(first)
                                {
                                    self.tabs.browse(&mut self.draft, entry);
                                    self.library_selection = self.draft.id;
                                }
                                if count > 0 || folders > 0 {
                                    self.search();
                                    return Update::Changed;
                                }
                            }
                            Err(error) => {
                                self.status = Some(format!("Could not import snippets ({error})"))
                            }
                        }
                    }
                    Ok(Outcome::Exported(count)) => {
                        self.status = Some(format!("Exported {count} snippets"));
                    }
                    Err(error) => self.status = Some(format!("Snippet exchange failed ({error})")),
                }
            }
            archive::Message::Cancel if !matches!(page, archive::Page::Running) => {
                self.archive = None;
            }
            _ => {
                if let Some(action) = page.update(message, &self.library, &self.recipient_path) {
                    *page = archive::Page::Running;
                    return Update::ExchangeArchive(action);
                }
            }
        }
        Update::None
    }

    fn change(&mut self, intent: Intent) -> Update {
        let dirty = match intent {
            Intent::Close | Intent::Reload => self.tabs.dirty(&self.draft),
            Intent::CloseTab(_) | Intent::MoveTab(_) => self.draft.dirty,
            _ => false,
        };
        if dirty {
            self.prompt = Some(Prompt::Discard(intent));
            Update::None
        } else {
            self.apply(intent)
        }
    }

    fn apply(&mut self, intent: Intent) -> Update {
        self.status = None;
        match intent {
            Intent::New => self.tabs.new_draft(&mut self.draft),
            Intent::Select(id) => {
                if let Some(entry) = self.library.get(id) {
                    self.tabs.browse(&mut self.draft, entry);
                }
            }
            Intent::Reveal(id) => match self.library.reload() {
                Ok(()) => {
                    self.structure
                        .declarations_changed(self.library.variables());
                    if let Some(selected) = self.library.get(id) {
                        self.query.clear();
                        self.folders.show_snippet(&self.library, id);
                        self.tabs.browse(&mut self.draft, selected);
                    } else {
                        self.status = Some("The selected snippet no longer exists".into());
                    }
                    self.tabs.refresh(&mut self.draft, &self.library);
                    self.search();
                }
                Err(error) => self.status = Some(format!("Could not reload snippets ({error})")),
            },
            Intent::Reload => match self.library.reload() {
                Ok(()) => {
                    self.snippet_policy = ClipboardPolicy::load().snippets;
                    self.managed_header = None;
                    self.discard_owned_recovery();
                    self.tabs.reset(&mut self.draft, &self.library);
                    self.search();
                    self.status = Some("Library reloaded".into());
                    return Update::Changed;
                }
                Err(error) => self.status = Some(format!("Could not reload snippets ({error})")),
            },
            Intent::Close => {}
            Intent::CloseTab(id) => {
                self.close_recovery_tab(id);
                return self.continue_closing_tabs();
            }
            Intent::MoveTab(tab) => {
                if let Some(snippet) = self.tabs.entry(&self.draft, tab).and_then(|draft| draft.id)
                {
                    return Update::MoveEditor { tab, snippet };
                }
            }
            Intent::Archive(mode) => {
                self.archive = Some(archive::Page::new(mode, self.draft.id, &self.library));
                if matches!(mode, archive::Mode::Export) {
                    return Update::LoadExportRecipients(snippets::recipients::Action::List(
                        self.recipient_path.clone(),
                    ));
                }
            }
        }
        self.library_selection = self.draft.id;
        Update::None
    }

    fn continue_closing_tabs(&mut self) -> Update {
        while let Some(id) = self.closing_tabs.pop_front() {
            if !self.tabs.contains(id) {
                continue;
            }
            self.tabs.select(&mut self.draft, id);
            if self.draft.dirty {
                self.prompt = Some(Prompt::Discard(Intent::CloseTab(id)));
                break;
            }
            self.close_recovery_tab(id);
        }
        self.library_selection = self.draft.id;
        Update::None
    }

    fn confirm(&mut self) -> Update {
        match self.prompt.take() {
            Some(Prompt::Discard(Intent::Close)) => Update::CloseConfirmed,
            Some(Prompt::Discard(intent)) => self.apply(intent),
            Some(Prompt::Delete(id)) if self.library.get(id) != self.draft.base.as_ref() => {
                self.status =
                    Some("This snippet changed in another manager. Reload it first.".into());
                Update::None
            }
            Some(Prompt::Delete(id)) => match self.library.remove(id) {
                Ok(()) => {
                    self.recovery_removals.extend(
                        self.tabs
                            .editors(&self.draft)
                            .filter(|draft| draft.id == Some(id))
                            .filter_map(|draft| draft.recovery.removed()),
                    );
                    self.tabs.remove(&mut self.draft, id);
                    self.library_selection = self.draft.id;
                    self.search();
                    self.status = Some("Snippet deleted".into());
                    Update::Changed
                }
                Err(error) => {
                    self.status = Some(format!("Could not delete snippet ({error})"));
                    Update::None
                }
            },
            None => Update::None,
        }
    }

    fn save(&mut self, id: Option<SnippetId>) -> Update {
        if let Some(id) = id
            && self.library.get(id) != self.draft.base.as_ref()
        {
            self.status = Some("This snippet changed in another manager. Reload it first.".into());
            return Update::None;
        }
        let content = self
            .draft
            .base
            .as_ref()
            .filter(|_| !self.draft.content_changed)
            .map_or_else(|| self.draft.content.text().into(), Snippet::shared_content);
        let input = snippets::Input {
            name: self.draft.name.clone(),
            language: self.draft.language.clone(),
            category: self.draft.category.clone(),
            content,
        };
        let mut omitted_diagrams = false;
        let saved = if id.is_none() {
            let key = if self.draft.id.is_none() {
                self.draft.recovery_key()
            } else {
                SnippetId::new()
            };
            key.and_then(|key| {
                let mut metadata = self.draft.metadata.clone();
                if self.draft.recovery.is_memory_only() {
                    let initial = metadata.diagrams.len();
                    metadata
                        .diagrams
                        .retain(|diagram| diagram.captured_for(&input.content));
                    omitted_diagrams = metadata.diagrams.len() != initial;
                }
                self.library.create_with_id(key, input, metadata)
            })
        } else if self.draft.metadata_changed {
            self.library
                .save_with_metadata(id, input, Some(self.draft.metadata.clone()))
        } else {
            self.library.save(id, input)
        };
        match saved {
            Ok(id) => {
                let previous_id = self.draft.id;
                if let Some(remove) = self.draft.recovery.removed() {
                    self.recovery_removals.push(remove);
                }
                let mut recovery =
                    std::mem::replace(&mut self.draft.recovery, recovery::State::new(Some(id)));
                recovery.saved();
                let mut replacement = Draft::new(self.library.get(id));
                if previous_id == Some(id) {
                    replacement.diagram_session = Arc::clone(&self.draft.diagram_session);
                }
                std::mem::swap(&mut replacement.structure, &mut self.draft.structure);
                replacement.structure.changed(None);
                self.draft = replacement;
                if previous_id == Some(id) {
                    self.draft.recovery = recovery;
                    self.draft
                        .recovery
                        .update_base(self.draft.base.as_ref().map(Snippet::content));
                }
                self.library_selection = self.draft.id;
                self.tabs.pin();
                self.search();
                self.status = Some(if omitted_diagrams {
                    "Snippet saved. Diagrams for earlier source were omitted.".into()
                } else {
                    "Snippet saved".into()
                });
                Update::Changed
            }
            Err(error) => {
                self.status = Some(format!("Could not save snippet ({error})"));
                Update::None
            }
        }
    }

    fn search(&mut self) {
        self.folders.refresh(&self.library);
        let scope = self.folders.scope(&self.library);
        let query = self.query.trim().to_lowercase();
        let entries = self.library.entries();
        self.matches.clear();
        self.matches
            .extend(entries.iter().enumerate().filter_map(|(index, entry)| {
                if scope
                    .as_ref()
                    .is_some_and(|scope| !scope.contains(&entry.id()))
                {
                    return None;
                }
                (query.is_empty()
                    || [
                        entry.name(),
                        entry.category(),
                        entry.language(),
                        &entry.metadata().description,
                        &entry.metadata().folder_path,
                        entry.content(),
                    ]
                    .iter()
                    .any(|value| value.to_lowercase().contains(&query))
                    || entry
                        .metadata()
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query)))
                .then_some(index)
            }));
        self.matches
            .sort_by_cached_key(|&index| entries[index].name().to_lowercase());
        self.marked_snippets
            .retain(|id| self.matches.iter().any(|&index| entries[index].id() == *id));
    }

    fn mark_snippet(&mut self, id: SnippetId, marked: bool) {
        if !marked {
            self.marked_snippets.retain(|&previous| previous != id);
        } else if !self.marked_snippets.contains(&id)
            && self
                .matches
                .iter()
                .any(|&index| self.library.entries()[index].id() == id)
        {
            self.marked_snippets.push(id);
        }
    }

    fn comparison_pair(&self) -> Option<(&Snippet, &Snippet)> {
        if self.marked_snippets.len() != 2 {
            return None;
        }
        let mut selected = self
            .matches
            .iter()
            .map(|&index| &self.library.entries()[index])
            .filter(|entry| self.marked_snippets.contains(&entry.id()));
        Some((selected.next()?, selected.next()?))
    }

    fn snippet_rows<'a>(
        &'a self,
        zone: &folders::drag::Zone,
        transfer_enabled: bool,
    ) -> Element<'a, Message> {
        let rows = self
            .matches
            .iter()
            .map(|&index| &self.library.entries()[index])
            .fold(column![].spacing(4), |rows, entry| {
                let id = entry.id();
                let label = mouse_area(
                    widget::container(
                        text(entry.label())
                            .size(13)
                            .wrapping(iced::advanced::text::Wrapping::None),
                    )
                    .padding(6)
                    .height(32)
                    .clip(true)
                    .width(Length::Fill)
                    .style(if self.library_selection == Some(id) {
                        widget::container::primary
                    } else {
                        widget::container::bordered_box
                    }),
                )
                .on_press(Message::Select(id))
                .on_double_click(Message::Open(id));
                let label = self.folders.context(
                    &self.library,
                    folders::MenuTarget::Snippet(id),
                    transfer_enabled,
                    label.into(),
                );
                rows.push(
                    row![
                        tooltip(
                            checkbox(self.marked_snippets.contains(&id))
                                .on_toggle(move |marked| Message::MarkSnippet(id, marked)),
                            "Select snippet for comparison, moving, or copying to a terminal",
                            tooltip::Position::Top,
                        ),
                        zone.item(label, Some(folders::drag::Source::Snippet(id)), None),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
            });
        scrollable(rows)
            .id(self.library_scroll.clone())
            .height(Length::Fill)
            .into()
    }

    pub(crate) fn view<'a>(
        &'a self,
        content_theme: iced::Theme,
        appearance: Appearance,
        caret_preferences: caret::Preferences,
        open_elsewhere: bool,
        transfer: transfer::Context<'a>,
        analysis: AnalysisPane<'a>,
    ) -> Element<'a, Message> {
        if let Some(page) = transfer.page {
            return transfer::view(page);
        }
        if let Some(token) = self.hosted_close
            && !self.busy()
        {
            return self.close_prompt(
                Message::HostedClose(token, false),
                Message::HostedClose(token, true),
                Message::HostedSave(token),
            );
        }
        if self.native_close_prompt() {
            return self.close_prompt(Message::Cancel, Message::Confirm, Message::SaveBeforeClose);
        }
        if matches!(
            self.prompt,
            Some(Prompt::Discard(Intent::CloseTab(_) | Intent::MoveTab(_)))
        ) {
            let operation = if matches!(self.prompt, Some(Prompt::Discard(Intent::MoveTab(_)))) {
                "moving to a new window"
            } else {
                "closing"
            };
            return column![
                text(format!(
                    "Save changes to {} before {operation}?",
                    self.draft.name
                ))
                .size(19),
                text(self.status.as_deref().unwrap_or_default()),
                row![
                    button("Keep editing").on_press(Message::Cancel),
                    button("Discard").on_press(Message::Confirm),
                    button("Save").on_press(Message::SaveTabBeforeClose),
                ]
                .spacing(8),
            ]
            .padding(16)
            .spacing(12)
            .into();
        }
        if let Some(prompt) = &self.prompt {
            let (question, confirm) = match prompt {
                Prompt::Discard(_) if self.variables.is_some() => {
                    ("Discard unsaved snippet or variable edits?", "Discard")
                }
                Prompt::Discard(_) => ("Discard unsaved snippet edits?", "Discard"),
                Prompt::Delete(_) => (
                    "Delete this snippet from the library? Linked jobs will be blocked.",
                    "Delete",
                ),
            };
            return column![
                text(question).size(19),
                row![
                    button("Keep editing").on_press(Message::Cancel),
                    button(confirm).on_press(Message::Confirm)
                ]
                .spacing(8),
            ]
            .padding(16)
            .spacing(12)
            .into();
        }
        if let Some(page) = self.variables.as_ref() {
            return page.view(&self.library).map(Message::Variables);
        }
        if let Some(pending) = self.copy.as_ref() {
            return pending.view().map(Message::Copy);
        }
        if let Some(page) = self.archive.as_ref() {
            return page.view(&self.library).map(Message::Archive);
        }
        if let Some(page) = self.details.as_ref() {
            return page.view(&self.library).map(Message::Details);
        }
        if let Some(page) = self.folders.page_view(&self.library) {
            return page;
        }
        if let Some(id) = self.managed_header
            && let Some(header) = self
                .managed_script_headers()
                .iter()
                .find(|header| header.id == id)
        {
            return managed_headers::view(self.managed_script_headers(), header);
        }
        let library = folders::drag::scope(|zone| {
            let snippet_zone = zone.clone();
            let split = folders::view::library_split(
                &self.folders,
                &self.library,
                &zone,
                transfer.enabled,
                move || self.snippet_rows(&snippet_zone, transfer.enabled),
            );
            let move_snippet = self
                .marked_snippets
                .first()
                .copied()
                .or(self.search_target());
            column![
                text("Snippet library").size(20),
                text_input("Search snippets", &self.query)
                    .id(self.search_input.clone())
                    .on_input(Message::Search)
                    .on_submit_maybe(self.search_target().map(Message::Open)),
                split,
                row![
                    button("New").on_press(Message::New),
                    button("Reload").on_press(Message::Reload),
                    button("Variables").on_press(Message::ShowVariables),
                ]
                .spacing(6),
                (!self.managed_script_headers().is_empty())
                    .then(
                        || button("Managed script headers…").on_press(Message::ShowManagedHeaders)
                    ),
                row![
                    button("Import snippets").on_press(Message::Import),
                    button("Export snippets").on_press_maybe(
                        (!self.library.entries().is_empty()).then_some(Message::Export)
                    ),
                ]
                .spacing(6),
                button("OpenPGP recipients").on_press(Message::ShowRecipients),
                row![
                    button("Compare snippets…").on_press_maybe(
                        self.comparison_pair().is_some().then_some(Message::Compare)
                    ),
                    button("Move to folder…").on_press_maybe(
                        move_snippet.map(|id| Message::Folders(folders::Message::MoveSnippets(id)))
                    ),
                ]
                .spacing(6)
                .wrap(),
            ]
            .spacing(8)
            .width(290)
            .height(Length::Fill)
            .into()
        });
        let mut tab_strip =
            self.tabs
                .labels(&self.draft)
                .into_iter()
                .fold(row![].spacing(4), |tabs, tab| {
                    let name = if tab.name.is_empty() {
                        "Untitled"
                    } else {
                        tab.name
                    };
                    let label = format!(
                        "{}{}{}",
                        if tab.preview { "Preview · " } else { "" },
                        name,
                        if tab.dirty { " *" } else { "" }
                    );
                    let trigger: Element<'_, Message> = row![
                        button(text(label).size(13))
                            .style(if tab.active && analysis.active.is_none() {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .on_press(Message::SelectTab(tab.id)),
                        button("×")
                            .style(button::text)
                            .on_press(Message::CloseTab(tab.id)),
                    ]
                    .spacing(2)
                    .into();
                    tabs.push(if tab.preview {
                        trigger
                    } else {
                        self.tab_context_view(tab.id, trigger)
                    })
                });
        for (subject, name) in analysis.tabs {
            let selected = analysis.active == Some(subject);
            tab_strip = tab_strip.push(
                row![
                    button(text(format!("Analysis · {name}")).size(13))
                        .style(if selected {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .on_press(Message::Analysis(analysis::Message::Tab(subject))),
                    selected.then(|| button("×")
                        .style(button::text)
                        .on_press(Message::Analysis(analysis::Message::Close))),
                ]
                .spacing(2),
            );
        }
        let tab_strip = scrollable(tab_strip).direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default(),
        ));
        if let Some(page) = analysis.page {
            let pane = column![tab_strip, page]
                .width(Length::Fill)
                .height(Length::Fill)
                .spacing(8);
            return row![library, pane]
                .padding(12)
                .spacing(12)
                .height(Length::Fill)
                .into();
        }
        let preview: Element<'_, Message> = if self.tabs.is_preview() {
            let label = if open_elsewhere {
                "This snippet is being edited in another window."
            } else {
                "Preview. Start typing or choose Edit to keep this tab open."
            };
            let mut notice = row![text(label).size(13).width(Length::Fill)]
                .spacing(8)
                .align_y(Alignment::Center);
            if let Some(id) = self.draft.id {
                notice = notice.push(
                    button(if open_elsewhere {
                        "Go to editor"
                    } else {
                        "Edit"
                    })
                    .on_press(Message::Open(id)),
                );
            }
            notice.into()
        } else {
            row![].into()
        };
        let is_lua = self.draft.is_lua();
        let code = move || {
            caret::view(
                text_editor(&self.draft.content)
                    .id(self.editor_id.clone())
                    .height(Length::Fill)
                    .placeholder("Snippet text")
                    .on_action(Message::Edit)
                    .key_binding(move |event| {
                        if is_lua && lua::completion_key(&event) {
                            return Some(text_editor::Binding::Custom(Message::CompleteLua));
                        }
                        editor_key_binding(event)
                    })
                    .highlight_with::<lua::Lua>(
                        lua::Settings::new(is_lua, appearance, self.draft.syntax_source),
                        lua::format,
                    ),
                content_theme.clone(),
                appearance,
                appearance.size,
                caret_preferences,
            )
        };
        let structure_available = !self.tabs.is_preview();
        let structure_visible = structure_available && self.structure_preferences().visible;
        let tool_context = self.tool_context();
        let can_complete_lua = is_lua && !self.tabs.is_empty();
        let mut source_tools = row![];
        if can_complete_lua {
            source_tools =
                source_tools.push(button("Complete Lua…").on_press(Message::CompleteLua));
        }
        for button in self.tools.actions(tool_context) {
            source_tools = source_tools.push(Element::new(button).map(Message::Tools));
        }
        let editor = column![
            self.recovery_banner(),
            tab_strip,
            preview,
            row![
                text_input("Name", &self.draft.name).on_input(Message::Name),
                text_input("Language", &self.draft.language).on_input(Message::Language),
                text_input("Category", &self.draft.category).on_input(Message::Category)
            ]
            .spacing(6),
            self.custom_header_save_blocked(None)
                .then(
                    || text("Managed policy blocks creating new Script-Header snippets.").size(13)
                ),
            (can_complete_lua || tool_context.is_some()).then(|| source_tools.spacing(6).wrap()),
            self.tools.view().map(|panel| panel.map(Message::Tools)),
            self.draft
                .lua_completion
                .as_ref()
                .filter(|_| is_lua)
                .map(|completion| { completion.view(Message::ChooseLua, Message::DismissLua) }),
            if structure_visible {
                self.structure_view(code)
            } else {
                code()
            },
            row![
                button("Save").on_press_maybe(
                    (!self.custom_header_save_blocked(self.draft.id)).then_some(Message::Save)
                ),
                button("Details").on_press(Message::ShowDetails),
                button(if structure_visible {
                    "Hide structure"
                } else {
                    "Structure"
                })
                .on_press_maybe(
                    structure_available.then_some(Message::Structure(structure::Message::Toggle))
                ),
                button("Save as new").on_press_maybe(
                    (self.draft.id.is_some() && !self.custom_header_save_blocked(None))
                        .then_some(Message::SaveAsNew)
                ),
                button("Delete").on_press_maybe(
                    (self.draft.id.is_some() && !self.draft.dirty).then_some(Message::Delete)
                ),
                button("Copy resolved").on_press_maybe(
                    (self.draft.id.is_some() && !self.draft.dirty)
                        .then_some(Message::Copy(copy::Message::Begin))
                ),
                button("Copy compact command").on_press_maybe(
                    (snippets::one_liner::supported(&self.draft.language)
                        || self.can_open_assist())
                    .then_some(Message::CopyCommand(snippets::one_liner::Mode::Compact))
                ),
                button("Copy embedded command").on_press_maybe(
                    snippets::one_liner::supported(&self.draft.language)
                        .then_some(Message::CopyCommand(snippets::one_liner::Mode::Embedded))
                ),
                button("Open diagram").on_press_maybe(
                    (self.draft.id.is_some() || self.draft.content_changed)
                        .then_some(Message::OpenDiagram)
                ),
                button("Saved diagrams…").on_press_maybe(
                    self.draft
                        .id
                        .map(|_| Message::DiagramLibrary(diagrams::Message::Toggle))
                ),
                button("Full-code analysis…").on_press_maybe(
                    self.draft
                        .id
                        .map(|id| Message::Analyze(AnalysisSubject::Snippet(id)))
                ),
                button("AI assist…").on_press_maybe(
                    self.can_open_assist()
                        .then_some(Message::Assist(ai_assist::Message::Open))
                ),
                button("Close").on_press(Message::Close)
            ]
            .spacing(6)
            .wrap(),
            self.diagram_library(),
            text(self.status.as_deref().unwrap_or(if self.draft.dirty {
                "Unsaved edits"
            } else {
                ""
            })),
        ]
        .spacing(8)
        .width(Length::Fill)
        .height(Length::Fill);
        let editor: Element<'_, Message> = if self.tabs.is_empty() {
            widget::container(
                column![
                    self.recovery_banner(),
                    text("Select a snippet to preview or create a new one."),
                    text(self.status.as_deref().unwrap_or_default()),
                ]
                .spacing(8),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center(Length::Fill)
            .into()
        } else {
            editor.into()
        };
        row![library, editor]
            .padding(12)
            .spacing(12)
            .height(Length::Fill)
            .into()
    }

    fn close_prompt(
        &self,
        cancel: Message,
        discard: Message,
        save: Message,
    ) -> Element<'_, Message> {
        column![
            text("Save all changes before closing the Snippet Manager?").size(19),
            text(self.status.as_deref().unwrap_or_default()),
            row![
                button("Keep editing").on_press(cancel),
                button("Discard all").on_press(discard),
                button("Save all").on_press_maybe(self.can_save_before_close().then_some(save)),
            ]
            .spacing(8),
        ]
        .padding(16)
        .spacing(12)
        .into()
    }
}

fn editor_key_binding(event: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    if event.modifiers.control()
        && !event.modifiers.alt()
        && event.key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab)
    {
        return Some(text_editor::Binding::Custom(Message::CycleTab(
            event.modifiers.shift(),
        )));
    }
    if matches!(event.status, text_editor::Status::Focused { .. })
        && event.modifiers.command()
        && !event.modifiers.shift()
        && !event.modifiers.alt()
    {
        match event.key.to_latin(event.physical_key) {
            Some('s') => return Some(text_editor::Binding::Custom(Message::Save)),
            Some('w') => return Some(text_editor::Binding::Custom(Message::CloseActiveTab)),
            _ => {}
        }
    }
    text_editor::Binding::from_key_press(event)
}

#[cfg(test)]
mod tests;
