use std::f32::consts::TAU;

use iced::widget::canvas::{Frame, Path, Stroke, Text};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, alignment};

use super::Effect;

const RADAR_GREEN: Color = Color::from_rgb8(53, 208, 138);
const HOLO_CYAN: Color = Color::from_rgb8(127, 231, 255);
const GHOST_RED: Color = Color::from_rgb8(255, 43, 43);

pub(super) fn draw(effect: Effect, frame: &mut Frame<Renderer>, seconds: f32) {
    let size = frame.size();
    if !seconds.is_finite()
        || !size.width.is_finite()
        || !size.height.is_finite()
        || size.width <= 0.0
        || size.height <= 0.0
    {
        return;
    }
    let seconds = seconds.max(0.0);
    frame.with_clip(Rectangle::new(Point::ORIGIN, size), |frame| match effect {
        Effect::DigitalRain => digital_rain(frame, size, seconds),
        Effect::DeepSpaceRadar => deep_space_radar(frame, size, seconds),
        Effect::HologramHud => hologram_hud(frame, size, seconds),
        Effect::Poltergeist => poltergeist(frame, size, seconds),
        _ => {}
    });
}

fn digital_rain(frame: &mut Frame<Renderer>, size: Size, seconds: f32) {
    const GLYPHS: &[u8] = b"01<>+*=$#%&@!?;:^~";
    const COLUMN_WIDTH: f32 = 14.0;
    const ROW_HEIGHT: f32 = 15.0;
    const MAX_COLUMNS: u32 = 64;

    let columns = ((size.width / COLUMN_WIDTH) as u32).clamp(1, MAX_COLUMNS);
    let rows = (size.height / ROW_HEIGHT).floor() + 1.0;
    let tick = (seconds * 20.0) as u32;
    for column in 0..columns {
        let key = column.wrapping_mul(0x9e37_79b9);
        let trail = 6 + mix(key ^ 5) % 9;
        let speed = 4.0 + fraction(key ^ 13) * 6.0;
        let travel = rows + trail as f32 + 1.0 + fraction(key ^ 29) * 14.0;
        let position = seconds * speed - fraction(key ^ 47) * (rows + 16.0);
        if position <= -1.0 {
            continue;
        }
        let position = position.max(0.0);
        let head = position.rem_euclid(travel).floor();
        let cycle = (position / travel) as u32;
        let x = column as f32 * COLUMN_WIDTH + 2.0;
        let distance = ((2.0 * x / size.width) - 1.0).abs();
        let edge = if distance > 0.7 { 1.6 } else { 0.6 + distance };
        for offset in 0..trail {
            let row = head - offset as f32;
            if !(0.0..=rows).contains(&row) {
                continue;
            }
            let glyph_key = key ^ (row as u32).rotate_left(12) ^ cycle.rotate_left(20);
            let (glyph_key, color) = if offset == 0 {
                let flicker = mix(glyph_key ^ tick);
                let glyph = if flicker.is_multiple_of(4) {
                    flicker
                } else {
                    glyph_key
                };
                (
                    glyph,
                    Color::from_rgba(0.71, 1.0, 0.78, (0.16 * edge).min(0.30)),
                )
            } else {
                let fade = 1.0 - offset as f32 / trail as f32;
                let alpha = ((0.03 + 0.07 * fade) * edge).min(0.14);
                (glyph_key, Color::from_rgba(0.0, 1.0, 0.4, alpha))
            };
            let glyph = char::from(GLYPHS[mix(glyph_key) as usize % GLYPHS.len()]);
            frame.fill_text(Text {
                content: glyph.to_string(),
                position: Point::new(x, (row + 1.0) * ROW_HEIGHT),
                color,
                size: Pixels(13.0),
                font: Font::MONOSPACE,
                align_y: alignment::Vertical::Bottom,
                ..Text::default()
            });
        }
    }
}

fn deep_space_radar(frame: &mut Frame<Renderer>, size: Size, seconds: f32) {
    const EXTENT: f32 = TAU * 32.0 / 360.0;
    const CONTACTS: Pulses = Pulses {
        first: 1.5,
        cadence: 4.0,
        jitter: 1.0,
        seed: 83,
    };
    // The moving sweep and fading rings can paint outside Tiny Skia's stroke damage.
    frame.fill_rectangle(Point::ORIGIN, size, Color::TRANSPARENT);
    let center = Point::new(size.width * 0.5, size.height * 0.5);
    let radius = size.width.hypot(size.height) * 0.5;
    let angle = seconds.rem_euclid(8.0) * TAU / 8.0;
    let at_angle = |angle: f32| {
        Point::new(
            center.x + angle.cos() * radius,
            center.y - angle.sin() * radius,
        )
    };
    let sector = Path::new(|path| {
        path.move_to(center);
        for segment in 0..=12 {
            path.line_to(at_angle(angle + EXTENT * segment as f32 / 12.0));
        }
        path.close();
    });
    frame.fill(&sector, opacity(RADAR_GREEN, 0.045));
    frame.stroke(
        &Path::line(center, at_angle(angle + EXTENT)),
        stroke(RADAR_GREEN, 0.10, 1.5),
    );
    let ring_base = size.width.min(size.height);
    for factor in [0.28, 0.50, 0.72] {
        frame.stroke(
            &Path::circle(center, ring_base * factor / 1.2),
            stroke(RADAR_GREEN, 0.05, 1.0),
        );
    }
    for (slot, age) in CONTACTS.nearby(seconds).filter(|&(_, age)| age < 2.5) {
        let contact = Point::new(
            center.x + (fraction(slot ^ 71) - 0.5) * size.width * 0.8,
            center.y + (fraction(slot ^ 137) - 0.5) * size.height * 0.8,
        );
        let fade = 1.0 - age / 2.5;
        frame.fill(
            &Path::circle(contact, 2.0),
            opacity(RADAR_GREEN, 0.16 * fade),
        );
        frame.stroke(
            &Path::circle(contact, 4.0 + age * 10.0),
            stroke(RADAR_GREEN, 0.10 * fade, 1.0),
        );
    }
    corners(frame, size, RADAR_GREEN, 22.0, 0.22);
}

fn hologram_hud(frame: &mut Frame<Renderer>, size: Size, seconds: f32) {
    const FLICKERS: Pulses = Pulses {
        first: 3.0,
        cadence: 7.0,
        jitter: 1.5,
        seed: 113,
    };
    frame.fill_rectangle(Point::ORIGIN, size, opacity(HOLO_CYAN, 0.035));
    for band in [0.0, 1.0, 2.0] {
        let speed = 14.0 + band * 9.0;
        let height = 26.0 + band * 16.0;
        let travel = size.height + height;
        let y = (seconds * speed + band * travel / 3.0).rem_euclid(travel) - height;
        frame.fill_rectangle(
            Point::new(0.0, y),
            Size::new(size.width, height),
            opacity(HOLO_CYAN, 0.05),
        );
        frame.fill_rectangle(
            Point::new(0.0, y + height * 0.4),
            Size::new(size.width, 1.0),
            opacity(Color::WHITE, 0.02),
        );
    }
    let spacing = 5.0_f32.max(size.height / 512.0);
    let lines = Path::new(|path| {
        for line in 0..(size.height / spacing).ceil() as u16 {
            path.rectangle(
                Point::new(0.0, f32::from(line) * spacing),
                Size::new(size.width, 1.0),
            );
        }
    });
    frame.fill(&lines, opacity(Color::BLACK, 0.08));
    let breath = 0.30 + 0.10 * (0.5 + 0.5 * (seconds * 1.5).sin());
    corners(frame, size, HOLO_CYAN, 26.0, breath);
    if FLICKERS
        .nearby(seconds)
        .any(|(slot, age)| age < 0.035 * (1 + mix(slot) % 2) as f32)
    {
        frame.fill_rectangle(Point::ORIGIN, size, opacity(HOLO_CYAN, 0.08));
    }
}

fn poltergeist(frame: &mut Frame<Renderer>, size: Size, seconds: f32) {
    const STATIC: Pulses = Pulses {
        first: 4.0,
        cadence: 10.5,
        jitter: 2.5,
        seed: 181,
    };
    const FLASH: Pulses = Pulses {
        first: 14.0,
        cadence: 35.0,
        jitter: 7.5,
        seed: 193,
    };
    let area = size.width * size.height;
    let tick = (seconds / 0.035) as u32;
    draw_grain(
        frame,
        size,
        ((area / 40_000.0) as u32).clamp(20, 160),
        tick,
        false,
        Color::from_rgba(0.0, 0.0, 0.0, 0.05),
    );

    if STATIC
        .nearby(seconds)
        .any(|(slot, age)| age < 0.30 + fraction(slot ^ 67) * 0.50)
    {
        draw_grain(
            frame,
            size,
            ((area / 9_000.0) as u32).clamp(120, 512),
            tick ^ 251,
            true,
            Color::from_rgba(0.78, 0.78, 0.78, 0.10),
        );
    }
    if let Some((_, age)) = FLASH.nearby(seconds).find(|&(_, age)| age < 0.60) {
        let color = if age < 0.08 {
            opacity(Color::WHITE, 0.07)
        } else {
            opacity(GHOST_RED, 0.05 * (1.0 - (age - 0.08) / 0.52))
        };
        frame.fill_rectangle(Point::ORIGIN, size, color);
    }

    let breath = 0.5 + 0.5 * (seconds * TAU / 7.0).sin();
    let alpha = 0.12 + 0.14 * breath;
    let edge = 30.0_f32.max(size.width.min(size.height) * 0.06);
    let horizontal = edge.min(size.height * 0.5);
    let vertical = edge.min(size.width * 0.5);
    let color = opacity(Color::BLACK, alpha);
    vertical_bands(frame, size, vertical, color);
    frame.fill_rectangle(Point::ORIGIN, Size::new(size.width, horizontal), color);
    frame.fill_rectangle(
        Point::new(0.0, size.height - horizontal),
        Size::new(size.width, horizontal),
        color,
    );
    vertical_bands(
        frame,
        size,
        (edge * 2.0).min(size.width * 0.5),
        opacity(Color::BLACK, alpha * 0.5),
    );
}

fn vertical_bands(frame: &mut Frame<Renderer>, size: Size, width: f32, color: Color) {
    let band = Size::new(width, size.height);
    frame.fill_rectangle(Point::ORIGIN, band, color);
    frame.fill_rectangle(Point::new(size.width - width, 0.0), band, color);
}

fn draw_grain(
    frame: &mut Frame<Renderer>,
    size: Size,
    count: u32,
    seed: u32,
    larger_dots: bool,
    color: Color,
) {
    let dots = Path::new(|path| {
        for point in 0..count {
            let key = mix(point ^ seed.rotate_left(11));
            let origin = Point::new(
                fraction(key ^ 197) * size.width,
                fraction(key ^ 241) * size.height,
            );
            let pixel = if larger_dots && key.is_multiple_of(5) {
                2.0
            } else {
                1.0
            };
            path.rectangle(origin, Size::new(pixel, pixel));
        }
    });
    frame.fill(&dots, color);
}

fn corners(frame: &mut Frame<Renderer>, size: Size, color: Color, length: f32, alpha: f32) {
    let inset = 10.0_f32.min(size.width.min(size.height) * 0.25);
    let length = length.min((size.width.min(size.height) - 2.0 * inset) * 0.5);
    let brackets = Path::new(|path| {
        for (x, dx) in [(inset, length), (size.width - inset, -length)] {
            for (y, dy) in [(inset, length), (size.height - inset, -length)] {
                path.move_to(Point::new(x + dx, y));
                path.line_to(Point::new(x, y));
                path.line_to(Point::new(x, y + dy));
            }
        }
    });
    frame.stroke(&brackets, stroke(color, alpha, 2.0));
}

fn opacity(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

fn stroke(color: Color, alpha: f32, width: f32) -> Stroke<'static> {
    Stroke::default()
        .with_color(opacity(color, alpha))
        .with_width(width)
}

#[derive(Clone, Copy)]
struct Pulses {
    first: f32,
    cadence: f32,
    jitter: f32,
    seed: u32,
}

impl Pulses {
    fn nearby(self, seconds: f32) -> impl Iterator<Item = (u32, f32)> {
        // Each event remains in its own slot. Check neighbors to find events whose jitter
        // crosses the nominal slot boundary without storing or replaying animation history.
        let slot = ((seconds - self.first) / self.cadence).floor() as u32;
        (slot.saturating_sub(1)..=slot.saturating_add(1)).filter_map(move |index| {
            let offset = if index == 0 {
                0.0
            } else {
                (fraction(index ^ self.seed) * 2.0 - 1.0) * self.jitter
            };
            let time = self.first + index as f32 * self.cadence + offset;
            (seconds >= time).then_some((index, seconds - time))
        })
    }
}

fn fraction(value: u32) -> f32 {
    (mix(value) >> 8) as f32 / 16_777_216.0
}

fn mix(mut value: u32) -> u32 {
    value = (value ^ (value >> 16)).wrapping_mul(0x7feb_352d);
    value = (value ^ (value >> 15)).wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}
