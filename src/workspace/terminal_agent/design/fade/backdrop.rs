use std::cell::RefCell;
use std::time::{Duration, Instant};

use iced::advanced::image::Image;
use iced::widget::image::Handle;
use iced::{Color, Rectangle, Renderer};

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const SAMPLE_SCALE: f32 = 0.5;
const RADIUS: usize = 3;
pub(in crate::workspace::terminal_agent::design) const CORNER_RADIUS: f32 = 10.0;

#[derive(Default)]
pub(in crate::workspace::terminal_agent::design) struct Backdrop(RefCell<Option<Frame>>);

struct Frame {
    bounds: Rectangle,
    background: Color,
    sampled: Instant,
    image: Image,
}

impl Backdrop {
    pub(in crate::workspace::terminal_agent::design) fn clear(&self) {
        *self.0.borrow_mut() = None;
    }

    pub(super) fn image(
        &self,
        renderer: &mut Renderer,
        bounds: Rectangle,
        visible: Rectangle,
        background: Color,
    ) -> Option<Image> {
        if visible != bounds {
            return None;
        }
        let mut frame = self.0.borrow_mut();
        let now = Instant::now();
        if let Some(cached) = &*frame
            && cached.bounds == bounds
            && cached.background == background
            && now.saturating_duration_since(cached.sampled) < REFRESH_INTERVAL
        {
            return Some(cached.image.clone());
        }

        let mut pixels = renderer.snapshot_region(bounds, SAMPLE_SCALE, background)?;
        let (width, height) = (pixels.width(), pixels.height());
        blur(pixels.data_mut(), width as usize, height as usize);
        let mut rgba = pixels.take();
        round_corners(&mut rgba, width as usize, height as usize);
        let image = Image::new(Handle::from_rgba(width, height, rgba));
        *frame = Some(Frame {
            bounds,
            background,
            sampled: now,
            image: image.clone(),
        });
        Some(image)
    }
}

fn round_corners(rgba: &mut [u8], width: usize, height: usize) {
    // Tiny Skia raster images clip to rectangles, so round the sampled image itself.
    let radius = (CORNER_RADIUS * SAMPLE_SCALE).min(width.min(height) as f32 / 2.0);
    let extent = radius.ceil() as usize;
    for y in 0..extent {
        for x in 0..extent {
            let dx = (radius - x as f32 - 0.5).max(0.0);
            let dy = (radius - y as f32 - 0.5).max(0.0);
            let coverage = (radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0);
            let alpha = (coverage * 255.0).round() as u8;
            for row in [y, height - 1 - y] {
                for column in [x, width - 1 - x] {
                    rgba[(row * width + column) * 4 + 3] = alpha;
                }
            }
        }
    }
}

fn blur(pixels: &mut [u8], width: usize, height: usize) {
    // Three separable box passes approximate a Gaussian on the reduced image.
    let mut scratch = pixels.to_vec();
    for _ in 0..3 {
        blur_axis(pixels, &mut scratch, (width, height), (4, 4 * width));
        blur_axis(&scratch, pixels, (height, width), (4 * width, 4));
    }
}

fn blur_axis(
    source: &[u8],
    destination: &mut [u8],
    (length, lines): (usize, usize),
    (pixel_stride, line_stride): (usize, usize),
) {
    const WINDOW: u32 = (2 * RADIUS + 1) as u32;
    for line in 0..lines {
        let base = line * line_stride;
        let index = |position: usize| base + position.min(length - 1) * pixel_stride;
        let mut sum = [0; 3];
        for position in 0..WINDOW as usize {
            let sample = index(position.saturating_sub(RADIUS));
            for (channel, value) in sum.iter_mut().enumerate() {
                *value += u32::from(source[sample + channel]);
            }
        }
        for position in 0..length {
            let out = index(position);
            let incoming = index(position + RADIUS + 1);
            let outgoing = index(position.saturating_sub(RADIUS));
            for (channel, value) in sum.iter_mut().enumerate() {
                destination[out + channel] = ((*value + WINDOW / 2) / WINDOW) as u8;
                *value += u32::from(source[incoming + channel]);
                *value -= u32::from(source[outgoing + channel]);
            }
        }
    }
}
