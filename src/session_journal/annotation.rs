//! Editable marks in the Java journal's screenshot pixel coordinates.

use std::fmt;
use std::io;

use super::metadata::{java_whitespace, xml_character};

mod codec;
mod raster;

pub(crate) use codec::{read, write_xml, xml};
pub(crate) use raster::render;

pub(crate) const DEFAULT_COLOR: [u8; 3] = [0xe1, 0x1d, 0x48];
pub(crate) const DEFAULT_STROKE_WIDTH: f32 = 6.0;
pub(crate) const DEFAULT_FONT_SIZE: f32 = 28.0;
pub(crate) const MAX_ANNOTATIONS: usize = 256;
pub(crate) const MAX_POINTS_PER_MARK: usize = 4096;
pub(crate) const MAX_POINTS_TOTAL: usize = 16_384;
pub(crate) const MAX_TEXT_BYTES: usize = 1024;
const MAX_TEXT_TOTAL: usize = 64 * 1024;
const MAX_DIMENSION: f32 = 512.0;
const MAX_COORDINATE: f32 = super::MAX_PNG_PIXELS as f32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Kind {
    #[default]
    Pen,
    Box,
    Pixelate,
    Text,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Annotation {
    pub(crate) kind: Kind,
    pub(crate) color: [u8; 3],
    pub(crate) stroke_width: f32,
    pub(crate) font_size: f32,
    pub(crate) text: String,
    /// Pen positions, a rectangle's origin and signed size, or a text baseline anchor.
    pub(crate) points: Vec<[f32; 2]>,
}

impl Default for Annotation {
    fn default() -> Self {
        Self {
            kind: Kind::default(),
            color: DEFAULT_COLOR,
            stroke_width: DEFAULT_STROKE_WIDTH,
            font_size: DEFAULT_FONT_SIZE,
            text: String::new(),
            points: Vec::new(),
        }
    }
}

impl fmt::Debug for Annotation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalScreenshotAnnotation")
            .field("kind", &self.kind)
            .field("point_count", &self.points.len())
            .finish_non_exhaustive()
    }
}

impl Annotation {
    pub(crate) fn is_drawable(&self) -> bool {
        match self.kind {
            Kind::Pen => self.points.len() >= 2,
            Kind::Box | Kind::Pixelate => {
                matches!(self.points.as_slice(), [_, [width, height]]
                    if width.abs() >= 1.0 && height.abs() >= 1.0)
            }
            Kind::Text => self.points.len() == 1 && !self.text.chars().all(java_whitespace),
        }
    }
}

pub(crate) fn validate(annotations: &[Annotation]) -> io::Result<()> {
    if annotations.len() > MAX_ANNOTATIONS {
        return Err(size_limit());
    }
    let mut points = 0;
    let mut text = 0;
    for mark in annotations {
        points += mark.points.len().min(MAX_POINTS_TOTAL + 1);
        text += mark.text.len().min(MAX_TEXT_TOTAL + 1);
        if mark.points.len() > MAX_POINTS_PER_MARK
            || mark.text.len() > MAX_TEXT_BYTES
            || points > MAX_POINTS_TOTAL
            || text > MAX_TEXT_TOTAL
        {
            return Err(size_limit());
        }
        if !mark.is_drawable()
            || !bounded_size(mark.stroke_width)
            || !bounded_size(mark.font_size)
            || mark
                .points
                .iter()
                .flatten()
                .any(|point| point.abs() > MAX_COORDINATE || !point.is_finite())
            || !mark.text.chars().all(xml_character)
        {
            return Err(invalid(
                "Invalid journal screenshot annotation geometry or text.",
            ));
        }
    }
    Ok(())
}

fn bounded_size(value: f32) -> bool {
    value.is_finite() && value > 0.0 && value <= MAX_DIMENSION
}

fn size_limit() -> io::Error {
    invalid("Journal screenshot annotations exceed their size limit.")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
