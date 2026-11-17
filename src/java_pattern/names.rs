use std::{io::Read, ops::Range, sync::OnceLock};

use flate2::read::ZlibDecoder;

struct Catalog {
    text: String,
    entries: Vec<(Range<usize>, char)>,
}

impl Catalog {
    fn load() -> Self {
        // Unicode names are only needed while translating patterns that use \N.
        let compressed = include_bytes!("../../vendor/fancy-regex/src/java_character_names.z");
        let mut text = String::new();
        ZlibDecoder::new(compressed.as_slice())
            .read_to_string(&mut text)
            .expect("generated Java Unicode name data");
        let mut entries = Vec::new();
        let mut offset = 0;
        for line in text.lines() {
            let (name, hex) = line.split_once(';').expect("generated name record");
            let code = u32::from_str_radix(hex, 16).expect("generated code point");
            let ch = char::from_u32(code).expect("generated Unicode scalar");
            entries.push((offset..offset + name.len(), ch));
            offset += line.len() + 1;
        }
        Self { text, entries }
    }

    fn lookup(&self, name: &str) -> Option<char> {
        let i = self
            .entries
            .binary_search_by(|(span, _)| self.text[span.clone()].cmp(name))
            .ok()?;
        Some(self.entries[i].1)
    }
}

pub(super) fn resolve(name: &str) -> Option<u32> {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    let name = name.trim_matches(|ch| ch <= ' ').to_uppercase();
    if let Some(ch) = CATALOG.get_or_init(Catalog::load).lookup(&name) {
        return Some(u32::from(ch));
    }
    let (block, hex) = name.rsplit_once(' ')?;
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) || hex.starts_with('0') {
        return None;
    }
    let code = u32::from_str_radix(hex, 16).ok()?;
    let canonical = match code {
        0xD800..=0xDB7F => "HIGH SURROGATES",
        0xDB80..=0xDBFF => "HIGH PRIVATE USE SURROGATES",
        0xDC00..=0xDFFF => "LOW SURROGATES",
        _ => fancy_regex::internal::java_unnamed_block(code)?,
    };
    (canonical == block).then_some(code)
}
