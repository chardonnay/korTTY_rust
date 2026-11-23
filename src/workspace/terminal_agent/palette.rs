//! Java custom theme colors scoped to the Agent activity surface.

use std::cell::RefCell;

use iced::widget::{Container, container, themer};
use iced::{Color, Element, Theme, theme};

use crate::terminal_agent::design::inline::Role;
use crate::terminal_theme::panel_colors::{luminance, mix};
use crate::terminal_theme::{Rgb, Scheme};

pub(in crate::workspace) use crate::terminal_theme::panel_colors::resolved_panel_colors;

use super::Phase;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Palette {
    background: Rgb,
    pub(super) border: Rgb,
    pub(super) text: Rgb,
    pub(super) muted: Rgb,
    pub(super) accent: Rgb,
    pub(super) error: Rgb,
}

impl Palette {
    pub(super) fn default_status() -> Self {
        // Java's default Agent Activity colors when no terminal theme is assigned.
        Self::new(
            Scheme::default(),
            [0x0d1218, 0x1d344e, 0xe6edf5, 0x8696ab, 0x4f9cf0, 0xe36a4d]
                .map(|value| Some(Rgb::from_hex(value))),
        )
    }

    pub(super) fn new(scheme: Scheme, colors: [Option<Rgb>; 6]) -> Self {
        let [background, border, text, muted, accent, error] =
            resolved_panel_colors(scheme, colors);
        Self {
            background,
            border,
            text,
            muted,
            accent,
            error,
        }
    }

    pub(super) fn role(self, role: Role) -> Color {
        let dark = luminance(self.background) < 0.5;
        match role {
            Role::Border => mix(self.border, self.accent, 0.4),
            Role::Accent | Role::Key => self.accent,
            Role::Title | Role::Text => self.text,
            Role::Muted => self.muted,
            Role::Ok => Rgb::from_hex(if dark { 0x3fb68b } else { 0x1f8a5f }),
            Role::Warn => Rgb::from_hex(if dark { 0xe5a84b } else { 0xb7791f }),
            Role::Error => self.error,
            Role::Think => Rgb::from_hex(if dark { 0xa48cff } else { 0x6b46c1 }),
        }
        .color()
    }

    fn theme(self) -> Theme {
        thread_local! {
            static RECENT_THEME: RefCell<Option<(Palette, Theme)>> = const { RefCell::new(None) };
        }
        RECENT_THEME.with_borrow_mut(|cached| {
            if cached.as_ref().is_some_and(|(palette, _)| *palette != self) {
                *cached = None;
            }
            let (_, theme) = cached.get_or_insert_with(|| {
                let dark = luminance(self.background) < 0.5;
                let toward = Rgb::from_hex(if dark { 0xffffff } else { 0x000000 });
                let surface = mix(self.background, toward, if dark { 0.08 } else { 0.04 });
                let hover = mix(self.background, toward, if dark { 0.14 } else { 0.08 });
                let colors = theme::Palette {
                    background: self.background.color(),
                    text: self.text.color(),
                    primary: self.accent.color(),
                    success: mix(
                        self.background,
                        Rgb::from_hex(0x38a169),
                        if dark { 0.90 } else { 1.0 },
                    )
                    .color(),
                    warning: Rgb::from_hex(if dark { 0xe5a84b } else { 0xb7791f }).color(),
                    danger: self.error.color(),
                };
                let theme = Theme::custom_with_fn("kortty agent panel", colors, |colors| {
                    use theme::palette::{Extended, Pair, Secondary};
                    Extended {
                        secondary: Secondary {
                            weak: Pair::new(surface.color(), colors.text),
                            base: Pair::new(surface.color(), colors.text),
                            strong: Pair::new(hover.color(), colors.text),
                        },
                        ..Extended::generate(colors)
                    }
                });
                (self, theme)
            });
            theme.clone()
        })
    }

    fn surface(self) -> container::Style {
        container::Style {
            background: Some(self.background.color().into()),
            text_color: Some(self.text.color()),
            border: iced::Border {
                width: 1.0,
                radius: 5.0.into(),
                color: self.border.color(),
            },
            ..container::Style::default()
        }
    }
}

pub(super) fn scope<'a, M: 'a>(palette: Option<Palette>, view: Element<'a, M>) -> Element<'a, M> {
    if let Some(palette) = palette {
        themer(Some(palette.theme()), view)
            .text_color(|theme| theme.palette().text)
            .into()
    } else {
        view
    }
}

pub(super) fn panel<'a, M: 'a>(palette: Option<Palette>, view: Container<'a, M>) -> Element<'a, M> {
    if let Some(palette) = palette {
        scope(Some(palette), view.style(move |_| palette.surface()).into())
    } else {
        view.style(container::bordered_box).into()
    }
}

pub(super) fn faded_panel<'a, M: 'a>(
    palette: Option<Palette>,
    view: Container<'a, M>,
    opacity: f32,
) -> Element<'a, M> {
    if opacity == 1.0 {
        return panel(palette, view);
    }
    let surface = move |theme: &Theme| {
        let mut style = palette.map_or_else(|| container::bordered_box(theme), Palette::surface);
        style.background = style.background.map(|bg| bg.scale_alpha(opacity));
        style.text_color = Some(
            style
                .text_color
                .unwrap_or(theme.palette().text)
                .scale_alpha(opacity),
        );
        style.border.color = style.border.color.scale_alpha(opacity);
        style.shadow.color = style.shadow.color.scale_alpha(opacity);
        style
    };
    if let Some(palette) = palette {
        scope(Some(palette), view.style(surface).into())
    } else {
        view.style(surface).into()
    }
}

pub(super) fn notice_color(palette: Option<Palette>, phase: Phase) -> Option<Color> {
    palette.map(|palette| {
        if matches!(phase, Phase::Failed | Phase::Blocked) {
            palette.error.color()
        } else {
            palette.muted.color()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_panel_palette_derives_missing_colors_and_preserves_explicit_roles() {
        let scheme = Scheme {
            background: Rgb::from_hex(0x1e1e1e),
            foreground: Rgb::from_hex(0xffffff),
            cursor: Rgb::from_hex(0xffffff),
            ..Scheme::default()
        };
        let explicit = [
            Some(Rgb::from_hex(0x112233)),
            Some(Rgb::from_hex(0x223344)),
            Some(Rgb::from_hex(0xddeeff)),
            Some(Rgb::from_hex(0x8899aa)),
            Some(Rgb::from_hex(0x5566ff)),
            Some(Rgb::from_hex(0xff6655)),
        ];
        assert_eq!(
            resolved_panel_colors(scheme, [None; 6]),
            [0x282d33, 0x324962, 0xffffff, 0xb0b0b0, 0x4f9cf0, 0xe36a4d].map(Rgb::from_hex)
        );
        let palette = Palette::new(scheme, explicit);
        let style = palette.surface();
        let theme = palette.theme();
        assert_eq!(style.background, Some(explicit[0].unwrap().color().into()));
        assert_eq!(style.border.color, explicit[1].unwrap().color());
        assert_eq!(style.text_color, Some(explicit[2].unwrap().color()));
        assert_eq!(
            notice_color(Some(palette), Phase::Setup),
            explicit[3].map(Rgb::color)
        );
        assert_eq!(theme.palette().primary, explicit[4].unwrap().color());
        assert_eq!(
            notice_color(Some(palette), Phase::Failed),
            explicit[5].map(Rgb::color)
        );
        assert_eq!(theme.palette().danger, explicit[5].unwrap().color());
        assert_eq!(notice_color(None, Phase::Failed), None);
    }
}
