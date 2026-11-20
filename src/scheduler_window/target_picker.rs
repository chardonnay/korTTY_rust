use std::collections::BTreeMap;

use iced::widget::{button, checkbox, column, row, scrollable, text, text_input};
use iced::{Element, Length};

use super::{ConnectionChoice, ConnectionGroup, Message, connection::MAX_SELECTED_GROUPS};

const MAX_SELECTION: usize = kortty_jobs::MAX_SWARM_TARGETS + MAX_SELECTED_GROUPS;

#[derive(Clone, Eq, PartialEq)]
pub enum Choice {
    Connection(ConnectionChoice),
    Group(ConnectionGroup),
}

#[derive(Clone)]
pub enum Event {
    Open,
    Close,
    Search(String),
    Select(Choice, bool),
    Clear,
    Apply,
}

#[derive(Default)]
pub(super) struct Picker {
    query: String,
    selected: Vec<Choice>,
}

impl Picker {
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

    pub(super) fn view(
        &self,
        connections: &[ConnectionChoice],
        groups: &[ConnectionGroup],
        paths: &BTreeMap<u64, String>,
        allowed: bool,
    ) -> Element<'_, Message> {
        let mut matches = self.matching(connections, groups, paths).peekable();
        let mut rows = column![].spacing(6);
        if matches.peek().is_none() {
            rows = rows.push(text("No matching saved targets").size(13));
        }
        for (choice, label) in matches {
            let selected = self.selected.contains(&choice);
            rows = rows.push(checkbox(selected).label(label).on_toggle_maybe(
                (allowed && (selected || self.selected.len() < MAX_SELECTION)).then_some(
                    move |selected| Message::TargetPicker(Event::Select(choice.clone(), selected)),
                ),
            ));
        }
        let has_selection = !self.selected.is_empty();
        column![
            text_input("Search by name, host or group", &self.query)
                .on_input(|query| Message::TargetPicker(Event::Search(query))),
            scrollable(rows).height(220).width(Length::Fill),
            text(format!("{} selected", self.selected.len())).size(13),
            row![
                button("Add selected").on_press_maybe(
                    (allowed && has_selection).then_some(Message::TargetPicker(Event::Apply)),
                ),
                button("Clear selection")
                    .on_press_maybe(has_selection.then_some(Message::TargetPicker(Event::Clear)),),
                button("Cancel").on_press(Message::TargetPicker(Event::Close)),
            ]
            .spacing(6)
            .wrap(),
        ]
        .spacing(6)
        .into()
    }
}
