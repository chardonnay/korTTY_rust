//! Built-in terminal effects from the Java reference's effect pack.

use std::collections::BTreeSet;

use iced::widget::canvas::{self, Canvas};
use iced::{Color, Element, Length, Rectangle, Renderer, Theme, mouse};
use serde::{Deserialize, Serialize};

use crate::terminal_theme::{Appearance, CursorStyle, Rgb};

mod mother;
mod retro;
mod signal;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    #[serde(rename = "amber-crt-90")]
    AmberCrt90,
    #[serde(rename = "commodore-blue")]
    CommodoreHeritage,
    DeepSpaceRadar,
    DigitalRain,
    HologramHud,
    Mother,
    NeonCity,
    Poltergeist,
    SynthwaveHorizon,
    TypewriterNoir,
    #[serde(rename = "vhs-1987")]
    Vhs1987,
}

impl Effect {
    pub(crate) const ALL: [Self; 11] = [
        Self::AmberCrt90,
        Self::CommodoreHeritage,
        Self::DeepSpaceRadar,
        Self::DigitalRain,
        Self::HologramHud,
        Self::Mother,
        Self::NeonCity,
        Self::Poltergeist,
        Self::SynthwaveHorizon,
        Self::TypewriterNoir,
        Self::Vhs1987,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::AmberCrt90 => "Amber CRT '90",
            Self::CommodoreHeritage => "Commodore Heritage",
            Self::DeepSpaceRadar => "Deep Space Radar",
            Self::DigitalRain => "Digital Rain",
            Self::HologramHud => "Hologram HUD",
            Self::Mother => "MU/TH/UR 6000",
            Self::NeonCity => "Neon City",
            Self::Poltergeist => "Poltergeist",
            Self::SynthwaveHorizon => "Synthwave Horizon",
            Self::TypewriterNoir => "Typewriter Noir",
            Self::Vhs1987 => "VHS 1987",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::AmberCrt90 => "Amber phosphor, scanlines and a rolling refresh band.",
            Self::CommodoreHeritage => "Commodore 64 colors, a block cursor and loader bars.",
            Self::DeepSpaceRadar => "A radar sweep, faint blips and tactical frame corners.",
            Self::DigitalRain => "Green falling glyphs on a dark terminal.",
            Self::HologramHud => "Blue interference bands and translucent HUD brackets.",
            Self::Mother => "Green phosphor, scanlines and static from MU/TH/UR 6000.",
            Self::NeonCity => "Neon glow, colored glitch tears and flicker.",
            Self::Poltergeist => "Monochrome vignette, static bursts and intermittent flashes.",
            Self::SynthwaveHorizon => "A glowing perspective grid in an eighties palette.",
            Self::TypewriterNoir => "Sepia paper, ink and a typewriter carriage.",
            Self::Vhs1987 => "Tracking noise, rolling distortion and a PLAY overlay.",
        }
    }

    fn palette(self) -> (u32, u32, u32, CursorStyle) {
        use CursorStyle::{BlinkBar, BlinkBlock, BlinkUnderline, SteadyBlock};
        match self {
            Self::AmberCrt90 => (0xFFB000, 0x0A0500, 0xFFD75F, BlinkBlock),
            Self::CommodoreHeritage => (0xA59FE6, 0x40318D, 0xA59FE6, SteadyBlock),
            Self::DeepSpaceRadar => (0x35D08A, 0x020D07, 0x9CFFC9, SteadyBlock),
            Self::DigitalRain => (0x00FF66, 0x020A04, 0xB4FFC8, BlinkBlock),
            Self::HologramHud => (0x9FE8FF, 0x04141C, 0xCFF6FF, BlinkBar),
            Self::Mother => (0x19FF4C, 0x000000, 0xF2F2F2, BlinkBlock),
            Self::NeonCity => (0xEAE6FF, 0x0B0716, 0x00E5FF, BlinkBlock),
            Self::Poltergeist => (0xC9C9C9, 0x050505, 0xFF2B2B, BlinkBlock),
            Self::SynthwaveHorizon => (0xF8E7FF, 0x14061F, 0xFF71CE, BlinkBlock),
            Self::TypewriterNoir => (0x241C12, 0xE9DFC8, 0x241C12, BlinkUnderline),
            Self::Vhs1987 => (0xE8E8E8, 0x0A0A0C, 0xFFFFFF, BlinkBlock),
        }
    }

    pub(crate) fn colors(self) -> (Color, Color) {
        let (foreground, background, _, _) = self.palette();
        (
            Rgb::from_hex(background).color(),
            Rgb::from_hex(foreground).color(),
        )
    }

    pub(crate) fn appearance(self, mut base: Appearance) -> Appearance {
        let (foreground, background, cursor, style) = self.palette();
        base.foreground = Rgb::from_hex(foreground);
        base.background = Rgb::from_hex(background);
        base.cursor = Rgb::from_hex(cursor);
        (base.cursor_shape, base.cursor_blink) = style.behavior();
        base
    }

    pub(crate) fn draw_overlay(self, frame: &mut canvas::Frame<Renderer>, seconds: f32) {
        if frame.width() <= 0.0 || frame.height() <= 0.0 {
            return;
        }
        match self {
            Self::Mother => mother::draw(frame, seconds),
            Self::DeepSpaceRadar | Self::DigitalRain | Self::HologramHud | Self::Poltergeist => {
                signal::draw(self, frame, seconds);
            }
            _ => retro::draw(self, frame, seconds),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) enabled: bool,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    disabled: BTreeSet<Effect>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: BTreeSet::new(),
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn effect_enabled(&self, effect: Effect) -> bool {
        !self.disabled.contains(&effect)
    }

    pub(crate) fn allows(&self, effect: Effect) -> bool {
        self.enabled && self.effect_enabled(effect)
    }

    pub(crate) fn set_effect_enabled(&mut self, effect: Effect, enabled: bool) {
        if enabled {
            self.disabled.remove(&effect);
        } else {
            self.disabled.insert(effect);
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Selection {
    pub(crate) effect: Effect,
    pub(crate) speed: u8,
}

impl Selection {
    pub(crate) fn speed(self) -> u8 {
        self.speed.clamp(1, 99)
    }

    pub(crate) fn overlay<'a, Message: 'a>(self, seconds: f32) -> Element<'a, Message> {
        self.overlay_with_flash(seconds, None)
    }

    pub(crate) fn overlay_with_flash<'a, Message: 'a>(
        self,
        seconds: f32,
        flash: Option<Flash>,
    ) -> Element<'a, Message> {
        Canvas::new(Overlay {
            effect: self.effect,
            seconds,
            flash,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Flash {
    pub(crate) y: f32,
    pub(crate) height: f32,
    pub(crate) alpha: f32,
}

struct Overlay {
    effect: Effect,
    seconds: f32,
    flash: Option<Flash>,
}

impl<Message> canvas::Program<Message> for Overlay {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        self.effect.draw_overlay(&mut frame, self.seconds);
        if let Some(flash) = self.flash {
            mother::draw_flash(&mut frame, flash);
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_effect_ids_roundtrip_with_reference_names() {
        let ids = [
            "amber-crt-90",
            "commodore-blue",
            "deep-space-radar",
            "digital-rain",
            "hologram-hud",
            "mother",
            "neon-city",
            "poltergeist",
            "synthwave-horizon",
            "typewriter-noir",
            "vhs-1987",
        ];
        for (effect, id) in Effect::ALL.into_iter().zip(ids) {
            assert_eq!(serde_json::to_value(effect).unwrap(), id);
            assert_eq!(serde_json::from_value::<Effect>(id.into()).unwrap(), effect);
        }
    }
}
