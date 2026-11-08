use iced::widget::{button, checkbox, column, container, pick_list, row, space, text, tooltip};
use iced::{Alignment, Border, Element, Font, Length, window};

use crate::terminal_theme::Scheme;

use super::super::appearance::{Design, Preferences};
use super::Message;

pub(super) fn view(preferences: Preferences, id: window::Id) -> Element<'static, Message> {
    let design = preferences.design;
    let step = |forward, arrow, label| {
        tooltip(
            button(arrow)
                .style(button::secondary)
                .on_press(Message::Design(id, design.step(forward))),
            text(label),
            tooltip::Position::Bottom,
        )
    };
    let controls = row![
        text("App Design"),
        pick_list(Design::ALL, Some(design), move |selected| Message::Design(
            id, selected
        ))
        .width(220),
        step(false, "◀", "Previous design"),
        step(true, "▶", "Next design"),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    column![
        controls,
        checkbox(preferences.animations)
            .label("Enable design animations")
            .on_toggle(move |enabled| Message::DesignAnimations(id, enabled)),
        text("Applies to app windows and dialogs. Terminal sessions and editor content keep their own color settings.")
            .size(13).width(460),
        text("Preview").size(14),
        preview(preferences),
    ].spacing(14).into()
}

fn preview(preferences: Preferences) -> Element<'static, Message> {
    let Some(palette) = preferences.design.palette() else {
        return container(text("No preview is available for this design.").size(13))
            .center_x(452)
            .center_y(276)
            .style(container::bordered_box)
            .into();
    };
    let foreground = palette.foreground.color();
    let muted = palette.muted.color();
    let accent = palette.accent.color();
    let terminal = Scheme::default();
    let contents = column![
        row![
            text("kortty").size(15).color(foreground),
            space::horizontal(),
            text("Window").size(12).color(muted)
        ]
        .align_y(Alignment::Center),
        row![
            text("File").color(foreground),
            text("Connections").color(foreground),
            text("View").color(foreground)
        ]
        .spacing(16),
        row![
            text("Terminal").color(accent),
            text("Tools").color(muted),
            text("Settings").color(muted)
        ]
        .spacing(16),
        container(
            text("$ ")
                .font(Font::MONOSPACE)
                .size(14)
                .color(terminal.foreground.color())
        )
        .style(move |_| container::Style::default().background(terminal.background.color()))
        .padding(10)
        .width(Length::Fill)
        .height(Length::Fill),
        row![
            text("Local shell").size(12).color(muted),
            space::horizontal(),
            preferences.status_cursor(false)
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(12);
    container(contents)
        .width(452)
        .height(276)
        .padding(14)
        .style(move |_| {
            container::Style::default()
                .background(palette.background.color())
                .border(Border {
                    color: palette.border.color(),
                    width: 1.0,
                    radius: 5.0.into(),
                })
        })
        .into()
}
