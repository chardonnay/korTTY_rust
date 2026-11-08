use kortty_diagram::{
    Arrowheads, DiagramKind, EdgeStyle, EndMarker, NodeShape, ParseErrorKind, Point, SourceRange,
    Theme, parse_mermaid,
};

#[test]
fn sequence_messages_use_ordered_lifelines_and_keep_their_arrows() {
    let source = "sequenceDiagram\nactor client as Client & UI\nparticipant server\nclient->>server: Connect\nserver-->>client: Reply\nclient->>client: Remember\nclient -x server: Cancel\nserver --) client: Async\nclient -> server: Plain";
    let diagram = parse_mermaid(source).unwrap();
    assert_eq!(diagram.kind(), DiagramKind::Sequence);
    assert_eq!(diagram.nodes()[0].shape(), NodeShape::Terminal);
    assert_eq!(diagram.nodes()[0].label(), "Client & UI");
    let scene = diagram.layout();
    let (client, server) = (scene.nodes()[0].bounds(), scene.nodes()[1].bounds());
    assert_eq!(client.y, server.y);
    assert!(client.x < server.x);
    assert_eq!(scene.edges()[0].points()[0].x, client.x + client.width / 2);
    assert_eq!(scene.edges()[0].points()[1].x, server.x + server.width / 2);
    assert_eq!(scene.edges()[1].style(), EdgeStyle::Dashed);
    assert_eq!(scene.edges()[2].points().len(), 4);
    assert_eq!(
        scene.edges()[2].points()[0].x,
        scene.edges()[2].points()[3].x
    );
    assert_eq!(scene.edges()[3].end_marker(), EndMarker::Cross);
    assert_eq!(scene.edges()[4].end_marker(), EndMarker::Open);
    assert_eq!(scene.edges()[5].arrowheads(), Arrowheads::None);
    let mut row = 0;
    for edge in scene.edges() {
        assert!(edge.points()[0].y > row);
        row = edge.points()[0].y;
    }
    let label = scene.edges()[1].label_bounds().unwrap();
    assert_eq!(
        scene.source_at(Point {
            x: label.x,
            y: label.y
        }),
        SourceRange::new(5, 5)
    );
    assert_eq!(scene, diagram.layout());
    let svg = scene.to_svg(Theme::Light);
    assert!(svg.contains("Client &amp; UI"));
    assert!(svg.contains("marker-end=\"url(#arrow-cross)\""));
    assert!(svg.contains("marker-end=\"url(#arrow-open)\""));
    assert!(svg.contains("<line"));
    assert_eq!(
        &scene.to_png(Theme::Light, 1).unwrap()[..8],
        b"\x89PNG\r\n\x1a\n"
    );
}

#[test]
fn sequence_notes_and_fragments_preserve_structure_and_export() {
    let source = "sequenceDiagram\na->>b: Initial\nparticipant a as Calling app\nparticipant a as Calling app\nNote left of a: Before\nalt authorized\n  opt audit\n    a->>b: Log\n  end\nelse denied\n  Note over a,b: <retry>#59; later\n  loop backoff\n    par network\n      b-->>a: Reject\n    and cleanup\n      Note right of b: Reset\n      b->>b: Clear\n    end\n  end\nend";
    let diagram = parse_mermaid(source).unwrap();
    assert_eq!(diagram.nodes().len(), 2);
    assert_eq!(diagram.nodes()[0].label(), "Calling app");
    assert_eq!(diagram.nodes()[0].source_range(), SourceRange::new(3, 3));
    let scene = diagram.layout();
    let note = scene
        .nodes()
        .iter()
        .find(|node| node.id() == "note:11")
        .unwrap();
    let bounds = note.bounds();
    assert_eq!(
        scene.source_at(Point {
            x: bounds.x,
            y: bounds.y
        }),
        SourceRange::new(11, 11)
    );
    let svg = scene.to_svg(Theme::Dark);
    assert!(svg.contains("&lt;retry&gt;; later"));
    assert!(!svg.contains("<retry>"));
    assert_eq!(
        svg.matches("fill=\"none\" stroke=\"#cbd5e1\"/>").count(),
        4 + 2
    );
    let exported = diagram.to_mermaid();
    assert!(exported.starts_with("sequenceDiagram\n    participant p0 as Calling app\n"));
    assert!(exported.contains("Note over p0,p1: #60;retry#62;#59; later"));
    assert!(exported.contains("\n    and cleanup\n"));
    let imported_again = parse_mermaid(&exported).unwrap();
    assert_eq!(imported_again.to_mermaid(), exported);
    assert_eq!(imported_again.layout().bounds(), scene.bounds());
}

#[test]
fn sequence_exports_keywords_as_display_labels_and_remaps_every_endpoint() {
    let source = concat!(
        "sequenceDiagram\n",
        "actor actor\n",
        "participant p0\n",
        "participant participant as Service & API\n",
        "actor->>p0: Connect\n",
        "p0-->>participant: Reply\n",
        "Note over actor,participant: Tracking\n",
    );
    let diagram = parse_mermaid(source).unwrap();
    let exported = diagram.to_mermaid();
    assert_eq!(
        exported,
        concat!(
            "sequenceDiagram\n",
            "    actor p0 as actor\n",
            "    participant p1 as p0\n",
            "    participant p2 as Service #38; API\n",
            "    p0->>p1: Connect\n",
            "    p1-->>p2: Reply\n",
            "    Note over p0,p2: Tracking\n",
        )
    );
    let imported = parse_mermaid(&exported).unwrap();
    assert_eq!(imported.to_mermaid(), exported);
    assert_eq!(imported.nodes()[0].shape(), NodeShape::Terminal);
    assert_eq!(imported.nodes()[0].label(), "actor");
    assert_eq!(imported.nodes()[1].label(), "p0");
    assert_eq!(imported.nodes()[2].label(), "Service & API");
    assert_eq!(imported.edges()[1].source(), "p1");
    assert_eq!(imported.edges()[1].target(), "p2");
}

#[test]
fn sequence_rejects_unsafe_unclosed_and_wrong_nested_constructs() {
    for (source, line, kind) in [
        (
            "sequenceDiagram\na->>b: Hi\nloop retry\n a->>b: Again",
            3,
            ParseErrorKind::Syntax,
        ),
        (
            "sequenceDiagram\na->>b: Hi\nloop retry\nelse fail\nend",
            4,
            ParseErrorKind::Syntax,
        ),
        ("sequenceDiagram\na->>b: Hi\nend", 3, ParseErrorKind::Syntax),
        (
            "sequenceDiagram\na->>b: Hi\nlink a: https://example.com",
            3,
            ParseErrorKind::Unsafe,
        ),
        (
            "sequenceDiagram\na->>b: Hi\nautonumber",
            3,
            ParseErrorKind::Unsupported,
        ),
        (
            "sequenceDiagram\na->>b: Hi; extra",
            2,
            ParseErrorKind::Unsupported,
        ),
    ] {
        let error = parse_mermaid(source).unwrap_err();
        assert_eq!(error.line(), Some(line), "{source}");
        assert_eq!(error.kind(), &kind, "{source}");
    }
}

#[test]
fn sequence_limits_messages_participants_and_nesting() {
    let many_participants = (0..13).fold("sequenceDiagram\n".to_owned(), |mut source, index| {
        use std::fmt::Write;
        let _ = writeln!(source, "participant p{index}");
        source
    });
    assert!(matches!(
        parse_mermaid(&many_participants).unwrap_err().kind(),
        ParseErrorKind::Limit(_)
    ));
    let many_messages = "sequenceDiagram\n".to_owned() + &"a->>b: m\n".repeat(61);
    assert!(matches!(
        parse_mermaid(&many_messages).unwrap_err().kind(),
        ParseErrorKind::Limit(_)
    ));
    let many_frames = "sequenceDiagram\n".to_owned() + &"loop retry\n".repeat(9);
    assert!(matches!(
        parse_mermaid(&many_frames).unwrap_err().kind(),
        ParseErrorKind::Limit(_)
    ));
}
