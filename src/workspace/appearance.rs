use std::fmt;
use std::time::{Duration, Instant};

use iced::mouse;
use iced::widget::canvas::{self, Action, Canvas, Frame, Geometry, Path};
use iced::{Color, Element, Event, Point, Rectangle, Renderer, Theme};
use serde::{Deserialize, Deserializer, Serialize};

use crate::terminal_theme::Rgb;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Design {
    #[serde(rename = "atlantafx-primer-dark")]
    AtlantaFxPrimerDark,
    #[serde(rename = "atlantafx-primer-light")]
    AtlantaFxPrimerLight,
    #[serde(rename = "atlantafx-nord-dark")]
    AtlantaFxNordDark,
    #[serde(rename = "atlantafx-nord-light")]
    AtlantaFxNordLight,
    #[serde(rename = "atlantafx-cupertino-dark")]
    AtlantaFxCupertinoDark,
    #[serde(rename = "atlantafx-cupertino-light")]
    AtlantaFxCupertinoLight,
    #[serde(rename = "atlantafx-dracula")]
    AtlantaFxDracula,
    #[default]
    Normal,
    MatrixTerminal,
    HolographicInterface,
    KlingonTactical,
    ElegantDark,
    AmberCrt,
    #[serde(rename = "synthwave-84")]
    Synthwave84,
    GruvboxRetro,
    NordArctic,
    Dracula,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Palette {
    pub(super) background: Rgb,
    pub(super) foreground: Rgb,
    pub(super) muted: Rgb,
    pub(super) accent: Rgb,
    pub(super) border: Rgb,
}

impl Palette {
    const fn new(background: u32, foreground: u32, muted: u32, accent: u32, border: u32) -> Self {
        Self {
            background: Rgb::from_hex(background),
            foreground: Rgb::from_hex(foreground),
            muted: Rgb::from_hex(muted),
            accent: Rgb::from_hex(accent),
            border: Rgb::from_hex(border),
        }
    }
}

impl Design {
    pub(super) const ALL: [Self; 17] = [
        Self::AtlantaFxPrimerDark,
        Self::AtlantaFxPrimerLight,
        Self::AtlantaFxNordDark,
        Self::AtlantaFxNordLight,
        Self::AtlantaFxCupertinoDark,
        Self::AtlantaFxCupertinoLight,
        Self::AtlantaFxDracula,
        Self::Normal,
        Self::MatrixTerminal,
        Self::HolographicInterface,
        Self::KlingonTactical,
        Self::ElegantDark,
        Self::AmberCrt,
        Self::Synthwave84,
        Self::GruvboxRetro,
        Self::NordArctic,
        Self::Dracula,
    ];

    pub(super) fn step(self, forward: bool) -> Self {
        let len = Self::ALL.len();
        let offset = if forward { 1 } else { len - 1 };
        Self::ALL[(self as usize + offset) % len]
    }

    pub(super) fn palette(self) -> Option<Palette> {
        self.definition().1
    }

    pub(super) fn label(self) -> &'static str {
        self.definition().0
    }

    fn definition(self) -> (&'static str, Option<Palette>) {
        use self::Design::*;

        // AppDesignStyleSupport in Java 1ebbe724ee84, with translucent
        // text/dim colors composited against the native palette background.
        let (label, colors) = match self {
            AtlantaFxPrimerDark => (
                "AtlantaFX Primer Dark",
                [0x0d1117, 0xc9d1d9, 0x8b949e, 0x58a6ff, 0x30363d],
            ),
            AtlantaFxPrimerLight => (
                "AtlantaFX Primer Light",
                [0xffffff, 0x24292f, 0x57606a, 0x0969da, 0xd0d7de],
            ),
            AtlantaFxNordDark => (
                "AtlantaFX Nord Dark",
                [0x2e3440, 0xd8dee9, 0x7b88a1, 0x88c0d0, 0x4c566a],
            ),
            AtlantaFxNordLight => (
                "AtlantaFX Nord Light",
                [0xeceff4, 0x2e3440, 0x4c566a, 0x5e81ac, 0xd8dee9],
            ),
            AtlantaFxCupertinoDark => (
                "AtlantaFX Cupertino Dark",
                [0x1c1c1e, 0xf2f2f7, 0x8e8e93, 0x0a84ff, 0x38383a],
            ),
            AtlantaFxCupertinoLight => (
                "AtlantaFX Cupertino Light",
                [0xf2f2f7, 0x1c1c1e, 0x8e8e93, 0x007aff, 0xd1d1d6],
            ),
            AtlantaFxDracula => (
                "AtlantaFX Dracula",
                [0x282a36, 0xf8f8f2, 0x6272a4, 0xbd93f9, 0x44475a],
            ),
            Normal => return ("Default", None),
            MatrixTerminal => (
                "Matrix Terminal",
                [0x080c09, 0x00ff88, 0x00c96e, 0x00ff88, 0x00ff88],
            ),
            HolographicInterface => (
                "Holographic Interface",
                [0x000000, 0x00d4ff, 0x006a80, 0x00d4ff, 0x00d4ff],
            ),
            KlingonTactical => (
                "Klingon Tactical",
                [0x0d0906, 0xdb9a9a, 0x802c35, 0xff3c5a, 0xff3c5a],
            ),
            ElegantDark => (
                "Elegant Dark",
                [0x1a1c20, 0xe2e4e8, 0x8b9099, 0xc8a96e, 0x35373b],
            ),
            AmberCrt => (
                "Amber CRT",
                [0x1a0f02, 0xffb000, 0x9c6a12, 0xffcc44, 0xffb000],
            ),
            Synthwave84 => (
                "Synthwave '84",
                [0x1a0b2e, 0xf5e1ff, 0x9d8fc7, 0xff2e88, 0xff2e88],
            ),
            GruvboxRetro => (
                "Gruvbox Retro",
                [0x282828, 0xebdbb2, 0xa89984, 0xfe8019, 0xfe8019],
            ),
            NordArctic => (
                "Nord Arctic",
                [0x2e3440, 0xeceff4, 0x7b88a1, 0x88c0d0, 0x88c0d0],
            ),
            Dracula => (
                "Dracula",
                [0x282a36, 0xf8f8f2, 0x6272a4, 0xbd93f9, 0xbd93f9],
            ),
        };
        let [background, foreground, muted, accent, border] = colors;
        (
            label,
            Some(Palette::new(background, foreground, muted, accent, border)),
        )
    }

    fn pulse_color(self) -> Option<Color> {
        let hex = match self {
            Self::AmberCrt => 0xffcc44,
            Self::Synthwave84 => 0xff2e88,
            Self::GruvboxRetro => 0xfe8019,
            Self::Dracula => 0xff79c6,
            _ => return None,
        };
        Some(Rgb::from_hex(hex).color())
    }
}

impl fmt::Display for Design {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct Preferences {
    #[serde(deserialize_with = "read_design_id")]
    pub(super) design: Design,
    pub(super) animations: bool,
}

fn read_design_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Design, D::Error> {
    let id = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    let normalized = id.trim().to_ascii_lowercase().replace('_', "-");
    let id = serde::de::value::StrDeserializer::<D::Error>::new(&normalized);
    Ok(Design::deserialize(id).unwrap_or_default())
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            design: Design::Normal,
            animations: true,
        }
    }
}

impl Preferences {
    pub(super) fn fresh() -> Self {
        Self {
            design: Design::AtlantaFxPrimerDark,
            ..Self::default()
        }
    }

    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(super) fn status_cursor<Message: 'static>(
        self,
        focused: bool,
    ) -> Option<Element<'static, Message>> {
        if !self.animations {
            return None;
        }
        let color = self.design.pulse_color()?;
        Some(
            Canvas::new(Pulse { color, focused })
                .width(18)
                .height(14)
                .into(),
        )
    }
}

const PULSE_HALF_PERIOD_MS: u128 = 1300;
const PULSE_FRAME: Duration = Duration::from_millis(50);

struct Pulse {
    color: Color,
    focused: bool,
}

#[derive(Default)]
struct PulseState {
    start: Option<Instant>,
    elapsed: Duration,
}

impl<Message> canvas::Program<Message> for Pulse {
    type State = PulseState;

    fn update(
        &self,
        state: &mut PulseState,
        event: &Event,
        _: Rectangle,
        _: mouse::Cursor,
    ) -> Option<Action<Message>> {
        if !self.focused {
            *state = PulseState::default();
            return None;
        }
        let Event::Window(iced::window::Event::RedrawRequested(now)) = event else {
            return None;
        };
        let now = *now;
        state.elapsed = now.saturating_duration_since(*state.start.get_or_insert(now));
        Some(Action::request_redraw_at(now + PULSE_FRAME))
    }

    fn draw(
        &self,
        state: &PulseState,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let opacity = if self.focused {
            pulse_opacity(state.elapsed)
        } else {
            1.0
        };
        frame.fill(
            &Path::circle(center, 7.0),
            Color {
                a: 0.19 * opacity,
                ..self.color
            },
        );
        frame.fill(
            &Path::circle(center, 5.0),
            Color {
                a: opacity,
                ..self.color
            },
        );
        vec![frame.into_geometry()]
    }
}

fn pulse_opacity(elapsed: Duration) -> f32 {
    let millis = elapsed.as_millis() % (2 * PULSE_HALF_PERIOD_MS);
    let distance = PULSE_HALF_PERIOD_MS.abs_diff(millis);
    let phase = distance as f32 / PULSE_HALF_PERIOD_MS as f32;
    let eased = phase * phase * (3.0 - 2.0 * phase);
    0.45 + 0.55 * eased
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn design_ids_and_navigation_match_the_java_order() {
        let ids = [
            "atlantafx-primer-dark",
            "atlantafx-primer-light",
            "atlantafx-nord-dark",
            "atlantafx-nord-light",
            "atlantafx-cupertino-dark",
            "atlantafx-cupertino-light",
            "atlantafx-dracula",
            "normal",
            "matrix-terminal",
            "holographic-interface",
            "klingon-tactical",
            "elegant-dark",
            "amber-crt",
            "synthwave-84",
            "gruvbox-retro",
            "nord-arctic",
            "dracula",
        ];
        let len = Design::ALL.len();
        for (index, (design, id)) in Design::ALL.into_iter().zip(ids).enumerate() {
            let encoded = serde_json::to_value(design).unwrap();
            assert_eq!(encoded, id);
            assert_eq!(serde_json::from_value::<Design>(encoded).unwrap(), design);
            assert_eq!(design.step(true), Design::ALL[(index + 1) % len]);
            assert_eq!(design.step(false), Design::ALL[(index + len - 1) % len]);
        }
        assert_eq!(
            Design::AtlantaFxPrimerDark
                .palette()
                .unwrap()
                .background
                .bytes(),
            [0x0d, 0x11, 0x17]
        );
        assert_eq!(Design::Normal.label(), "Default");
        assert!(Design::Normal.palette().is_none());
        let unsupported: Preferences =
            serde_json::from_str(r#"{"design":"unknown","animations":false}"#).unwrap();
        assert_eq!(unsupported.design, Design::Normal);
        assert!(!unsupported.animations);
    }

    #[test]
    fn decorative_pulse_runs_only_for_the_four_designs_in_a_focused_window() {
        assert_eq!(
            Design::ALL
                .into_iter()
                .filter(|design| design.pulse_color().is_some())
                .count(),
            4
        );
        let disabled = Preferences {
            design: Design::AmberCrt,
            animations: false,
        };
        assert!(disabled.status_cursor::<()>(true).is_none());
        let now = Instant::now();
        let event = Event::Window(iced::window::Event::RedrawRequested(now));
        let mut pulse = Pulse {
            color: Color::WHITE,
            focused: true,
        };
        let mut state = PulseState::default();
        assert!(
            <Pulse as canvas::Program<()>>::update(
                &pulse,
                &mut state,
                &event,
                Rectangle::default(),
                mouse::Cursor::Unavailable
            )
            .is_some()
        );
        assert_eq!(state.start, Some(now));
        pulse.focused = false;
        assert!(
            <Pulse as canvas::Program<()>>::update(
                &pulse,
                &mut state,
                &event,
                Rectangle::default(),
                mouse::Cursor::Unavailable
            )
            .is_none()
        );
        assert!(state.start.is_none());
        assert_eq!(pulse_opacity(Duration::ZERO), 1.0);
        assert_eq!(pulse_opacity(Duration::from_millis(1300)), 0.45);
        assert_eq!(pulse_opacity(Duration::from_millis(2600)), 1.0);
    }
}
