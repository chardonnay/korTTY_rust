use std::sync::LazyLock;

use latex_rust::{Dim, MathFont, MathStyle, ParseOptions, SvgOptions};
use roxmltree::{Document, ParsingOptions};

use super::{Preview, rasterize};

const MAX_SOURCE_BYTES: usize = 2_048;
const MAX_TOKENS: usize = 384;
const MAX_DEPTH: usize = 16;
const MAX_SVG_BYTES: usize = 768 * 1024;
const UNSUPPORTED: &str = "Unsupported LaTeX math. The source is available below.";
const TOO_LARGE: &str = "The LaTeX formula exceeds the preview limits.";

pub(super) fn is_fenced(language: &str, source: &str) -> bool {
    ["latex", "tex", "math", "katex"]
        .iter()
        .any(|tag| language.eq_ignore_ascii_case(tag))
        && !source.trim().is_empty()
        && !source.contains("\\documentclass")
        && !source.contains("\\begin{document}")
}

fn unframe(source: &str) -> &str {
    let source = source.trim();
    [("$$", "$$"), ("\\[", "\\]")]
        .into_iter()
        .find_map(|(start, end)| source.strip_prefix(start)?.strip_suffix(end))
        .unwrap_or(source)
        .trim()
}

pub(super) fn render(source: &str) -> Result<Preview, &'static str> {
    static FONT: LazyLock<Result<MathFont, &'static str>> = LazyLock::new(|| {
        MathFont::stix_two_math().map_err(|_| "The embedded math font could not be loaded.")
    });

    let source = unframe(source);
    if source.len() > MAX_SOURCE_BYTES {
        return Err(TOO_LARGE);
    }
    if source.is_empty() {
        return Err(UNSUPPORTED);
    }
    if latex_rust::tokenize(source).map_err(|_| UNSUPPORTED)?.len() > MAX_TOKENS {
        return Err(TOO_LARGE);
    }

    let parse = ParseOptions::new()
        .with_max_depth(MAX_DEPTH)
        .with_unicode_symbols(true)
        .with_prime_symbol(true);
    let (math, _) = latex_rust::parse_with_options(source, &parse).map_err(|_| UNSUPPORTED)?;
    let font = FONT.as_ref().map_err(|error| *error)?;
    let mut options = SvgOptions::new();
    options.font_size_pt = Dim::from_i64(16);
    let layout =
        latex_rust::layout_with_em_size_pt(&math, font, MathStyle::Display, &options.font_size_pt)
            .map_err(|_| UNSUPPORTED)?;
    let svg = latex_rust::render_svg(&layout, font, &options).map_err(|_| UNSUPPORTED)?;
    if svg.len() > MAX_SVG_BYTES {
        return Err(TOO_LARGE);
    }
    let document = Document::parse_with_options(
        &svg,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 2_048,
            entity_resolver: None,
        },
    )
    .map_err(|_| UNSUPPORTED)?;
    if document.descendants().any(|node| {
        node.is_element()
            && (!matches!(
                node.tag_name().name(),
                "svg" | "g" | "path" | "rect" | "line"
            ) || node.attributes().any(|attr| attr.name() == "href"))
    }) {
        return Err(UNSUPPORTED);
    }
    rasterize(&document)
}
