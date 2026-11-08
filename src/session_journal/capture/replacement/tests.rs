use std::fs;

use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use super::super::live_tests::{ID, Scratch};
use super::*;

const TIME: &str = "2026-10-01T11:05:00+02:00";
const BUDGET: u64 = 4 * MAX_PART_BYTES;

fn header(number: u32) -> String {
    format!(
        "{{\"journalId\":\"{ID}\",\"formatVersion\":1,\"type\":\"meta\",\"part\":{number},\"unknown\":true}}"
    )
}

fn fixture(extension: &str) -> (String, String) {
    if extension == "xml" {
        let input = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\
             <session-log formatVersion=\"1\" journalId=\"{ID}\" part=\"1\">\n\
             <meta host=\"secret\"/>\r\n\
             \t<out seq=\"1\" unknown='a > b' t=\"{TIME}\" repeat=\"2\">\
             <!--prefix-->se<![CDATA[cret]]> secret<!--suffix--></out> \r\n\
             <note t=\"{TIME}\" seq=\"2\">unchanged &amp; exact</note>\n\
             \r\n</session-log>"
        );
        let expected = input.replacen(
            "se<![CDATA[cret]]> secret",
            "a&lt;&amp;&#10;Ω a&lt;&amp;&#10;Ω",
            1,
        );
        return (input, expected);
    }

    let (start, prefix, end) = if extension == "json" {
        (format!("{}\r\n", header(1)), "", "{\"type\":\"end\"}")
    } else {
        (
            format!(
                "formatVersion: 1\r\njournalId: \"{ID}\"\npart: 1\r\nmeta: {}\nentries:\r\n",
                header(1)
            ),
            "- ",
            "end: true",
        )
    };
    let input = format!(
        "{start}\t{prefix}{{ \"k\":\"out\",\"unknown\":{{\"x\":\"secret\",\"number\":1.00e+02}}, \
         \"seq\":1,\"t\":\"{TIME}\",\"repeat\":2,\"\\u0078\" : \"secret secret\" }} \r\n\
         {prefix}{{\"t\":\"{TIME}\",\"k\":\"note\",\"x\":\"unchanged\\u0020exact\",\"seq\":2}}\n\
         \r\n{end}"
    );
    let expected = input.replacen("\"secret secret\"", "\"a<&\\nΩ a<&\\nΩ\"", 1);
    (input, expected)
}

fn encode_fixture(suffix: &str, bytes: &[u8]) -> Vec<u8> {
    match suffix {
        ".gz" => {
            let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        ".zst" => compress_to_vec(bytes, CompressionLevel::Fastest),
        _ => bytes.to_vec(),
    }
}

#[test]
fn capture_replacement_preserves_nine_format_and_codec_combinations() {
    let scratch = Scratch::new();
    let directory = Directory::open(&scratch.journal, false).unwrap();
    let rule = Rule::compile("secret", "a<&\nΩ", false, false).unwrap();
    let no_match = Rule::compile("not-in-the-record", "no", false, false).unwrap();
    for extension in ["xml", "json", "yaml"] {
        let (input, expected) = fixture(extension);
        for suffix in ["", ".gz", ".zst"] {
            let name = format!("session-log.{extension}{suffix}");
            let path = scratch.journal.join(&name);
            let original = encode_fixture(suffix, input.as_bytes());
            fs::write(&path, &original).unwrap();
            let parts = inventory(&directory, 1).unwrap();
            assert_eq!(parts.len(), 1);
            let part = &parts[0];
            assert_eq!(part.name(), name);
            let mut budget = BUDGET;
            let (_, raw) = part.read(&directory, &mut budget).unwrap();
            assert_eq!(budget, BUDGET - original.len() as u64);
            let preview = part
                .rewrite(
                    &raw,
                    ID,
                    &rule,
                    Mode::Preview { allow_open: false },
                    &mut budget,
                    &|| false,
                )
                .unwrap();
            assert_eq!(preview, (1, None), "{name}");
            let result = part
                .rewrite(&raw, ID, &rule, Mode::Rewrite, &mut budget, &|| false)
                .unwrap();
            assert_eq!(result.0, 1, "{name}");
            let encoded = result.1.unwrap();
            let mut decoded = Vec::new();
            part.format
                .decode(encoded.as_slice(), || false)
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(std::str::from_utf8(&decoded).unwrap(), expected, "{name}");
            let unchanged = part
                .rewrite(&raw, ID, &no_match, Mode::Rewrite, &mut budget, &|| false)
                .unwrap();
            assert_eq!(unchanged, (0, None), "{name}");
            assert_eq!(fs::read(&path).unwrap(), original, "{name}");
            fs::remove_file(path).unwrap();
        }
    }
}

#[test]
fn capture_replacement_requires_unambiguous_complete_part_inventory() {
    let scratch = Scratch::new();
    let directory = Directory::open(&scratch.journal, false).unwrap();
    assert!(inventory(&directory, 0).unwrap().is_empty());
    assert!(inventory(&directory, 1).is_err());

    let first = scratch.journal.join("session-log.json");
    let second = scratch.journal.join("session-log-2.xml.zst");
    fs::write(&second, b"").unwrap();
    assert!(inventory(&directory, 0).is_err());
    fs::write(&first, b"").unwrap();
    let parts = inventory(&directory, 1).unwrap();
    assert_eq!(
        parts.iter().map(Part::name).collect::<Vec<_>>(),
        ["session-log.json", "session-log-2.xml.zst"]
    );
    assert!(inventory(&directory, 3).is_err());

    for unexpected in [
        "session-log.xml.gz",
        "session-log-2.XML",
        "session-log-3.json.old",
    ] {
        let path = scratch.journal.join(unexpected);
        fs::write(&path, b"").unwrap();
        assert!(inventory(&directory, 2).is_err(), "{unexpected}");
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn capture_replacement_read_rejects_hardlinks_and_an_inventory_file_replacement() {
    let scratch = Scratch::new();
    let directory = Directory::open(&scratch.journal, false).unwrap();
    let path = scratch.journal.join("session-log.json");
    let other = scratch.journal.join("other.json");
    fs::write(&path, b"original").unwrap();
    let parts = inventory(&directory, 1).unwrap();
    fs::hard_link(&path, &other).unwrap();
    let mut budget = BUDGET;
    assert!(parts[0].read(&directory, &mut budget).is_err());
    fs::remove_file(&other).unwrap();
    fs::rename(&path, &other).unwrap();
    fs::write(&path, b"different object").unwrap();
    assert!(parts[0].read(&directory, &mut budget).is_err());
    assert_eq!(budget, BUDGET);
}

#[test]
fn capture_replacement_preview_allows_an_open_part_but_rejects_corrupt_or_oversized_rows() {
    let scratch = Scratch::new();
    let directory = Directory::open(&scratch.journal, false).unwrap();
    let rule = Rule::compile("secret", "changed", false, false).unwrap();
    let path = scratch.journal.join("session-log.json");
    let open = format!(
        "{}\r\n{{\"seq\":1,\"t\":\"{TIME}\",\"k\":\"out\",\"x\":\"secret\"}}\n",
        header(1)
    );
    fs::write(&path, &open).unwrap();
    let parts = inventory(&directory, 1).unwrap();
    let part = &parts[0];
    let preview = Mode::Preview { allow_open: true };
    let mut budget = BUDGET;
    let (_, raw) = part.read(&directory, &mut budget).unwrap();
    assert_eq!(
        part.rewrite(&raw, ID, &rule, preview, &mut budget, &|| false)
            .unwrap(),
        (1, None)
    );
    assert!(
        part.rewrite(&raw, ID, &rule, Mode::Rewrite, &mut budget, &|| false)
            .is_err()
    );

    for corrupt in [
        format!("{open}{{\"seq\":2"),
        format!("{open}{{\"type\":\"end\"}}\n{{\"type\":\"end\"}}\n"),
        format!("{}\n", header(2)),
        format!("{open}{}\n", " ".repeat(MAX_LINE_BYTES as usize)),
    ] {
        assert!(
            part.rewrite(corrupt.as_bytes(), ID, &rule, preview, &mut budget, &|| {
                false
            })
            .is_err()
        );
    }
    let mut insufficient = raw.len() as u64 - 1;
    assert!(
        part.rewrite(&raw, ID, &rule, preview, &mut insufficient, &|| false)
            .is_err()
    );
    assert_eq!(
        part.rewrite(&raw, ID, &rule, preview, &mut budget, &|| true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn capture_replacement_rejects_inconsistent_yaml_and_corrupt_compressed_data() {
    let scratch = Scratch::new();
    let directory = Directory::open(&scratch.journal, false).unwrap();
    let rule = Rule::compile("secret", "changed", false, false).unwrap();
    let preview = Mode::Preview { allow_open: true };
    for (extension, suffix) in [("yaml", ""), ("json", ".gz"), ("xml", ".zst")] {
        let (input, _) = fixture(extension);
        let mut raw = if extension == "yaml" {
            input.replacen("part: 1", "part: 2", 1).into_bytes()
        } else {
            encode_fixture(suffix, input.as_bytes())
        };
        if !suffix.is_empty() {
            raw.truncate(raw.len() - 1);
        }
        let path = scratch
            .journal
            .join(format!("session-log.{extension}{suffix}"));
        fs::write(&path, &raw).unwrap();
        let mut budget = BUDGET;
        let parts = inventory(&directory, 1).unwrap();
        let part = &parts[0];
        let (_, raw) = part.read(&directory, &mut budget).unwrap();
        assert!(
            part.rewrite(&raw, ID, &rule, preview, &mut budget, &|| false)
                .is_err()
        );
        fs::remove_file(path).unwrap();
    }
}
