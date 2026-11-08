use std::fmt::Write;

use crate::{
    Arrowheads, Diagram, DiagramKind, Direction, Edge, EdgeStyle, EndMarker, Node, NodeShape,
    ParseErrorKind, SourceRange,
    model::{Member, Relation, validate_label},
};

use super::{Cursor, encode_label, plain_label};

const MAX_TYPES: usize = 12;
const MAX_CLASS_MEMBERS: usize = 20;
const MAX_ER_ATTRIBUTES: usize = 40;
const MAX_MULTIPLICITY_CHARS: usize = 24;

#[derive(Default)]
pub(super) struct TypeParser {
    open: Option<(usize, usize)>,
    attributes: usize,
}

impl TypeParser {
    pub(super) fn read(
        &mut self,
        source: &str,
        line: usize,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let class = diagram.kind() == DiagramKind::Class;
        if let Some((node, _)) = self.open {
            if source == "}" {
                self.open = None;
                return Ok(());
            }
            return self.member(diagram, node, source, line);
        }
        if let Some(direction) = after_keyword(source, "direction") {
            diagram.set_direction(match direction {
                "LR" => Direction::LeftToRight,
                "TB" | "TD" => Direction::TopToBottom,
                _ => return Err(ParseErrorKind::Unsupported),
            });
            return Ok(());
        }
        if class && let Some(declaration) = after_keyword(source, "class") {
            let mut cursor = Cursor(declaration);
            let id = node_id(cursor.id()?)?;
            let block = declaration_tail(cursor.0.trim())?;
            self.declare(diagram, id, block, line)?;
            return Ok(());
        }
        if class {
            self.class_line(source, line, diagram)
        } else {
            self.er_line(source, line, diagram)
        }
    }

    pub(super) fn finish(&self, diagram: &Diagram) -> Result<(), (Option<usize>, ParseErrorKind)> {
        if let Some((_, line)) = self.open {
            return Err((Some(line), ParseErrorKind::Syntax));
        }
        if diagram.nodes().is_empty() {
            return Err((None, ParseErrorKind::Syntax));
        }
        Ok(())
    }

    fn declare(
        &mut self,
        diagram: &mut Diagram,
        id: &str,
        block: bool,
        line: usize,
    ) -> Result<(), ParseErrorKind> {
        let index = ensure_node(diagram, id, line)?;
        if let Some(range) = line_range(line) {
            diagram.nodes[index].set_source_range(range);
        }
        if block {
            self.open = Some((index, line));
        }
        Ok(())
    }

    fn member(
        &mut self,
        diagram: &mut Diagram,
        index: usize,
        source: &str,
        line: usize,
    ) -> Result<(), ParseErrorKind> {
        let text = if diagram.kind() == DiagramKind::Class {
            if diagram.nodes[index].members.len() == MAX_CLASS_MEMBERS {
                return Err(ParseErrorKind::Limit("20 members per class"));
            }
            class_member(source)?.to_owned()
        } else {
            if self.attributes == MAX_ER_ATTRIBUTES {
                return Err(ParseErrorKind::Limit("40 ER attributes"));
            }
            self.attributes += 1;
            er_attribute(source)?
        };
        validate_label(&text)?;
        diagram.nodes[index].members.push(Member {
            text,
            source_range: line_range(line),
        });
        Ok(())
    }

    fn class_line(
        &mut self,
        source: &str,
        line: usize,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let mut cursor = Cursor(source);
        let id = class_endpoint(&mut cursor)?;
        if cursor.take(":") {
            let node = ensure_node(diagram, id, line)?;
            return self.member(diagram, node, cursor.0.trim(), line);
        }
        let left = multiplicity(&mut cursor)?;
        let operator = CLASS_OPERATORS
            .iter()
            .find(|operator| cursor.take(operator.syntax))
            .ok_or(ParseErrorKind::Unsupported)?;
        let right = multiplicity(&mut cursor)?;
        let target = class_endpoint(&mut cursor)?;
        let label = relationship_label(&mut cursor, false)?;
        let mut edge = Edge::new(id, target, label)?
            .with_style(operator.style)
            .with_arrowheads(operator.arrows);
        edge.relation = Some(Relation {
            operator: operator.syntax.to_owned(),
            markers: operator.markers,
            multiplicities: [left, right],
        });
        add_relationship(diagram, edge, line)
    }

    fn er_line(
        &mut self,
        source: &str,
        line: usize,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let mut cursor = Cursor(source);
        let id = node_id(cursor.id()?)?;
        let tail = cursor.0.trim();
        if matches!(tail, "" | "{") {
            return self.declare(diagram, id, tail == "{", line);
        }
        let (left, start_marker) = cardinality(&mut cursor, &ER_LEFT)?;
        let (connector, style) = if cursor.take("--") {
            ("--", EdgeStyle::Solid)
        } else if cursor.take("..") {
            ("..", EdgeStyle::Dashed)
        } else {
            return Err(ParseErrorKind::Unsupported);
        };
        let (right, end_marker) = cardinality(&mut cursor, &ER_RIGHT)?;
        let target = node_id(cursor.id()?)?;
        let label = relationship_label(&mut cursor, true)?;
        let mut edge = Edge::new(id, target, label)?
            .with_style(style)
            .with_arrowheads(Arrowheads::Both);
        edge.relation = Some(Relation {
            operator: format!("{left}{connector}{right}"),
            markers: [start_marker, end_marker],
            multiplicities: [None, None],
        });
        add_relationship(diagram, edge, line)
    }
}

fn after_keyword<'a>(source: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = source.strip_prefix(keyword)?;
    rest.starts_with(char::is_whitespace)
        .then_some(rest.trim_start())
}

fn declaration_tail(tail: &str) -> Result<bool, ParseErrorKind> {
    match tail {
        "" => Ok(false),
        "{" => Ok(true),
        _ => Err(ParseErrorKind::Unsupported),
    }
}

fn node_id(id: &str) -> Result<&str, ParseErrorKind> {
    if !id.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
        return Err(ParseErrorKind::Syntax);
    }
    Ok(id)
}

fn class_endpoint<'a>(cursor: &mut Cursor<'a>) -> Result<&'a str, ParseErrorKind> {
    cursor.trim();
    let end = cursor
        .0
        .char_indices()
        .find(|&(index, ch)| {
            (!ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-'))
                || (index > 0
                    && CLASS_OPERATORS
                        .iter()
                        .any(|operator| cursor.0[index..].starts_with(operator.syntax)))
        })
        .map_or(cursor.0.len(), |(index, _)| index);
    let (id, rest) = cursor.0.split_at(end);
    cursor.0 = rest;
    node_id(id)
}

fn ensure_node(diagram: &mut Diagram, id: &str, line: usize) -> Result<usize, ParseErrorKind> {
    if let Some(index) = diagram.node_index(id) {
        return Ok(index);
    }
    let index = diagram.nodes().len();
    if index == MAX_TYPES {
        let limit = if diagram.kind() == DiagramKind::Class {
            "12 classes"
        } else {
            "12 ER entities"
        };
        return Err(ParseErrorKind::Limit(limit));
    }
    let mut node = Node::new(id, id, NodeShape::Compartment)?;
    if let Some(range) = line_range(line) {
        node = node.with_source_range(range);
    }
    diagram.add_node(node)?;
    Ok(index)
}

fn add_relationship(
    diagram: &mut Diagram,
    mut edge: Edge,
    line: usize,
) -> Result<(), ParseErrorKind> {
    ensure_node(diagram, edge.source(), line)?;
    ensure_node(diagram, edge.target(), line)?;
    if let Some(range) = line_range(line) {
        edge = edge.with_source_range(range);
    }
    diagram.add_edge(edge)?;
    Ok(())
}

fn multiplicity(cursor: &mut Cursor<'_>) -> Result<Option<String>, ParseErrorKind> {
    if !cursor.take("\"") {
        return Ok(None);
    }
    let label = plain_label(cursor.until("\"")?)?;
    if label.trim().is_empty() || label.contains('\n') {
        return Err(ParseErrorKind::Syntax);
    }
    if label.chars().count() > MAX_MULTIPLICITY_CHARS {
        return Err(ParseErrorKind::Limit("24 characters in a multiplicity"));
    }
    Ok(Some(label))
}

fn relationship_label(cursor: &mut Cursor<'_>, required: bool) -> Result<String, ParseErrorKind> {
    if !cursor.take(":") {
        return if !required && cursor.0.trim().is_empty() {
            Ok(String::new())
        } else {
            Err(ParseErrorKind::Syntax)
        };
    }
    let label = if cursor.take("\"") {
        let label = cursor.until("\"")?;
        if !cursor.0.trim().is_empty() {
            return Err(ParseErrorKind::Unsupported);
        }
        label
    } else {
        cursor.0.trim()
    };
    let label = plain_label(label)?;
    if label.trim().is_empty() {
        return Err(ParseErrorKind::Syntax);
    }
    Ok(label)
}

fn class_member(source: &str) -> Result<&str, ParseErrorKind> {
    let name = source
        .strip_prefix(['+', '-', '#', '~'])
        .unwrap_or(source)
        .trim_start();
    if source.len() > 120
        || !name
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        || !name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'_' | b'$'
                        | b'~'
                        | b','
                        | b'.'
                        | b'('
                        | b')'
                        | b'['
                        | b']'
                        | b' '
                        | b'*'
                        | b'-'
                )
        })
    {
        return Err(ParseErrorKind::Unsupported);
    }
    Ok(source)
}

fn er_attribute(source: &str) -> Result<String, ParseErrorKind> {
    let (kind, rest) = word(source);
    let (name, tail) = word(rest);
    if !attribute_token(kind, true) || !attribute_token(name, false) {
        return Err(ParseErrorKind::Unsupported);
    }
    let (keys, comment) = match tail.split_once('"') {
        Some((keys, comment)) => {
            let (comment, rest) = comment.split_once('"').ok_or(ParseErrorKind::Syntax)?;
            if !rest.trim().is_empty() {
                return Err(ParseErrorKind::Unsupported);
            }
            (keys.trim(), Some(plain_label(comment)?))
        }
        None => (tail.trim(), None),
    };
    let mut attribute = format!("{kind} {name}");
    if !keys.is_empty() {
        let mut seen = [false; 3];
        for (index, key) in keys.split(',').map(str::trim).enumerate() {
            let key_index = match key {
                "PK" => 0,
                "FK" => 1,
                "UK" => 2,
                _ => return Err(ParseErrorKind::Unsupported),
            };
            if std::mem::replace(&mut seen[key_index], true) {
                return Err(ParseErrorKind::Syntax);
            }
            attribute.push_str(if index == 0 { " " } else { ", " });
            attribute.push_str(key);
        }
    }
    if let Some(comment) = comment {
        let _ = write!(attribute, " \"{comment}\"");
    }
    Ok(attribute)
}

fn word(source: &str) -> (&str, &str) {
    source
        .split_once(char::is_whitespace)
        .map_or((source, ""), |(head, tail)| (head, tail.trim()))
}

fn attribute_token(token: &str, kind: bool) -> bool {
    token.len() <= 64
        && token
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && token.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || byte == b'_'
                || kind && matches!(byte, b'(' | b')' | b'[' | b']' | b'-')
        })
}

fn line_range(line: usize) -> Option<SourceRange> {
    SourceRange::new(line as u32, line as u32)
}

struct ClassOperator {
    syntax: &'static str,
    style: EdgeStyle,
    arrows: Arrowheads,
    markers: [EndMarker; 2],
}

impl ClassOperator {
    const fn new(
        syntax: &'static str,
        style: EdgeStyle,
        arrows: Arrowheads,
        marker: EndMarker,
    ) -> Self {
        let markers = match arrows {
            Arrowheads::Start => [marker, EndMarker::Filled],
            _ => [EndMarker::Filled, marker],
        };
        Self {
            syntax,
            style,
            arrows,
            markers,
        }
    }
}

use Arrowheads::{End, None as NoArrow, Start};
use EdgeStyle::{Dashed, Solid};
use EndMarker::{
    Aggregation, Composition, ExactlyOne, Filled, Inheritance, OneOrMany, Open, ZeroOrMany,
    ZeroOrOne,
};

const CLASS_OPERATORS: [ClassOperator; 14] = [
    ClassOperator::new("<|--", Solid, Start, Inheritance),
    ClassOperator::new("--|>", Solid, End, Inheritance),
    ClassOperator::new("<|..", Dashed, Start, Inheritance),
    ClassOperator::new("..|>", Dashed, End, Inheritance),
    ClassOperator::new("*--", Solid, Start, Composition),
    ClassOperator::new("--*", Solid, End, Composition),
    ClassOperator::new("o--", Solid, Start, Aggregation),
    ClassOperator::new("--o", Solid, End, Aggregation),
    ClassOperator::new("-->", Solid, End, Open),
    ClassOperator::new("<--", Solid, Start, Open),
    ClassOperator::new("..>", Dashed, End, Open),
    ClassOperator::new("<..", Dashed, Start, Open),
    ClassOperator::new("--", Solid, NoArrow, Filled),
    ClassOperator::new("..", Dashed, NoArrow, Filled),
];

const ER_LEFT: [(&str, EndMarker); 5] = [
    ("||", ExactlyOne),
    ("o|", ZeroOrOne),
    ("|o", ZeroOrOne),
    ("}|", OneOrMany),
    ("}o", ZeroOrMany),
];
const ER_RIGHT: [(&str, EndMarker); 5] = [
    ("||", ExactlyOne),
    ("o|", ZeroOrOne),
    ("|o", ZeroOrOne),
    ("|{", OneOrMany),
    ("o{", ZeroOrMany),
];

fn cardinality<'a>(
    cursor: &mut Cursor<'_>,
    sides: &'a [(&'a str, EndMarker)],
) -> Result<(&'a str, EndMarker), ParseErrorKind> {
    sides
        .iter()
        .copied()
        .find(|(syntax, _)| cursor.take(syntax))
        .ok_or(ParseErrorKind::Unsupported)
}

pub(super) fn to_mermaid(diagram: &Diagram) -> String {
    let class = diagram.kind() == DiagramKind::Class;
    let mut output = String::from(if class {
        "classDiagram\n"
    } else {
        "erDiagram\n"
    });
    if diagram.direction() == Direction::LeftToRight {
        output.push_str("    direction LR\n");
    }
    for node in diagram.nodes() {
        let prefix = if class { "class " } else { "" };
        let _ = write!(output, "    {prefix}{}", node.id());
        if node.members.is_empty() {
            output.push('\n');
            continue;
        }
        output.push_str(" {\n");
        for member in node.members() {
            if !class
                && let Some((head, tail)) = member.split_once('"')
                && let Some(comment) = tail.strip_suffix('"')
            {
                let _ = writeln!(output, "        {head}\"{}\"", encode_label(comment));
            } else {
                let _ = writeln!(output, "        {member}");
            }
        }
        output.push_str("    }\n");
    }
    for edge in diagram.edges() {
        let Some(relation) = &edge.relation else {
            continue;
        };
        let _ = write!(output, "    {} ", edge.source());
        if let Some(left) = relation.multiplicities[0].as_deref() {
            let _ = write!(output, "\"{}\" ", encode_label(left));
        }
        let _ = write!(output, "{} ", relation.operator);
        if let Some(right) = relation.multiplicities[1].as_deref() {
            let _ = write!(output, "\"{}\" ", encode_label(right));
        }
        output.push_str(edge.target());
        if !edge.label().is_empty() {
            let label = encode_label(edge.label());
            if class {
                let _ = write!(output, ": {label}");
            } else {
                let _ = write!(output, ": \"{label}\"");
            }
        }
        output.push('\n');
    }
    output
}
