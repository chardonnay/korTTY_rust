use std::fmt::Write;

use crate::{Diagram, DiagramError, Direction, Edge, Node, NodeShape, ParseErrorKind, SourceRange};

use super::{Cursor, encode_label, plain_label};

const INITIAL_ID: &str = "_kortty_state_start";
const FINAL_ID: &str = "_kortty_state_end";
const MAX_STATES: usize = 12;

pub(super) fn read_state(
    line: &str,
    number: usize,
    diagram: &mut Diagram,
) -> Result<(), ParseErrorKind> {
    if let Some(direction) = after_keyword(line, "direction") {
        diagram.set_direction(match direction.trim() {
            "LR" => Direction::LeftToRight,
            "TD" | "TB" => Direction::TopToBottom,
            _ => return Err(ParseErrorKind::Unsupported),
        });
        return Ok(());
    }
    if let Some(declaration) = after_keyword(line, "state") {
        return read_alias(declaration, number, diagram);
    }
    let mut cursor = Cursor(line);
    let source = endpoint(&mut cursor, true)?;
    if cursor.take("-->") {
        let target = endpoint(&mut cursor, false)?;
        if source == INITIAL_ID && target == FINAL_ID {
            return Err(ParseErrorKind::Syntax);
        }
        let label = if cursor.take(":") {
            if cursor.0.starts_with(':') || cursor.0.trim().is_empty() {
                return Err(ParseErrorKind::Unsupported);
            }
            plain_label(cursor.0.trim())?
        } else if cursor.0.trim().is_empty() {
            String::new()
        } else {
            return Err(ParseErrorKind::Unsupported);
        };
        state_node(diagram, source, number)?;
        state_node(diagram, target, number)?;
        let mut edge = Edge::new(source, target, label)?;
        if let Some(range) = source_line(number) {
            edge = edge.with_source_range(range);
        }
        diagram.add_edge(edge)?;
    } else if cursor.take(":") && source != INITIAL_ID {
        if cursor.0.starts_with(':') || cursor.0.trim().is_empty() {
            return Err(ParseErrorKind::Unsupported);
        }
        let description = plain_label(cursor.0.trim())?;
        let index = state_node(diagram, source, number)?;
        let previous = &diagram.nodes[index];
        let label = format!("{}\n{description}", previous.label());
        diagram.nodes[index] = replace_label(previous, label, previous.source_range())?;
    } else if cursor.0.trim().is_empty() && source != INITIAL_ID {
        state_node(diagram, source, number)?;
    } else {
        return Err(ParseErrorKind::Unsupported);
    }
    Ok(())
}

fn read_alias(
    declaration: &str,
    number: usize,
    diagram: &mut Diagram,
) -> Result<(), ParseErrorKind> {
    let mut cursor = Cursor(declaration);
    if !cursor.take("\"") {
        return Err(ParseErrorKind::Unsupported);
    }
    let alias = plain_label(cursor.until("\"")?)?;
    let Some(id) = after_keyword(cursor.0.trim_start(), "as") else {
        return Err(ParseErrorKind::Syntax);
    };
    if alias.trim().is_empty() || alias.contains('\n') {
        return Err(ParseErrorKind::Syntax);
    }
    cursor.0 = id;
    let id = named_endpoint(&mut cursor)?;
    if !cursor.0.trim().is_empty() {
        return Err(ParseErrorKind::Unsupported);
    }
    let index = state_node(diagram, id, number)?;
    let previous = &diagram.nodes[index];
    let (current, details) = previous
        .label()
        .split_once('\n')
        .unwrap_or((previous.label(), ""));
    if current != id && current != alias {
        return Err(DiagramError::DuplicateNode(id.to_owned()).into());
    }
    let label = if details.is_empty() {
        alias
    } else {
        format!("{alias}\n{details}")
    };
    diagram.nodes[index] = replace_label(previous, label, source_line(number))?;
    Ok(())
}

fn after_keyword<'a>(source: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = source.strip_prefix(keyword)?;
    rest.starts_with(char::is_whitespace)
        .then_some(rest.trim_start())
}

fn endpoint<'a>(cursor: &mut Cursor<'a>, initial: bool) -> Result<&'a str, ParseErrorKind> {
    if cursor.take("[*]") {
        Ok(if initial { INITIAL_ID } else { FINAL_ID })
    } else {
        named_endpoint(cursor)
    }
}

fn named_endpoint<'a>(cursor: &mut Cursor<'a>) -> Result<&'a str, ParseErrorKind> {
    let id = cursor.id()?;
    if !id.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
        return Err(ParseErrorKind::Syntax);
    }
    Ok(id)
}

fn state_node(diagram: &mut Diagram, id: &str, number: usize) -> Result<usize, ParseErrorKind> {
    if let Some(index) = diagram.node_index(id) {
        return Ok(index);
    }
    let (label, shape) = match id {
        INITIAL_ID => ("", NodeShape::StateStart),
        FINAL_ID => ("", NodeShape::StateEnd),
        _ => {
            let count = diagram
                .nodes()
                .iter()
                .filter(|node| node.shape() == NodeShape::Terminal)
                .count();
            if count == MAX_STATES {
                return Err(ParseErrorKind::Limit("12 named states"));
            }
            (id, NodeShape::Terminal)
        }
    };
    let mut node = Node::new(id, label, shape)?;
    if let Some(range) = source_line(number) {
        node = node.with_source_range(range);
    }
    let index = diagram.nodes().len();
    diagram.add_node(node)?;
    Ok(index)
}

fn replace_label(
    node: &Node,
    label: String,
    source: Option<SourceRange>,
) -> Result<Node, DiagramError> {
    let node = Node::new(node.id(), label, node.shape())?.with_tone(node.tone());
    Ok(if let Some(range) = source {
        node.with_source_range(range)
    } else {
        node
    })
}

fn source_line(number: usize) -> Option<SourceRange> {
    SourceRange::new(number as u32, number as u32)
}

pub(super) fn to_mermaid(diagram: &Diagram) -> String {
    let mut output = String::from("stateDiagram-v2\n");
    if diagram.direction() == Direction::LeftToRight {
        output.push_str("    direction LR\n");
    }
    for node in diagram
        .nodes()
        .iter()
        .filter(|node| !matches!(node.shape(), NodeShape::StateStart | NodeShape::StateEnd))
    {
        let (caption, description) = node.label().split_once('\n').unwrap_or((node.label(), ""));
        if caption == node.id() {
            let _ = writeln!(output, "    {}", node.id());
        } else {
            let _ = writeln!(
                output,
                "    state \"{}\" as {}",
                encode_label(caption),
                node.id()
            );
        }
        if !description.is_empty() {
            let _ = writeln!(output, "    {}: {}", node.id(), encode_label(description));
        }
    }
    for edge in diagram.edges() {
        let endpoint = |id| {
            if matches!(id, INITIAL_ID | FINAL_ID) {
                "[*]"
            } else {
                id
            }
        };
        let _ = write!(
            output,
            "    {} --> {}",
            endpoint(edge.source()),
            endpoint(edge.target())
        );
        if !edge.label().is_empty() {
            let _ = write!(output, ": {}", encode_label(edge.label()));
        }
        output.push('\n');
    }
    output
}
