use std::cell::Ref;

use iced::widget::{Column, button, column, row, space, text};
use iced::{Alignment, Length, Task, window};

use crate::workspace::open_recent::Entry;

use super::{Branch, Choice, ChromeColors, Message, OpenMenu, button_style, divider, wrap};
use super::{Workspace, WorkspaceMessage};

const EMPTY: &str = "No recent connections or projects";
const CLEAR: &str = "Clear List";

#[derive(Default)]
pub(super) struct Rows {
    entries: Vec<(u64, Entry)>,
    clear: Option<u64>,
    sequence: u64,
}

#[derive(Debug, Eq, PartialEq)]
enum Row<'a> {
    Heading(&'a str),
    Item(u64, &'a str),
    Separator,
}

enum Selection {
    Item(Entry),
    Clear,
}

impl Rows {
    fn update(&mut self, entries: Vec<Entry>) {
        if self.entries.iter().map(|(_, entry)| entry).eq(&entries) {
            return;
        }
        // A changed snapshot gets fresh tokens. A delayed action cannot
        // select the new item at an old index or clear a newer list.
        self.entries = entries
            .into_iter()
            .map(|entry| (next_token(&mut self.sequence), entry))
            .collect();
        self.clear = (!self.entries.is_empty()).then(|| next_token(&mut self.sequence));
    }

    fn layout(&self) -> Vec<Row<'_>> {
        if self.entries.is_empty() {
            return vec![Row::Heading(EMPTY)];
        }
        let mut rows = Vec::new();
        for (connection, title) in [(true, "Connections"), (false, "Projects")] {
            let mut entries = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.is_connection() == connection)
                .peekable();
            if entries.peek().is_some() {
                if !rows.is_empty() {
                    rows.push(Row::Separator);
                }
                rows.push(Row::Heading(title));
                rows.extend(entries.map(|(id, entry)| Row::Item(*id, entry.label())));
            }
        }
        if let Some(clear) = self.clear {
            rows.extend([Row::Separator, Row::Item(clear, CLEAR)]);
        }
        rows
    }

    fn select(&self, id: u64) -> Option<Selection> {
        if self.clear == Some(id) {
            return Some(Selection::Clear);
        }
        self.entries
            .iter()
            .find(|(token, _)| *token == id)
            .map(|(_, entry)| Selection::Item(entry.clone()))
    }
}

fn next_token(sequence: &mut u64) -> u64 {
    *sequence = sequence
        .checked_add(1)
        .expect("exhausted Open Recent menu IDs");
    *sequence
}

impl Workspace {
    fn open_recent_rows(&self) -> Ref<'_, Rows> {
        let entries = self.open_recent_entries();
        let menu = &self.chrome.open_recent;
        menu.borrow_mut().update(entries);
        menu.borrow()
    }

    pub(super) fn open_recent_choices(&self, owner: window::Id) -> Vec<Choice> {
        if !self.palette_can_open(owner) {
            return Vec::new();
        }
        self.open_recent_rows()
            .layout()
            .into_iter()
            .filter_map(|row| match row {
                Row::Item(id, _) => Some(Choice::OpenRecent(id)),
                Row::Heading(_) | Row::Separator => None,
            })
            .collect()
    }

    pub(super) fn open_recent_menu_rows(
        &self,
        menu: OpenMenu,
        colors: ChromeColors,
    ) -> Column<'_, WorkspaceMessage> {
        let enabled = self.palette_can_open(menu.window);
        let button_row = |title: &str, id: Option<u64>| {
            let action = id.filter(|_| enabled);
            let selected = action.is_some_and(|id| menu.selected == Some(Choice::OpenRecent(id)));
            button(
                row![
                    space::horizontal().width(15),
                    text(title.to_owned()).size(13)
                ]
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([5, 6])
            .style(move |_, status| button_style(status, colors.menu_hover, selected, colors))
            .on_press_maybe(action.map(|id| wrap(Message::OpenRecent(menu.window, id))))
        };
        self.open_recent_rows()
            .layout()
            .into_iter()
            .fold(column![].spacing(2), |rows, row| match row {
                Row::Heading(title) => rows.push(button_row(title, None)),
                Row::Item(id, title) => rows.push(button_row(title, Some(id))),
                Row::Separator => rows.push(divider(colors.menu_hover)),
            })
    }

    pub(super) fn invoke_open_recent(
        &mut self,
        owner: window::Id,
        id: u64,
    ) -> Task<WorkspaceMessage> {
        if !self
            .chrome
            .menu
            .is_some_and(|menu| menu.window == owner && menu.branch == Some(Branch::OpenRecent))
            || !self.menu_surface_available(owner, super::Category::File)
            || !self.palette_can_open(owner)
        {
            return Task::none();
        }
        let selection = self.open_recent_rows().select(id);
        match selection {
            Some(selection) => {
                self.chrome.dismiss(owner);
                match selection {
                    Selection::Item(entry) => self.open_recent_item(owner, entry),
                    Selection::Clear => self.clear_open_recent(Some(owner)),
                }
            }
            None => Task::none(),
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn sync_native_open_recent(&self, enabled: bool) {
        use kortty_macos::OpenRecentMenuRow;

        let menu = self.open_recent_rows();
        let rows: Vec<_> = menu
            .layout()
            .into_iter()
            .map(|row| match row {
                Row::Heading(title) => OpenRecentMenuRow::Heading(title),
                Row::Item(id, title) => OpenRecentMenuRow::Item { id, title },
                Row::Separator => OpenRecentMenuRow::Separator,
            })
            .collect();
        kortty_macos::sync_open_recent_menu(&rows, enabled);
    }

    #[cfg(target_os = "macos")]
    pub(super) fn native_open_recent_available(
        &self,
        active: Option<(window::Id, &super::Window)>,
    ) -> bool {
        self.chrome.dialog.is_none()
            && active.is_none_or(|(owner, window)| {
                self.palette_can_open(owner) && !self.native_menu_blocked(window)
            })
    }

    #[cfg(target_os = "macos")]
    pub(in crate::workspace) fn native_open_recent(&mut self, id: u64) -> Task<WorkspaceMessage> {
        let active = self.native_active_window();
        if self.native_modal_blocks_fallback(active) || !self.native_open_recent_available(active) {
            return Task::none();
        }
        let owner = active.map(|(owner, _)| owner);
        let selection = self.open_recent_rows().select(id);
        match selection {
            Some(Selection::Clear) => self.clear_open_recent(owner),
            Some(Selection::Item(entry)) => {
                if let Some(owner) = owner {
                    return self.open_recent_item(owner, entry);
                }
                let (owner, opened) = self.open_window(false);
                opened.chain(Task::done(wrap(Message::OpenRecentInNewWindow(owner, id))))
            }
            None => Task::none(),
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn open_recent_in_new_window(
        &mut self,
        owner: window::Id,
        id: u64,
    ) -> Task<WorkspaceMessage> {
        let Some(window) = self.windows.get(&owner) else {
            return Task::none();
        };
        if !self.native_open_recent_available(Some((owner, window)))
            || self.native_modal_blocks_fallback(Some((owner, window)))
        {
            return Task::none();
        }
        let selection = self.open_recent_rows().select(id);
        match selection {
            Some(Selection::Item(entry)) => self.open_recent_item(owner, entry),
            _ => Task::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::profile::projects::Name;

    use super::super::{Action, Category, Entry as MenuEntry, Navigation};
    use super::*;

    fn project(label: &str) -> Entry {
        Entry::Project {
            name: Name::parse(label).unwrap(),
            label: label.into(),
            cleared_at: 0,
        }
    }

    #[test]
    fn open_recent_menu_groups_targets_and_invalidates_old_items_and_clear() {
        let mut menu = Rows::default();
        assert_eq!(menu.layout(), vec![Row::Heading(EMPTY)]);
        let build = project("Build");
        let connection = Entry::Connection {
            id: 42,
            label: "Server · dev@example.test".into(),
            used_at: 1,
        };
        menu.update(vec![build.clone(), connection.clone()]);
        let project_id = menu.entries[0].0;
        let connection_id = menu.entries[1].0;
        let clear = menu.clear.unwrap();
        assert_eq!(
            menu.layout(),
            vec![
                Row::Heading("Connections"),
                Row::Item(connection_id, connection.label()),
                Row::Separator,
                Row::Heading("Projects"),
                Row::Item(project_id, build.label()),
                Row::Separator,
                Row::Item(clear, CLEAR)
            ]
        );
        menu.update(vec![build.clone(), connection]);
        assert_eq!(menu.clear, Some(clear));
        assert!(matches!(menu.select(project_id), Some(Selection::Item(entry)) if entry == build));
        assert!(matches!(menu.select(clear), Some(Selection::Clear)));
        menu.update(vec![project("Next")]);
        assert!(menu.select(project_id).is_none());
        assert!(menu.select(connection_id).is_none());
        assert!(menu.select(clear).is_none());
        assert!(matches!(
            menu.layout().as_slice(),
            [Row::Heading("Projects"), ..]
        ));
        menu.update(Vec::new());
        assert_eq!(menu.layout(), vec![Row::Heading(EMPTY)]);
        assert_eq!(menu.clear, None);
    }

    #[test]
    fn open_recent_menu_keyboard_routes_only_a_current_item_in_its_window() {
        assert!(Category::File.entries().windows(3).any(|entries| matches!(
            entries,
            [
                MenuEntry::Action(Action::OpenProject),
                MenuEntry::Submenu(Branch::OpenRecent),
                MenuEntry::Action(Action::SaveProject)
            ]
        )));
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        // Test double for the off-thread catalog delivery. Project opening
        // below uses the ordinary guarded workspace route.
        workspace.seed_open_recent_entries(vec![project("Build")]);
        let _ = workspace.update_chrome(Message::OpenMenu(owner, Category::File));
        workspace.open_menu_branch(owner, Branch::OpenRecent);
        let _ = workspace.navigate_chrome(owner, Navigation::Next);
        let Some(Choice::OpenRecent(old)) = workspace.chrome.menu.unwrap().selected else {
            panic!("project was not selected");
        };
        assert_eq!(workspace.invoke_open_recent(other, old).units(), 0);
        workspace.seed_open_recent_entries(vec![project("Next")]);
        assert_eq!(
            workspace
                .navigate_chrome(owner, Navigation::Activate)
                .units(),
            0
        );
        assert!(!workspace.project_panel_open(owner));

        let _ = workspace.navigate_chrome(owner, Navigation::Next);
        let task = workspace.navigate_chrome(owner, Navigation::Activate);
        assert!(task.units() > 0);
        assert!(workspace.project_panel_open(owner));
        assert!(!workspace.project_panel_open(other));
        assert!(workspace.chrome.menu.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_recent_native_fallback_opens_a_main_window_only_for_a_current_item() {
        let mut workspace = Workspace::default();
        // Test double for the off-thread catalog delivery.
        workspace.seed_open_recent_entries(vec![project("Build")]);
        let stale = workspace.open_recent_rows().entries[0].0;
        workspace.seed_open_recent_entries(vec![project("Next")]);
        assert_eq!(workspace.native_open_recent(stale).units(), 0);
        assert!(workspace.windows.is_empty());

        let current = workspace.open_recent_rows().entries[0].0;
        assert!(workspace.native_open_recent(current).units() > 0);
        let owner = *workspace.windows.keys().next().unwrap();
        assert!(!workspace.project_panel_open(owner));
        assert!(workspace.open_recent_in_new_window(owner, current).units() > 0);
        assert!(workspace.project_panel_open(owner));
    }
}
