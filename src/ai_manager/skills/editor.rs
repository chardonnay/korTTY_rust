//! Native split-view skill editor with explicit file operations and library drafts.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use iced::widget::{
    button, checkbox, column, container, pick_list, responsive, row, scrollable, text, text_editor,
    text_input,
};
use iced::{Alignment, Element, Font, Length, Task};

use crate::ai_manager::document_find;
use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::local_ai;

use super::{MAX_SKILL_BYTES, Skill, Store, Target, markdown, shorten};

mod chooser;
mod external;
mod statistics_view;

const MAX_IMPORT_FILES: usize = 32;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone)]
pub(crate) enum Message {
    Search(String),
    ShowHidden(bool),
    Sort(Sort),
    EnableLibrary(bool),
    AutoDetect(bool),
    LearnedRulesEnabled(bool),
    UsageStatistics(bool),
    Select(u64),
    Add,
    Delete,
    Hide(bool),
    Reset,
    Confirm,
    Cancel,
    EditName(String),
    EditDescription(String),
    EditTags(String),
    EditContent(text_editor::Action),
    Find(document_find::Message),
    EditRules(text_editor::Action),
    EditTarget(Target),
    EnableSkill(bool),
    Commit,
    Revert,
    Import,
    Export,
    BrowseImport,
    BrowseExport,
    UseTypedPath,
    ExportSkill(u64, bool),
    ExportAll(bool),
    Chooser(chooser::Message),
    ImportPaths(text_editor::Action),
    Path(String),
    RunFile,
    Imported(Result<Vec<Skill>, String>),
    Exported(Result<String, String>),
    Shorten,
    Shortened(u64, Result<Box<shorten::Proposal>, String>),
    AcceptShortening,
    External(external::Message),
    Statistics(statistics_view::Message),
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AiSkillMessage([redacted])")
    }
}

impl Message {
    pub(in crate::ai_manager) fn completes_work(&self) -> bool {
        matches!(
            self,
            Self::Imported(_)
                | Self::Exported(_)
                | Self::Shortened(..)
                | Self::Chooser(chooser::Message::Listed(..))
                | Self::External(external::Message::Completed(..))
                | Self::Statistics(statistics_view::Message::Loaded(..))
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Sort {
    #[default]
    Name,
    Status,
}

impl fmt::Display for Sort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Name => "Name",
            Self::Status => "Status",
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FileAction {
    Import,
    Export,
}

#[derive(Clone, Copy)]
enum Confirmation {
    Delete(u64),
    Reset(u64),
    DisableAutoDetection,
}

#[derive(Default)]
pub(in crate::ai_manager) struct Editor {
    search: String,
    show_hidden: bool,
    sort: Sort,
    skill_creation_override: Option<bool>,
    form: Option<Form>,
    confirmation: Option<Confirmation>,
    file: Option<FileAction>,
    chooser: Option<chooser::Chooser>,
    import_paths: text_editor::Content,
    export_skills: BTreeSet<u64>,
    path: String,
    in_flight: bool,
    ticket: u64,
    shortening: Option<(u64, local_ai::Cancel)>,
    proposal: Option<shorten::Proposal>,
    external: Option<external::Panel>,
    statistics_root: Option<PathBuf>,
    statistics_policy: Option<bool>,
    statistics: Option<statistics_view::Panel>,
    notice: Option<String>,
}

struct Form {
    id: u64,
    skill: Skill,
    tags: String,
    markdown: text_editor::Content,
    find: document_find::Find,
    rules: text_editor::Content,
    dirty: bool,
}

impl Form {
    fn new(id: u64, skill: &Skill) -> Self {
        Self {
            id,
            skill: skill.clone(),
            tags: skill.tags.join(", "),
            markdown: text_editor::Content::with_text(&skill.content),
            find: document_find::Find::default(),
            rules: text_editor::Content::with_text(&skill.rules.join("\n")),
            dirty: false,
        }
    }

    fn snapshot(&self) -> Skill {
        let mut skill = self.skill.clone();
        skill.tags = markdown::parse_tags(self.tags.trim());
        skill.content = self.markdown.text();
        skill.rules = self
            .rules
            .text()
            .lines()
            .map(str::trim)
            .filter(|rule| !rule.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        skill
    }
}

impl Editor {
    pub(in crate::ai_manager) fn with_statistics_root(mut self, root: PathBuf) -> Self {
        self.statistics_root = Some(root);
        self
    }

    pub(in crate::ai_manager) fn set_statistics_policy(&mut self, policy: Option<bool>) {
        self.statistics_policy = policy;
    }

    pub(in crate::ai_manager) fn set_skill_creation_override(
        &mut self,
        override_value: Option<bool>,
    ) {
        self.skill_creation_override = override_value;
    }

    pub(in crate::ai_manager) fn busy(&self) -> bool {
        self.in_flight
            || self.shortening.is_some()
            || self.external.as_ref().is_some_and(external::Panel::busy)
            || self
                .statistics
                .as_ref()
                .is_some_and(statistics_view::Panel::busy)
    }

    pub(in crate::ai_manager) fn pending_edit(&self) -> bool {
        self.form.as_ref().is_some_and(|form| form.dirty)
            || self
                .external
                .as_ref()
                .is_some_and(external::Panel::pending_edit)
    }

    pub(in crate::ai_manager) fn file_prompt_open(&self) -> bool {
        self.file.is_some() || self.external.is_some() || self.statistics.is_some()
    }

    pub(in crate::ai_manager) fn find_open(&self) -> bool {
        self.shortening.is_none()
            && self.proposal.is_none()
            && self.confirmation.is_none()
            && self.external.is_none()
            && self.statistics.is_none()
            && self.form.as_ref().is_some_and(|form| form.find.is_open())
    }

    pub(in crate::ai_manager) fn can_reload_after_rule(&self) -> bool {
        !self.busy()
            && !self.pending_edit()
            && !self.file_prompt_open()
            && self.proposal.is_none()
            && self.confirmation.is_none()
    }

    pub(in crate::ai_manager) fn reload_after_rule(&mut self, store: &Store) {
        debug_assert!(self.can_reload_after_rule());
        if let Some(id) = self.form.as_ref().map(|form| form.id) {
            self.select(store, id);
        }
    }

    fn importing(&self) -> bool {
        self.file == Some(FileAction::Import)
    }

    pub(in crate::ai_manager) fn commit(&mut self, store: &mut Store) -> Result<(), String> {
        if let Some(form) = self.form.as_mut().filter(|form| form.dirty) {
            store.replace(form.id, form.snapshot())?;
            form.dirty = false;
        }
        if let Some(panel) = &mut self.external {
            panel.commit(store)?;
        }
        Ok(())
    }

    pub(in crate::ai_manager) fn update(
        &mut self,
        store: &mut Store,
        message: Message,
        profile: Option<local_ai::Profile>,
    ) -> Task<Message> {
        if self.statistics.is_some() {
            return match message {
                Message::Cancel | Message::Statistics(statistics_view::Message::Close) => {
                    self.statistics = None;
                    Task::none()
                }
                Message::Statistics(event) => self
                    .statistics
                    .as_mut()
                    .expect("statistics report is open")
                    .update(event, &mut self.ticket)
                    .map(Message::Statistics),
                _ => Task::none(),
            };
        }
        if self.external.is_some() {
            return match message {
                Message::External(message) => self.update_external(store, message),
                Message::Cancel => {
                    self.external = None;
                    Task::none()
                }
                _ => Task::none(),
            };
        }
        if let Some((ticket, _)) = self.shortening.as_ref() {
            match message {
                Message::Shortened(completed, result) if *ticket == completed => {
                    self.shortening = None;
                    match result {
                        Ok(proposal) => {
                            self.proposal = Some(*proposal);
                            self.notice = None;
                        }
                        Err(error) => self.notice = Some(error),
                    }
                }
                Message::Cancel => {
                    self.shortening = None;
                    self.notice = Some("Shortening cancelled.".into());
                }
                _ => {}
            }
            return Task::none();
        }
        if self.proposal.is_some() {
            match message {
                Message::AcceptShortening => self.apply_shortening(store),
                Message::Cancel => self.proposal = None,
                _ => {}
            }
            return Task::none();
        }
        if self.in_flight {
            match message {
                Message::Imported(result) if self.file == Some(FileAction::Import) => {
                    self.in_flight = false;
                    self.finish_import(store, result);
                }
                Message::Exported(result) if self.file == Some(FileAction::Export) => {
                    self.in_flight = false;
                    self.finish_file(result);
                }
                _ => {}
            }
            return Task::none();
        }
        if self.confirmation.is_some() {
            match message {
                Message::Confirm => self.confirm(store),
                Message::Cancel => self.confirmation = None,
                _ => {}
            }
            return Task::none();
        }
        if self.file.is_some() {
            match message {
                Message::BrowseImport if self.importing() && self.chooser.is_none() => {
                    return self.open_chooser(chooser::Mode::Markdown);
                }
                Message::BrowseExport
                    if self.file == Some(FileAction::Export)
                        && self.chooser.is_none()
                        && self.export_skills.len() > 1 =>
                {
                    return self.open_chooser(chooser::Mode::ExportFolder);
                }
                Message::UseTypedPath => {
                    self.chooser = None;
                    self.notice = None;
                }
                Message::Chooser(message) => {
                    if let Some(chooser) = &mut self.chooser {
                        return chooser
                            .update(message, &mut self.ticket)
                            .map(Message::Chooser);
                    }
                }
                Message::ImportPaths(action) if self.importing() && self.chooser.is_none() => {
                    edit_document(&mut self.import_paths, action);
                }
                Message::Path(path)
                    if self.file == Some(FileAction::Export) && self.chooser.is_none() =>
                {
                    self.path = bounded(path, MAX_PATH_BYTES);
                }
                Message::ExportSkill(id, selected)
                    if self.file == Some(FileAction::Export)
                        && self.chooser.is_none()
                        && store.skills.contains_key(&id) =>
                {
                    let previous = self.export_skills.len();
                    if selected {
                        self.export_skills.insert(id);
                    } else {
                        self.export_skills.remove(&id);
                    }
                    self.export_selection_changed(previous);
                }
                Message::ExportAll(selected)
                    if self.file == Some(FileAction::Export) && self.chooser.is_none() =>
                {
                    let previous = self.export_skills.len();
                    self.export_skills.clear();
                    if selected {
                        self.export_skills.extend(store.skills.keys().copied());
                    }
                    self.export_selection_changed(previous);
                }
                Message::RunFile => return self.run_file(store),
                Message::Cancel => {
                    self.file = None;
                    self.chooser = None;
                    self.export_skills.clear();
                    self.notice = None;
                }
                _ => {}
            }
            return Task::none();
        }
        match message {
            Message::Search(value) => self.search = bounded(value, 256),
            Message::Find(message) => {
                if let Some(form) = &mut self.form {
                    return form.find.update(&mut form.markdown, message);
                }
            }
            Message::ShowHidden(value) => self.show_hidden = value,
            Message::Sort(sort) => self.sort = sort,
            Message::EnableLibrary(value) => store.enabled = value,
            Message::AutoDetect(true) => store.auto_detect = true,
            Message::AutoDetect(false) if store.auto_detect => {
                self.confirmation = Some(Confirmation::DisableAutoDetection);
            }
            Message::LearnedRulesEnabled(value) if self.skill_creation_override.is_none() => {
                store.learned_rules_enabled = value;
            }
            Message::UsageStatistics(value) if self.statistics_policy.is_none() => {
                store.usage_statistics = value;
            }
            Message::Select(id) if self.commit_or_notice(store) => self.select(store, id),
            Message::Add if self.commit_or_notice(store) => self.add(store),
            Message::Delete => {
                if let Some(form) = self
                    .form
                    .as_ref()
                    .filter(|form| form.skill.builtin.is_none())
                {
                    self.confirmation = Some(Confirmation::Delete(form.id));
                }
            }
            Message::Reset => {
                if let Some(form) = self
                    .form
                    .as_ref()
                    .filter(|form| form.skill.baseline().is_some())
                {
                    self.confirmation = Some(Confirmation::Reset(form.id));
                }
            }
            Message::Hide(hidden) if self.commit_or_notice(store) => {
                if let Some(form) = &mut self.form
                    && let Some(skill) = store
                        .skills
                        .get_mut(&form.id)
                        .filter(|skill| skill.builtin.is_some())
                {
                    skill.hidden = hidden;
                    form.skill.hidden = hidden;
                }
            }
            Message::Commit => {
                if self.commit_or_notice(store) {
                    self.notice = Some("Skill updated in the library draft.".into());
                }
            }
            Message::Revert => {
                if let Some(form) = &self.form {
                    self.select(store, form.id);
                }
            }
            Message::Shorten if self.commit_or_notice(store) => {
                return self.start_shortening(store, profile);
            }
            Message::External(message) => return self.update_external(store, message),
            Message::Statistics(statistics_view::Message::Open)
                if !self.pending_edit() && self.statistics_root.is_some() =>
            {
                let (panel, task) = statistics_view::Panel::open(
                    self.statistics_root
                        .clone()
                        .expect("statistics root is configured"),
                    &mut self.ticket,
                );
                self.statistics = Some(panel);
                return task.map(Message::Statistics);
            }
            Message::Import | Message::Export if self.commit_or_notice(store) => {
                self.file = if matches!(message, Message::Import) {
                    Some(FileAction::Import)
                } else if !store.skills.is_empty() {
                    Some(FileAction::Export)
                } else {
                    None
                };
                self.import_paths = text_editor::Content::new();
                self.path.clear();
                self.notice = None;
                if self.importing() {
                    return self.open_chooser(chooser::Mode::Markdown);
                }
                self.export_skills.clear();
                if let Some(form) = &self.form {
                    self.export_skills.insert(form.id);
                } else {
                    self.export_skills.extend(store.skills.keys().copied());
                }
            }
            _ => self.edit(message),
        }
        Task::none()
    }

    fn select(&mut self, store: &Store, id: u64) {
        self.form = store.skills.get(&id).map(|skill| Form::new(id, skill));
        self.notice = None;
    }

    fn add(&mut self, store: &mut Store) {
        let name = (1..=MAX_NEW_NAME_ATTEMPTS)
            .map(|index| {
                if index == 1 {
                    "New AI Skill".to_owned()
                } else {
                    format!("New AI Skill {index}")
                }
            })
            .find(|name| {
                !store
                    .skills
                    .values()
                    .any(|skill| skill.name.eq_ignore_ascii_case(name))
            });
        let Some(name) = name else {
            self.notice = Some("Could not find a unique skill name.".into());
            return;
        };
        let skill = Skill {
            name,
            enabled: true,
            ..Skill::default()
        };
        match store.insert(skill) {
            Ok(id) => {
                self.search.clear();
                self.select(store, id);
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn confirm(&mut self, store: &mut Store) {
        match self.confirmation.take() {
            Some(Confirmation::Delete(id)) => {
                if store
                    .skills
                    .get(&id)
                    .is_some_and(|skill| skill.builtin.is_none())
                {
                    store.skills.remove(&id);
                    self.form = None;
                    self.notice = Some("Skill removed from the library draft.".into());
                }
            }
            Some(Confirmation::Reset(id)) => {
                if let Some(base) = store.skills.get(&id).and_then(Skill::baseline) {
                    match store.replace(id, base.clone()) {
                        Ok(()) => {
                            self.select(store, id);
                            self.notice = Some("Bundled skill restored.".into());
                        }
                        Err(error) => self.notice = Some(error),
                    }
                }
            }
            Some(Confirmation::DisableAutoDetection) if self.commit_or_notice(store) => {
                store.auto_detect = false;
                for skill in store
                    .skills
                    .values_mut()
                    .filter(|skill| skill.builtin.is_some())
                {
                    skill.enabled = false;
                }
                if let Some(form) = &self.form {
                    self.select(store, form.id);
                }
                self.notice = Some("Bundled skills were disabled.".into());
            }
            Some(Confirmation::DisableAutoDetection) | None => {}
        }
    }

    fn edit(&mut self, message: Message) {
        let Some(form) = &mut self.form else { return };
        match message {
            Message::EditName(value) => form.skill.name = bounded(value, 256),
            Message::EditDescription(value) => form.skill.description = bounded(value, 4096),
            Message::EditTags(value) => form.tags = bounded(value, 8192),
            Message::EditContent(action) => {
                form.dirty |= edit_document(&mut form.markdown, action);
                return;
            }
            Message::EditRules(action) => {
                form.dirty |= edit_document(&mut form.rules, action);
                return;
            }
            Message::EditTarget(target) => form.skill.target = target,
            Message::EnableSkill(enabled) => form.skill.enabled = enabled,
            _ => return,
        }
        form.dirty = true;
    }

    fn commit_or_notice(&mut self, store: &mut Store) -> bool {
        match self.commit(store) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(error);
                false
            }
        }
    }

    fn export_selection_changed(&mut self, previous: usize) {
        if (previous == 1) != (self.export_skills.len() == 1) {
            self.path.clear();
        }
        self.notice = None;
    }

    fn open_chooser(&mut self, mode: chooser::Mode) -> Task<Message> {
        self.notice = None;
        let start = PathBuf::from(self.path.trim());
        let (chooser, task) =
            chooser::Chooser::open(mode, start.is_absolute().then_some(start), &mut self.ticket);
        self.chooser = Some(chooser);
        task.map(Message::Chooser)
    }

    fn run_file(&mut self, store: &Store) -> Task<Message> {
        self.notice = None;
        match self.file {
            Some(FileAction::Import) => {
                let paths = if let Some(chooser) = &self.chooser {
                    let paths = chooser.selection();
                    if paths.is_empty() {
                        Err("Choose at least one Markdown file.".into())
                    } else {
                        Ok(paths.to_vec())
                    }
                } else {
                    import_paths(&self.import_paths.text())
                };
                let paths = match paths {
                    Ok(paths) => paths,
                    Err(error) => {
                        self.notice = Some(error);
                        return Task::none();
                    }
                };
                self.in_flight = true;
                Task::future(async move {
                    Message::Imported(
                        paths
                            .iter()
                            .enumerate()
                            .map(|(index, path)| {
                                markdown::read(path).map_err(|error| {
                                    format!("Could not import Markdown file {}. {error}", index + 1)
                                })
                            })
                            .collect(),
                    )
                })
            }
            Some(FileAction::Export) => {
                let skills: Vec<_> = self
                    .export_skills
                    .iter()
                    .filter_map(|id| store.skills.get(id).cloned())
                    .collect();
                if skills.is_empty() || skills.len() != self.export_skills.len() {
                    self.notice = Some("Choose skills to export.".into());
                    return Task::none();
                }
                let mut path = match &self.chooser {
                    Some(chooser) => {
                        let Some(path) = chooser.export_folder() else {
                            return Task::none();
                        };
                        path.to_path_buf()
                    }
                    None => PathBuf::from(self.path.trim()),
                };
                if !path.is_absolute() {
                    self.notice = Some(if skills.len() == 1 {
                        "Enter an absolute Markdown file path.".into()
                    } else {
                        "Enter an existing absolute folder path.".into()
                    });
                    return Task::none();
                }
                if skills.len() == 1 && path.extension().is_none() {
                    path.set_extension("md");
                }
                self.in_flight = true;
                Task::future(async move {
                    let result = if let [skill] = skills.as_slice() {
                        markdown::write(&path, skill).map(|()| "Skill exported.".into())
                    } else {
                        markdown::write_batch(&path, &skills)
                    };
                    Message::Exported(result)
                })
            }
            None => Task::none(),
        }
    }

    fn finish_import(&mut self, store: &mut Store, result: Result<Vec<Skill>, String>) {
        let inserted = result.and_then(|skills| {
            let count = skills.len();
            let mut candidate = store.clone();
            let mut last = None;
            for skill in skills {
                last = Some(candidate.insert(skill)?);
            }
            let id = last.ok_or("Choose at least one Markdown file.")?;
            *store = candidate;
            Ok((id, count))
        });
        match inserted {
            Ok((id, count)) => {
                self.search.clear();
                self.select(store, id);
                let notice = if count == 1 {
                    "Skill imported into the library draft.".into()
                } else {
                    format!("{count} skills imported into the library draft.")
                };
                self.finish_file(Ok(notice));
            }
            Err(error) => self.finish_file(Err(error)),
        }
    }

    fn finish_file(&mut self, result: Result<String, String>) {
        self.notice = Some(match result {
            Ok(success) => {
                self.file = None;
                self.chooser = None;
                self.export_skills.clear();
                success
            }
            Err(error) => error,
        });
    }

    fn start_shortening(
        &mut self,
        store: &Store,
        profile: Option<local_ai::Profile>,
    ) -> Task<Message> {
        let Some(profile) = profile else {
            self.notice = Some("Enable a local text profile to shorten a skill.".into());
            return Task::none();
        };
        let Some(skill) = self
            .form
            .as_ref()
            .and_then(|form| store.skills.get(&form.id))
        else {
            return Task::none();
        };
        let pending = match shorten::request(profile, skill.clone()) {
            Ok(pending) => pending,
            Err(error) => {
                self.notice = Some(error);
                return Task::none();
            }
        };
        self.ticket = self.ticket.wrapping_add(1);
        let ticket = self.ticket;
        let local_ai::Pending { reply, cancel } = pending;
        self.shortening = Some((ticket, cancel));
        self.notice = None;
        Task::future(async move {
            let result = reply
                .recv()
                .await
                .unwrap_or_else(|_| Err("The AI worker stopped.".into()));
            Message::Shortened(ticket, result.map(Box::new))
        })
    }

    fn apply_shortening(&mut self, store: &mut Store) {
        let (Some(proposal), Some(form)) = (self.proposal.take(), &self.form) else {
            return;
        };
        let id = form.id;
        if store.skills.get(&id) != Some(&proposal.original) {
            self.notice = Some("The skill has changed. Request a fresh proposal.".into());
            return;
        }
        match store.replace(id, proposal.suggested) {
            Ok(()) => {
                self.select(store, id);
                self.notice = Some("Shortening applied to the library draft.".into());
            }
            Err(error) => self.notice = Some(error),
        }
    }

    pub(in crate::ai_manager) fn view<'a>(
        &'a self,
        store: &'a Store,
        shorten_available: bool,
    ) -> Element<'a, Message> {
        if let Some(panel) = &self.statistics {
            return panel
                .view(store, self.statistics_policy)
                .map(Message::Statistics);
        }
        if let Some(panel) = &self.external {
            return panel.view(store).map(Message::External);
        }
        if let Some(chooser) = &self.chooser {
            let (count, label, can_run, typed_path) = if self.importing() {
                let count = chooser.selection().len();
                (count, "Import selected", count > 0, "Enter paths…")
            } else {
                (
                    self.export_skills.len(),
                    "Export to this folder",
                    chooser.export_folder().is_some(),
                    "Enter folder path…",
                )
            };
            return column![
                chooser.view(self.in_flight).map(Message::Chooser),
                self.notice.as_deref().map(|notice| text(notice).size(13)),
                row![
                    text(format!("{count} selected")).size(13),
                    button(label)
                        .on_press_maybe((!self.in_flight && can_run).then_some(Message::RunFile),),
                    button(typed_path)
                        .on_press_maybe((!self.in_flight).then_some(Message::UseTypedPath),),
                    button("Cancel").on_press_maybe((!self.in_flight).then_some(Message::Cancel)),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap(),
            ]
            .spacing(8)
            .height(Length::Fill)
            .into();
        }
        if self.file == Some(FileAction::Export) {
            return self.export_view(store);
        }
        let header = row![
            checkbox(store.enabled)
                .label("Enable AI skills")
                .on_toggle(Message::EnableLibrary),
            checkbox(store.auto_detect)
                .label("Automatically select relevant skills")
                .on_toggle(Message::AutoDetect),
            checkbox(
                self.skill_creation_override
                    .unwrap_or(store.learned_rules_enabled)
            )
            .label("Allow saving learned rules in skills")
            .on_toggle_maybe(
                self.skill_creation_override
                    .is_none()
                    .then_some(Message::LearnedRulesEnabled)
            ),
            checkbox(self.show_hidden)
                .label("Show hidden")
                .on_toggle(Message::ShowHidden),
            checkbox(store.statistics_enabled(self.statistics_policy))
                .label("Record local skill statistics")
                .on_toggle_maybe(
                    self.statistics_policy
                        .is_none()
                        .then_some(Message::UsageStatistics)
                ),
            button("Skill statistics…").on_press_maybe(
                (self.statistics_root.is_some() && !self.pending_edit() && !self.busy())
                    .then_some(Message::Statistics(statistics_view::Message::Open))
            ),
        ]
        .spacing(14)
        .wrap();
        let query = self.search.trim().to_lowercase();
        let mut visible: Vec<_> = store
            .skills
            .iter()
            .filter(|(_, skill)| (self.show_hidden || !skill.hidden) && skill.matches(&query))
            .collect();
        visible.sort_by_cached_key(|(_, skill)| {
            (
                if self.sort == Sort::Status {
                    (!skill.enabled, skill.hidden)
                } else {
                    (false, false)
                },
                skill.name.to_lowercase(),
            )
        });
        let mut items = column![].spacing(2).width(Length::Fill);
        let selected = self.form.as_ref().map(|form| form.id);
        for &(id, skill) in &visible {
            let marker = if skill.hidden {
                "Hidden · "
            } else if !skill.enabled {
                "Disabled · "
            } else {
                ""
            };
            let name = format!("{marker}{}", skill.name);
            items = items.push(
                button(text(name).size(13))
                    .width(Length::Fill)
                    .style(if selected == Some(*id) {
                        button::primary
                    } else {
                        button::text
                    })
                    .on_press(Message::Select(*id)),
            );
        }
        let library = column![
            text_input("Search skills…", &self.search).on_input(Message::Search),
            row![
                text(format!(
                    "{} of {} skills",
                    visible.len(),
                    store.skills.len()
                ))
                .size(13)
                .width(Length::Fill),
                pick_list([Sort::Name, Sort::Status], Some(self.sort), Message::Sort)
                    .dismiss_on_escape()
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            scrollable(items).height(Length::Fill),
            row![
                button("Add").on_press(Message::Add),
                button("Delete…").on_press_maybe(
                    self.form
                        .as_ref()
                        .filter(|form| form.skill.builtin.is_none())
                        .map(|_| Message::Delete)
                )
            ]
            .spacing(6)
            .wrap(),
            row![
                button("Import Markdown…").on_press(Message::Import),
                button("Export Markdown…")
                    .on_press_maybe((!store.skills.is_empty()).then_some(Message::Export))
            ]
            .spacing(6)
            .wrap(),
            row![
                button("Import external skills…")
                    .on_press(Message::External(external::Message::OpenImport)),
                button("Check external updates…").on_press_maybe(
                    external::Panel::can_check(store)
                        .then_some(Message::External(external::Message::OpenUpdates))
                ),
            ]
            .spacing(6)
            .wrap(),
        ]
        .width(235)
        .spacing(8);
        let body: Element<'_, Message> = if self.shortening.is_some() {
            column![
                text("Shortening skill with the local model…"),
                button("Cancel").on_press(Message::Cancel)
            ]
            .spacing(8)
            .into()
        } else if let Some(proposal) = &self.proposal {
            preview(proposal)
        } else if let Some(confirmation) = self.confirmation {
            confirmation_view(confirmation)
        } else if self.importing() {
            self.import_paths_view()
        } else if let Some(form) = &self.form {
            Self::form_view(form, shorten_available)
        } else {
            container(text("Select a skill to view or edit it."))
                .padding(12)
                .into()
        };
        let layout = row![
            library,
            container(body).width(Length::Fill).height(Length::Fill)
        ]
        .spacing(14)
        .height(Length::Fill);
        column![
            header,
            self.skill_creation_override.map(|_| text(
                "Learning rules into skills is managed by policy."
            )
            .size(13)
            .wrapping(text::Wrapping::WordOrGlyph)),
            layout,
            self.notice.as_deref().map(|notice| text(notice).size(13))
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    }

    fn import_paths_view(&self) -> Element<'_, Message> {
        let paths = text_editor(&self.import_paths)
            .placeholder("Absolute Markdown file paths")
            .height(Length::Fill);
        let paths = if self.in_flight {
            paths
        } else {
            paths.on_action(Message::ImportPaths)
        };
        column![
            text("Import Markdown").size(19),
            text("Enter one absolute Markdown file path per line. All files must be valid to import them together.")
                .size(13),
            paths,
            row![
                button("Import").on_press_maybe((!self.in_flight).then_some(Message::RunFile)),
                button("Browse…").on_press_maybe((!self.in_flight).then_some(Message::BrowseImport)),
                button("Cancel").on_press_maybe((!self.in_flight).then_some(Message::Cancel))
            ]
            .spacing(6)
            .wrap(),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn export_view<'a>(&'a self, store: &'a Store) -> Element<'a, Message> {
        let enabled = !self.in_flight;
        let count = self.export_skills.len();
        let batch = count > 1;
        let placeholder = if batch {
            "Existing absolute folder path"
        } else {
            "Absolute Markdown file path"
        };
        let mut entries = column![].spacing(5);
        for (&id, skill) in &store.skills {
            entries = entries.push(
                checkbox(self.export_skills.contains(&id))
                    .label(&skill.name)
                    .on_toggle_maybe(
                        enabled.then_some(move |selected| Message::ExportSkill(id, selected)),
                    ),
            );
        }
        column![
            text("Export Markdown").size(19),
            row![
                text(format!("{count} selected")).size(13),
                button("Select all").on_press_maybe(enabled.then_some(Message::ExportAll(true))),
                button("Clear selection")
                    .on_press_maybe(enabled.then_some(Message::ExportAll(false))),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            scrollable(entries).height(Length::Fill),
            text(if batch {
                "Select one existing folder. New files receive unique names and existing files are preserved."
            } else {
                "Choose a new Markdown file. Existing files are preserved."
            })
            .size(13),
            row![
                text_input(placeholder, &self.path)
                    .on_input_maybe((enabled && count > 0).then_some(Message::Path)),
                batch.then(|| button("Browse…").on_press_maybe(enabled.then_some(Message::BrowseExport))),
            ]
            .spacing(8),
            self.notice.as_deref().map(|notice| text(notice).size(13)),
            row![
                button("Export selected")
                    .on_press_maybe((enabled && count > 0).then_some(Message::RunFile)),
                button("Cancel").on_press_maybe(enabled.then_some(Message::Cancel)),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn form_view(form: &Form, shorten_available: bool) -> Element<'_, Message> {
        let fields = responsive(move |size| {
            // Give the form a finite height inside the scrollable so its editor can fill
            // the remaining space. A minimum keeps the fields usable in compact tabs.
            let minimum = if form.find.is_open() { 560.0 } else { 480.0 };
            Self::form_fields(form, size.height.max(minimum))
        });
        let actions = row![
            button("Apply skill edits").on_press_maybe(form.dirty.then_some(Message::Commit)),
            button("Revert edits").on_press_maybe(form.dirty.then_some(Message::Revert)),
            button("Shorten with local AI…").on_press_maybe(
                (shorten_available && (form.dirty || shorten::available(&form.skill)))
                    .then_some(Message::Shorten)
            )
        ]
        .spacing(6)
        .wrap();

        column![fields, actions]
            .spacing(8)
            .height(Length::Fill)
            .into()
    }

    fn form_fields(form: &Form, height: f32) -> Element<'_, Message> {
        let skill = &form.skill;
        let builtin = skill.builtin.as_ref().map(|_| {
            row![
                text("Bundled skill").width(Length::Fill),
                button("Restore bundled version…").on_press(Message::Reset),
                button(if skill.hidden { "Unhide" } else { "Hide" })
                    .on_press(Message::Hide(!skill.hidden))
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .wrap()
        });
        let fields = column![
            builtin,
            external::source_view(skill),
            field(
                "Name",
                text_input("Skill name", &skill.name).on_input(Message::EditName)
            ),
            field(
                "Description",
                text_input("Description", &skill.description).on_input(Message::EditDescription)
            ),
            field(
                "Tags",
                text_input("Comma-separated tags", &form.tags).on_input(Message::EditTags)
            ),
            row![
                checkbox(skill.enabled)
                    .label("Enabled")
                    .on_toggle(Message::EnableSkill),
                pick_list(Target::ALL, Some(skill.target), Message::EditTarget).dismiss_on_escape()
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            column![
                row![
                    text("Skill Markdown").size(13).width(Length::Fill),
                    button("Find…").on_press(Message::Find(document_find::Message::Open)),
                ]
                .align_y(Alignment::Center),
                form.find.view(Message::Find),
            ]
            .spacing(4),
            form.find.bind(
                text_editor(&form.markdown)
                    .height(Length::Fill)
                    .font(Font::MONOSPACE)
                    .on_action(Message::EditContent),
                Message::Find,
            ),
            text("Learned rules · one per line").size(13),
            text_editor(&form.rules)
                .height(85)
                .on_action(Message::EditRules),
        ]
        .spacing(8)
        .width(Length::Fill)
        .height(height);
        scrollable(fields).height(Length::Fill).into()
    }
}

const MAX_NEW_NAME_ATTEMPTS: usize = super::MAX_SKILLS + 1;

fn preview(proposal: &shorten::Proposal) -> Element<'static, Message> {
    let before = shorten::display(&proposal.original);
    let after = shorten::display(&proposal.suggested);
    let summary = format!("{} → {} bytes", before.len(), after.len());
    let panel = |title, body| {
        column![
            text(title).size(14),
            scrollable(text(body).font(Font::MONOSPACE).size(13)).height(Length::Fill)
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(8)
    };
    column![
        text("Review shortened skill").size(19),
        text(summary).size(13),
        row![panel("Original", before), panel("Proposal", after)]
            .height(Length::Fill)
            .spacing(12),
        row![
            button("Apply proposal").on_press(Message::AcceptShortening),
            button("Cancel").on_press(Message::Cancel),
        ]
        .spacing(6)
    ]
    .height(Length::Fill)
    .spacing(8)
    .into()
}

fn confirmation_view(confirmation: Confirmation) -> Element<'static, Message> {
    let (heading, explanation) = match confirmation {
        Confirmation::Delete(_) => (
            "Remove skill?",
            "The skill will be removed when you save the library.",
        ),
        Confirmation::Reset(_) => (
            "Restore bundled skill?",
            "Customizations to the bundled skill will be discarded.",
        ),
        Confirmation::DisableAutoDetection => (
            "Disable automatic selection?",
            "All bundled skills will be disabled. Enabled personal skills will apply without relevance filtering.",
        ),
    };
    column![
        text(heading).size(19),
        text(explanation).size(13),
        row![
            button("Confirm").on_press(Message::Confirm),
            button("Cancel").on_press(Message::Cancel),
        ]
        .spacing(6)
    ]
    .spacing(8)
    .into()
}

fn field<'a>(label: &'static str, input: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).size(13).width(94), input.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

fn bounded(mut value: String, limit: usize) -> String {
    value.truncate(value.floor_char_boundary(limit.min(value.len())));
    value
}

fn import_paths(text: &str) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if paths.len() == MAX_IMPORT_FILES {
            return Err(format!(
                "Choose up to {MAX_IMPORT_FILES} Markdown files at a time."
            ));
        }
        let path = PathBuf::from(line);
        if line.len() > MAX_PATH_BYTES || line.chars().any(char::is_control) || !path.is_absolute()
        {
            return Err("Enter one absolute Markdown file path per line.".into());
        }
        paths.push(path);
    }
    if paths.is_empty() {
        return Err("Choose at least one Markdown file.".into());
    }
    Ok(paths)
}

fn edit_document(content: &mut text_editor::Content, action: text_editor::Action) -> bool {
    if !action.is_edit() {
        content.perform(action);
        return false;
    }
    let before = content.text();
    content.perform(action);
    if content.text().len() > MAX_SKILL_BYTES {
        *content = text_editor::Content::with_text(&before);
        false
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::futures::{StreamExt as _, executor::block_on};
    use iced::{Pixels, Renderer, Size};

    use super::*;

    fn output(task: Task<Message>) -> Message {
        let mut actions = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(result)) = block_on(actions.next()) else {
            panic!("Missing Markdown import task output");
        };
        result
    }

    #[test]
    fn skill_learning_policy_locks_the_effective_value_without_editing_the_saved_preference() {
        let mut editor = Editor::default();
        let mut store = Store::default();
        assert!(store.learned_rules_enabled);
        let auto_detect = store.auto_detect;
        let _ = editor.update(&mut store, Message::LearnedRulesEnabled(false), None);
        assert!(!store.learned_rules_enabled);
        editor.set_skill_creation_override(Some(true));
        let _ = editor.update(&mut store, Message::LearnedRulesEnabled(true), None);
        assert!(!store.learned_rules_enabled);
        editor.set_skill_creation_override(None);
        let _ = editor.update(&mut store, Message::LearnedRulesEnabled(true), None);
        editor.set_skill_creation_override(Some(false));
        let _ = editor.update(&mut store, Message::LearnedRulesEnabled(false), None);
        assert!(store.learned_rules_enabled);
        assert_eq!(store.auto_detect, auto_detect);
    }

    #[test]
    fn skill_rule_reload_keeps_selection_and_search() {
        let mut editor = Editor::default();
        let mut store = Store::default();
        let id = *store.skills.keys().next().unwrap();
        editor.select(&store, id);
        editor.search = "Search text".into();
        let skill = store.skills.get_mut(&id).unwrap();
        skill.rules.push("Check the target.".into());
        assert!(editor.can_reload_after_rule());
        editor.reload_after_rule(&store);
        let form = editor.form.as_ref().unwrap();
        assert_eq!(form.id, id);
        assert!(form.rules.text().contains("Check the target."));
        assert_eq!(editor.search, "Search text");
        assert!(!editor.pending_edit());
        store.skills.remove(&id);
        editor.reload_after_rule(&store);
        assert!(editor.form.is_none());
        assert_eq!(editor.search, "Search text");
    }

    #[test]
    fn skill_markdown_chooser_filters_imports_and_discards_cancelled_listings() {
        let profile = crate::snippets::tests::Profile::new();
        let folder = profile.0.canonicalize().unwrap();
        std::fs::create_dir(folder.join("Child")).unwrap();
        for name in ["first skill.MD", "last skill.Markdown", "ignored.txt"] {
            std::fs::write(folder.join(name), "Test instructions.\n").unwrap();
        }

        let mut editor = Editor::default();
        let mut store = Store::default();
        let original = store.clone();
        let folder_input = Message::Chooser(chooser::Message::Folder(
            folder.to_str().unwrap().to_owned(),
        ));
        let go = Message::Chooser(chooser::Message::Go);
        let open = |editor: &mut Editor, store: &mut Store| {
            let _ = editor.update(store, Message::Import, None);
            // This test double avoids enumerating the user's home directory.
            let _ = editor.update(
                store,
                Message::Chooser(chooser::Message::Listed(
                    editor.ticket,
                    Err("Fixture starting folder".into()),
                )),
                None,
            );
            let _ = editor.update(store, folder_input.clone(), None);
            output(editor.update(store, go.clone(), None))
        };
        let cancelled = open(&mut editor, &mut store);
        let _ = editor.update(&mut store, Message::Cancel, None);
        assert_eq!(store, original);
        assert!(!editor.importing());

        let listing = open(&mut editor, &mut store);
        let Message::Chooser(chooser::Message::Listed(ticket, Ok(directory))) = &listing else {
            panic!("Missing filtered Markdown listing");
        };
        assert_eq!(
            directory
                .entries
                .iter()
                .map(|entry| entry.display.as_str())
                .collect::<Vec<_>>(),
            ["Child", "first skill.MD", "last skill.Markdown"],
        );
        let Message::Chooser(chooser::Message::Listed(cancelled_ticket, _)) = &cancelled else {
            panic!("Missing cancelled listing");
        };
        let _ = editor.update(&mut store, cancelled.clone(), None);
        let _ = editor.update(
            &mut store,
            Message::Chooser(chooser::Message::Entry(*cancelled_ticket, 1)),
            None,
        );
        assert!(editor.chooser.as_ref().unwrap().selection().is_empty());
        let _ = editor.update(&mut store, listing.clone(), None);
        for index in [1, 2] {
            let _ = editor.update(
                &mut store,
                Message::Chooser(chooser::Message::Entry(*ticket, index)),
                None,
            );
        }
        assert_eq!(editor.chooser.as_ref().unwrap().selection().len(), 2);
        let result = output(editor.update(&mut store, Message::RunFile, None));
        let _ = editor.update(&mut store, result, None);
        assert_eq!(store.skills.len(), original.skills.len() + 2);
        assert_eq!(editor.form.as_ref().unwrap().skill.name, "last skill");
        assert!(!editor.importing());
        assert!(editor.chooser.is_none());
    }

    #[test]
    fn skill_markdown_batch_import_commits_only_after_every_file_is_read() {
        let profile = crate::snippets::tests::Profile::new();
        let folder = profile.0.canonicalize().unwrap();
        let first = folder.join("first skill.md");
        let invalid = folder.join("invalid.md");
        let last = folder.join("last skill.markdown");
        std::fs::write(&first, "First skill instructions.\n").unwrap();
        std::fs::write(&invalid, [0xff, 0xfe]).unwrap();
        std::fs::write(&last, "Last skill instructions.\n").unwrap();
        let mut editor = Editor::default();
        let mut store = Store::default();
        let original = store.clone();
        let _ = editor.update(
            &mut store,
            Message::Search("No matching skills".into()),
            None,
        );
        let _ = editor.update(&mut store, Message::Import, None);
        let _ = editor.update(&mut store, Message::UseTypedPath, None);
        let submit = |editor: &mut Editor, store: &mut Store, second: &std::path::Path| {
            editor.import_paths = text_editor::Content::new();
            let _ = editor.update(
                store,
                Message::ImportPaths(text_editor::Action::Edit(text_editor::Edit::Paste(
                    std::sync::Arc::new(format!("{}\n\n{}", first.display(), second.display())),
                ))),
                None,
            );
            let result = output(editor.update(store, Message::RunFile, None));
            let _ = editor.update(store, result, None);
        };

        submit(&mut editor, &mut store, &invalid);
        assert_eq!(store, original);
        assert!(editor.file == Some(FileAction::Import));
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap()
                .contains("Markdown file 2")
        );

        submit(&mut editor, &mut store, &last);
        assert_eq!(store.skills.len(), original.skills.len() + 2);
        assert_eq!(editor.form.as_ref().unwrap().skill.name, "last skill");
        assert!(editor.search.is_empty());
        assert!(editor.file.is_none());
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap()
                .contains("2 skills imported")
        );
    }

    #[test]
    fn skill_markdown_batch_import_leaves_the_library_unchanged_when_full() {
        let skill = |name: &str| Skill {
            name: name.into(),
            ..Skill::default()
        };
        let mut editor = Editor::default();
        let mut store = Store {
            skills: (1..super::super::MAX_SKILLS as u64)
                .map(|id| (id, skill("Existing skill")))
                .collect(),
            ..Store::default()
        };
        let original = store.clone();
        editor.finish_import(&mut store, Ok(vec![skill("First"), skill("Second")]));
        assert_eq!(store, original);
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap()
                .contains("library exceeds its size limit")
        );
    }

    #[test]
    fn skill_markdown_batch_export_uses_selected_drafts_and_preserves_existing_files() {
        let profile = crate::snippets::tests::Profile::new();
        let folder = profile.0.canonicalize().unwrap();
        let existing = folder.join("DEMO-TOOL.MD");
        std::fs::write(&existing, "Existing file").unwrap();
        let mut store = Store {
            skills: ["Demo / Tool", "demo-tool", "Unselected"]
                .into_iter()
                .zip(1..)
                .map(|(name, id)| {
                    (
                        id,
                        Skill {
                            name: name.into(),
                            content: "Original instructions.".into(),
                            ..Skill::default()
                        },
                    )
                })
                .collect(),
            ..Store::default()
        };
        let mut editor = Editor::default();
        let _ = editor.update(&mut store, Message::Export, None);
        assert_eq!(editor.export_skills, BTreeSet::from([1, 2, 3]));
        let _ = editor.update(&mut store, Message::ExportAll(false), None);
        assert!(editor.export_skills.is_empty());
        let _ = editor.update(&mut store, Message::Cancel, None);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);

        let _ = editor.update(&mut store, Message::Select(1), None);
        let _ = editor.update(
            &mut store,
            Message::EditContent(text_editor::Action::Edit(text_editor::Edit::Paste(
                std::sync::Arc::new("Updated draft. ".into()),
            ))),
            None,
        );
        let _ = editor.update(&mut store, Message::Export, None);
        assert_eq!(editor.export_skills, BTreeSet::from([1]));
        let _ = editor.update(&mut store, Message::ExportSkill(2, true), None);
        let _ = editor.update(
            &mut store,
            Message::Path(folder.to_str().unwrap().to_owned()),
            None,
        );
        let listing = output(editor.update(&mut store, Message::BrowseExport, None));
        let _ = editor.update(&mut store, listing, None);
        assert_eq!(
            editor.chooser.as_ref().unwrap().export_folder(),
            Some(folder.as_path())
        );
        let result = output(editor.update(&mut store, Message::RunFile, None));
        let _ = editor.update(&mut store, result, None);

        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "Existing file");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 3);
        let first = markdown::read(&folder.join("demo-tool-2.md")).unwrap();
        assert_eq!(first.name, "Demo / Tool");
        assert!(first.content.contains("Updated draft."));
        let second = markdown::read(&folder.join("demo-tool-3.md")).unwrap();
        assert_eq!(second.name, "demo-tool");
        assert_eq!(editor.notice.as_deref(), Some("2 skills exported."));
        assert!(!editor.file_prompt_open());
    }

    #[test]
    fn skill_markdown_grows_and_actions_stay_visible_in_compact_forms() {
        let store = Store::default();
        let (&id, skill) = store.skills.first_key_value().unwrap();
        for show_find in [false, true] {
            let mut form = Form::new(id, skill);
            if show_find {
                let _ = form
                    .find
                    .update::<Message>(&mut form.markdown, document_find::Message::Open);
            }
            let mut view = Editor::form_view(&form, false);
            let mut tree = Tree::new(view.as_widget());
            let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));

            let mut measure = |size| {
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, size),
                );
                let layout = Layout::new(&node);
                let mut sections = layout.children();
                let fields = sections.next().unwrap();
                let actions = sections.next().unwrap();
                let viewport = fields.children().next().unwrap();
                let form = viewport.children().next().unwrap();
                let mut fields = form.children();
                let markdown = fields.nth(6).unwrap().bounds();
                let rules = fields.nth(1).unwrap().bounds();
                (
                    viewport.bounds().height,
                    form.bounds().height,
                    markdown,
                    rules,
                    actions.bounds(),
                )
            };

            let (
                regular_viewport,
                regular_height,
                regular_markdown,
                regular_rules,
                regular_actions,
            ) = measure(Size::new(640.0, 640.0));
            let (tall_viewport, tall_height, tall_markdown, tall_rules, _) =
                measure(Size::new(640.0, 1040.0));
            assert_eq!(regular_height, regular_viewport);
            assert_eq!(tall_height, tall_viewport);
            assert!((tall_markdown.height - regular_markdown.height - 400.0).abs() < 1.0);
            assert_eq!(tall_rules.height, regular_rules.height);
            assert!(tall_markdown.y + tall_markdown.height < tall_rules.y);

            let (viewport_height, form_height, markdown, rules, actions) =
                measure(Size::new(245.0, 180.0));
            assert!(viewport_height > 0.0);
            assert!(form_height > viewport_height);
            assert!(markdown.height >= 85.0);
            assert!(rules.y + rules.height <= form_height);
            assert!(actions.height > regular_actions.height);
            assert!(actions.y >= viewport_height);
            assert!(actions.y + actions.height <= 180.0);
        }
    }
}
