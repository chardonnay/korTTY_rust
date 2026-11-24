use std::{cell::RefCell, fmt};

use iced::widget::{container, scrollable, text_editor, themer};
use iced::{Border, Color, Element, Length, Theme, theme};
use serde::{Deserialize, Serialize};

use crate::terminal_theme::panel_colors::{luminance, mix, resolved_panel_colors};
use crate::terminal_theme::{Rgb, Themes};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Profile {
    Original,
    Paper,
    Midnight,
    Cyberpunk,
    Retrowave,
    Forest,
    Ocean,
    Terminal,
    Gpt,
    Cute,
    #[default]
    #[serde(other)]
    Auto,
}

impl Profile {
    pub(super) const ALL: [Self; 11] = [
        Self::Auto,
        Self::Original,
        Self::Paper,
        Self::Midnight,
        Self::Cyberpunk,
        Self::Retrowave,
        Self::Forest,
        Self::Ocean,
        Self::Terminal,
        Self::Gpt,
        Self::Cute,
    ];

    pub(super) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|profile| profile.id() == id)
    }

    fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Original => "original",
            Self::Paper => "paper",
            Self::Midnight => "midnight",
            Self::Cyberpunk => "cyberpunk",
            Self::Retrowave => "retrowave",
            Self::Forest => "forest",
            Self::Ocean => "ocean",
            Self::Terminal => "terminal",
            Self::Gpt => "gpt",
            Self::Cute => "cute",
        }
    }

    pub(super) fn palette(self, themes: &Themes) -> Palette {
        // Tagged Java profiles use the same nine slots in this order.
        Palette::from_hex(match self {
            Self::Auto => {
                if let Some(id) = themes.selected()
                    && let Some(terminal) = themes.find(id)
                {
                    let scheme = terminal.scheme;
                    let [surface, border, _, muted, accent, _] = resolved_panel_colors(
                        scheme,
                        themes.agent_panel_colors(id).unwrap_or_default(),
                    );
                    return Palette {
                        background: scheme.background,
                        surface,
                        foreground: scheme.foreground,
                        muted,
                        accent,
                        border,
                        code_background: blend(scheme.background, Rgb::from_hex(0), 25),
                        user_background: blend(scheme.background, accent, 16),
                        user_border: blend(scheme.background, accent, 42),
                    };
                }
                [
                    0x1e2228, 0x262b33, 0xd7dee8, 0x8a94a3, 0x4f9cf0, 0x333a44, 0x14181e, 0x243244,
                    0x3b5273,
                ]
            }
            Self::Original => [
                0xf1ebdf, 0xe7dfce, 0x35322b, 0x8f887a, 0x2f8f84, 0xd9cfbb, 0xe7dfce, 0xf6f0e4,
                0xd9cfbb,
            ],
            Self::Paper => [
                0xf7f3ea, 0xefe9dc, 0x33302a, 0x918a7c, 0x2f8f84, 0xe0d8c6, 0xefe9dc, 0xfbf7ef,
                0xe0d8c6,
            ],
            Self::Midnight => [
                0x0d0f14, 0x171a21, 0xc6cdd6, 0x6a7280, 0x45b3bd, 0x262b34, 0x090a0d, 0x161922,
                0x2c313c,
            ],
            Self::Cyberpunk => [
                0x08070d, 0x13101c, 0xb6e3e6, 0x7c6d92, 0xc94fd6, 0x7a3d9e, 0x060509, 0x150e1e,
                0xb64fce,
            ],
            Self::Retrowave => [
                0x16131f, 0x1e1a2c, 0xcdc9e0, 0x7d7596, 0xe0607f, 0x57497d, 0x0f0d18, 0x1c1730,
                0x6d5399,
            ],
            Self::Forest => [
                0x0d130e, 0x151d16, 0xc6d2c2, 0x7e8b78, 0x5cb874, 0x2c3f2a, 0x080c08, 0x131d15,
                0x3a5738,
            ],
            Self::Ocean => [
                0x0a0e15, 0x121824, 0xc3ccd8, 0x6a7688, 0x4a90d9, 0x26303f, 0x070a10, 0x121a28,
                0x2e4159,
            ],
            Self::Terminal => [
                0x030803, 0x0a1a0d, 0x5fd88a, 0x3f7a50, 0x4ade80, 0x14401f, 0x020602, 0x08160c,
                0x1f5a2c,
            ],
            Self::Gpt => [
                0x131315, 0x1c1c20, 0xcdcdce, 0x7a7a7d, 0x2f9e8f, 0x2c2c31, 0x0d0d0f, 0x1b1b1f,
                0x34343a,
            ],
            Self::Cute => [
                0xfce7ee, 0xf6d9e4, 0x4a3540, 0xa98a97, 0xdd5c8e, 0xf0c7d6, 0xfaecf2, 0xfdf2f6,
                0xf0c7d6,
            ],
        })
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Automatic (theme)",
            Self::Original => "Original",
            Self::Paper => "Paper",
            Self::Midnight => "Midnight",
            Self::Cyberpunk => "Cyberpunk",
            Self::Retrowave => "Retrowave",
            Self::Forest => "Forest",
            Self::Ocean => "Ocean",
            Self::Terminal => "Terminal",
            Self::Gpt => "GPT",
            Self::Cute => "Cute",
        })
    }
}

fn blend(from: Rgb, toward: Rgb, percent: u16) -> Rgb {
    let from = from.bytes();
    let toward = toward.bytes();
    Rgb::from_bytes(std::array::from_fn(|index| {
        ((u16::from(from[index]) * (100 - percent) + u16::from(toward[index]) * percent) / 100)
            as u8
    }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) struct Palette {
    background: Rgb,
    surface: Rgb,
    foreground: Rgb,
    pub(in crate::workspace) muted: Rgb,
    pub(in crate::workspace) accent: Rgb,
    border: Rgb,
    code_background: Rgb,
    user_background: Rgb,
    user_border: Rgb,
}

impl Palette {
    fn from_hex(colors: [u32; 9]) -> Self {
        let [
            background,
            surface,
            foreground,
            muted,
            accent,
            border,
            code_background,
            user_background,
            user_border,
        ] = colors.map(Rgb::from_hex);
        Self {
            background,
            surface,
            foreground,
            muted,
            accent,
            border,
            code_background,
            user_background,
            user_border,
        }
    }

    fn dark(self) -> bool {
        luminance(self.background) < 0.5
    }

    pub(in crate::workspace) fn error(self) -> Color {
        Rgb::from_hex(if self.dark() { 0xe36a4d } else { 0xb42318 }).color()
    }

    fn theme(self) -> Theme {
        thread_local! {
            static RECENT: RefCell<Option<(Palette, Theme)>> = const { RefCell::new(None) };
        }
        RECENT.with_borrow_mut(|cached| {
            if let Some((palette, theme)) = cached
                && *palette == self
            {
                return theme.clone();
            }
            let colors = theme::Palette {
                background: self.background.color(),
                text: self.foreground.color(),
                primary: self.accent.color(),
                danger: self.error(),
                ..theme::Palette::DARK
            };
            let hover_target = Rgb::from_hex(if self.dark() { 0xffffff } else { 0 });
            let theme = Theme::custom_with_fn("kortty chat", colors, |colors| {
                use theme::palette::{Extended, Pair, Secondary};
                let surface = Pair::new(self.surface.color(), colors.text);
                let hover = Pair::new(mix(self.surface, hover_target, 0.10).color(), colors.text);
                Extended {
                    secondary: Secondary {
                        weak: surface,
                        base: surface,
                        strong: hover,
                    },
                    ..Extended::generate(colors)
                }
            });
            *cached = Some((self, theme.clone()));
            theme
        })
    }

    pub(in crate::workspace) fn wrap<'a, Message: 'a>(
        self,
        view: Element<'a, Message>,
    ) -> Element<'a, Message> {
        themer(
            Some(self.theme()),
            container(view)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(move |_| container::Style::default().background(self.background.color())),
        )
        .text_color(|theme| theme.palette().text)
        .into()
    }

    fn border(color: Rgb, radius: f32) -> Border {
        Border {
            color: color.color(),
            width: 1.0,
            radius: radius.into(),
        }
    }

    pub(in crate::workspace) fn user(self) -> container::Style {
        container::Style::default()
            .background(self.user_background.color())
            .border(Self::border(self.user_border, 12.0))
    }

    pub(in crate::workspace) fn assistant(self) -> container::Style {
        container::Style::default().border(Self::border(self.border, 12.0))
    }

    pub(in crate::workspace) fn code(self) -> container::Style {
        container::Style::default()
            .background(self.code_background.color())
            .border(Self::border(self.border, 8.0))
    }

    pub(in crate::workspace) fn editor(self, status: text_editor::Status) -> text_editor::Style {
        let border = match status {
            text_editor::Status::Focused { .. } => self.accent,
            text_editor::Status::Hovered => self.muted,
            _ => self.border,
        };
        text_editor::Style {
            background: self.surface.color().into(),
            border: Self::border(border, 8.0),
            placeholder: self.muted.color(),
            value: self.foreground.color(),
            selection: blend(self.background, self.accent, 35).color(),
        }
    }

    pub(in crate::workspace) fn scroll(
        self,
        theme: &Theme,
        status: scrollable::Status,
    ) -> scrollable::Style {
        let mut style = scrollable::default(theme, status);
        style.container = style.container.background(self.background.color());
        for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
            rail.background = Some(self.surface.color().into());
            rail.scroller.background = if matches!(status, scrollable::Status::Active { .. }) {
                blend(self.surface, self.muted, 55).color()
            } else {
                self.muted.color()
            }
            .into();
        }
        style
    }
}

impl super::Workspace {
    pub(in crate::workspace) fn chat_palette(&self) -> Palette {
        self.window_settings
            .appearance
            .chat_color_profile
            .unwrap_or_default()
            .palette(&self.window_settings.themes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_theme::{Id, Preset};

    #[test]
    fn chat_profiles_keep_java_ids_and_only_auto_follows_the_selected_theme() {
        let mut themes = Themes::default();
        let original = Profile::Auto.palette(&themes);
        let fixed = Profile::Paper.palette(&themes);
        assert_eq!(fixed.code_background, Rgb::from_hex(0xefe9dc));
        assert_eq!(fixed.user_background, Rgb::from_hex(0xfbf7ef));
        assert_eq!(fixed.user_border, Rgb::from_hex(0xe0d8c6));
        assert_eq!(
            fixed.theme().palette().text,
            Rgb::from_hex(0x33302a).color()
        );
        themes.select(Some(Id::BuiltIn(Preset::Dark)));
        let selected = themes.appearance().unwrap();
        let automatic = Profile::Auto.palette(&themes);
        assert_ne!(automatic, original);
        assert_eq!(automatic.background, selected.background);
        assert_eq!(automatic.foreground, selected.foreground);
        assert_eq!(
            automatic.code_background,
            blend(selected.background, Rgb::from_hex(0), 25)
        );
        assert_eq!(fixed, Profile::Paper.palette(&themes));
        assert_eq!(Profile::from_id("unrecognized"), None);
        assert_eq!(
            serde_json::from_str::<Profile>("\"unrecognized\"").unwrap(),
            Profile::Auto
        );
        for profile in Profile::ALL {
            assert_eq!(Profile::from_id(profile.id()), Some(profile));
            assert_eq!(serde_json::to_value(profile).unwrap(), profile.id());
        }
    }
}
