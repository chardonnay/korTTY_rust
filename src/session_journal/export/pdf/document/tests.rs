use std::io::Read as _;

use flate2::read::ZlibDecoder;

use super::*;

struct Stream<'a> {
    dictionary: &'a str,
    decoded: Vec<u8>,
}

// Reads the actual emitted stream lengths and inflates their bytes, without an external PDF tool.
fn streams(mut bytes: &[u8]) -> Vec<Stream<'_>> {
    let mut streams = Vec::new();
    const START: &[u8] = b"\nstream\n";
    while let Some(start) = bytes
        .windows(START.len())
        .position(|window| window == START)
    {
        let dictionary_start = bytes[..start]
            .windows(2)
            .rposition(|window| window == b"<<")
            .unwrap();
        let dictionary = std::str::from_utf8(&bytes[dictionary_start..start]).unwrap();
        let (_, length) = dictionary.split_once("/Length ").unwrap();
        let length = length
            .split_ascii_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let data = &bytes[start + START.len()..][..length];
        let mut decoded = Vec::new();
        ZlibDecoder::new(data).read_to_end(&mut decoded).unwrap();
        streams.push(Stream {
            dictionary,
            decoded,
        });
        bytes = &bytes[start + START.len() + length..];
    }
    streams
}

fn contains(bytes: &[u8], expected: &str) -> bool {
    bytes
        .windows(expected.len())
        .any(|part| part == expected.as_bytes())
}

#[test]
fn journal_pdf_embeds_real_unicode_subsets_with_searchable_cids() {
    let mut document = Document::new().unwrap();
    let page = document.new_page().unwrap();
    assert_eq!(
        document.text_width(Font::Regular, 12.0, "🦀"),
        document.text_width(Font::Regular, 12.0, "?")
    );
    let encoded = document.fonts[Font::Regular.index()]
        .encode("éЖΩ🦀")
        .unwrap();
    assert_eq!(encoded, [0, 1, 0, 2, 0, 3, 0, 4]);
    document
        .draw_text(page, [40.0, 780.0], Font::Regular, 12.0, [0, 0, 0], "éЖΩ🦀")
        .unwrap();
    document
        .draw_text(
            page,
            [40.0, 760.0],
            Font::Bold,
            12.0,
            [0, 0, 0],
            "Noto Bold",
        )
        .unwrap();
    document
        .draw_text(
            page,
            [40.0, 740.0],
            Font::Mono,
            12.0,
            [0, 0, 0],
            "Noto Mono",
        )
        .unwrap();
    let pdf = document.finish().unwrap();
    for marker in [
        "/Subtype /Type0",
        "/Subtype /CIDFontType2",
        "/Encoding /Identity-H",
        "/CIDToGIDMap ",
        "/ToUnicode ",
        "xref",
    ] {
        assert!(contains(&pdf, marker), "{marker}");
    }
    let objects = streams(&pdf);
    let fonts: Vec<_> = objects
        .iter()
        .filter(|stream| stream.dictionary.contains("/Length1"))
        .collect();
    assert_eq!(fonts.len(), 3);
    for font in fonts {
        let face = ttf_parser::Face::parse(&font.decoded, 0).unwrap();
        assert!(face.number_of_glyphs() < 32);
        assert!(font.decoded.len() < 64 * 1024);
    }
    let cmap = objects
        .iter()
        .find(|stream| contains(&stream.decoded, "<0001> <00E9>"))
        .unwrap();
    for mapping in ["<0002> <0416>", "<0003> <03A9>", "<0004> <003F>"] {
        assert!(contains(&cmap.decoded, mapping), "{mapping}");
    }
    assert!(
        objects
            .iter()
            .any(|stream| stream.decoded.len() == 10 && stream.decoded.starts_with(&[0, 0, 0, 1]))
    );
}

#[test]
fn journal_pdf_writes_lossless_rgb_alpha_outline_links_and_opacity() {
    let mut document = Document::new().unwrap();
    let first = document.new_page().unwrap();
    let second = document.new_page().unwrap();
    assert_eq!(document.page_count(), 2);
    let rgb = [255, 0, 0, 0, 255, 0];
    let alpha = [0, 170];
    let image = document.embed_image(2, 1, &rgb, Some(&alpha)).unwrap();
    document
        .draw_image(first, image, [40.0, 200.0, 80.0, 40.0])
        .unwrap();
    document
        .draw_image(second, image, [40.0, 240.0, 80.0, 40.0])
        .unwrap();
    document
        .fill_rect(first, [24.0, 700.0, 320.0, 25.0], [10, 20, 30])
        .unwrap();
    document
        .line(first, [24.0, 40.0], [260.0, 40.0], [10, 20, 30], 0.7)
        .unwrap();
    document
        .draw_rotated_text(
            second,
            [200.0, 400.0],
            Font::Bold,
            40.0,
            [100; 3],
            "Watermark",
            [32.0, 0.2],
        )
        .unwrap();
    document
        .footer_uri(first, [50.0, 24.0, 100.0, 12.0])
        .unwrap();
    let chats = document.bookmark(second, 730.0, "Chats", None).unwrap();
    document
        .bookmark(second, 705.0, "Résumé", Some(chats))
        .unwrap();
    let pdf = document.finish().unwrap();
    for marker in [
        "/SMask ",
        "/Outlines ",
        "/Title (Chats)",
        "/XYZ 0 705 0",
        "/Subtype /Link",
        "/S /URI",
        REPOSITORY_URL,
        "/ca 0.2",
    ] {
        assert!(contains(&pdf, marker), "{marker}");
    }
    let objects = streams(&pdf);
    let data = objects
        .iter()
        .find(|stream| stream.dictionary.contains("/DeviceRGB"))
        .unwrap();
    let mask = objects
        .iter()
        .find(|stream| stream.dictionary.contains("/DeviceGray"))
        .unwrap();
    assert_eq!(data.decoded, rgb);
    assert_eq!(mask.decoded, alpha);
    assert_eq!(
        objects
            .iter()
            .filter(|stream| stream.dictionary.contains("/Subtype /Image"))
            .count(),
        2
    );
}

#[test]
fn journal_pdf_rejects_invalid_geometry_image_dimensions_and_references() {
    let mut document = Document::new().unwrap();
    assert!(document.finish().is_err());
    document = Document::new().unwrap();
    let page = document.new_page().unwrap();
    assert!(
        document
            .draw_text(
                page,
                [f32::NAN, 0.0],
                Font::Regular,
                12.0,
                [0; 3],
                "invalid"
            )
            .is_err()
    );
    assert!(document.draw_image(page, 0, [0.0, 0.0, 2.0, 2.0]).is_err());
    assert!(document.embed_image(4, 1, &[0, 0, 0], None).is_err());
    assert!(document.embed_image(u32::MAX, 4, &[], None).is_err());
    assert!(
        document
            .bookmark(page, 100.0, "Invalid parent", Some(0))
            .is_err()
    );
    assert!(
        document
            .footer_uri(page + 1, [0.0, 0.0, 100.0, 20.0])
            .is_err()
    );
    assert!(document.finish().is_ok());
}
