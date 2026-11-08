use iced::widget::{
    button, column, container, image, mouse_area, opaque, row, sensor, space, stack, text,
};
use iced::{Alignment, Color, ContentFit, Element, Length, window};

use crate::workspace::journal_image_menu;
use crate::workspace::journal_thumbnail::{Cache, Key};

use super::super::{Action, Message};
use super::{Desktop, State};

const THUMBNAIL_HEIGHT: f32 = 144.0;

#[derive(Clone, Copy)]
pub(in crate::workspace::journal_dock) struct Context<'a> {
    pub owner: window::Id,
    pub tab: u64,
    pub run: u64,
    pub thumbnails: &'a Cache,
    pub state: &'a State,
    pub enabled: bool,
    pub analysis: bool,
}

impl Context<'_> {
    fn send(self, action: Action) -> Desktop {
        Desktop::JournalDock(self.owner, Message::Bound(self.tab, self.run, action))
    }

    fn visibility(self, key: Key, visible: bool) -> Desktop {
        Desktop::JournalDock(
            self.owner,
            Message::ThumbnailVisible(self.tab, self.run, key, visible),
        )
    }
}

pub(in crate::workspace::journal_dock) fn thumbnail(
    context: Context<'_>,
    key: Key,
) -> Element<'_, Desktop> {
    let content = if let Some(handle) = context.thumbnails.get(&key) {
        Element::from(
            image(handle.clone())
                .width(Length::Fill)
                .height(THUMBNAIL_HEIGHT)
                .content_fit(ContentFit::Contain),
        )
    } else {
        container(text("Open screenshot").size(12))
            .center_x(Length::Fill)
            .center_y(THUMBNAIL_HEIGHT)
            .into()
    };
    let trigger = button(content)
        .width(Length::Fill)
        .padding(2)
        .style(button::secondary)
        .on_press_maybe(
            context
                .enabled
                .then(|| context.send(Action::ShowScreenshot(key.clone()))),
        )
        .into();
    let trigger = menu(trigger, context, &key, false);
    let hide = context.visibility(key.clone(), false);
    // The sensor's key resets even if the same Iced child slot is recycled for another row.
    let sensor_key = (context.tab, context.run, key.clone());
    let owner = context.owner;
    let tab = context.tab;
    let run = context.run;
    sensor(trigger)
        .key(sensor_key)
        .on_show(move |_| {
            Desktop::JournalDock(
                owner,
                Message::ThumbnailVisible(tab, run, key.clone(), true),
            )
        })
        .on_hide(hide)
        .into()
}

pub(in crate::workspace::journal_dock) fn lightbox<'a>(
    panel: Element<'a, Desktop>,
    context: Option<Context<'a>>,
) -> Element<'a, Desktop> {
    let Some((context, full)) = context.and_then(|context| {
        context
            .state
            .full
            .as_ref()
            .filter(|full| full.is_lightbox())
            .map(|full| (context, full))
    }) else {
        return stack([panel, Element::from(space())]).into();
    };
    let close = context.send(Action::HideScreenshot(full.key.clone()));
    let body = match &full.result {
        Some(Ok(displayed)) => {
            let preview: Element<'_, Desktop> = image(displayed.image.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(ContentFit::Contain)
                .into();
            menu(preview, context, &full.key, true)
        }
        Some(Err(error)) => container(text(error).size(13).color(Color::WHITE))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),
        None => container(text("Loading screenshot…").size(13).color(Color::WHITE))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),
    };
    let close_button = button("Back to timeline")
        .style(button::secondary)
        .on_press_maybe(context.enabled.then_some(close.clone()));
    let body: Element<'_, Desktop> = if context.enabled {
        mouse_area(body).on_press(close).into()
    } else {
        body
    };
    let overlay = container(
        column![
            row![
                text("Screenshot").color(Color::WHITE),
                space::horizontal(),
                close_button
            ]
            .align_y(Alignment::Center)
            .spacing(4),
            body,
        ]
        .spacing(8),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(8)
    .style(|_| container::Style::default().background(Color::from_rgb8(18, 20, 25)));
    stack([panel, opaque(overlay)]).into()
}

fn menu<'a>(
    trigger: Element<'a, Desktop>,
    context: Context<'a>,
    key: &Key,
    full: bool,
) -> Element<'a, Desktop> {
    if !context.enabled {
        return trigger;
    }
    let active = context
        .state
        .menu
        .as_ref()
        .filter(|menu| menu.key == *key && menu.full == full)
        .map(|menu| (menu.point, menu.choice));
    let request = key.clone();
    journal_image_menu::menu(
        trigger,
        active,
        context.analysis,
        move |point| {
            context.send(Action::ScreenshotMenu {
                key: request.clone(),
                full,
                point,
            })
        },
        |choice| {
            context.send(Action::ChooseScreenshot {
                key: key.clone(),
                full,
                choice,
            })
        },
        |forward| {
            context.send(Action::StepScreenshotChoice {
                key: key.clone(),
                full,
                forward,
            })
        },
        context.send(Action::DismissScreenshotMenu {
            key: key.clone(),
            full,
        }),
    )
}
