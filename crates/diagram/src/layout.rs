use std::collections::{HashMap, HashSet};

use crate::{Arrowheads, Diagram, Direction, EdgeStyle, Node, NodeShape, NodeTone, SourceRange};

const MARGIN: i32 = 24;
const COLUMN_GAP: i32 = 48;
const MIN_RANK_GAP: i32 = 92;
const OUTER_LANE_STEP: i32 = 20;
pub(crate) const LINE_HEIGHT: i32 = 19;
pub(crate) const CARD_HEADER_PAD: i32 = 8;
pub(crate) const CARD_DIVIDER_PAD: i32 = 4;
pub(crate) const CARD_BODY_PAD: i32 = 6;
const LABEL_WIDTH: i32 = 210;

/// An integer point in scene coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
    /// Horizontal coordinate from the scene's left side.
    pub x: i32,
    /// Vertical coordinate from the scene's top side.
    pub y: i32,
}

/// An axis-aligned rectangle in scene coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    /// Horizontal coordinate of the left edge.
    pub x: i32,
    /// Vertical coordinate of the top edge.
    pub y: i32,
    /// Horizontal extent in pixels at one-to-one scale.
    pub width: i32,
    /// Vertical extent in pixels at one-to-one scale.
    pub height: i32,
}

impl Rect {
    /// Returns whether a point is within the bounds, including the edges.
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.right()
            && point.y >= self.y
            && point.y <= self.bottom()
    }

    pub(crate) fn right(self) -> i32 {
        self.x.saturating_add(self.width)
    }

    pub(crate) fn bottom(self) -> i32 {
        self.y.saturating_add(self.height)
    }

    pub(crate) fn center(self) -> Point {
        Point {
            x: self.x + self.width / 2,
            y: self.y + self.height / 2,
        }
    }
}

/// A positioned node and its interaction metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneNode {
    pub(crate) id: String,
    pub(crate) bounds: Rect,
    pub(crate) lines: Vec<String>,
    pub(crate) shape: NodeShape,
    pub(crate) tone: NodeTone,
    pub(crate) source_range: Option<SourceRange>,
    pub(crate) heading_lines: usize,
    pub(crate) member_hits: Vec<(Rect, SourceRange)>,
}

impl SceneNode {
    /// Returns the stable node identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the node's outer bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Returns the pre-wrapped label lines for painting.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Returns the node's shape.
    pub fn shape(&self) -> NodeShape {
        self.shape
    }

    /// Returns the node's semantic palette role.
    pub fn tone(&self) -> NodeTone {
        self.tone
    }

    /// Returns the source lines targeted by the node.
    pub fn source_range(&self) -> Option<SourceRange> {
        self.source_range
    }

    fn source_at(&self, point: Point) -> Option<SourceRange> {
        self.member_hits
            .iter()
            .find_map(|(bounds, range)| bounds.contains(point).then_some(*range))
            .or(self.source_range)
    }

    fn contains(&self, point: Point) -> bool {
        if !self.bounds.contains(point) {
            return false;
        }
        let center = self.bounds.center();
        if matches!(self.shape, NodeShape::StateStart | NodeShape::StateEnd) {
            let dx = point.x - center.x;
            let dy = point.y - center.y;
            let radius = self.bounds.width / 2;
            return dx * dx + dy * dy <= radius * radius;
        }
        if self.shape != NodeShape::Decision {
            return true;
        }
        let dx = (point.x - center.x).abs() * self.bounds.height;
        let dy = (point.y - center.y).abs() * self.bounds.width;
        (dx + dy) * 2 <= self.bounds.width * self.bounds.height
    }
}

/// A routed arrow, including the geometry of its optional label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneEdge {
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) points: Vec<Point>,
    pub(crate) label_bounds: Option<Rect>,
    pub(crate) label_lines: Vec<String>,
    pub(crate) style: EdgeStyle,
    pub(crate) arrowheads: Arrowheads,
    pub(crate) end_marker: EndMarker,
    pub(crate) start_marker: EndMarker,
    pub(crate) multiplicities: [Option<EndpointLabel>; 2],
    pub(crate) source_range: Option<SourceRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EndpointLabel {
    pub(crate) bounds: Rect,
    pub(crate) lines: Vec<String>,
}

/// Appearance of a marker at one endpoint of an edge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EndMarker {
    /// A filled triangular arrow.
    #[default]
    Filled,
    /// An open arrow for asynchronous messages.
    Open,
    /// A crossed end for a terminated message.
    Cross,
    /// Hollow triangle marking inheritance or realization.
    Inheritance,
    /// Filled diamond marking composition.
    Composition,
    /// Hollow diamond marking aggregation.
    Aggregation,
    /// Crow's-foot marker for exactly one instance.
    ExactlyOne,
    /// Crow's-foot marker for zero or one instance.
    ZeroOrOne,
    /// Crow's-foot marker for one or more instances.
    OneOrMany,
    /// Crow's-foot marker for zero or more instances.
    ZeroOrMany,
}

impl SceneEdge {
    /// Returns the originating node identifier.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the destination node identifier.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the route ordered from the start to the arrow tip.
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// Returns the bounds of the label when one is present.
    pub fn label_bounds(&self) -> Option<Rect> {
        self.label_bounds
    }

    /// Returns the pre-wrapped label lines for painting.
    pub fn label_lines(&self) -> &[String] {
        &self.label_lines
    }

    /// Returns which endpoints receive arrowheads.
    pub fn arrowheads(&self) -> Arrowheads {
        self.arrowheads
    }

    /// Returns the appearance of the target marker.
    pub fn end_marker(&self) -> EndMarker {
        self.end_marker
    }

    /// Returns the appearance of the source marker.
    pub fn start_marker(&self) -> EndMarker {
        self.start_marker
    }

    /// Returns the original source lines when this edge represents a statement.
    pub fn source_range(&self) -> Option<SourceRange> {
        self.source_range
    }

    /// Returns the edge's line style.
    pub fn style(&self) -> EdgeStyle {
        self.style
    }

    fn contains_route(&self, point: Point) -> bool {
        const TOLERANCE: i32 = 4;
        self.points.windows(2).any(|segment| {
            let start = segment[0];
            let end = segment[1];
            if start.x == end.x {
                point.x.abs_diff(start.x) <= TOLERANCE as u32
                    && (start.y.min(end.y)..=start.y.max(end.y)).contains(&point.y)
            } else if start.y == end.y {
                point.y.abs_diff(start.y) <= TOLERANCE as u32
                    && (start.x.min(end.x)..=start.x.max(end.x)).contains(&point.x)
            } else {
                false
            }
        })
    }
}

/// A laid-out diagram with a stable coordinate system for painting and hit testing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scene {
    pub(crate) bounds: Rect,
    pub(crate) nodes: Vec<SceneNode>,
    pub(crate) edges: Vec<SceneEdge>,
    pub(crate) decorations: Vec<Decoration>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Decoration {
    Line {
        start: Point,
        end: Point,
    },
    Frame(Rect),
    Label {
        bounds: Rect,
        lines: Vec<String>,
        source_range: Option<SourceRange>,
    },
}

impl Scene {
    /// Returns the scene bounds, including edge labels and padding.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Returns the positioned nodes.
    pub fn nodes(&self) -> &[SceneNode] {
        &self.nodes
    }

    /// Returns the routed edges.
    pub fn edges(&self) -> &[SceneEdge] {
        &self.edges
    }

    /// Returns the node at the specified point in scene coordinates.
    pub fn node_at(&self, point: Point) -> Option<&SceneNode> {
        self.nodes.iter().find(|node| node.contains(point))
    }

    /// Returns source lines for a node, member, edge or fragment caption at a point.
    pub fn source_at(&self, point: Point) -> Option<SourceRange> {
        self.node_at(point)
            .and_then(|node| node.source_at(point))
            .or_else(|| {
                self.edges.iter().find_map(|edge| {
                    let on_label = edge
                        .label_bounds
                        .is_some_and(|bounds| bounds.contains(point))
                        || edge
                            .multiplicities
                            .iter()
                            .flatten()
                            .any(|label| label.bounds.contains(point));
                    on_label.then_some(edge.source_range).flatten()
                })
            })
            .or_else(|| {
                self.decorations
                    .iter()
                    .find_map(|decoration| match decoration {
                        Decoration::Label {
                            bounds,
                            source_range,
                            ..
                        } if bounds.contains(point) => *source_range,
                        _ => None,
                    })
            })
            .or_else(|| {
                self.edges
                    .iter()
                    .find_map(|edge| edge.source_range.filter(|_| edge.contains_route(point)))
            })
    }
}

impl Diagram {
    /// Lays out the diagram in its configured direction. The layout is deterministic
    /// for the same graph and may be computed on a worker thread.
    pub fn layout(&self) -> Scene {
        if let Some(sequence) = &self.sequence {
            return sequence.layout(self);
        }
        let indices: HashMap<_, _> = self
            .nodes()
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id(), index))
            .collect();
        // Diagram validates every endpoint at insertion and never removes nodes.
        let endpoints: Vec<_> = self
            .edges()
            .iter()
            .map(|edge| (indices[edge.source()], indices[edge.target()]))
            .collect();
        let ranks = ranks(self.nodes().len(), &endpoints);
        let node_labels: Vec<_> = self
            .nodes()
            .iter()
            .map(|node| {
                let width = if node.shape() == NodeShape::Decision {
                    110
                } else {
                    LABEL_WIDTH
                };
                NodeLabels::new(node, width)
            })
            .collect();
        let node_width = 250;
        let node_height = node_labels
            .iter()
            .zip(self.nodes())
            .map(|(labels, node)| {
                let text_height = labels.lines.len() as i32 * LINE_HEIGHT;
                if node.shape() == NodeShape::Decision {
                    text_height * 2 + 32
                } else {
                    text_height + 32
                }
            })
            .max()
            .unwrap_or(0)
            .max(64);
        let (primary_size, cross_size) = match self.direction() {
            Direction::TopToBottom => (node_height, node_width),
            Direction::LeftToRight => (node_width, node_height),
        };
        let edge_labels: Vec<_> = self
            .edges()
            .iter()
            .map(|edge| {
                let lines = wrap(edge.label(), LABEL_WIDTH);
                let size = label_size(&lines);
                (lines, size)
            })
            .collect();
        let rank_gap = edge_labels
            .iter()
            .filter_map(|(_, size)| *size)
            .map(|(width, height)| match self.direction() {
                Direction::TopToBottom => height,
                Direction::LeftToRight => width,
            })
            .max()
            .unwrap_or_default()
            .saturating_add(24)
            .max(MIN_RANK_GAP);
        let mut ranks_counts = vec![0; self.nodes().len()];
        for &rank in &ranks {
            ranks_counts[rank] += 1;
        }
        let widest_rank = ranks_counts.iter().copied().max().unwrap_or_default();
        let mut seen = vec![0; ranks_counts.len()];
        let nodes: Vec<_> = self
            .nodes()
            .iter()
            .zip(&ranks)
            .zip(node_labels)
            .map(|((node, &rank), labels)| {
                let cross_stride = cross_size + COLUMN_GAP;
                let cross = MARGIN
                    + (widest_rank - ranks_counts[rank]) * cross_stride / 2
                    + seen[rank] * cross_stride;
                seen[rank] += 1;
                let origin = project(
                    self.direction(),
                    primary_start(rank, primary_size, rank_gap),
                    cross,
                );
                let bounds = if matches!(node.shape(), NodeShape::StateStart | NodeShape::StateEnd)
                {
                    let size = 24;
                    Rect {
                        x: origin.x + (node_width - size) / 2,
                        y: origin.y + (node_height - size) / 2,
                        width: size,
                        height: size,
                    }
                } else if node.shape() == NodeShape::Compartment {
                    let height = (labels.lines.len() as i32 * LINE_HEIGHT + 28).max(48);
                    Rect {
                        x: origin.x,
                        y: origin.y + (node_height - height) / 2,
                        width: node_width,
                        height,
                    }
                } else {
                    Rect {
                        x: origin.x,
                        y: origin.y,
                        width: node_width,
                        height: node_height,
                    }
                };
                let member_hits = labels.member_hits(bounds);
                SceneNode {
                    id: node.id().to_owned(),
                    bounds,
                    lines: labels.lines,
                    shape: node.shape(),
                    tone: node.tone(),
                    source_range: node.source_range(),
                    heading_lines: labels.heading_lines,
                    member_hits,
                }
            })
            .collect();
        let outer = MARGIN + widest_rank * (cross_size + COLUMN_GAP);
        let mut lane = outer;
        let mut routed = HashSet::new();
        let edges: Vec<_> = self
            .edges()
            .iter()
            .zip(endpoints)
            .zip(edge_labels)
            .map(|((edge, (source, target)), (label_lines, label_size))| {
                let adjacent =
                    ranks[target] == ranks[source] + 1 && routed.insert((source, target));
                let start = forward_port(self.direction(), nodes[source].bounds, true);
                let end = forward_port(self.direction(), nodes[target].bounds, false);
                let source_end =
                    primary_start(ranks[source], primary_size, rank_gap) + primary_size;
                let (points, label_anchor) = if adjacent {
                    route_adjacent(self.direction(), start, end, source_end + rank_gap / 2)
                } else {
                    let cross_label_size =
                        label_size.map_or(0, |(width, height)| match self.direction() {
                            Direction::TopToBottom => width,
                            Direction::LeftToRight => height,
                        });
                    let target_start = primary_start(ranks[target], primary_size, rank_gap);
                    let route = route_outer(
                        self.direction(),
                        start,
                        end,
                        lane,
                        cross_label_size,
                        source_end + MIN_RANK_GAP / 3,
                        target_start - MIN_RANK_GAP / 3,
                    );
                    lane += OUTER_LANE_STEP + cross_label_size;
                    route
                };
                let label_bounds = label_size.map(|(width, height)| Rect {
                    x: label_anchor.x - width / 2,
                    y: label_anchor.y - height / 2,
                    width,
                    height,
                });
                let markers = edge
                    .relation
                    .as_ref()
                    .map_or([EndMarker::Filled; 2], |relation| relation.markers);
                let [left, right] = edge.multiplicities();
                SceneEdge {
                    source: edge.source().to_owned(),
                    target: edge.target().to_owned(),
                    points,
                    label_bounds,
                    label_lines,
                    style: edge.style(),
                    arrowheads: edge.arrowheads(),
                    start_marker: markers[0],
                    end_marker: markers[1],
                    multiplicities: [
                        left.map(|label| endpoint_label(label, start, self.direction(), true)),
                        right.map(|label| endpoint_label(label, end, self.direction(), false)),
                    ],
                    source_range: edge.source_range(),
                }
            })
            .collect();
        normalize_scene(nodes, edges)
    }
}

struct NodeLabels {
    lines: Vec<String>,
    heading_lines: usize,
    member_rows: Vec<(usize, usize, SourceRange)>,
}

impl NodeLabels {
    fn new(node: &Node, width: i32) -> Self {
        let mut lines = wrap(node.label(), width);
        let heading_lines = if node.shape() == NodeShape::Compartment {
            lines.len()
        } else {
            0
        };
        let mut member_rows = Vec::new();
        for member in &node.members {
            let start = lines.len() - heading_lines;
            let details = wrap_lines(&member.text, width, true);
            if let Some(range) = member.source_range {
                member_rows.push((start, details.len(), range));
            }
            lines.extend(details);
        }
        Self {
            lines,
            heading_lines,
            member_rows,
        }
    }

    fn member_hits(&self, bounds: Rect) -> Vec<(Rect, SourceRange)> {
        let top = card_body_top(bounds, self.heading_lines);
        self.member_rows
            .iter()
            .map(|&(start, count, range)| {
                (
                    Rect {
                        x: bounds.x,
                        y: top + start as i32 * LINE_HEIGHT,
                        width: bounds.width,
                        height: count as i32 * LINE_HEIGHT - 1,
                    },
                    range,
                )
            })
            .collect()
    }
}

pub(crate) fn card_body_top(bounds: Rect, heading_lines: usize) -> i32 {
    bounds.y
        + CARD_HEADER_PAD
        + heading_lines as i32 * LINE_HEIGHT
        + CARD_DIVIDER_PAD
        + CARD_BODY_PAD
}

fn endpoint_label(label: &str, port: Point, direction: Direction, outgoing: bool) -> EndpointLabel {
    let lines = wrap(label, 92);
    let (width, height) = label_size(&lines).unwrap_or((0, 0));
    let (x, y) = match (direction, outgoing) {
        (Direction::TopToBottom, true) => (port.x + 8, port.y + 4),
        (Direction::TopToBottom, false) => (port.x + 8, port.y - height - 4),
        (Direction::LeftToRight, true) => (port.x + 5, port.y + 8),
        (Direction::LeftToRight, false) => (port.x - width - 5, port.y + 8),
    };
    EndpointLabel {
        bounds: Rect {
            x,
            y,
            width,
            height,
        },
        lines,
    }
}

fn ranks(count: usize, endpoints: &[(usize, usize)]) -> Vec<usize> {
    let mut outbound = vec![Vec::new(); count];
    for &(source, target) in endpoints {
        outbound[source].push(target);
    }
    let mut visited = vec![false; count];
    let mut finished = Vec::with_capacity(count);
    for node in 0..count {
        visit(node, &outbound, &mut visited, &mut finished);
    }
    let mut order = vec![0; count];
    for (position, &node) in finished.iter().rev().enumerate() {
        order[node] = position;
    }
    let mut ranks = vec![0; count];
    // Reverse finish order is topological for acyclic input. In a cycle, the
    // backward edges retain their actual endpoints but do not assign ranks.
    for &node in finished.iter().rev() {
        for &target in &outbound[node] {
            if order[node] < order[target] {
                ranks[target] = ranks[target].max(ranks[node] + 1);
            }
        }
    }
    ranks
}

fn visit(node: usize, outbound: &[Vec<usize>], visited: &mut [bool], finished: &mut Vec<usize>) {
    if visited[node] {
        return;
    }
    visited[node] = true;
    // The graph's node limit also bounds the maximum recursion depth.
    for &target in &outbound[node] {
        visit(target, outbound, visited, finished);
    }
    finished.push(node);
}

fn primary_start(rank: usize, size: i32, rank_gap: i32) -> i32 {
    MARGIN + rank_gap / 2 + rank as i32 * (size + rank_gap)
}

fn project(direction: Direction, primary: i32, cross: i32) -> Point {
    match direction {
        Direction::TopToBottom => Point {
            x: cross,
            y: primary,
        },
        Direction::LeftToRight => Point {
            x: primary,
            y: cross,
        },
    }
}

fn unproject(direction: Direction, point: Point) -> (i32, i32) {
    match direction {
        Direction::TopToBottom => (point.y, point.x),
        Direction::LeftToRight => (point.x, point.y),
    }
}

fn forward_port(direction: Direction, bounds: Rect, outgoing: bool) -> Point {
    match direction {
        Direction::TopToBottom => Point {
            x: bounds.center().x,
            y: if outgoing { bounds.bottom() } else { bounds.y },
        },
        Direction::LeftToRight => Point {
            x: if outgoing { bounds.right() } else { bounds.x },
            y: bounds.center().y,
        },
    }
}

fn route_adjacent(
    direction: Direction,
    start: Point,
    end: Point,
    middle: i32,
) -> (Vec<Point>, Point) {
    let (_, start_cross) = unproject(direction, start);
    let (_, end_cross) = unproject(direction, end);
    let mut points = vec![
        start,
        project(direction, middle, start_cross),
        project(direction, middle, end_cross),
        end,
    ];
    points.dedup();
    (
        points,
        project(direction, middle, (start_cross + end_cross) / 2),
    )
}

fn route_outer(
    direction: Direction,
    start: Point,
    end: Point,
    lane: i32,
    cross_label_size: i32,
    exit: i32,
    entry: i32,
) -> (Vec<Point>, Point) {
    let (_, start_cross) = unproject(direction, start);
    let (_, end_cross) = unproject(direction, end);
    let points = vec![
        start,
        project(direction, exit, start_cross),
        project(direction, exit, lane),
        project(direction, entry, lane),
        project(direction, entry, end_cross),
        end,
    ];
    let label_anchor = project(
        direction,
        (exit + entry) / 2,
        lane + 8 + cross_label_size / 2,
    );
    (points, label_anchor)
}

pub(crate) fn label_size(lines: &[String]) -> Option<(i32, i32)> {
    let width = lines.iter().map(|line| estimated_width(line)).max()? + 14;
    let height = lines.len() as i32 * LINE_HEIGHT + 4;
    Some((width, height))
}

fn estimated_width(text: &str) -> i32 {
    text.chars().map(glyph_width).sum()
}

fn glyph_width(character: char) -> i32 {
    if character.is_ascii() { 9 } else { 16 }
}

pub(crate) fn wrap(text: &str, max_width: i32) -> Vec<String> {
    wrap_lines(text, max_width, false)
}

fn wrap_lines(text: &str, max_width: i32, prefer_words: bool) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut width = 0;
        for character in paragraph.chars() {
            let next = glyph_width(character);
            if width + next > max_width {
                if prefer_words && character == ' ' {
                    result.push(std::mem::take(&mut line));
                    width = 0;
                    continue;
                }
                if prefer_words && let Some((prefix, tail)) = line.rsplit_once(' ') {
                    result.push(prefix.to_owned());
                    width = estimated_width(tail);
                    line = tail.to_owned();
                } else {
                    result.push(std::mem::take(&mut line));
                    width = 0;
                }
            }
            line.push(character);
            width += next;
        }
        result.push(line);
    }
    result
}

fn normalize_scene(mut nodes: Vec<SceneNode>, mut edges: Vec<SceneEdge>) -> Scene {
    let (mut min_x, mut min_y) = (MARGIN, MARGIN);
    let (mut max_x, mut max_y) = (MARGIN, MARGIN);
    let mut include = |x, y| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for node in &nodes {
        include(node.bounds.x, node.bounds.y);
        include(node.bounds.right(), node.bounds.bottom());
    }
    for edge in &edges {
        for point in &edge.points {
            include(point.x, point.y);
        }
        if let Some(bounds) = edge.label_bounds {
            include(bounds.x, bounds.y);
            include(bounds.right(), bounds.bottom());
        }
        for label in edge.multiplicities.iter().flatten() {
            include(label.bounds.x, label.bounds.y);
            include(label.bounds.right(), label.bounds.bottom());
        }
    }
    let (dx, dy) = (MARGIN - min_x, MARGIN - min_y);
    if dx != 0 || dy != 0 {
        for node in &mut nodes {
            node.bounds.x += dx;
            node.bounds.y += dy;
            for (bounds, _) in &mut node.member_hits {
                bounds.x += dx;
                bounds.y += dy;
            }
        }
        for edge in &mut edges {
            for point in &mut edge.points {
                point.x += dx;
                point.y += dy;
            }
            if let Some(bounds) = &mut edge.label_bounds {
                bounds.x += dx;
                bounds.y += dy;
            }
            for label in edge.multiplicities.iter_mut().flatten() {
                label.bounds.x += dx;
                label.bounds.y += dy;
            }
        }
    }
    Scene {
        bounds: Rect {
            x: 0,
            y: 0,
            width: max_x + dx + MARGIN,
            height: max_y + dy + MARGIN,
        },
        nodes,
        edges,
        decorations: Vec::new(),
    }
}
