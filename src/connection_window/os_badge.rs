use std::sync::OnceLock;

use iced::widget::{button, column, image, row, scrollable, svg, text};
use iced::{Alignment, Element, Length};

use crate::connection_store::OsBadge;
use crate::connection_store::custom_os_badges::{Catalog, CustomBadge};

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
    select: impl Fn(Option<String>) -> Message,
) -> Element<'a, Message> {
    let mut summary = row![text(selected.map_or("No symbol", Badge::name))];
    if let Some(badge) = selected {
        summary = row![badge.image(18), summary];
    }
    let toggle = button(
        summary
            .push(iced::widget::space::horizontal())
            .push(text(if open { "▴" } else { "▾" }))
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .style(button::secondary)
    .on_press_maybe(toggle);
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
    let mut content = column![label, toggle].spacing(5);
    if open {
        let mut choices = column![choice(None, selected, select(None))].spacing(2);
        let mut custom: Vec<_> = catalog
            .entries()
            .iter()
            .filter(|badge| !badge.png().is_empty())
            .map(Badge::Custom)
            .collect();
        custom.sort_by_key(|badge| (badge.family(), badge.name()));
        let mut family = "";
        for badge in OsBadge::all().map(Badge::BuiltIn).chain(custom) {
            if badge.family() != family {
                family = badge.family();
                choices = choices.push(text(family).size(12).style(text::secondary));
            }
            choices = choices.push(choice(
                Some(badge),
                selected,
                select(Some(badge.reference().to_owned())),
            ));
        }
        content = content.push(scrollable(choices).height(228));
    }
    content.into()
}

fn choice<'a>(
    badge: Option<Badge<'a>>,
    selected: Option<Badge<'a>>,
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
            } else {
                button::text
            },
        )
        .on_press(message)
        .into()
}
