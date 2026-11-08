use std::f32::consts::TAU;

use iced::widget::canvas::{Frame, Path, Stroke, Text};
use iced::{Color, Font, Pixels, Point, Renderer, Size, alignment, font};

use super::Effect;

const FRAME_SECONDS: f32 = 0.035;
const MAX_SCANLINES: usize = 2048;
const MAX_GRAIN_POINTS: usize = 4096;

const AMBER: Color = Color::from_rgb8(255, 176, 0);
const AMBER_BRIGHT: Color = Color::from_rgb8(255, 214, 96);
const VHS_MAGENTA: Color = Color::from_rgb8(255, 80, 200);
const VHS_CYAN: Color = Color::from_rgb8(80, 220, 255);
const NEON_MAGENTA: Color = Color::from_rgb8(255, 46, 136);
const NEON_CYAN: Color = Color::from_rgb8(0, 229, 255);
const GRID_PINK: Color = Color::from_rgb8(255, 113, 206);

pub(super) fn draw(effect: Effect, frame: &mut Frame<Renderer>, seconds: f32) {
    if !seconds.is_finite()
        || !frame.width().is_finite()
        || !frame.height().is_finite()
        || frame.width() <= 0.0
        || frame.height() <= 0.0
    {
        return;
    }
    let seconds = seconds.max(0.0);
    match effect {
        Effect::AmberCrt90 => amber_crt(frame, seconds),
        Effect::Vhs1987 => vhs(frame, seconds),
        Effect::CommodoreHeritage => commodore(frame, seconds),
        Effect::TypewriterNoir => typewriter(frame, seconds),
        Effect::NeonCity => neon_city(frame, seconds),
        Effect::SynthwaveHorizon => synthwave(frame, seconds),
        _ => {}
    }
}

fn amber_crt(frame: &mut Frame<Renderer>, seconds: f32) {
    let (width, height) = (frame.width(), frame.height());
    let mut random = Noise::for_frame(seconds, 10);
    let flicker = 0.02 + 0.02 * wave(seconds, 2.1) + random.unit() * 0.008;
    rect(frame, 0.0, 0.0, width, height, alpha(AMBER, flicker));
    scanlines(frame, 0.0, 3.0, alpha(Color::BLACK, 0.22));
    scanlines(frame, 1.0, 7.0, alpha(AMBER, 0.04));

    let band = (seconds % 6.0) / 6.0 * (height + 64.0) - 64.0;
    for slice in 0..8 {
        let y = band + 8.0 * slice as f32;
        let brightness = 0.055 * (slice + 1) as f32 / 8.0;
        rect(frame, 0.0, y, width, 9.0, alpha(AMBER_BRIGHT, brightness));
    }
    vignette(frame, 0.18);
}

fn vhs(frame: &mut Frame<Renderer>, seconds: f32) {
    let (width, height) = (frame.width(), frame.height());
    let mut random = Noise::for_frame(seconds, 20);
    let count = ((width * height / 30_000.0) as usize).clamp(40, 512);
    for _ in 0..count {
        let (x, y) = (random.unit() * width, random.unit() * height);
        rect(frame, x, y, 1.0, 1.0, alpha(Color::WHITE, 0.04));
    }
    let band = height - 22.0;
    rect(
        frame,
        0.0,
        band,
        width,
        22.0,
        Color::from_rgba(0.6, 0.6, 0.6, 0.05),
    );
    for _ in 0..8 + random.below(7) {
        let y = band + random.unit() * 20.0;
        let strip = 20.0 + random.unit() * 60.0;
        let x = random.unit() * width - strip / 2.0;
        rect(
            frame,
            x,
            y,
            strip,
            2.0,
            alpha(Color::WHITE, 0.10 + random.unit() * 0.10),
        );
    }
    for _ in 0..3 {
        let y = random.unit() * height;
        let x = random.unit() * width * 0.5;
        let length = width * (0.2 + random.unit() * 0.3);
        rect(frame, x, y, length, 1.0, alpha(VHS_MAGENTA, 0.04));
        rect(frame, x + 2.0, y + 1.0, length, 1.0, alpha(VHS_CYAN, 0.04));
    }
    if let Some((_, age)) = event_age(seconds, 5.0, 8.0, 20.0, 21)
        && age <= 1.5
    {
        let y = height - age / 1.5 * (height + 40.0);
        rect(frame, 0.0, y, width, 26.0, alpha(Color::WHITE, 0.06));
        rect(frame, 0.0, y - 2.0, width, 2.0, alpha(VHS_MAGENTA, 0.06));
        rect(frame, 0.0, y + 26.0, width, 2.0, alpha(VHS_CYAN, 0.06));
    }

    let x = width - 74.0;
    let opacity = (0.42 + 0.08 * phase_sin(seconds, 9.0) + random.unit() * 0.04).clamp(0.25, 0.55);
    let color = alpha(Color::WHITE, opacity);
    let triangle = Path::new(|path| {
        path.move_to(Point::new(x, 16.0));
        path.line_to(Point::new(x, 26.0));
        path.line_to(Point::new(x + 9.0, 21.0));
        path.close();
    });
    frame.fill(&triangle, color);
    frame.fill_text(Text {
        content: "PLAY".into(),
        position: Point::new(x + 15.0, 25.0),
        size: Pixels(13.0),
        color,
        font: Font {
            weight: font::Weight::Bold,
            ..Font::MONOSPACE
        },
        align_y: alignment::Vertical::Bottom,
        ..Text::default()
    });
    vignette(frame, 0.10);
}

fn commodore(frame: &mut Frame<Renderer>, seconds: f32) {
    const STRIPES: [Color; 5] = [
        Color::BLACK,
        Color::WHITE,
        Color::from_rgb8(136, 57, 50),
        Color::from_rgb8(103, 182, 189),
        Color::from_rgb8(191, 206, 114),
    ];
    let (width, height) = (frame.width(), frame.height());
    border(frame, 16.0, Color::from_rgba8(165, 159, 230, 0.26));
    scanlines(frame, 0.0, 4.0, alpha(Color::BLACK, 0.10));
    if let Some((_, age)) = event_age(seconds, 3.0, 7.4, 15.4, 30)
        && age <= 1.4
    {
        let scroll = (f64::from(seconds) * 80.0) % STRIPES.len() as f64;
        for index in 0..4 {
            let y = index as f32 * 4.0;
            let stripe = STRIPES[(index + scroll as usize) % STRIPES.len()];
            let color = alpha(stripe, 0.55);
            rect(frame, 0.0, y, width, 4.0, color);
            rect(frame, 0.0, height - 16.0 + y, width, 4.0, color);
        }
    }
}

fn typewriter(frame: &mut Frame<Renderer>, seconds: f32) {
    let (width, height) = (frame.width(), frame.height());
    let seed = (u64::from(width.to_bits()) << 32) | u64::from(height.to_bits());
    let mut random = Noise(seed);
    let count = ((width * height / 2500.0) as usize).clamp(60, MAX_GRAIN_POINTS);
    let dark = Color::from_rgba8(107, 91, 69, 0.05);
    let light = Color::from_rgba8(255, 248, 232, 0.05);
    for _ in 0..count {
        let (x, y) = (random.unit() * width, random.unit() * height);
        let color = if random.below(2) == 0 { dark } else { light };
        rect(frame, x, y, 1.0, 1.0, color);
    }

    let opacity = 0.06 + 0.03 * wave(seconds, TAU / 9.0);
    let shadow = Color::from_rgba8(59, 47, 30, opacity);
    let edge = (width.min(height) * 0.06).max(28.0);
    border(frame, edge, shadow);
    rect(
        frame,
        0.0,
        height - edge * 1.8,
        width,
        edge * 0.8,
        alpha(shadow, opacity * 0.6),
    );
}

fn neon_city(frame: &mut Frame<Renderer>, seconds: f32) {
    let (width, height) = (frame.width(), frame.height());
    let breath = wave(seconds, TAU / 3.0);
    rect(
        frame,
        0.0,
        0.0,
        width,
        height * 0.55,
        alpha(NEON_MAGENTA, 0.025 + 0.02 * breath),
    );
    rect(
        frame,
        0.0,
        height * 0.45,
        width,
        height * 0.55,
        alpha(NEON_CYAN, 0.025 + 0.02 * (1.0 - breath)),
    );
    scanlines(frame, 0.0, 4.0, alpha(Color::BLACK, 0.10));

    if let Some((event, age)) = event_age(seconds, 2.0, 3.0, 9.0, 40) {
        let mut strips = Noise(event ^ 41);
        if age < (2 + strips.below(4)) as f32 * FRAME_SECONDS {
            for _ in 0..2 + strips.below(3) {
                let y = strips.unit() * height;
                let thickness = 5.0 + strips.unit() * 10.0;
                rect(frame, 0.0, y, width, thickness, alpha(Color::WHITE, 0.05));
                rect(frame, -6.0, y, width, thickness, alpha(NEON_MAGENTA, 0.11));
                rect(frame, 6.0, y, width, thickness, alpha(NEON_CYAN, 0.11));
            }
            let mut random = Noise::for_frame(seconds, 42);
            if random.below(3) == 0 {
                rect(
                    frame,
                    0.0,
                    random.unit() * height,
                    width,
                    1.0,
                    alpha(Color::WHITE, 0.12),
                );
            }
        }
    }
    vignette(frame, 0.10);
}

fn synthwave(frame: &mut Frame<Renderer>, seconds: f32) {
    let (width, height) = (frame.width(), frame.height());
    let breath = wave(seconds, TAU / 4.0);
    rect(
        frame,
        0.0,
        0.0,
        width,
        height,
        alpha(GRID_PINK, 0.02 + 0.02 * breath),
    );
    rect(
        frame,
        0.0,
        height * 0.5,
        width,
        height * 0.5,
        Color::from_rgba8(138, 43, 226, 0.025),
    );
    scanlines(frame, 0.0, 5.0, alpha(Color::BLACK, 0.08));

    let grid_height = (height * 0.16).max(40.0);
    let horizon = height - grid_height;
    let center = Point::new(width * 0.5, horizon);
    line(
        frame,
        Point::new(0.0, horizon),
        Point::new(width, horizon),
        alpha(GRID_PINK, 0.25),
    );
    for index in -6..=6 {
        let x = center.x + index as f32 * (width / 6.0);
        line(frame, center, Point::new(x, height), alpha(GRID_PINK, 0.15));
    }
    for index in 0..7 {
        let progress = ((seconds % 2.5) / 2.5 + index as f32 / 7.0) % 1.0;
        let y = horizon + progress * progress * grid_height;
        line(
            frame,
            Point::new(0.0, y),
            Point::new(width, y),
            alpha(GRID_PINK, 0.06 + 0.14 * progress),
        );
    }
}

fn rect(frame: &mut Frame<Renderer>, x: f32, y: f32, width: f32, height: f32, color: Color) {
    frame.fill_rectangle(Point::new(x, y), Size::new(width, height), color);
}

fn line(frame: &mut Frame<Renderer>, start: Point, end: Point, color: Color) {
    frame.stroke(&Path::line(start, end), Stroke::default().with_color(color));
}

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

fn scanlines(frame: &mut Frame<Renderer>, offset: f32, spacing: f32, color: Color) {
    let (width, height) = (frame.width(), frame.height());
    let spacing = spacing.max(height / MAX_SCANLINES as f32);
    for index in 0..MAX_SCANLINES {
        let y = offset + index as f32 * spacing;
        if y >= height {
            break;
        }
        rect(frame, 0.0, y, width, 1.0, color);
    }
}

fn border(frame: &mut Frame<Renderer>, edge: f32, color: Color) {
    let (width, height) = (frame.width(), frame.height());
    rect(frame, 0.0, 0.0, edge, height, color);
    rect(frame, width - edge, 0.0, edge, height, color);
    rect(frame, 0.0, 0.0, width, edge, color);
    rect(frame, 0.0, height - edge, width, edge, color);
}

fn vignette(frame: &mut Frame<Renderer>, opacity: f32) {
    let edge = (frame.width().min(frame.height()) * 0.035).max(24.0);
    border(frame, edge, alpha(Color::BLACK, opacity));
}

fn phase_sin(seconds: f32, frequency: f32) -> f32 {
    (seconds % (TAU / frequency) * frequency).sin()
}

fn wave(seconds: f32, frequency: f32) -> f32 {
    0.5 + 0.5 * phase_sin(seconds, frequency)
}

// A bounded offset per event retains the Java gap range. Looking at the neighboring
// slots recovers the current event even when an obscured pane skipped redraws.
fn event_age(
    seconds: f32,
    first: f64,
    minimum: f64,
    maximum: f64,
    seed: u64,
) -> Option<(u64, f32)> {
    let elapsed = f64::from(seconds) - first;
    if elapsed < 0.0 {
        return None;
    }
    let period = (minimum + maximum) / 2.0;
    let start = |index: u64| {
        if index == 0 {
            return 0.0;
        }
        let noise = f64::from(Noise(index ^ seed).unit());
        index as f64 * period + (noise - 0.5) * (maximum - minimum) / 2.0
    };
    let index = (elapsed / period) as u64;
    [index.saturating_add(1), index, index.saturating_sub(1)]
        .into_iter()
        .find_map(|index| {
            let age = elapsed - start(index);
            (age >= 0.0).then_some((index, age as f32))
        })
}

struct Noise(u64);

impl Noise {
    fn for_frame(seconds: f32, seed: u64) -> Self {
        Self((seconds / FRAME_SECONDS) as u64 ^ seed)
    }

    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut bits = self.0;
        bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((bits ^ (bits >> 31)) >> 40) as f32 / 16_777_216.0
    }

    fn below(&mut self, upper: usize) -> usize {
        (self.unit() * upper as f32) as usize
    }
}
