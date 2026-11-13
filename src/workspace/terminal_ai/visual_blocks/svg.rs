//! Screens untrusted static SVG before the in-memory rasterizer sees it.

use std::sync::LazyLock;

use regex::Regex;
use roxmltree::{Document, Node, ParsingOptions};
use svgtypes::{Color, NumberListParser, PathParser, Transform, TransformListParser};

use super::{Preview, rasterize};

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const MAX_NODES: u32 = 384;
const MAX_DEPTH: usize = 12;
const MAX_COORDINATE: f64 = 16_384.0;
const MAX_PATH_SEGMENTS: usize = 512;

pub(super) fn has_svg_root(source: &str) -> bool {
    static ROOT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)^\s*(?:(?:<\?xml\s.*?\?>|<!--.*?-->)\s*)*<svg(?:\s|>|/)")
            .expect("static SVG root pattern")
    });
    source.starts_with('<') && ROOT.is_match(source)
}

pub(super) fn render(source: &str) -> Result<Preview, &'static str> {
    const UNSUPPORTED: &str = "Unsupported or unsafe SVG content.";
    let source = source.trim_start_matches('\u{feff}').trim();
    if source.len() > crate::workspace::terminal_ai::MAX_ANSWER_BYTES {
        return Err("The SVG exceeds the chat response limit.");
    }
    let document = Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| "Expected one well-formed SVG document.")?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NS) {
        return Err("Expected an SVG document with its standard namespace.");
    }
    if document.descendants().any(|node| {
        node.is_pi()
            || (node.is_element()
                && (node.ancestors().take(MAX_DEPTH + 1).count() > MAX_DEPTH
                    || !safe_element(node, root)))
    }) {
        return Err(UNSUPPORTED);
    }
    if !root.descendants().any(|node| {
        node.is_element()
            && matches!(
                node.tag_name().name(),
                "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "text"
            )
    }) {
        return Err("The SVG contains no supported shapes or text.");
    }
    rasterize(&document)
}

fn safe_element(node: Node<'_, '_>, root: Node<'_, '_>) -> bool {
    let name = node.tag_name().name();
    if node.tag_name().namespace() != Some(SVG_NS)
        || !matches!(
            name,
            "svg"
                | "g"
                | "defs"
                | "path"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "linearGradient"
                | "radialGradient"
                | "stop"
                | "title"
                | "desc"
                | "text"
                | "tspan"
        )
        || (name == "svg" && node != root)
    {
        return false;
    }
    node.attributes().all(|attribute| {
        attribute.namespace().is_none()
            && safe_attribute(name, attribute.name(), attribute.value().trim())
    }) && bounded_transform_chain(node)
}

fn safe_attribute(tag: &str, name: &str, value: &str) -> bool {
    match name {
        "id" | "class" => identifier(value),
        "version" => matches!(value, "1.0" | "1.1" | "2.0"),
        "role" => matches!(value, "img" | "presentation"),
        "viewBox" if tag == "svg" => view_box(value),
        "preserveAspectRatio" => {
            value.len() <= 40 && value.bytes().all(|b| b.is_ascii_alphabetic() || b == b' ')
        }
        "d" if tag == "path" => {
            bounded_expression(value)
                && PathParser::from(value)
                    .enumerate()
                    .all(|(index, segment)| index < MAX_PATH_SEGMENTS && segment.is_ok())
        }
        "points" if matches!(tag, "polygon" | "polyline") => numbers(value, 256)
            .is_some_and(|values| values.len() >= 4 && values.len().is_multiple_of(2)),
        "transform" | "gradientTransform" => transform(value).is_some(),
        "gradientUnits" => matches!(value, "objectBoundingBox" | "userSpaceOnUse"),
        "spreadMethod" => value == "pad",
        "style" => {
            value.len() <= 512
                && value
                    .split(';')
                    .filter(|item| !item.trim().is_empty())
                    .all(|item| {
                        item.split_once(':')
                            .is_some_and(|(name, value)| paint(tag, name.trim(), value.trim()))
                    })
        }
        "width" | "height" if tag == "svg" => length(value, false).is_some_and(|value| value > 0.0),
        "x" | "y" | "x1" | "y1" | "x2" | "y2" | "width" | "height" | "rx" | "ry" | "cx" | "cy"
        | "fx" | "fy" | "r" | "fr" | "offset" | "dx" | "dy" => length(value, true).is_some(),
        _ => paint(tag, name, value),
    }
}

fn paint(tag: &str, name: &str, value: &str) -> bool {
    match name {
        "fill" | "stroke" => {
            value == "none"
                || value == "currentColor"
                || value.parse::<Color>().is_ok()
                || value
                    .strip_prefix("url(#")
                    .and_then(|value| value.strip_suffix(')'))
                    .is_some_and(identifier)
        }
        "color" | "stop-color" => value.parse::<Color>().is_ok(),
        // Opacity on a group can allocate an entire separate raster layer.
        "opacity" if matches!(tag, "svg" | "g" | "text" | "tspan") => number(value) == Some(1.0),
        "opacity" | "fill-opacity" | "stroke-opacity" | "stop-opacity" => {
            number(value).is_some_and(|value| (0.0..=1.0).contains(&value))
        }
        "stroke-width" => length(value, false).is_some_and(|value| (0.0..=256.0).contains(&value)),
        "stroke-linecap" => matches!(value, "butt" | "round" | "square"),
        "stroke-linejoin" => matches!(value, "miter" | "round" | "bevel"),
        "fill-rule" => matches!(value, "nonzero" | "evenodd"),
        "font-size" => length(value, false).is_some_and(|value| (1.0..=256.0).contains(&value)),
        "font-family" => {
            value.len() <= 96
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b" -_,\"'".contains(&byte))
        }
        "font-weight" => matches!(value, "normal" | "bold" | "400" | "700"),
        "text-anchor" => matches!(value, "start" | "middle" | "end"),
        "dominant-baseline" => matches!(
            value,
            "auto" | "middle" | "central" | "hanging" | "alphabetic"
        ),
        _ => false,
    }
}

fn transform(value: &str) -> Option<Transform> {
    if !bounded_expression(value)
        || !TransformListParser::from(value)
            .enumerate()
            .all(|(index, token)| index < 8 && token.is_ok())
    {
        return None;
    }
    let matrix = value.parse::<Transform>().ok()?;
    [matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f]
        .into_iter()
        .all(|value| value.abs() <= MAX_COORDINATE)
        .then_some(matrix)
}

fn bounded_transform_chain(node: Node<'_, '_>) -> bool {
    let mut linear_bound = 1.0;
    let mut translation_bound = 0.0;
    for value in node
        .ancestors()
        .filter_map(|ancestor| ancestor.attribute("transform"))
    {
        let Some(matrix) = transform(value) else {
            return false;
        };
        let scale = (matrix.a.abs() + matrix.c.abs()).max(matrix.b.abs() + matrix.d.abs());
        let shift = matrix.e.abs().max(matrix.f.abs());
        linear_bound *= scale.max(1.0);
        translation_bound = translation_bound * scale + shift;
        if linear_bound > 128.0 || translation_bound > MAX_COORDINATE * 2.0 {
            return false;
        }
    }
    true
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn number(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.abs() <= MAX_COORDINATE)
}

fn length(value: &str, percent: bool) -> Option<f64> {
    let plain = value.strip_suffix("px").unwrap_or(value);
    if percent && let Some(value) = plain.strip_suffix('%') {
        number(value).filter(|value| (-1000.0..=1000.0).contains(value))
    } else {
        number(plain)
    }
}

fn numbers(value: &str, limit: usize) -> Option<Vec<f64>> {
    let values: Vec<_> = NumberListParser::from(value)
        .enumerate()
        .map(|(index, value)| {
            value
                .ok()
                .filter(|value| index < limit && value.abs() <= MAX_COORDINATE)
        })
        .collect::<Option<_>>()?;
    (!values.is_empty()).then_some(values)
}

fn view_box(value: &str) -> bool {
    numbers(value, 4)
        .is_some_and(|values| values.len() == 4 && values[2] >= 0.1 && values[3] >= 0.1)
}

fn bounded_expression(value: &str) -> bool {
    static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[+-]?(?:[0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE][+-]?[0-9]+)?")
            .expect("static SVG number pattern")
    });
    !value.is_empty()
        && NUMBER
            .find_iter(value)
            .all(|value| number(value.as_str()).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_active_remote_and_unbounded_svg() {
        for contents in [
            "<script>alert(1)</script>",
            "<image href='file:///tmp/read' />",
            "<use href='#cycle' id='cycle' />",
            "<style>@import url(https://example.invalid/theme.css)</style>",
            "<rect width='10' height='10' onclick='run()' />",
            "<rect width='10' height='10' style='fill:url(file:///tmp/read)' />",
            "<rect width='10' height='10' fill='url(https://example.invalid/img)' />",
            "<rect width='10' height='10' filter='url(#large)' />",
            "<rect width='10' height='1e99' />",
            "<path d='M 0 0 L 1e999 50' />",
        ] {
            assert!(
                render(&format!("<svg xmlns='{SVG_NS}'>{contents}</svg>")).is_err(),
                "{contents}"
            );
        }
        let dtd = format!(
            "<!DOCTYPE svg [<!ENTITY external SYSTEM 'file:///tmp/secret'>]><svg xmlns='{SVG_NS}'>&external;</svg>"
        );
        assert!(render(&dtd).is_err());
        let pi = format!(
            "<?xml-stylesheet href='file:///tmp/theme.css'?><svg xmlns='{SVG_NS}'><rect width='1' height='1'/></svg>"
        );
        assert!(render(&pi).is_err());
        let deep = format!(
            "<svg xmlns='{SVG_NS}'>{}<circle r='5'/>{}</svg>",
            "<g>".repeat(MAX_DEPTH),
            "</g>".repeat(MAX_DEPTH)
        );
        assert!(render(&deep).is_err());
    }

    #[test]
    fn previews_static_gradients_and_text_with_embedded_fonts() {
        let source = format!(
            "<svg xmlns='{SVG_NS}' width='2000' height='1000' viewBox='0 0 200 100'><defs><linearGradient id='paint'><stop offset='0' stop-color='#001122'/><stop offset='100%' stop-color='#117733'/></linearGradient></defs><rect width='200' height='100' fill='url(#paint)'/><text x='100' y='50' text-anchor='middle' font-size='16' fill='white'>Ready</text></svg>"
        );
        let preview = render(&source).unwrap();
        let iced::widget::image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = preview.handle
        else {
            panic!("the preview must be an in-memory RGBA image");
        };
        assert_eq!((width, height), (560, 280));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[1] > 35 && pixel[1] < 120)
        );
        assert!(pixels.as_chunks::<4>().0.contains(&[255, 255, 255, 255]));
    }
}
