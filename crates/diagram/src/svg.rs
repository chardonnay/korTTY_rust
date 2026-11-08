use std::fmt::Write;

use crate::{
    Arrowheads, EdgeStyle, EndMarker, NodeShape, NodeTone, Rect, Scene, SceneNode,
    layout::{CARD_BODY_PAD, CARD_HEADER_PAD, Decoration, LINE_HEIGHT, card_body_top},
    png::{PORTABLE_MONOSPACE_FAMILIES, system_label_family},
};

/// A color palette for a standalone diagram.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Theme {
    /// A light canvas suitable for printing.
    #[default]
    Light,
    /// A dark canvas for on-screen viewing.
    Dark,
}

impl Theme {
    fn palette(self) -> Palette {
        match self {
            Self::Light => Palette {
                canvas: "#ffffff",
                text: "#0f172a",
                edge: "#475569",
            },
            Self::Dark => Palette {
                canvas: "#0f172a",
                text: "#f8fafc",
                edge: "#cbd5e1",
            },
        }
    }

    pub(crate) fn text_color(self) -> &'static str {
        self.palette().text
    }

    pub(crate) fn node_colors(self, tone: NodeTone) -> (&'static str, &'static str) {
        match (self, tone) {
            (Self::Light, NodeTone::Neutral) => ("#f1f5f9", "#64748b"),
            (Self::Light, NodeTone::Active) => ("#dbeafe", "#2563eb"),
            (Self::Light, NodeTone::Success) => ("#dcfce7", "#16a34a"),
            (Self::Light, NodeTone::Failure) => ("#fee2e2", "#dc2626"),
            (Self::Light, NodeTone::Blocked) => ("#fef9c3", "#a16207"),
            (Self::Light, NodeTone::Cancelled) => ("#ffedd5", "#c2410c"),
            (Self::Light, NodeTone::Disabled) => ("#f8fafc", "#64748b"),
            (Self::Dark, NodeTone::Neutral) => ("#334155", "#94a3b8"),
            (Self::Dark, NodeTone::Active) => ("#1e3a8a", "#60a5fa"),
            (Self::Dark, NodeTone::Success) => ("#14532d", "#4ade80"),
            (Self::Dark, NodeTone::Failure) => ("#7f1d1d", "#f87171"),
            (Self::Dark, NodeTone::Blocked) => ("#713f12", "#facc15"),
            (Self::Dark, NodeTone::Cancelled) => ("#7c2d12", "#fb923c"),
            (Self::Dark, NodeTone::Disabled) => ("#1e293b", "#94a3b8"),
        }
    }
}

struct Palette {
    canvas: &'static str,
    text: &'static str,
    edge: &'static str,
}

impl Scene {
    /// Renders the scene as self-contained SVG. Input labels are escaped as
    /// plain text. The SVG contains no scripts, callbacks, links or resources.
    pub fn to_svg(&self, theme: Theme) -> String {
        let mut svg = String::new();
        let width = self.bounds.width;
        let height = self.bounds.height;
        let palette = theme.palette();
        let _ = write!(
            svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" font-family=""#
        );
        write_font_families(&mut svg, system_label_family());
        svg.push_str("\">\n");
        let _ = writeln!(
            svg,
            r#"<defs><marker id="arrow-end" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M 0 0 L 10 5 L 0 10 z" fill="{edge}"/></marker><marker id="arrow-start" viewBox="0 0 10 10" refX="1" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M 10 0 L 0 5 L 10 10 z" fill="{edge}"/></marker><marker id="arrow-open" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 1 1 L 9 5 L 1 9" fill="none" stroke="{edge}"/></marker><marker id="arrow-cross" viewBox="0 0 10 10" refX="5" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M 1 1 L 9 9 M 1 9 L 9 1" fill="none" stroke="{edge}"/></marker></defs>"#,
            edge = palette.edge
        );
        if self
            .edges
            .iter()
            .any(|edge| relation_marker(edge.start_marker) || relation_marker(edge.end_marker))
        {
            write_relation_markers(&mut svg, &palette);
        }
        let _ = writeln!(
            svg,
            r#"<rect width="{width}" height="{height}" fill="{}"/>"#,
            palette.canvas
        );
        write_decorations(&mut svg, &self.decorations, &palette);
        for edge in &self.edges {
            svg.push_str("<polyline points=\"");
            for point in &edge.points {
                let _ = write!(svg, "{},{} ", point.x, point.y);
            }
            let dashed = dash_attribute(edge.style == EdgeStyle::Dashed);
            let _ = write!(
                svg,
                r#"" fill="none" stroke="{}" stroke-width="2" stroke-linejoin="round""#,
                palette.edge
            );
            if matches!(edge.arrowheads, Arrowheads::Start | Arrowheads::Both) {
                let id = marker_id(edge.start_marker, true);
                let _ = write!(svg, r#" marker-start="url(#{id})""#);
            }
            if matches!(edge.arrowheads, Arrowheads::End | Arrowheads::Both) {
                let id = marker_id(edge.end_marker, false);
                let _ = write!(svg, r#" marker-end="url(#{id})""#);
            }
            let _ = writeln!(svg, "{dashed}/>");
        }
        for edge in &self.edges {
            for label in edge.multiplicities.iter().flatten() {
                write_label(&mut svg, label.bounds, &label.lines, palette.text);
            }
            if let Some(bounds) = edge.label_bounds {
                let _ = writeln!(
                    svg,
                    r#"<rect x="{}" y="{}" width="{}" height="{}" rx="3" fill="{}"/>"#,
                    bounds.x, bounds.y, bounds.width, bounds.height, palette.canvas
                );
                write_label(&mut svg, bounds, &edge.label_lines, palette.text);
            }
        }
        for node in &self.nodes {
            write_node(&mut svg, node, theme, &palette);
        }
        svg.push_str("</svg>\n");
        svg
    }
}

fn marker_id(marker: EndMarker, source: bool) -> &'static str {
    match marker {
        EndMarker::Filled if source => "arrow-start",
        EndMarker::Filled => "arrow-end",
        EndMarker::Open => "arrow-open",
        EndMarker::Cross => "arrow-cross",
        EndMarker::Inheritance => "relation-inheritance",
        EndMarker::Composition => "relation-composition",
        EndMarker::Aggregation => "relation-aggregation",
        EndMarker::ExactlyOne => "relation-one",
        EndMarker::ZeroOrOne => "relation-zero-or-one",
        EndMarker::OneOrMany => "relation-one-or-many",
        EndMarker::ZeroOrMany => "relation-zero-or-many",
    }
}

fn relation_marker(marker: EndMarker) -> bool {
    !matches!(
        marker,
        EndMarker::Filled | EndMarker::Open | EndMarker::Cross
    )
}

fn write_relation_markers(svg: &mut String, palette: &Palette) {
    svg.push_str("<defs>\n");
    for (marker, path, fill, circle) in [
        (
            EndMarker::Inheritance,
            "M 5 1 L 19 8 L 5 15 Z",
            palette.canvas,
            false,
        ),
        (
            EndMarker::Composition,
            "M 1 8 L 10 1 L 19 8 L 10 15 Z",
            palette.edge,
            false,
        ),
        (
            EndMarker::Aggregation,
            "M 1 8 L 10 1 L 19 8 L 10 15 Z",
            palette.canvas,
            false,
        ),
        (
            EndMarker::ExactlyOne,
            "M 9 1 V 15 M 16 1 V 15",
            "none",
            false,
        ),
        (EndMarker::ZeroOrOne, "M 16 1 V 15", "none", true),
        (
            EndMarker::OneOrMany,
            "M 6 1 V 15 M 12 8 L 19 1 M 12 8 L 19 15",
            "none",
            false,
        ),
        (
            EndMarker::ZeroOrMany,
            "M 12 8 L 19 1 M 12 8 L 19 15",
            "none",
            true,
        ),
    ] {
        let id = marker_id(marker, false);
        let stroke = palette.edge;
        let _ = writeln!(
            svg,
            r#"<marker id="{id}" viewBox="0 0 20 16" refX="19" refY="8" markerWidth="20" markerHeight="16" markerUnits="userSpaceOnUse" orient="auto-start-reverse"><path d="{path}" fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>"#
        );
        if circle {
            let _ = writeln!(
                svg,
                r#"<circle cx="5" cy="8" r="3.5" fill="{}" stroke="{stroke}" stroke-width="1.5"/>"#,
                palette.canvas
            );
        }
        svg.push_str("</marker>\n");
    }
    svg.push_str("</defs>\n");
}

fn write_decorations(svg: &mut String, decorations: &[Decoration], palette: &Palette) {
    for decoration in decorations {
        match decoration {
            Decoration::Line { start, end } => {
                let _ = writeln!(
                    svg,
                    r#"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{}" stroke-dasharray="5 5"/>"#,
                    start.x, start.y, end.x, end.y, palette.edge
                );
            }
            Decoration::Frame(bounds) => {
                let _ = writeln!(
                    svg,
                    r#"<rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="{}"/>"#,
                    bounds.x, bounds.y, bounds.width, bounds.height, palette.edge
                );
            }
            Decoration::Label { .. } => {}
        }
    }
    for decoration in decorations {
        if let Decoration::Label { bounds, lines, .. } = decoration {
            let _ = writeln!(
                svg,
                r#"<rect x="{}" y="{}" width="{}" height="{}" rx="3" fill="{}"/>"#,
                bounds.x, bounds.y, bounds.width, bounds.height, palette.canvas
            );
            write_label(svg, *bounds, lines, palette.text);
        }
    }
}

fn write_node(svg: &mut String, node: &SceneNode, theme: Theme, palette: &Palette) {
    let Rect {
        x,
        y,
        width,
        height,
    } = node.bounds;
    let (fill, stroke) = theme.node_colors(node.tone);
    let dashed = dash_attribute(node.tone == NodeTone::Disabled);
    match node.shape {
        NodeShape::Compartment => {
            let _ = writeln!(
                svg,
                r#"<rect x="{x}" y="{y}" width="{width}" height="{height}" rx="5" fill="{fill}" stroke="{stroke}" stroke-width="2"/>"#
            );
            write_compartment(svg, node, stroke, palette.text);
            return;
        }
        NodeShape::StateStart | NodeShape::StateEnd => {
            let center = node.bounds.center();
            let color = palette.text;
            let radius = width / 2;
            if node.shape == NodeShape::StateEnd {
                let _ = writeln!(
                    svg,
                    r#"<circle cx="{}" cy="{}" r="{radius}" fill="{}" stroke="{color}" stroke-width="2"/>"#,
                    center.x, center.y, palette.canvas
                );
            }
            let dot = if node.shape == NodeShape::StateStart {
                radius
            } else {
                radius - 5
            };
            let _ = writeln!(
                svg,
                r#"<circle cx="{}" cy="{}" r="{dot}" fill="{color}"/>"#,
                center.x, center.y
            );
        }
        NodeShape::Decision => {
            let center = node.bounds.center();
            let right = node.bounds.right();
            let bottom = node.bounds.bottom();
            let _ = writeln!(
                svg,
                r#"<polygon points="{},{y} {right},{} {},{bottom} {x},{}" fill="{fill}" stroke="{stroke}" stroke-width="2"{dashed}/>"#,
                center.x, center.y, center.x, center.y
            );
        }
        NodeShape::Action | NodeShape::Terminal => {
            let corner = if node.shape == NodeShape::Action {
                5
            } else {
                (height / 2).min(36)
            };
            let _ = writeln!(
                svg,
                r#"<rect x="{x}" y="{y}" width="{width}" height="{height}" rx="{corner}" fill="{fill}" stroke="{stroke}" stroke-width="2"{dashed}/>"#,
            );
        }
    }
    write_label(svg, node.bounds, &node.lines, palette.text);
}

fn write_compartment(svg: &mut String, node: &SceneNode, stroke: &str, color: &str) {
    let heading_height = node.heading_lines as i32 * LINE_HEIGHT;
    let heading = Rect {
        y: node.bounds.y + CARD_HEADER_PAD,
        height: heading_height,
        ..node.bounds
    };
    write_label(svg, heading, &node.lines[..node.heading_lines], color);
    let members = &node.lines[node.heading_lines..];
    if members.is_empty() {
        return;
    }
    let top = card_body_top(node.bounds, node.heading_lines);
    let divider = top - CARD_BODY_PAD;
    let _ = writeln!(
        svg,
        r#"<line x1="{}" x2="{}" y1="{divider}" y2="{divider}" stroke="{stroke}"/>"#,
        node.bounds.x,
        node.bounds.right()
    );
    let x = node.bounds.x + 12;
    for (index, member) in members.iter().enumerate() {
        let y = top + index as i32 * LINE_HEIGHT + 14;
        let _ = write!(
            svg,
            r#"<text x="{x}" y="{y}" font-size="14" fill="{color}" xml:space="preserve">"#
        );
        write_text(svg, member);
        svg.push_str("</text>\n");
    }
}

fn write_label(svg: &mut String, bounds: Rect, lines: &[String], color: &str) {
    let center = bounds.center();
    let first_baseline = center.y - lines.len() as i32 * LINE_HEIGHT / 2 + 14;
    for (index, line) in lines.iter().enumerate() {
        let y = first_baseline + index as i32 * LINE_HEIGHT;
        let _ = write!(
            svg,
            r#"<text x="{}" y="{y}" text-anchor="middle" font-size="14" fill="{color}" xml:space="preserve">"#,
            center.x
        );
        write_text(svg, line);
        svg.push_str("</text>\n");
    }
}

fn write_font_families(svg: &mut String, preferred: Option<&str>) {
    for family in preferred.into_iter().chain(
        PORTABLE_MONOSPACE_FAMILIES
            .into_iter()
            .filter(|family| Some(*family) != preferred),
    ) {
        let quote = if family.contains('\'') { "&quot;" } else { "'" };
        svg.push_str(quote);
        write_text(svg, family);
        svg.push_str(quote);
        svg.push_str(", ");
    }
    svg.push_str("monospace");
}

fn write_text(svg: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => svg.push_str("&amp;"),
            '<' => svg.push_str("&lt;"),
            '>' => svg.push_str("&gt;"),
            '\'' => svg.push_str("&apos;"),
            '"' => svg.push_str("&quot;"),
            _ => svg.push(character),
        }
    }
}

fn dash_attribute(dashed: bool) -> &'static str {
    if dashed {
        r#" stroke-dasharray="6 4""#
    } else {
        ""
    }
}
