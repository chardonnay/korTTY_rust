use std::fmt;
use std::ops::RangeInclusive;

use iced::widget::{Id, button, checkbox, column, pick_list, row, rule, text, text_input};
use iced::{Alignment, Element};

use crate::connection_store::{ANSI_NAMES, TerminalStyle};
use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT};
use crate::terminal_theme::{BoldTextMode, CursorStyle, Rgb, Scheme, Themes};
use crate::workspace::dialog_design::{self, keyboard_table, number_input};

use super::{Choice, Draft, Edit, ThemeChoice, theme_choices};

const LABEL_WIDTH: f32 = 116.0;
const FIELD_WIDTH: f32 = 200.0;

#[derive(Clone, Eq, PartialEq)]
struct StyleTheme {
    choice: ThemeChoice,
    scheme: Option<Scheme>,
}

impl fmt::Display for StyleTheme {
    fn fmt(&self, format: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.choice.fmt(format)
    }
}

impl Draft {
    pub(in crate::connection_window) fn ssh_style_view(
        &self,
        themes: &Themes,
        defaults: &TerminalStyle,
        own: bool,
    ) -> Element<'_, Edit> {
        let defaults = &defaults.appearance;
        let current = if own {
            self.options.theme.as_ref()
        } else {
            defaults.theme.as_ref()
        };
        let (choices, selected) = theme_choices(themes, current);
        let choice = |choice: ThemeChoice| StyleTheme {
            scheme: choice.value.as_ref().and_then(|theme| theme.scheme(themes)),
            choice,
        };
        let selected = choice(selected);
        let theme = if own {
            pick_list(
                choices.into_iter().map(choice).collect::<Vec<_>>(),
                Some(selected),
                |choice| Edit::SshTheme(choice.choice.value, choice.scheme),
            )
            .width(FIELD_WIDTH)
            .keyboard_navigation(self.style_theme_picker.clone())
            .into()
        } else {
            read_only_picker(if current.is_none() {
                "Custom".into()
            } else {
                selected.to_string()
            })
        };
        let font = defaults.font_family.as_deref().unwrap_or("Monospaced");
        let size = defaults
            .font_size
            .map_or_else(String::new, |value| value.to_string());
        let size_input = if own { self.size.as_str() } else { &size };
        column![
            field("Theme", theme),
            field(
                "Font family",
                input(
                    font,
                    if own { &self.font } else { font },
                    own.then_some(Edit::FontFamily),
                )
            ),
            field(
                "Font size",
                quantity(
                    &size,
                    size_input,
                    i64::from(MIN_TERMINAL_FONT)..=i64::from(MAX_TERMINAL_FONT),
                    own.then_some(Edit::FontSize),
                )
            ),
            rule::horizontal(1),
            field(
                "Text color",
                color(0, &self.colors[0], defaults.foreground, own)
            ),
            field(
                "Background color",
                color(1, &self.colors[1], defaults.background, own)
            ),
            ssh_checkbox(
                "Enable terminal colors",
                self.options.sgr_enabled,
                defaults.sgr_enabled.unwrap_or(true),
                own,
                Edit::Colors,
            ),
        ]
        .spacing(10)
        .into()
    }

    pub(in crate::connection_window) fn ssh_close_confirmation_view(
        &self,
        defaults: &TerminalStyle,
        own: bool,
    ) -> Element<'_, Edit> {
        ssh_checkbox(
            "Close without confirmation",
            self.options.confirm_close.map(|confirm| !confirm),
            !defaults.confirm_close.unwrap_or(true),
            own,
            |value| Edit::ConfirmClose(value.map(|skip| !skip)),
        )
    }

    pub(in crate::connection_window) fn ssh_additional_style_view(
        &self,
        defaults: &TerminalStyle,
        own: bool,
    ) -> Element<'_, Edit> {
        let lines = defaults
            .scrollback_lines
            .map_or_else(String::new, |value| value.to_string());
        let memory = defaults
            .scrollback_memory_mib
            .map_or_else(String::new, |value| value.to_string());
        let ansi = ANSI_NAMES.into_iter().enumerate().map(|(index, label)| {
            field(
                label,
                color(
                    index + 4,
                    &self.colors[index + 4],
                    defaults.ansi[index],
                    own,
                ),
            )
        });
        column![
            field(
                "Cursor shape",
                enum_picker(
                    CursorStyle::ALL,
                    self.options.cursor_style,
                    defaults.cursor_style,
                    self.style_cursor_picker.clone(),
                    own,
                    Edit::CursorStyle,
                )
            ),
            field(
                "Bold text",
                enum_picker(
                    BoldTextMode::ALL,
                    self.options.bold,
                    defaults.bold,
                    self.style_bold_picker.clone(),
                    own,
                    Edit::Bold,
                )
            ),
            field(
                "Cursor color",
                color(2, &self.colors[2], defaults.cursor, own)
            ),
            field(
                "Selection color",
                color(3, &self.colors[3], defaults.selection, own)
            ),
            field(
                "Maximum scrollback lines",
                input(
                    &lines,
                    if own { &self.scrollback_lines } else { &lines },
                    own.then_some(Edit::ScrollbackLines),
                )
            ),
            field(
                "Scrollback limit (MiB)",
                input(
                    &memory,
                    if own {
                        &self.scrollback_memory
                    } else {
                        &memory
                    },
                    own.then_some(Edit::ScrollbackMemory),
                )
            ),
            ssh_checkbox(
                "Compress scrollback",
                self.options.scrollback_compression,
                defaults.scrollback_compression.unwrap_or(true),
                own,
                Edit::ScrollbackCompression,
            ),
        ]
        .extend(ansi)
        .spacing(9)
        .into()
    }

    pub(in crate::connection_window) fn ssh_launch_size_view(&self) -> Element<'_, Edit> {
        column![
            field(
                "Initial columns",
                input("Inherit", &self.columns, Some(Edit::Columns))
            ),
            field(
                "Initial rows",
                input("Inherit", &self.rows, Some(Edit::Rows))
            ),
        ]
        .spacing(8)
        .into()
    }
}

pub(in crate::connection_window) fn ssh_checkbox<'a, Message: Clone + 'a>(
    label: &'static str,
    selected: Option<bool>,
    default: bool,
    own: bool,
    edit: fn(Option<bool>) -> Message,
) -> Element<'a, Message> {
    let value = if own {
        selected.unwrap_or(default)
    } else {
        default
    };
    let checkbox = checkbox(value)
        .label(label)
        .text_size(13)
        .on_toggle_maybe(own.then_some(move |value| edit(Some(value))));
    let inherit = (own && selected.is_some()).then(|| edit(None));
    row![
        keyboard_table::action(checkbox.into(), own.then(|| edit(Some(!value))), false),
        inherit.map(|reset| keyboard_table::action(
            button(text("Use global value").size(12))
                .style(button::text)
                .on_press(reset.clone())
                .into(),
            Some(reset),
            true,
        )),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn field<'a>(label: &'static str, control: impl Into<Element<'a, Edit>>) -> Element<'a, Edit> {
    row![text(label).size(14).width(LABEL_WIDTH), control.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

fn input<'a>(
    placeholder: &str,
    value: &str,
    edit: Option<fn(String) -> Edit>,
) -> Element<'a, Edit> {
    text_input(placeholder, value)
        .width(FIELD_WIDTH)
        .style(dialog_design::form_input)
        .on_input_maybe(edit)
        .into()
}

fn quantity<'a>(
    placeholder: &str,
    value: &str,
    bounds: RangeInclusive<i64>,
    edit: Option<fn(String) -> Edit>,
) -> Element<'a, Edit> {
    number_input(
        text_input(placeholder, value).on_input_maybe(edit),
        value,
        bounds,
        1,
        edit,
        54.0,
    )
}

fn color<'a>(index: usize, value: &str, default: Option<Rgb>, own: bool) -> Element<'a, Edit> {
    let placeholder = default.map_or_else(|| "Inherit".into(), |rgb| rgb.to_string());
    let color = if own {
        Rgb::parse(value).or(default)
    } else {
        default
    };
    row![
        text_input(&placeholder, if own { value } else { &placeholder })
            .on_input_maybe(own.then_some(move |value| Edit::Color(index, value)))
            .style(dialog_design::form_input)
            .width(100),
        color.map(|color| text("■").size(19).color(color.color())),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn read_only_picker<'a>(value: String) -> Element<'a, Edit> {
    button(text(value))
        .width(FIELD_WIDTH)
        .style(dialog_design::secondary_button)
        .into()
}

fn enum_picker<'a, T: Copy + Eq + fmt::Display + 'a, const N: usize>(
    values: [T; N],
    current: Option<T>,
    default: Option<T>,
    id: Id,
    own: bool,
    edit: fn(Option<T>) -> Edit,
) -> Element<'a, Edit> {
    if own {
        let choices: Vec<_> = std::iter::once(Choice(None))
            .chain(values.map(|value| Choice(Some(value))))
            .collect();
        pick_list(choices, Some(Choice(current)), move |choice| edit(choice.0))
            .width(FIELD_WIDTH)
            .keyboard_navigation(id)
            .into()
    } else {
        read_only_picker(Choice(default).to_string())
    }
}
