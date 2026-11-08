//! Screens model drawings before the native SVG rasterizer sees them.

use std::fmt::Write as _;
use std::sync::OnceLock;

use regex::Regex;
use resvg::{tiny_skia, usvg};
use roxmltree::{Document, Node, ParsingOptions};
use svgtypes::{Color, NumberListParser, PathParser, TransformListParser};

use super::super::picture::{Size, raster_ascii};

const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const MAX_SVG_BYTES: usize = 64 * 1024;
const MAX_NODES: u32 = 512;
const MAX_SHAPES: usize = 128;
const MAX_DEPTH: usize = 16;
const MAX_COORDINATE: f64 = 10_000.0;

pub(super) fn render(source: &str, size: Size) -> Result<String, &'static str> {
    let (width, height) = size.raster_dimensions();
    let safe_svg = screen(source, width, height)?;
    // Only normalized geometry enters resvg. The screen rejects every element
    // and attribute that could refer to files, URLs, images, fonts or code.
    let tree = usvg::Tree::from_str(&safe_svg, &usvg::Options::default())
        .map_err(|_| "the SVG geometry could not be rendered")?;
    let mut image = tiny_skia::Pixmap::new(width as u32, height as u32)
        .ok_or("the picture could not be allocated")?;
    image.fill(tiny_skia::Color::WHITE);
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut image.as_mut());
    let luma = image.pixels().iter().map(|pixel| {
        let level = 2126 * u32::from(pixel.red())
            + 7152 * u32::from(pixel.green())
            + 722 * u32::from(pixel.blue());
        ((level + 5000) / 10_000) as u8
    });
    if luma.clone().filter(|&level| level < 100).count() * 100 > width * height * 95 {
        return Err("the drawing covers the canvas with a solid background");
    }
    let output = raster_ascii(size, luma);
    if output.trim().is_empty() {
        return Err("the drawing has no visible shapes");
    }
    Ok(output)
}

fn screen(source: &str, width: usize, height: usize) -> Result<String, &'static str> {
    if source.len() > MAX_SVG_BYTES {
        return Err("the SVG exceeds 64 KiB");
    }
    let document = Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| "expected one well-formed SVG without DTDs")?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" || !svg_namespace(root) {
        return Err("expected an SVG document");
    }
    if document.descendants().any(|node| node.is_pi()) {
        return Err("processing instructions are not allowed");
    }
    let view_box = root.attribute("viewBox").unwrap_or("0 0 100 100");
    let view_box = numbers(view_box, 4)
        .filter(|values| values.len() == 4 && values[2] >= 0.01 && values[3] >= 0.01)
        .ok_or("invalid viewBox")?;
    let mut out = format!(
        "<svg xmlns=\"{SVG_NAMESPACE}\" width=\"{width}\" height=\"{height}\" viewBox=\"{} {} {} {}\"",
        view_box[0], view_box[1], view_box[2], view_box[3],
    );
    write_attributes(root, &mut out)?;
    out.push('>');
    let mut shapes = 0;
    write_children(root, 0, &mut shapes, &mut out)?;
    out.push_str("</svg>");
    if shapes == 0 {
        return Err("the drawing contains no shapes");
    }
    Ok(out)
}

fn svg_namespace(node: Node<'_, '_>) -> bool {
    matches!(node.tag_name().namespace(), None | Some(SVG_NAMESPACE))
}

fn write_children(
    parent: Node<'_, '_>,
    depth: usize,
    shapes: &mut usize,
    out: &mut String,
) -> Result<(), &'static str> {
    if depth >= MAX_DEPTH {
        return Err("the drawing has too many nested groups");
    }
    for child in parent.children() {
        if child.is_text() && child.text().is_some_and(|value| !value.trim().is_empty()) {
            return Err("text is not allowed inside a drawing");
        }
        if !child.is_element() {
            continue;
        }
        let tag = child.tag_name().name();
        if !svg_namespace(child)
            || !matches!(
                tag,
                "g" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path"
            )
        {
            return Err("use only groups and SVG geometry shapes");
        }
        write!(out, "<{tag}").expect("write to String");
        write_attributes(child, out)?;
        if tag == "g" {
            out.push('>');
            write_children(child, depth + 1, shapes, out)?;
            out.push_str("</g>");
        } else {
            *shapes += 1;
            if *shapes > MAX_SHAPES {
                return Err("the drawing has too many shapes");
            }
            if child.children().any(|node| {
                node.is_element()
                    || node
                        .text()
                        .is_some_and(|text| node.is_text() && !text.trim().is_empty())
            }) {
                return Err("a shape may not contain other content");
            }
            out.push_str("/>");
        }
    }
    Ok(())
}

fn write_attributes(node: Node<'_, '_>, out: &mut String) -> Result<(), &'static str> {
    let tag = node.tag_name().name();
    for attribute in node.attributes() {
        let name = attribute.name();
        let value = attribute.value().trim();
        if attribute.namespace().is_some() || value.contains(['<', '>', '&', '"', '\'', '\\']) {
            return Err("unsafe drawing attribute");
        }
        if tag == "svg" && matches!(name, "viewBox" | "width" | "height") {
            // The canvas always uses the requested character-grid dimensions.
            continue;
        }
        if !valid_attribute(tag, name, value) {
            return Err("unsupported or invalid drawing attribute");
        }
        write!(out, " {name}=\"{value}\"").expect("write to String");
    }
    Ok(())
}

fn valid_attribute(tag: &str, name: &str, value: &str) -> bool {
    match name {
        "style" => {
            !value.is_empty()
                && value
                    .split(';')
                    .filter(|item| !item.trim().is_empty())
                    .all(|item| {
                        item.split_once(':')
                            .is_some_and(|(name, value)| paint(tag, name.trim(), value.trim()))
                    })
        }
        "transform" if tag != "svg" => {
            bounded_expression(value)
                && TransformListParser::from(value)
                    .enumerate()
                    .all(|(index, token)| index < 16 && token.is_ok())
        }
        "d" if tag == "path" => {
            bounded_expression(value)
                && PathParser::from(value)
                    .enumerate()
                    .all(|(index, segment)| index < 2048 && segment.is_ok())
        }
        "points" if matches!(tag, "polyline" | "polygon") => numbers(value, 512)
            .is_some_and(|values| values.len() >= 4 && values.len().is_multiple_of(2)),
        "x" | "y" | "width" | "height" if tag == "rect" => number(value).is_some(),
        "rx" | "ry" if matches!(tag, "rect" | "ellipse") => number(value).is_some_and(|v| v >= 0.0),
        "cx" | "cy" if matches!(tag, "circle" | "ellipse") => number(value).is_some(),
        "r" if tag == "circle" => number(value).is_some_and(|v| v >= 0.0),
        "x1" | "y1" | "x2" | "y2" if tag == "line" => number(value).is_some(),
        _ => paint(tag, name, value),
    }
}

fn paint(tag: &str, name: &str, value: &str) -> bool {
    match name {
        "fill" | "stroke" => value == "none" || value.parse::<Color>().is_ok(),
        // Group opacity isolates each nested group in a separate raster layer.
        "opacity" if matches!(tag, "svg" | "g") => number(value) == Some(1.0),
        "opacity" | "fill-opacity" | "stroke-opacity" => {
            number(value).is_some_and(|value| (0.0..=1.0).contains(&value))
        }
        "stroke-width" => number(value).is_some_and(|value| value >= 0.0),
        "stroke-linecap" => matches!(value, "butt" | "round" | "square"),
        "stroke-linejoin" => matches!(value, "miter" | "round" | "bevel"),
        "fill-rule" => matches!(value, "nonzero" | "evenodd"),
        _ => false,
    }
}

fn within_bound(value: f64) -> bool {
    value.abs() <= MAX_COORDINATE
}

fn number(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|&value| within_bound(value))
}

fn numbers(value: &str, limit: usize) -> Option<Vec<f64>> {
    NumberListParser::from(value)
        .enumerate()
        .map(|(index, number)| {
            number
                .ok()
                .filter(|&number| index < limit && within_bound(number))
        })
        .collect()
}

fn bounded_expression(value: &str) -> bool {
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    let number_pattern = NUMBER.get_or_init(|| {
        Regex::new(r"[+-]?(?:[0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE][+-]?[0-9]+)?")
            .expect("valid SVG number expression")
    });
    !value.is_empty()
        && number_pattern
            .find_iter(value)
            .all(|value| number(value.as_str()).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_screen_rejects_active_content_and_unbounded_geometry() {
        for source in [
            "<?xml-stylesheet href=\"file:///tmp/theme.css\"?><svg><rect width=\"1\" height=\"1\"/></svg>",
            "<!DOCTYPE svg [<!ENTITY x SYSTEM 'file:///tmp/secret'>]><svg>&x;</svg>",
            "<svg onload=\"run()\"><rect width=\"1\" height=\"1\"/></svg>",
            "<svg><use href=\"https://example.test/a.svg\"/></svg>",
            "<svg><rect width=\"1\" height=\"1\" style=\"fill:url(file:///tmp/a.svg)\"/></svg>",
            "<svg><g opacity=\"0.99\"><rect width=\"5\" height=\"5\"/></g></svg>",
            "<svg style=\"opacity:0.5\"><rect width=\"5\" height=\"5\"/></svg>",
            "<svg><path d=\"M 0 0 L 1e9999 50\"/></svg>",
            "<svg viewBox=\"0 0 1e300 100\"><rect width=\"1\" height=\"1\"/></svg>",
        ] {
            assert!(screen(source, 320, 320).is_err(), "{source}");
        }
        let many = format!("<svg>{}</svg>", "<circle r=\"2\"/>".repeat(MAX_SHAPES + 1));
        assert!(screen(&many, 320, 320).is_err());
    }

    #[test]
    fn svg_screen_keeps_plain_paths_transforms_and_colors() {
        let source = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\"><g transform=\"translate(8,10)\"><path d=\"M 0 10 Q 20 60 42 35 L 48 51 Z\" style=\"fill:#555;stroke:black;stroke-width:2\"/></g></svg>";
        let art = render(source, Size::Small).unwrap();
        assert!(!art.is_empty());
        assert!(art.lines().count() <= 20);
    }
}
