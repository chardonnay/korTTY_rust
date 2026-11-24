use iced::widget::{button, checkbox, column, container, pick_list, row, space, text, tooltip};
use iced::{Alignment, Border, Element, Font, Length, Size};

use crate::terminal_theme::Scheme;

use super::super::appearance::{Design, Preferences};
use super::super::chat_appearance::Profile;
use super::super::ui_font_scale;
use super::{Desktop, Dialog, Message, Setting, highlight_setting, highlighted};

pub(super) fn view(dialog: &Dialog, display: Option<Size>, ui_scale: f32) -> Element<'_, Desktop> {
    let id = dialog.id;
    let preferences = dialog.draft.preferences.appearance;
    let design = preferences.design();
    let automatic_font_scale = preferences.ui_font_scale_auto.unwrap_or(false);
    let font_percent = ui_font_scale::effective(preferences, display);
    let select = move |selected| Desktop::Settings(Message::Design(id, selected));
    let step = |setting: Setting, forward, arrow| {
        highlight_setting(
            dialog,
            setting,
            container(tooltip(
                button(arrow)
                    .style(button::secondary)
                    .on_press(select(design.step(forward))),
                text(setting.label()),
                tooltip::Position::Bottom,
            ))
            .padding(4),
        )
    };
    let controls = row![
        highlight_setting(
            dialog,
            Setting::AppDesign,
            container(
                row![
                    text(Setting::AppDesign.label()),
                    pick_list(Design::ALL, Some(design), select).width(220.0 * ui_scale),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            )
            .padding(4),
        ),
        step(Setting::PreviousAppDesign, false, "◀"),
        step(Setting::NextAppDesign, true, "▶"),
    ]
    .align_y(Alignment::Center)
    .wrap();
    column![
        controls,
        highlighted(
            dialog,
            Setting::DesignAnimations,
            checkbox(preferences.animations())
                .label(Setting::DesignAnimations.label())
                .on_toggle(move |enabled| Desktop::Settings(Message::DesignAnimations(id, enabled))),
        ),
        highlighted(
            dialog,
            Setting::UiFontScale,
            row![
                text(Setting::UiFontScale.label()),
                button("−").style(button::secondary).on_press_maybe(
                    (!automatic_font_scale && font_percent > ui_font_scale::MIN_PERCENT)
                        .then_some(Desktop::Settings(Message::UiFontScaleStep(id, false))),
                ),
                text(format!("{font_percent}%")),
                button("+").style(button::secondary).on_press_maybe(
                    (!automatic_font_scale && font_percent < ui_font_scale::MAX_PERCENT)
                        .then_some(Desktop::Settings(Message::UiFontScaleStep(id, true))),
                ),
            ].spacing(8).align_y(Alignment::Center),
        ),
        highlighted(
            dialog,
            Setting::UiFontScaleAuto,
            checkbox(automatic_font_scale).label(Setting::UiFontScaleAuto.label())
                .on_toggle(move |enabled| Desktop::Settings(Message::UiFontScaleAuto(id, enabled))),
        ),
        container(text("Scales menus, dialogs and labels. Terminal, editor and AI chat keep their own font sizes.")
            .size(13).width(460)).padding([0, 4]),
        highlighted(
            dialog,
            Setting::ChatColorProfile,
            row![
                text(Setting::ChatColorProfile.label()),
                pick_list(Profile::ALL, Some(preferences.chat_color_profile.unwrap_or_default()), move |profile| {
                    Desktop::Settings(Message::ChatColorProfile(id, profile))
                }).width(220.0 * ui_scale),
            ].spacing(8).align_y(Alignment::Center).wrap(),
        ),
        container(column![
            text("Applies to app windows and dialogs. Terminal sessions and editor content keep their own color settings.")
                .size(13).width(460),
            text("Preview").size(14),
            ui_font_scale::unscaled(preview(preferences)),
        ].spacing(14)).padding([0, 4]),
    ].spacing(14).into()
}

fn preview(preferences: Preferences) -> Element<'static, Desktop> {
    let Some(palette) = preferences.design().palette() else {
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
