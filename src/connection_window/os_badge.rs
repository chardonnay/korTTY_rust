use std::sync::OnceLock;

use iced::keyboard::{Key, key::Named};
use iced::widget::{Id, button, column, container, image, row, scrollable, svg, text};
use iced::{Alignment, Element, Length, Task};

use crate::connection_store::OsBadge;
use crate::connection_store::custom_os_badges::{Catalog, CustomBadge};
use crate::workspace::dialog_design::{keyboard_table, scroll};

use super::Message;

#[derive(Clone, Copy)]
pub(super) enum Badge<'a> {
    BuiltIn(OsBadge),
    Custom(&'a CustomBadge),
}

impl<'a> Badge<'a> {
    pub(super) fn resolve(reference: Option<&str>, catalog: &'a Catalog) -> Option<Self> {
        let reference = reference?;
        OsBadge::find(reference)
            .map(Self::BuiltIn)
            .or_else(|| catalog.find(reference).map(Self::Custom))
    }

    fn reference(self) -> &'a str {
        match self {
            Self::BuiltIn(badge) => badge.id(),
            Self::Custom(badge) => badge.reference(),
        }
    }

    fn name(self) -> &'a str {
        match self {
            Self::BuiltIn(badge) => badge.name(),
            Self::Custom(badge) => badge.name(),
        }
    }

    fn family(self) -> &'a str {
        match self {
            Self::BuiltIn(badge) => badge.family(),
            Self::Custom(badge) => badge.family(),
        }
    }

    pub(super) fn image(self, size: u32) -> Element<'a, Message> {
        match self {
            Self::BuiltIn(badge) => svg::Svg::new(builtin_handle(badge))
                .width(size)
                .height(size)
                .into(),
            Self::Custom(badge) => badge.image_handle().map_or_else(
                || iced::widget::space().width(size).height(size).into(),
                |handle| image(handle).width(size).height(size).into(),
            ),
        }
    }
}

pub(super) fn builtin_handle(badge: OsBadge) -> svg::Handle {
    static HANDLES: OnceLock<Vec<svg::Handle>> = OnceLock::new();
    HANDLES.get_or_init(|| {
        OsBadge::all()
            .map(|badge| svg::Handle::from_memory(badge.svg()))
            .collect()
    })[badge.index()]
    .clone()
}

pub(super) fn picker<'a>(
    selected: Option<Badge<'a>>,
    catalog: &'a Catalog,
    open: bool,
    toggle: Option<Message>,
    manage: Option<Message>,
    select: impl Fn(Option<String>) -> Message + 'a,
) -> Element<'a, Message> {
    let mut label = row![
        text("OS symbol").size(14),
        iced::widget::space::horizontal()
    ]
    .align_y(Alignment::Center);
    if let Some(message) = manage {
        label = label.push(
            button(text("Manage symbols…").size(13))
                .style(button::text)
                .on_press(message),
        );
    }
    column![
        label,
        picker_control(selected, catalog, open, toggle, select, None)
    ]
    .spacing(5)
    .into()
}

pub(super) struct KeyboardPicker<'a> {
    pub scroll: Id,
    pub cursor: Option<&'a str>,
}

pub(super) fn picker_control<'a>(
    selected: Option<Badge<'a>>,
    catalog: &'a Catalog,
    open: bool,
    toggle: Option<Message>,
    select: impl Fn(Option<String>) -> Message + 'a,
    keyboard: Option<KeyboardPicker<'a>>,
) -> Element<'a, Message> {
    let mut summary = row![text(selected.map_or("No symbol", Badge::name))];
    if let Some(badge) = selected {
        summary = row![badge.image(18), summary];
    }
    let toggle_button = button(
        summary
            .push(iced::widget::space::horizontal())
            .push(text(if open { "▴" } else { "▾" }))
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .style(button::secondary)
    .on_press_maybe(toggle.clone());
    let trigger = if keyboard.is_some() {
        keyboard_table::action(toggle_button.into(), toggle.clone(), true)
    } else {
        toggle_button.into()
    };
    let mut content = column![trigger].spacing(5);
    if open {
        let mut custom: Vec<_> = catalog
            .entries()
            .iter()
            .filter(|badge| !badge.png().is_empty())
            .map(Badge::Custom)
            .collect();
        custom.sort_by_key(|badge| (badge.family(), badge.name()));
        let options: Vec<_> = std::iter::once(None)
            .chain(OsBadge::all().map(Badge::BuiltIn).chain(custom).map(Some))
            .collect();
        let cursor = keyboard
            .as_ref()
            .and_then(|navigation| navigation.cursor)
            .and_then(|reference| {
                options
                    .iter()
                    .position(|&option| reference_of(option) == reference)
            })
            .unwrap_or(0);
        let mut choices = column![].spacing(2);
        let mut family = "";
        for (index, &badge) in options.iter().enumerate() {
            if let Some(badge) = badge
                && badge.family() != family
            {
                family = badge.family();
                choices = choices.push(text(family).size(12).style(text::secondary));
            }
            let item = choice(
                badge,
                selected,
                keyboard.is_some() && index == cursor,
                select(badge.map(|badge| badge.reference().to_owned())),
            );
            choices = choices.push(if let Some(navigation) = &keyboard {
                container(item)
                    .id(row_id(&navigation.scroll, reference_of(badge)))
                    .into()
            } else {
                item
            });
        }
        let list = scrollable(choices).height(228);
        content = content.push(if let Some(navigation) = keyboard {
            keyboard_table::view(
                list.id(navigation.scroll).into(),
                move |key, modifiers, repeat| {
                    use keyboard_table::TableKey;
                    if !modifiers.is_empty() {
                        return TableKey::Ignored;
                    }
                    let index = match key {
                        Key::Named(Named::ArrowDown) => (cursor + 1).min(options.len() - 1),
                        Key::Named(Named::ArrowUp) => cursor.saturating_sub(1),
                        Key::Named(Named::Home) => 0,
                        Key::Named(Named::End) => options.len() - 1,
                        Key::Named(Named::Enter | Named::Space) => {
                            return TableKey::Handled((!repeat).then(|| {
                                select(options[cursor].map(|badge| badge.reference().to_owned()))
                            }));
                        }
                        Key::Named(Named::Escape) => return TableKey::Handled(toggle.clone()),
                        _ => return TableKey::Ignored,
                    };
                    TableKey::Handled(
                        (index != cursor).then(|| {
                            Message::NavigateOsBadge(reference_of(options[index]).to_owned())
                        }),
                    )
                },
            )
        } else {
            list.into()
        });
    }
    content.into()
}

fn reference_of(badge: Option<Badge<'_>>) -> &str {
    badge.map_or("", Badge::reference)
}

fn row_id(list: &Id, reference: &str) -> Id {
    Id::from(format!("connection-os-badge/{list:?}/{reference}"))
}

pub(super) fn reveal(list: Id, reference: &str) -> Task<Message> {
    let row = row_id(&list, reference);
    scroll::reveal_row(list, row)
}

fn choice<'a>(
    badge: Option<Badge<'a>>,
    selected: Option<Badge<'a>>,
    highlighted: bool,
    message: Message,
) -> Element<'a, Message> {
    let mut contents = row![].spacing(8).align_y(Alignment::Center);
    if let Some(badge) = badge {
        contents = contents.push(badge.image(18));
    }
    button(contents.push(text(badge.map_or("No symbol", Badge::name)).size(14)))
        .width(Length::Fill)
        .style(
            if badge.map(Badge::reference) == selected.map(Badge::reference) {
                button::primary
            } else if highlighted {
                button::secondary
            } else {
                button::text
            },
        )
        .on_press(message)
        .into()
}
