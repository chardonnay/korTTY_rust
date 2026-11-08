use kortty_diagram::{
    Arrowheads, Diagram, DiagramError, DiagramKind, Direction, Edge, EdgeStyle, MAX_EDGES,
    MAX_NODES, MAX_SOURCE_BYTES, Node, NodeShape, NodeTone, ParseErrorKind, PngError, Point,
    SourceRange, Theme, parse_mermaid,
};

#[test]
fn reads_documented_chat_flowchart() {
    let graph = parse_mermaid(
        "flowchart TD\n  Login-->Validate;\n  Validate-->|ok| Session;\n  Validate-->|fail| Error;",
    )
    .unwrap();
    let labels: Vec<_> = graph.nodes().iter().map(Node::label).collect();
    assert_eq!(labels, ["Login", "Validate", "Session", "Error"]);
    let branches: Vec<_> = graph.edges().iter().skip(1).map(Edge::label).collect();
    assert_eq!(branches, ["ok", "fail"]);
}

#[test]
fn reads_snippet_shapes_semantics_and_chained_arrows() {
    let graph = parse_mermaid(
        "%% ordinary comment\nflowchart TB\nstart_1([\"Anfang\"]) --> decision{\"Fertig?\"}\n\
         decision -->|nein| work[\"Weiterarbeiten\"] --> decision; decision -->|ja| stop_1([\"Ende\"])\n\
         class start_1 setup\nclass decision,work work\nclass stop_1 success",
    )
    .unwrap();
    assert_eq!(graph.nodes()[0].shape(), NodeShape::Terminal);
    assert_eq!(graph.nodes()[0].tone(), NodeTone::Active);
    assert_eq!(graph.nodes()[1].shape(), NodeShape::Decision);
    assert_eq!(graph.nodes()[3].tone(), NodeTone::Success);
    assert_eq!(graph.edges().len(), 4);
    assert_eq!(graph.edges()[2].target(), "decision");
}

#[test]
fn diagram_source_round_trip_preserves_labels_and_edge_directions() {
    let mut graph = Diagram::new(Direction::LeftToRight);
    let special_label = "Ready \"soon\"; <ok>\n#60; & [✓]";
    graph
        .add_node(Node::new("compile", special_label, NodeShape::Terminal).unwrap())
        .unwrap();
    graph
        .add_node(Node::new("end", "Ausführen", NodeShape::Decision).unwrap())
        .unwrap();
    graph
        .add_edge(
            Edge::new("compile", "end", "same | next")
                .unwrap()
                .with_style(EdgeStyle::Dashed),
        )
        .unwrap();
    let round_trip = parse_mermaid(&graph.to_mermaid()).unwrap();
    assert_eq!(round_trip.direction(), graph.direction());
    assert_eq!(round_trip.nodes()[0].label(), special_label);
    assert_eq!(round_trip.nodes()[0].shape(), NodeShape::Terminal);
    assert_eq!(round_trip.nodes()[1].shape(), NodeShape::Decision);
    assert_eq!(round_trip.edges()[0].label(), "same | next");
    assert_eq!(round_trip.edges()[0].style(), EdgeStyle::Dashed);
    assert_eq!(round_trip.edges()[0].source(), round_trip.nodes()[0].id());
    assert_eq!(round_trip.edges()[0].target(), round_trip.nodes()[1].id());
}

#[test]
fn explicit_flowchart_declarations_and_edges_keep_source_lines() {
    let graph = parse_mermaid(
        "flowchart TD\na --> b\na[\"Start here\"]\na[\"Start here\"]\nb[\"Continue\"]",
    )
    .unwrap();
    let line = |id: &str| {
        graph
            .nodes()
            .iter()
            .find(|node| node.id() == id)
            .unwrap()
            .source_range()
            .unwrap()
            .start()
    };
    assert_eq!(line("a"), 3);
    assert_eq!(line("b"), 5);
    assert_eq!(graph.edges()[0].source_range().unwrap().start(), 2);
}

#[test]
fn rejects_unsupported_unsafe_malformed_and_large_sources_with_context() {
    let error = parse_mermaid("gantt\ntitle Unsupported here").unwrap_err();
    assert_eq!(error.line(), Some(1));
    assert_eq!(error.kind(), &ParseErrorKind::Unsupported);

    for input in [
        "---\ntitle: custom\n---\nflowchart TD",
        "flowchart TD\n%%{init: {}}%%\na --> b",
        "flowchart TD\na --> b; click a \"https://example.com\"",
        "flowchart TD\na --> b; %%{init: {}}%%",
        "flowchart TD\nclassDef name fill:red",
    ] {
        assert_eq!(
            parse_mermaid(input).unwrap_err().kind(),
            &ParseErrorKind::Unsafe
        );
    }

    let error = parse_mermaid("flowchart TD\na[\"unclosed] --> b").unwrap_err();
    assert_eq!(error.line(), Some(2));
    assert_eq!(error.kind(), &ParseErrorKind::Syntax);

    let oversized = "ä".repeat(MAX_SOURCE_BYTES / 2 + 1);
    let error = parse_mermaid(&oversized).unwrap_err();
    assert_eq!(error.line(), None);
    assert_eq!(error.kind(), &ParseErrorKind::SourceTooLarge);
}

#[test]
fn graph_validation_is_bounded_and_keeps_previous_state() {
    let mut graph = Diagram::default();
    for index in 0..MAX_NODES {
        graph
            .add_node(Node::new(format!("id{index}"), "", NodeShape::Action).unwrap())
            .unwrap();
    }
    assert_eq!(
        graph.add_node(Node::new("extra", "", NodeShape::Action).unwrap()),
        Err(DiagramError::TooManyNodes)
    );
    assert_eq!(
        graph.add_edge(Edge::new("id0", "missing", "").unwrap()),
        Err(DiagramError::UnknownNode("missing".to_owned()))
    );
    for _ in 0..MAX_EDGES {
        graph
            .add_edge(Edge::new("id0", "id1", "").unwrap())
            .unwrap();
    }
    assert_eq!(
        graph.add_edge(Edge::new("id0", "id1", "").unwrap()),
        Err(DiagramError::TooManyEdges)
    );
    assert_eq!(graph.layout().edges().len(), MAX_EDGES);
    assert_eq!(
        Node::new("invalid", "RTL\u{202E}", NodeShape::Action),
        Err(DiagramError::InvalidLabel)
    );
    assert_eq!(SourceRange::new(0, 1), None);
    assert_eq!(SourceRange::new(12, 4), None);
}

#[test]
fn layout_is_deterministic_and_preserves_cycles_and_hit_targets() {
    let mut graph = Diagram::new(Direction::TopToBottom);
    let range = SourceRange::new(12, 14).unwrap();
    graph
        .add_node(Node::new("start", "Start", NodeShape::Terminal).unwrap())
        .unwrap();
    graph
        .add_node(
            Node::new("choose", "Continue?", NodeShape::Decision)
                .unwrap()
                .with_source_range(range),
        )
        .unwrap();
    graph
        .add_node(Node::new("end", "End", NodeShape::Terminal).unwrap())
        .unwrap();
    for (source, target, label) in [
        ("start", "choose", ""),
        ("choose", "end", "yes"),
        ("end", "choose", "retry"),
        ("choose", "choose", "wait"),
    ] {
        graph
            .add_edge(Edge::new(source, target, label).unwrap())
            .unwrap();
    }
    let scene = graph.layout();
    assert_eq!(scene, graph.layout());
    let source = scene.nodes()[0].bounds();
    let decision = scene.nodes()[1].bounds();
    let end = scene.nodes()[2].bounds();
    assert!(source.y < decision.y && decision.y < end.y);
    let center = Point {
        x: decision.x + decision.width / 2,
        y: decision.y + decision.height / 2,
    };
    assert_eq!(scene.node_at(center).unwrap().source_range(), Some(range));
    assert!(
        scene
            .node_at(Point {
                x: decision.x,
                y: decision.y
            })
            .is_none()
    );
    let back_edge = &scene.edges()[2];
    assert_eq!(back_edge.source(), "end");
    assert_eq!(back_edge.target(), "choose");
    assert_eq!(back_edge.points().last().unwrap().y, decision.y);
    assert!(scene.edges()[3].points().len() > 2);
    for edge in scene.edges() {
        assert!(
            edge.points()
                .iter()
                .all(|&point| scene.bounds().contains(point))
        );
    }

    graph.set_direction(Direction::LeftToRight);
    let horizontal = graph.layout();
    assert!(horizontal.nodes()[0].bounds().x < horizontal.nodes()[1].bounds().x);
    assert_eq!(horizontal.nodes()[1].source_range(), Some(range));
}

#[test]
fn svg_contains_escaped_text_and_static_paint_only() {
    let graph = parse_mermaid("flowchart LR\na[\"<script>&\"] -.->|one| b{\"safe\"}").unwrap();
    let light = graph.layout().to_svg(Theme::Light);
    let dark = graph.layout().to_svg(Theme::Dark);
    assert!(light.contains("&lt;script&gt;&amp;"));
    assert!(!light.contains("<script>"));
    assert!(!light.contains("href="));
    assert!(!light.contains("foreignObject"));
    assert!(light.contains("stroke-dasharray=\"6 4\""));
    assert!(light.contains("<polygon"));
    assert!(light.ends_with("</svg>\n"));
    assert_ne!(dark, light);
}

#[test]
fn long_horizontal_edge_labels_stay_between_nodes() {
    let label = "label ".repeat(15);
    let mut graph = Diagram::new(Direction::LeftToRight);
    for id in ["left", "right"] {
        graph
            .add_node(Node::new(id, id, NodeShape::Action).unwrap())
            .unwrap();
    }
    graph
        .add_edge(Edge::new("left", "right", label).unwrap())
        .unwrap();
    graph
        .add_edge(Edge::new("left", "right", "another route").unwrap())
        .unwrap();
    let scene = graph.layout();
    let [source, target] = scene.nodes() else {
        panic!("expected two nodes")
    };
    let [first, second] = scene.edges() else {
        panic!("expected two edges")
    };
    let label = first.label_bounds().unwrap();
    assert!(source.bounds().x + source.bounds().width < label.x);
    assert!(label.x + label.width < target.bounds().x);
    assert!(second.label_bounds().unwrap().y > target.bounds().y + target.bounds().height);
    assert_ne!(first.points(), second.points());
}

#[test]
fn mindmap_uses_indentation_and_undirected_branches() {
    let diagram = parse_mermaid(
        "mindmap\n  root((Connection))\n    SSH\n      key[Key authentication]\n    Terminal display\n      Fonts",
    )
    .unwrap();
    assert_eq!(diagram.kind(), DiagramKind::Mindmap);
    let scene = diagram.layout();
    assert_eq!(scene.nodes()[0].id(), "root");
    assert_eq!(scene.nodes()[0].shape(), NodeShape::Terminal);
    assert_eq!(scene.nodes()[2].id(), "key");
    assert_eq!(scene.edges()[1].source(), scene.nodes()[1].id());
    assert_eq!(scene.edges()[2].source(), "root");
    assert!(
        scene
            .edges()
            .iter()
            .all(|edge| edge.arrowheads() == Arrowheads::None)
    );
    assert!(!scene.to_svg(Theme::Light).contains("marker-end="));
    let flow = parse_mermaid(&diagram.to_mermaid()).unwrap();
    assert!(
        flow.edges()
            .iter()
            .all(|edge| edge.arrowheads() == Arrowheads::None)
    );
    let multiple_roots = parse_mermaid("mindmap\n  first\n  second").unwrap_err();
    assert_eq!(multiple_roots.line(), Some(3));
    assert_eq!(multiple_roots.kind(), &ParseErrorKind::Syntax);
    let icon = parse_mermaid("mindmap\n  first\n    ::icon(fa fa-book)").unwrap_err();
    assert_eq!(icon.kind(), &ParseErrorKind::Unsafe);
}

#[test]
fn flat_architecture_keeps_services_junctions_and_arrow_directions() {
    let diagram = parse_mermaid(
        "architecture-beta\n  service web(server)[Web]\n  service db(database)[Database]\n  junction hub\n\
         web:R --> L:hub\n  hub:R -- L:db\n  db:T <--> B:web\n  hub:T <-- B:web",
    )
    .unwrap();
    assert_eq!(diagram.kind(), DiagramKind::Architecture);
    assert_eq!(diagram.nodes()[0].label(), "Web\nserver");
    let scene = diagram.layout();
    let arrows: Vec<_> = scene.edges().iter().map(|edge| edge.arrowheads()).collect();
    assert_eq!(
        arrows,
        [
            Arrowheads::End,
            Arrowheads::None,
            Arrowheads::Both,
            Arrowheads::Start
        ]
    );
    let svg = scene.to_svg(Theme::Dark);
    assert_eq!(svg.matches("marker-end=").count(), 2);
    assert_eq!(svg.matches("marker-start=").count(), 2);
    assert_eq!(
        parse_mermaid("architecture-beta\nservice app(custom:icon)[App]")
            .unwrap_err()
            .kind(),
        &ParseErrorKind::Unsafe,
    );
    assert_eq!(
        parse_mermaid("architecture-beta\ngroup cloud(cloud)[Cloud]")
            .unwrap_err()
            .kind(),
        &ParseErrorKind::Unsupported,
    );
}

#[test]
fn png_encodes_scene_bounds_and_rejects_oversized_rasters() {
    let scene = Diagram::default().layout();
    let png = scene.to_png(Theme::Dark, 2).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let image = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
    assert_eq!(image.width(), scene.bounds().width as u32 * 2);
    assert_eq!(image.height(), scene.bounds().height as u32 * 2);
    assert!(matches!(
        scene.to_png(Theme::Light, 0),
        Err(PngError::InvalidScale)
    ));

    let mut graph = Diagram::default();
    for index in 0..MAX_NODES {
        graph
            .add_node(Node::new(format!("n{index}"), "", NodeShape::Action).unwrap())
            .unwrap();
    }
    assert!(matches!(
        graph.layout().to_png(Theme::Light, 1),
        Err(PngError::ImageTooLarge)
    ));
}

#[test]
fn png_paints_label_glyphs_when_system_fonts_exist() {
    let graph = parse_mermaid("flowchart TD\na[Native text]").unwrap();
    let scene = graph.layout();
    let png = match scene.to_png(Theme::Light, 1) {
        Ok(png) => png,
        Err(PngError::MissingFonts) => return,
        Err(error) => panic!("could not render label: {error}"),
    };
    let image = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
    assert_label_pixels(&image, scene.nodes()[0].bounds());
}

#[test]
fn svg_paints_label_glyphs_without_a_generic_monospace_mapping() {
    use std::sync::Arc;

    use resvg::{tiny_skia, usvg};

    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    if fonts.faces().next().is_none() {
        return;
    }
    let missing_family = "__missing_kortty_test_font__";
    // Iced owns its SVG font database independently of the PNG exporter.
    fonts.set_monospace_family(missing_family);
    let options = usvg::Options {
        font_family: missing_family.into(),
        fontdb: Arc::new(fonts),
        ..Default::default()
    };
    let scene = parse_mermaid("flowchart TD\na[Native text]")
        .unwrap()
        .layout();
    let svg = scene.to_svg(Theme::Light);
    assert!(svg.contains(">Native text</text>"));
    let tree = usvg::Tree::from_str(&svg, &options).unwrap();
    let size = tree.size().to_int_size();
    let mut image = tiny_skia::Pixmap::new(size.width(), size.height()).unwrap();
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut image.as_mut());
    assert_label_pixels(&image, scene.nodes()[0].bounds());
}

fn assert_label_pixels(image: &resvg::tiny_skia::Pixmap, bounds: kortty_diagram::Rect) {
    let dark_pixels = (bounds.y + 10..bounds.y + bounds.height - 10)
        .flat_map(|y| (bounds.x + 10..bounds.x + bounds.width - 10).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            image
                .pixel(x as u32, y as u32)
                .is_some_and(|pixel| pixel.red() < 80)
        })
        .count();
    assert!(dark_pixels > 20, "expected dark text inside the node");
}
