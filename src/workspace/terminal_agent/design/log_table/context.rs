use iced::keyboard::{Key, key::Named};
use iced::widget::{Column, button, container, scrollable, text};
use iced::{Element, Length, Point};

use crate::workspace::chrome::popup;

use super::super::{Action, runs};
use super::{AgentMessage, Message, Row, Surface, event};

#[derive(Clone, Copy)]
pub(super) struct Menu {
    pub(super) surface: Surface,
    row: Option<Row>,
    point: Point,
    pub(super) selected: usize,
    pub(super) page: Page,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Page {
    Actions,
    EarlierRuns,
}

pub(super) struct Entry {
    label: String,
    action: AgentMessage,
}

impl From<Action> for Entry {
    fn from(action: Action) -> Self {
        Self {
            label: action.label().into(),
            action: AgentMessage::DesignAction(action),
        }
    }
}

pub(super) fn run_entries(
    actions: &[Action],
    earlier_runs: &[runs::Choice],
    epoch: u64,
    page: Page,
) -> Vec<Entry> {
    match page {
        Page::Actions => {
            let mut entries: Vec<_> = actions.iter().copied().map(Entry::from).collect();
            if !earlier_runs.is_empty() {
                entries.push(Entry {
                    label: "Earlier runs ›".into(),
                    action: event(Message::MenuPage(Page::EarlierRuns)),
                });
            }
            entries
        }
        Page::EarlierRuns => std::iter::once(Entry {
            label: "‹ Run actions".into(),
            action: event(Message::MenuPage(Page::Actions)),
        })
        .chain(earlier_runs.iter().map(|run| Entry {
            label: run.label.clone(),
            action: AgentMessage::SelectPresentedRun(epoch, run.id),
        }))
        .collect(),
    }
}

impl Menu {
    pub(super) fn new(surface: Surface, row: Option<Row>, point: Point) -> Self {
        Self {
            surface,
            row,
            point,
            selected: 0,
            page: Page::Actions,
        }
    }
}

pub(super) fn anchor<'a>(
    trigger: Element<'a, AgentMessage>,
    active: Option<Menu>,
    surface: Surface,
    row: Option<Row>,
    entries: impl FnOnce(Page) -> Vec<Entry>,
) -> Element<'a, AgentMessage> {
    let open = move |point| event(Message::OpenMenu(surface, row, point));
    let dismiss = event(Message::CloseMenu);
    let Some(menu) = active.filter(|menu| menu.row == row) else {
        return popup::context_action(trigger, open, None, dismiss.clone(), dismiss, None);
    };
    let entries = entries(menu.page);
    if entries.is_empty() {
        return trigger;
    }
    let current = menu.selected.min(entries.len() - 1);
    let buttons = entries.iter().enumerate().map(|(index, entry)| {
        button(text(entry.label.clone()).size(13))
            .width(Length::Fill)
            .style(if index == current {
                button::secondary
            } else {
                button::text
            })
            .on_press(entry.action.clone())
            .into()
    });
    let contents = container(scrollable(Column::with_children(buttons).spacing(2)))
        .width(if menu.page == Page::Actions { 225 } else { 320 })
        .max_height(380)
        .padding(5)
        .style(container::bordered_box)
        .into();
    let selection = entries[current].action.clone();
    let previous = (current + entries.len() - 1) % entries.len();
    let next = (current + 1) % entries.len();
    let escape = dismiss.clone();
    popup::context_action_with_keyboard(
        trigger,
        open,
        Some((menu.point, contents)),
        dismiss,
        move |key, _| match key {
            Key::Named(Named::Escape) => Some(escape.clone()),
            Key::Named(Named::Enter) => Some(selection.clone()),
            Key::Named(Named::ArrowUp) => Some(event(Message::SelectMenuItem(previous))),
            Key::Named(Named::ArrowDown) => Some(event(Message::SelectMenuItem(next))),
            Key::Named(Named::ArrowLeft) if menu.page == Page::EarlierRuns => {
                Some(event(Message::MenuPage(Page::Actions)))
            }
            Key::Named(Named::ArrowRight)
                if matches!(
                    selection,
                    AgentMessage::Design(super::presentation::Message::LogTable(
                        Message::MenuPage(Page::EarlierRuns)
                    ))
                ) =>
            {
                Some(selection.clone())
            }
            _ => None,
        },
    )
}
