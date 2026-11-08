//! Native context actions for pinned snippet editor tabs.

use iced::keyboard::{Key, key::Named};
use iced::widget::{button, column, container, mouse_area, text};
use iced::{Element, Length, Point};

use crate::workspace::native_popup;

use super::{Message as SnippetMessage, SnippetWindow, tabs};

#[derive(Clone)]
pub enum Message {
    Open(tabs::Id, Point),
    Dismiss,
    Hover(usize),
    Step(bool),
    Activate,
}

pub(super) struct Menu {
    target: tabs::Id,
    point: Point,
    selected: usize,
}

impl SnippetWindow {
    pub(super) fn update_tab_context(&mut self, message: Message) -> Option<SnippetMessage> {
        match message {
            Message::Open(target, point) if self.tabs.contains(target) => {
                self.tab_context = Some(Menu {
                    target,
                    point,
                    selected: 0,
                });
            }
            Message::Dismiss => self.tab_context = None,
            Message::Hover(selected) => {
                if let Some(menu) = &mut self.tab_context {
                    menu.selected = selected;
                }
            }
            Message::Step(backwards) => {
                let enabled = self.tab_context.as_ref().map(|menu| {
                    self.tab_actions(menu.target)
                        .map(|(_, action)| action.is_some())
                });
                if let Some(menu) = &mut self.tab_context {
                    for _ in 0..5 {
                        menu.selected = (menu.selected + if backwards { 4 } else { 1 }) % 5;
                        if enabled.is_some_and(|enabled| enabled[menu.selected]) {
                            break;
                        }
                    }
                }
            }
            Message::Activate => {
                let menu = self.tab_context.take()?;
                return self.tab_actions(menu.target).get(menu.selected)?.1.clone();
            }
            _ => {}
        }
        None
    }

    fn tab_actions(&self, id: tabs::Id) -> [(&'static str, Option<SnippetMessage>); 5] {
        let saved = self
            .tabs
            .entry(&self.draft, id)
            .and_then(|draft| draft.id)
            .is_some();
        [
            ("Close tab", Some(SnippetMessage::CloseTab(id))),
            ("Close other tabs", Some(SnippetMessage::CloseOtherTabs(id))),
            ("Close all tabs", Some(SnippetMessage::CloseAllTabs)),
            (
                "Show in library",
                saved.then_some(SnippetMessage::RevealTab(id)),
            ),
            (
                "Move to new window",
                saved.then_some(SnippetMessage::MoveTab(id)),
            ),
        ]
    }

    pub(super) fn tab_context_view<'a>(
        &'a self,
        id: tabs::Id,
        trigger: Element<'a, SnippetMessage>,
    ) -> Element<'a, SnippetMessage> {
        let popup = self
            .tab_context
            .as_ref()
            .filter(|menu| menu.target == id)
            .map(|menu| {
                let entries = self.tab_actions(id).into_iter().enumerate().fold(
                    column![].spacing(2),
                    |rows, (index, (label, action))| {
                        rows.push(
                            mouse_area(
                                button(text(label).size(13))
                                    .width(Length::Fill)
                                    .style(if index == menu.selected {
                                        button::primary
                                    } else {
                                        button::text
                                    })
                                    .on_press_maybe(action),
                            )
                            .on_enter(SnippetMessage::TabContext(Message::Hover(index))),
                        )
                    },
                );
                let view: Element<'a, SnippetMessage> = container(entries)
                    .width(210)
                    .padding(5)
                    .style(container::rounded_box)
                    .into();
                (menu.point, view)
            });
        native_popup::context_action_with_keyboard(
            trigger,
            move |point| SnippetMessage::TabContext(Message::Open(id, point)),
            popup,
            SnippetMessage::TabContext(Message::Dismiss),
            |key, _| {
                Some(SnippetMessage::TabContext(match key {
                    Key::Named(Named::Escape) => Message::Dismiss,
                    Key::Named(Named::ArrowUp) => Message::Step(true),
                    Key::Named(Named::ArrowDown) => Message::Step(false),
                    Key::Named(Named::Enter) => Message::Activate,
                    _ => return None,
                }))
            },
        )
    }
}
