//! Agent panel color derivation and migration from Java kortty 1ebbe724ee84.

use super::{AgentPanelColors, Rgb, Scheme};

pub(crate) fn resolved_panel_colors(scheme: Scheme, colors: AgentPanelColors) -> [Rgb; 6] {
    let dark = luminance(scheme.background) < 0.55;
    let accent = theme_accent(
        scheme,
        Rgb::from_hex(if dark { 0x4f9cf0 } else { 0x0b63ce }),
    );
    let surface = if dark {
        mix(scheme.background, scheme.foreground, 0.03)
    } else {
        scheme.background
    };
    let defaults = [
        mix(surface, accent, if dark { 0.07 } else { 0.05 }),
        mix(surface, accent, if dark { 0.30 } else { 0.22 }),
        scheme.foreground,
        mix(
            scheme.foreground,
            scheme.background,
            if dark { 0.35 } else { 0.45 },
        ),
        accent,
        Rgb::from_hex(if dark { 0xe36a4d } else { 0xb42318 }),
    ];
    std::array::from_fn(|index| colors[index].unwrap_or(defaults[index]))
}

pub(super) fn migrate_legacy_colors(
    scheme: Scheme,
    mut colors: AgentPanelColors,
) -> AgentPanelColors {
    // Theme.clearLegacyDerivedAgentPanelColors only replaces exact matches
    // of the former background, border and accent formulas.
    let dark = luminance(scheme.background) < 0.55;
    let accent = theme_accent(
        scheme,
        Rgb::from_hex(if dark { 0x18c26e } else { 0x087f5b }),
    );
    let legacy = [
        (
            0,
            mix(scheme.background, accent, if dark { 0.18 } else { 0.08 }),
        ),
        (
            1,
            mix(scheme.background, accent, if dark { 0.42 } else { 0.30 }),
        ),
        (4, accent),
    ];
    for (index, derived) in legacy {
        if colors[index] == Some(derived) {
            colors[index] = None;
        }
    }
    resolved_panel_colors(scheme, colors).map(Some)
}

fn theme_accent(scheme: Scheme, fallback: Rgb) -> Rgb {
    let cursor = scheme.cursor;
    if saturation(cursor) >= 0.12 && color_distance(cursor, scheme.background) >= 55 {
        cursor
    } else {
        fallback
    }
}

pub(crate) fn luminance(color: Rgb) -> f64 {
    let [red, green, blue] = color.bytes().map(f64::from);
    (0.299 * red + 0.587 * green + 0.114 * blue) / 255.0
}

fn saturation(color: Rgb) -> f64 {
    let [red, green, blue] = color.bytes();
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    if max == 0 {
        0.0
    } else {
        f64::from(max - min) / f64::from(max)
    }
}

fn color_distance(left: Rgb, right: Rgb) -> u16 {
    left.bytes()
        .into_iter()
        .zip(right.bytes())
        .map(|(left, right)| u16::from(left.abs_diff(right)))
        .sum()
}

pub(crate) fn mix(left: Rgb, right: Rgb, ratio: f64) -> Rgb {
    let left = left.bytes();
    let right = right.bytes();
    Rgb::from_bytes(std::array::from_fn(|index| {
        (f64::from(left[index]) * (1.0 - ratio) + f64::from(right[index]) * ratio).round() as u8
    }))
}
