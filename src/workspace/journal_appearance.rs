use std::cell::RefCell;
use std::fmt;
use std::sync::LazyLock;

use iced::{Color, Element, Font, Theme, theme, widget::themer};
use serde::{Deserialize, Deserializer, Serialize};

use crate::terminal_theme::{Rgb, installed_families, installed_font};

pub(super) mod controls;

pub(super) const MIN_SCALE: u16 = 70;
pub(super) const DEFAULT_SCALE: u16 = 100;
pub(super) const MAX_SCALE: u16 = 250;
pub(super) const SCALE_STEP: u16 = 10;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct Preferences {
    pub(super) scheme: SchemeId,
    #[serde(
        deserialize_with = "read_font",
        skip_serializing_if = "Option::is_none"
    )]
    pub(super) ui_font: Option<String>,
    #[serde(
        deserialize_with = "read_font",
        skip_serializing_if = "Option::is_none"
    )]
    pub(super) mono_font: Option<String>,
    #[serde(deserialize_with = "read_scale")]
    pub(super) scale: u16,
    #[serde(deserialize_with = "read_theme")]
    pub(super) theme: PageTheme,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            scheme: SchemeId::Auto,
            ui_font: None,
            mono_font: None,
            scale: DEFAULT_SCALE,
            theme: PageTheme::Auto,
        }
    }
}

impl Preferences {
    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(super) fn set_scale(&mut self, scale: u16) {
        self.scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    }

    pub(super) fn zoom(&mut self, zoom: Zoom) {
        let scale = self.scale.clamp(MIN_SCALE, MAX_SCALE);
        self.set_scale(match zoom {
            Zoom::Smaller => scale - SCALE_STEP,
            Zoom::Reset => DEFAULT_SCALE,
            Zoom::Larger => scale + SCALE_STEP,
        });
    }

    pub(super) fn reset(&mut self) {
        *self = Self {
            theme: self.theme,
            ..Self::default()
        };
    }

    pub(super) fn toggle_theme(&mut self, system_dark: bool) {
        self.theme = PageTheme::for_dark(!self.theme.is_dark(system_dark));
    }

    pub(super) fn content(&self, system_dark: bool, terminal: Option<(Rgb, Rgb)>) -> Content {
        Content {
            palette: self
                .scheme
                .palette(self.theme.is_dark(system_dark), terminal),
            ui_font: self
                .ui_font
                .as_deref()
                .and_then(installed_font)
                .unwrap_or(Font::DEFAULT),
            mono_font: self
                .mono_font
                .as_deref()
                .and_then(installed_font)
                .unwrap_or(Font::MONOSPACE),
            scale: f32::from(self.scale.clamp(MIN_SCALE, MAX_SCALE)) / 100.0,
        }
    }
}

fn read_font<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(
        Option::<String>::deserialize(deserializer)?.and_then(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty() && !trimmed.chars().any(char::is_control))
                .then(|| trimmed.to_owned())
        }),
    )
}

fn read_scale<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let scale = Option::<i64>::deserialize(deserializer)?.unwrap_or(i64::from(DEFAULT_SCALE));
    Ok(scale.clamp(i64::from(MIN_SCALE), i64::from(MAX_SCALE)) as u16)
}

fn read_theme<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PageTheme, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(match value.trim().to_ascii_lowercase().as_str() {
        "light" => PageTheme::Light,
        "dark" => PageTheme::Dark,
        _ => PageTheme::Auto,
    })
}

pub(super) fn ui_font_families() -> &'static [String] {
    static FAMILIES: LazyLock<Vec<String>> = LazyLock::new(|| {
        let mut families = installed_families().to_vec();
        families.sort_unstable();
        families
    });
    &FAMILIES
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Zoom {
    Smaller,
    Reset,
    Larger,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PageTheme {
    #[default]
    Auto,
    Light,
    Dark,
}

impl PageTheme {
    fn for_dark(dark: bool) -> Self {
        if dark { Self::Dark } else { Self::Light }
    }

    pub(super) fn is_dark(self, system_dark: bool) -> bool {
        match self {
            Self::Auto => system_dark,
            Self::Light => false,
            Self::Dark => true,
        }
    }
}

impl fmt::Display for PageTheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Automatic",
            Self::Light => "Light",
            Self::Dark => "Dark",
        })
    }
}

#[derive(Default)]
pub(super) struct ThemeCycle {
    first: Option<PageTheme>,
}

impl ThemeCycle {
    pub(super) fn advance(&mut self, preferences: &mut Preferences, system_dark: bool) {
        if preferences.scheme.is_fixed() {
            return;
        }
        let current = preferences.theme;
        preferences.theme = if current == PageTheme::Auto {
            let opposite = PageTheme::for_dark(!system_dark);
            self.first = Some(opposite);
            opposite
        } else if current == *self.first.get_or_insert(current) {
            PageTheme::for_dark(!current.is_dark(system_dark))
        } else {
            PageTheme::Auto
        };
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SchemeId {
    Theme,
    Paper,
    Midnight,
    Ocean,
    Forest,
    Retrowave,
    HighContrast,
    #[default]
    #[serde(other)]
    Auto,
}

impl SchemeId {
    pub(super) const ALL: [Self; 8] = [
        Self::Auto,
        Self::Theme,
        Self::Paper,
        Self::Midnight,
        Self::Ocean,
        Self::Forest,
        Self::Retrowave,
        Self::HighContrast,
    ];

    pub(super) fn is_fixed(self) -> bool {
        self != Self::Auto
    }

    pub(super) fn swatches(self) -> Option<[Rgb; 5]> {
        let colors = self.fixed_colors()?;
        Some([
            colors.background,
            colors.surface,
            colors.accent,
            colors.input,
            colors.output,
        ])
    }

    pub(super) fn palette(self, dark: bool, terminal: Option<(Rgb, Rgb)>) -> Palette {
        let base = if dark { Palette::DARK } else { Palette::LIGHT };
        let colors = match (self, terminal) {
            (Self::Theme, Some((background, foreground))) => {
                SchemeColors::terminal(background, foreground)
            }
            _ => self.fixed_colors().unwrap_or(base.colors),
        };
        // The Java scheme CSS overrides the page colors but leaves its status colors in place.
        Palette { colors, ..base }
    }

    fn fixed_colors(self) -> Option<SchemeColors> {
        // CSS variable order in the pinned SessionJournalPageSchemes registry.
        let colors = match self {
            Self::Auto | Self::Theme => return None,
            Self::Paper => [
                0xf7f3ea, 0xfffdf8, 0xefe9dc, 0xded5c4, 0x2c2a26, 0x6b6558, 0x8a5a1f, 0x3f6b2f,
                0x2a4a6b, 0xf2cc60, 0xff9f43,
            ],
            Self::Midnight => [
                0x0b1020, 0x141a2e, 0x1b2340, 0x2a3358, 0xdbe2f5, 0x8891b4, 0x7aa2f7, 0x9ece6a,
                0x7dcfff, 0xe0af68, 0xff9e64,
            ],
            Self::Ocean => [
                0x0d1f26, 0x123039, 0x17404c, 0x1f5866, 0xd5e9ef, 0x82a6b1, 0x3fb8c6, 0x7fd1a5,
                0x8fc7e8, 0xf0d066, 0xff9f43,
            ],
            Self::Forest => [
                0x101a12, 0x17261a, 0x1e3223, 0x2a4530, 0xdceadf, 0x8aa791, 0x5fb87a, 0xa6d98b,
                0x8fc7a8, 0xe8cf6a, 0xff9f43,
            ],
            Self::Retrowave => [
                0x1a1030, 0x241542, 0x2f1c55, 0x432a75, 0xf2e7ff, 0xa693c9, 0xff6ec7, 0x7ef0d0,
                0x8ab4ff, 0xffd166, 0xff5f9e,
            ],
            Self::HighContrast => [
                0x000000, 0x0a0a0a, 0x141414, 0x666666, 0xffffff, 0xcccccc, 0x00d4ff, 0x00ff66,
                0x66ccff, 0xffee00, 0xff8800,
            ],
        };
        Some(SchemeColors::from_hex(colors))
    }
}

impl fmt::Display for SchemeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Automatic (follows the system)",
            Self::Theme => "Follow the terminal theme",
            Self::Paper => "Paper",
            Self::Midnight => "Midnight",
            Self::Ocean => "Ocean",
            Self::Forest => "Forest",
            Self::Retrowave => "Retrowave",
            Self::HighContrast => "High contrast",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SchemeColors {
    pub(super) background: Rgb,
    pub(super) surface: Rgb,
    pub(super) surface2: Rgb,
    pub(super) border: Rgb,
    pub(super) text: Rgb,
    pub(super) muted: Rgb,
    pub(super) accent: Rgb,
    pub(super) input: Rgb,
    pub(super) output: Rgb,
    pub(super) mark: Rgb,
    pub(super) mark_current: Rgb,
}

impl SchemeColors {
    const fn from_hex(colors: [u32; 11]) -> Self {
        let [
            background,
            surface,
            surface2,
            border,
            text,
            muted,
            accent,
            input,
            output,
            mark,
            mark_current,
        ] = colors;
        Self {
            background: Rgb::from_hex(background),
            surface: Rgb::from_hex(surface),
            surface2: Rgb::from_hex(surface2),
            border: Rgb::from_hex(border),
            text: Rgb::from_hex(text),
            muted: Rgb::from_hex(muted),
            accent: Rgb::from_hex(accent),
            input: Rgb::from_hex(input),
            output: Rgb::from_hex(output),
            mark: Rgb::from_hex(mark),
            mark_current: Rgb::from_hex(mark_current),
        }
    }

    fn terminal(background: Rgb, foreground: Rgb) -> Self {
        let [red, green, blue] = background.bytes().map(f64::from);
        let dark = (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255.0 < 0.5;
        let shift = |amount| {
            Rgb::from_bytes(
                background
                    .bytes()
                    .map(|value| rounded_channel(f64::from(value) + 255.0 * amount)),
            )
        };
        let muted = Rgb::from_bytes(std::array::from_fn(|index| {
            let text = f64::from(foreground.bytes()[index]);
            let background = f64::from(background.bytes()[index]);
            rounded_channel(text * (1.0 - 0.42) + background * 0.42)
        }));
        let [surface, surface2, border] = if dark {
            [0.06, 0.11, 0.20]
        } else {
            [-0.02, -0.05, -0.12]
        };
        let base = if dark { Palette::DARK } else { Palette::LIGHT };
        Self {
            background,
            surface: shift(surface),
            surface2: shift(surface2),
            border: shift(border),
            text: foreground,
            muted,
            ..base.colors
        }
    }
}

fn rounded_channel(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Palette {
    pub(super) colors: SchemeColors,
    pub(super) error: Rgb,
    pub(super) important: Rgb,
    pub(super) info: Rgb,
    pub(super) none: Rgb,
}

impl Palette {
    const DARK: Self = Self {
        colors: SchemeColors::from_hex([
            0x0f1115, 0x171a21, 0x1e232d, 0x2a303c, 0xd7dce4, 0x8b93a1, 0x58a6ff, 0x7ee787,
            0x9ecbff, 0xf2cc60, 0xff9f43,
        ]),
        error: Rgb::from_hex(0xf85149),
        important: Rgb::from_hex(0xd29922),
        info: Rgb::from_hex(0x58a6ff),
        none: Rgb::from_hex(0x6e7681),
    };
    const LIGHT: Self = Self {
        colors: SchemeColors::from_hex([
            0xf6f8fa, 0xffffff, 0xeef1f5, 0xd0d7de, 0x1f2328, 0x57606a, 0x0969da, 0x116329,
            0x0a3069, 0xf2cc60, 0xff9f43,
        ]),
        error: Rgb::from_hex(0xcf222e),
        important: Rgb::from_hex(0x9a6700),
        info: Rgb::from_hex(0x0969da),
        none: Rgb::from_hex(0x6e7781),
    };

    pub(super) fn theme(self) -> Theme {
        thread_local! {
            static RECENT_THEME: RefCell<Option<(Palette, Theme)>> = const { RefCell::new(None) };
        }
        RECENT_THEME.with_borrow_mut(|cached| {
            if cached.as_ref().is_some_and(|(palette, _)| *palette != self) {
                *cached = None;
            }
            let (_, theme) = cached.get_or_insert_with(|| {
                let palette = theme::Palette {
                    background: self.colors.background.color(),
                    text: self.colors.text.color(),
                    primary: self.colors.accent.color(),
                    success: self.colors.input.color(),
                    warning: self.important.color(),
                    danger: self.error.color(),
                };
                let theme = Theme::custom_with_fn("kortty journal", palette, |palette| {
                    use theme::palette::{Extended, Pair, Secondary};
                    let surface = Pair::new(self.colors.surface.color(), palette.text);
                    let surface2 = Pair::new(self.colors.surface2.color(), palette.text);
                    Extended {
                        secondary: Secondary {
                            weak: surface,
                            base: surface2,
                            strong: Pair::new(self.colors.border.color(), palette.text),
                        },
                        ..Extended::generate(palette)
                    }
                });
                (self, theme)
            });
            theme.clone()
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Content {
    pub(super) palette: Palette,
    pub(super) ui_font: Font,
    pub(super) mono_font: Font,
    pub(super) scale: f32,
}

impl Content {
    pub(super) fn size(self, base: f32) -> f32 {
        base * self.scale
    }

    pub(super) fn highlight(self, active: bool) -> Color {
        if active {
            self.palette.colors.mark_current.color()
        } else {
            self.palette.colors.mark.color()
        }
    }

    pub(super) fn wrap<'a, M: 'a>(&self, view: Element<'a, M>) -> Element<'a, M> {
        themer(Some(self.palette.theme()), view)
            .background(|theme| theme.palette().background.into())
            .text_color(|theme| theme.palette().text)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn automatic_and_fixed_palettes_keep_the_java_page_and_status_colors() {
        let light = SchemeId::Auto.palette(false, None);
        let dark = SchemeId::Auto.palette(true, None);
        assert_eq!(light.colors.background, Rgb::from_hex(0xf6f8fa));
        assert_eq!(dark.colors.background, Rgb::from_hex(0x0f1115));
        assert_eq!(light.error, Rgb::from_hex(0xcf222e));
        assert_eq!(dark.error, Rgb::from_hex(0xf85149));
        assert_eq!(SchemeId::Theme.palette(false, None), light);

        let fixed = [
            (SchemeId::Paper, 0xf7f3ea, 0x8a5a1f, 0xf2cc60),
            (SchemeId::Midnight, 0x0b1020, 0x7aa2f7, 0xe0af68),
            (SchemeId::Ocean, 0x0d1f26, 0x3fb8c6, 0xf0d066),
            (SchemeId::Forest, 0x101a12, 0x5fb87a, 0xe8cf6a),
            (SchemeId::Retrowave, 0x1a1030, 0xff6ec7, 0xffd166),
            (SchemeId::HighContrast, 0x000000, 0x00d4ff, 0xffee00),
        ];
        for (scheme, background, accent, mark) in fixed {
            let fixed_light = scheme.palette(false, None);
            let fixed_dark = scheme.palette(true, None);
            assert_eq!(fixed_light.colors, fixed_dark.colors);
            assert_eq!(fixed_light.colors.background, Rgb::from_hex(background));
            assert_eq!(fixed_light.colors.accent, Rgb::from_hex(accent));
            assert_eq!(fixed_light.colors.mark, Rgb::from_hex(mark));
            assert_eq!(fixed_light.info, light.info);
            assert_eq!(fixed_dark.important, dark.important);
        }
    }

    #[test]
    fn terminal_scheme_uses_terminal_luminance_and_keeps_page_status_tone() {
        let dark_terminal = SchemeId::Theme.palette(
            false,
            Some((Rgb::from_hex(0x102030), Rgb::from_hex(0xc8e0f8))),
        );
        let colors = dark_terminal.colors;
        assert_eq!(colors.surface, Rgb::from_hex(0x1f2f3f));
        assert_eq!(colors.surface2, Rgb::from_hex(0x2c3c4c));
        assert_eq!(colors.border, Rgb::from_hex(0x435363));
        assert_eq!(colors.muted, Rgb::from_hex(0x7b8fa4));
        assert_eq!(colors.accent, Rgb::from_hex(0x58a6ff));
        assert_eq!(dark_terminal.error, Rgb::from_hex(0xcf222e));

        let light_terminal = SchemeId::Theme.palette(
            true,
            Some((Rgb::from_hex(0xeff9fa), Rgb::from_hex(0x112233))),
        );
        let colors = light_terminal.colors;
        assert_eq!(colors.surface, Rgb::from_hex(0xeaf4f5));
        assert_eq!(colors.surface2, Rgb::from_hex(0xe2eced));
        assert_eq!(colors.border, Rgb::from_hex(0xd0dadb));
        assert_eq!(colors.muted, Rgb::from_hex(0x6e7c87));
        assert_eq!(colors.accent, Rgb::from_hex(0x0969da));
        assert_eq!(light_terminal.error, Rgb::from_hex(0xf85149));
    }

    #[test]
    fn viewer_cycles_three_ways_but_the_dock_switches_the_resolved_tone() {
        for (system_dark, opposite, same) in [
            (true, PageTheme::Light, PageTheme::Dark),
            (false, PageTheme::Dark, PageTheme::Light),
        ] {
            let mut preferences = Preferences::default();
            let mut cycle = ThemeCycle::default();
            for expected in [opposite, same, PageTheme::Auto] {
                cycle.advance(&mut preferences, system_dark);
                assert_eq!(preferences.theme, expected);
            }
            for expected in [opposite, same] {
                preferences.toggle_theme(system_dark);
                assert_eq!(preferences.theme, expected);
            }
            preferences.scheme = SchemeId::Midnight;
            cycle.advance(&mut preferences, system_dark);
            assert_eq!(preferences.theme, same);
        }
    }

    #[test]
    fn saved_appearance_bounds_zoom_and_resets_only_the_popover_fields() {
        let mut preferences: Preferences = serde_json::from_str(
            r#"{"scheme":"paper","scale":999,"theme":" DARK ","ui_font":"Missing journal font","mono_font":" "}"#,
        )
        .unwrap();
        assert_eq!(preferences.scheme, SchemeId::Paper);
        assert_eq!(preferences.theme, PageTheme::Dark);
        assert_eq!(preferences.scale, MAX_SCALE);
        assert_eq!(preferences.content(false, None).ui_font, Font::DEFAULT);
        assert_eq!(preferences.content(false, None).mono_font, Font::MONOSPACE);
        preferences.zoom(Zoom::Larger);
        assert_eq!(preferences.scale, MAX_SCALE);
        preferences.zoom(Zoom::Smaller);
        assert_eq!(preferences.scale, MAX_SCALE - SCALE_STEP);
        preferences.zoom(Zoom::Reset);
        assert_eq!(preferences.scale, DEFAULT_SCALE);
        assert_eq!(preferences.scheme, SchemeId::Paper);
        preferences.reset();
        assert_eq!(preferences.scheme, SchemeId::Auto);
        assert_eq!(preferences.ui_font, None);
        assert_eq!(preferences.theme, PageTheme::Dark);

        let preferences: Preferences =
            serde_json::from_str(r#"{"scale":-4,"scheme":"future","theme":"future"}"#).unwrap();
        assert_eq!(preferences.scale, MIN_SCALE);
        assert_eq!(preferences.scheme, SchemeId::Auto);
        assert_eq!(preferences.theme, PageTheme::Auto);
    }

    #[test]
    fn iced_theme_reuses_the_current_exact_content_colors() {
        let paper = SchemeId::Paper.palette(false, None);
        let first = paper.theme();
        let second = paper.theme();
        assert_eq!(first.palette().background, paper.colors.background.color());
        assert_eq!(first.palette().text, paper.colors.text.color());
        assert_eq!(
            first.extended_palette().secondary.base.color,
            paper.colors.surface2.color()
        );
        let (Theme::Custom(first), Theme::Custom(second)) = (first, second) else {
            panic!("expected a custom journal theme")
        };
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            SchemeId::Auto
                .palette(true, None)
                .theme()
                .palette()
                .background,
            Rgb::from_hex(0x0f1115).color()
        );
    }
}
