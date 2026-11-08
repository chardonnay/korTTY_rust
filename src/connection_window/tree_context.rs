//! Context actions for a row in the Connection Manager tree.

use iced::keyboard::{Key, key::Named};
use iced::widget::{
    button, column, container, mouse_area, row, rule, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Point, Task, Theme};

use crate::connection_store::{Connection, OsBadge};
use crate::workspace::native_popup;

use super::os_badge::Badge;
use super::selection::RowKey;
use super::{ConnectionWindow, FolderDialog, Message as ConnectionMessage, Persisted};

const TAG_INPUT: &str = "connection-tree-tag";
const MAX_TAG_LENGTH: usize = 96;

#[derive(Clone, Debug)]
pub enum Message {
    Dismiss,
    Navigate(Direction),
    Hover(Page, usize),
    Activate(Page, usize),
    Tag(String),
    SaveTag,
}

#[derive(Clone, Copy, Debug)]
pub enum Direction {
    Previous,
    Next,
    Left,
    Right,
    Activate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Page {
    Root,
    OsSymbols,
    BadgeFamily(String),
}

#[derive(Default)]
pub(super) struct TreeContext {
    menu: Option<Menu>,
    prompt: Option<TagPrompt>,
}

impl TreeContext {
    pub(super) fn open(&mut self, row: RowKey, point: Point) {
        self.menu = Some(Menu {
            row,
            point,
            levels: vec![Level::new(Page::Root)],
        });
    }

    pub(super) fn dismiss(&mut self) {
        self.menu = None;
    }

    pub(super) fn has_prompt(&self) -> bool {
        self.prompt.is_some()
    }

    pub(super) fn has_unsaved_prompt(&self) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|prompt| prompt.tag.trim() != prompt.original.as_deref().unwrap_or(""))
    }

    pub(super) fn clear_prompt(&mut self) {
        self.prompt = None;
    }
}

struct Menu {
    row: RowKey,
    point: Point,
    levels: Vec<Level>,
}

impl Menu {
    fn show(&mut self, level: usize, item: usize, page: Option<Page>) {
        self.levels[level].selected = Some(item);
        if self.levels.get(level + 1).map(|entry| &entry.page) == page.as_ref() {
            return;
        }
        self.levels.truncate(level + 1);
        if let Some(page) = page {
            self.levels.push(Level::new(page));
        }
    }
}

struct Level {
    page: Page,
    selected: Option<usize>,
}

impl Level {
    fn new(page: Page) -> Self {
        Self {
            page,
            selected: None,
        }
    }
}

struct TagPrompt {
    ids: Vec<u64>,
    tag: String,
    original: Option<String>,
}

impl TagPrompt {
    fn from_targets(targets: &[&Connection]) -> Option<Self> {
        let first = &targets.first()?.tag;
        let original = (!first.is_empty() && targets.iter().all(|entry| entry.tag == *first))
            .then(|| first.clone());
        Some(Self {
            ids: targets.iter().map(|entry| entry.id).collect(),
            tag: original.clone().unwrap_or_default(),
            original,
        })
    }

    fn can_save(&self) -> bool {
        self.original.is_some() || !self.tag.trim().is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Action {
    Open(Page),
    Edit,
    Favorite,
    Export,
    Overview,
    AssignTag,
    RemoveTag,
    Delete,
    NewRootFolder,
    NewConnection,
    RenameFolder,
    CreateSubfolder,
    FolderColor,
    ExportFolder,
    DeleteFolder,
    Badge(Option<String>),
    ManageBadges,
}

enum Entry<'a> {
    Item {
        label: &'a str,
        action: Action,
        enabled: bool,
        badge: Option<Badge<'a>>,
    },
    Separator,
}

impl<'a> Entry<'a> {
    fn item(label: &'a str, action: Action, enabled: bool) -> Self {
        Self::Item {
            label,
            action,
            enabled,
            badge: None,
        }
    }

    fn enabled(&self) -> bool {
        matches!(self, Self::Item { enabled: true, .. })
    }

    fn action(&self) -> Option<&Action> {
        match self {
            Self::Item {
                action,
                enabled: true,
                ..
            } => Some(action),
            _ => None,
        }
    }
}

impl ConnectionWindow {
    pub(super) fn tree_context_view<'a>(
        &'a self,
        row: &RowKey,
        base: Element<'a, ConnectionMessage>,
    ) -> Element<'a, ConnectionMessage> {
        if !self.can_browse() {
            return base;
        }
        let anchor = row.clone();
        let popup = self
            .tree_context
            .menu
            .as_ref()
            .filter(|menu| &menu.row == row)
            .map(|menu| (menu.point, self.tree_context_page(menu, 0)));
        native_popup::context_action_with_keyboard(
            base,
            move |point| ConnectionMessage::OpenTreeContext(anchor.clone(), point),
            popup,
            ConnectionMessage::TreeContext(Message::Dismiss),
            |key, _| keyboard(key).map(ConnectionMessage::TreeContext),
        )
    }

    fn tree_context_page<'a>(
        &'a self,
        menu: &'a Menu,
        depth: usize,
    ) -> Element<'a, ConnectionMessage> {
        let level = &menu.levels[depth];
        let entries = self.tree_context_entries(&menu.row, &level.page);
        let mut items = column![].spacing(2).width(Length::Fill);
        for (index, entry) in entries.into_iter().enumerate() {
            let item = match entry {
                Entry::Separator => rule::horizontal(1).into(),
                Entry::Item {
                    label,
                    action,
                    enabled,
                    badge,
                } => {
                    let branch = match action {
                        Action::Open(page) => Some(page),
                        _ => None,
                    };
                    let mut content = row![].spacing(8).align_y(Alignment::Center);
                    if let Some(badge) = badge {
                        content = content.push(badge.image(16));
                    }
                    content = content.push(text(label).size(14).width(Length::Fill));
                    if branch.is_some() {
                        content = content.push(text("›").size(16));
                    }
                    let selected = level.selected == Some(index);
                    let button = button(content)
                        .width(Length::Fill)
                        .padding([4, 7])
                        .style(if selected {
                            button::primary
                        } else {
                            button::text
                        })
                        .on_press_maybe(enabled.then(|| {
                            ConnectionMessage::TreeContext(Message::Activate(
                                level.page.clone(),
                                index,
                            ))
                        }));
                    let trigger: Element<'_, _> = mouse_area(button)
                        .on_enter(ConnectionMessage::TreeContext(Message::Hover(
                            level.page.clone(),
                            index,
                        )))
                        .into();
                    if enabled
                        && selected
                        && branch.as_ref() == menu.levels.get(depth + 1).map(|child| &child.page)
                        && branch.is_some()
                    {
                        native_popup::submenu(trigger, self.tree_context_page(menu, depth + 1))
                    } else {
                        trigger
                    }
                }
            };
            items = items.push(item);
        }
        container(scrollable(items).height(Length::Shrink))
            .style(menu_style)
            .padding(4)
            .width(225)
            .into()
    }

    fn tree_context_entries<'a>(&'a self, row: &RowKey, page: &Page) -> Vec<Entry<'a>> {
        use Action as A;
        use Entry::Separator;

        match page {
            Page::Root => match row {
                RowKey::Connection(_) => {
                    let ids = self.selected_connection_ids();
                    let any = !ids.is_empty();
                    let tagged = ids.iter().any(|id| {
                        self.store
                            .get(*id)
                            .is_some_and(|entry| !entry.tag.is_empty())
                    });
                    vec![
                        Entry::item("Edit", A::Edit, ids.len() == 1),
                        Entry::item("Favorite on/off", A::Favorite, any),
                        Entry::item("OS symbol", A::Open(Page::OsSymbols), any),
                        Entry::item("Export", A::Export, any),
                        Entry::item("Export overview…", A::Overview, any),
                        Separator,
                        Entry::item("Assign Tag", A::AssignTag, any),
                        Entry::item("Remove Tag", A::RemoveTag, tagged),
                        Separator,
                        Entry::item("Delete", A::Delete, any),
                    ]
                }
                RowKey::Group(path) => {
                    let exists = self.store.has_group(path);
                    let nonempty = self.store.in_group(path).next().is_some();
                    vec![
                        Entry::item("Rename Folder", A::RenameFolder, exists),
                        Entry::item("Create Subfolder", A::CreateSubfolder, exists),
                        Separator,
                        Entry::item("Tab Color…", A::FolderColor, exists),
                        Separator,
                        Entry::item("Assign Tag", A::AssignTag, exists),
                        Entry::item("Remove Tag", A::RemoveTag, exists),
                        Separator,
                        Entry::item("Export Folder", A::ExportFolder, nonempty),
                        Entry::item("Export overview…", A::Overview, nonempty),
                        Entry::item("Delete Folder", A::DeleteFolder, exists),
                    ]
                }
                RowKey::Empty => vec![
                    Entry::item("New Folder", A::NewRootFolder, true),
                    Entry::item("New Connection", A::NewConnection, true),
                ],
            },
            Page::OsSymbols => {
                let mut entries = vec![Entry::item("No symbol", A::Badge(None), true), Separator];
                let mut families = Vec::new();
                for (family, ..) in self.tree_badges() {
                    if !families.contains(&family) {
                        families.push(family);
                        entries.push(Entry::item(
                            family,
                            A::Open(Page::BadgeFamily(family.into())),
                            true,
                        ));
                    }
                }
                entries.push(Separator);
                entries.push(Entry::item("Your own symbols…", A::ManageBadges, true));
                entries
            }
            Page::BadgeFamily(family) => self
                .tree_badges()
                .filter(|(current, ..)| current == family)
                .map(|(_, label, id, badge)| Entry::Item {
                    label,
                    action: A::Badge(Some(id.to_owned())),
                    enabled: true,
                    badge: Some(badge),
                })
                .collect(),
        }
    }

    fn tree_badges(&self) -> impl Iterator<Item = (&str, &str, &str, Badge<'_>)> {
        OsBadge::all()
            .map(|badge| {
                (
                    badge.family(),
                    badge.name(),
                    badge.id(),
                    Badge::BuiltIn(badge),
                )
            })
            .chain(
                self.custom_badges
                    .entries()
                    .iter()
                    .filter(|badge| !badge.png().is_empty())
                    .map(|badge| {
                        (
                            badge.family(),
                            badge.name(),
                            badge.reference(),
                            Badge::Custom(badge),
                        )
                    }),
            )
    }

    pub(super) fn tree_context_prompt(&self) -> Option<Element<'_, ConnectionMessage>> {
        let prompt = self.tree_context.prompt.as_ref()?;
        let apply = prompt
            .can_save()
            .then_some(ConnectionMessage::TreeContext(Message::SaveTag));
        Some(
            column![
                text("Assign Tag").size(21),
                text(format!(
                    "Assign a tag to {} connection(s)",
                    prompt.ids.len()
                ))
                .size(14),
                text_input("Tag", &prompt.tag)
                    .id(TAG_INPUT)
                    .on_input(|tag| ConnectionMessage::TreeContext(Message::Tag(tag)))
                    .on_submit_maybe(apply.clone()),
                row![
                    space::horizontal(),
                    button("Cancel")
                        .style(button::secondary)
                        .on_press(ConnectionMessage::TreeContext(Message::Dismiss)),
                    button("Apply").on_press_maybe(apply),
                ]
                .spacing(8),
            ]
            .spacing(10)
            .into(),
        )
    }

    pub(super) fn update_tree_context(&mut self, message: Message) -> Task<ConnectionMessage> {
        match message {
            Message::Dismiss => {
                self.tree_context.dismiss();
                self.tree_context.prompt = None;
            }
            Message::Tag(tag) if tag.len() <= MAX_TAG_LENGTH => {
                if let Some(prompt) = self.tree_context.prompt.as_mut() {
                    prompt.tag = tag;
                }
            }
            Message::SaveTag => {
                if let Some(prompt) = self
                    .tree_context
                    .prompt
                    .as_ref()
                    .filter(|prompt| prompt.can_save())
                {
                    return self.persist_tree_tag(prompt.ids.clone(), prompt.tag.clone());
                }
            }
            Message::Navigate(direction) if self.can_browse() => {
                return self.navigate_tree_context(direction);
            }
            Message::Hover(page, index) if self.can_browse() => {
                return self.choose_tree_context(page, index, false);
            }
            Message::Activate(page, index) if self.can_browse() => {
                return self.choose_tree_context(page, index, true);
            }
            _ => {}
        }
        Task::none()
    }

    fn navigate_tree_context(&mut self, direction: Direction) -> Task<ConnectionMessage> {
        let Some(menu) = self.tree_context.menu.as_ref() else {
            return Task::none();
        };
        let depth = menu.levels.len() - 1;
        let level = &menu.levels[depth];
        let entries = self.tree_context_entries(&menu.row, &level.page);
        match direction {
            Direction::Previous | Direction::Next => {
                let next = adjacent(&entries, level.selected, direction);
                if let Some(menu) = &mut self.tree_context.menu {
                    menu.levels[depth].selected = next;
                }
            }
            Direction::Left => {
                if depth > 0
                    && let Some(menu) = &mut self.tree_context.menu
                {
                    menu.levels.pop();
                }
            }
            Direction::Right | Direction::Activate => {
                if let Some(index) = level.selected
                    && (matches!(direction, Direction::Activate)
                        || matches!(entries[index].action(), Some(Action::Open(_))))
                {
                    return self.choose_tree_context(level.page.clone(), index, true);
                }
            }
        }
        Task::none()
    }

    fn choose_tree_context(
        &mut self,
        page: Page,
        index: usize,
        activate: bool,
    ) -> Task<ConnectionMessage> {
        let Some(menu) = self.tree_context.menu.as_ref() else {
            return Task::none();
        };
        let Some(depth) = menu.levels.iter().position(|level| level.page == page) else {
            return Task::none();
        };
        let entries = self.tree_context_entries(&menu.row, &page);
        let action = entries.get(index).and_then(Entry::action).cloned();
        let branch = match &action {
            Some(Action::Open(child)) => Some(child.clone()),
            _ => None,
        };
        if let Some(action) = action.filter(|_| activate && branch.is_none()) {
            let row = menu.row.clone();
            self.tree_context.dismiss();
            return self.apply_tree_context(row, action);
        }
        if let Some(menu) = &mut self.tree_context.menu {
            menu.show(depth, index, branch);
        }
        Task::none()
    }

    fn apply_tree_context(&mut self, row: RowKey, action: Action) -> Task<ConnectionMessage> {
        let group = match &row {
            RowKey::Group(path) => Some(path.as_str()),
            _ => None,
        };
        match action {
            Action::Edit => self.update(ConnectionMessage::EditSelected),
            Action::Favorite => self.update(ConnectionMessage::ToggleFavorite),
            Action::Delete => self.update(ConnectionMessage::Delete),
            Action::NewConnection => self.update(ConnectionMessage::New),
            Action::NewRootFolder => {
                self.folder = Some(FolderDialog::Create {
                    parent: None,
                    name: String::new(),
                });
                self.notice = None;
                Task::none()
            }
            Action::RenameFolder
            | Action::CreateSubfolder
            | Action::FolderColor
            | Action::DeleteFolder => {
                if !group.is_some_and(|path| self.select_context_group(path)) {
                    return Task::none();
                }
                self.update(match action {
                    Action::RenameFolder => ConnectionMessage::RenameGroup,
                    Action::CreateSubfolder => ConnectionMessage::NewGroup,
                    Action::FolderColor => ConnectionMessage::EditGroupColor,
                    _ => ConnectionMessage::DeleteGroup,
                })
            }
            Action::Export | Action::Overview | Action::ExportFolder => {
                if group.is_some_and(|path| !self.select_context_group(path)) {
                    return Task::none();
                }
                Task::done(if matches!(action, Action::Overview) {
                    ConnectionMessage::ExportOverview
                } else {
                    ConnectionMessage::ExportConnections
                })
            }
            Action::AssignTag | Action::RemoveTag => self.apply_tree_tag(group, action),
            Action::Badge(badge) => {
                let ids = self.selected_connection_ids();
                if ids.is_empty() || !self.valid_os_badge(badge.as_deref()) {
                    return Task::none();
                }
                self.persist(move |mut store| {
                    store.set_os_badges(&ids, badge.as_deref())?;
                    Ok(Persisted::preserving_selection(store, "OS symbol saved."))
                })
            }
            Action::ManageBadges => Task::done(ConnectionMessage::ManageOsBadges),
            Action::Open(_) => Task::none(),
        }
    }

    fn apply_tree_tag(&mut self, group: Option<&str>, action: Action) -> Task<ConnectionMessage> {
        let targets: Vec<_> = match group {
            Some(path) => self.store.in_group(path).collect(),
            None => self
                .selected_connection_ids()
                .iter()
                .filter_map(|id| self.store.get(*id))
                .collect(),
        };
        if targets.is_empty() {
            self.notice = Some("The folder has no connections.".into());
            return Task::none();
        }
        if action == Action::AssignTag {
            self.tree_context.prompt = TagPrompt::from_targets(&targets);
            self.notice = None;
            return iced::widget::operation::focus(TAG_INPUT);
        }
        let ids: Vec<_> = targets
            .iter()
            .filter(|entry| !entry.tag.is_empty())
            .map(|entry| entry.id)
            .collect();
        if ids.is_empty() {
            return Task::none();
        }
        self.persist_tree_tag(ids, String::new())
    }

    fn persist_tree_tag(&mut self, ids: Vec<u64>, tag: String) -> Task<ConnectionMessage> {
        self.persist(move |mut store| {
            let tag = tag.trim();
            store.set_connection_tags(&ids, tag)?;
            let status = if tag.is_empty() {
                "Tag removed."
            } else {
                "Tag saved."
            };
            Ok(Persisted::preserving_selection(store, status))
        })
    }
}

fn adjacent(entries: &[Entry<'_>], selected: Option<usize>, direction: Direction) -> Option<usize> {
    if matches!(direction, Direction::Previous) {
        let before = selected.unwrap_or(entries.len());
        (0..before)
            .rev()
            .chain((before..entries.len()).rev())
            .find(|index| entries[*index].enabled())
    } else {
        let after = selected.map_or(0, |index| index + 1);
        (after..entries.len())
            .chain(0..after)
            .find(|index| entries[*index].enabled())
    }
}

fn keyboard(key: &Key) -> Option<Message> {
    let direction = match key {
        Key::Named(Named::Escape) => return Some(Message::Dismiss),
        Key::Named(Named::ArrowUp) => Direction::Previous,
        Key::Named(Named::ArrowDown) => Direction::Next,
        Key::Named(Named::ArrowLeft) => Direction::Left,
        Key::Named(Named::ArrowRight) => Direction::Right,
        Key::Named(Named::Enter) => Direction::Activate,
        _ => return None,
    };
    Some(Message::Navigate(direction))
}

fn menu_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style::default()
        .background(palette.background.base.color)
        .border(
            iced::border::rounded(5)
                .color(palette.background.strong.color)
                .width(1),
        )
}
