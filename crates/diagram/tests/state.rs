use kortty_diagram::{
    DiagramKind, Direction, NodeShape, ParseErrorKind, Point, SourceRange, Theme, parse_mermaid,
};

#[test]
fn state_diagram_supports_markers_aliases_labels_and_a_backedge() {
    let source = "stateDiagram-v2\ndirection LR\n[*] --> idle\nidle: Waiting for input\nstate \"Awaiting input\" as idle\nidle --> running: run & verify\nrunning --> idle: again\nrunning --> [*]: stop";
    let diagram = parse_mermaid(source).unwrap();
    assert_eq!(diagram.kind(), DiagramKind::State);
    assert_eq!(diagram.direction(), Direction::LeftToRight);
    let initial = &diagram.nodes()[0];
    assert_eq!(initial.shape(), NodeShape::StateStart);
    assert_eq!(initial.source_range(), SourceRange::new(3, 3));
    assert_eq!(
        diagram.nodes()[1].label(),
        "Awaiting input\nWaiting for input"
    );
    assert_eq!(diagram.nodes()[1].source_range(), SourceRange::new(5, 5));
    assert_eq!(diagram.nodes()[3].shape(), NodeShape::StateEnd);
    assert_eq!(diagram.edges()[2].source(), "running");
    assert_eq!(diagram.edges()[2].target(), "idle");
    let scene = diagram.layout();
    let bounds = scene.edges()[1].label_bounds().unwrap();
    assert_eq!(
        scene.source_at(Point {
            x: bounds.x,
            y: bounds.y
        }),
        SourceRange::new(6, 6)
    );
    assert_eq!(scene.nodes()[0].bounds().width, 24);
    let svg = scene.to_svg(Theme::Light);
    assert_eq!(svg.matches("<circle").count(), 3);
    assert!(svg.contains("run &amp; verify"));
    let exported = diagram.to_mermaid();
    assert!(exported.contains("state \"Awaiting input\" as idle"));
    assert!(exported.contains("running --> [*]: stop"));
    assert_eq!(parse_mermaid(&exported).unwrap().to_mermaid(), exported);
}

#[test]
fn flat_states_accept_implicit_and_explicit_declarations_without_a_start() {
    let diagram = parse_mermaid("stateDiagram\nReady\nReady --> busy\nbusy --> Ready").unwrap();
    assert_eq!(diagram.nodes().len(), 2);
    assert_eq!(diagram.edges().len(), 2);
    assert_eq!(diagram.kind(), DiagramKind::State);
}

#[test]
fn state_diagrams_reject_composites_stereotypes_styles_and_excess_states() {
    for (source, line, expected) in [
        (
            "stateDiagram-v2\n[*] --> outer\nstate outer {\ninner --> done\n}",
            Some(3),
            ParseErrorKind::Unsupported,
        ),
        (
            "stateDiagram-v2\nstate branch <<choice>>\n[*] --> branch",
            Some(2),
            ParseErrorKind::Unsupported,
        ),
        (
            "stateDiagram-v2\nA --> B\nclassDef evil fill:red",
            Some(3),
            ParseErrorKind::Unsafe,
        ),
        (
            "stateDiagram-v2\n[*] --> [*]",
            Some(2),
            ParseErrorKind::Syntax,
        ),
        (
            "stateDiagram-v2\nstate \"Idle\" as idle",
            None,
            ParseErrorKind::Syntax,
        ),
    ] {
        let error = parse_mermaid(source).unwrap_err();
        assert_eq!(error.line(), line, "{source}");
        assert_eq!(error.kind(), &expected, "{source}");
    }
    let mut source = "stateDiagram-v2\n[*] --> s0\n".to_owned();
    for i in 0..12 {
        use std::fmt::Write;
        let _ = writeln!(source, "s{i} --> s{}", i + 1);
    }
    assert!(matches!(
        parse_mermaid(&source).unwrap_err().kind(),
        ParseErrorKind::Limit(_)
    ));
}
