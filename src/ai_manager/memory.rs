//! The AI Manager's editor for local global and per-connection memory.

use std::{
    collections::HashSet,
    fmt, io,
    ops::{Range, RangeInclusive},
    path::PathBuf,
    sync::Arc,
};

use iced::{
    Alignment, Element, Length, Task,
    widget::{
        button, checkbox, column, container, pick_list, progress_bar, row, rule, scrollable, text,
        text_editor, text_input,
    },
};
use serde::{Deserialize, Serialize};

use crate::clipboard_policy::{AiMemoryLearningMode, AiMemoryPolicy, UserMemoryMode};
use crate::local_ai;
use crate::vault_window::VaultWindow;

use super::profiles::picker::DismissOnEscape as _;
use super::{Configuration, document_find};

mod cleanup;
mod comparison;
mod format;
pub(super) mod learning;
pub(super) mod prompt;
mod references;
pub(super) mod store;

use comparison::Comparison;
pub(crate) use format::checked_reference_text;
use store::{ArchiveItem, Snapshot};

const MIN_TOKENS: u16 = 500;
const MAX_TOKENS: u16 = 4_000;
const DEFAULT_TOKENS: u16 = 1_500;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) enabled: bool,
    pub(crate) learning: Learning,
    pub(crate) max_tokens: u16,
}

impl Settings {
    pub(super) fn budget_with_limit(&self, limit: Option<u16>) -> u16 {
        self.max_tokens
            .clamp(MIN_TOKENS, MAX_TOKENS)
            .min(limit.unwrap_or(MAX_TOKENS))
    }

    pub(super) fn effective_learning(&self, policy: &AiMemoryPolicy) -> Learning {
        match policy.learning {
            Some(AiMemoryLearningMode::Propose) => Learning::Propose,
            Some(AiMemoryLearningMode::Off) => Learning::Off,
            None => self.learning,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            learning: Learning::Propose,
            max_tokens: DEFAULT_TOKENS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum Learning {
    Propose,
    Off,
}

impl fmt::Display for Learning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Propose => "Propose new rules",
            Self::Off => "Off",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Scope {
    id: Option<u64>,
    label: String,
}

impl Scope {
    pub(super) fn global() -> Self {
        Self {
            id: None,
            label: "Global".into(),
        }
    }

    pub(crate) fn connection(id: u64, label: String) -> Self {
        Self {
            id: Some(id),
            label,
        }
    }

    fn name(&self) -> String {
        self.id
            .map_or_else(|| "MEMORY.md".into(), |id| format!("{id}.md"))
    }

    fn key(&self) -> String {
        self.id
            .map_or_else(|| "global".into(), |id| format!("connection-{id}"))
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Clone)]
pub(crate) enum Message {
    ToggleOrganizationRule(usize),
    Enable(bool),
    Learning(Learning),
    Tokens(String),
    Scope(Scope),
    Reload,
    Entries,
    Markdown,
    MarkdownAction(text_editor::Action),
    Find(document_find::Message),
    SaveMarkdown,
    EditRow(usize),
    RowText(text_editor::Action),
    SaveRow,
    DeleteRow(usize),
    ArchiveRow(usize),
    ShowCleanup,
    ReviewCleanup(cleanup::Kind),
    CleanupFinished(u64, Result<cleanup::Proposal, String>),
    SelectArchiveCandidate(usize, bool),
    ReviewArchiveCandidates,
    ShowArchive,
    RestoreArchive(usize),
    DeleteArchive(usize),
    ShowHistory,
    ViewHistory(String),
    RestoreHistory,
    ShowReferences,
    ReferencePath(String),
    ReviewReference,
    ReviewExisting(String),
    ReadReference(text_editor::Action),
    ApproveReference,
    SetReferenceActive(String, bool),
    RemoveReference(String),
    Clear,
    Back,
    Confirm,
    Cancel,
    Completed(Result<Outcome, String>),
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Markdown and editor actions can contain credentials before validation.
        f.write_str("AI Memory")
    }
}

#[derive(Clone)]
pub(crate) enum Outcome {
    Document(Snapshot, &'static str),
    History(Vec<String>),
    Version(String, String),
    Archive(Snapshot, Vec<ArchiveItem>),
    References(references::Registry, &'static str),
    ReferencePreview(references::Review),
}

enum Decision {
    Scope(Scope),
    Reload,
    Clear(Comparison),
    DeleteRow(usize),
    ArchiveRow(usize),
    ArchiveCandidates(Vec<format::Entry>, Comparison),
    CleanupRequest(Box<cleanup::Review>),
    CleanupProposal(cleanup::Proposal, Comparison),
    RestoreArchive(usize),
    DeleteArchive(usize),
    RestoreHistory,
    RemoveReference(String),
}

enum Page {
    Entries,
    Markdown,
    Archive(Snapshot, Vec<ArchiveItem>),
    Cleanup(Vec<ArchiveCandidate>),
    History(Vec<String>, Option<Box<HistoryVersion>>),
    References(references::Registry),
}

struct ArchiveCandidate {
    entry: format::Entry,
    selected: bool,
}

struct HistoryVersion {
    name: String,
    value: String,
    comparison: Comparison,
}

struct EntryRow {
    range: Range<usize>,
    managed: Option<format::Entry>,
}

pub(crate) struct State {
    directory: PathBuf,
    scopes: Vec<Scope>,
    scope: Scope,
    editable: bool,
    policy: AiMemoryPolicy,
    organization_open: HashSet<usize>,
    settings: Settings,
    token_input: String,
    initial_settings: Settings,
    base: Option<Snapshot>,
    editor: text_editor::Content,
    find: document_find::Find,
    row_edit: Option<(usize, text_editor::Content)>,
    reference_path: String,
    reference_preview: Option<(references::Review, text_editor::Content)>,
    page: Page,
    decision: Option<Decision>,
    pending: bool,
    pending_cleanup: Option<(u64, local_ai::Cancel)>,
    cleanup_epoch: u64,
    notice: Option<String>,
}

impl State {
    pub(crate) fn new(
        mut settings: Settings,
        directory: PathBuf,
        mut scopes: Vec<Scope>,
        editable: bool,
    ) -> Self {
        settings.max_tokens = settings.max_tokens.clamp(MIN_TOKENS, MAX_TOKENS);
        scopes.sort_by_cached_key(|scope| scope.label.to_lowercase());
        scopes.dedup_by_key(|scope| scope.id);
        let scope = Scope::global();
        scopes.insert(0, scope.clone());
        Self {
            directory,
            scopes,
            scope,
            editable,
            policy: AiMemoryPolicy::default(),
            organization_open: HashSet::new(),
            token_input: settings.max_tokens.to_string(),
            initial_settings: settings.clone(),
            settings,
            base: None,
            editor: text_editor::Content::new(),
            find: document_find::Find::default(),
            row_edit: None,
            reference_path: String::new(),
            reference_preview: None,
            page: Page::Entries,
            decision: None,
            pending: false,
            pending_cleanup: None,
            cleanup_epoch: 0,
            notice: None,
        }
    }

    pub(super) fn set_policy(&mut self, policy: &AiMemoryPolicy, ai_allowed: bool) {
        let mut effective = policy.clone();
        effective.feature_allowed &= ai_allowed;
        if self.policy != effective {
            self.reference_preview = None;
            self.organization_open.clear();
            self.invalidate_cleanup();
        }
        self.policy = effective;
        if !self.policy.external_files_allowed() {
            self.reference_path.clear();
        }
        self.token_input = self.effective_budget().to_string();
        if !self.can_write_files() {
            if self.markdown_dirty()
                && let Some(base) = &self.base
            {
                self.editor = text_editor::Content::with_text(&base.text);
                self.find.reset();
            }
            self.row_edit = None;
            self.decision = None;
            if matches!(self.page, Page::Cleanup(_)) {
                self.page = Page::Entries;
            }
        }
        if !self.can_read_files() {
            self.base = None;
            self.editor = text_editor::Content::new();
            self.find.reset();
            self.reference_path.clear();
            self.reference_preview = None;
            self.page = Page::Entries;
            self.notice = None;
        }
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn mark_saved(&mut self) {
        self.initial_settings = self.settings.clone();
    }

    pub(crate) fn settings_dirty(&self) -> bool {
        self.initial_settings != self.settings || !self.settings_valid()
    }

    pub(crate) fn has_unsaved_document(&self) -> bool {
        self.markdown_dirty() || self.row_edit.is_some()
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.settings_dirty() || self.has_unsaved_document()
    }

    pub(crate) fn settings_valid(&self) -> bool {
        !self.can_read_files()
            || self
                .token_input
                .parse::<u16>()
                .is_ok_and(|tokens| self.budget_range().contains(&tokens))
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending
    }

    pub(super) fn reviewing_document(&self) -> bool {
        matches!(
            self.decision,
            Some(
                Decision::Clear(_)
                    | Decision::ArchiveCandidates(..)
                    | Decision::CleanupRequest(_)
                    | Decision::CleanupProposal(..)
                    | Decision::RestoreHistory
            )
        )
    }

    pub(super) fn find_open(&self) -> bool {
        self.can_find_markdown() && self.find.is_open()
    }

    pub(super) fn cleanup_open(&self) -> bool {
        self.pending_cleanup.is_some()
            || matches!(
                self.decision,
                Some(Decision::CleanupRequest(_) | Decision::CleanupProposal(..))
            )
    }

    pub(crate) fn load(&mut self) -> Task<Message> {
        if !self.can_read_files()
            || self.pending
            || self.decision.is_some()
            || self.has_unsaved_document()
        {
            return Task::none();
        }
        self.run(|path, scope| {
            store::load(&path, &scope).map(|file| Outcome::Document(file, "Memory loaded."))
        })
    }

    fn markdown_dirty(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.text != self.editor.text())
    }

    fn run(
        &mut self,
        work: impl FnOnce(PathBuf, Scope) -> io::Result<Outcome> + Send + 'static,
    ) -> Task<Message> {
        let path = self.directory.clone();
        let scope = self.scope.clone();
        self.pending = true;
        self.notice = None;
        Task::perform(
            async move { work(path, scope).map_err(|error| error.to_string()) },
            Message::Completed,
        )
    }

    #[cfg(test)]
    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        self.update_with_cleanup(message, None, None)
    }

    pub(super) fn update_with_cleanup(
        &mut self,
        message: Message,
        config: Option<&Arc<Configuration>>,
        vault: Option<&VaultWindow>,
    ) -> Task<Message> {
        if let Message::ToggleOrganizationRule(index) = message {
            if index < self.policy.organization().len() && !self.organization_open.remove(&index) {
                self.organization_open.insert(index);
            }
            return Task::none();
        }
        if let Message::CleanupFinished(epoch, result) = message {
            if self
                .pending_cleanup
                .as_ref()
                .is_none_or(|(current, _)| *current != epoch)
            {
                return Task::none();
            }
            self.cancel_cleanup();
            if self.can_edit_rows() {
                match result {
                    Ok(proposal) => {
                        if let Some(base) = self.base.as_ref()
                            && proposal.current(&self.directory, &self.scope, base)
                        {
                            let comparison = Comparison::new(&base.text, &proposal.text);
                            self.decision = Some(Decision::CleanupProposal(proposal, comparison));
                        }
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            return Task::none();
        }
        if matches!(message, Message::Cancel) && self.pending_cleanup.is_some() {
            self.cancel_cleanup();
            self.notice = Some("AI memory request cancelled.".into());
            return Task::none();
        }
        if let Message::Completed(result) = message {
            self.pending = false;
            if !self.can_read_files() {
                return Task::none();
            }
            match result {
                Ok(Outcome::Document(file, notice)) => {
                    self.editor = text_editor::Content::with_text(&file.text);
                    self.find.reset();
                    self.base = Some(file);
                    self.row_edit = None;
                    self.notice = Some(notice.into());
                    if !matches!(self.page, Page::Markdown) {
                        self.page = Page::Entries;
                    }
                }
                Ok(Outcome::History(names)) => self.page = Page::History(names, None),
                Ok(Outcome::Version(name, value)) => {
                    if let Page::History(_, version) = &mut self.page
                        && let Some(base) = &self.base
                    {
                        *version = Some(Box::new(HistoryVersion {
                            name,
                            comparison: Comparison::new(&base.text, &value),
                            value,
                        }));
                    }
                }
                Ok(Outcome::Archive(file, rows)) => self.page = Page::Archive(file, rows),
                Ok(Outcome::References(registry, notice)) => {
                    self.page = Page::References(registry);
                    self.reference_preview = None;
                    self.notice = (!notice.is_empty()).then(|| notice.into());
                }
                Ok(Outcome::ReferencePreview(preview)) => {
                    if self.can_read_references() {
                        let content = text_editor::Content::with_text(&preview.text);
                        self.reference_preview = Some((preview, content));
                    }
                }
                Err(error) => self.notice = Some(error),
            }
            return Task::none();
        }
        if self.pending || !self.can_read_files() {
            return Task::none();
        }
        if self.decision.is_some() {
            return match message {
                Message::Confirm => self.confirm(),
                Message::Cancel => {
                    self.decision = None;
                    Task::none()
                }
                _ => Task::none(),
            };
        }
        match message {
            Message::Enable(enabled) if self.editable => self.settings.enabled = enabled,
            Message::Learning(learning) if self.editable && self.policy.learning.is_none() => {
                self.settings.learning = learning;
            }
            Message::Tokens(tokens) if self.editable && self.can_adjust_budget() => {
                self.token_input = tokens.chars().take(5).collect();
                if self.settings_valid()
                    && let Ok(value) = self.token_input.parse::<u16>()
                {
                    self.settings.max_tokens = value;
                }
            }
            Message::Scope(scope) if self.scope != scope && self.scopes.contains(&scope) => {
                return self.request(Decision::Scope(scope), self.has_unsaved_document());
            }
            Message::Reload => return self.request(Decision::Reload, self.has_unsaved_document()),
            Message::Entries => self.page = Page::Entries,
            Message::Markdown if self.row_edit.is_none() => self.page = Page::Markdown,
            Message::MarkdownAction(action)
                if self.can_find_markdown() && (self.can_write_files() || !action.is_edit()) =>
            {
                bounded_edit(&mut self.editor, action, store::MAX_BYTES);
            }
            Message::Find(message) if self.can_find_markdown() => {
                return self.find.update(&mut self.editor, message);
            }
            Message::SaveMarkdown if self.can_write_files() && self.markdown_dirty() => {
                return self.save(self.editor.text(), "Memory saved.");
            }
            Message::EditRow(index) if self.can_edit_rows() => {
                if let Some(base) = self.base.as_ref()
                    && let Some(row) = rows(&base.text).get(index)
                {
                    let value = row.managed.as_ref().map_or_else(
                        || base.text[row.range.clone()].trim_end_matches(['\r', '\n']),
                        |entry| entry.body.as_str(),
                    );
                    self.row_edit = Some((index, text_editor::Content::with_text(value)));
                }
            }
            Message::RowText(action) if self.can_write_files() => {
                if let Some((_, editor)) = &mut self.row_edit {
                    bounded_edit(editor, action, store::MAX_BYTES);
                }
            }
            Message::SaveRow if self.can_write_files() && !self.markdown_dirty() => {
                return self.save_row();
            }
            Message::DeleteRow(index) if self.can_edit_rows() => {
                self.decision = Some(Decision::DeleteRow(index));
            }
            Message::ArchiveRow(index) if self.can_edit_rows() => {
                self.decision = Some(Decision::ArchiveRow(index));
            }
            Message::ShowCleanup if self.can_edit_rows() => {
                let today = jiff::Zoned::now().date();
                let candidates = self
                    .base
                    .as_ref()
                    .into_iter()
                    .flat_map(|base| format::archive_candidates(&base.text, today))
                    .map(|entry| ArchiveCandidate {
                        entry,
                        selected: true,
                    })
                    .collect();
                self.page = Page::Cleanup(candidates);
                self.notice = None;
            }
            Message::ReviewCleanup(kind)
                if self.can_edit_rows()
                    && prompt::environment_enabled()
                    && matches!(self.page, Page::Cleanup(_)) =>
            {
                if let (Some(config), Some(base)) = (config, self.base.as_ref()) {
                    match cleanup::Review::prepare(
                        kind,
                        Arc::clone(config),
                        vault,
                        &self.directory,
                        &self.scope,
                        base,
                    ) {
                        Ok(review) => {
                            self.decision = Some(Decision::CleanupRequest(Box::new(review)))
                        }
                        Err(error) => self.notice = Some(error),
                    }
                }
            }
            Message::SelectArchiveCandidate(index, selected) if self.can_edit_rows() => {
                if let Page::Cleanup(candidates) = &mut self.page
                    && let Some(candidate) = candidates.get_mut(index)
                {
                    candidate.selected = selected;
                }
            }
            Message::ReviewArchiveCandidates if self.can_edit_rows() => {
                if let Page::Cleanup(candidates) = &self.page
                    && let Some(base) = &self.base
                {
                    let selected: Vec<_> = candidates
                        .iter()
                        .filter(|candidate| candidate.selected)
                        .map(|candidate| candidate.entry.clone())
                        .collect();
                    if !selected.is_empty() {
                        let next = store::without_entries(&base.text, &selected);
                        let review = Comparison::new(&base.text, &next);
                        self.decision = Some(Decision::ArchiveCandidates(selected, review));
                    }
                }
            }
            Message::ShowArchive if !self.has_unsaved_document() => {
                return self.run(|path, scope| {
                    store::archive(&path, &scope)
                        .map(|(snapshot, rows)| Outcome::Archive(snapshot, rows))
                });
            }
            Message::RestoreArchive(index) if self.can_edit_rows() => {
                self.decision = Some(Decision::RestoreArchive(index));
            }
            Message::DeleteArchive(index) if self.can_edit_rows() => {
                self.decision = Some(Decision::DeleteArchive(index));
            }
            Message::ShowHistory if !self.has_unsaved_document() => {
                return self.run(|path, scope| store::history(&path, &scope).map(Outcome::History));
            }
            Message::ViewHistory(name) if matches!(&self.page, Page::History(versions, _) if versions.contains(&name)) =>
            {
                return self.run(move |path, scope| {
                    store::history_text(&path, &scope, &name)
                        .map(|value| Outcome::Version(name, value))
                });
            }
            Message::RestoreHistory if self.can_restore_history() => {
                self.decision = Some(Decision::RestoreHistory);
            }
            Message::ShowReferences if !self.has_unsaved_document() => {
                return self.run(|path, _| {
                    references::registry(&path).map(|registry| Outcome::References(registry, ""))
                });
            }
            Message::ReferencePath(value) if self.can_edit_references() => {
                self.reference_path = value.chars().take(4_096).collect();
                self.reference_preview = None;
            }
            Message::ReviewReference if self.can_edit_references() => {
                return self.review_reference(self.reference_path.clone());
            }
            Message::ReviewExisting(path) if self.can_review_existing_reference(&path) => {
                self.reference_path.clone_from(&path);
                return self.review_reference(path);
            }
            Message::ReadReference(action) if self.can_read_references() && !action.is_edit() => {
                if let Some((_, editor)) = &mut self.reference_preview {
                    editor.perform(action);
                }
            }
            Message::ApproveReference if self.can_edit_references() => {
                if let Page::References(registry) = &self.page
                    && let Some((preview, _)) = self.reference_preview.as_ref()
                {
                    let registry = registry.clone();
                    let preview = preview.clone();
                    let policy = self.policy.clone();
                    return self.run(move |path, scope| {
                        references::approve(&path, &scope, registry, preview, &policy).map(
                            |registry| {
                                Outcome::References(registry, "Reference approved for this scope.")
                            },
                        )
                    });
                }
            }
            Message::SetReferenceActive(id, active) if self.can_edit_references() => {
                return self.change_reference(id, Some(active));
            }
            Message::RemoveReference(id) if self.can_edit_references() => {
                self.decision = Some(Decision::RemoveReference(id));
            }
            Message::Clear if self.can_edit_rows() => {
                if let Some(base) = &self.base
                    && !base.text.is_empty()
                {
                    self.decision = Some(Decision::Clear(Comparison::new(&base.text, "")));
                }
            }
            Message::Back => {
                self.page = Page::Entries;
                self.reference_preview = None;
            }
            Message::Cancel => self.row_edit = None,
            _ => {}
        }
        Task::none()
    }

    fn request(&mut self, decision: Decision, confirm: bool) -> Task<Message> {
        self.decision = Some(decision);
        if confirm {
            Task::none()
        } else {
            self.confirm()
        }
    }

    fn confirm(&mut self) -> Task<Message> {
        match self.decision.take() {
            Some(Decision::Scope(scope)) => {
                self.scope = scope;
                self.base = None;
                self.find.reset();
                self.row_edit = None;
                self.reference_preview = None;
                self.page = Page::Entries;
                self.load()
            }
            Some(Decision::Reload) => {
                self.row_edit = None;
                self.find.reset();
                if let Some(base) = self.base.as_ref() {
                    self.editor = text_editor::Content::with_text(&base.text);
                }
                self.load()
            }
            Some(Decision::Clear(_)) if self.can_edit_rows() => {
                self.save(String::new(), "Memory cleared.")
            }
            Some(Decision::DeleteRow(index)) if self.can_edit_rows() => {
                if let Some(base) = self.base.as_ref()
                    && let Some(row) = rows(&base.text).get(index)
                {
                    let mut next = base.text.clone();
                    next.replace_range(row.range.clone(), "");
                    return self.save(next, "Entry deleted.");
                }
                Task::none()
            }
            Some(Decision::ArchiveRow(index)) if self.can_edit_rows() => {
                if let Some(base) = self.base.clone()
                    && let Some(row) = rows(&base.text).into_iter().nth(index)
                    && let Some(entry) = row.managed
                {
                    return self.archive_entries(base, vec![entry], "Entry archived.");
                }
                Task::none()
            }
            Some(Decision::ArchiveCandidates(selected, _)) if self.can_edit_rows() => {
                if let Some(base) = self.base.clone() {
                    return self.archive_entries(base, selected, "Selected entries archived.");
                }
                Task::none()
            }
            Some(Decision::CleanupRequest(review)) if self.can_edit_rows() => {
                self.start_cleanup(*review)
            }
            Some(Decision::CleanupProposal(proposal, _)) if self.can_edit_rows() => {
                self.run(move |_, _| {
                    proposal
                        .apply()
                        .map(|file| Outcome::Document(file, "Memory saved."))
                })
            }
            Some(Decision::RestoreArchive(index)) if self.can_edit_rows() => {
                self.change_archive(index, true)
            }
            Some(Decision::DeleteArchive(index)) if self.can_edit_rows() => {
                self.change_archive(index, false)
            }
            Some(Decision::RestoreHistory) if self.can_restore_history() => {
                if let Page::History(_, Some(version)) = &self.page {
                    return self.save(version.value.clone(), "History version restored.");
                }
                Task::none()
            }
            Some(Decision::RemoveReference(id)) if self.can_edit_references() => {
                self.change_reference(id, None)
            }
            _ => Task::none(),
        }
    }

    fn can_read_files(&self) -> bool {
        self.policy.user_files_allowed()
    }

    fn start_cleanup(&mut self, review: cleanup::Review) -> Task<Message> {
        match review.start() {
            Ok(local_ai::Pending { reply, cancel }) => {
                self.cleanup_epoch = self.cleanup_epoch.wrapping_add(1);
                let epoch = self.cleanup_epoch;
                self.pending_cleanup = Some((epoch, cancel));
                self.pending = true;
                self.notice = None;
                Task::perform(
                    async move {
                        reply
                            .recv()
                            .await
                            .unwrap_or_else(|_| Err("The AI memory worker stopped.".into()))
                    },
                    move |result| Message::CleanupFinished(epoch, result),
                )
            }
            Err(error) => {
                self.notice = Some(error);
                Task::none()
            }
        }
    }

    fn cancel_cleanup(&mut self) {
        if self.pending_cleanup.take().is_some() {
            self.pending = false;
        }
    }

    pub(super) fn invalidate_cleanup(&mut self) {
        self.cancel_cleanup();
        if matches!(
            self.decision,
            Some(Decision::CleanupRequest(_) | Decision::CleanupProposal(..))
        ) {
            self.decision = None;
        }
    }

    fn can_write_files(&self) -> bool {
        self.editable
            && self.policy.feature_allowed
            && self.policy.user_memory == UserMemoryMode::Editable
    }

    fn effective_learning(&self) -> Learning {
        self.settings.effective_learning(&self.policy)
    }

    fn budget_range(&self) -> RangeInclusive<u16> {
        let max = self.policy.max_tokens.unwrap_or(MAX_TOKENS).min(MAX_TOKENS);
        MIN_TOKENS.min(max)..=max
    }

    fn can_adjust_budget(&self) -> bool {
        self.policy.max_tokens.is_none_or(|cap| cap >= MIN_TOKENS)
    }

    fn effective_budget(&self) -> u16 {
        self.settings.max_tokens.min(*self.budget_range().end())
    }

    fn can_edit_rows(&self) -> bool {
        self.can_write_files() && self.base.is_some() && !self.has_unsaved_document()
    }

    fn can_read_references(&self) -> bool {
        self.policy.external_files_allowed() && matches!(self.page, Page::References(_))
    }

    fn can_review_existing_reference(&self, path: &str) -> bool {
        self.can_read_references()
            && matches!(&self.page, Page::References(registry) if registry.entries.iter().any(|entry|
                entry.path == path && entry.applies_to(&self.scope)))
    }

    fn can_edit_references(&self) -> bool {
        self.can_write_files() && self.can_read_references()
    }

    fn can_restore_history(&self) -> bool {
        self.can_edit_rows()
            && matches!((&self.page, &self.base), (Page::History(_, Some(version)), Some(base))
                if version.value != base.text)
    }

    fn archive_entries(
        &mut self,
        base: Snapshot,
        entries: Vec<format::Entry>,
        notice: &'static str,
    ) -> Task<Message> {
        self.run(move |path, scope| {
            store::move_to_archive(&path, &scope, &base, &entries)
                .map(|snapshot| Outcome::Document(snapshot, notice))
        })
    }

    fn save(&mut self, text: String, notice: &'static str) -> Task<Message> {
        let Some(base) = self.base.clone() else {
            return Task::none();
        };
        self.run(move |path, scope| {
            store::save(&path, &scope, &base, &text).map(|file| Outcome::Document(file, notice))
        })
    }

    fn save_row(&mut self) -> Task<Message> {
        let Some((index, editor)) = self.row_edit.as_ref() else {
            return Task::none();
        };
        let Some(base) = self.base.as_ref() else {
            return Task::none();
        };
        let Some(row) = rows(&base.text).into_iter().nth(*index) else {
            return Task::none();
        };
        let value = editor.text();
        let replacement = if let Some(entry) = row.managed {
            let Some(fence) = entry.rewrite(&value) else {
                self.notice = Some("A managed entry needs 1–300 characters of text.".into());
                return Task::none();
            };
            fence
        } else {
            let original = &base.text[row.range.clone()];
            let trimmed = original.trim_end_matches(['\r', '\n']);
            format!("{value}{}", &original[trimmed.len()..])
        };
        let mut next = base.text.clone();
        next.replace_range(row.range, &replacement);
        self.save(next, "Entry saved.")
    }

    fn change_archive(&mut self, index: usize, restore: bool) -> Task<Message> {
        let Page::Archive(archive, items) = &self.page else {
            return Task::none();
        };
        let Some(base) = self.base.clone() else {
            return Task::none();
        };
        let Some(item) = items.get(index).cloned() else {
            return Task::none();
        };
        let archive = archive.clone();
        let budget = self.effective_budget();
        self.run(move |path, scope| {
            store::change_archive(&path, &scope, &base, &archive, &item, restore, budget).map(
                |file| {
                    Outcome::Document(
                        file,
                        if restore {
                            "Entry restored."
                        } else {
                            "Archived entry deleted."
                        },
                    )
                },
            )
        })
    }

    fn review_reference(&mut self, path: String) -> Task<Message> {
        self.reference_preview = None;
        let policy = self.policy.clone();
        self.run(move |_, _| {
            references::review(PathBuf::from(path), &policy).map(Outcome::ReferencePreview)
        })
    }

    fn change_reference(&mut self, id: String, active: Option<bool>) -> Task<Message> {
        let Page::References(registry) = &self.page else {
            return Task::none();
        };
        let registry = registry.clone();
        let policy = self.policy.clone();
        self.run(move |path, scope| {
            references::change(&path, &scope, registry, &id, active, &policy)
                .map(|registry| Outcome::References(registry, "Reference list saved."))
        })
    }

    pub(super) fn view(&self, cleanup_ai_available: bool) -> Element<'_, Message> {
        let environment_notice = (!prompt::environment_enabled()).then(|| {
            text("KORTTY_AI_MEMORY=off keeps personal memory out of AI requests for this session.")
                .size(13)
        });
        if !self.can_read_files() {
            let reason = if self.policy.feature_allowed {
                "User memory files are disabled by managed policy."
            } else {
                "AI memory is disabled by managed policy."
            };
            return column![
                text("Memory settings").size(19),
                text(reason),
                environment_notice,
                self.organization_rules(),
            ]
            .spacing(11)
            .into();
        }
        if let Some(decision) = &self.decision {
            match decision {
                Decision::Clear(comparison) => {
                    return review_view(
                        "Clear this memory file? Its previous version remains in History.",
                        "After clearing",
                        comparison,
                        "Clear memory",
                        true,
                    );
                }
                Decision::ArchiveCandidates(_, comparison) => {
                    return review_view(
                        "Move the selected managed entries to the archive?",
                        "After archiving",
                        comparison,
                        "Archive selected entries",
                        false,
                    );
                }
                Decision::CleanupRequest(review) => return cleanup_request_view(review),
                Decision::CleanupProposal(_, comparison) => {
                    return review_view(
                        "Review the proposed memory changes before saving.",
                        "After cleanup",
                        comparison,
                        "Apply memory changes",
                        false,
                    );
                }
                Decision::RestoreHistory => {
                    if let Page::History(_, Some(version)) = &self.page {
                        return review_view(
                            "Replace the current memory with this version?",
                            "Selected version",
                            &version.comparison,
                            "Restore version",
                            false,
                        );
                    }
                }
                _ => {}
            }
        }
        let can_change_settings = self.editable && !self.pending && self.decision.is_none();
        let enabled = checkbox(self.settings.enabled).label("Enable AI memory");
        let enabled = if can_change_settings {
            enabled.on_toggle(Message::Enable)
        } else {
            enabled
        };
        let learning: Element<'_, Message> =
            if can_change_settings && self.policy.learning.is_none() {
                pick_list(
                    [Learning::Propose, Learning::Off],
                    Some(self.settings.learning),
                    Message::Learning,
                )
                .dismiss_on_escape()
            } else {
                text(self.effective_learning().to_string()).into()
            };
        let bounds = self.budget_range();
        let budget_hint = format!("{}–{}", bounds.start(), bounds.end());
        let tokens = text_input(&budget_hint, &self.token_input)
            .on_input_maybe(
                (can_change_settings && self.can_adjust_budget()).then_some(Message::Tokens),
            )
            .width(115);
        let budget = f32::from(self.effective_budget());
        let token_label = format!(
            "Approx. {} / {:.0} tokens in this file",
            self.base
                .as_ref()
                .map_or(0, |file| format::estimate(&file.text)),
            budget
        );
        let estimated = self
            .base
            .as_ref()
            .map_or(0, |file| format::estimate(&file.text)) as f32;
        let selector = pick_list(self.scopes.as_slice(), Some(&self.scope), Message::Scope)
            .width(Length::Fill)
            .dismiss_on_escape();
        let can_edit = self.can_edit_rows() && !self.pending && self.decision.is_none();
        let mut body = column![
            environment_notice,
            self.organization_rules(),
            text("Memory settings").size(19),
            enabled,
            row![
                text("Learning").width(140),
                learning,
                self.policy
                    .learning
                    .map(|_| text("Managed by policy").size(13)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Token budget").width(140),
                tokens,
                self.policy
                    .max_tokens
                    .map(|cap| text(format!("Policy limit: {cap} tokens")).size(13)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            rule::horizontal(1),
            row![
                text("Scope").width(85),
                selector,
                button("Reload").on_press_maybe((!self.pending).then_some(Message::Reload)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                progress_bar(0.0..=budget, estimated.min(budget)),
                text(token_label).size(13)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                button("Entries")
                    .style(if matches!(self.page, Page::Entries) {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(Message::Entries),
                button("Markdown")
                    .style(if matches!(self.page, Page::Markdown) {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press_maybe(self.row_edit.is_none().then_some(Message::Markdown)),
                button("Archive…").on_press_maybe(
                    (!self.has_unsaved_document() && !self.pending).then_some(Message::ShowArchive)
                ),
                button("History…").on_press_maybe(
                    (!self.has_unsaved_document() && !self.pending).then_some(Message::ShowHistory)
                ),
                button("Clean up…").on_press_maybe(can_edit.then_some(Message::ShowCleanup)),
                button("Clear…").on_press_maybe(
                    (can_edit && self.base.as_ref().is_some_and(|base| !base.text.is_empty()))
                        .then_some(Message::Clear)
                ),
            ]
            .spacing(6)
            .wrap(),
            self.current_page(can_edit, cleanup_ai_available),
            button("External memory files…").on_press_maybe(
                (!self.pending
                    && !self.has_unsaved_document()
                    && !matches!(self.page, Page::References(_)))
                .then_some(Message::ShowReferences)
            ),
        ]
        .spacing(11);
        if !self.can_write_files() {
            let reason = if self.policy.user_memory == UserMemoryMode::ReadOnly {
                "Memory files are read-only under managed policy."
            } else {
                "Memory files are read-only."
            };
            body = body.push(text(reason));
        }
        if !self.settings_valid() {
            body = body.push(text(format!(
                "Enter a memory budget from {} to {} tokens.",
                bounds.start(),
                bounds.end()
            )));
        }
        if self.pending {
            body = body.push(text("Working…"));
            if self.pending_cleanup.is_some() {
                body = body.push(button("Cancel AI request").on_press(Message::Cancel));
            }
        }
        if let Some(notice) = self.notice.as_deref() {
            body = body.push(text(notice));
        }
        if let Some(action) = &self.decision {
            let prompt = match action {
                Decision::Scope(_) | Decision::Reload => "Discard the unsaved memory edit?",
                Decision::DeleteRow(_) => "Delete this entry from the memory file?",
                Decision::ArchiveRow(_) => "Move this managed entry to the archive?",
                Decision::RestoreArchive(_) => "Return this entry to the memory file?",
                Decision::DeleteArchive(_) => "Permanently delete this archived entry?",
                Decision::RemoveReference(_) => "Remove this reference approval from the list?",
                Decision::Clear(_)
                | Decision::ArchiveCandidates(..)
                | Decision::CleanupRequest(_)
                | Decision::CleanupProposal(..)
                | Decision::RestoreHistory => {
                    return body.into();
                }
            };
            body = body.push(
                container(
                    column![
                        text(prompt),
                        row![
                            button("Cancel").on_press(Message::Cancel),
                            button("Confirm").on_press(Message::Confirm)
                        ]
                        .spacing(8)
                    ]
                    .spacing(8),
                )
                .padding(8)
                .style(container::bordered_box),
            );
        }
        body.into()
    }

    fn organization_rules(&self) -> Option<Element<'_, Message>> {
        let rules = self.policy.organization();
        if rules.is_empty() {
            return None;
        }
        let explanation = if self.policy.feature_allowed {
            "Your organization sets these read-only rules for Terminal AI requests."
        } else {
            "Managed policy blocks the use of these organization rules in AI requests."
        };
        let mut content = column![
            text("From your organization").size(17),
            text(explanation).size(13),
        ]
        .spacing(6);
        for (index, rule) in rules.iter().enumerate() {
            let expanded = self.organization_open.contains(&index);
            let title = rule.title.trim();
            let title = if title.is_empty() {
                "Organization rule"
            } else {
                title
            };
            content = content.push(
                button(
                    row![
                        text(if expanded { "▾" } else { "▸" }),
                        text(title)
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(6),
                )
                .style(button::secondary)
                .width(Length::Fill)
                .on_press(Message::ToggleOrganizationRule(index)),
            );
            if expanded {
                content = content.push(
                    container(
                        text(rule.content.as_str())
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    )
                    .padding([4, 8]),
                );
            }
        }
        Some(
            container(content)
                .width(Length::Fill)
                .padding(10)
                .style(container::bordered_box)
                .into(),
        )
    }

    fn can_find_markdown(&self) -> bool {
        self.can_read_files()
            && matches!(self.page, Page::Markdown)
            && !self.pending
            && self.base.is_some()
            && self.decision.is_none()
    }

    fn current_page(&self, can_edit: bool, cleanup_ai_available: bool) -> Element<'_, Message> {
        match &self.page {
            Page::Entries => self.entries_page(can_edit),
            Page::Markdown => {
                let editor = text_editor(&self.editor)
                    .height(230)
                    .placeholder("Enter Markdown for this scope");
                let can_find = self.can_find_markdown();
                let editor = if can_find {
                    self.find
                        .bind(editor.on_action(Message::MarkdownAction), Message::Find)
                } else {
                    editor
                };
                column![
                    can_find
                        .then(|| button("Find…")
                            .on_press(Message::Find(document_find::Message::Open))),
                    if can_find {
                        self.find.view(Message::Find)
                    } else {
                        None
                    },
                    editor,
                    button("Save Markdown").on_press_maybe(
                        (self.can_write_files() && !self.pending && self.markdown_dirty())
                            .then_some(Message::SaveMarkdown)
                    ),
                ]
                .spacing(8)
                .into()
            }
            Page::Archive(_, items) => {
                let mut list = column![text("Archived entries for this scope").size(17)].spacing(8);
                if items.is_empty() {
                    list = list.push(text("The archive is empty for this scope."));
                }
                for (index, item) in items.iter().enumerate() {
                    list = list.push(
                        row![
                            text(&item.entry.body).width(Length::Fill),
                            button("Restore…")
                                .on_press_maybe(can_edit.then_some(Message::RestoreArchive(index))),
                            button("Delete…")
                                .on_press_maybe(can_edit.then_some(Message::DeleteArchive(index))),
                        ]
                        .spacing(8),
                    );
                }
                list.push(button("Back").on_press(Message::Back)).into()
            }
            Page::Cleanup(candidates) => {
                let can_ask = can_edit && cleanup_ai_available && prompt::environment_enabled();
                let mut content = column![
                    row![
                        button("Merge managed entries…").on_press_maybe(
                            can_ask.then_some(Message::ReviewCleanup(cleanup::Kind::Merge)),
                        ),
                        button("Shorten my paragraphs…").on_press_maybe(
                            can_ask.then_some(Message::ReviewCleanup(cleanup::Kind::Shorten)),
                        ),
                    ]
                    .spacing(8)
                    .wrap(),
                    text("Archive old managed entries").size(17),
                ]
                .spacing(8);
                if candidates.is_empty() {
                    content = content.push(text("No managed entries are more than 180 days old."));
                } else {
                    let list = column(candidates.iter().enumerate().map(|(index, candidate)| {
                        let label =
                            format!("{} · {}", candidate.entry.created, candidate.entry.body);
                        let check = checkbox(candidate.selected).label(label);
                        if can_edit {
                            check
                                .on_toggle(move |selected| {
                                    Message::SelectArchiveCandidate(index, selected)
                                })
                                .into()
                        } else {
                            check.into()
                        }
                    }))
                    .spacing(8);
                    content = content.push(scrollable(list).height(180)).push(
                        button("Review selected entries…").on_press_maybe(
                            (can_edit && candidates.iter().any(|candidate| candidate.selected))
                                .then_some(Message::ReviewArchiveCandidates),
                        ),
                    );
                }
                content.push(button("Back").on_press(Message::Back)).into()
            }
            Page::History(versions, preview) => {
                let mut list = column![text("Memory history").size(17)].spacing(8);
                if versions.is_empty() {
                    list = list.push(text("No previous versions have been saved."));
                }
                for name in versions {
                    list = list.push(
                        button(name.as_str())
                            .style(button::secondary)
                            .on_press(Message::ViewHistory(name.clone())),
                    );
                }
                let mut content = column![scrollable(list).height(140)].spacing(8);
                if let Some(version) = preview {
                    content = content
                        .push(text(&version.name))
                        .push(
                            version
                                .comparison
                                .view("Selected version", Length::Fixed(210.0)),
                        )
                        .push(
                            button("Restore version…").on_press_maybe(
                                (can_edit && self.can_restore_history())
                                    .then_some(Message::RestoreHistory),
                            ),
                        );
                }
                content.push(button("Back").on_press(Message::Back)).into()
            }
            Page::References(registry) => self.references_page(registry),
        }
    }

    fn references_page<'a>(&'a self, registry: &'a references::Registry) -> Element<'a, Message> {
        let enabled = self.can_edit_references() && !self.pending && self.decision.is_none();
        let review_allowed = self.can_read_references() && !self.pending && self.decision.is_none();
        let explanation = if self.policy.external_files_allowed() {
            "Approved local .md and .mdc files in your home directory can contribute excerpts when you opt in for a Terminal AI chat and review the request."
        } else {
            "External memory files are disabled by managed policy."
        };
        let mut list = column![
            text("External memory files").size(17),
            text(explanation).size(13),
            row![
                text_input("Absolute path to a UTF-8 reference", &self.reference_path)
                    .on_input_maybe(enabled.then_some(Message::ReferencePath)),
                button("Review file…").on_press_maybe(enabled.then_some(Message::ReviewReference)),
            ]
            .spacing(8),
        ]
        .spacing(8);
        for reference in registry
            .entries
            .iter()
            .filter(|entry| entry.applies_to(&self.scope))
        {
            let id = &reference.id;
            list = list.push(
                row![
                    text(&reference.path).width(Length::Fill).size(13),
                    button("Review…").on_press_maybe(
                        review_allowed.then(|| Message::ReviewExisting(reference.path.clone()))
                    ),
                    button(if reference.active {
                        "Disable"
                    } else {
                        "Enable"
                    })
                    .on_press_maybe(
                        enabled.then(|| Message::SetReferenceActive(id.clone(), !reference.active))
                    ),
                    button("Remove…")
                        .on_press_maybe(enabled.then(|| Message::RemoveReference(id.clone()))),
                ]
                .spacing(8),
            );
        }
        if let Some((_, content)) = &self.reference_preview {
            let explanation = if self.can_edit_references() {
                "Review the file content before approving it for this scope."
            } else {
                "Reference file preview"
            };
            list = list
                .push(text(explanation))
                .push(
                    text_editor(content)
                        .height(180)
                        .on_action(Message::ReadReference),
                )
                .push(
                    button("Approve this version")
                        .on_press_maybe(enabled.then_some(Message::ApproveReference)),
                );
        }
        list.push(button("Back").on_press(Message::Back)).into()
    }

    fn entries_page(&self, can_edit: bool) -> Element<'_, Message> {
        let Some(base) = &self.base else {
            return text("Memory has not been loaded.").into();
        };
        let entries = rows(&base.text);
        if entries.is_empty() {
            return text("No entries. Switch to Markdown to add your own notes.").into();
        }
        let mut list = column![].spacing(8);
        for (index, item) in entries.into_iter().enumerate() {
            if let Some((edited, editor)) = &self.row_edit
                && *edited == index
            {
                list = list.push(
                    column![
                        text_editor(editor).height(100).on_action(Message::RowText),
                        row![
                            button("Cancel").on_press(Message::Cancel),
                            button("Save entry").on_press_maybe(
                                (self.can_write_files() && !self.pending)
                                    .then_some(Message::SaveRow)
                            ),
                        ]
                        .spacing(8),
                    ]
                    .spacing(8),
                );
                continue;
            }
            let (label, content) = item.managed.map_or_else(
                || ("User text".into(), &base.text[item.range]),
                |entry| {
                    let label = format!("kortty · {} · {}", entry.created, entry.source);
                    (label, &base.text[entry.range])
                },
            );
            let is_managed = label.starts_with("kortty ·");
            let display = if is_managed {
                content
                    .split_once('\n')
                    .and_then(|(_, body)| body.rsplit_once("<!-- /kortty:entry -->"))
                    .map_or(content, |(body, _)| body)
            } else {
                content
            };
            let line = row![
                column![text(label).size(12), text(display.trim()).size(14)]
                    .width(Length::Fill)
                    .spacing(4),
                button("Edit").on_press_maybe(can_edit.then_some(Message::EditRow(index))),
                button("Delete…").on_press_maybe(can_edit.then_some(Message::DeleteRow(index))),
                is_managed.then(|| button("Archive…")
                    .on_press_maybe(can_edit.then_some(Message::ArchiveRow(index)))),
            ]
            .spacing(6)
            .align_y(Alignment::Center);
            list = list.push(container(line).padding(8).style(container::bordered_box));
        }
        scrollable(list).height(240).into()
    }
}

fn cleanup_request_view(review: &cleanup::Review) -> Element<'_, Message> {
    let mut details = column![text(review.destination.as_str())].spacing(7);
    let prompt = |value| {
        text(value)
            .font(iced::Font::MONOSPACE)
            .wrapping(text::Wrapping::WordOrGlyph)
    };
    if let Some(request) = &review.cli {
        let (label, primary) = request.primary();
        details = details
            .push(text(request.command_review()))
            .push(text(
                "The approved program can access files, accounts or the network.",
            ))
            .push(text(label))
            .push(prompt(primary));
        if let Some(secondary) = request.secondary() {
            details = details
                .push(text("Exact UTF-8 user-prompt.txt"))
                .push(prompt(secondary));
        }
    } else {
        details = details
            .push(prompt(review.system.as_str()))
            .push(prompt(review.payload.as_str()));
    }
    column![
        text(review.title()).size(17),
        scrollable(details).height(Length::Fill),
        row![
            button("Cancel")
                .style(button::secondary)
                .on_press(Message::Cancel),
            button("Send reviewed request").on_press(Message::Confirm),
        ]
        .spacing(8),
    ]
    .height(Length::Fill)
    .spacing(10)
    .into()
}

fn review_view<'a>(
    prompt: &'static str,
    label: &'static str,
    comparison: &'a Comparison,
    apply: &'static str,
    destructive: bool,
) -> Element<'a, Message> {
    column![
        text(prompt).size(17),
        comparison.view(label, Length::Fill),
        row![
            button("Cancel")
                .style(button::secondary)
                .on_press(Message::Cancel),
            button(apply)
                .style(if destructive {
                    button::danger
                } else {
                    button::primary
                })
                .on_press(Message::Confirm),
        ]
        .spacing(8),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .spacing(10)
    .into()
}

fn bounded_edit(editor: &mut text_editor::Content, action: text_editor::Action, limit: usize) {
    let before = editor.text();
    editor.perform(action);
    if editor.text().len() > limit {
        *editor = text_editor::Content::with_text(&before);
    }
}

fn rows(text: &str) -> Vec<EntryRow> {
    let mut result = Vec::new();
    let mut next = 0;
    for entry in format::entries(text) {
        user_rows(text, next..entry.range.start, &mut result);
        next = entry.range.end;
        result.push(EntryRow {
            range: entry.range.clone(),
            managed: Some(entry),
        });
    }
    user_rows(text, next..text.len(), &mut result);
    result
}

fn user_rows(text: &str, range: Range<usize>, rows: &mut Vec<EntryRow>) {
    let mut offset = range.start;
    let mut current = None;
    for line in text[range].split_inclusive('\n') {
        if line.trim().is_empty() {
            if let Some(start) = current.take() {
                rows.push(EntryRow {
                    range: start..offset,
                    managed: None,
                });
            }
        } else if current.is_none() {
            current = Some(offset);
        }
        offset += line.len();
    }
    if let Some(start) = current {
        rows.push(EntryRow {
            range: start..offset,
            managed: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::futures::{StreamExt as _, executor::block_on};
    use iced::{Font, Pixels, Renderer, Size};

    use super::*;

    fn finish(state: &mut State, task: Task<Message>) {
        let mut actions =
            iced_runtime::task::into_stream(task).expect("scheduled memory operation");
        let Some(iced_runtime::Action::Output(message)) = block_on(actions.next()) else {
            panic!("memory operation did not finish");
        };
        assert!(iced_runtime::task::into_stream(state.update(message)).is_none());
    }

    #[test]
    fn memory_review_grows_and_keeps_actions_visible() {
        let comparison = Comparison::new(&"Current note\n".repeat(100), "");
        let mut view = review_view(
            "Clear memory?",
            "After clearing",
            &comparison,
            "Clear",
            true,
        );
        let mut tree = Tree::new(view.as_widget());
        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));
        let mut measure = |height| {
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(260.0, height)),
            );
            let mut children = Layout::new(&node).children();
            let comparison = children.nth(1).unwrap().bounds();
            let actions = children.next().unwrap().bounds();
            assert!(actions.y >= comparison.y + comparison.height);
            assert!(actions.y + actions.height <= height);
            comparison.height
        };
        let compact = measure(160.0);
        let tall = measure(480.0);
        assert!(compact > 0.0);
        assert!((tall - compact - 320.0).abs() < 1.0);
    }

    #[test]
    fn organization_rules_are_readable_with_personal_memory_disabled() -> io::Result<()> {
        let mut policy = crate::clipboard_policy::parse(
            r#"
[meta]
schema-version = 1
[[rule]]
[rule.ai-memory]
user-memory = "disabled"
[[organization-memory]]
title = "First rule"
content = "Confirm the target before deployment."
[[organization-memory]]
title = "Incident procedure"
content = "Use the approved incident procedure."
"#,
        )?
        .ai_memory;
        let settings = Settings {
            enabled: false,
            ..Settings::default()
        };
        let mut state = State::new(settings, PathBuf::new(), Vec::new(), false);
        state.set_policy(&policy, true);
        assert!(!state.can_read_files());
        assert!(state.organization_rules().is_some());
        assert!(state.organization_open.is_empty());
        for index in [0, 1, 2, 0] {
            let task = state.update(Message::ToggleOrganizationRule(index));
            assert!(iced_runtime::task::into_stream(task).is_none());
        }
        assert_eq!(state.organization_open, HashSet::from([1]));
        assert!(!state.is_dirty());

        for (feature_allowed, ai_allowed) in [(true, false), (false, true)] {
            state.set_policy(&policy, true);
            policy.feature_allowed = feature_allowed;
            state.set_policy(&policy, ai_allowed);
            assert!(!state.can_read_files());
            assert!(iced_runtime::task::into_stream(state.load()).is_none());
            assert!(state.organization_rules().is_some());
            assert!(state.organization_open.is_empty());
            let _ = state.update(Message::ToggleOrganizationRule(0));
            assert_eq!(state.organization_open, HashSet::from([0]));
            assert!(!state.is_dirty());
        }
        Ok(())
    }

    #[test]
    fn managed_memory_denial_blocks_file_actions_and_late_results() {
        for (feature_allowed, mode) in [
            (false, UserMemoryMode::Editable),
            (true, UserMemoryMode::Disabled),
        ] {
            let settings = Settings::default();
            let mut policy = AiMemoryPolicy::default();
            policy.feature_allowed = feature_allowed;
            policy.user_memory = mode;
            let scope = Scope::connection(7, "Other scope".into());
            let mut state = State::new(
                settings.clone(),
                PathBuf::from("unused-relative-memory-directory"),
                vec![scope.clone()],
                true,
            );
            state.set_policy(&policy, true);
            assert!(iced_runtime::task::into_stream(state.load()).is_none());
            for message in [
                Message::Reload,
                Message::Scope(scope),
                Message::ShowArchive,
                Message::ShowHistory,
                Message::ShowReferences,
                Message::Enable(false),
                Message::Tokens("bad".into()),
            ] {
                assert!(iced_runtime::task::into_stream(state.update(message)).is_none());
            }
            let mut file = Snapshot::default();
            file.text = "Saved content from an earlier request.".into();
            let _ = state.update(Message::Completed(Ok(Outcome::Document(file, "loaded"))));
            assert!(state.base.is_none());
            assert!(state.editor.text().trim().is_empty());
            assert_eq!(state.scope, Scope::global());
            assert_eq!(state.settings(), &settings);
            assert!(state.settings_valid());
            assert!(!state.is_dirty());
            assert!(!state.busy());
        }
    }

    #[test]
    fn managed_read_only_memory_browses_but_refuses_file_changes() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let directory = root.join("memory");
        let scope = Scope::global();
        let saved = store::save(&directory, &scope, &Snapshot::default(), "Older note.\n")?;
        let current = "Current note.\n";
        store::save(&directory, &scope, &saved, current)?;

        let path = root.join("reference.md");
        std::fs::write(&path, "Previously approved note.\n")?;
        let mut policy = AiMemoryPolicy::default();
        let approved = references::approve(
            &directory,
            &scope,
            references::registry(&directory)?,
            references::review(path.clone(), &policy)?,
            &policy,
        )?;
        let id = &approved.entries[0].id;

        let mut state = State::new(Settings::default(), directory.clone(), Vec::new(), true);
        policy.user_memory = UserMemoryMode::ReadOnly;
        state.set_policy(&policy, true);
        let task = state.load();
        finish(&mut state, task);
        let _ = state.update(Message::Markdown);
        assert!(state.can_find_markdown());
        let _ = state.update(Message::MarkdownAction(text_editor::Action::Edit(
            text_editor::Edit::Insert('!'),
        )));
        assert_eq!(state.editor.text(), current);
        for message in [Message::SaveMarkdown, Message::Clear, Message::EditRow(0)] {
            assert!(iced_runtime::task::into_stream(state.update(message)).is_none());
        }
        assert!(state.decision.is_none());
        assert!(state.row_edit.is_none());

        let task = state.update(Message::ShowHistory);
        finish(&mut state, task);
        let Page::History(names, _) = &state.page else {
            panic!("history should remain available");
        };
        let task = state.update(Message::ViewHistory(names[0].clone()));
        finish(&mut state, task);
        assert!(matches!(state.page, Page::History(_, Some(_))));
        let _ = state.update(Message::RestoreHistory);
        assert!(state.decision.is_none());

        let task = state.update(Message::ShowArchive);
        finish(&mut state, task);
        assert!(matches!(state.page, Page::Archive(..)));
        let task = state.update(Message::ShowReferences);
        finish(&mut state, task);
        let task = state.update(Message::ReviewExisting(path.to_string_lossy().into_owned()));
        finish(&mut state, task);
        assert!(state.reference_preview.is_some());
        for message in [
            Message::ReviewExisting(root.join("another.md").to_string_lossy().into_owned()),
            Message::ApproveReference,
            Message::SetReferenceActive(id.clone(), false),
            Message::RemoveReference(id.clone()),
        ] {
            assert!(iced_runtime::task::into_stream(state.update(message)).is_none());
        }
        assert!(state.decision.is_none());
        assert_eq!(store::load(&directory, &scope)?.text, current);
        assert!(references::registry(&directory)?.entries[0].active);
        Ok(())
    }

    #[test]
    fn managed_memory_settings_apply_learning_and_budget_without_overwriting_preferences() {
        let settings = Settings::default();
        let mut state = State::new(settings.clone(), PathBuf::new(), Vec::new(), true);
        let mut policy = AiMemoryPolicy::default();
        policy.user_memory = UserMemoryMode::ReadOnly;
        policy.learning = Some(AiMemoryLearningMode::Off);
        policy.max_tokens = Some(300);
        state.set_policy(&policy, true);
        assert_eq!(state.effective_learning(), Learning::Off);
        assert_eq!(state.effective_budget(), 300);
        assert_eq!(state.token_input, "300");
        let _ = state.update(Message::Learning(Learning::Off));
        let _ = state.update(Message::Tokens("299".into()));
        assert!(state.settings_valid());
        assert_eq!(state.settings(), &settings);
        assert!(!state.is_dirty());

        policy.learning = None;
        policy.max_tokens = Some(1000);
        state.set_policy(&policy, true);
        assert_eq!(state.effective_budget(), 1000);
        assert_eq!(state.settings(), &settings);
        let _ = state.update(Message::Tokens("1001".into()));
        assert!(!state.settings_valid());
        assert_eq!(state.settings(), &settings);
        let _ = state.update(Message::Tokens("700".into()));
        assert!(state.settings_valid());
        assert_eq!(state.effective_budget(), 700);
        assert_eq!(state.settings().max_tokens, 700);
    }

    #[test]
    fn memory_reference_policy_keeps_metadata_and_blocks_management() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let path = root.join("reference.md");
        std::fs::write(&path, "A previously approved note.\n")?;
        let directory = root.join("memory");
        let scope = Scope::global();
        let mut policy = AiMemoryPolicy::default();
        let preview = references::review(path.clone(), &policy)?;
        let approved = references::approve(
            &directory,
            &scope,
            references::registry(&directory)?,
            preview.clone(),
            &policy,
        )?;
        let id = &approved.entries[0].id;
        let path = path.to_string_lossy().into_owned();
        let mut state = State::new(Settings::default(), directory.clone(), Vec::new(), true);
        policy.external_files_allowed = false;
        state.set_policy(&policy, true);

        let task = state.update(Message::ShowReferences);
        finish(&mut state, task);
        let Page::References(registry) = &state.page else {
            panic!("reference metadata should remain visible");
        };
        assert_eq!(registry.entries[0].path, path);
        assert!(registry.entries[0].active);

        for message in [
            Message::ReferencePath(path.clone()),
            Message::ReviewReference,
            Message::ReviewExisting(path),
            Message::Completed(Ok(Outcome::ReferencePreview(preview))),
            Message::ApproveReference,
            Message::SetReferenceActive(id.clone(), false),
            Message::RemoveReference(id.clone()),
        ] {
            assert!(iced_runtime::task::into_stream(state.update(message)).is_none());
        }
        assert!(state.reference_path.is_empty());
        assert!(state.reference_preview.is_none());
        assert!(state.decision.is_none());
        assert!(!state.busy());
        let stored = references::registry(&directory)?;
        assert_eq!(stored.entries.len(), 1);
        assert_eq!(&stored.entries[0].id, id);
        assert!(stored.entries[0].active);
        Ok(())
    }

    #[test]
    fn memory_archive_review_cancellation_and_confirmation_preserve_unselected_text()
    -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.join("memory");
        let owned = |id: &str, date: &str, body: &str| {
            format::Entry {
                range: 0..0,
                id: id.into(),
                origin: "kortty".into(),
                created: date.into(),
                source: "chat".into(),
                body: String::new(),
            }
            .rewrite(body)
            .unwrap()
        };
        let first = owned("first", "2000-01-03", "Keep port settings.");
        let second = owned("second", "2000-01-01", "Keep terminal colors.");
        let unchecked = owned("unchecked", "2000-01-02", "Do not archive this.");
        let managed = owned("edited", "2000-01-01", "Original");
        let modified = managed.replace("Original", "Edited by user");
        let user = "Notes written by the user\n\n";
        let original = format!("{first}{user}{unchecked}{second}{modified}");
        let expected = format!("{user}{unchecked}{modified}");
        let scope = Scope::global();
        let initial = store::save(
            &directory,
            &scope,
            &Snapshot::default(),
            &format!("{first}{user}{unchecked}{second}{managed}"),
        )?;
        store::save(&directory, &scope, &initial, &original)?;

        let mut state = State::new(Settings::default(), directory.clone(), Vec::new(), true);
        let task = state.load();
        finish(&mut state, task);
        let _ = state.update(Message::ShowCleanup);
        let Page::Cleanup(candidates) = &state.page else {
            panic!("archive selection")
        };
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.entry.id.as_str())
                .collect::<Vec<_>>(),
            ["second", "unchecked", "first"]
        );
        let _ = state.update(Message::SelectArchiveCandidate(1, false));
        let _ = state.update(Message::ReviewArchiveCandidates);
        assert!(matches!(
            state.decision,
            Some(Decision::ArchiveCandidates(..))
        ));
        let _ = state.update(Message::Cancel);
        assert!(store::archive(&directory, &scope)?.1.is_empty());
        assert_eq!(store::load(&directory, &scope)?.text, original);
        let _ = state.update(Message::ReviewArchiveCandidates);
        let task = state.update(Message::Confirm);
        finish(&mut state, task);
        assert_eq!(store::load(&directory, &scope)?.text, expected);
        let (_, archive) = store::archive(&directory, &scope)?;
        assert_eq!(
            archive
                .iter()
                .map(|item| item.entry.id.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );

        let task = state.update(Message::ShowHistory);
        finish(&mut state, task);
        let versions = store::history(&directory, &scope)?;
        assert_eq!(versions.len(), 2);
        let task = state.update(Message::ViewHistory(versions[0].clone()));
        finish(&mut state, task);
        let _ = state.update(Message::RestoreHistory);
        assert!(matches!(state.decision, Some(Decision::RestoreHistory)));
        let task = state.update(Message::Confirm);
        finish(&mut state, task);
        assert_eq!(store::load(&directory, &scope)?.text, original);

        let _ = state.update(Message::Clear);
        assert!(matches!(state.decision, Some(Decision::Clear(_))));
        let _ = state.update(Message::Cancel);
        assert_eq!(store::load(&directory, &scope)?.text, original);
        let _ = state.update(Message::Clear);
        let task = state.update(Message::Confirm);
        finish(&mut state, task);
        assert!(store::load(&directory, &scope)?.text.is_empty());
        assert_eq!(store::history(&directory, &scope)?.len(), 4);
        Ok(())
    }
}
