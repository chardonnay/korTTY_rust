use std::cell::RefCell;

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry};
use iced::widget::image::Handle;
use iced::{Point, Rectangle, Renderer, Size, Theme};
use kortty_terminal::{KittyPlacement, ViewportImage};
use tiny_skia::{ColorU8, FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};

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
    pub images: &'a [ViewportImage],
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
    raster: Option<(Rectangle, Handle)>,
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
        let images = self
            .images
            .iter()
            .filter(|image| self.layer.includes(image.placement.z_index));
        if !cached.as_ref().is_some_and(|cache| {
            cache.viewport == viewport
                && cache.scale == self.scale
                && cache.cell == self.cell
                && cache.images.iter().eq(images.clone())
        }) {
            let images = images.cloned().collect::<Vec<_>>();
            let raster =
                rasterize(&images, viewport, self.cell, self.scale).map(|(bounds, pixels)| {
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
                });
            *cached = Some(RasterLayer {
                viewport,
                cell: self.cell,
                scale: self.scale,
                images,
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

fn rasterize(
    images: &[ViewportImage],
    viewport: Rectangle,
    cell: Size,
    scale: f32,
) -> Option<(Rectangle, Pixmap)> {
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
            Some((placement, bounds, bounds.intersection(&viewport)?))
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
        let transform = Transform::from_scale(
            bounds.width * scale / source.width() as f32,
            bounds.height * scale / source.height() as f32,
        )
        .post_translate(bounds.x * scale - left, bounds.y * scale - top);
        pixmap.draw_pixmap(0, 0, source.as_ref(), &paint, transform, None);
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
    use super::*;
    use kortty_terminal::Terminal;

    #[test]
    fn graphics_raster_scales_the_selected_crop_at_its_cell_position() {
        let mut terminal = Terminal::new(8, 4);
        terminal.advance(b"\x1b[2;3H\x1b_Ga=T,f=24,s=2,v=1,x=1,w=1,c=2,r=1,q=2,C=1;/wAAAP8A\x1b\\");
        let screen = terminal.snapshot();
        let viewport = Rectangle::new(Point::new(5.0, 5.0), Size::new(80.0, 80.0));
        let (bounds, raster) =
            rasterize(&screen.images, viewport, Size::new(10.0, 20.0), 2.0).expect("visible image");
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
        let (bounds, raster) = rasterize(&screen.images, viewport, Size::new(10.0, 20.0), 1.0)
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
}
