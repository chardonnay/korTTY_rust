use crate::core::Rectangle;
use tiny_skia::{FillRule, Mask, PathBuilder, Rect, Transform};

#[derive(Clone, Copy)]
pub(crate) struct PixelRect {
    pub(crate) left: usize,
    pub(crate) top: usize,
    pub(crate) right: usize,
    pub(crate) bottom: usize,
}

impl PixelRect {
    fn conservative(bounds: Rectangle, width: u32, height: u32) -> Self {
        Self {
            left: ((bounds.x.floor() - 1.0) as usize).min(width as usize),
            top: ((bounds.y.floor() - 1.0) as usize).min(height as usize),
            right: (((bounds.x + bounds.width).ceil() + 1.0) as usize)
                .min(width as usize),
            bottom: (((bounds.y + bounds.height).ceil() + 1.0) as usize)
                .min(height as usize),
        }
    }

    fn fill_mask(self, mask: &mut Mask, alpha: u8) {
        let stride = mask.width() as usize;
        for row in mask
            .data_mut()
            .chunks_exact_mut(stride)
            .take(self.bottom)
            .skip(self.top)
        {
            row[self.left..self.right].fill(alpha);
        }
    }
}

/// Matches Tiny Skia 0.11's non-antialiased scan for axis-aligned rectangles
/// contained in an untiled buffer. Edge clipping and very thin paths use the
/// original scanner because those can change its rounding and early-outs.
pub(crate) fn scan_rect(
    bounds: Rectangle,
    width: u32,
    height: u32,
) -> Option<PixelRect> {
    let right = bounds.x + bounds.width;
    let bottom = bounds.y + bounds.height;
    if !(width <= 8191
        && height <= 8191
        && bounds.width >= 1.0
        && bounds.height >= 1.0
        && bounds.x >= 0.0
        && bounds.y >= 0.0
        && right <= width as f32
        && bottom <= height as f32)
    {
        return None;
    }
    // The scanner truncates each coordinate to 26.6 fixed point before
    // rounding the vertical edge positions and the horizontal scan spans.
    let round =
        |coordinate: f32| (((coordinate * 64.0) as u32 + 32) >> 6) as usize;
    Some(PixelRect {
        left: round(bounds.x),
        top: round(bounds.y),
        right: round(right),
        bottom: round(bottom),
    })
}

pub(crate) struct ClipMask<'a> {
    mask: &'a mut Mask,
    previous: Option<(Rectangle, PixelRect)>,
}

impl<'a> ClipMask<'a> {
    pub(crate) fn new(mask: &'a mut Mask) -> Self {
        Self {
            mask,
            previous: None,
        }
    }

    pub(crate) fn clip(
        &mut self,
        bounds: Rectangle,
        drawing: Rectangle,
    ) -> Option<&Mask> {
        if drawing.is_within(&bounds) {
            return None;
        }
        self.set(bounds);
        Some(self.mask)
    }

    fn set(&mut self, bounds: Rectangle) {
        if let Some((previous, pixels)) = self.previous {
            if previous == bounds {
                return;
            }
            pixels.fill_mask(self.mask, 0);
        } else {
            // Renderer::draw accepts a caller-owned mask with arbitrary contents.
            self.mask.clear();
        }
        let (width, height) = (self.mask.width(), self.mask.height());
        let pixels = if let Some(pixels) = scan_rect(bounds, width, height) {
            pixels.fill_mask(self.mask, 255);
            pixels
        } else {
            fill_path(self.mask, bounds);
            PixelRect::conservative(bounds, width, height)
        };
        self.previous = Some((bounds, pixels));
    }
}

fn fill_path(mask: &mut Mask, bounds: Rectangle) {
    let path = PathBuilder::from_rect(
        Rect::from_xywh(bounds.x, bounds.y, bounds.width, bounds.height)
            .expect("Create clip rectangle"),
    );
    mask.fill_path(&path, FillRule::EvenOdd, false, Transform::identity());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare_with_full_clear(width: u32, height: u32, bounds: &[Rectangle]) {
        let mut original = Mask::new(width, height).unwrap();
        let mut reused = Mask::new(width, height).unwrap();
        reused.data_mut().fill(137);
        let mut clip = ClipMask::new(&mut reused);
        for &rect in bounds {
            original.clear();
            fill_path(&mut original, rect);
            clip.set(rect);
            assert_eq!(original.data(), clip.mask.data(), "clip {rect:?}");
        }
    }

    #[test]
    fn clip_mask_matches_original_for_fractional_and_clipped_rectangles() {
        let mut bounds = Vec::new();
        for x in [-3.75, -0.507, -0.25, 0.0, 0.49, 0.5, 0.51, 2.75, 9.99, 12.0]
        {
            for y in [-2.0, -0.2, 0.0, 0.49, 0.5, 0.501, 4.25, 8.0] {
                for width in [0.02, 1.0, 3.72, 10.0] {
                    bounds.push(Rectangle {
                        x,
                        y,
                        width,
                        height: width * 0.78,
                    });
                }
            }
        }
        let first = bounds[0];
        bounds.extend([first, first]);
        compare_with_full_clear(12, 8, &bounds);
    }

    #[test]
    fn clip_mask_matches_original_for_tiled_masks() {
        compare_with_full_clear(
            8193,
            4,
            &[
                Rectangle {
                    x: 8189.25,
                    y: -0.6,
                    width: 3.7,
                    height: 4.6,
                },
                Rectangle {
                    x: 0.1,
                    y: 1.2,
                    width: 9.7,
                    height: 1.1,
                },
            ],
        );
    }
}
