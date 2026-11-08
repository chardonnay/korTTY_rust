use std::{collections::HashSet, error::Error, fmt, fmt::Write};

use crate::{
    Arrowheads, Diagram, DiagramError, DiagramKind, Direction, Edge, EdgeStyle, Node, NodeShape,
    NodeTone, SourceRange, Theme, model::validate_label, sequence::SequenceParser,
};

mod relations;
mod state;
mod structured;

use structured::{MindmapOutline, read_architecture};

/// Maximum UTF-8 byte size of a diagram source before parsing.
pub const MAX_SOURCE_BYTES: usize = 32 * 1024;

/// Reason a Mermaid source could not be rendered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseErrorKind {
    /// The source exceeds [`MAX_SOURCE_BYTES`].
    SourceTooLarge,
    /// The source has no diagram header.
    MissingHeader,
    /// The diagram requests syntax outside the supported subset.
    Unsupported,
    /// The diagram exceeds a family-specific structural limit.
    Limit(&'static str),
    /// The diagram contains a configuration or action that is unsafe to render.
    Unsafe,
    /// A supported statement is malformed.
    Syntax,
    /// A validated model limit or invariant was violated.
    Model(DiagramError),
}

/// A recoverable parse error with a one-based source line when available.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    line: Option<usize>,
    kind: ParseErrorKind,
}

impl ParseError {
    /// Returns the source line where the problem was found.
    pub fn line(&self) -> Option<usize> {
        self.line
    }

    /// Returns the reason the source could not be read.
    pub fn kind(&self) -> &ParseErrorKind {
        &self.kind
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(formatter, "line {line}: ")?;
        }
        match &self.kind {
            ParseErrorKind::SourceTooLarge => {
                write!(formatter, "Mermaid source exceeds {MAX_SOURCE_BYTES} bytes")
            }
            ParseErrorKind::MissingHeader => {
                write!(formatter, "expected a Mermaid diagram header")
            }
            ParseErrorKind::Unsupported => write!(formatter, "unsupported Mermaid syntax"),
            ParseErrorKind::Limit(limit) => write!(formatter, "diagram exceeds {limit}"),
            ParseErrorKind::Unsafe => write!(formatter, "unsafe Mermaid configuration or action"),
            ParseErrorKind::Syntax => write!(formatter, "malformed Mermaid diagram statement"),
            ParseErrorKind::Model(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            ParseErrorKind::Model(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DiagramError> for ParseErrorKind {
    fn from(error: DiagramError) -> Self {
        Self::Model(error)
    }
}

/// Reads the safe subset of Mermaid syntax used for local diagrams.
///
/// Flowcharts support `flowchart` and `graph` in `TD`, `TB` or `LR`, implicit
/// nodes, rectangular, rounded and decision nodes, chained solid or dashed
/// arrows, pipe-delimited edge labels, comments, semicolon statement separators,
/// and the snippet classes `setup`, `work`, `success` and `failure`.
///
/// Mindmaps support a single space-indented outline with plain, square and
/// rounded labels. Architecture diagrams support flat services, junctions
/// and connections. Service icons are shown as type text, and the native graph
/// layout routes edges without using the Mermaid port-placement hints.
/// Sequence diagrams support ordered participant interactions, notes, and
/// `alt`, `opt`, `loop` and `par` frames. State diagrams support flat named
/// states, start and end markers and labeled transitions.
/// Class diagrams support member compartments and UML relationships. ER
/// diagrams support typed attributes and crow's-foot cardinalities.
/// Input is the contents of a Mermaid code block, without Markdown fences.
///
/// # Errors
///
/// Returns a structured error for invalid, unsafe, oversized or unsupported
/// input. The caller can keep the original source available on failure.
pub fn parse_mermaid(source: &str) -> Result<Diagram, ParseError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ParseError {
            line: None,
            kind: ParseErrorKind::SourceTooLarge,
        });
    }
    let mut diagram: Option<Diagram> = None;
    let mut mindmap = MindmapOutline::default();
    let mut sequence = SequenceParser::default();
    let mut types = relations::TypeParser::default();
    let mut declared = HashSet::new();
    for (index, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        let error_at = |kind| ParseError {
            line: Some(index + 1),
            kind,
        };
        if line.starts_with("classDef ")
            && diagram
                .as_ref()
                .is_some_and(|diagram| diagram.kind() == DiagramKind::Flowchart)
            && FLOWCHART_TONES.iter().any(|&(name, tone)| {
                [Theme::Light, Theme::Dark]
                    .into_iter()
                    .any(|theme| flowchart_class_definition(name, tone, theme) == line)
            })
        {
            continue;
        }
        if unsafe_statement(line) {
            return Err(error_at(ParseErrorKind::Unsafe));
        }
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if let Some(diagram) = diagram.as_mut() {
            match diagram.kind() {
                DiagramKind::Flowchart => read_statements(line, index + 1, diagram, &mut declared),
                DiagramKind::Mindmap => mindmap.read_node(raw_line, diagram),
                DiagramKind::Architecture => read_architecture(line, diagram),
                DiagramKind::Sequence => sequence.read(line, index + 1, diagram),
                DiagramKind::State => state::read_state(line, index + 1, diagram),
                DiagramKind::Class | DiagramKind::EntityRelationship => {
                    types.read(line, index + 1, diagram)
                }
            }
            .map_err(error_at)?;
        } else {
            diagram = Some(match line {
                "mindmap" => Diagram::of_kind(DiagramKind::Mindmap, Direction::LeftToRight),
                "architecture-beta" => {
                    Diagram::of_kind(DiagramKind::Architecture, Direction::LeftToRight)
                }
                header if header.eq_ignore_ascii_case("sequenceDiagram") => {
                    Diagram::of_kind(DiagramKind::Sequence, Direction::TopToBottom)
                }
                header if header.eq_ignore_ascii_case("classDiagram") => {
                    Diagram::of_kind(DiagramKind::Class, Direction::TopToBottom)
                }
                header if header.eq_ignore_ascii_case("erDiagram") => {
                    Diagram::of_kind(DiagramKind::EntityRelationship, Direction::TopToBottom)
                }
                header
                    if header.eq_ignore_ascii_case("stateDiagram-v2")
                        || header.eq_ignore_ascii_case("stateDiagram") =>
                {
                    Diagram::of_kind(DiagramKind::State, Direction::TopToBottom)
                }
                _ => Diagram::new(read_header(line).map_err(error_at)?),
            });
        }
    }
    let mut diagram = diagram.ok_or(ParseError {
        line: None,
        kind: ParseErrorKind::MissingHeader,
    })?;
    if diagram.kind() == DiagramKind::State && diagram.edges().is_empty() {
        return Err(ParseError {
            line: None,
            kind: ParseErrorKind::Syntax,
        });
    }
    if diagram.kind() == DiagramKind::Sequence {
        diagram.sequence = Some(
            sequence
                .finish()
                .map_err(|(line, kind)| ParseError { line, kind })?,
        );
    }
    if matches!(
        diagram.kind(),
        DiagramKind::Class | DiagramKind::EntityRelationship
    ) {
        types
            .finish(&diagram)
            .map_err(|(line, kind)| ParseError { line, kind })?;
    }
    Ok(diagram)
}

fn read_header(line: &str) -> Result<Direction, ParseErrorKind> {
    let mut words = line.trim_end_matches(';').split_whitespace();
    if !matches!(words.next(), Some("flowchart" | "graph")) {
        return Err(ParseErrorKind::Unsupported);
    }
    let direction = match words.next() {
        Some("TD" | "TB") => Direction::TopToBottom,
        Some("LR") => Direction::LeftToRight,
        Some(_) => return Err(ParseErrorKind::Unsupported),
        None => return Err(ParseErrorKind::Syntax),
    };
    if words.next().is_some() {
        return Err(ParseErrorKind::Syntax);
    }
    Ok(direction)
}

fn read_statements(
    line: &str,
    line_number: usize,
    diagram: &mut Diagram,
    declared: &mut HashSet<String>,
) -> Result<(), ParseErrorKind> {
    let mut cursor = Cursor(line);
    while !cursor.0.is_empty() {
        cursor.trim();
        if cursor.0.is_empty() || cursor.0.starts_with("%%") && !cursor.0.starts_with("%%{") {
            return Ok(());
        }
        if unsafe_statement(cursor.0) {
            return Err(ParseErrorKind::Unsafe);
        }
        if let Some(class) = cursor.0.strip_prefix("class ") {
            cursor.0 = class;
            read_class(&mut cursor, diagram)?;
        } else {
            let line_number = line_number as u32;
            let range = SourceRange::new(line_number, line_number).expect("bounded source line");
            read_node_chain(&mut cursor, diagram, declared, range)?;
        }
        cursor.trim();
        if cursor.0.is_empty() || cursor.0.starts_with("%%") && !cursor.0.starts_with("%%{") {
            return Ok(());
        }
        if !cursor.take(";") {
            return Err(if unsafe_statement(cursor.0) {
                ParseErrorKind::Unsafe
            } else {
                ParseErrorKind::Unsupported
            });
        }
    }
    Ok(())
}

fn read_node_chain(
    cursor: &mut Cursor<'_>,
    diagram: &mut Diagram,
    declared: &mut HashSet<String>,
    range: SourceRange,
) -> Result<(), ParseErrorKind> {
    let mut id = read_node(cursor, diagram, declared, range)?;
    while let Some((style, arrowheads)) = cursor.take_connection() {
        let label = if cursor.take("|") {
            decode_label(cursor.until("|")?)
        } else {
            String::new()
        };
        let target = read_node(cursor, diagram, declared, range)?;
        diagram.add_edge(
            Edge::new(id, target, label)?
                .with_style(style)
                .with_arrowheads(arrowheads)
                .with_source_range(range),
        )?;
        id = target;
    }
    Ok(())
}

fn read_class(cursor: &mut Cursor<'_>, diagram: &mut Diagram) -> Result<(), ParseErrorKind> {
    let mut ids = vec![cursor.id()?];
    while cursor.take(",") {
        ids.push(cursor.id()?);
    }
    let name = cursor.id()?;
    let tone = match name {
        "setup" => NodeTone::Active,
        "work" => NodeTone::Neutral,
        "success" => NodeTone::Success,
        "failure" => NodeTone::Failure,
        _ => FLOWCHART_TONES
            .iter()
            .find_map(|&(class, tone)| (name == class).then_some(tone))
            .ok_or(ParseErrorKind::Unsupported)?,
    };
    for id in ids {
        let index = diagram
            .node_index(id)
            .ok_or_else(|| DiagramError::UnknownNode(id.to_owned()))?;
        diagram.nodes[index].set_tone(tone);
    }
    Ok(())
}

fn read_node<'a>(
    cursor: &mut Cursor<'a>,
    diagram: &mut Diagram,
    declared: &mut HashSet<String>,
    range: SourceRange,
) -> Result<&'a str, ParseErrorKind> {
    let id = cursor.id()?;
    let shape = if cursor.take("([") {
        Some((NodeShape::Terminal, "])"))
    } else if cursor.take("[") {
        Some((NodeShape::Action, "]"))
    } else if cursor.take("{") {
        Some((NodeShape::Decision, "}"))
    } else if cursor.take("(") {
        Some((NodeShape::Terminal, ")"))
    } else {
        None
    };
    if let Some((shape, delimiter)) = shape {
        let label = if cursor.take("\"") {
            let label = cursor.until("\"")?;
            if !cursor.take(delimiter) {
                return Err(ParseErrorKind::Syntax);
            }
            label
        } else {
            cursor.until(delimiter)?
        };
        let text = label
            .split("<br/>")
            .map(decode_label)
            .collect::<Vec<_>>()
            .join("\n");
        let node = Node::new(id, text, shape)?.with_source_range(range);
        if let Some(index) = diagram.node_index(id) {
            let existing = &diagram.nodes[index];
            if declared.contains(id) {
                if existing.label() != node.label() || existing.shape() != node.shape() {
                    return Err(DiagramError::DuplicateNode(id.to_owned()).into());
                }
            } else {
                diagram.nodes[index] = node.with_tone(existing.tone());
            }
        } else {
            diagram.add_node(node)?;
        }
        declared.insert(id.to_owned());
    } else if diagram.node_index(id).is_none() {
        diagram.add_node(Node::new(id, id, NodeShape::Action)?.with_source_range(range))?;
    }
    Ok(id)
}

struct Cursor<'a>(&'a str);

impl<'a> Cursor<'a> {
    fn trim(&mut self) {
        self.0 = self.0.trim_start();
    }

    fn take(&mut self, token: &str) -> bool {
        self.trim();
        let Some(rest) = self.0.strip_prefix(token) else {
            return false;
        };
        self.0 = rest;
        true
    }

    fn take_connection(&mut self) -> Option<(EdgeStyle, Arrowheads)> {
        [
            ("<-->", EdgeStyle::Solid, Arrowheads::Both),
            ("<-.->", EdgeStyle::Dashed, Arrowheads::Both),
            ("-->", EdgeStyle::Solid, Arrowheads::End),
            ("-.->", EdgeStyle::Dashed, Arrowheads::End),
            ("---", EdgeStyle::Solid, Arrowheads::None),
            ("-.-", EdgeStyle::Dashed, Arrowheads::None),
        ]
        .into_iter()
        .find_map(|(token, style, arrows)| self.take(token).then_some((style, arrows)))
    }

    fn until(&mut self, delimiter: &str) -> Result<&'a str, ParseErrorKind> {
        let (value, rest) = self.0.split_once(delimiter).ok_or(ParseErrorKind::Syntax)?;
        self.0 = rest;
        Ok(value)
    }

    fn id(&mut self) -> Result<&'a str, ParseErrorKind> {
        self.trim();
        let end = self
            .0
            .char_indices()
            .find(|&(index, character)| {
                !character.is_ascii_alphanumeric()
                    && character != '_'
                    && (character != '-'
                        || self.0[index..].starts_with("-->")
                        || self.0[index..].starts_with("-.")
                        || self.0[index..].starts_with("---"))
            })
            .map_or(self.0.len(), |(index, _)| index);
        if end == 0 {
            return Err(ParseErrorKind::Syntax);
        }
        let (id, rest) = self.0.split_at(end);
        self.0 = rest;
        Ok(id)
    }
}

fn unsafe_statement(statement: &str) -> bool {
    if statement.starts_with("%%{") || statement.starts_with("---") {
        return true;
    }
    let keyword = statement.split_whitespace().next().unwrap_or_default();
    statement.contains(char::is_whitespace)
        && ["click", "classDef", "style", "linkStyle", "href"]
            .iter()
            .any(|word| keyword.eq_ignore_ascii_case(word))
}

pub(crate) fn plain_label(source: &str) -> Result<String, ParseErrorKind> {
    // A bare semicolon separates Mermaid statements. Labels can carry it as
    // a numeric escape, which decode_label resolves after this boundary check.
    for (index, _) in source.match_indices(';') {
        let before = &source[..index];
        if before.rsplit_once('#').is_none_or(|(_, digits)| {
            digits.is_empty()
                || digits.len() > 8
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err(ParseErrorKind::Unsupported);
        }
    }
    let text = decode_label(source);
    validate_label(&text)?;
    Ok(text)
}

pub(crate) fn decode_label(label: &str) -> String {
    let mut text = String::with_capacity(label.len());
    let mut rest = label;
    while let Some((prefix, tail)) = rest.split_once('#') {
        text.push_str(prefix);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).take(8).count();
        if digits > 0
            && let Some(next) = tail[digits..].strip_prefix(';')
            && let Ok(code) = tail[..digits].parse::<u32>()
            && let Some(character) = char::from_u32(code)
        {
            text.push(character);
            rest = next;
        } else {
            text.push('#');
            rest = tail;
        }
    }
    text.push_str(rest);
    text
}

impl Diagram {
    /// Serializes the diagram to Mermaid source for export. Mindmaps and
    /// architecture diagrams are exported as flowcharts. Source ranges remain
    /// application metadata.
    pub fn to_mermaid(&self) -> String {
        if let Some(sequence) = &self.sequence {
            return sequence.to_mermaid(self);
        }
        if self.kind() == DiagramKind::State {
            return state::to_mermaid(self);
        }
        if matches!(
            self.kind(),
            DiagramKind::Class | DiagramKind::EntityRelationship
        ) {
            return relations::to_mermaid(self);
        }
        self.flowchart_source(None)
    }

    /// Serializes a generated flowchart with a fixed palette for external Mermaid renderers.
    /// The emitted presentation rules are application-owned.
    pub fn to_styled_mermaid(&self, theme: Theme) -> String {
        if self.kind() == DiagramKind::Flowchart {
            self.flowchart_source(Some(theme))
        } else {
            self.to_mermaid()
        }
    }

    fn flowchart_source(&self, theme: Option<Theme>) -> String {
        let mut output = String::from(match self.direction() {
            Direction::TopToBottom => "flowchart TD\n",
            Direction::LeftToRight => "flowchart LR\n",
        });
        // Exporting stable, generated tokens avoids turning diagram ids into
        // Mermaid keywords such as `end`.
        let mut ids = std::collections::HashMap::new();
        for (index, node) in self.nodes().iter().enumerate() {
            ids.insert(node.id(), index);
            let label = if theme.is_some() {
                node.label()
                    .split('\n')
                    .map(encode_label)
                    .collect::<Vec<_>>()
                    .join("<br/>")
            } else {
                encode_label(node.label())
            };
            let _ = match node.shape() {
                NodeShape::Action | NodeShape::Compartment => {
                    writeln!(output, "    n{index}[\"{label}\"]")
                }
                NodeShape::Decision => writeln!(output, "    n{index}{{\"{label}\"}}"),
                NodeShape::Terminal | NodeShape::StateStart | NodeShape::StateEnd => {
                    writeln!(output, "    n{index}([\"{label}\"])")
                }
            };
        }
        for edge in self.edges() {
            let mut source = ids[edge.source()];
            let mut target = ids[edge.target()];
            if edge.arrowheads() == Arrowheads::Start {
                std::mem::swap(&mut source, &mut target);
            }
            let arrow = match (edge.style(), edge.arrowheads()) {
                (EdgeStyle::Solid, Arrowheads::None) => "---",
                (EdgeStyle::Solid, Arrowheads::Both) => "<-->",
                (EdgeStyle::Solid, _) => "-->",
                (EdgeStyle::Dashed, Arrowheads::None) => "-.-",
                (EdgeStyle::Dashed, Arrowheads::Both) => "<-.->",
                (EdgeStyle::Dashed, _) => "-.->",
            };
            let _ = write!(output, "    n{source} {arrow}");
            if !edge.label().is_empty() {
                let _ = write!(output, "|{}|", encode_label(edge.label()));
            }
            let _ = writeln!(output, " n{target}");
        }
        if let Some(theme) = theme {
            write_flowchart_styles(&mut output, self.nodes(), theme);
        }
        output
    }
}

const FLOWCHART_TONES: [(&str, NodeTone); 7] = [
    ("jobNeutral", NodeTone::Neutral),
    ("jobActive", NodeTone::Active),
    ("jobSuccess", NodeTone::Success),
    ("jobFailure", NodeTone::Failure),
    ("jobBlocked", NodeTone::Blocked),
    ("jobCancelled", NodeTone::Cancelled),
    ("jobDisabled", NodeTone::Disabled),
];

fn flowchart_class_definition(name: &str, tone: NodeTone, theme: Theme) -> String {
    let (fill, stroke) = theme.node_colors(tone);
    let text = theme.text_color();
    let width = if tone == NodeTone::Active { 3 } else { 2 };
    let dash = if tone == NodeTone::Disabled {
        ",stroke-dasharray:4 3"
    } else {
        ""
    };
    format!("classDef {name} fill:{fill},stroke:{stroke},color:{text},stroke-width:{width}px{dash}")
}

fn write_flowchart_styles(output: &mut String, nodes: &[Node], theme: Theme) {
    for (name, tone) in FLOWCHART_TONES {
        let mut matches = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.tone() == tone);
        let Some((first, _)) = matches.next() else {
            continue;
        };
        let style = flowchart_class_definition(name, tone, theme);
        let _ = write!(output, "    {style}\n    class n{first}");
        for (index, _) in matches {
            let _ = write!(output, ",n{index}");
        }
        let _ = writeln!(output, " {name}");
    }
}

pub(crate) fn encode_label(label: &str) -> String {
    let mut encoded = String::new();
    let trimmed_start = label.len() - label.trim_start_matches(' ').len();
    let trimmed_end = label.trim_end_matches(' ').len();
    for (index, character) in label.char_indices() {
        match character {
            ' ' if index < trimmed_start || index >= trimmed_end => encoded.push_str("#32;"),
            '\n' | '#' | '"' | '<' | '>' | '&' | '|' | '[' | ']' | '{' | '}' | '(' | ')' | ';'
            | '\x60' | '\\' | '%' => {
                let _ = write!(encoded, "#{};", u32::from(character));
            }
            _ => encoded.push(character),
        }
    }
    encoded
}
