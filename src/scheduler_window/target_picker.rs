use std::collections::{BTreeMap, BTreeSet};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{
    Id, button, column, container, mouse_area, row, rule, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Task, Theme};
use sha2::{Digest, Sha256};

use super::{ConnectionChoice, ConnectionGroup, connection::MAX_SELECTED_GROUPS};
use crate::workspace::dialog_design::{
    self,
    keyboard_table::{self, TableKey},
};

mod tree;
const MAX_SELECTION: usize = kortty_jobs::MAX_SWARM_TARGETS + MAX_SELECTED_GROUPS;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Choice {
    Connection(ConnectionChoice),
    Group(ConnectionGroup),
}

impl Choice {
    fn key(&self) -> String {
        let value = match self {
            Self::Connection(ConnectionChoice::Saved { reference, .. }) => {
                format!("connection:{reference:?}")
            }
            Self::Connection(ConnectionChoice::Manual) => "manual".into(),
            Self::Group(group) => format!("group:{group:?}"),
        };
        format!("scheduler-target-{:x}", Sha256::digest(value.as_bytes()))
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Open,
    Close,
    Focus,
    Traverse(bool),
    RowKey(String, Named, Modifiers),
    FirstRow,
    AllRows,
    Click(Choice),
    Accept(Choice),
    Expand(String),
    Search(String),
    Select(Choice, bool),
    Clear,
    Apply,
}

pub(crate) struct Picker {
    query: String,
    selected: Vec<Choice>,
    collapsed: BTreeSet<String>,
    anchor: Option<Choice>,
    pub(crate) modifiers: Modifiers,
    scope: Id,
    scroll: Id,
    search: Id,
    pending_focus: Option<Id>,
}

impl Default for Picker {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected: Vec::new(),
            collapsed: BTreeSet::new(),
            anchor: None,
            modifiers: Modifiers::default(),
            scope: Id::unique(),
            scroll: Id::unique(),
            search: Id::unique(),
            pending_focus: None,
        }
    }
}

impl Picker {
    pub(super) fn new(selected: Vec<Choice>) -> Self {
        Self {
            selected,
            ..Self::default()
        }
    }

    pub(super) fn selected(&self) -> &[Choice] {
        &self.selected
    }

    pub(super) fn search(&mut self, query: String) {
        self.query = query[..query.floor_char_boundary(query.len().min(512))].into();
    }

    pub(super) fn select(&mut self, choice: Choice, selected: bool) {
        if selected {
            if self.selected.len() < MAX_SELECTION && !self.selected.contains(&choice) {
                self.selected.push(choice);
            }
        } else {
            self.selected.retain(|entry| *entry != choice);
        }
    }

    pub(super) fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    pub(super) fn expand(&mut self, key: String) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
    }

    fn choose(&mut self, choice: Choice, visible: &[tree::Row], modifiers: Modifiers) {
        let multiple = modifiers.command();
        if modifiers.shift()
            && let Some(anchor) = &self.anchor
            && let (Some(start), Some(end)) = (
                visible
                    .iter()
                    .position(|row| row.choice.as_ref() == Some(anchor)),
                visible
                    .iter()
                    .position(|row| row.choice.as_ref() == Some(&choice)),
            )
        {
            let choices: Vec<_> = visible[start.min(end)..=start.max(end)]
                .iter()
                .filter_map(|row| row.choice.clone())
                .collect();
            if !multiple {
                self.selected.clear();
            }
            for choice in choices {
                self.select(choice, true);
            }
        } else {
            if multiple {
                let selected = !self.selected.contains(&choice);
                self.select(choice.clone(), selected);
            } else {
                self.selected.clear();
                self.select(choice.clone(), true);
            }
            self.anchor = Some(choice);
        }
    }

    pub(super) fn click(&mut self, choice: Choice, rows: &[tree::Row]) {
        self.pending_focus = Some(Id::from(choice.key()));
        self.choose(choice, rows, self.modifiers);
    }

    pub(super) fn key(
        &mut self,
        key: &str,
        action: Named,
        modifiers: Modifiers,
        rows: &[tree::Row],
    ) {
        let Some(index) = rows.iter().position(|row| row.key == key) else {
            return;
        };
        let target = match action {
            Named::ArrowUp => index.saturating_sub(1),
            Named::ArrowDown => (index + 1).min(rows.len().saturating_sub(1)),
            Named::Home => 0,
            Named::End => rows.len().saturating_sub(1),
            Named::ArrowLeft | Named::ArrowRight => {
                if rows[index].branch {
                    if action == Named::ArrowLeft {
                        self.collapsed.insert(key.into());
                    } else {
                        self.collapsed.remove(key);
                    }
                }
                return;
            }
            Named::Space => {
                if let Some(choice) = &rows[index].choice {
                    self.select(choice.clone(), !self.selected.contains(choice));
                    self.anchor = Some(choice.clone());
                }
                return;
            }
            _ => return,
        };
        self.pending_focus = Some(Id::from(rows[target].key.clone()));
        if (!modifiers.command() || modifiers.shift())
            && let Some(choice) = &rows[target].choice
        {
            self.choose(choice.clone(), rows, modifiers);
        }
    }

    pub(crate) fn focus_search<Message: Send + 'static>(&self) -> Task<Message> {
        self.focus_widget(self.search.clone())
    }

    pub(crate) fn traverse<Message: Send + 'static>(&self, backwards: bool) -> Task<Message> {
        use iced::advanced::widget::operation::{focusable, scope};
        let action = if backwards {
            iced_runtime::Action::widget(scope(self.scope.clone(), focusable::focus_previous()))
        } else {
            iced_runtime::Action::widget(scope(self.scope.clone(), focusable::focus_next()))
        };
        iced_runtime::task::effect(action).chain(self.reveal())
    }

    pub(crate) fn take_focus<Message: Send + 'static>(&mut self) -> Task<Message> {
        self.pending_focus
            .take()
            .map_or_else(Task::none, |id| self.focus_widget(id).chain(self.reveal()))
    }

    fn focus_widget<Message: Send + 'static>(&self, id: Id) -> Task<Message> {
        use iced::advanced::widget::operation::{focusable, scope};
        iced_runtime::task::effect(iced_runtime::Action::widget(scope(
            self.scope.clone(),
            focusable::focus(id),
        )))
    }

    fn reveal<Message: Send + 'static>(&self) -> Task<Message> {
        dialog_design::scroll::reveal_focused(self.scope.clone(), self.scroll.clone())
    }

    pub(super) fn matching<'a>(
        &'a self,
        connections: &'a [ConnectionChoice],
        groups: &'a [ConnectionGroup],
        paths: &'a BTreeMap<u64, String>,
    ) -> impl Iterator<Item = (Choice, String)> + 'a {
        let query = self.query.trim().to_lowercase();
        let group_query = query.clone();
        let connections = connections.iter().filter_map(move |choice| {
            let ConnectionChoice::Saved { reference, name } = choice else {
                return None;
            };
            let group = paths.get(&reference.id).map_or("", String::as_str);
            (query.is_empty()
                || [name.as_str(), reference.target.host.as_str(), group]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&query)))
            .then(|| {
                let label = if group.is_empty() {
                    choice.to_string()
                } else {
                    format!("{choice} · {group}")
                };
                (Choice::Connection(choice.clone()), label)
            })
        });
        let groups = groups.iter().filter_map(move |group| {
            let label = group.to_string();
            label
                .to_lowercase()
                .contains(&group_query)
                .then(|| (Choice::Group(group.clone()), format!("Group · {label}")))
        });
        connections.chain(groups)
    }

    pub(super) fn rows(
        &self,
        connections: &[ConnectionChoice],
        groups: &[ConnectionGroup],
        paths: &BTreeMap<u64, String>,
    ) -> Vec<tree::Row> {
        tree::rows(self, connections, groups, paths)
    }

    pub(super) fn view(
        &self,
        connections: &[ConnectionChoice],
        groups: &[ConnectionGroup],
        paths: &BTreeMap<u64, String>,
        allowed: bool,
        status: Option<&str>,
    ) -> Element<'_, Event> {
        let rows = self.rows(connections, groups, paths);
        let mut list = column![].spacing(1);
        if rows.is_empty() {
            list = list.push(text("No matching saved targets").size(13));
        }
        for item in rows {
            let selected = item
                .choice
                .as_ref()
                .is_some_and(|choice| self.selected.contains(choice));
            let mut title = text(item.label).size(13);
            if item.branch {
                title = title.font(dialog_design::ui_font(iced::font::Weight::Bold));
            }
            let labels = column![
                row![
                    title,
                    item.count
                        .map(|count| text(count.to_string()).size(12).style(text::secondary))
                ]
                .spacing(8),
                item.detail
                    .map(|value| text(value).size(12).style(text::secondary))
            ]
            .spacing(2);
            let icon: Element<'_, Event> = if item.branch {
                let toggle = button(
                    text(
                        if self.collapsed.contains(&item.key) && self.query.trim().is_empty() {
                            "▸"
                        } else {
                            "▾"
                        },
                    )
                    .size(13),
                )
                .padding(2)
                .width(16)
                .style(button::text)
                .on_press(Event::Expand(item.key.clone()));
                row![
                    toggle,
                    crate::connection_window::ConnectionIcon::Folder.widget(16, true)
                ]
                .spacing(10)
                .into()
            } else {
                row![
                    space::horizontal().width(28),
                    crate::connection_window::ConnectionIcon::Server.widget(16, true)
                ]
                .spacing(10)
                .into()
            };
            let choice = item.choice.clone();
            let key = item.key.clone();
            let content = row![
                space::horizontal().width(32 * item.depth as u32),
                icon,
                labels
            ]
            .spacing(6)
            .align_y(Alignment::Center);
            let item_row = container(content)
                .width(Length::Fill)
                .padding([6, 8])
                .style(move |theme: &Theme| dialog_design::table_row(theme, selected, false));
            let keyboard_choice = choice.clone();
            let mut clickable = mouse_area(item_row);
            if allowed && let Some(choice) = choice {
                clickable = clickable
                    .on_press(Event::Click(choice.clone()))
                    .on_double_click(Event::Accept(choice));
            }
            let element = keyboard_table::view_with_id(
                clickable.into(),
                Id::from(item.key),
                move |key_event, modifiers, repeat| {
                    if repeat && matches!(key_event, Key::Named(Named::Enter | Named::Space)) {
                        return TableKey::Handled(None);
                    }
                    match key_event {
                        Key::Named(Named::Space) if allowed && keyboard_choice.is_some() => {
                            TableKey::Handled(Some(Event::Select(
                                keyboard_choice.clone().unwrap(),
                                !selected,
                            )))
                        }
                        Key::Character(value)
                            if allowed
                                && modifiers.command()
                                && value.eq_ignore_ascii_case("a") =>
                        {
                            TableKey::Handled(Some(Event::AllRows))
                        }
                        Key::Named(Named::Enter) if allowed => {
                            TableKey::Handled(Some(Event::Apply))
                        }
                        Key::Named(
                            action @ (Named::ArrowUp
                            | Named::ArrowDown
                            | Named::ArrowLeft
                            | Named::ArrowRight
                            | Named::Home
                            | Named::End
                            | Named::Space),
                        ) if allowed => {
                            TableKey::Handled(Some(Event::RowKey(key.clone(), *action, modifiers)))
                        }
                        _ => TableKey::Ignored,
                    }
                },
            );
            list = list.push(element);
        }
        let apply = (allowed && !self.selected.is_empty()).then_some(Event::Apply);
        let action = |label, message: Option<Event>| {
            let control = button(text(label).size(13).width(Length::Fill).center())
                .width(if label == "Clear selection" { 110 } else { 70 })
                .on_press_maybe(message.clone());
            let control = if label == "Select" {
                control
            } else {
                control.style(dialog_design::secondary_button)
            };
            keyboard_table::action(control.into(), message, true)
        };
        let body = column![
            text("Select servers or server groups from the Connection Manager.").size(14),
            rule::horizontal(1),
            row![
                text("Search:").size(13),
                text_input("Search by name, host or group", &self.query)
                    .size(13)
                    .style(dialog_design::form_input)
                    .id(self.search.clone())
                    .vertical_steps(None, Some(Event::FirstRow))
                    .on_submit_maybe((allowed && !self.selected.is_empty()).then_some(Event::Apply))
                    .on_input(Event::Search)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            scrollable(list)
                .id(self.scroll.clone())
                .height(Length::Fill)
                .width(Length::Fill),
            status.map(|value| text(value.to_owned()).size(13)),
            row![
                text(format!("{} selected", self.selected.len())).size(12),
                space::horizontal(),
                action(
                    "Clear selection",
                    (!self.selected.is_empty()).then_some(Event::Clear)
                ),
                action("Cancel", Some(Event::Close)),
                action("Select", apply)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10)
        .padding(10)
        .height(Length::Fill);
        dialog_design::on_key_press(
            container(body).id(self.scope.clone()).into(),
            |key, modifiers| {
                if key == &Key::Named(Named::Escape) {
                    Some(Event::Close)
                } else if key == &Key::Named(Named::Tab)
                    && (modifiers - Modifiers::SHIFT).is_empty()
                {
                    Some(Event::Traverse(modifiers.shift()))
                } else {
                    None
                }
            },
        )
    }
}

impl super::SchedulerWindow {
    pub(crate) fn target_picker_view<'a>(
        &'a self,
        view: &'a super::SchedulerView,
    ) -> Option<Element<'a, Event>> {
        let picker = view.draft.target_picker.as_ref()?;
        Some(picker.view(
            &self.connection_choices,
            &self.connection_group_choices,
            &self.connection_group_paths,
            self.can_edit() && self.draft_matches(view),
            view.status.as_deref(),
        ))
    }
}

impl super::SchedulerView {
    pub(crate) fn target_picker(&self) -> Option<&Picker> {
        self.draft.target_picker.as_ref()
    }
    pub(crate) fn target_picker_mut(&mut self) -> Option<&mut Picker> {
        self.draft.target_picker.as_mut()
    }
    pub(crate) fn cancel_target_picker(&mut self) {
        self.draft.target_picker = None;
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use crate::settings_search::test_input;
    use iced::{Font, Renderer, Size};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn down_leaves_search_then_moves_within_the_tree() {
        let picker = Picker::default();
        let connections = [ConnectionChoice::Saved {
            reference: (&crate::scheduler_window::tests::grouped_connection("keyboard.example"))
                .into(),
            name: "Keyboard target".into(),
        }];
        let paths = BTreeMap::new();
        let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
        let mut ui = UserInterface::build(
            picker.view(&connections, &[], &paths, true, None),
            Size::new(500.0, 340.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(
            &mut ui,
            &renderer,
            iced::widget::operation::focus(picker.search.clone()),
        );
        assert!(matches!(
            test_input::press(&mut ui, &mut renderer, Key::Named(Named::ArrowDown), None)
                .as_slice(),
            [Event::FirstRow]
        ));
        let rows = picker.rows(&connections, &[], &paths);
        test_input::apply(
            &mut ui,
            &renderer,
            iced::widget::operation::focus(Id::from(rows[0].key.clone())),
        );
        assert!(matches!(
            test_input::press(&mut ui, &mut renderer, Key::Named(Named::ArrowDown), None)
                .as_slice(),
            [Event::RowKey(_, Named::ArrowDown, _)]
        ));
        assert!(matches!(
            test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None).as_slice(),
            [Event::Select(_, true)]
        ));
    }
}

#[cfg(test)]
mod activation_tests {
    use super::*;
    use iced::advanced::clipboard;
    use iced::{Font, Point, Renderer, Size, mouse};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn a_target_row_publishes_double_click_and_repeated_arrow_navigation() {
        let picker = Picker::default();
        let connections = [ConnectionChoice::Saved {
            reference: (&crate::scheduler_window::tests::grouped_connection("mouse.example"))
                .into(),
            name: "Mouse target".into(),
        }];
        let paths = BTreeMap::new();
        let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
        let mut ui = UserInterface::build(
            picker.view(&connections, &[], &paths, true, None),
            Size::new(500.0, 340.0),
            Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &[
                iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(Point::new(240.0, 105.0)),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, Event::Click(_)))
        );
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, Event::Accept(_)))
        );
        messages.clear();
        let _ = ui.update(
            &[iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: Key::Named(Named::ArrowDown),
                modified_key: Key::Named(Named::ArrowDown),
                physical_key: iced::keyboard::key::Physical::Unidentified(
                    iced::keyboard::key::NativeCode::Unidentified,
                ),
                location: iced::keyboard::Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: true,
            })],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(
            messages.as_slice(),
            [Event::RowKey(_, Named::ArrowDown, _)]
        ));
    }
}

#[cfg(test)]
mod focus_scope_tests {
    use super::*;
    use crate::settings_search::test_input;
    use iced::advanced::widget::{
        Operation,
        operation::{Outcome, focusable, scope},
    };
    use iced::widget::column;
    use iced::{Font, Renderer, Size};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn focusing_a_shared_row_identity_does_not_move_another_pickers_focus() {
        let mut first = Picker::default();
        let second = Picker::default();
        let connections = [ConnectionChoice::Saved {
            reference: (&crate::scheduler_window::tests::grouped_connection("scope.example"))
                .into(),
            name: "Shared row identity".into(),
        }];
        let paths = BTreeMap::new();
        first.pending_focus = Some(Id::from(
            first.rows(&connections, &[], &paths)[0].key.clone(),
        ));
        let focus_first = first.take_focus::<Event>();
        let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
        let mut ui = UserInterface::build(
            column![
                first.view(&connections, &[], &paths, true, None),
                second.view(&connections, &[], &paths, true, None)
            ],
            Size::new(700.0, 900.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut ui, &renderer, second.focus_search());
        test_input::apply(&mut ui, &renderer, focus_first);
        let mut find = scope(second.scope.clone(), focusable::find_focused());
        ui.operate(
            &renderer,
            &mut iced::advanced::widget::operation::black_box(&mut find),
        );
        assert!(matches!(find.finish(), Outcome::Some(id) if id == second.search));
    }
}
