//! Native split-view skill editor with explicit file operations and library drafts.

use std::fmt;
use std::path::PathBuf;

use iced::widget::{
    button, checkbox, column, container, pick_list, responsive, row, scrollable, text, text_editor,
    text_input,
};
use iced::{Alignment, Element, Font, Length, Task};

use crate::local_ai;

use super::{MAX_SKILL_BYTES, Skill, Store, Target, markdown, shorten};

#[derive(Clone)]
pub(crate) enum Message {
    Search(String),
    ShowHidden(bool),
    Sort(Sort),
    EnableLibrary(bool),
    AutoDetect(bool),
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
    EditRules(text_editor::Action),
    EditTarget(Target),
    EnableSkill(bool),
    Commit,
    Revert,
    Import,
    Export,
    Path(String),
    RunFile,
    Imported(Result<Box<Skill>, String>),
    Exported(Result<(), String>),
    Shorten,
    Shortened(u64, Result<Box<shorten::Proposal>, String>),
    AcceptShortening,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AiSkillMessage([redacted])")
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
    form: Option<Form>,
    confirmation: Option<Confirmation>,
    file: Option<FileAction>,
    path: String,
    in_flight: bool,
    ticket: u64,
    shortening: Option<(u64, local_ai::Cancel)>,
    proposal: Option<shorten::Proposal>,
    notice: Option<String>,
}

struct Form {
    id: u64,
    skill: Skill,
    tags: String,
    markdown: text_editor::Content,
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
    pub(in crate::ai_manager) fn busy(&self) -> bool {
        self.in_flight || self.shortening.is_some()
    }

    pub(in crate::ai_manager) fn pending_edit(&self) -> bool {
        self.form.as_ref().is_some_and(|form| form.dirty)
    }

    pub(in crate::ai_manager) fn commit(&mut self, store: &mut Store) -> Result<(), String> {
        if let Some(form) = self.form.as_mut().filter(|form| form.dirty) {
            store.replace(form.id, form.snapshot())?;
            form.dirty = false;
        }
        Ok(())
    }

    pub(in crate::ai_manager) fn update(
        &mut self,
        store: &mut Store,
        message: Message,
        profile: Option<local_ai::Profile>,
    ) -> Task<Message> {
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
                    self.finish_import(store, result.map(|skill| *skill));
                }
                Message::Exported(result) if self.file == Some(FileAction::Export) => {
                    self.in_flight = false;
                    self.finish_file(result, "Skill exported.");
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
                Message::Path(path) => self.path = bounded(path, 4096),
                Message::RunFile => return self.run_file(store),
                Message::Cancel => self.file = None,
                _ => {}
            }
            return Task::none();
        }
        match message {
            Message::Search(value) => self.search = bounded(value, 256),
            Message::ShowHidden(value) => self.show_hidden = value,
            Message::Sort(sort) => self.sort = sort,
            Message::EnableLibrary(value) => store.enabled = value,
            Message::AutoDetect(true) => store.auto_detect = true,
            Message::AutoDetect(false) if store.auto_detect => {
                self.confirmation = Some(Confirmation::DisableAutoDetection);
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
            Message::Import | Message::Export if self.commit_or_notice(store) => {
                self.file = if matches!(message, Message::Import) {
                    Some(FileAction::Import)
                } else if self.form.is_some() {
                    Some(FileAction::Export)
                } else {
                    None
                };
                self.path.clear();
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

    fn run_file(&mut self, store: &Store) -> Task<Message> {
        let path = PathBuf::from(self.path.trim());
        if !path.is_absolute() {
            self.notice = Some("Enter an absolute Markdown file path.".into());
            return Task::none();
        }
        self.notice = None;
        match self.file {
            Some(FileAction::Import) => {
                self.in_flight = true;
                Task::future(async move { Message::Imported(markdown::read(&path).map(Box::new)) })
            }
            Some(FileAction::Export) => {
                let Some(skill) = self
                    .form
                    .as_ref()
                    .and_then(|form| store.skills.get(&form.id))
                    .cloned()
                else {
                    return Task::none();
                };
                let mut path = path;
                if path.extension().is_none() {
                    path.set_extension("md");
                }
                self.in_flight = true;
                Task::future(async move { Message::Exported(markdown::write(&path, &skill)) })
            }
            None => Task::none(),
        }
    }

    fn finish_import(&mut self, store: &mut Store, result: Result<Skill, String>) {
        match result.and_then(|skill| store.insert(skill)) {
            Ok(id) => {
                self.search.clear();
                self.select(store, id);
                self.finish_file(Ok(()), "Skill imported into the library draft.");
            }
            Err(error) => self.finish_file(Err(error), ""),
        }
    }

    fn finish_file(&mut self, result: Result<(), String>, success: &str) {
        self.notice = Some(match result {
            Ok(()) => {
                self.file = None;
                success.to_owned()
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
        let header = row![
            checkbox(store.enabled)
                .label("Enable AI skills")
                .on_toggle(Message::EnableLibrary),
            checkbox(store.auto_detect)
                .label("Automatically select relevant skills")
                .on_toggle(Message::AutoDetect),
            checkbox(self.show_hidden)
                .label("Show hidden")
                .on_toggle(Message::ShowHidden)
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
                button("Export Markdown…").on_press_maybe(selected.map(|_| Message::Export))
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
        } else if let Some(action) = self.file {
            self.file_view(action)
        } else if let Some(form) = &self.form {
            self.form_view(form, shorten_available)
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
            layout,
            self.notice.as_deref().map(|notice| text(notice).size(13))
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    }

    fn file_view(&self, action: FileAction) -> Element<'_, Message> {
        let (title, label, description) = match action {
            FileAction::Import => (
                "Import Markdown",
                "Import",
                "Enter the absolute path of a local Markdown skill.",
            ),
            FileAction::Export => (
                "Export Markdown",
                "Export",
                "Enter the absolute destination path. Existing files are preserved.",
            ),
        };
        column![
            text(title).size(19),
            text(description).size(13),
            text_input("Absolute Markdown file path", &self.path)
                .on_input_maybe((!self.in_flight).then_some(Message::Path)),
            row![
                button(label).on_press_maybe((!self.in_flight).then_some(Message::RunFile)),
                button("Cancel").on_press_maybe((!self.in_flight).then_some(Message::Cancel))
            ]
            .spacing(6),
        ]
        .spacing(8)
        .into()
    }

    fn form_view<'a>(&'a self, form: &'a Form, shorten_available: bool) -> Element<'a, Message> {
        responsive(move |size| {
            // Give the form a finite height inside the scrollable so its editor can fill
            // the remaining space. A minimum keeps the fields usable in compact tabs.
            self.form_fields(form, shorten_available, size.height.max(520.0))
        })
        .into()
    }

    fn form_fields<'a>(
        &self,
        form: &'a Form,
        shorten_available: bool,
        height: f32,
    ) -> Element<'a, Message> {
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
                pick_list(Target::ALL, Some(skill.target), Message::EditTarget)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text("Skill Markdown").size(13),
            text_editor(&form.markdown)
                .height(Length::Fill)
                .font(Font::MONOSPACE)
                .on_action(Message::EditContent),
            text("Learned rules · one per line").size(13),
            text_editor(&form.rules)
                .height(85)
                .on_action(Message::EditRules),
            row![
                button("Apply skill edits").on_press_maybe(form.dirty.then_some(Message::Commit)),
                button("Revert edits").on_press_maybe(form.dirty.then_some(Message::Revert)),
                button("Shorten with local AI…").on_press_maybe(
                    (shorten_available && (form.dirty || shorten::available(skill)))
                        .then_some(Message::Shorten)
                )
            ]
            .spacing(6)
            .wrap(),
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
    use iced::{Pixels, Renderer, Size};

    use super::*;

    #[test]
    fn skill_markdown_grows_with_the_form_and_compact_forms_remain_scrollable() {
        let store = Store::default();
        let (&id, skill) = store.skills.first_key_value().unwrap();
        let editor = Editor::default();
        let form = Form::new(id, skill);
        let mut view = editor.form_view(&form, false);
        let mut tree = Tree::new(view.as_widget());
        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));

        let mut measure = |size| {
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let viewport = Layout::new(&node).children().next().unwrap();
            let form = viewport.children().next().unwrap();
            let mut fields = form.children();
            let markdown = fields.nth(6).unwrap().bounds();
            let rules = fields.nth(1).unwrap().bounds();
            (
                viewport.bounds().height,
                form.bounds().height,
                markdown,
                rules,
            )
        };

        let (_, regular_height, regular_markdown, regular_rules) = measure(Size::new(640.0, 600.0));
        let (_, tall_height, tall_markdown, tall_rules) = measure(Size::new(640.0, 1000.0));
        assert_eq!(regular_height, 600.0);
        assert_eq!(tall_height, 1000.0);
        assert!((tall_markdown.height - regular_markdown.height - 400.0).abs() < 1.0);
        assert_eq!(tall_rules.height, regular_rules.height);
        assert!(tall_markdown.y + tall_markdown.height < tall_rules.y);

        let (viewport_height, form_height, markdown, rules) = measure(Size::new(245.0, 180.0));
        assert_eq!(viewport_height, 180.0);
        assert!(form_height > viewport_height);
        assert!(markdown.height >= 85.0);
        assert!(rules.y + rules.height < form_height);
    }
}
