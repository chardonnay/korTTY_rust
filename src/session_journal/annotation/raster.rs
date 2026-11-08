use std::io;

use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PremultipliedColorU8, Rect,
    Stroke, Transform,
};
use ttf_parser::{Face, OutlineBuilder};

use super::{Annotation, Kind, invalid, validate};
use crate::session_journal::MAX_PNG_PIXELS;

const MAX_RASTER_WORK: u64 = 64 * 1024 * 1024;
const MAX_OUTLINE_VERBS: usize = 1024 * 1024;
const BOLD: &[u8] = include_bytes!("../export/pdf/fonts/NotoSans-Bold.ttf");

/// Renders from the untouched image. Pixelation is completed before any visible vector marks.
pub(crate) fn render(base: &Pixmap, annotations: &[Annotation]) -> io::Result<Pixmap> {
    validate(annotations)?;
    if u64::from(base.width()) * u64::from(base.height()) > MAX_PNG_PIXELS {
        return Err(invalid("Journal screenshot exceeds the image size limit."));
    }
    let mut image = base.clone();
    let mut work = MAX_RASTER_WORK;
    let mut outline_budget = MAX_OUTLINE_VERBS;
    for mark in annotations
        .iter()
        .filter(|mark| mark.kind == Kind::Pixelate)
    {
        pixelate(&mut image, mark, &mut work)?;
    }
    let face = annotations
        .iter()
        .any(|mark| mark.kind == Kind::Text)
        .then(|| {
            Face::parse(BOLD, 0).map_err(|_| invalid("Invalid bundled journal annotation font."))
        })
        .transpose()?;
    for mark in annotations {
        match mark.kind {
            Kind::Pixelate => {}
            Kind::Pen | Kind::Box => stroke(&mut image, mark, &mut work)?,
            Kind::Text => {
                if let Some(face) = &face {
                    draw_text(&mut image, mark, face, &mut work, &mut outline_budget)?;
                }
            }
        }
    }
    Ok(image)
}

fn stroke(image: &mut Pixmap, mark: &Annotation, work: &mut u64) -> io::Result<()> {
    let (path, cap, join) = match mark.kind {
        Kind::Pen => {
            let mut path = PathBuilder::new();
            let [x, y] = mark.points[0];
            path.move_to(x, y);
            for &[x, y] in &mark.points[1..] {
                path.line_to(x, y);
            }
            (path.finish(), LineCap::Round, LineJoin::Round)
        }
        Kind::Box => {
            let [x, y, width, height] = rectangle(mark);
            (
                Rect::from_xywh(x, y, width, height).map(PathBuilder::from_rect),
                LineCap::Butt,
                LineJoin::Miter,
            )
        }
        _ => return Ok(()),
    };
    if let Some(path) = path {
        charge(work, path_area(image, &path, mark.stroke_width + 1.0))?;
        image.stroke_path(
            &path,
            &paint(mark.color, 255),
            &Stroke {
                width: mark.stroke_width,
                miter_limit: 10.0,
                line_cap: cap,
                line_join: join,
                ..Stroke::default()
            },
            Transform::identity(),
            None,
        );
    }
    Ok(())
}

fn rectangle(mark: &Annotation) -> [f32; 4] {
    let [x, y] = mark.points[0];
    let [width, height] = mark.points[1];
    [
        x.min(x + width),
        y.min(y + height),
        width.abs(),
        height.abs(),
    ]
}

fn pixelate(image: &mut Pixmap, mark: &Annotation, work: &mut u64) -> io::Result<()> {
    let [x, y] = mark.points[0].map(f64::from);
    let [width, height] = mark.points[1].map(f64::from);
    let stride = image.width() as usize;
    let rows = image.height() as usize;
    // Java rounds the signed origin, then clamps it independently of the rounded extent.
    let left = java_round(x.min(x + width)).clamp(0, stride as i32 - 1) as usize;
    let top = java_round(y.min(y + height)).clamp(0, rows as i32 - 1) as usize;
    let width = (java_round(width.abs()) as usize).min(stride - left);
    let height = (java_round(height.abs()) as usize).min(rows - top);
    charge(work, (width * height * 2) as u64)?;
    let block = java_round(f64::from(mark.stroke_width) * 2.0).max(8) as usize;
    let pixels = image.pixels_mut();
    for y in (top..top + height).step_by(block) {
        for x in (left..left + width).step_by(block) {
            let width = block.min(left + width - x);
            let height = block.min(top + height - y);
            average_block(pixels, stride, x, y, width, height);
        }
    }
    Ok(())
}

fn average_block(
    pixels: &mut [PremultipliedColorU8],
    stride: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) {
    let mut sums = [0_u64; 3];
    for row in y..y + height {
        let offset = row * stride + x;
        for pixel in &pixels[offset..offset + width] {
            let color = pixel.demultiply();
            sums[0] += u64::from(color.red());
            sums[1] += u64::from(color.green());
            sums[2] += u64::from(color.blue());
        }
    }
    let count = (width * height) as u64;
    let [red, green, blue] = sums.map(|sum| (sum / count) as u8);
    let color = PremultipliedColorU8::from_rgba(red, green, blue, 255).expect("opaque block color");
    for row in y..y + height {
        let offset = row * stride + x;
        pixels[offset..offset + width].fill(color);
    }
}

fn java_round(value: f64) -> i32 {
    (value + 0.5).floor() as i32
}

fn draw_text(
    image: &mut Pixmap,
    mark: &Annotation,
    face: &Face<'_>,
    work: &mut u64,
    outline_budget: &mut usize,
) -> io::Result<()> {
    let Some(path) = text_path(mark, face, outline_budget)? else {
        return Ok(());
    };
    charge(work, path_area(image, &path, 2.0) * 9)?;
    let halo = paint([0; 3], 140);
    for dx in -1..=1 {
        for dy in -1..=1 {
            if dx != 0 || dy != 0 {
                image.fill_path(
                    &path,
                    &halo,
                    FillRule::Winding,
                    Transform::from_translate(dx as f32, dy as f32),
                    None,
                );
            }
        }
    }
    image.fill_path(
        &path,
        &paint(mark.color, 255),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    Ok(())
}

fn text_path(mark: &Annotation, face: &Face<'_>, budget: &mut usize) -> io::Result<Option<Path>> {
    let scale = (java_round(f64::from(mark.font_size)) as f32) / f32::from(face.units_per_em());
    let [mut x, y] = mark.points[0];
    let fallback = face
        .glyph_index('?')
        .ok_or_else(|| invalid("Bundled annotation font lacks its fallback."))?;
    let mut path = PathBuilder::new();
    for character in mark
        .text
        .chars()
        .filter(|character| !character.is_control())
    {
        let glyph = face.glyph_index(character).unwrap_or(fallback);
        let mut outline = GlyphPath {
            path: &mut path,
            x,
            y,
            scale,
            budget,
            exhausted: false,
        };
        let _ = face.outline_glyph(glyph, &mut outline);
        if outline.exhausted {
            return Err(invalid(
                "Journal screenshot annotation text exceeds its outline limit.",
            ));
        }
        x += f32::from(face.glyph_hor_advance(glyph).unwrap_or_default()) * scale;
    }
    Ok(path.finish())
}

struct GlyphPath<'a> {
    path: &'a mut PathBuilder,
    x: f32,
    y: f32,
    scale: f32,
    budget: &'a mut usize,
    exhausted: bool,
}

impl GlyphPath<'_> {
    fn position(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x * self.scale, self.y - y * self.scale)
    }

    fn charge(&mut self) -> bool {
        if *self.budget == 0 {
            self.exhausted = true;
            return false;
        }
        *self.budget -= 1;
        true
    }
}

impl OutlineBuilder for GlyphPath<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.charge() {
            let (x, y) = self.position(x, y);
            self.path.move_to(x, y);
        }
    }

    fn line_to(&mut self, x: f32, y: f32) {
        if self.charge() {
            let (x, y) = self.position(x, y);
            self.path.line_to(x, y);
        }
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        if self.charge() {
            let (x1, y1) = self.position(x1, y1);
            let (x, y) = self.position(x, y);
            self.path.quad_to(x1, y1, x, y);
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        if self.charge() {
            let (x1, y1) = self.position(x1, y1);
            let (x2, y2) = self.position(x2, y2);
            let (x, y) = self.position(x, y);
            self.path.cubic_to(x1, y1, x2, y2, x, y);
        }
    }

    fn close(&mut self) {
        if self.charge() {
            self.path.close();
        }
    }
}

fn paint([red, green, blue]: [u8; 3], alpha: u8) -> Paint<'static> {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color_rgba8(red, green, blue, alpha);
    paint
}

fn path_area(image: &Pixmap, path: &Path, padding: f32) -> u64 {
    let bounds = path.bounds();
    let span = |start: f32, end: f32, limit: u32| {
        let limit = limit as f32;
        let start = (start - padding).floor().clamp(0.0, limit);
        let end = (end + padding).ceil().clamp(0.0, limit);
        (end - start) as u64
    };
    span(bounds.left(), bounds.right(), image.width())
        * span(bounds.top(), bounds.bottom(), image.height())
}

fn charge(work: &mut u64, cost: u64) -> io::Result<()> {
    *work = work.checked_sub(cost).ok_or_else(|| {
        invalid("Journal screenshot annotations exceed their rendering work limit.")
    })?;
    Ok(())
}
