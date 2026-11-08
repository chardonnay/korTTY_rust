use std::{error::Error, fmt};

/// Maximum number of nodes in a diagram.
pub const MAX_NODES: usize = 256;
/// Maximum number of edges in a diagram.
pub const MAX_EDGES: usize = 300;
/// Maximum number of Unicode scalar values in a label.
pub const MAX_LABEL_CHARS: usize = 160;
const MAX_ID_BYTES: usize = 64;

/// The direction in which forward edges are laid out.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Direction {
    /// The source is above its target.
    #[default]
    TopToBottom,
    /// The source is to the left of its target.
    LeftToRight,
}

/// The semantic family of a diagram.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiagramKind {
    /// A directed workflow or dependency graph.
    #[default]
    Flowchart,
    /// An indented outline represented as a tree.
    Mindmap,
    /// Services and junctions connected by edges.
    Architecture,
    /// An ordered timeline of participant interactions.
    Sequence,
    /// A flat state machine with named states and transitions.
    State,
    /// Types with attributes, operations and their relationships.
    Class,
    /// Data entities with attributes and relationship cardinalities.
    EntityRelationship,
}

/// The semantic shape of a node.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NodeShape {
    /// A plain rectangular step.
    #[default]
    Action,
    /// A branch drawn as a diamond.
    Decision,
    /// A rounded start or end point.
    Terminal,
    /// The initial marker of a state machine.
    StateStart,
    /// The final marker of a state machine.
    StateEnd,
    /// A header and a compartment of members or attributes.
    Compartment,
}

/// A theme-independent visual role for a node.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NodeTone {
    /// The ordinary palette.
    #[default]
    Neutral,
    /// A setup or currently running step.
    Active,
    /// A completed or successful step.
    Success,
    /// A step that failed.
    Failure,
    /// A step awaiting an unmet condition.
    Blocked,
    /// A cancelled step.
    Cancelled,
    /// A disabled step, drawn with a dashed border.
    Disabled,
}

/// Whether an edge is a direct transition or a condition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EdgeStyle {
    /// A solid line.
    #[default]
    Solid,
    /// A dashed line.
    Dashed,
}

/// Which endpoints of an edge receive an arrowhead.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Arrowheads {
    /// A connection without arrows.
    None,
    /// An arrow points at the source.
    Start,
    /// An arrow points at the target.
    #[default]
    End,
    /// Arrows point at both endpoints.
    Both,
}

/// An inclusive range of one-based source lines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRange {
    start: u32,
    end: u32,
}

impl SourceRange {
    /// Constructs a range, returning `None` if it is zero-based or reversed.
    pub fn new(start: u32, end: u32) -> Option<Self> {
        (start > 0 && start <= end).then_some(Self { start, end })
    }

    /// Returns the first line.
    pub fn start(self) -> u32 {
        self.start
    }

    /// Returns the last line.
    pub fn end(self) -> u32 {
        self.end
    }
}

/// A step or state with a stable identifier and an optional source target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    id: String,
    label: String,
    shape: NodeShape,
    tone: NodeTone,
    source_range: Option<SourceRange>,
    pub(crate) members: Vec<Member>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Member {
    pub(crate) text: String,
    pub(crate) source_range: Option<SourceRange>,
}

impl Node {
    /// Constructs a node.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid identifier or unsupported label text.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        shape: NodeShape,
    ) -> Result<Self, DiagramError> {
        let id = id.into();
        let label = label.into();
        validate_id(&id)?;
        validate_label(&label)?;
        Ok(Self {
            id,
            label,
            shape,
            tone: NodeTone::Neutral,
            source_range: None,
            members: Vec::new(),
        })
    }

    /// Assigns a visual role.
    #[must_use]
    pub fn with_tone(mut self, tone: NodeTone) -> Self {
        self.tone = tone;
        self
    }

    pub(crate) fn set_tone(&mut self, tone: NodeTone) {
        self.tone = tone;
    }

    /// Assigns a source navigation target.
    #[must_use]
    pub fn with_source_range(mut self, range: SourceRange) -> Self {
        self.source_range = Some(range);
        self
    }

    pub(crate) fn set_source_range(&mut self, range: SourceRange) {
        self.source_range = Some(range);
    }

    /// Returns the stable identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the display label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the shape.
    pub fn shape(&self) -> NodeShape {
        self.shape
    }

    /// Returns the visual role.
    pub fn tone(&self) -> NodeTone {
        self.tone
    }

    /// Returns the source navigation target.
    pub fn source_range(&self) -> Option<SourceRange> {
        self.source_range
    }

    /// Returns the class members or entity attributes in source order.
    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(|member| member.text.as_str())
    }
}

/// A directed, optionally labeled connection between nodes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edge {
    source: String,
    target: String,
    label: String,
    style: EdgeStyle,
    arrowheads: Arrowheads,
    source_range: Option<SourceRange>,
    pub(crate) relation: Option<Relation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Relation {
    pub(crate) operator: String,
    pub(crate) markers: [crate::EndMarker; 2],
    pub(crate) multiplicities: [Option<String>; 2],
}

impl Edge {
    /// Constructs an edge. Pass an empty string for an unlabeled edge.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid identifier or unsupported label text.
    pub fn new(
        source: impl Into<String>,
        target: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<Self, DiagramError> {
        let source = source.into();
        let target = target.into();
        let label = label.into();
        validate_id(&source)?;
        validate_id(&target)?;
        validate_label(&label)?;
        Ok(Self {
            source,
            target,
            label,
            style: EdgeStyle::Solid,
            arrowheads: Arrowheads::End,
            source_range: None,
            relation: None,
        })
    }

    /// Assigns the edge's line style.
    #[must_use]
    pub fn with_style(mut self, style: EdgeStyle) -> Self {
        self.style = style;
        self
    }

    /// Sets which endpoints receive arrowheads.
    #[must_use]
    pub fn with_arrowheads(mut self, arrowheads: Arrowheads) -> Self {
        self.arrowheads = arrowheads;
        self
    }

    /// Assigns original source lines for this edge.
    #[must_use]
    pub fn with_source_range(mut self, range: SourceRange) -> Self {
        self.source_range = Some(range);
        self
    }

    /// Returns the original source lines when available.
    pub fn source_range(&self) -> Option<SourceRange> {
        self.source_range
    }

    /// Returns a UML multiplicity label for the source or target, if present.
    pub fn multiplicities(&self) -> [Option<&str>; 2] {
        self.relation.as_ref().map_or([None, None], |relation| {
            [
                relation.multiplicities[0].as_deref(),
                relation.multiplicities[1].as_deref(),
            ]
        })
    }

    /// Returns which endpoints receive arrowheads.
    pub fn arrowheads(&self) -> Arrowheads {
        self.arrowheads
    }

    /// Returns the originating node identifier.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the destination node identifier.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the label, which can be empty.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the edge's line style.
    pub fn style(&self) -> EdgeStyle {
        self.style
    }
}

/// A bounded graph with an insertion order stable across layouts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diagram {
    kind: DiagramKind,
    direction: Direction,
    pub(crate) nodes: Vec<Node>,
    edges: Vec<Edge>,
    pub(crate) sequence: Option<crate::sequence::Sequence>,
}

impl Diagram {
    /// Creates an empty diagram with the given orientation.
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            ..Self::default()
        }
    }

    pub(crate) fn of_kind(kind: DiagramKind, direction: Direction) -> Self {
        Self {
            kind,
            ..Self::new(direction)
        }
    }

    /// Returns the diagram's family.
    pub fn kind(&self) -> DiagramKind {
        self.kind
    }

    /// Sets graph direction. Sequence diagrams keep their timeline orientation.
    pub fn set_direction(&mut self, direction: Direction) {
        if self.sequence.is_none() {
            self.direction = direction;
        }
    }

    /// Returns the direction.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Adds a node.
    ///
    /// # Errors
    ///
    /// Returns an error if the identifier exists or the graph is full.
    pub fn add_node(&mut self, node: Node) -> Result<(), DiagramError> {
        if self.sequence.is_some() {
            return Err(DiagramError::FixedTimeline);
        }
        if self.nodes.len() == MAX_NODES {
            return Err(DiagramError::TooManyNodes);
        }
        if self.node_index(node.id()).is_some() {
            return Err(DiagramError::DuplicateNode(node.id));
        }
        self.nodes.push(node);
        Ok(())
    }

    /// Adds an edge between two existing nodes. Cycles and self-loops are allowed.
    ///
    /// # Errors
    ///
    /// Returns an error if an endpoint is absent or the graph is full.
    pub fn add_edge(&mut self, edge: Edge) -> Result<(), DiagramError> {
        if self.sequence.is_some() {
            return Err(DiagramError::FixedTimeline);
        }
        if matches!(
            self.kind,
            DiagramKind::Class | DiagramKind::EntityRelationship
        ) && edge.relation.is_none()
        {
            return Err(DiagramError::RelationshipRequired);
        }
        if self.edges.len() == MAX_EDGES {
            return Err(DiagramError::TooManyEdges);
        }
        for id in [edge.source(), edge.target()] {
            if self.node_index(id).is_none() {
                return Err(DiagramError::UnknownNode(id.to_owned()));
            }
        }
        self.edges.push(edge);
        Ok(())
    }

    /// Returns all nodes in insertion order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Returns all edges in insertion order.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub(crate) fn node_index(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|node| node.id() == id)
    }
}

/// Invalid or oversized graph data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagramError {
    /// An identifier is empty, oversized, or contains unsupported characters.
    InvalidId,
    /// A label exceeds [`MAX_LABEL_CHARS`].
    LabelTooLong,
    /// A label contains a control or direction-override character.
    InvalidLabel,
    /// Another node already uses the identifier.
    DuplicateNode(String),
    /// An edge refers to a missing node.
    UnknownNode(String),
    /// The graph reached [`MAX_NODES`].
    TooManyNodes,
    /// The graph reached [`MAX_EDGES`].
    TooManyEdges,
    /// A parsed sequence timeline cannot be changed through graph operations.
    FixedTimeline,
    /// A generic edge has no UML or ER relationship data.
    RelationshipRequired,
}

impl fmt::Display for DiagramError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => write!(formatter, "invalid diagram node identifier"),
            Self::LabelTooLong => write!(
                formatter,
                "diagram label exceeds {MAX_LABEL_CHARS} characters"
            ),
            Self::InvalidLabel => {
                write!(formatter, "diagram label contains a disallowed character")
            }
            Self::DuplicateNode(id) => write!(formatter, "diagram node {id} is already defined"),
            Self::UnknownNode(id) => write!(formatter, "diagram node {id} is not defined"),
            Self::TooManyNodes => write!(formatter, "diagram exceeds {MAX_NODES} nodes"),
            Self::TooManyEdges => write!(formatter, "diagram exceeds {MAX_EDGES} edges"),
            Self::FixedTimeline => {
                write!(formatter, "edit the sequence timeline through its source")
            }
            Self::RelationshipRequired => {
                write!(formatter, "edit relationships through the diagram source")
            }
        }
    }
}

impl Error for DiagramError {}

fn validate_id(id: &str) -> Result<(), DiagramError> {
    let first = id.as_bytes().first();
    if !first.is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        || id.len() > MAX_ID_BYTES
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(DiagramError::InvalidId);
    }
    Ok(())
}

pub(crate) fn validate_label(label: &str) -> Result<(), DiagramError> {
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(DiagramError::LabelTooLong);
    }
    if label.chars().any(|character| {
        (character.is_control() && character != '\n')
            || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{fffe}' | '\u{ffff}')
    }) {
        return Err(DiagramError::InvalidLabel);
    }
    Ok(())
}
