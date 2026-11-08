use std::{
    error::Error,
    fmt,
    sync::{Arc, OnceLock},
};

use resvg::{
    tiny_skia,
    usvg::{self, fontdb},
};

use crate::{Scene, Theme};

const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DIMENSION: u32 = 16_384;
pub(crate) const PORTABLE_MONOSPACE_FAMILIES: [&str; 8] = [
    "Menlo",
    "Consolas",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Courier New",
    "Adwaita Mono",
    "Noto Sans Mono",
    "Monaco",
];
type RenderError = Box<dyn Error + Send + Sync>;

/// A failure to rasterize a diagram into PNG.
#[derive(Debug)]
pub enum PngError {
    /// The requested scale was outside 1 through 3.
    InvalidScale,
    /// The raster would exceed the supported width, height or pixel budget.
    ImageTooLarge,
    /// The scene uses text, but the system has no discoverable fonts.
    MissingFonts,
    /// The generated vector scene could not be interpreted.
    Render(RenderError),
    /// The raster could not be encoded as PNG.
    Encode(RenderError),
}

impl fmt::Display for PngError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScale => formatter.write_str("PNG scale must be between 1 and 3"),
            Self::ImageTooLarge => formatter.write_str("diagram exceeds the PNG raster size limit"),
            Self::MissingFonts => {
                formatter.write_str("no system fonts are available for diagram labels")
            }
            Self::Render(error) => write!(formatter, "could not rasterize diagram: {error}"),
            Self::Encode(error) => write!(formatter, "could not encode diagram PNG: {error}"),
        }
    }
}

impl Error for PngError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Render(error) | Self::Encode(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl Scene {
    /// Renders a PNG at an integer scale of 1 through 3 with the same geometry
    /// and colors as [`Scene::to_svg`]. Text uses available system fonts.
    ///
    /// Font discovery on first use and rasterization can run on a worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid scale, a raster beyond the allocation
    /// budget, missing fonts for labels, or a render or encoding failure.
    pub fn to_png(&self, theme: Theme, scale: u8) -> Result<Vec<u8>, PngError> {
        if !(1..=3).contains(&scale) {
            return Err(PngError::InvalidScale);
        }
        let dimensions = |dimension: i32| {
            u32::try_from(dimension)
                .ok()
                .and_then(|value| value.checked_mul(u32::from(scale)))
                .filter(|&value| (1..=MAX_DIMENSION).contains(&value))
                .ok_or(PngError::ImageTooLarge)
        };
        let width = dimensions(self.bounds.width)?;
        let height = dimensions(self.bounds.height)?;
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err(PngError::ImageTooLarge);
        }

        let has_labels = self.nodes.iter().any(|node| !node.lines.is_empty())
            || self.edges.iter().any(|edge| !edge.label_lines.is_empty());
        let mut options = usvg::Options::default();
        if has_labels {
            options.fontdb = Arc::clone(&system_fonts().database);
            if options.fontdb.faces().next().is_none() {
                return Err(PngError::MissingFonts);
            }
        }
        // Only the crate's generated SVG enters the rasterizer. Diagram labels
        // have already been escaped, so no input can introduce SVG resources.
        let tree = usvg::Tree::from_str(&self.to_svg(theme), &options)
            .map_err(|error| PngError::Render(Box::new(error)))?;
        let mut image = tiny_skia::Pixmap::new(width, height).ok_or(PngError::ImageTooLarge)?;
        let scale = f32::from(scale);
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut image.as_mut(),
        );
        image
            .encode_png()
            .map_err(|error| PngError::Encode(Box::new(error)))
    }
}

struct SystemFonts {
    database: Arc<fontdb::Database>,
    label_family: Option<String>,
}

pub(crate) fn system_label_family() -> Option<&'static str> {
    system_fonts().label_family.as_deref()
}

fn is_svg_font_family(name: &str) -> bool {
    // Both SVG rasterizers read quoted names literally, without unescaping
    // CSS backslash escapes.
    !name.trim().is_empty()
        && !name
            .contains(|ch: char| ch.is_control() || matches!(ch, '\\' | '\u{fffe}' | '\u{ffff}'))
        && !(name.contains('\'') && name.contains('"'))
}

fn system_fonts() -> &'static SystemFonts {
    static SYSTEM_FONTS: OnceLock<SystemFonts> = OnceLock::new();
    SYSTEM_FONTS.get_or_init(|| {
        let mut fonts = fontdb::Database::new();
        fonts.load_system_fonts();
        let preferred = PORTABLE_MONOSPACE_FAMILIES.map(fontdb::Family::Name);
        let label_family = fonts
            .query(&fontdb::Query {
                families: &preferred,
                ..Default::default()
            })
            .and_then(|id| fonts.face(id))
            .into_iter()
            .chain(fonts.faces().filter(|face| face.monospaced))
            .chain(fonts.faces())
            .flat_map(|face| face.families.iter().map(|(name, _)| name))
            .find(|name| is_svg_font_family(name))
            .cloned();
        if let Some(family) = &label_family {
            fonts.set_monospace_family(family.clone());
        }
        SystemFonts {
            database: Arc::new(fonts),
            label_family,
        }
    })
}
