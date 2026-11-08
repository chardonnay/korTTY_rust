//! The AI Manager's editor for local global and per-connection memory.

use std::{fmt, io, ops::Range, path::PathBuf};

use iced::{
    Alignment, Element, Length, Task,
    widget::{
        button, checkbox, column, container, pick_list, progress_bar, row, rule, scrollable, space,
        text, text_editor, text_input,
    },
};
use serde::{Deserialize, Serialize};

mod format;
mod references;
mod store;

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
    fn global() -> Self {
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
    Enable(bool),
    Learning(Learning),
    Tokens(String),
    Scope(Scope),
    Reload,
    Entries,
    Markdown,
    MarkdownAction(text_editor::Action),
    SaveMarkdown,
    EditRow(usize),
    RowText(text_editor::Action),
    SaveRow,
    DeleteRow(usize),
    ArchiveRow(usize),
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
    Clear,
    DeleteRow(usize),
    ArchiveRow(usize),
    RestoreArchive(usize),
    DeleteArchive(usize),
    RestoreHistory,
    RemoveReference(String),
}

enum Page {
    Entries,
    Markdown,
    Archive(Snapshot, Vec<ArchiveItem>),
    History(Vec<String>, Option<(String, String)>),
    References(references::Registry),
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
    settings: Settings,
    token_input: String,
    initial_settings: Settings,
    base: Option<Snapshot>,
    editor: text_editor::Content,
    row_edit: Option<(usize, text_editor::Content)>,
    reference_path: String,
    reference_preview: Option<(references::Review, text_editor::Content)>,
    page: Page,
    decision: Option<Decision>,
    pending: bool,
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
            token_input: settings.max_tokens.to_string(),
            initial_settings: settings.clone(),
            settings,
            base: None,
            editor: text_editor::Content::new(),
            row_edit: None,
            reference_path: String::new(),
            reference_preview: None,
            page: Page::Entries,
            decision: None,
            pending: false,
            notice: None,
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
        self.token_input
            .parse::<u16>()
            .is_ok_and(|tokens| (MIN_TOKENS..=MAX_TOKENS).contains(&tokens))
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending
    }

    pub(crate) fn load(&mut self) -> Task<Message> {
        if self.pending || self.has_unsaved_document() {
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

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        if let Message::Completed(result) = message {
            self.pending = false;
            match result {
                Ok(Outcome::Document(file, notice)) => {
                    self.editor = text_editor::Content::with_text(&file.text);
                    self.base = Some(file);
                    self.row_edit = None;
                    self.notice = Some(notice.into());
                    if !matches!(self.page, Page::Markdown) {
                        self.page = Page::Entries;
                    }
                }
                Ok(Outcome::History(names)) => self.page = Page::History(names, None),
                Ok(Outcome::Version(name, value)) => {
                    if let Page::History(_, version) = &mut self.page {
                        *version = Some((name, value));
                    }
                }
                Ok(Outcome::Archive(file, rows)) => self.page = Page::Archive(file, rows),
                Ok(Outcome::References(registry, notice)) => {
                    self.page = Page::References(registry);
                    self.reference_preview = None;
                    self.notice = (!notice.is_empty()).then(|| notice.into());
                }
                Ok(Outcome::ReferencePreview(preview)) => {
                    let content = text_editor::Content::with_text(&preview.text);
                    self.reference_preview = Some((preview, content));
                }
                Err(error) => self.notice = Some(error),
            }
            return Task::none();
        }
        if self.pending {
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
            Message::Learning(learning) if self.editable => self.settings.learning = learning,
            Message::Tokens(tokens) if self.editable => {
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
            Message::MarkdownAction(action) if self.editable && self.base.is_some() => {
                bounded_edit(&mut self.editor, action, store::MAX_BYTES);
            }
            Message::SaveMarkdown if self.editable && self.markdown_dirty() => {
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
            Message::RowText(action) if self.editable => {
                if let Some((_, editor)) = &mut self.row_edit {
                    bounded_edit(editor, action, store::MAX_BYTES);
                }
            }
            Message::SaveRow if self.editable && !self.markdown_dirty() => {
                return self.save_row();
            }
            Message::DeleteRow(index) if self.can_edit_rows() => {
                self.decision = Some(Decision::DeleteRow(index));
            }
            Message::ArchiveRow(index) if self.can_edit_rows() => {
                self.decision = Some(Decision::ArchiveRow(index));
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
            Message::ViewHistory(name) => {
                return self.run(move |path, scope| {
                    store::history_text(&path, &scope, &name)
                        .map(|value| Outcome::Version(name, value))
                });
            }
            Message::RestoreHistory if self.can_edit_rows() => {
                self.decision = Some(Decision::RestoreHistory);
            }
            Message::ShowReferences if !self.has_unsaved_document() => {
                return self.run(|path, _| {
                    references::registry(&path).map(|registry| Outcome::References(registry, ""))
                });
            }
            Message::ReferencePath(value) => {
                self.reference_path = value.chars().take(4_096).collect();
                self.reference_preview = None;
            }
            Message::ReviewReference
                if self.editable && matches!(self.page, Page::References(_)) =>
            {
                return self.review_reference(self.reference_path.clone());
            }
            Message::ReviewExisting(path)
                if self.editable && matches!(self.page, Page::References(_)) =>
            {
                self.reference_path.clone_from(&path);
                return self.review_reference(path);
            }
            Message::ReadReference(action) if !action.is_edit() => {
                if let Some((_, editor)) = &mut self.reference_preview {
                    editor.perform(action);
                }
            }
            Message::ApproveReference if self.editable => {
                if let Page::References(registry) = &self.page
                    && let Some((preview, _)) = self.reference_preview.as_ref()
                {
                    let registry = registry.clone();
                    let preview = preview.clone();
                    return self.run(move |path, scope| {
                        references::approve(&path, &scope, registry, preview).map(|registry| {
                            Outcome::References(registry, "Reference approved for this scope.")
                        })
                    });
                }
            }
            Message::SetReferenceActive(id, active) if self.editable => {
                return self.change_reference(id, Some(active));
            }
            Message::RemoveReference(id) if self.editable => {
                self.decision = Some(Decision::RemoveReference(id));
            }
            Message::Clear if self.can_edit_rows() => self.decision = Some(Decision::Clear),
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
                self.row_edit = None;
                self.reference_preview = None;
                self.page = Page::Entries;
                self.load()
            }
            Some(Decision::Reload) => {
                self.row_edit = None;
                if let Some(base) = self.base.as_ref() {
                    self.editor = text_editor::Content::with_text(&base.text);
                }
                self.load()
            }
            Some(Decision::Clear) if self.can_edit_rows() => {
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
                    return self.run(move |path, scope| {
                        store::move_to_archive(&path, &scope, &base, &entry)
                            .map(|snapshot| Outcome::Document(snapshot, "Entry archived."))
                    });
                }
                Task::none()
            }
            Some(Decision::RestoreArchive(index)) if self.can_edit_rows() => {
                self.change_archive(index, true)
            }
            Some(Decision::DeleteArchive(index)) if self.can_edit_rows() => {
                self.change_archive(index, false)
            }
            Some(Decision::RestoreHistory) if self.can_edit_rows() => {
                if let Page::History(_, Some((_, value))) = &self.page {
                    return self.save(value.clone(), "History version restored.");
                }
                Task::none()
            }
            Some(Decision::RemoveReference(id)) if self.editable => self.change_reference(id, None),
            _ => Task::none(),
        }
    }

    fn can_edit_rows(&self) -> bool {
        self.editable && self.base.is_some() && !self.has_unsaved_document()
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
        let budget = self.settings.max_tokens.clamp(MIN_TOKENS, MAX_TOKENS);
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
        self.run(move |_, _| references::review(PathBuf::from(path)).map(Outcome::ReferencePreview))
    }

    fn change_reference(&mut self, id: String, active: Option<bool>) -> Task<Message> {
        let Page::References(registry) = &self.page else {
            return Task::none();
        };
        let registry = registry.clone();
        self.run(move |path, scope| {
            references::change(&path, &scope, registry, &id, active)
                .map(|registry| Outcome::References(registry, "Reference list saved."))
        })
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let can_change_settings = self.editable && !self.pending && self.decision.is_none();
        let enabled = checkbox(self.settings.enabled).label("Enable AI memory");
        let enabled = if can_change_settings {
            enabled.on_toggle(Message::Enable)
        } else {
            enabled
        };
        let tokens = text_input("500–4,000", &self.token_input)
            .on_input_maybe(can_change_settings.then_some(Message::Tokens))
            .width(115);
        let budget = f32::from(self.settings.max_tokens.clamp(MIN_TOKENS, MAX_TOKENS));
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
            .width(Length::Fill);
        let can_edit = self.can_edit_rows() && !self.pending && self.decision.is_none();
        let mut body = column![
            text("Memory settings").size(19),
            enabled,
            row![
                text("Learning").width(140),
                pick_list(
                    [Learning::Propose, Learning::Off],
                    Some(self.settings.learning),
                    Message::Learning
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![text("Token budget").width(140), tokens]
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
                space::horizontal(),
                button("Archive…").on_press_maybe(
                    (!self.has_unsaved_document() && !self.pending).then_some(Message::ShowArchive)
                ),
                button("History…").on_press_maybe(
                    (!self.has_unsaved_document() && !self.pending).then_some(Message::ShowHistory)
                ),
                button("Clear…").on_press_maybe(can_edit.then_some(Message::Clear)),
            ]
            .spacing(6),
            self.current_page(can_edit),
            button("External memory files…").on_press_maybe(
                (!self.pending
                    && !self.has_unsaved_document()
                    && !matches!(self.page, Page::References(_)))
                .then_some(Message::ShowReferences)
            ),
        ]
        .spacing(11);
        if !self.editable {
            body = body.push(text("Memory files are read-only."));
        }
        if !self.settings_valid() {
            body = body.push(text("Enter a memory budget from 500 to 4,000 tokens."));
        }
        if self.pending {
            body = body.push(text("Working…"));
        }
        if let Some(notice) = self.notice.as_deref() {
            body = body.push(text(notice));
        }
        if let Some(action) = &self.decision {
            let prompt = match action {
                Decision::Scope(_) | Decision::Reload => "Discard the unsaved memory edit?",
                Decision::Clear => {
                    "Clear this memory file? Its previous version remains in History."
                }
                Decision::DeleteRow(_) => "Delete this entry from the memory file?",
                Decision::ArchiveRow(_) => "Move this managed entry to the archive?",
                Decision::RestoreArchive(_) => "Return this entry to the memory file?",
                Decision::DeleteArchive(_) => "Permanently delete this archived entry?",
                Decision::RestoreHistory => "Replace the current memory with this version?",
                Decision::RemoveReference(_) => "Remove this reference approval from the list?",
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

    fn current_page(&self, can_edit: bool) -> Element<'_, Message> {
        match &self.page {
            Page::Entries => self.entries_page(can_edit),
            Page::Markdown => {
                let editor = text_editor(&self.editor)
                    .height(230)
                    .placeholder("Enter Markdown for this scope");
                let editor = if self.editable && !self.pending && self.base.is_some() {
                    editor.on_action(Message::MarkdownAction)
                } else {
                    editor
                };
                column![
                    editor,
                    button("Save Markdown").on_press_maybe(
                        (self.editable && !self.pending && self.markdown_dirty())
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
                if let Some((name, value)) = preview {
                    content = content
                        .push(text(name))
                        .push(scrollable(container(text(value)).padding(8)).height(150))
                        .push(
                            button("Restore version…")
                                .on_press_maybe(can_edit.then_some(Message::RestoreHistory)),
                        );
                }
                content.push(button("Back").on_press(Message::Back)).into()
            }
            Page::References(registry) => self.references_page(registry),
        }
    }

    fn references_page<'a>(&'a self, registry: &'a references::Registry) -> Element<'a, Message> {
        let enabled = self.editable && !self.pending && self.decision.is_none();
        let mut list = column![
            text("External memory files").size(17),
            text("Approvals are stored locally for this scope. AI requests currently use no external references.").size(13),
            row![
                text_input("Absolute path to a UTF-8 reference", &self.reference_path)
                    .on_input_maybe(enabled.then_some(Message::ReferencePath)),
                button("Review file…").on_press_maybe(enabled.then_some(Message::ReviewReference)),
            ].spacing(8),
        ].spacing(8);
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
                        enabled.then(|| Message::ReviewExisting(reference.path.clone()))
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
            list = list
                .push(text(
                    "Review the file content before approving it for this scope.",
                ))
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
                                (self.editable && !self.pending).then_some(Message::SaveRow)
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
