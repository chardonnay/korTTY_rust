use std::collections::HashMap;
use std::io;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};

use crate::sftp_window::LocalSource;

use super::super::markdown::load_screenshot;
use super::layout::{BODY_WIDTH, BODY_X, Layout};

const MAX_SCREENSHOT_INPUT: u64 = 64 * 1024 * 1024;
const MAX_PIXEL_WIDTH: u32 = 1200;
const MAX_IMAGE_HEIGHT: f32 = 220.0;

#[derive(Clone, Copy)]
struct Image {
    index: usize,
    width: u32,
    height: u32,
}

pub(super) struct Screenshots<'a> {
    source: &'a LocalSource,
    seen: HashMap<&'a str, Option<Image>>,
    remaining: u64,
    pub skipped: usize,
}

impl<'a> Screenshots<'a> {
    pub(super) fn new(source: &'a LocalSource) -> Self {
        Self {
            source,
            seen: HashMap::new(),
            remaining: MAX_SCREENSHOT_INPUT,
            skipped: 0,
        }
    }

    pub(super) fn draw(&mut self, layout: &mut Layout<'_>, reference: &'a str) -> io::Result<()> {
        let image = match self.seen.get(reference) {
            Some(image) => *image,
            None => {
                layout.check_cancelled()?;
                let image = self.load(layout, reference)?;
                self.skipped += usize::from(image.is_none());
                self.seen.insert(reference, image);
                image
            }
        };
        if let Some(image) = image {
            let width = image.width as f32;
            let height = image.height as f32;
            let scale = (BODY_WIDTH / 2.0 / width)
                .min(MAX_IMAGE_HEIGHT / height)
                .min(1.0);
            let width = width * scale;
            let height = height * scale;
            layout.ensure_space(height + 8.0)?;
            layout.document.draw_image(
                layout.page,
                image.index,
                [BODY_X, layout.y - height, width, height],
            )?;
            layout.y -= height + 8.0;
        }
        Ok(())
    }

    fn load(&mut self, layout: &mut Layout<'_>, reference: &str) -> io::Result<Option<Image>> {
        let Ok(bytes) = load_screenshot(self.source, reference, self.remaining) else {
            return Ok(None);
        };
        self.remaining -= bytes.len() as u64;
        let Ok(image) = image::load_from_memory_with_format(&bytes, ImageFormat::Png) else {
            return Ok(None);
        };
        drop(bytes);
        layout.check_cancelled()?;
        let image = downscale(image);
        let width = image.width();
        let height = image.height();
        let index = if image.color().has_alpha() {
            let rgba = image.into_rgba8();
            let mut rgb = Vec::with_capacity(rgba.as_raw().len() / 4 * 3);
            let mut alpha = Vec::with_capacity(rgba.as_raw().len() / 4);
            for pixel in rgba.as_raw().as_chunks::<4>().0 {
                rgb.extend_from_slice(&pixel[..3]);
                alpha.push(pixel[3]);
            }
            let alpha = alpha
                .iter()
                .any(|value| *value != 255)
                .then_some(alpha.as_slice());
            layout.document.embed_image(width, height, &rgb, alpha)?
        } else {
            layout
                .document
                .embed_image(width, height, image.into_rgb8().as_raw(), None)?
        };
        Ok(Some(Image {
            index,
            width,
            height,
        }))
    }
}

fn downscale(image: DynamicImage) -> DynamicImage {
    if image.width() <= MAX_PIXEL_WIDTH {
        return image;
    }
    let height = ((u64::from(image.height()) * u64::from(MAX_PIXEL_WIDTH)
        + u64::from(image.width()) / 2)
        / u64::from(image.width())) as u32;
    image.resize_exact(MAX_PIXEL_WIDTH, height.max(1), FilterType::Triangle)
}
