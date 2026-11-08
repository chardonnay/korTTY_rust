use std::{collections::HashSet, fmt::Write};

use crate::{
    Arrowheads, Diagram, DiagramError, Edge, EdgeStyle, EndMarker, Node, NodeShape, NodeTone,
    ParseErrorKind, Point, Rect, Scene, SceneEdge, SceneNode, SourceRange,
    layout::{Decoration, LINE_HEIGHT, label_size, wrap},
    mermaid::{encode_label, plain_label},
};

const MAX_PARTICIPANTS: usize = 12;
const MAX_MESSAGES: usize = 60;
const MAX_EVENTS: usize = 160;
const MAX_FRAME_DEPTH: usize = 8;
const MARGIN: i32 = 20;
const SIDE_GUTTER: i32 = 320;
const PARTICIPANT_STEP: i32 = 220;
const PARTICIPANT_WIDTH: i32 = 170;
const FRAGMENT_INSET: i32 = 9;
const LABEL_WIDTH: i32 = 190;

#[derive(Default)]
pub(crate) struct SequenceParser {
    sequence: Sequence,
    explicit: HashSet<usize>,
    open: Vec<(Fragment, usize)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Sequence {
    events: Vec<Event>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Event {
    Message {
        edge: usize,
        arrow: Arrow,
        line: usize,
    },
    Note {
        position: NotePosition,
        participant: usize,
        last: Option<usize>,
        text: String,
        line: usize,
    },
    Start {
        kind: Fragment,
        text: String,
        line: usize,
    },
    Branch {
        kind: Fragment,
        text: String,
        line: usize,
    },
    End,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NotePosition {
    Left,
    Right,
    Over,
}

impl NotePosition {
    fn syntax(self) -> &'static str {
        match self {
            Self::Left => "left of",
            Self::Right => "right of",
            Self::Over => "over",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fragment {
    Alternative,
    Optional,
    Loop,
    Parallel,
}

impl Fragment {
    fn syntax(self) -> &'static str {
        match self {
            Self::Alternative => "alt",
            Self::Optional => "opt",
            Self::Loop => "loop",
            Self::Parallel => "par",
        }
    }

    fn branch(self) -> &'static str {
        match self {
            Self::Parallel => "and",
            _ => "else",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Arrow {
    syntax: &'static str,
    style: EdgeStyle,
    arrows: Arrowheads,
    marker: EndMarker,
}

const ARROWS: [Arrow; 8] = [
    Arrow {
        syntax: "-->>",
        style: EdgeStyle::Dashed,
        arrows: Arrowheads::End,
        marker: EndMarker::Filled,
    },
    Arrow {
        syntax: "->>",
        style: EdgeStyle::Solid,
        arrows: Arrowheads::End,
        marker: EndMarker::Filled,
    },
    Arrow {
        syntax: "-->",
        style: EdgeStyle::Dashed,
        arrows: Arrowheads::None,
        marker: EndMarker::Filled,
    },
    Arrow {
        syntax: "->",
        style: EdgeStyle::Solid,
        arrows: Arrowheads::None,
        marker: EndMarker::Filled,
    },
    Arrow {
        syntax: "--x",
        style: EdgeStyle::Dashed,
        arrows: Arrowheads::End,
        marker: EndMarker::Cross,
    },
    Arrow {
        syntax: "-x",
        style: EdgeStyle::Solid,
        arrows: Arrowheads::End,
        marker: EndMarker::Cross,
    },
    Arrow {
        syntax: "--)",
        style: EdgeStyle::Dashed,
        arrows: Arrowheads::End,
        marker: EndMarker::Open,
    },
    Arrow {
        syntax: "-)",
        style: EdgeStyle::Solid,
        arrows: Arrowheads::End,
        marker: EndMarker::Open,
    },
];

impl SequenceParser {
    pub(crate) fn read(
        &mut self,
        source: &str,
        line: usize,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let (word, tail) = keyword(source);
        if word.eq_ignore_ascii_case("note") {
            return self.read_note(tail, line, diagram);
        }
        match word {
            "participant" | "actor" => self.read_declaration(tail, word == "actor", line, diagram),
            "link" | "links" | "properties" | "details" => Err(ParseErrorKind::Unsafe),
            "alt" => self.open_frame(Fragment::Alternative, tail, line),
            "opt" => self.open_frame(Fragment::Optional, tail, line),
            "loop" => self.open_frame(Fragment::Loop, tail, line),
            "par" => self.open_frame(Fragment::Parallel, tail, line),
            "else" => self.add_branch(Fragment::Alternative, tail, line),
            "and" => self.add_branch(Fragment::Parallel, tail, line),
            "end" if tail.is_empty() => {
                self.open.pop().ok_or(ParseErrorKind::Syntax)?;
                self.push(Event::End)
            }
            _ => self.read_message(source, line, diagram),
        }
    }

    pub(crate) fn finish(self) -> Result<Sequence, (Option<usize>, ParseErrorKind)> {
        if let Some((_, line)) = self.open.first() {
            return Err((Some(*line), ParseErrorKind::Syntax));
        }
        if !self
            .sequence
            .events
            .iter()
            .any(|event| matches!(event, Event::Message { .. }))
        {
            return Err((None, ParseErrorKind::Syntax));
        }
        Ok(self.sequence)
    }

    fn push(&mut self, event: Event) -> Result<(), ParseErrorKind> {
        if self.sequence.events.len() == MAX_EVENTS {
            return Err(ParseErrorKind::Limit("160 sequence events"));
        }
        self.sequence.events.push(event);
        Ok(())
    }

    fn participant(
        &self,
        diagram: &mut Diagram,
        id: &str,
        line: usize,
    ) -> Result<usize, ParseErrorKind> {
        if let Some(index) = diagram.node_index(id) {
            return Ok(index);
        }
        let index = diagram.nodes().len();
        if index == MAX_PARTICIPANTS {
            return Err(ParseErrorKind::Limit("12 sequence participants"));
        }
        let node = Node::new(id, id, NodeShape::Action)?;
        diagram.add_node(with_line(node, line))?;
        Ok(index)
    }

    fn read_declaration(
        &mut self,
        tail: &str,
        actor: bool,
        line: usize,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let (id, rest) = take_id(tail)?;
        let rest = rest.trim();
        let label = if rest.is_empty() {
            id.to_owned()
        } else {
            let (alias, text) = keyword(rest);
            if alias != "as" || text.is_empty() {
                return Err(ParseErrorKind::Unsupported);
            }
            plain_label(text)?
        };
        let shape = if actor {
            NodeShape::Terminal
        } else {
            NodeShape::Action
        };
        let index = self.participant(diagram, id, line)?;
        if self.explicit.insert(index) {
            diagram.nodes[index] = with_line(Node::new(id, label, shape)?, line);
        } else if diagram.nodes[index].shape() != shape || diagram.nodes[index].label() != label {
            return Err(DiagramError::DuplicateNode(id.to_owned()).into());
        }
        Ok(())
    }

    fn read_message(
        &mut self,
        source: &str,
        line: usize,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let (head, text) = source.split_once(':').ok_or(ParseErrorKind::Unsupported)?;
        let (from, rest) = take_id(head)?;
        let rest = rest.trim_start();
        let (arrow, rest) = ARROWS
            .iter()
            .find_map(|arrow| rest.strip_prefix(arrow.syntax).map(|rest| (*arrow, rest)))
            .ok_or(ParseErrorKind::Unsupported)?;
        let (to, remaining) = take_id(rest.trim_start())?;
        if !remaining.trim().is_empty() || text.trim().is_empty() {
            return Err(ParseErrorKind::Syntax);
        }
        let edge = diagram.edges().len();
        if edge == MAX_MESSAGES {
            return Err(ParseErrorKind::Limit("60 sequence messages"));
        }
        self.participant(diagram, from, line)?;
        self.participant(diagram, to, line)?;
        diagram.add_edge(
            Edge::new(from, to, plain_label(text.trim())?)?
                .with_style(arrow.style)
                .with_arrowheads(arrow.arrows),
        )?;
        self.push(Event::Message { edge, arrow, line })
    }

    fn read_note(
        &mut self,
        tail: &str,
        line: usize,
        diagram: &mut Diagram,
    ) -> Result<(), ParseErrorKind> {
        let (head, text) = tail.split_once(':').ok_or(ParseErrorKind::Syntax)?;
        let (direction, mut targets) = keyword(head);
        let position = match direction.to_ascii_lowercase().as_str() {
            "over" => NotePosition::Over,
            "left" | "right" => {
                let (of, rest) = keyword(targets);
                if !of.eq_ignore_ascii_case("of") {
                    return Err(ParseErrorKind::Syntax);
                }
                targets = rest;
                if direction.eq_ignore_ascii_case("left") {
                    NotePosition::Left
                } else {
                    NotePosition::Right
                }
            }
            _ => return Err(ParseErrorKind::Unsupported),
        };
        let (id, remaining) = take_id(targets)?;
        let rest = remaining.trim();
        let last = if rest.is_empty() {
            None
        } else if position == NotePosition::Over
            && let Some(last) = rest.strip_prefix(',')
        {
            let (last, rest) = take_id(last.trim())?;
            if !rest.is_empty() {
                return Err(ParseErrorKind::Syntax);
            }
            Some(last)
        } else {
            return Err(ParseErrorKind::Syntax);
        };
        if text.trim().is_empty() {
            return Err(ParseErrorKind::Syntax);
        }
        let participant = self.participant(diagram, id, line)?;
        let last = last
            .map(|id| self.participant(diagram, id, line))
            .transpose()?;
        self.push(Event::Note {
            position,
            participant,
            last,
            text: plain_label(text.trim())?,
            line,
        })
    }

    fn open_frame(
        &mut self,
        kind: Fragment,
        text: &str,
        line: usize,
    ) -> Result<(), ParseErrorKind> {
        if self.open.len() == MAX_FRAME_DEPTH {
            return Err(ParseErrorKind::Limit("8 nested sequence frames"));
        }
        self.open.push((kind, line));
        self.push(Event::Start {
            kind,
            text: plain_label(text)?,
            line,
        })
    }

    fn add_branch(
        &mut self,
        kind: Fragment,
        text: &str,
        line: usize,
    ) -> Result<(), ParseErrorKind> {
        if self.open.last().is_none_or(|(parent, _)| *parent != kind) {
            return Err(ParseErrorKind::Syntax);
        }
        self.push(Event::Branch {
            kind,
            text: plain_label(text)?,
            line,
        })
    }
}

fn keyword(source: &str) -> (&str, &str) {
    source
        .split_once(char::is_whitespace)
        .map_or((source, ""), |(word, tail)| (word, tail.trim()))
}

fn take_id(source: &str) -> Result<(&str, &str), ParseErrorKind> {
    let end = source
        .char_indices()
        .find(|&(index, character)| {
            !character.is_ascii_alphanumeric()
                && character != '_'
                && (character != '-'
                    || ARROWS
                        .iter()
                        .any(|arrow| source[index..].starts_with(arrow.syntax)))
        })
        .map_or(source.len(), |(index, _)| index);
    let (id, rest) = source.split_at(end);
    if !id
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        || id.eq_ignore_ascii_case("end")
    {
        return Err(ParseErrorKind::Syntax);
    }
    Ok((id, rest))
}

fn line_range(line: usize) -> Option<SourceRange> {
    // Source length is limited to 32 KiB, so all line numbers fit in u32.
    SourceRange::new(line as u32, line as u32)
}

fn with_line(node: Node, line: usize) -> Node {
    if let Some(range) = line_range(line) {
        node.with_source_range(range)
    } else {
        node
    }
}

impl Sequence {
    pub(crate) fn to_mermaid(&self, diagram: &Diagram) -> String {
        let mut output = String::from("sequenceDiagram\n");
        for (index, node) in diagram.nodes().iter().enumerate() {
            let role = if node.shape() == NodeShape::Terminal {
                "actor"
            } else {
                "participant"
            };
            // External Mermaid may interpret a participant id as a keyword.
            let id = format!("p{index}");
            let _ = write!(output, "    {role} {id}");
            if node.label() != id {
                let _ = write!(output, " as {}", encode_label(node.label()));
            }
            output.push('\n');
        }
        for event in &self.events {
            match event {
                Event::Message { edge, arrow, .. } => {
                    let edge = &diagram.edges()[*edge];
                    let source = diagram
                        .node_index(edge.source())
                        .expect("sequence edge source");
                    let target = diagram
                        .node_index(edge.target())
                        .expect("sequence edge target");
                    let _ = writeln!(
                        output,
                        "    p{source}{}p{target}: {}",
                        arrow.syntax,
                        encode_label(edge.label())
                    );
                }
                Event::Note {
                    position,
                    participant,
                    last,
                    text,
                    ..
                } => {
                    let _ = write!(output, "    Note {} p{participant}", position.syntax());
                    if let Some(last) = last {
                        let _ = write!(output, ",p{last}");
                    }
                    let _ = writeln!(output, ": {}", encode_label(text));
                }
                Event::Start { kind, text, .. } => write_fragment(&mut output, kind.syntax(), text),
                Event::Branch { kind, text, .. } => {
                    write_fragment(&mut output, kind.branch(), text)
                }
                Event::End => output.push_str("    end\n"),
            }
        }
        output
    }

    pub(crate) fn layout(&self, diagram: &Diagram) -> Scene {
        let participant_count = diagram.nodes().len();
        let width = SIDE_GUTTER + participant_x(participant_count.saturating_sub(1));
        let labels: Vec<_> = diagram
            .nodes()
            .iter()
            .map(|node| wrap(node.label(), PARTICIPANT_WIDTH - 24))
            .collect();
        let header_height = labels
            .iter()
            .map(|lines| lines.len() as i32 * LINE_HEIGHT + 22)
            .max()
            .unwrap_or(0)
            .max(48);
        let mut nodes: Vec<_> = diagram
            .nodes()
            .iter()
            .zip(labels)
            .enumerate()
            .map(|(index, (node, lines))| SceneNode {
                id: node.id().to_owned(),
                bounds: Rect {
                    x: participant_x(index) - PARTICIPANT_WIDTH / 2,
                    y: MARGIN,
                    width: PARTICIPANT_WIDTH,
                    height: header_height,
                },
                lines,
                shape: node.shape(),
                tone: node.tone(),
                source_range: node.source_range(),
                heading_lines: 0,
                member_hits: Vec::new(),
            })
            .collect();
        let mut edges = Vec::with_capacity(diagram.edges().len());
        let mut decorations = Vec::new();
        let mut open_frames: Vec<Rect> = Vec::new();
        let header_bottom = MARGIN + header_height;
        let mut y = header_bottom + 26;
        for event in &self.events {
            match event {
                Event::Message { edge, arrow, line } => {
                    let edge = &diagram.edges()[*edge];
                    // Validated sequence participants and messages have stable indices.
                    let source = diagram
                        .node_index(edge.source())
                        .map_or(SIDE_GUTTER, participant_x);
                    let target = diagram
                        .node_index(edge.target())
                        .map_or(SIDE_GUTTER, participant_x);
                    let (scene_edge, height) =
                        layout_message(edge, *arrow, *line, source, target, y);
                    edges.push(scene_edge);
                    y += height;
                }
                Event::Note {
                    position,
                    participant,
                    last,
                    text,
                    line,
                } => {
                    let note = layout_note(*position, *participant, *last, text, *line, y);
                    y = note.bounds.bottom() + 12;
                    nodes.push(note);
                }
                Event::Start { kind, text, line } => {
                    let inset = MARGIN + open_frames.len() as i32 * FRAGMENT_INSET;
                    let bounds = Rect {
                        x: inset,
                        y,
                        width: width - inset * 2,
                        height: 0,
                    };
                    y += 4;
                    let (decoration, height) =
                        fragment_label(kind.syntax(), text, *line, bounds.x + 7, y);
                    decorations.push(decoration);
                    y += height + 12;
                    open_frames.push(bounds);
                }
                Event::Branch { kind, text, line } => {
                    if let Some(frame) = open_frames.last() {
                        decorations.push(Decoration::Line {
                            start: Point { x: frame.x, y },
                            end: Point {
                                x: frame.right(),
                                y,
                            },
                        });
                        y += 4;
                        let (decoration, height) =
                            fragment_label(kind.branch(), text, *line, frame.x + 7, y);
                        decorations.push(decoration);
                        y += height + 12;
                    }
                }
                Event::End => {
                    if let Some(mut frame) = open_frames.pop() {
                        frame.height = y - frame.y;
                        decorations.push(Decoration::Frame(frame));
                        y += 12;
                    }
                }
            }
        }
        let lifelines = (0..participant_count).map(|index| {
            let x = participant_x(index);
            Decoration::Line {
                start: Point {
                    x,
                    y: header_bottom,
                },
                end: Point { x, y },
            }
        });
        Scene {
            bounds: Rect {
                x: 0,
                y: 0,
                width,
                height: y + MARGIN,
            },
            nodes,
            edges,
            decorations: lifelines.chain(decorations).collect(),
        }
    }
}

fn write_fragment(output: &mut String, keyword: &str, text: &str) {
    let _ = write!(output, "    {keyword}");
    if !text.is_empty() {
        let _ = write!(output, " {}", encode_label(text));
    }
    output.push('\n');
}

fn participant_x(index: usize) -> i32 {
    SIDE_GUTTER + index as i32 * PARTICIPANT_STEP
}

fn layout_message(
    edge: &Edge,
    arrow: Arrow,
    line: usize,
    source: i32,
    target: i32,
    y: i32,
) -> (SceneEdge, i32) {
    let label_lines = wrap(edge.label(), LABEL_WIDTH);
    let (label_width, label_height) = label_size(&label_lines).unwrap_or((0, 0));
    let is_self = source == target;
    let label_x = if is_self {
        source + 7
    } else {
        (source + target - label_width) / 2
    };
    let arrow_y = y + label_height + 7;
    let mut points = vec![Point {
        x: source,
        y: arrow_y,
    }];
    if is_self {
        points.extend([
            Point {
                x: source + 64,
                y: arrow_y,
            },
            Point {
                x: source + 64,
                y: arrow_y + 22,
            },
            Point {
                x: target,
                y: arrow_y + 22,
            },
        ]);
    } else {
        points.push(Point {
            x: target,
            y: arrow_y,
        });
    }
    let label_bounds = (label_height != 0).then_some(Rect {
        x: label_x,
        y,
        width: label_width,
        height: label_height,
    });
    let scene = SceneEdge {
        source: edge.source().to_owned(),
        target: edge.target().to_owned(),
        points,
        label_bounds,
        label_lines,
        style: arrow.style,
        arrowheads: arrow.arrows,
        end_marker: arrow.marker,
        start_marker: EndMarker::Filled,
        multiplicities: [None, None],
        source_range: line_range(line),
    };
    (scene, label_height + if is_self { 51 } else { 29 })
}

fn layout_note(
    position: NotePosition,
    participant: usize,
    last: Option<usize>,
    text: &str,
    line: usize,
    y: i32,
) -> SceneNode {
    let lines = wrap(text, LABEL_WIDTH);
    let (width, height) = label_size(&lines).unwrap_or((40, 24));
    let width = width.max(60);
    let first_x = participant_x(participant);
    let last_x = last.map_or(first_x, participant_x);
    let x = match position {
        NotePosition::Left => first_x - width - 13,
        NotePosition::Right => first_x + 13,
        NotePosition::Over => (first_x + last_x - width) / 2,
    };
    SceneNode {
        id: format!("note:{line}"),
        bounds: Rect {
            x,
            y,
            width,
            height: height + 12,
        },
        lines,
        shape: NodeShape::Action,
        tone: NodeTone::Blocked,
        source_range: line_range(line),
        heading_lines: 0,
        member_hits: Vec::new(),
    }
}

fn fragment_label(keyword: &str, text: &str, line: usize, x: i32, y: i32) -> (Decoration, i32) {
    let label = if text.is_empty() {
        keyword.to_owned()
    } else {
        format!("{keyword}  {text}")
    };
    let lines = wrap(&label, LABEL_WIDTH * 2);
    let (width, height) = label_size(&lines).unwrap_or((40, 24));
    let bounds = Rect {
        x,
        y,
        width,
        height,
    };
    (
        Decoration::Label {
            bounds,
            lines,
            source_range: line_range(line),
        },
        height,
    )
}
