use iced::keyboard::{Key, key::Named};
use iced::widget::{button, column, container, scrollable, text, tooltip};
use iced::{Element, Length, Point, Task, window};

use crate::workspace::chrome::popup;

use super::super::actions::{Action, run_actions};
use super::{AgentMessage, Desktop, Run, State, Workspace, action, size};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Open,
    Dismiss,
    Highlight(usize),
    Invoke(Action),
}

fn event(message: Message) -> AgentMessage {
    action(super::Message::Menu(message))
}

pub(super) fn view(state: &State, run: &Run) -> Option<Element<'static, AgentMessage>> {
    let choices = run_actions(run, state.has_archived_activity(run));
    if choices.is_empty() {
        return None;
    }
    let panel = &run.presentation.pip;
    let trigger = tooltip(
        button("⋯").on_press(event(Message::Open)).padding([3, 7]),
        text("Run actions").size(12),
        tooltip::Position::Bottom,
    );
    let Some(selected) = panel.menu_selection else {
        return Some(trigger.into());
    };
    let current = selected.min(choices.len() - 1);
    let mut items = column![].spacing(2);
    for (index, &choice) in choices.iter().enumerate() {
        items = items.push(
            button(text(choice.label()).size(13))
                .width(Length::Fill)
                .style(if index == current {
                    button::secondary
                } else {
                    button::text
                })
                .on_press(event(Message::Invoke(choice))),
        );
    }
    let contents = container(scrollable(items))
        .width(225)
        .padding(5)
        .style(container::bordered_box);
    let previous = (current + choices.len() - 1) % choices.len();
    let next = (current + 1) % choices.len();
    let choice = choices[current];
    Some(popup::context_action_with_keyboard(
        trigger.into(),
        |_| event(Message::Open),
        Some((Point::new(0.0, 24.0), contents.into())),
        event(Message::Dismiss),
        move |key, _| {
            Some(event(match key {
                Key::Named(Named::Escape) => Message::Dismiss,
                Key::Named(Named::Enter) => Message::Invoke(choice),
                Key::Named(Named::ArrowUp) => Message::Highlight(previous),
                Key::Named(Named::ArrowDown) => Message::Highlight(next),
                _ => return None,
            }))
        },
    ))
}

impl Workspace {
    pub(super) fn agent_pip_menu(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        let run = &self.terminal_agents.runs[&id];
        let panel = &run.presentation.pip;
        if panel.hidden {
            return Task::none();
        }
        let choices = run_actions(run, self.terminal_agents.has_archived_activity(run));
        let was_open = panel.menu_selection.is_some();
        let mut invoke = None;
        let selection = match message {
            Message::Open if !choices.is_empty() && self.current_run_pane(run).is_ok() => Some(0),
            Message::Dismiss => None,
            Message::Highlight(index) if was_open && index < choices.len() => Some(index),
            Message::Invoke(choice) if was_open && choices.contains(&choice) => {
                invoke = Some(choice);
                None
            }
            _ => return Task::none(),
        };
        let resize = if was_open != selection.is_some() && panel.folded {
            panel.detached.map_or_else(Task::none, |detached| {
                // The Iced popup must fit inside the detached native window.
                let mut bounds = size(run);
                if selection.is_some() {
                    bounds.height += 10.0 + choices.len() as f32 * 30.0;
                }
                window::resize(detached, bounds)
            })
        } else {
            Task::none()
        };
        self.terminal_agents
            .runs
            .get_mut(&id)
            .unwrap()
            .presentation
            .pip
            .menu_selection = selection;
        if let Some(choice) = invoke {
            resize.chain(self.update_terminal_agent(id, AgentMessage::DesignAction(choice)))
        } else {
            resize
        }
    }
}
