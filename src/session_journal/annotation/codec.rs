use std::fmt::Write as _;
use std::io;

use roxmltree::Node;

use crate::session_journal::metadata::escape;
use crate::session_journal::{field, scalar_text};

use super::{
    Annotation, DEFAULT_COLOR, DEFAULT_FONT_SIZE, DEFAULT_STROKE_WIDTH, Kind, MAX_ANNOTATIONS,
    MAX_POINTS_PER_MARK, MAX_POINTS_TOTAL, MAX_TEXT_BYTES, MAX_TEXT_TOTAL, invalid, size_limit,
    validate,
};

const FIELDS: &[&str] = &["kind", "color", "strokeWidth", "fontSize", "text", "points"];

/// Reads exactly one unambiguous Java annotation wrapper from its entry.
pub(crate) fn read(entry: Node<'_, '_>) -> io::Result<Vec<Annotation>> {
    let Some(wrapper) = unique_child(entry, "annotations")? else {
        return Ok(Vec::new());
    };
    require_children(wrapper, &["annotation"])?;
    let mut marks = Vec::new();
    let mut points_remaining = MAX_POINTS_TOTAL;
    let mut text_remaining = MAX_TEXT_TOTAL;
    for node in wrapper.children().filter(Node::is_element) {
        if marks.len() == MAX_ANNOTATIONS {
            return Err(size_limit());
        }
        let mark = read_mark(node, points_remaining, text_remaining)?;
        points_remaining -= mark.points.len();
        text_remaining -= mark.text.len();
        marks.push(mark);
    }
    validate(&marks)?;
    Ok(marks)
}

fn read_mark(node: Node<'_, '_>, points: usize, text: usize) -> io::Result<Annotation> {
    require_children(node, FIELDS)?;
    let kind = match field(node, "kind")?.as_deref() {
        None | Some("PEN") => Kind::Pen,
        Some("BOX") => Kind::Box,
        Some("PIXELATE") => Kind::Pixelate,
        Some("TEXT") => Kind::Text,
        Some(_) => return Err(invalid("Unknown journal screenshot annotation tool.")),
    };
    let color = field(node, "color")?
        .as_deref()
        .and_then(parse_color)
        .unwrap_or(DEFAULT_COLOR);
    let stroke_width = read_size(node, "strokeWidth", DEFAULT_STROKE_WIDTH)?;
    let font_size = read_size(node, "fontSize", DEFAULT_FONT_SIZE)?;
    let text_value = field(node, "text")?.unwrap_or_default();
    if text_value.len() > text.min(MAX_TEXT_BYTES) {
        return Err(size_limit());
    }
    let points = read_points(node, points.min(MAX_POINTS_PER_MARK))?;
    Ok(Annotation {
        kind,
        color,
        stroke_width,
        font_size,
        text: text_value.into_owned(),
        points,
    })
}

fn read_points(node: Node<'_, '_>, limit: usize) -> io::Result<Vec<[f32; 2]>> {
    let Some(wrapper) = unique_child(node, "points")? else {
        return Ok(Vec::new());
    };
    require_children(wrapper, &["v"])?;
    let mut nodes = wrapper.children().filter(Node::is_element);
    let mut points = Vec::new();
    while let Some(x) = nodes.next() {
        if points.len() == limit {
            return Err(size_limit());
        }
        let y = nodes
            .next()
            .ok_or_else(|| invalid("Unpaired journal screenshot coordinate."))?;
        points.push([coordinate(x)?, coordinate(y)?]);
    }
    Ok(points)
}

fn coordinate(node: Node<'_, '_>) -> io::Result<f32> {
    require_attributes(node)?;
    finite_number(&scalar_text(node)?)
}

fn read_size(node: Node<'_, '_>, name: &str, fallback: f32) -> io::Result<f32> {
    let Some(value) = field(node, name)? else {
        return Ok(fallback);
    };
    let value = finite_number(&value)?;
    Ok(if value > 0.0 { value } else { fallback })
}

fn finite_number(value: &str) -> io::Result<f32> {
    let value = value.trim();
    if value.len() <= 64
        && let Ok(number) = value.parse::<f32>()
        && number.is_finite()
    {
        return Ok(number);
    }
    Err(invalid("Invalid journal screenshot annotation number."))
}

fn parse_color(value: &str) -> Option<[u8; 3]> {
    let value = value.trim();
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.is_empty() || hex.len() > 8 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let number = i32::from_str_radix(hex, 16).ok()?;
    if hex.len() == 3 {
        return Some([
            (((number >> 8) & 0xf) * 17) as u8,
            (((number >> 4) & 0xf) * 17) as u8,
            ((number & 0xf) * 17) as u8,
        ]);
    }
    let [_, red, green, blue] = number.to_be_bytes();
    Some([red, green, blue])
}

fn unique_child<'a, 'input>(
    node: Node<'a, 'input>,
    name: &str,
) -> io::Result<Option<Node<'a, 'input>>> {
    let mut matches = node.children().filter(|child| child.has_tag_name(name));
    let first = matches.next();
    if matches.next().is_some() || first.is_some_and(|child| child.tag_name().namespace().is_some())
    {
        return Err(invalid("Ambiguous journal screenshot annotation XML."));
    }
    Ok(first)
}

fn require_children(node: Node<'_, '_>, allowed: &[&str]) -> io::Result<()> {
    require_attributes(node)?;
    for child in node.children() {
        if child.is_element() {
            if child.tag_name().namespace().is_some() || !allowed.contains(&child.tag_name().name())
            {
                return Err(invalid("Unexpected journal screenshot annotation XML."));
            }
            require_attributes(child)?;
        } else if child.is_text() && child.text().is_some_and(|text| !text.trim().is_empty()) {
            return Err(invalid(
                "Unexpected journal screenshot annotation XML text.",
            ));
        }
    }
    Ok(())
}

fn require_attributes(node: Node<'_, '_>) -> io::Result<()> {
    if node.attributes().next().is_some() {
        return Err(invalid(
            "Unexpected journal screenshot annotation XML attribute.",
        ));
    }
    Ok(())
}

/// Returns the complete Java annotation wrapper, or an empty string when it would be omitted.
pub(crate) fn xml(annotations: &[Annotation]) -> io::Result<String> {
    let mut output = String::new();
    write_xml(&mut output, annotations)?;
    Ok(output)
}

pub(crate) fn write_xml(output: &mut String, annotations: &[Annotation]) -> io::Result<()> {
    validate(annotations)?;
    if annotations.is_empty() {
        return Ok(());
    }
    output.push_str("<annotations>\n");
    for mark in annotations {
        let kind = match mark.kind {
            Kind::Pen => "PEN",
            Kind::Box => "BOX",
            Kind::Pixelate => "PIXELATE",
            Kind::Text => "TEXT",
        };
        let [red, green, blue] = mark.color;
        let stroke = mark.stroke_width;
        let font = mark.font_size;
        writeln!(output, "<annotation><kind>{kind}</kind><color>#{red:02x}{green:02x}{blue:02x}</color><strokeWidth>{stroke}</strokeWidth><fontSize>{font}</fontSize>")
            .expect("writing to a String");
        if !mark.text.is_empty() {
            writeln!(output, "<text>{}</text>", escape(&mark.text)).expect("writing to a String");
        }
        output.push_str("<points>");
        for [x, y] in &mark.points {
            write!(output, "<v>{x}</v><v>{y}</v>").expect("writing to a String");
        }
        output.push_str("</points></annotation>\n");
    }
    output.push_str("</annotations>");
    Ok(())
}
