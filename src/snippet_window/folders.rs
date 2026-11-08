//! Folder navigation, dialogs and library actions in the snippet manager.

use std::collections::HashSet;
use std::{fmt, io};

use iced::keyboard::{Key, key::Named};
use iced::widget::{self, button, column, container, pick_list, row, text, text_input};
use iced::{Element, Length, Point};

use crate::snippets::{Library, Snippet, SnippetId, folders::Id};
use crate::workspace::native_popup;

use super::{Draft, Message as WindowMessage, SnippetWindow, Update, archive, recovery, transfer};

pub(super) mod drag;
pub(super) mod view;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    All,
    TopLevel,
    Folder(Id),
}

impl Selection {
    pub(super) fn folder(self) -> Option<Id> {
        match self {
            Self::Folder(id) => Some(id),
            Self::All | Self::TopLevel => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuTarget {
    Folder(Selection),
    Snippet(SnippetId),
}

#[derive(Clone)]
pub enum Message {
    Select(Selection),
    IncludeSubfolders(bool),
    Toggle(Id),
    Expanded(Id, bool),
    ResizeTree(f32),
    NewFolder(Selection),
    Rename(Id),
    MoveFolder(Id),
    MoveSnippets(SnippetId),
    Delete(Id),
    Export(Selection),
    Name(String),
    Destination(Destination),
    Apply,
    KeepContents,
    DeleteContents,
    DeleteConfirmed,
    Cancel,
    Context(Context),
    Drop(drag::Source, Option<Id>),
}

#[derive(Clone)]
pub enum Context {
    Open(MenuTarget, Point),
    Dismiss,
    Step { backwards: bool, count: usize },
}

#[derive(Clone, PartialEq, Eq)]
pub struct Destination {
    id: Option<Id>,
    label: String,
}

impl fmt::Display for Destination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

enum NameTarget {
    New(Option<Id>),
    Rename(Id),
}

enum MoveTarget {
    Folder(Id),
    Snippets(Vec<SnippetId>),
}

enum Form {
    Name {
        target: NameTarget,
        value: String,
    },
    Move {
        target: MoveTarget,
        destination: Option<Id>,
    },
    Delete {
        id: Id,
        snippets: Vec<SnippetId>,
        confirm: bool,
    },
}

struct Page {
    form: Form,
    error: Option<String>,
}

struct Menu {
    target: MenuTarget,
    point: Point,
    selected: usize,
}

pub(super) struct State {
    pub(super) selection: Selection,
    pub(super) include_subfolders: bool,
    pub(super) expanded: HashSet<Id>,
    pub(super) tree_height: Option<f32>,
    name_input: widget::Id,
    page: Option<Page>,
    menu: Option<Menu>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            selection: Selection::All,
            include_subfolders: true,
            expanded: HashSet::new(),
            tree_height: None,
            name_input: widget::Id::unique(),
            page: None,
            menu: None,
        }
    }
}

impl State {
    pub(super) fn modal(&self) -> bool {
        self.page.is_some()
    }

    pub(super) fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    pub(super) fn dismiss_menu(&mut self) {
        self.menu = None;
    }

    pub(super) fn cancel(&mut self) {
        self.page = None;
    }

    fn form(&mut self, form: Form) {
        self.page = Some(Page { form, error: None });
    }

    fn name_form(&mut self, target: NameTarget, value: String) -> Update {
        self.form(Form::Name { target, value });
        Update::FocusFolderName(self.name_input.clone())
    }

    pub(super) fn refresh(&mut self, library: &Library) {
        if self
            .selection
            .folder()
            .is_some_and(|id| library.folder(id).is_none())
        {
            self.selection = Selection::All;
        }
        self.expanded.retain(|&id| library.folder(id).is_some());
    }

    pub(super) fn scope(&self, library: &Library) -> Option<HashSet<SnippetId>> {
        match self.selection {
            Selection::All => None,
            Selection::TopLevel => Some(
                library
                    .snippets_in_folder(None, false)
                    .into_iter()
                    .collect(),
            ),
            Selection::Folder(id) => Some(
                library
                    .snippets_in_folder(Some(id), self.include_subfolders)
                    .into_iter()
                    .collect(),
            ),
        }
    }

    fn select_and_expand(&mut self, library: &Library, id: Id) {
        self.selection = Selection::Folder(id);
        if let Some(selected) = library.folder(id) {
            self.expanded.extend(
                library
                    .folders()
                    .iter()
                    .filter(|folder| {
                        selected
                            .path
                            .strip_prefix(&folder.path)
                            .is_some_and(|tail| tail.starts_with('/'))
                    })
                    .map(|folder| folder.id),
            );
        }
    }

    pub(super) fn show_snippet(&mut self, library: &Library, id: SnippetId) {
        if self
            .scope(library)
            .is_some_and(|scope| !scope.contains(&id))
        {
            self.selection = Selection::All;
        }
    }

    fn actions(
        target: MenuTarget,
        transfer_enabled: bool,
    ) -> Vec<(&'static str, Option<WindowMessage>)> {
        let (folder_actions, transfer_action) = match target {
            MenuTarget::Folder(selection) => {
                let mut actions = vec![("New folder…", Message::NewFolder(selection))];
                if let Selection::Folder(id) = selection {
                    actions.extend([
                        ("Rename folder…", Message::Rename(id)),
                        ("Move folder…", Message::MoveFolder(id)),
                        ("Delete folder…", Message::Delete(id)),
                    ]);
                }
                if selection != Selection::TopLevel {
                    actions.push(("Export folder…", Message::Export(selection)));
                }
                let transfer_action = selection
                    .folder()
                    .map(|id| ("Copy folder to terminal…", transfer::Message::Folder(id)));
                (actions, transfer_action)
            }
            MenuTarget::Snippet(id) => (
                vec![("Move to folder…", Message::MoveSnippets(id))],
                Some(("Copy files to terminal…", transfer::Message::Snippets(id))),
            ),
        };
        let mut actions: Vec<_> = folder_actions
            .into_iter()
            .map(|(label, message)| (label, Some(wrap(message))))
            .collect();
        use crate::snippet_analysis::model::Subject;
        match target {
            MenuTarget::Folder(Selection::Folder(id)) => {
                let subject = Subject::Folder(id);
                actions.extend([
                    (
                        "Analyze folder with AI…",
                        Some(WindowMessage::Analyze(subject)),
                    ),
                    (
                        "Export analysis reports…",
                        Some(WindowMessage::AnalysisReports(Some(subject))),
                    ),
                ]);
            }
            MenuTarget::Folder(Selection::All) => actions.push((
                "Export analysis reports…",
                Some(WindowMessage::AnalysisReports(None)),
            )),
            MenuTarget::Snippet(id) => {
                let subject = Subject::Snippet(id);
                actions.extend([
                    ("Full-code analysis…", Some(WindowMessage::Analyze(subject))),
                    (
                        "Export analysis reports…",
                        Some(WindowMessage::AnalysisReports(Some(subject))),
                    ),
                ]);
            }
            MenuTarget::Folder(Selection::TopLevel) => {}
        }
        if let Some((label, message)) = transfer_action {
            actions.push((
                label,
                transfer_enabled.then_some(WindowMessage::Transfer(message)),
            ));
        }
        actions
    }

    pub(super) fn context<'a>(
        &'a self,
        _library: &'a Library,
        target: MenuTarget,
        transfer_enabled: bool,
        child: Element<'a, WindowMessage>,
    ) -> Element<'a, WindowMessage> {
        let (popup, enabled, activate) = self
            .menu
            .as_ref()
            .filter(|menu| menu.target == target)
            .map(|menu| {
                let actions = Self::actions(target, transfer_enabled);
                // The only optional action is the final transfer entry.
                let enabled = actions
                    .iter()
                    .filter(|(_, action)| action.is_some())
                    .count();
                let selected = menu.selected.min(enabled.saturating_sub(1));
                let activate = actions
                    .get(selected)
                    .and_then(|(_, action)| action.as_ref())
                    .cloned();
                let buttons = actions.into_iter().enumerate().fold(
                    column![].spacing(3),
                    |items, (index, (label, message))| {
                        items.push(
                            button(text(label).size(13))
                                .width(Length::Fill)
                                .style(if index == selected {
                                    button::primary
                                } else {
                                    button::text
                                })
                                .on_press_maybe(message),
                        )
                    },
                );
                (
                    Some((
                        menu.point,
                        container(buttons)
                            .width(200)
                            .padding(5)
                            .style(container::rounded_box)
                            .into(),
                    )),
                    enabled,
                    activate,
                )
            })
            .unwrap_or_default();
        native_popup::context_action_with_keyboard(
            child,
            move |point| wrap(Message::Context(Context::Open(target, point))),
            popup,
            wrap(Message::Context(Context::Dismiss)),
            move |key, _| match key {
                Key::Named(Named::Escape) => Some(wrap(Message::Context(Context::Dismiss))),
                Key::Named(Named::ArrowUp | Named::ArrowDown) => {
                    Some(wrap(Message::Context(Context::Step {
                        backwards: matches!(key, Key::Named(Named::ArrowUp)),
                        count: enabled,
                    })))
                }
                Key::Named(Named::Enter) => activate.clone(),
                _ => None,
            },
        )
    }

    fn update_context(&mut self, event: Context) {
        match event {
            Context::Open(target, point) => {
                self.menu = Some(Menu {
                    target,
                    point,
                    selected: 0,
                })
            }
            Context::Dismiss => self.dismiss_menu(),
            Context::Step { backwards, count } => {
                if let Some(menu) = &mut self.menu
                    && count > 0
                {
                    let selected = menu.selected.min(count - 1);
                    menu.selected = if backwards {
                        selected.checked_sub(1).unwrap_or(count - 1)
                    } else {
                        (selected + 1) % count
                    };
                }
            }
        }
    }

    pub(super) fn page_view<'a>(
        &'a self,
        library: &'a Library,
    ) -> Option<Element<'a, WindowMessage>> {
        let page = self.page.as_ref()?;
        let mut view = match &page.form {
            Form::Name { target, value } => column![
                text(match target {
                    NameTarget::New(_) => "New folder",
                    NameTarget::Rename(_) => "Rename folder",
                })
                .size(20),
                text_input("Folder name", value)
                    .id(self.name_input.clone())
                    .on_input(|value| wrap(Message::Name(value)))
                    .on_submit_maybe((!value.trim().is_empty()).then_some(wrap(Message::Apply))),
                row![
                    button("Cancel").on_press(wrap(Message::Cancel)),
                    button("Save")
                        .on_press_maybe((!value.trim().is_empty()).then_some(wrap(Message::Apply)))
                ]
                .spacing(8),
            ],
            Form::Move {
                target,
                destination,
            } => {
                let choices = destinations(library, target);
                let selected = choices
                    .iter()
                    .find(|choice| choice.id == *destination)
                    .cloned();
                let title = match target {
                    MoveTarget::Folder(_) => "Move folder".into(),
                    MoveTarget::Snippets(ids) => format!("Move {} snippet(s) to folder", ids.len()),
                };
                column![
                    text(title).size(20),
                    pick_list(choices, selected, |choice| wrap(Message::Destination(
                        choice
                    )))
                    .width(Length::Fill),
                    row![
                        button("Cancel").on_press(wrap(Message::Cancel)),
                        button("Move").on_press(wrap(Message::Apply))
                    ]
                    .spacing(8)
                ]
            }
            Form::Delete {
                id,
                snippets,
                confirm,
            } => {
                let name = library
                    .folder(*id)
                    .and_then(|folder| folder.path.rsplit('/').next())
                    .unwrap_or("Folder");
                if *confirm {
                    column![
                        text(format!(
                            "Permanently delete {name} and {} snippet(s)?",
                            snippets.len()
                        ))
                        .size(20),
                        text("Jobs linked to these snippets will be blocked."),
                        row![
                            button("Cancel").on_press(wrap(Message::Cancel)),
                            button("Delete permanently")
                                .style(button::danger)
                                .on_press(wrap(Message::DeleteConfirmed))
                        ]
                        .spacing(8)
                    ]
                } else {
                    column![
                        text(format!("Delete folder {name}?")).size(20),
                        text(format!(
                            "{} snippet(s) in this folder and its subfolders.",
                            snippets.len()
                        )),
                        row![
                            button("Cancel").on_press(wrap(Message::Cancel)),
                            button("Keep contents").on_press(wrap(Message::KeepContents)),
                            button("Delete all contents")
                                .style(button::danger)
                                .on_press(wrap(Message::DeleteContents))
                        ]
                        .spacing(8)
                        .wrap()
                    ]
                }
            }
        };
        if let Some(error) = &page.error {
            view = view.push(text(error));
        }
        let dialog = container(view.spacing(10).width(Length::Fill))
            .style(container::rounded_box)
            .padding(16)
            .max_width(460);
        Some(container(dialog).padding(12).center(Length::Fill).into())
    }
}

fn destinations(library: &Library, target: &MoveTarget) -> Vec<Destination> {
    let blocked = match target {
        MoveTarget::Folder(id) => library.folder(*id),
        MoveTarget::Snippets(_) => None,
    };
    let mut choices: Vec<_> = library
        .folders()
        .iter()
        .filter(|folder| {
            blocked.is_none_or(|blocked| {
                folder.id != blocked.id
                    && !folder
                        .path
                        .strip_prefix(&blocked.path)
                        .is_some_and(|tail| tail.starts_with('/'))
            })
        })
        .map(|folder| Destination {
            id: Some(folder.id),
            label: folder.path.clone(),
        })
        .collect();
    choices.sort_by_cached_key(|choice| choice.label.to_lowercase());
    choices.insert(
        0,
        Destination {
            id: None,
            label: "Top level".into(),
        },
    );
    choices
}

impl SnippetWindow {
    pub(super) fn marked_move_ids(&self, id: SnippetId) -> Vec<SnippetId> {
        if self.marked_snippets.contains(&id) {
            self.marked_snippets.clone()
        } else {
            vec![id]
        }
    }

    pub(super) fn update_folders(&mut self, message: Message) -> Update {
        if !matches!(message, Message::Context(_)) {
            self.folders.dismiss_menu();
        }
        if self.folders.modal() {
            return self.update_folder_page(message);
        }
        match message {
            Message::Select(selection)
                if selection
                    .folder()
                    .is_none_or(|id| self.library.folder(id).is_some()) =>
            {
                self.folders.selection = selection;
                self.search();
                if self.search_target() != self.library_selection {
                    self.library_selection = None;
                }
            }
            Message::IncludeSubfolders(include) => {
                self.folders.include_subfolders = include;
                self.search();
            }
            Message::Toggle(id) => {
                return self
                    .update_folders(Message::Expanded(id, !self.folders.expanded.contains(&id)));
            }
            Message::Expanded(id, expanded) if let Some(folder) = self.library.folder(id) => {
                if expanded {
                    self.folders.expanded.insert(id);
                } else if self.folders.expanded.remove(&id)
                    && self
                        .folders
                        .selection
                        .folder()
                        .and_then(|selected| self.library.folder(selected))
                        .and_then(|selected| selected.path.strip_prefix(&folder.path))
                        .is_some_and(|suffix| suffix.starts_with('/'))
                {
                    return self.update_folders(Message::Select(Selection::Folder(id)));
                }
            }
            Message::ResizeTree(height) if height.is_finite() => {
                self.folders.tree_height = Some(height.max(0.0))
            }
            Message::NewFolder(selection) => {
                return self
                    .folders
                    .name_form(NameTarget::New(selection.folder()), String::new());
            }
            Message::Rename(id) => {
                if let Some(folder) = self.library.folder(id) {
                    let name = folder.path.rsplit('/').next().unwrap_or_default().into();
                    return self.folders.name_form(NameTarget::Rename(id), name);
                }
            }
            Message::MoveFolder(id) => {
                if let Some(folder) = self.library.folder(id) {
                    let parent = folder
                        .path
                        .rsplit_once('/')
                        .and_then(|(path, _)| {
                            self.library
                                .folders()
                                .iter()
                                .find(|entry| entry.path == path)
                        })
                        .map(|folder| folder.id);
                    self.folders.form(Form::Move {
                        target: MoveTarget::Folder(id),
                        destination: parent,
                    });
                }
            }
            Message::MoveSnippets(id) => {
                if let Some(entry) = self.library.get(id) {
                    self.folders.form(Form::Move {
                        target: MoveTarget::Snippets(self.marked_move_ids(id)),
                        destination: self.library.folder_id(entry),
                    });
                }
            }
            Message::Delete(id) if self.library.folder(id).is_some() => {
                self.folders.form(Form::Delete {
                    id,
                    snippets: self.library.snippets_in_folder(Some(id), true),
                    confirm: false,
                });
            }
            Message::Export(selection) if selection != Selection::TopLevel => {
                match archive::Page::new_folder(selection.folder(), &self.library) {
                    Ok(page) => {
                        self.archive = Some(page);
                        return self.load_folder_export_recipients();
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            Message::Drop(source, destination) => {
                let result = match source {
                    drag::Source::Folder(id) => {
                        self.library.move_folder(id, destination).map(|()| {
                            self.folders.select_and_expand(&self.library, id);
                            true
                        })
                    }
                    drag::Source::Snippet(id) => {
                        let ids = self.marked_move_ids(id);
                        self.library.move_snippets_to_folder(&ids, destination)
                    }
                };
                return self.finish_folder_change(result, "Moved");
            }
            Message::Context(event) => self.folders.update_context(event),
            _ => {}
        }
        Update::None
    }

    fn load_folder_export_recipients(&self) -> Update {
        Update::LoadExportRecipients(crate::snippets::recipients::Action::List(
            self.recipient_path.clone(),
        ))
    }

    fn update_folder_page(&mut self, message: Message) -> Update {
        let Some(mut page) = self.folders.page.take() else {
            return Update::None;
        };
        let result = match (&mut page.form, message) {
            (_, Message::Cancel) => return Update::None,
            (Form::Name { value, .. }, Message::Name(name)) if name.len() <= 1024 => {
                *value = name;
                page.error = None;
                Ok(false)
            }
            (Form::Move { destination, .. }, Message::Destination(choice)) => {
                *destination = choice.id;
                page.error = None;
                Ok(false)
            }
            (
                Form::Name {
                    target: NameTarget::New(parent),
                    value,
                },
                Message::Apply,
            ) => self.library.create_folder(*parent, value).map(|id| {
                self.folders.select_and_expand(&self.library, id);
                true
            }),
            (
                Form::Name {
                    target: NameTarget::Rename(id),
                    value,
                },
                Message::Apply,
            ) => self.library.rename_folder(*id, value).map(|()| true),
            (
                Form::Move {
                    target: MoveTarget::Folder(id),
                    destination,
                },
                Message::Apply,
            ) => self.library.move_folder(*id, *destination).map(|()| {
                self.folders.select_and_expand(&self.library, *id);
                true
            }),
            (
                Form::Move {
                    target: MoveTarget::Snippets(ids),
                    destination,
                },
                Message::Apply,
            ) => match self.library.move_snippets_to_folder(ids, *destination) {
                Ok(false) => return Update::None,
                result => result,
            },
            (
                Form::Delete {
                    id, confirm: false, ..
                },
                Message::KeepContents,
            ) => self.library.remove_folder(*id, false).map(|_| true),
            (
                Form::Delete {
                    id,
                    snippets,
                    confirm,
                },
                Message::DeleteContents,
            ) if !*confirm => {
                if snippets.is_empty() {
                    return Update::DeleteFolder(*id, Vec::new());
                }
                *confirm = true;
                Ok(false)
            }
            (
                Form::Delete {
                    id,
                    snippets,
                    confirm: true,
                },
                Message::DeleteConfirmed,
            ) => return Update::DeleteFolder(*id, snippets.clone()),
            _ => Ok(false),
        };
        match result {
            Ok(true) => self.finish_folder_change(Ok(true), "Folder changes saved"),
            Ok(false) => {
                self.folders.page = Some(page);
                Update::None
            }
            Err(error) => {
                page.error = Some(error.to_string());
                self.folders.page = Some(page);
                Update::None
            }
        }
    }

    pub(crate) fn folder_delete_blocker(&self, ids: &[SnippetId]) -> Option<SnippetId> {
        self.tabs.editors(&self.draft).find_map(|draft| {
            let id = draft.id.filter(|id| ids.contains(id))?;
            let details_dirty = self.draft.id == Some(id)
                && self
                    .details
                    .as_ref()
                    .is_some_and(|details| details.changed(&draft.metadata));
            (draft.dirty || details_dirty || self.busy()).then_some(id)
        })
    }

    pub(crate) fn folder_delete_blocked(&mut self, snippet: SnippetId) {
        self.tabs.select_saved(&mut self.draft, snippet);
        self.status =
            Some("Save or discard this snippet's changes before deleting its folder.".into());
    }

    pub(crate) fn remove_snippet_folder(&mut self, id: Id, expected: &[SnippetId]) -> Update {
        if self.library.snippets_in_folder(Some(id), true) != expected {
            self.status = Some("The folder contents changed. Review the deletion again.".into());
            return Update::None;
        }
        let result = self.library.remove_folder(id, true).map(|removed| {
            self.recovery_removals
                .extend(removed.into_iter().map(|key| recovery::Remove {
                    key,
                    revision: None,
                }));
            true
        });
        self.finish_folder_change(result, "Folder and contents deleted")
    }

    fn finish_folder_change(&mut self, result: io::Result<bool>, status: &str) -> Update {
        match result {
            Ok(true) => {
                self.rebase_open_folder_changes();
                self.tabs.refresh(&mut self.draft, &self.library);
                self.folders.refresh(&self.library);
                self.search();
                self.status = Some(status.into());
                Update::Changed
            }
            Ok(false) => Update::None,
            Err(error) => {
                self.status = Some(error.to_string());
                Update::None
            }
        }
    }

    pub(super) fn rebase_open_folder_changes(&mut self) {
        let old_folder = self
            .draft
            .base
            .as_ref()
            .map(|base| base.metadata().folder_path.clone());
        for draft in self.tabs.editors_mut(&mut self.draft) {
            if let Some(saved) = draft.id.and_then(|id| self.library.get(id)) {
                draft.rebase_folder(saved);
            }
        }
        if let (Some(details), Some(old), Some(base)) =
            (&mut self.details, old_folder, &self.draft.base)
        {
            details.rebase_folder(&old, &base.metadata().folder_path);
        }
    }
}

impl Draft {
    fn rebase_folder(&mut self, saved: &Snippet) {
        let Some(base) = &self.base else {
            return;
        };
        let previous = &base.metadata().folder_path;
        let current = &saved.metadata().folder_path;
        if previous == current
            || base.id() != saved.id()
            || base.name() != saved.name()
            || base.language() != saved.language()
            || base.category() != saved.category()
            || base.content() != saved.content()
        {
            return;
        }
        let mut metadata = base.metadata().clone();
        metadata.folder_path.clone_from(current);
        if &metadata != saved.metadata() {
            return;
        }
        if &self.metadata.folder_path == previous {
            self.metadata.folder_path.clone_from(current);
        }
        self.base = Some(saved.clone());
        if self.dirty {
            self.recovery.changed();
        }
    }
}

fn wrap(message: Message) -> WindowMessage {
    WindowMessage::Folders(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::{Input, Metadata, tests::Profile};

    #[test]
    fn folder_filters_combine_scope_recursion_and_text_search() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let parent = library.create_folder(None, "Operations").unwrap();
        library.create_folder(Some(parent), "Unix").unwrap();
        let mut save = |name: &str, folder_path: &str| {
            library
                .save_with_metadata(
                    None,
                    Input {
                        name: name.into(),
                        language: "bash".into(),
                        category: String::new(),
                        content: "echo ok".into(),
                    },
                    Some(Metadata {
                        folder_path: folder_path.into(),
                        ..Default::default()
                    }),
                )
                .unwrap()
        };
        let direct = save("Direct", "Operations");
        let child = save("Child", "Operations/Unix");
        let root = save("Root", "");
        let mut manager = SnippetWindow::open(profile.path()).unwrap();
        let visible = |manager: &SnippetWindow| {
            manager
                .matches
                .iter()
                .map(|&index| manager.library.entries()[index].id())
                .collect::<Vec<_>>()
        };
        assert_eq!(visible(&manager).len(), 3);
        manager.update(wrap(Message::Select(Selection::Folder(parent))));
        assert_eq!(visible(&manager), [child, direct]);
        manager.update(wrap(Message::IncludeSubfolders(false)));
        assert_eq!(visible(&manager), [direct]);
        manager.update(WindowMessage::Search("child".into()));
        assert!(visible(&manager).is_empty());
        manager.update(wrap(Message::IncludeSubfolders(true)));
        assert_eq!(visible(&manager), [child]);
        manager.update(WindowMessage::Search(String::new()));
        manager.update(wrap(Message::Select(Selection::TopLevel)));
        assert_eq!(visible(&manager), [root]);
        assert!(manager.reveal_saved(child));
        assert_eq!(manager.library_selection, Some(child));
        assert_eq!(visible(&manager).len(), 3);
    }
}
