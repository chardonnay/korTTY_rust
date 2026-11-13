use std::cell::RefCell;

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry};
use iced::widget::image::Handle;
use iced::{Point, Rectangle, Renderer, Size, Theme};
use kortty_terminal::{KittyPlacement, ViewportImage};
use tiny_skia::{ColorU8, FilterQuality, IntSize, Pixmap, PixmapMut, PixmapPaint, Transform};

use crate::session::Frame as TerminalFrame;
use crate::workspace::Message;

const MAX_LAYER_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub enum ImageLayer {
    BehindBackground,
    BehindText,
    AboveText,
}

impl ImageLayer {
    fn includes(self, z: i32) -> bool {
        let under_background = i32::MIN / 2;
        match self {
            Self::BehindBackground => z < under_background,
            Self::BehindText => (under_background..0).contains(&z),
            Self::AboveText => z >= 0,
        }
    }
}

pub struct KittyView<'a> {
    pub frame: &'a TerminalFrame,
    pub layer: ImageLayer,
    pub grid: Rectangle,
    pub cell: Size,
    pub scale: f32,
}

#[derive(Default)]
pub struct State(RefCell<Option<RasterLayer>>);

struct RasterLayer {
    viewport: Rectangle,
    cell: Size,
    scale: f32,
    images: Vec<ViewportImage>,
    bands: Option<Vec<RowBand>>,
    raster: Option<(Rectangle, Handle)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RowBand {
    raw: u16,
    display: u16,
    rows: u16,
}

fn row_bands(frame: &TerminalFrame) -> Option<Vec<RowBand>> {
    frame.inline.as_ref()?;
    let mut bands: Vec<RowBand> = Vec::new();
    for display in 0..frame.screen.rows {
        let Some(raw) = frame.display_raw_row(display) else {
            continue;
        };
        if let Some(last) = bands.last_mut()
            && last.raw + last.rows == raw
            && last.display + last.rows == display
        {
            last.rows += 1;
        } else {
            bands.push(RowBand {
                raw,
                display,
                rows: 1,
            });
        }
    }
    let identity = RowBand {
        raw: 0,
        display: 0,
        rows: frame.screen.rows,
    };
    (bands.as_slice() != [identity]).then_some(bands)
}

impl canvas::Program<Message> for KittyView<'_> {
    type State = State;

    fn draw(
        &self,
        state: &State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut cached = state.0.borrow_mut();
        let Some(viewport) = self.grid.intersection(&Rectangle::with_size(bounds.size())) else {
            *cached = None;
            return Vec::new();
        };
        let bands = row_bands(self.frame);
        let images = self
            .frame
            .screen
            .images
            .iter()
            .filter(|image| self.layer.includes(image.placement.z_index));
        if !cached.as_ref().is_some_and(|cache| {
            cache.viewport == viewport
                && cache.scale == self.scale
                && cache.cell == self.cell
                && cache.bands == bands
                && cache.images.iter().eq(images.clone())
        }) {
            let images = images.cloned().collect::<Vec<_>>();
            let raster = rasterize(&images, viewport, self.cell, self.scale, bands.as_deref()).map(
                |(bounds, pixels)| {
                    let (width, height) = (pixels.width(), pixels.height());
                    let rgba = pixels
                        .pixels()
                        .iter()
                        .flat_map(|pixel| {
                            let color = pixel.demultiply();
                            [color.red(), color.green(), color.blue(), color.alpha()]
                        })
                        .collect::<Vec<_>>();
                    (bounds, Handle::from_rgba(width, height, rgba))
                },
            );
            *cached = Some(RasterLayer {
                viewport,
                cell: self.cell,
                scale: self.scale,
                images,
                bands,
                raster,
            });
        }
        let Some((destination, image)) = cached.as_ref().and_then(|cache| cache.raster.as_ref())
        else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        frame.draw_image(*destination, image);
        vec![frame.into_geometry()]
    }
}

struct Band {
    clip: Rectangle,
    shift: f32,
}

impl Band {
    fn clipped(&self, mut bounds: Rectangle) -> Option<Rectangle> {
        bounds.y += self.shift;
        bounds.intersection(&self.clip)
    }
}

fn rasterize(
    images: &[ViewportImage],
    viewport: Rectangle,
    cell: Size,
    scale: f32,
    row_bands: Option<&[RowBand]>,
) -> Option<(Rectangle, Pixmap)> {
    let bands = row_bands.map(|rows| {
        rows.iter()
            .filter_map(|band| {
                let band_bounds = Rectangle::new(
                    Point::new(
                        viewport.x,
                        viewport.y + f32::from(band.display) * cell.height,
                    ),
                    Size::new(viewport.width, f32::from(band.rows) * cell.height),
                );
                Some(Band {
                    clip: band_bounds.intersection(&viewport)?,
                    shift: (f32::from(band.display) - f32::from(band.raw)) * cell.height,
                })
            })
            .collect::<Vec<_>>()
    });
    let destinations = images
        .iter()
        .filter_map(|image| {
            let placement = &image.placement;
            let bounds = Rectangle::new(
                Point::new(
                    viewport.x + (f32::from(placement.column) + placement.left as f32) * cell.width,
                    viewport.y + (image.row as f32 + placement.top as f32) * cell.height,
                ),
                Size::new(
                    placement.width as f32 * cell.width,
                    placement.height as f32 * cell.height,
                ),
            );
            let visible = match &bands {
                Some(bands) => bands
                    .iter()
                    .filter_map(|band| band.clipped(bounds))
                    .reduce(|left, right| left.union(&right)),
                None => bounds.intersection(&viewport),
            }?;
            Some((placement, bounds, visible))
        })
        .collect::<Vec<_>>();
    let area = destinations
        .iter()
        .map(|(_, _, clipped)| *clipped)
        .reduce(|left, right| left.union(&right))?;
    let left = (area.x * scale).floor();
    let top = (area.y * scale).floor();
    let width = (((area.x + area.width) * scale).ceil() - left) as u32;
    let height = (((area.y + area.height) * scale).ceil() - top) as u32;
    if u64::from(width) * u64::from(height) > MAX_LAYER_PIXELS {
        return None;
    }
    let mut pixmap = Pixmap::new(width, height)?;
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    for (placement, bounds, _) in destinations {
        let Some(source) = crop(placement) else {
            continue;
        };
        let scale_transform = Transform::from_scale(
            bounds.width * scale / source.width() as f32,
            bounds.height * scale / source.height() as f32,
        );
        let x = bounds.x * scale - left;
        if let Some(bands) = &bands {
            let stride = width as usize * 4;
            for band in bands.iter().filter(|band| band.clipped(bounds).is_some()) {
                // A device row belongs to the band that contains its pixel center.
                let pixel_row = |y: f32| ((y * scale - 0.5).ceil() - top).max(0.0) as u32;
                let first = pixel_row(band.clip.y).min(height);
                let end = pixel_row(band.clip.y + band.clip.height).min(height);
                if first >= end {
                    continue;
                }
                let bytes = &mut pixmap.data_mut()[first as usize * stride..end as usize * stride];
                let mut target = PixmapMut::from_bytes(bytes, width, end - first)?;
                let y = (bounds.y + band.shift) * scale - top - first as f32;
                let transform = scale_transform.post_translate(x, y);
                target.draw_pixmap(0, 0, source.as_ref(), &paint, transform, None);
            }
        } else {
            let transform = scale_transform.post_translate(x, bounds.y * scale - top);
            pixmap.draw_pixmap(0, 0, source.as_ref(), &paint, transform, None);
        }
    }
    let bounds = Rectangle::new(
        Point::new(left / scale, top / scale),
        Size::new(width as f32 / scale, height as f32 / scale),
    );
    Some((bounds, pixmap))
}

fn crop(placement: &KittyPlacement) -> Option<Pixmap> {
    let size = IntSize::from_wh(placement.source_width, placement.source_height)?;
    let width = size.width() as usize;
    let mut pixels = Vec::with_capacity(width * size.height() as usize * 4);
    for row in placement.source_y..placement.source_y + placement.source_height {
        let start =
            (row as usize * placement.image.width as usize + placement.source_x as usize) * 4;
        let row = placement.image.rgba.get(start..start + width * 4)?;
        for &[red, green, blue, alpha] in row.as_chunks::<4>().0 {
            let pixel = ColorU8::from_rgba(red, green, blue, alpha).premultiply();
            pixels.extend([pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]);
        }
    }
    Pixmap::from_vec(pixels, size)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::session::inline_history;
    use crate::terminal_agent::design::inline::Line;
    use crate::terminal_view::selection_test_frame;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use kortty_terminal::Terminal;

    #[test]
    fn graphics_raster_scales_the_selected_crop_at_its_cell_position() {
        let mut terminal = Terminal::new(8, 4);
        terminal.advance(b"\x1b[2;3H\x1b_Ga=T,f=24,s=2,v=1,x=1,w=1,c=2,r=1,q=2,C=1;/wAAAP8A\x1b\\");
        let screen = terminal.snapshot();
        let viewport = Rectangle::new(Point::new(5.0, 5.0), Size::new(80.0, 80.0));
        let (bounds, raster) =
            rasterize(&screen.images, viewport, Size::new(10.0, 20.0), 2.0, None)
                .expect("visible image");
        assert_eq!(
            bounds,
            Rectangle::new(Point::new(25.0, 25.0), Size::new(20.0, 20.0))
        );
        assert_eq!((raster.width(), raster.height()), (40, 40));
        assert_eq!(
            raster.pixel(20, 20).unwrap().demultiply(),
            ColorU8::from_rgba(0, 255, 0, 255)
        );
    }

    #[test]
    fn graphics_raster_keeps_only_the_visible_part_when_the_anchor_is_above_the_viewport() {
        let mut terminal = Terminal::new(8, 4);
        terminal.advance(b"\x1b_Ga=T,f=24,s=1,v=6,c=1,r=3,q=2,C=1;/wAA/wAA/wAA/wAA/wAA/wAA\x1b\\");
        terminal.advance(b"\r\n\r\n\r\n\r\n");
        let screen = terminal.snapshot();
        let viewport = Rectangle::new(Point::new(5.0, 5.0), Size::new(80.0, 80.0));
        let (bounds, raster) =
            rasterize(&screen.images, viewport, Size::new(10.0, 20.0), 1.0, None)
                .expect("partly visible image");
        assert_eq!(
            bounds,
            Rectangle::new(Point::new(5.0, 5.0), Size::new(10.0, 40.0))
        );
        assert_eq!(
            raster.pixel(5, 10).unwrap().demultiply(),
            ColorU8::from_rgba(255, 0, 0, 255)
        );
    }

    #[test]
    fn kitty_projection_splits_a_cropped_negative_anchor_at_fractional_scale() {
        let mut terminal = Terminal::new(8, 4);
        terminal.set_pixel_size(9, 18, 72, 72);
        let pixels = STANDARD.encode(
            [
                [255, 0, 255],
                [255, 0, 0],
                [255, 0, 0],
                [0, 255, 0],
                [0, 255, 0],
                [0, 0, 255],
                [0, 0, 255],
            ]
            .concat(),
        );
        terminal.advance(
            format!("\x1b_Ga=T,f=24,s=1,v=7,y=1,h=6,c=1,r=3,q=2,C=1;{pixels}\x1b\\").as_bytes(),
        );
        terminal.advance(b"\x1b[4;1H\n");
        let mut frame = selection_test_frame(terminal.snapshot());
        assert_eq!(frame.screen.images.len(), 1);
        assert_eq!(frame.screen.images[0].row, -1);
        assert_eq!(frame.screen.images[0].placement.source_y, 1);
        inline_history::fixture(
            Arc::get_mut(&mut frame).unwrap(),
            1,
            vec![Line::default()],
            false,
        );
        let bands = row_bands(&frame).unwrap();
        assert_eq!(
            bands,
            [
                RowBand {
                    raw: 0,
                    display: 0,
                    rows: 1
                },
                RowBand {
                    raw: 1,
                    display: 2,
                    rows: 2
                }
            ]
        );
        let viewport = Rectangle::new(Point::new(5.0, 5.0), Size::new(72.0, 72.0));
        let scale = 1.25;
        let (bounds, raster) = rasterize(
            &frame.screen.images,
            viewport,
            Size::new(9.0, 18.0),
            scale,
            Some(&bands),
        )
        .unwrap();
        let color_at = |y: f32| {
            raster
                .pixel(
                    ((9.5 - bounds.x) * scale) as u32,
                    ((y - bounds.y) * scale) as u32,
                )
                .unwrap()
                .demultiply()
        };
        assert_eq!(color_at(14.0), ColorU8::from_rgba(0, 255, 0, 255));
        assert_eq!(color_at(32.0), ColorU8::from_rgba(0, 0, 0, 0));
        assert_eq!(color_at(50.0), ColorU8::from_rgba(0, 0, 255, 255));
    }

    #[test]
    fn kitty_projection_keeps_layer_order_and_refreshes_changed_row_bands() {
        use canvas::Program as _;
        use iced::{Font, Pixels};

        let mut terminal = Terminal::new(8, 4);
        terminal.set_pixel_size(9, 18, 72, 72);
        terminal.advance(b"\x1b[2;1H");
        for (z, rgb) in [
            (7, [0, 0, 255]),
            (-1, [0, 255, 0]),
            (-2_000_000_000, [255, 0, 0]),
            (0, [255, 255, 255]),
        ] {
            let pixels = STANDARD.encode([rgb, rgb].concat());
            terminal.advance(
                format!("\x1b_Ga=T,f=24,s=1,v=2,c=1,r=1,z={z},q=2,C=1;{pixels}\x1b\\").as_bytes(),
            );
        }
        let raw = selection_test_frame(terminal.snapshot());
        let mut projected = (*raw).clone();
        inline_history::fixture(&mut projected, 1, vec![Line::default()], false);
        let renderer = Renderer::new(Font::MONOSPACE, Pixels(14.0));
        let grid = Rectangle::new(Point::new(5.0, 5.0), Size::new(72.0, 72.0));
        let canvas = Rectangle::with_size(Size::new(82.0, 82.0));
        for (layer, color) in [
            (ImageLayer::BehindBackground, [255, 0, 0, 255]),
            (ImageLayer::BehindText, [0, 255, 0, 255]),
            (ImageLayer::AboveText, [0, 0, 255, 255]),
        ] {
            let state = State::default();
            for (frame, expected_y) in [(&projected, 41.0), (&*raw, 23.0)] {
                let view = KittyView {
                    frame,
                    layer,
                    grid,
                    cell: Size::new(9.0, 18.0),
                    scale: 1.0,
                };
                assert_eq!(
                    view.draw(
                        &state,
                        &renderer,
                        &Theme::Dark,
                        canvas,
                        mouse::Cursor::Unavailable
                    )
                    .len(),
                    1
                );
                let cached = state.0.borrow();
                let Some((bounds, Handle::Rgba { width, pixels, .. })) =
                    cached.as_ref().unwrap().raster.as_ref()
                else {
                    panic!("expected one decoded Kitty layer");
                };
                assert_eq!(bounds.y, expected_y);
                let center = (9 * *width as usize + 4) * 4;
                assert_eq!(&pixels[center..center + 4], &color);
            }
        }
    }
}
