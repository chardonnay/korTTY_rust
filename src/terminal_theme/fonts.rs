use std::collections::BTreeMap;
use std::sync::OnceLock;

use iced::advanced::graphics::text::{self as graphics_text, Paragraph};
use iced::advanced::text::{self, Paragraph as _};
use iced::{Font, Pixels, Size, font};

pub(crate) const MONOSPACED: &str = "Monospaced";
const DEFAULT_CELL_WIDTH_RATIO: f32 = 0.61;

const PREFERRED: [&str; 12] = [
    MONOSPACED,
    "Courier New",
    "Consolas",
    "Monaco",
    "Menlo",
    "Source Code Pro",
    "JetBrains Mono",
    "Fira Code",
    "SF Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Ubuntu Mono",
];

struct Catalog {
    families: BTreeMap<String, OnceLock<f32>>,
    choices: Vec<String>,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut system = graphics_text::font_system()
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let families: BTreeMap<_, _> = system
            .raw()
            .db()
            .faces()
            .filter_map(|face| face.families.first())
            .map(|(family, _)| family)
            .filter(|family| !family.starts_with('.') && family.as_str() != "Iced-Icons")
            .map(|family| (family.clone(), OnceLock::new()))
            .collect();
        let choices = ordered_families(families.keys().map(String::as_str));
        Catalog { families, choices }
    })
}

fn ordered_families<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut names: Vec<_> = names.filter(|name| *name != MONOSPACED).collect();
    names.push(MONOSPACED);
    names.sort_unstable_by_key(|name| {
        let priority = PREFERRED
            .iter()
            .position(|preferred| name == preferred)
            .unwrap_or(PREFERRED.len());
        (priority, *name)
    });
    names.into_iter().map(str::to_owned).collect()
}

pub(crate) fn installed_families() -> &'static [String] {
    &catalog().choices
}

pub(crate) fn installed_font(name: &str) -> Option<Font> {
    if name == MONOSPACED {
        return Some(Font::MONOSPACE);
    }
    let (family, _) = catalog().families.get_key_value(name)?;
    Some(Font::with_name(family))
}

pub(crate) fn cell_width_ratio(font: Font) -> f32 {
    let font::Family::Name(name) = font.family else {
        return DEFAULT_CELL_WIDTH_RATIO;
    };
    catalog()
        .families
        .get(name)
        .map_or(DEFAULT_CELL_WIDTH_RATIO, |ratio| {
            *ratio.get_or_init(|| measure_width_ratio(font))
        })
}

fn measure_width_ratio(font: Font) -> f32 {
    // Like the Java terminal, fit the fixed-width grid to the selected family's W advance.
    let size = Pixels(100.0);
    let paragraph = Paragraph::with_text(text::Text {
        content: "W",
        bounds: Size::INFINITE,
        size,
        line_height: text::LineHeight::default(),
        font,
        align_x: text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    });
    let ratio = paragraph.min_bounds().width / size.0;
    if ratio.is_finite() && ratio > 0.0 {
        ratio
    } else {
        DEFAULT_CELL_WIDTH_RATIO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_puts_installed_preferred_families_first_and_keeps_proportional_families() {
        let installed = ["Arial", "Menlo", "Monospaced", "Georgia", "Consolas"];
        assert_eq!(
            ordered_families(installed.into_iter()),
            ["Monospaced", "Consolas", "Menlo", "Arial", "Georgia"]
        );
        assert_eq!(installed_font(MONOSPACED), Some(Font::MONOSPACE));
    }
}
