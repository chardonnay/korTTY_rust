use iced::widget::canvas::{Frame, Path};
use iced::{Color, Point, Renderer, Size};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

const FRAME_SECONDS: f32 = 0.035;
const MAX_SCANLINES: usize = 2_048;
const MAX_NOISE_POINTS: usize = 2_048;
const NOISE_COLOR: Color = Color::from_rgba8(180, 255, 190, 0.06);

pub(super) fn draw_flash(frame: &mut Frame<Renderer>, flash: super::Flash) {
    let size = frame.size();
    let height = (flash.height + 2.0).min(size.height - flash.y);
    if height > 0.0 {
        let origin = Point::new(0.0, flash.y);
        frame.fill_rectangle(
            origin,
            Size::new(size.width, height),
            Color::from_rgba8(150, 255, 175, flash.alpha),
        );
        frame.fill_rectangle(
            origin,
            Size::new(size.width, (height * 0.45).max(1.0)),
            Color::from_rgba8(245, 255, 210, flash.alpha * 0.45),
        );
    }
}

pub(super) fn draw(frame: &mut Frame<Renderer>, seconds: f32) {
    let size = frame.size();
    if !seconds.is_finite()
        || !size.width.is_finite()
        || !size.height.is_finite()
        || size.width <= 0.0
        || size.height <= 0.0
    {
        return;
    }

    scanlines(frame, 0.0, 4.0, Color::from_rgba8(0, 0, 0, 0.28));
    scanlines(frame, 2.0, 8.0, Color::from_rgba8(40, 255, 80, 0.05));
    noise(frame, seconds.max(0.0));
    vignette(frame);
}

fn scanlines(frame: &mut Frame<Renderer>, offset: f32, spacing: f32, color: Color) {
    let Size { width, height } = frame.size();
    let spacing = spacing.max(height / MAX_SCANLINES as f32);
    let lines = Path::new(|path| {
        for index in 0..MAX_SCANLINES {
            let y = offset + index as f32 * spacing;
            if y >= height {
                break;
            }
            path.rectangle(Point::new(0.0, y), Size::new(width, 1.0));
        }
    });
    frame.fill(&lines, color);
}

fn noise(frame: &mut Frame<Renderer>, seconds: f32) {
    let Size { width, height } = frame.size();
    let points = ((width * height / 26_000.0) as usize).clamp(80, MAX_NOISE_POINTS);
    // Repaints during the same CRT tick use the same noise pattern.
    let mut random = StdRng::seed_from_u64((seconds / FRAME_SECONDS) as u64);
    let grain = Path::new(|path| {
        for _ in 0..points {
            let point = Point::new(random.gen_range(0.0..width), random.gen_range(0.0..height));
            path.rectangle(point, Size::new(1.0, 1.0));
        }
    });
    frame.fill(&grain, NOISE_COLOR);
}

fn vignette(frame: &mut Frame<Renderer>) {
    let Size { width, height } = frame.size();
    let edge = (width.min(height) * 0.035).max(24.0);
    let vertical = Size::new(edge.min(width), height);
    let horizontal = Size::new(width, edge.min(height));
    let shadow = Color::from_rgba8(0, 0, 0, 0.18);
    frame.fill_rectangle(Point::ORIGIN, vertical, shadow);
    frame.fill_rectangle(Point::new(width - vertical.width, 0.0), vertical, shadow);
    frame.fill_rectangle(Point::ORIGIN, horizontal, shadow);
    frame.fill_rectangle(
        Point::new(0.0, height - horizontal.height),
        horizontal,
        shadow,
    );
}
