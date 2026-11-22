use iced::keyboard::{Key, key::Named};
use iced::widget::{Column, button, container, scrollable, text};
use iced::{Element, Length, Point};

use crate::workspace::chrome::popup;

use super::super::{Action, runs};
use super::{Activity, AgentMessage, Message, Row, Surface, actions, event};

#[derive(Clone, Copy)]
pub(super) struct Menu {
    pub(super) surface: Surface,
    pub(super) row: Option<Row>,
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
    action: Option<AgentMessage>,
}

impl Entry {
    pub(super) fn action(label: &str, action: AgentMessage) -> Self {
        Self {
            label: label.into(),
            action: Some(action),
        }
    }
}

impl From<Action> for Entry {
    fn from(action: Action) -> Self {
        Self {
            label: action.label().into(),
            action: action
                .enabled()
                .then_some(AgentMessage::DesignAction(action)),
        }
    }
}

pub(super) struct RunMenu {
    actions: Vec<Action>,
    earlier_runs: Vec<runs::Choice>,
    epoch: u64,
}

impl RunMenu {
    pub(super) fn for_activity(activity: Activity<'_>) -> Self {
        let run = activity.live_run;
        Self {
            actions: run.map_or_else(Vec::new, |run| {
                actions::run_actions(
                    run,
                    activity
                        .owner
                        .is_some_and(|(state, _)| state.has_archived_activity(run)),
                )
            }),
            earlier_runs: run
                .zip(activity.owner)
                .map_or_else(Vec::new, |(run, (state, id))| {
                    state.history_choices(id, run)
                }),
            epoch: run.map_or(0, |run| run.epoch),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.earlier_runs.is_empty()
    }

    pub(super) fn entries(&self, page: Page) -> Vec<Entry> {
        match page {
            Page::Actions => {
                let mut entries: Vec<_> = self.actions.iter().copied().map(Entry::from).collect();
                if !self.earlier_runs.is_empty() {
                    entries.push(Entry {
                        label: "Earlier runs ›".into(),
                        action: Some(event(Message::MenuPage(Page::EarlierRuns))),
                    });
                }
                entries
            }
            Page::EarlierRuns => std::iter::once(Entry {
                label: "‹ Run actions".into(),
                action: Some(event(Message::MenuPage(Page::Actions))),
            })
            .chain(self.earlier_runs.iter().map(|run| Entry {
                label: run.label.clone(),
                action: Some(AgentMessage::SelectPresentedRun(self.epoch, run.id)),
            }))
            .collect(),
        }
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
            .on_press_maybe(entry.action.clone())
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
            Key::Named(Named::Enter) => selection.clone(),
            Key::Named(Named::ArrowUp) => Some(event(Message::SelectMenuItem(previous))),
            Key::Named(Named::ArrowDown) => Some(event(Message::SelectMenuItem(next))),
            Key::Named(Named::ArrowLeft) if menu.page == Page::EarlierRuns => {
                Some(event(Message::MenuPage(Page::Actions)))
            }
            Key::Named(Named::ArrowRight)
                if matches!(
                    selection,
                    Some(AgentMessage::Design(
                        super::presentation::Message::LogTable(Message::MenuPage(
                            Page::EarlierRuns
                        ))
                    ))
                ) =>
            {
                selection.clone()
            }
            _ => None,
        },
    )
}
