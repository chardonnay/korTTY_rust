//! Avoid scanning the masked-out part of a large solid background.
use crate::clip_mask::{PixelRect, scan_rect};
use crate::core::Rectangle;
use tiny_skia::{
    BlendMode, Color, FillRule, Mask, Paint, Path, PathBuilder, PathSegment,
    PixmapMut, Point, Rect, Shader, Transform,
};

pub fn clear(pixels: &mut PixmapMut<'_>, bounds: Rectangle, color: Color) {
    if let Some(rect) = scan_rect(bounds, pixels.width(), pixels.height()) {
        fill_pixels(pixels, rect, color);
    } else {
        let rect =
            Rect::from_xywh(bounds.x, bounds.y, bounds.width, bounds.height)
                .expect("Create damage rectangle");
        pixels.fill_path(
            &PathBuilder::from_rect(rect),
            &Paint {
                shader: Shader::SolidColor(color),
                anti_alias: false,
                blend_mode: BlendMode::Source,
                ..Paint::default()
            },
            FillRule::default(),
            Transform::identity(),
            None,
        );
    }
}

fn fill_pixels(pixels: &mut PixmapMut<'_>, rect: PixelRect, color: Color) {
    let stride = pixels.width() as usize;
    let color = color.premultiply().to_color_u8();
    for row in pixels
        .pixels_mut()
        .chunks_exact_mut(stride)
        .take(rect.bottom)
        .skip(rect.top)
    {
        row[rect.left..rect.right].fill(color);
    }
}

pub fn fill_clipped(
    pixels: &mut PixmapMut<'_>,
    path: &Path,
    paint: &Paint<'_>,
    rule: FillRule,
    transform: Transform,
    mask: &Mask,
    clip: Rectangle,
) -> bool {
    use PathSegment::{Close, LineTo, MoveTo};
    if !paint.is_solid_color()
        || transform.kx != 0.0
        || transform.ky != 0.0
        || path.len() != 5
    {
        return false;
    }

    let mut segments = path.segments();
    let (
        Some(MoveTo(a)),
        Some(LineTo(b)),
        Some(LineTo(c)),
        Some(LineTo(d)),
        Some(Close),
    ) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    )
    else {
        return false;
    };
    let horizontal_first =
        a.y == b.y && b.x == c.x && c.y == d.y && d.x == a.x;
    let vertical_first = a.x == b.x && b.y == c.y && c.x == d.x && d.y == a.y;
    if !(horizontal_first || vertical_first) {
        return false;
    }

    let mut corners = [a, c];
    transform.map_points(&mut corners);
    let [Point { x: ax, y: ay }, Point { x: cx, y: cy }] = corners;
    if ![ax, ay, cx, cy].into_iter().all(f32::is_finite) {
        return false;
    }
    let (left, top, right, bottom) =
        (ax.min(cx), ay.min(cy), ax.max(cx), ay.max(cy));

    if !paint.anti_alias
        && !paint.force_hq_pipeline
        && paint.blend_mode == BlendMode::SourceOver
        && let Shader::SolidColor(color) = paint.shader
        && color.is_opaque()
        && let Some(rect) = scan_rect(
            Rectangle {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            },
            pixels.width(),
            pixels.height(),
        )
        && let Some(clip) = scan_rect(clip, pixels.width(), pixels.height())
    {
        let intersection = PixelRect {
            left: rect.left.max(clip.left),
            top: rect.top.max(clip.top),
            right: rect.right.min(clip.right),
            bottom: rect.bottom.min(clip.bottom),
        };
        if intersection.left < intersection.right
            && intersection.top < intersection.bottom
        {
            fill_pixels(pixels, intersection, color);
        }
        return true;
    }

    // Clip at pixel boundaries so the cut cannot introduce antialiasing into
    // the visible part. Keep the original mask and original fractional edges.
    let left = left.max(clip.x.floor());
    let top = top.max(clip.y.floor());
    let right = right.min((clip.x + clip.width).ceil());
    let bottom = bottom.min((clip.y + clip.height).ceil());
    let Some(rect) = Rect::from_ltrb(left, top, right, bottom) else {
        return false;
    };
    pixels.fill_path(
        &PathBuilder::from_rect(rect),
        paint,
        rule,
        Transform::identity(),
        Some(mask),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    #[test]
    fn clipped_rect_matches_masked_path_at_fractional_edges() {
        let path = PathBuilder::from_rect(
            Rect::from_ltrb(2.125, 2.75, 35.875, 30.5).unwrap(),
        );
        for (clip, transform) in [
            (
                Rectangle::new(
                    crate::core::Point::new(3.375, 11.125),
                    crate::core::Size::new(41.75, 12.0),
                ),
                Transform::from_row(1.25, 0.0, 0.0, 1.25, -1.375, 0.5),
            ),
            (
                Rectangle::new(
                    crate::core::Point::new(0.0, 1.5),
                    crate::core::Size::new(50.0, 5.125),
                ),
                Transform::identity(),
            ),
        ] {
            let mut mask = Mask::new(64, 64).unwrap();
            let clip_path = PathBuilder::from_rect(
                Rect::from_xywh(clip.x, clip.y, clip.width, clip.height)
                    .unwrap(),
            );
            mask.fill_path(
                &clip_path,
                FillRule::EvenOdd,
                false,
                Transform::identity(),
            );

            for anti_alias in [false, true] {
                for alpha in [96, 255] {
                    let mut paint = Paint {
                        anti_alias,
                        ..Paint::default()
                    };
                    paint.set_color_rgba8(203, 71, 137, alpha);
                    let mut reference = Pixmap::new(64, 64).unwrap();
                    reference.fill(Color::from_rgba8(17, 35, 71, 180));
                    let mut result = reference.clone();
                    reference.fill_path(
                        &path,
                        &paint,
                        FillRule::EvenOdd,
                        transform,
                        Some(&mask),
                    );
                    assert!(fill_clipped(
                        &mut result.as_mut(),
                        &path,
                        &paint,
                        FillRule::EvenOdd,
                        transform,
                        &mask,
                        clip,
                    ));
                    assert_eq!(result.data(), reference.data());
                }
            }
        }
    }
}
