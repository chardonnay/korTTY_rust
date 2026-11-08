use std::fmt::Write;

use kortty_diagram::{
    Arrowheads, DiagramKind, Direction, EdgeStyle, EndMarker, NodeShape, ParseErrorKind, PngError,
    Point, SourceRange, Theme, parse_mermaid,
};

#[test]
fn class_cards_preserve_members_relationships_and_source_navigation() {
    let diagram = parse_mermaid(
        "classDiagram\nclass BackupJob {\n+String name\n-List~Path~ sources\n+run() int\n}\n\
         class Archive\nBackupJob --> Archive : creates\nArchive <|-- EncryptedArchive\n\
         BackupJob \"1\" *-- \"0..*\" Archive : owns & checks\nArchive : #boolean archived",
    )
    .unwrap();
    assert_eq!(diagram.kind(), DiagramKind::Class);
    assert_eq!(diagram.nodes().len(), 3);
    assert_eq!(diagram.nodes()[0].shape(), NodeShape::Compartment);
    assert_eq!(
        diagram.nodes()[0].members().collect::<Vec<_>>(),
        ["+String name", "-List~Path~ sources", "+run() int"]
    );
    assert_eq!(
        diagram.nodes()[1].members().collect::<Vec<_>>(),
        ["#boolean archived"]
    );
    assert_eq!(
        diagram.edges()[2].multiplicities(),
        [Some("1"), Some("0..*")]
    );
    let scene = diagram.layout();
    let bounds = scene.nodes()[0].bounds();
    assert_eq!(
        scene.source_at(Point {
            x: bounds.x + 18,
            y: bounds.y + 12
        }),
        SourceRange::new(2, 2)
    );
    assert_eq!(
        scene.source_at(Point {
            x: bounds.x + 18,
            y: bounds.y + 61
        }),
        SourceRange::new(4, 4)
    );
    let composition = &scene.edges()[2];
    let port = composition.points()[0];
    assert_eq!(
        scene.source_at(Point {
            x: port.x + 10,
            y: port.y + 7
        }),
        SourceRange::new(10, 10)
    );
    assert_eq!(scene.edges()[0].end_marker(), EndMarker::Open);
    let inheritance = &scene.edges()[1];
    assert_eq!(inheritance.start_marker(), EndMarker::Inheritance);
    let [first, next, ..] = inheritance.points() else {
        panic!("missing inheritance route");
    };
    assert_eq!(
        scene.source_at(Point {
            x: (first.x + next.x) / 2,
            y: (first.y + next.y) / 2,
        }),
        SourceRange::new(9, 9)
    );
    assert_eq!(composition.start_marker(), EndMarker::Composition);
    let svg = scene.to_svg(Theme::Dark);
    assert!(svg.contains("marker-start=\"url(#relation-composition)\""));
    assert!(svg.contains("owns &amp; checks"));
    assert!(svg.contains(">-List~Path~ sources</text>"));
    let exported = diagram.to_mermaid();
    assert!(exported.contains("BackupJob \"1\" *-- \"0..*\" Archive: owns #38; checks"));
    assert_eq!(parse_mermaid(&exported).unwrap().to_mermaid(), exported);
}

#[test]
fn er_cards_keep_attribute_metadata_and_render_cardinality_at_each_end() {
    let diagram = parse_mermaid(
        "erDiagram\ndirection LR\nCUSTOMER ||--o{ ORDER : places\n\
         ORDER }|..o| CUSTOMER : may refer\nORDER ||--|{ ORDER_ITEM : contains\nCUSTOMER {\n\
         int id PK, FK \"& ref\"\nvarchar(80) name\nvarchar(120) email UK \"unique login\"\n}",
    )
    .unwrap();
    assert_eq!(diagram.kind(), DiagramKind::EntityRelationship);
    assert_eq!(diagram.direction(), Direction::LeftToRight);
    assert_eq!(diagram.nodes()[0].source_range(), SourceRange::new(6, 6));
    assert_eq!(
        diagram.nodes()[0].members().collect::<Vec<_>>(),
        [
            "int id PK, FK \"& ref\"",
            "varchar(80) name",
            "varchar(120) email UK \"unique login\""
        ]
    );
    let scene = diagram.layout();
    let first = &scene.edges()[0];
    assert_eq!(first.arrowheads(), Arrowheads::Both);
    assert_eq!(first.start_marker(), EndMarker::ExactlyOne);
    assert_eq!(first.end_marker(), EndMarker::ZeroOrMany);
    let second = &scene.edges()[1];
    assert_eq!(second.style(), EdgeStyle::Dashed);
    assert_eq!(second.start_marker(), EndMarker::OneOrMany);
    assert_eq!(second.end_marker(), EndMarker::ZeroOrOne);
    let caption = second.label_bounds().unwrap();
    assert_eq!(
        scene.source_at(Point {
            x: caption.x + 2,
            y: caption.y + 2
        }),
        SourceRange::new(4, 4)
    );
    let svg = scene.to_svg(Theme::Light);
    assert!(svg.contains("marker-start=\"url(#relation-one)\""));
    assert!(svg.contains("marker-end=\"url(#relation-zero-or-many)\""));
    assert!(svg.contains("&amp; ref"));
    let exported = diagram.to_mermaid();
    assert!(exported.contains("CUSTOMER ||--o{ ORDER: \"places\""));
    assert!(exported.contains("ORDER }|..o| CUSTOMER: \"may refer\""));
    assert!(exported.contains("int id PK, FK \"#38; ref\""));
    assert_eq!(parse_mermaid(&exported).unwrap().to_mermaid(), exported);

    let png = match scene.to_png(Theme::Light, 1) {
        Ok(png) => png,
        Err(PngError::MissingFonts) => return,
        Err(error) => panic!("could not render relationship scene: {error}"),
    };
    let image = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
    assert_eq!(image.width(), scene.bounds().width as u32);
    assert_eq!(image.height(), scene.bounds().height as u32);
}

#[test]
fn type_parsers_reject_unsupported_directives_and_invalid_or_excess_data() {
    for (source, line, kind) in [
        (
            "classDiagram\nclass Job {\n<<interface>>\n}",
            3,
            ParseErrorKind::Unsupported,
        ),
        (
            "classDiagram\nclass Job {\n+run()",
            2,
            ParseErrorKind::Syntax,
        ),
        (
            "classDiagram\nJob : ++run()",
            2,
            ParseErrorKind::Unsupported,
        ),
        (
            "erDiagram\nCUSTOMER ||--o{ ORDER",
            2,
            ParseErrorKind::Syntax,
        ),
        (
            "erDiagram\nCUSTOMER |}--o{ ORDER: places",
            2,
            ParseErrorKind::Unsupported,
        ),
        (
            "erDiagram\nCUSTOMER {\nint id PK, PK\n}",
            3,
            ParseErrorKind::Syntax,
        ),
        ("erDiagram\nCUSTOMER {\nint id", 2, ParseErrorKind::Syntax),
    ] {
        let error = parse_mermaid(source).unwrap_err();
        assert_eq!(error.line(), Some(line), "{source}");
        assert_eq!(error.kind(), &kind, "{source}");
    }
    for (header, member, count) in [
        ("classDiagram\nclass Item {\n", "+String member", 21),
        ("erDiagram\nITEM {\n", "int attribute", 41),
    ] {
        let mut source = header.to_owned();
        for index in 0..count {
            writeln!(source, "{member}{index}").unwrap();
        }
        source.push('}');
        assert!(matches!(
            parse_mermaid(&source).unwrap_err().kind(),
            ParseErrorKind::Limit(_)
        ));
    }
}
