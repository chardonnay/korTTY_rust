use std::fmt;

use iced::widget::{button, pick_list, row, text_input};
use iced::{Alignment, Element};
use kortty_diagram::Theme;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Auto,
    Light,
    Dark,
}

impl fmt::Display for Mode {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Auto => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<Mode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) background: Option<[u8; 3]>,
}

#[derive(Clone, Debug)]
pub enum Edit {
    Mode(Mode),
    Background(String),
    ApplyBackground,
}

impl Preferences {
    pub(crate) fn is_empty(&self) -> bool {
        self.mode.is_none() && self.background.is_none()
    }

    pub(crate) fn colors(self, system_dark: bool) -> (Theme, Option<[u8; 3]>) {
        match self.mode.unwrap_or_default() {
            Mode::Auto if system_dark => (Theme::Dark, None),
            Mode::Dark => (Theme::Dark, None),
            _ => (Theme::Light, self.background),
        }
    }

    pub(crate) fn edit(
        self,
        system_dark: bool,
        draft: &mut Option<String>,
        edit: Edit,
    ) -> Option<Self> {
        let mut next = self;
        let light = self.colors(system_dark).0 == Theme::Light;
        match edit {
            Edit::Mode(mode) => next.mode = Some(mode),
            Edit::Background(value) if light && value.len() <= 7 => *draft = Some(value),
            Edit::ApplyBackground if light => {
                next.background = Some(parse_rgb(draft.as_deref()?)?);
                *draft = None;
            }
            _ => {}
        }
        (next != self).then_some(next)
    }

    pub(crate) fn controls<'a>(self, system_dark: bool, draft: Option<&str>) -> Element<'a, Edit> {
        let [r, g, b] = self.background.unwrap_or([255; 3]);
        let stored = format!("#{r:02X}{g:02X}{b:02X}");
        let light = self.colors(system_dark).0 == Theme::Light;
        row![
            pick_list(
                [Mode::Auto, Mode::Light, Mode::Dark],
                Some(self.mode.unwrap_or_default()),
                Edit::Mode
            ),
            text_input("Canvas color (#RRGGBB)", draft.unwrap_or(&stored))
                .width(140)
                .on_input_maybe(light.then_some(Edit::Background)),
            button("Apply background").on_press_maybe(
                (light && draft.and_then(parse_rgb).is_some()).then_some(Edit::ApplyBackground)
            ),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .wrap()
        .into()
    }
}

pub(crate) fn parse_rgb(value: &str) -> Option<[u8; 3]> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let number = u32::from_str_radix(hex, 16).ok()?.to_be_bytes();
    Some([number[1], number[2], number[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_appearance_uses_the_canvas_override_only_with_the_light_palette() {
        let preferences = Preferences {
            mode: None,
            background: Some([18, 52, 86]),
        };
        let mut draft = None;
        assert_eq!(
            preferences.colors(false),
            (Theme::Light, Some([18, 52, 86]))
        );
        assert_eq!(preferences.colors(true), (Theme::Dark, None));
        assert!(
            preferences
                .edit(true, &mut draft, Edit::Background("#123456".into()))
                .is_none()
        );
        assert!(draft.is_none());
        let light = preferences
            .edit(true, &mut draft, Edit::Mode(Mode::Light))
            .unwrap();
        assert!(
            light
                .edit(true, &mut draft, Edit::Background("#AABBCC".into()))
                .is_none()
        );
        let updated = light.edit(true, &mut draft, Edit::ApplyBackground).unwrap();
        assert_eq!(updated.colors(true), (Theme::Light, Some([170, 187, 204])));
        assert!(draft.is_none());
    }
}
