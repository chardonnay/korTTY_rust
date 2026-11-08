use std::fmt;
use std::io::{Cursor, Read as _};
use std::path::Path;

use image::{ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};

mod glyphs;
use glyphs::{CELL_HEIGHT, CELL_WIDTH, Ink};

const MAX_FILE: u64 = 32 * 1024 * 1024;
const MAX_EDGE: u32 = 16_384;
const MAX_PIXELS: u64 = 24_000_000;
const WORKING_EDGE: usize = 1600;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl Size {
    pub(super) const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Large, Self::ExtraLarge];

    pub(super) fn dimensions(self) -> (usize, usize) {
        match self {
            Self::Small => (40, 20),
            Self::Medium => (60, 30),
            Self::Large => (80, 40),
            Self::ExtraLarge => (100, 50),
        }
    }

    pub(super) fn raster_dimensions(self) -> (usize, usize) {
        let (columns, rows) = self.dimensions();
        (columns * CELL_WIDTH, rows * CELL_HEIGHT)
    }
}

impl fmt::Display for Size {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
            Self::ExtraLarge => "Extra large",
        };
        let (columns, rows) = self.dimensions();
        write!(formatter, "{label} ({columns} × {rows})")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Adjustments {
    pub(super) brightness: i16,
    pub(super) contrast: i16,
    pub(super) invert: bool,
}

#[derive(Debug)]
pub struct Picture {
    pub(super) source: (u32, u32),
    working: Luminance,
}

impl Picture {
    pub(super) fn load(path: &Path) -> Result<Self, String> {
        let file = crate::profile::open_preference(path).map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_FILE {
            return Err("Choose a regular image file up to 32 MiB.".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("The image file exceeds 32 MiB.".into());
        }
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if let Some(payload) = bytes.strip_prefix(&[0, 0]) {
            return Self::decode_wbmp(payload);
        }
        let format = image::guess_format(bytes).map_err(|error| error.to_string())?;
        if !matches!(
            format,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Bmp
        ) {
            return Err("Choose a PNG, JPEG, GIF, BMP or WBMP image.".into());
        }
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_EDGE);
        limits.max_image_height = Some(MAX_EDGE);
        limits.max_alloc = Some(MAX_PIXELS * 8);
        let reader = || {
            let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
            reader.limits(limits.clone());
            reader
        };
        let source = reader()
            .into_dimensions()
            .map_err(|error| error.to_string())?;
        let (width, height) = source;
        check_dimensions(width, height)?;
        let rgba = reader()
            .decode()
            .map_err(|error| error.to_string())?
            .into_rgba8();
        let gray = Luminance {
            width: width as usize,
            height: height as usize,
            values: rgba
                .pixels()
                .map(|pixel| {
                    let [red, green, blue, alpha] = pixel.0.map(|level| f64::from(level) / 255.0);
                    let lightness = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
                    (lightness * alpha + (1.0 - alpha)) as f32
                })
                .collect(),
        };
        drop(rgba);
        Ok(Self::with_luminance(source, gray))
    }

    fn decode_wbmp(mut payload: &[u8]) -> Result<Self, String> {
        let width = wbmp_dimension(&mut payload).ok_or("Invalid WBMP width.")?;
        let height = wbmp_dimension(&mut payload).ok_or("Invalid WBMP height.")?;
        check_dimensions(width, height)?;
        let stride = (width as usize).div_ceil(8);
        if payload.len() != stride * height as usize {
            return Err("WBMP pixel data does not match the image dimensions.".into());
        }
        let values = payload
            .chunks_exact(stride)
            .flat_map(|row| {
                (0..width as usize).map(move |x| f32::from((row[x / 8] >> (7 - x % 8)) & 1))
            })
            .collect();
        Ok(Self::with_luminance(
            (width, height),
            Luminance {
                width: width as usize,
                height: height as usize,
                values,
            },
        ))
    }

    fn with_luminance(source: (u32, u32), gray: Luminance) -> Self {
        let longest = gray.width.max(gray.height);
        let working = if longest > WORKING_EDGE {
            let shrink = |edge: usize| ((edge * WORKING_EDGE + longest / 2) / longest).max(1);
            gray.resample(shrink(gray.width), shrink(gray.height))
        } else {
            gray
        };
        Self { source, working }
    }

    pub(super) fn grid(&self, size: Size) -> Grid {
        let (columns, rows) = size.dimensions();
        let width = self.working.width as f64;
        let height = self.working.height as f64;
        let scale =
            ((columns * CELL_WIDTH) as f64 / width).min((rows * CELL_HEIGHT) as f64 / height);
        let target_width = ((width * scale).round() as usize).clamp(1, columns * CELL_WIDTH);
        let target_height = ((height * scale).round() as usize).clamp(1, rows * CELL_HEIGHT);
        let mut fitted = self.working.resample(target_width, target_height);
        fitted.stretch();
        Grid { size, fitted }
    }
}

fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if !(1..=MAX_EDGE).contains(&width)
        || !(1..=MAX_EDGE).contains(&height)
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err("The image dimensions exceed the decoding limit.".into());
    }
    Ok(())
}

fn wbmp_dimension(payload: &mut &[u8]) -> Option<u32> {
    // A supported edge fits in at most three 7-bit groups.
    let mut value = 0_u32;
    for _ in 0..3 {
        let (&byte, rest) = payload.split_first()?;
        *payload = rest;
        value = (value << 7) | u32::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

#[derive(Debug)]
struct Luminance {
    width: usize,
    height: usize,
    values: Vec<f32>,
}

impl Luminance {
    fn get(&self, x: i32, y: i32) -> f64 {
        let x = (x.max(0) as usize).min(self.width - 1);
        let y = (y.max(0) as usize).min(self.height - 1);
        f64::from(self.values[y * self.width + x])
    }

    fn resample(&self, width: usize, height: usize) -> Self {
        let mut values = Vec::with_capacity(width * height);
        let scale_x = self.width as f64 / width as f64;
        let scale_y = self.height as f64 / height as f64;
        for y in 0..height {
            let top = (y as f64 * scale_y).floor() as i32;
            let bottom = (((y + 1) as f64 * scale_y).ceil() as i32).max(top + 1);
            for x in 0..width {
                let left = (x as f64 * scale_x).floor() as i32;
                let right = (((x + 1) as f64 * scale_x).ceil() as i32).max(left + 1);
                let value = if right - left == 1 && bottom - top == 1 {
                    self.bilinear(
                        (x as f64 + 0.5) * scale_x - 0.5,
                        (y as f64 + 0.5) * scale_y - 0.5,
                    )
                } else {
                    let mut sum = 0.0;
                    for sy in top..bottom {
                        for sx in left..right {
                            sum += self.get(sx, sy);
                        }
                    }
                    sum / f64::from((right - left) * (bottom - top))
                };
                values.push(value as f32);
            }
        }
        Self {
            width,
            height,
            values,
        }
    }

    fn bilinear(&self, x: f64, y: f64) -> f64 {
        let (left, top) = (x.floor() as i32, y.floor() as i32);
        let (horizontal, vertical) = (x - f64::from(left), y - f64::from(top));
        let upper = self.get(left, top) * (1.0 - horizontal) + self.get(left + 1, top) * horizontal;
        let lower =
            self.get(left, top + 1) * (1.0 - horizontal) + self.get(left + 1, top + 1) * horizontal;
        upper * (1.0 - vertical) + lower * vertical
    }

    fn stretch(&mut self) {
        let mut histogram = [0_usize; 256];
        for &tone in &self.values {
            histogram[(tone.clamp(0.0, 1.0) * 255.0).round() as usize] += 1;
        }
        let percentile = |fraction: f64| {
            let target = (self.values.len() as f64 * fraction).round().max(1.0) as usize;
            let mut seen = 0;
            for (tone, count) in histogram.into_iter().enumerate() {
                seen += count;
                if seen >= target {
                    return tone as f64 / 255.0;
                }
            }
            1.0
        };
        let low = percentile(0.01);
        let span = percentile(0.99) - low;
        if span >= 0.02 {
            for tone in &mut self.values {
                *tone = ((f64::from(*tone) - low) / span).clamp(0.0, 1.0) as f32;
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct Grid {
    pub(super) size: Size,
    fitted: Luminance,
}

impl Grid {
    pub(super) fn render(&self, adjust: Adjustments) -> String {
        let contrast = f64::from(adjust.contrast.clamp(-100, 100)) / 100.0;
        let factor = if contrast >= 0.0 {
            1.0 + contrast * 3.0
        } else {
            1.0 + contrast
        };
        let brightness = f64::from(adjust.brightness.clamp(-100, 100)) / 200.0;
        let (width, height) = self.size.raster_dimensions();
        let mut canvas = vec![255; width * height];
        let x0 = (width - self.fitted.width) / 2;
        let y0 = (height - self.fitted.height) / 2;
        for (row, values) in self
            .fitted
            .values
            .chunks_exact(self.fitted.width)
            .enumerate()
        {
            let start = (y0 + row) * width + x0;
            for (cell, &tone) in canvas[start..start + values.len()].iter_mut().zip(values) {
                let light = ((f64::from(tone) - 0.5) * factor + 0.5 + brightness).clamp(0.0, 1.0);
                let light = if adjust.invert { 1.0 - light } else { light } as f32;
                *cell = (f64::from(light) * 255.0).round() as u8;
            }
        }
        raster_ascii(self.size, canvas.into_iter())
    }
}

pub(super) fn raster_ascii(size: Size, gray: impl Iterator<Item = u8>) -> String {
    let (columns, rows) = size.dimensions();
    let (width, height) = size.raster_dimensions();
    let pixels: Vec<_> = gray.map(glyphs::from_gray).collect();
    let ink = Ink::new(width, height, &pixels);
    trim(
        (0..rows)
            .map(|row| (0..columns).map(|col| ink.glyph(col, row)).collect())
            .collect(),
    )
}

pub(super) fn trim(lines: Vec<String>) -> String {
    let Some(start) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(start);
    let left = lines[start..=end]
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or_default();
    lines[start..=end]
        .iter()
        .map(|line| line.get(left..).unwrap_or_default().trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_art_png_keeps_transparency_white_and_fits_the_grid() {
        let rgba = image::RgbaImage::from_fn(60, 180, |_, y| {
            image::Rgba(if y < 40 { [0, 0, 0, 0] } else { [0, 0, 0, 255] })
        });
        let mut bytes = Cursor::new(Vec::new());
        rgba.write_to(&mut bytes, ImageFormat::Png).unwrap();
        let picture = Picture::decode(bytes.get_ref()).unwrap();
        assert_eq!(picture.source, (60, 180));
        let grid = picture.grid(Size::Small);
        assert_eq!(grid.fitted.width, 107);
        let art = grid.render(Adjustments::default());
        assert!(art.contains('@'));
        assert!(art.lines().count() < 20);
        let inverted = grid.render(Adjustments {
            invert: true,
            ..Adjustments::default()
        });
        assert_ne!(art, inverted);
        assert!(Picture::decode(b"not an image").is_err());
    }

    #[test]
    fn ascii_art_trims_the_shared_blank_frame() {
        assert_eq!(
            trim(vec![
                "     ".into(),
                "  @  ".into(),
                "     ".into(),
                "  @@@".into()
            ]),
            "@\n\n@@@"
        );
    }

    #[test]
    fn ascii_art_reads_wbmp_type_zero_with_row_padding_and_bounds() {
        let bytes = [0, 0, 9, 2, 0xff, 0x00, 0x00, 0xff];
        let picture = Picture::decode(&bytes).unwrap();
        assert_eq!(picture.source, (9, 2));
        assert_eq!(picture.working.get(0, 0), 1.0);
        assert_eq!(picture.working.get(8, 0), 0.0);
        assert_eq!(picture.working.get(0, 1), 0.0);
        assert_eq!(picture.working.get(8, 1), 1.0);
        assert!(Picture::decode(&bytes[..7]).is_err());
        assert!(Picture::decode(&[0, 0, 0xff, 0xff, 0xff, 0x01, 0x01]).is_err());
        assert_eq!(wbmp_dimension(&mut &[0x81, 0x00][..]), Some(128));
    }

    #[test]
    fn ascii_art_preserves_both_slopes_of_a_filled_roof() {
        let (width, height) = (80, 80);
        let values = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x: usize| {
                    if (8..68).contains(&y) && x.abs_diff(width / 2) <= y - 8 {
                        0.0
                    } else {
                        1.0
                    }
                })
            })
            .collect();
        let picture = Picture::with_luminance(
            (80, 80),
            Luminance {
                width,
                height,
                values,
            },
        );
        let roof = picture.grid(Size::Medium).render(Adjustments::default());
        assert!(roof.matches('/').count() >= 3, "{roof}");
        assert!(roof.matches('\\').count() >= 3, "{roof}");
        assert!(roof.lines().any(|line| line.contains("@@@@")), "{roof}");
    }
}
