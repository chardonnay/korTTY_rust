//! Native, bounded diagrams for terminal workflows and job dependencies.
//!
//! Build a [`Diagram`] from application data or read supported Mermaid syntax with
//! [`parse_mermaid`]. [`Diagram::layout`] produces a scene for a native view or
//! static SVG and PNG output and can run on a worker thread.
//!
//! ```
//! use kortty_diagram::{Theme, parse_mermaid};
//!
//! let diagram = parse_mermaid("flowchart TD\nstart[Connect] --> ready[Ready]")?;
//! let scene = diagram.layout();
//! assert_eq!(scene.nodes().len(), 2);
//! assert!(scene.to_svg(Theme::Light).contains("Connect"));
//! # Ok::<(), kortty_diagram::ParseError>(())
//! ```

#![forbid(unsafe_code)]

mod layout;
mod mermaid;
mod model;
mod png;
mod script;
mod sequence;
mod svg;

pub use layout::{EndMarker, Point, Rect, Scene, SceneEdge, SceneNode};
pub use mermaid::{MAX_SOURCE_BYTES, ParseError, ParseErrorKind, parse_mermaid};
pub use model::{
    Arrowheads, Diagram, DiagramError, DiagramKind, Direction, Edge, EdgeStyle, MAX_EDGES,
    MAX_LABEL_CHARS, MAX_NODES, Node, NodeShape, NodeTone, SourceRange,
};
pub use png::PngError;
pub use script::{MAX_SCRIPT_BYTES, MAX_SCRIPT_LINES, ScriptError, ScriptErrorKind, script_flow};
pub use svg::Theme;
