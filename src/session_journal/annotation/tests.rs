use tiny_skia::{Color, Pixmap, PremultipliedColorU8};

use super::*;

fn parse(input: &str) -> io::Result<Vec<Annotation>> {
    let source = format!("<entry><id>shot-7</id>{input}</entry>");
    let document = crate::session_journal::parse_xml(&source, 64 * 1024)?;
    read(document.root_element())
}

fn mark(kind: Kind, points: &[[f32; 2]]) -> Annotation {
    Annotation {
        kind,
        points: points.into(),
        color: [255, 0, 0],
        stroke_width: 2.0,
        ..Annotation::default()
    }
}

fn white(width: u32, height: u32) -> Pixmap {
    let mut image = Pixmap::new(width, height).unwrap();
    image.fill(Color::WHITE);
    image
}

fn pixel(image: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let color = image.pixel(x, y).unwrap();
    [color.red(), color.green(), color.blue(), color.alpha()]
}

#[test]
fn journal_annotation_xml_round_trips_java_flattened_points_and_omits_empty_wrapper()
-> io::Result<()> {
    let input = "<annotations>
        <annotation><color>bad-color</color><strokeWidth>0</strokeWidth>
        <points><v>1.5</v><v>2</v><v>3</v><v>4.25</v></points></annotation>
        <annotation><kind>BOX</kind><color>#fa0</color><strokeWidth>8</strokeWidth>
        <points><v>14</v><v>24</v><v>-8</v><v>-12</v></points></annotation>
        <annotation><kind>PIXELATE</kind><color>#0e7490</color>
        <points><v>2</v><v>4</v><v>32</v><v>16</v></points></annotation>
        <annotation><kind>TEXT</kind><fontSize>40</fontSize><text>Ä &amp; &lt;red&gt;&#13;line</text>
        <points><v>16</v><v>40</v></points></annotation>
        </annotations>";
    let marks = parse(input)?;
    assert_eq!(marks.len(), 4);
    assert_eq!(marks[0].kind, Kind::Pen);
    assert_eq!(marks[0].color, DEFAULT_COLOR);
    assert_eq!(marks[0].stroke_width, DEFAULT_STROKE_WIDTH);
    assert_eq!(marks[0].font_size, DEFAULT_FONT_SIZE);
    assert_eq!(marks[0].points, [[1.5, 2.0], [3.0, 4.25]]);
    assert_eq!(marks[1].color, [255, 170, 0]);
    assert_eq!(marks[1].points[1], [-8.0, -12.0]);
    assert_eq!(marks[2].kind, Kind::Pixelate);
    assert_eq!(marks[3].text, "Ä & <red>\rline");
    assert_eq!(marks[3].font_size, 40.0);

    let encoded = xml(&marks)?;
    assert!(encoded.contains("<points><v>14</v><v>24</v><v>-8</v><v>-12</v></points>"));
    assert!(encoded.contains("<text>Ä &amp; &lt;red&gt;&#13;line</text>"));
    assert_eq!(parse(&encoded)?, marks);
    let mut row = "<entry>".to_owned();
    write_xml(&mut row, &marks)?;
    assert_eq!(row, format!("<entry>{encoded}"));
    assert!(xml(&[])?.is_empty());
    assert!(parse("<annotations/>")?.is_empty());
    assert!(parse("")?.is_empty());
    Ok(())
}

#[test]
fn journal_annotation_xml_rejects_ambiguous_or_unsupported_geometry() {
    for broken in [
        "<annotations/><annotations/>",
        "<x:annotations xmlns:x='urn:unknown'/>",
        "<annotations><annotation future='x'/></annotations>",
        "<annotations><annotation><unknown/></annotation></annotations>",
        "<annotations><annotation><kind>ELLIPSE</kind></annotation></annotations>",
        "<annotations><annotation><kind>PEN</kind><kind>BOX</kind></annotation></annotations>",
        "<annotations><annotation><points><v>1</v><v>2</v><v>3</v></points></annotation></annotations>",
        "<annotations><annotation><points><v>NaN</v><v>2</v><v>3</v><v>4</v></points></annotation></annotations>",
        "<annotations><annotation><kind>BOX</kind><points><v>1</v><v>2</v><v>0.5</v><v>4</v></points></annotation></annotations>",
    ] {
        assert!(parse(broken).is_err(), "{broken}");
    }
    let oversized_pen = mark(Kind::Pen, &vec![[0.0, 0.0]; MAX_POINTS_PER_MARK + 1]);
    assert!(validate(&[oversized_pen]).is_err());
    let long_pen = mark(Kind::Pen, &vec![[0.0, 0.0]; MAX_POINTS_PER_MARK]);
    assert!(validate(&vec![long_pen; MAX_POINTS_TOTAL / MAX_POINTS_PER_MARK + 1]).is_err());
    let invalid = Annotation {
        text: "\0hidden".into(),
        ..mark(Kind::Text, &[[8.0, 8.0]])
    };
    assert!(validate(&[invalid]).is_err());
    let oversized_text = Annotation {
        text: "a".repeat(MAX_TEXT_BYTES + 1),
        ..mark(Kind::Text, &[[8.0, 8.0]])
    };
    assert!(xml(&[oversized_text]).is_err());
}

#[test]
fn journal_annotation_pixelates_in_java_block_order_before_drawing_marks() -> io::Result<()> {
    let mut base = white(24, 16);
    let dark = PremultipliedColorU8::from_rgba(0, 0, 0, 255).unwrap();
    for row in base.pixels_mut().as_chunks_mut::<24>().0.iter_mut().take(8) {
        row[..4].fill(dark);
    }
    let pen = mark(Kind::Pen, &[[1.0, 4.0], [14.0, 4.0]]);
    let pix = mark(Kind::Pixelate, &[[16.0, 8.0], [-16.0, -8.0]]);
    let annotated = render(&base, &[pen, pix])?;
    assert_eq!(pixel(&annotated, 1, 1), [127, 127, 127, 255]);
    assert_eq!(pixel(&annotated, 9, 1), [255; 4]);
    assert_eq!(pixel(&annotated, 5, 4), [255, 0, 0, 255]);
    assert_eq!(pixel(&annotated, 17, 4), pixel(&base, 17, 4));
    assert_eq!(pixel(&base, 1, 1), [0, 0, 0, 255]);
    assert_eq!(render(&base, &[])?.data(), base.data());
    Ok(())
}

#[test]
fn journal_annotation_normalizes_reverse_boxes_and_renders_text_at_baseline_with_halo()
-> io::Result<()> {
    let base = white(120, 70);
    let rectangle = mark(Kind::Box, &[[25.0, 25.0], [-20.0, -18.0]]);
    let label = Annotation {
        color: [0, 0, 255],
        text: "ÁΩ".into(),
        ..mark(Kind::Text, &[[40.0, 52.0]])
    };
    let image = render(&base, &[rectangle.clone(), label.clone()])?;
    assert_eq!(pixel(&image, 10, 7), [255, 0, 0, 255]);
    assert_eq!(pixel(&image, 10, 10), [255; 4]);
    assert_eq!(pixel(&image, 2, 2), [255; 4]);
    assert!(
        image
            .pixels()
            .iter()
            .any(|pixel| pixel.blue() == 255 && pixel.red() == 0)
    );
    assert!(image.pixels().iter().any(|pixel| pixel.red() < 200
        && pixel.red() == pixel.green()
        && pixel.green() == pixel.blue()));
    assert!((54..70).all(|y| (39..100).all(|x| pixel(&image, x, y) == [255; 4])));
    let again = render(&base, &[rectangle, label])?;
    assert_eq!(again.data(), image.data());
    Ok(())
}
