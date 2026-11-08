use crate::{Arrowheads, Diagram, Edge, Node, NodeShape, NodeTone};

use super::{Cursor, ParseErrorKind, decode_label};

#[derive(Default)]
pub(super) struct MindmapOutline {
    ancestors: Vec<(usize, String)>,
}

impl MindmapOutline {
    pub(super) fn read_node(
        &mut self,
        raw_line: &str,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let line = raw_line.trim_start_matches(' ');
        if line.starts_with(char::is_whitespace) {
            return Err(ParseErrorKind::Syntax);
        }
        let indent = raw_line.len() - line.len();
        let line = line.trim_end();
        if line.starts_with("::icon") {
            return Err(ParseErrorKind::Unsafe);
        }
        if line.starts_with(":::") {
            return Err(ParseErrorKind::Unsupported);
        }
        while self
            .ancestors
            .last()
            .is_some_and(|(level, _)| *level >= indent)
        {
            self.ancestors.pop();
        }
        let is_root = diagram.nodes().is_empty();
        if !is_root && self.ancestors.is_empty() {
            return Err(ParseErrorKind::Syntax);
        }

        let (explicit_id, text, shape) = mindmap_node(line)?;
        let id = explicit_id.map(str::to_owned).unwrap_or_else(|| {
            let mut index = diagram.nodes().len();
            loop {
                let id = format!("kortty_mindmap_{index}");
                if diagram.node_index(&id).is_none() {
                    break id;
                }
                index += 1;
            }
        });
        let mut node = Node::new(&id, decode_label(text), shape)?;
        if is_root {
            node = node.with_tone(NodeTone::Active);
        }
        diagram.add_node(node)?;
        if let Some((_, parent)) = self.ancestors.last() {
            diagram.add_edge(Edge::new(parent, &id, "")?.with_arrowheads(Arrowheads::None))?;
        }
        self.ancestors.push((indent, id));
        Ok(())
    }
}

fn mindmap_node(line: &str) -> Result<(Option<&str>, &str, NodeShape), ParseErrorKind> {
    let mut cursor = Cursor(line);
    let Ok(id) = cursor.id() else {
        return Ok((None, line, NodeShape::Action));
    };
    // A delimiter after whitespace is part of an ordinary text label.
    if cursor.0.starts_with(char::is_whitespace) {
        return Ok((None, line, NodeShape::Action));
    }
    let (shape, delimiter) = if cursor.take("((") {
        (NodeShape::Terminal, "))")
    } else if cursor.take("(") {
        (NodeShape::Terminal, ")")
    } else if cursor.take("[") {
        (NodeShape::Action, "]")
    } else if cursor.0.starts_with(['{', ')']) {
        return Err(ParseErrorKind::Unsupported);
    } else {
        return Ok((None, line, NodeShape::Action));
    };
    let label = cursor.until(delimiter)?;
    if !cursor.0.trim().is_empty() {
        return Err(ParseErrorKind::Unsupported);
    }
    Ok((Some(id), label.trim_matches('"'), shape))
}

pub(super) fn read_architecture(line: &str, diagram: &mut Diagram) -> Result<(), ParseErrorKind> {
    let mut cursor = Cursor(line);
    let first = cursor.id()?;
    if cursor.0.starts_with(char::is_whitespace) {
        match first {
            "group" | "align" => return Err(ParseErrorKind::Unsupported),
            "service" => return read_service(cursor, diagram),
            "junction" => return read_junction(cursor, diagram),
            _ => {}
        }
    }
    read_architecture_edge(first, cursor, diagram)
}

fn read_service(mut cursor: Cursor<'_>, diagram: &mut Diagram) -> Result<(), ParseErrorKind> {
    let id = cursor.id()?;
    let icon = if cursor.take("(") {
        let icon = cursor.until(")")?;
        if !matches!(icon, "cloud" | "database" | "disk" | "internet" | "server") {
            return Err(ParseErrorKind::Unsafe);
        }
        Some(icon)
    } else {
        None
    };
    let label = if cursor.take("[") {
        cursor.until("]")?
    } else {
        id
    };
    if !cursor.0.trim().is_empty() {
        return Err(ParseErrorKind::Unsupported);
    }
    let mut label = decode_label(label);
    if let Some(icon) = icon {
        label.push('\n');
        label.push_str(icon);
    }
    diagram.add_node(Node::new(id, label, NodeShape::Action)?)?;
    Ok(())
}

fn read_junction(mut cursor: Cursor<'_>, diagram: &mut Diagram) -> Result<(), ParseErrorKind> {
    let id = cursor.id()?;
    if !cursor.0.trim().is_empty() {
        return Err(ParseErrorKind::Unsupported);
    }
    diagram.add_node(Node::new(id, id, NodeShape::Terminal)?)?;
    Ok(())
}

fn read_architecture_edge(
    source: &str,
    mut cursor: Cursor<'_>,
    diagram: &mut Diagram,
) -> Result<(), ParseErrorKind> {
    if cursor.take("{") {
        return Err(ParseErrorKind::Unsupported);
    }
    if !cursor.take(":") || !valid_side(cursor.id()?) {
        return Err(ParseErrorKind::Syntax);
    }
    let arrowheads = if cursor.take("<-->") {
        Arrowheads::Both
    } else if cursor.take("<--") {
        Arrowheads::Start
    } else if cursor.take("-->") {
        Arrowheads::End
    } else if cursor.take("--") {
        Arrowheads::None
    } else {
        return Err(ParseErrorKind::Syntax);
    };
    if !valid_side(cursor.id()?) || !cursor.take(":") {
        return Err(ParseErrorKind::Syntax);
    }
    let target = cursor.id()?;
    if !cursor.0.trim().is_empty() {
        return Err(ParseErrorKind::Unsupported);
    }
    diagram.add_edge(Edge::new(source, target, "")?.with_arrowheads(arrowheads))?;
    Ok(())
}

fn valid_side(side: &str) -> bool {
    matches!(side, "T" | "B" | "L" | "R")
}
