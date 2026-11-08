use std::fmt;
use std::sync::OnceLock;

use iced::widget::{
    button, column, container, pick_list, row, scrollable, slider, space, text, tooltip,
};
use iced::{Alignment, Element, Length, Theme};

use crate::terminal_theme::installed_families;
use crate::workspace::chrome::popup;

use super::{
    DEFAULT_SCALE, MAX_SCALE, MIN_SCALE, Preferences, SCALE_STEP, SchemeId, Zoom, ui_font_families,
};

const PANEL_WIDTH: u32 = 352;
const LABEL_WIDTH: u32 = 108;

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Toggle,
    Dismiss,
    ToggleSchemes,
    DismissSchemes,
    Scheme(SchemeId),
    TextFont(Option<String>),
    MonoFont(Option<String>),
    Scale(u16),
    Reset,
    CycleTheme,
    Zoom(Zoom),
}

pub(in crate::workspace) fn appearance_button<'a, Message: Clone + 'a>(
    preferences: &'a Preferences,
    open: bool,
    schemes_open: bool,
    enabled: bool,
    on_event: impl Fn(Event) -> Message + Clone + 'a,
) -> Element<'a, Message> {
    let trigger = button("Appearance")
        .on_press_maybe(enabled.then(|| on_event(Event::Toggle)))
        .into();
    attach_appearance(
        trigger,
        preferences,
        open && enabled,
        schemes_open,
        on_event,
    )
}

pub(in crate::workspace) fn attach_appearance<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    preferences: &'a Preferences,
    open: bool,
    schemes_open: bool,
    on_event: impl Fn(Event) -> Message + Clone + 'a,
) -> Element<'a, Message> {
    let dismiss = on_event(Event::Dismiss);
    let contents = open.then(|| panel(preferences, schemes_open, on_event));
    popup::popover(trigger, contents, dismiss)
}

pub(in crate::workspace) fn page_controls<'a, Message: Clone + 'a>(
    preferences: &Preferences,
    enabled: bool,
    on_event: impl Fn(Event) -> Message + 'a,
) -> Element<'a, Message> {
    let zoom = |caption, title, amount| {
        tooltip(
            button(caption).on_press_maybe(enabled.then(|| on_event(Event::Zoom(amount)))),
            text(title).size(12),
            tooltip::Position::Bottom,
        )
    };
    let fixed = preferences.scheme.is_fixed();
    let theme = if fixed {
        "The colour scheme is fixed in the settings".to_owned()
    } else {
        format!("Theme · {}", preferences.theme)
    };
    row![
        zoom("A−", "Smaller font", Zoom::Smaller),
        zoom("A", "Reset font size", Zoom::Reset),
        zoom("A+", "Larger font", Zoom::Larger),
        tooltip(
            button("◐").on_press_maybe((enabled && !fixed).then(|| on_event(Event::CycleTheme))),
            text(theme).size(12),
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}

fn panel<'a, Message: Clone + 'a>(
    preferences: &Preferences,
    schemes_open: bool,
    on_event: impl Fn(Event) -> Message + Clone + 'a,
) -> Element<'a, Message> {
    let scheme = scheme_picker(preferences.scheme, schemes_open, &on_event);
    let ui_fonts = on_event.clone();
    let mono_fonts = on_event.clone();
    let reset = on_event(Event::Reset);
    let contents = column![
        row![text("Colour scheme").width(LABEL_WIDTH), scheme]
            .spacing(8)
            .align_y(Alignment::Center),
        row![
            text("Text font").width(LABEL_WIDTH),
            font_picker(false, preferences.ui_font.as_deref(), move |font| ui_fonts(
                Event::TextFont(font)
            )),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            text("Monospace font").width(LABEL_WIDTH),
            font_picker(true, preferences.mono_font.as_deref(), move |font| {
                mono_fonts(Event::MonoFont(font))
            }),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            text("Text size").width(LABEL_WIDTH),
            slider(MIN_SCALE..=MAX_SCALE, preferences.scale, move |scale| {
                on_event(Event::Scale(scale))
            })
            .step(SCALE_STEP)
            .default(DEFAULT_SCALE),
            text(format!("{} %", preferences.scale)).width(46),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        button("Reset").on_press(reset),
        text("The A− A A+ and ◐ buttons on the page itself keep working.").size(12),
    ]
    .spacing(9);
    container(scrollable(contents).height(Length::Shrink))
        .width(PANEL_WIDTH)
        .padding(10)
        .style(popover_style)
        .into()
}

fn scheme_picker<'a, Message: Clone + 'a>(
    current: SchemeId,
    open: bool,
    on_event: &impl Fn(Event) -> Message,
) -> Element<'a, Message> {
    let trigger = button(
        row![scheme_label(current), text("▾")]
            .spacing(6)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .style(button::secondary)
    .on_press(on_event(Event::ToggleSchemes))
    .into();
    let contents = open.then(|| {
        let options = column(SchemeId::ALL.into_iter().map(|scheme| {
            button(scheme_label(scheme))
                .width(Length::Fill)
                .style(if scheme == current {
                    button::primary
                } else {
                    button::text
                })
                .on_press(on_event(Event::Scheme(scheme)))
                .into()
        }))
        .spacing(2);
        container(scrollable(options).height(Length::Shrink))
            .width(PANEL_WIDTH - LABEL_WIDTH - 28)
            .padding(4)
            .style(popover_style)
            .into()
    });
    popup::popover_options(trigger, contents, on_event(Event::DismissSchemes))
}

fn scheme_label<'a, Message: 'a>(scheme: SchemeId) -> Element<'a, Message> {
    let mut label = row![text(scheme.to_string()).width(Length::Fill)]
        .spacing(6)
        .align_y(Alignment::Center);
    if let Some(colors) = scheme.swatches() {
        let strip = row(colors.into_iter().map(|color| {
            container(space::Space::new().width(9).height(9))
                .style(move |_| container::Style {
                    background: Some(color.color().into()),
                    border: iced::border::rounded(2),
                    ..container::Style::default()
                })
                .into()
        }))
        .spacing(2);
        label = label.push(strip);
    }
    label.into()
}

fn popover_style(theme: &Theme) -> container::Style {
    let style = container::bordered_box(theme);
    // Keep all pixels inside the bounds that the renderer invalidates on dismissal.
    #[cfg(not(target_os = "macos"))]
    let style = container::Style {
        shadow: iced::Shadow {
            color: iced::Color::BLACK.scale_alpha(0.25),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 10.0,
        },
        ..style
    };
    style
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FontChoice(Option<&'static str>);

impl fmt::Display for FontChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.unwrap_or("(default)"))
    }
}

fn font_picker<'a, Message: Clone + 'a>(
    monospace_first: bool,
    selected: Option<&str>,
    choose: impl Fn(Option<String>) -> Message + 'a,
) -> Element<'a, Message> {
    static TEXT: OnceLock<Vec<FontChoice>> = OnceLock::new();
    static MONO: OnceLock<Vec<FontChoice>> = OnceLock::new();
    let (cache, installed) = if monospace_first {
        (&MONO, installed_families())
    } else {
        (&TEXT, ui_font_families())
    };
    let choices = cache.get_or_init(|| {
        std::iter::once(FontChoice(None))
            .chain(installed.iter().map(|font| FontChoice(Some(font.as_str()))))
            .collect()
    });
    let selected = choices
        .iter()
        .copied()
        .find(|font| font.0 == selected)
        .unwrap_or(FontChoice(None));
    pick_list(choices.as_slice(), Some(selected), move |font| {
        choose(font.0.map(str::to_owned))
    })
    .width(Length::Fill)
    .menu_height(240)
    .into()
}
