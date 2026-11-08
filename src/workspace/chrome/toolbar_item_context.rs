use iced::widget::{Id, button, column, mouse_area, row, space, text};
use iced::{Alignment, Element, Length, Point, Task, window};

use super::toolbar_settings::{Draft, Edit, Group, Location};
use super::{
    ChromeColors, Message, Navigation, ToolbarMessage, WorkspaceMessage, button_style, menu_popup,
    next_item, popup, wrap,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    revision: Id,
    pub(super) location: Location,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Remove,
    Step(bool),
    Move(Group),
}

#[derive(Clone, Debug)]
pub enum Event {
    Open(Target, Option<Point>),
    Dismiss(Target, Id),
    Navigate(Target, Id, Navigation),
    Sections(Target, Id, bool),
    Choose(Target, Id, Command),
    Shortcut(Target, Command),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Choice {
    Command(Command),
    Sections,
}

struct Menu {
    id: Id,
    target: Target,
    point: Option<Point>,
    selected: Option<Choice>,
    sections: bool,
}

pub(super) struct Context {
    revision: Id,
    focus: Id,
    menu: Option<Menu>,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            revision: Id::unique(),
            focus: Id::unique(),
            menu: None,
        }
    }
}

impl Context {
    pub(super) fn target(&self, location: Location) -> Target {
        Target {
            revision: self.revision.clone(),
            location,
        }
    }

    pub(super) fn focus_id(&self) -> Id {
        self.focus.clone()
    }

    pub(super) fn dismiss(&mut self) {
        self.menu = None;
    }

    pub(super) fn invalidate(&mut self) {
        self.revision = Id::unique();
        self.dismiss();
    }
}

impl Command {
    fn label(self) -> &'static str {
        match self {
            Self::Remove => "Remove button",
            Self::Step(false) => "Move left",
            Self::Step(true) => "Move right",
            Self::Move(group) => group.label(),
        }
    }
}

impl Draft {
    pub(super) fn item_context_event(&mut self, event: Event) -> Task<WorkspaceMessage> {
        let target = match &event {
            Event::Open(target, _)
            | Event::Dismiss(target, _)
            | Event::Navigate(target, _, _)
            | Event::Sections(target, _, _)
            | Event::Choose(target, _, _)
            | Event::Shortcut(target, _) => target,
        }
        .clone();
        let location = target.location;
        if self.item_context.target(location) != target
            || self
                .layout
                .items(location.group)
                .get(location.index)
                .is_none()
        {
            return Task::none();
        }
        match event {
            Event::Open(target, point) => {
                self.selected = Some(location);
                self.edit(Edit::CancelDrag);
                self.item_context.menu = Some(Menu {
                    id: Id::unique(),
                    target,
                    point,
                    selected: None,
                    sections: false,
                });
                self.focus_item()
            }
            Event::Shortcut(_, command) => self.apply_item_command(location, command),
            event => {
                let id = match &event {
                    Event::Dismiss(_, id)
                    | Event::Navigate(_, id, _)
                    | Event::Sections(_, id, _)
                    | Event::Choose(_, id, _) => id,
                    Event::Open(..) | Event::Shortcut(..) => unreachable!(),
                };
                let Some(mut menu) = self
                    .item_context
                    .menu
                    .take_if(|menu| menu.target == target && menu.id == *id)
                else {
                    return Task::none();
                };
                match event {
                    Event::Dismiss(..) => return Task::none(),
                    Event::Choose(_, _, command) => {
                        return self.apply_item_command(location, command);
                    }
                    Event::Sections(_, _, show) => {
                        menu.sections = show;
                        menu.selected = None;
                    }
                    Event::Navigate(_, _, navigation) => match navigation {
                        Navigation::Escape | Navigation::Left if menu.sections => {
                            menu.sections = false;
                            menu.selected = Some(Choice::Sections);
                        }
                        Navigation::Escape => return Task::none(),
                        Navigation::Right | Navigation::Activate
                            if menu.selected == Some(Choice::Sections) =>
                        {
                            menu.sections = true;
                            menu.selected = None;
                        }
                        Navigation::Activate => {
                            if let Some(Choice::Command(command)) = menu.selected {
                                return self.apply_item_command(location, command);
                            }
                        }
                        Navigation::Next | Navigation::Previous => {
                            let choices = self.item_choices(location, menu.sections);
                            menu.selected = next_item(
                                &choices,
                                menu.selected,
                                matches!(navigation, Navigation::Next),
                            );
                        }
                        Navigation::Left | Navigation::Right => {}
                    },
                    Event::Open(..) | Event::Shortcut(..) => unreachable!(),
                }
                self.item_context.menu = Some(menu);
                Task::none()
            }
        }
    }

    fn apply_item_command(
        &mut self,
        location: Location,
        command: Command,
    ) -> Task<WorkspaceMessage> {
        if !self.item_command_enabled(location, command) {
            return Task::none();
        }
        self.edit(Edit::CancelDrag);
        self.item_context.dismiss();
        self.selected = Some(location);
        self.edit(match command {
            Command::Remove => Edit::Remove,
            Command::Step(forward) => Edit::Step(forward),
            Command::Move(group) => Edit::Move(group),
        });
        self.focus_item()
    }

    fn focus_item(&self) -> Task<WorkspaceMessage> {
        if self.selected.is_some() {
            iced::widget::operation::focus(self.item_context.focus_id())
        } else {
            Task::none()
        }
    }

    fn item_command_enabled(&self, location: Location, command: Command) -> bool {
        let items = self.layout.items(location.group);
        let Some(item) = items.get(location.index) else {
            return false;
        };
        match command {
            Command::Remove => true,
            Command::Step(false) => location.index > 0,
            Command::Step(true) => location.index + 1 < items.len(),
            Command::Move(group) => item.accepts(group),
        }
    }

    fn item_choices(&self, location: Location, sections: bool) -> Vec<Choice> {
        if sections {
            Group::ALL
                .into_iter()
                .map(Command::Move)
                .filter(|command| self.item_command_enabled(location, *command))
                .map(Choice::Command)
                .collect()
        } else {
            [Command::Remove, Command::Step(false), Command::Step(true)]
                .into_iter()
                .filter(|command| self.item_command_enabled(location, *command))
                .map(Choice::Command)
                .chain([Choice::Sections])
                .collect()
        }
    }

    pub(super) fn item_menu<'a>(
        &self,
        owner: window::Id,
        target: Target,
        trigger: Element<'a, WorkspaceMessage>,
        colors: ChromeColors,
    ) -> Element<'a, WorkspaceMessage> {
        let Some(menu) = self
            .item_context
            .menu
            .as_ref()
            .filter(|menu| menu.target == target)
        else {
            return popup::without_menu(trigger);
        };
        let id = menu.id.clone();
        let dismiss = message(owner, Event::Dismiss(target.clone(), id.clone()));
        popup::toolbar_item_menu(
            trigger,
            menu.point,
            self.item_menu_contents(owner, menu, colors),
            dismiss,
            move |navigation| {
                message(
                    owner,
                    Event::Navigate(target.clone(), id.clone(), navigation),
                )
            },
        )
    }

    fn item_menu_contents<'a>(
        &self,
        owner: window::Id,
        menu: &Menu,
        colors: ChromeColors,
    ) -> Element<'a, WorkspaceMessage> {
        let target = &menu.target;
        let action = |command: Command| {
            let selected = menu.selected == Some(Choice::Command(command));
            button(text(command.label()).size(13))
                .width(Length::Fill)
                .padding([5, 8])
                .style(move |_, status| button_style(status, colors.menu_hover, selected, colors))
                .on_press_maybe(
                    self.item_command_enabled(target.location, command)
                        .then(|| {
                            message(
                                owner,
                                Event::Choose(target.clone(), menu.id.clone(), command),
                            )
                        }),
                )
        };
        let sections_selected = menu.sections || menu.selected == Some(Choice::Sections);
        let sections_message = message(
            owner,
            Event::Sections(target.clone(), menu.id.clone(), true),
        );
        let sections = mouse_area(
            button(
                row![
                    text("Move to section").size(13),
                    space::horizontal(),
                    text("›")
                ]
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([5, 8])
            .style(move |_, status| {
                button_style(status, colors.menu_hover, sections_selected, colors)
            })
            .on_press(sections_message.clone()),
        )
        .on_enter(sections_message);
        let sections = if menu.sections {
            let choices = self.item_choices(target.location, true);
            let rows = choices
                .into_iter()
                .fold(column![].spacing(2), |rows, choice| {
                    if let Choice::Command(command) = choice {
                        rows.push(action(command))
                    } else {
                        rows
                    }
                });
            popup::submenu(sections.into(), menu_popup(rows, colors))
        } else {
            sections.into()
        };
        let rows = [Command::Remove, Command::Step(false), Command::Step(true)]
            .into_iter()
            .fold(column![].spacing(2), |rows, command| {
                rows.push(mouse_area(action(command)).on_enter(message(
                    owner,
                    Event::Sections(target.clone(), menu.id.clone(), false),
                )))
            })
            .push(sections);
        menu_popup(rows, colors)
    }
}

pub(super) fn message(owner: window::Id, event: Event) -> WorkspaceMessage {
    wrap(Message::Toolbar(ToolbarMessage::Edit(
        owner,
        Edit::ItemContext(event),
    )))
}

#[cfg(test)]
mod tests {
    use super::super::toolbar_settings::{Catalog, Item, Toolbar};
    use super::*;

    #[test]
    fn toolbar_item_menu_keeps_the_exact_draft_item_and_compatible_destinations() {
        let owner = window::Id::unique();
        let other = window::Id::unique();
        let mut toolbar = Toolbar::default();
        let _ = toolbar.update(ToolbarMessage::Open(owner, Catalog::Connections));
        let _ = toolbar.update(ToolbarMessage::Edit(
            owner,
            Edit::Add(Group::Connections, Item::Connection(42)),
        ));
        let location = Location {
            group: Group::Connections,
            index: 0,
        };
        let draft = toolbar.draft(owner).unwrap();
        let target = draft.item_context.target(location);
        assert_eq!(
            draft.item_choices(location, true),
            vec![Choice::Command(Command::Move(Group::Connections))]
        );
        let event = |owner, event| ToolbarMessage::Edit(owner, Edit::ItemContext(event));
        let _ = toolbar.update(event(other, Event::Open(target.clone(), None)));
        assert!(toolbar.draft(owner).unwrap().item_context.menu.is_none());
        let _ = toolbar.update(event(owner, Event::Open(target.clone(), None)));
        let old = toolbar
            .draft(owner)
            .unwrap()
            .item_context
            .menu
            .as_ref()
            .unwrap()
            .id
            .clone();
        let _ = toolbar.update(event(
            owner,
            Event::Open(target.clone(), Some(Point::ORIGIN)),
        ));
        let _ = toolbar.update(event(
            owner,
            Event::Choose(target.clone(), old, Command::Remove),
        ));
        assert_eq!(
            toolbar
                .draft(owner)
                .unwrap()
                .layout
                .items(Group::Connections),
            &[Item::Connection(42)]
        );
        let _ = toolbar.update(event(
            owner,
            Event::Shortcut(target.clone(), Command::Remove),
        ));
        let _ = toolbar.update(ToolbarMessage::Edit(
            owner,
            Edit::Add(Group::Connections, Item::Connection(43)),
        ));
        let _ = toolbar.update(event(owner, Event::Shortcut(target, Command::Remove)));
        assert_eq!(
            toolbar
                .draft(owner)
                .unwrap()
                .layout
                .items(Group::Connections),
            &[Item::Connection(43)]
        );
    }
}
