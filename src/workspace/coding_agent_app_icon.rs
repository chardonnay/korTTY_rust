//! Window icons for the Windows coding-agent badge.

use std::sync::OnceLock;

use iced::Task;
use iced::window::{self, Icon, icon};
use image::{ImageFormat, RgbaImage, imageops};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

const EDGE: u32 = 256;
const CENTRE: f32 = 198.0;
const CELL: f32 = 10.0;
const OVERFLOW: usize = 10;
// Original MIT-licensed artwork from Java 1ebbe724ee84, src/main/resources/icon/kortty_icon.png.
const LOGO: &[u8] = include_bytes!("coding_agent_app_icon/kortty_icon.png");

// Small heavy digits stay legible when the native taskbar scales the icon down.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];
const PLUS: [u8; 5] = [0b000, 0b010, 0b111, 0b010, 0b000];

static BASE: OnceLock<Option<RgbaImage>> = OnceLock::new();
static ICONS: [OnceLock<Option<Icon>>; OVERFLOW + 1] = [const { OnceLock::new() }; OVERFLOW + 1];

/// Returns the icon for a main window. Zero restores the plain application icon.
pub(super) fn windows_icon(count: usize) -> Option<Icon> {
    let bucket = count.min(OVERFLOW);
    ICONS[bucket].get_or_init(|| render(bucket)).clone()
}

/// Changes both native icon slots after confirming this request is still current.
pub(super) fn apply_windows_icon(
    id: window::Id,
    count: usize,
    is_current: impl FnOnce() -> bool + Send + 'static,
) -> Task<bool> {
    let Some(icon) = windows_icon(count) else {
        return Task::done(false);
    };
    let (rgba, size) = icon.into_raw();
    window::run(id, move |native| {
        is_current()
            && native.window_handle().is_ok_and(|window| {
                kortty_windowing::set_app_window_icon(window, rgba, size.width, size.height)
            })
    })
}

fn render(bucket: usize) -> Option<Icon> {
    let mut base = BASE
        .get_or_init(|| {
            let logo = image::load_from_memory_with_format(LOGO, ImageFormat::Png)
                .ok()?
                .into_rgba8();
            Some(imageops::resize(
                &logo,
                EDGE,
                EDGE,
                imageops::FilterType::Lanczos3,
            ))
        })
        .as_ref()?
        .clone();
    if bucket != 0 {
        imageops::overlay(&mut base, &badge(bucket)?, 0, 0);
    }
    icon::from_rgba(base.into_raw(), EDGE, EDGE).ok()
}

fn badge(bucket: usize) -> Option<RgbaImage> {
    let mut pixmap = Pixmap::new(EDGE, EDGE)?;
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    let outer = PathBuilder::from_circle(CENTRE, CENTRE, 55.0)?;
    pixmap.fill_path(
        &outer,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    paint.set_color_rgba8(0xd9, 0x30, 0x25, 255);
    let inner = PathBuilder::from_circle(CENTRE, CENTRE, 49.0)?;
    pixmap.fill_path(
        &inner,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    paint.set_color_rgba8(255, 255, 255, 255);
    let mut draw = |left: f32, glyph: &[u8; 5]| {
        for (row, &bits) in glyph.iter().enumerate() {
            for column in 0..3 {
                if bits & (0b100 >> column) != 0 {
                    let x = left + column as f32 * CELL;
                    let y = CENTRE + (row as f32 - 2.5) * CELL;
                    if let Some(rect) = Rect::from_xywh(x, y, CELL, CELL) {
                        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                    }
                }
            }
        }
    };
    if bucket == OVERFLOW {
        draw(CENTRE - 3.5 * CELL, &DIGITS[9]);
        draw(CENTRE + 0.5 * CELL, &PLUS);
    } else {
        draw(CENTRE - 1.5 * CELL, &DIGITS[bucket]);
    }
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    RgbaImage::from_raw(EDGE, EDGE, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_agent_windows_icon_restores_plain_and_caps_overflow() {
        let (plain, size) = windows_icon(0).unwrap().into_raw();
        let (one, _) = windows_icon(1).unwrap().into_raw();
        let (nine, _) = windows_icon(9).unwrap().into_raw();
        let (overflow, _) = windows_icon(10).unwrap().into_raw();
        assert_eq!((size.width, size.height), (EDGE, EDGE));
        let row = 158;
        let col = 198;
        let start = ((row * EDGE + col) * 4) as usize;
        assert_eq!(&one[start..start + 4], &[0xd9, 0x30, 0x25, 255]);
        assert_eq!(&one[..100], &plain[..100]);
        assert_ne!(one, plain);
        assert_ne!(one, nine);
        assert_ne!(nine, overflow);
        assert_eq!(overflow, windows_icon(usize::MAX).unwrap().into_raw().0);
    }
}
