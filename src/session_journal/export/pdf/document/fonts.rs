//! The Java export's three pinned Noto faces, embedded as TrueType CID subsets.

use std::collections::HashMap;
use std::io;

use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Finish as _, Name, Pdf, Rect, Ref, Str};
use subsetter::GlyphRemapper;
use ttf_parser::{Face, GlyphId};

use super::{deflate, invalid};
use crate::session_journal::is_display_character;

const REGULAR: &[u8] = include_bytes!("../fonts/NotoSans-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/NotoSans-Bold.ttf");
const MONO: &[u8] = include_bytes!("../fonts/NotoSansMono-Regular.ttf");
const IDENTITY: SystemInfo<'_> = SystemInfo {
    registry: Str(b"Adobe"),
    ordering: Str(b"Identity"),
    supplement: 0,
};
const UNICODE: SystemInfo<'_> = SystemInfo {
    registry: Str(b"Adobe"),
    ordering: Str(b"UCS"),
    supplement: 0,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Font {
    Regular,
    Bold,
    Mono,
}

impl Font {
    pub(super) const ALL: [Self; 3] = [Self::Regular, Self::Bold, Self::Mono];

    pub(super) fn index(self) -> usize {
        self as usize
    }

    pub(super) fn resource(self) -> Name<'static> {
        Name(match self {
            Self::Regular => b"F1",
            Self::Bold => b"F2",
            Self::Mono => b"F3",
        })
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Regular => REGULAR,
            Self::Bold => BOLD,
            Self::Mono => MONO,
        }
    }

    fn subset_name(self) -> Name<'static> {
        Name(match self {
            Self::Regular => b"KORTTA+NotoSans-Regular",
            Self::Bold => b"KORTTB+NotoSans-Bold",
            Self::Mono => b"KORTTC+NotoSansMono-Regular",
        })
    }
}

pub(super) struct Embedded {
    font: Font,
    face: Face<'static>,
    question: GlyphId,
    space: GlyphId,
    by_character: HashMap<char, u16>,
    used: Vec<(char, GlyphId)>,
}

impl Embedded {
    pub(super) fn new(font: Font) -> io::Result<Self> {
        let face = Face::parse(font.bytes(), 0).map_err(|_| invalid("Invalid bundled PDF font"))?;
        let question = face
            .glyph_index('?')
            .ok_or_else(|| invalid("Bundled PDF font lacks its fallback"))?;
        let space = face
            .glyph_index(' ')
            .ok_or_else(|| invalid("Bundled PDF font lacks a space"))?;
        Ok(Self {
            font,
            face,
            question,
            space,
            by_character: HashMap::new(),
            used: Vec::new(),
        })
    }

    pub(super) fn width(&self, size: f32, text: &str) -> f32 {
        let units: u64 = text
            .chars()
            .filter_map(|character| self.glyph(character))
            .map(|(_, glyph)| u64::from(self.advance(glyph)))
            .sum();
        units as f32 * size / f32::from(self.face.units_per_em())
    }

    pub(super) fn encode(&mut self, text: &str) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(text.len() * 2);
        for character in text.chars() {
            let Some((character, glyph)) = self.glyph(character) else {
                continue;
            };
            let cid = match self.by_character.get(&character) {
                Some(cid) => *cid,
                None => {
                    let cid = u16::try_from(self.used.len() + 1)
                        .map_err(|_| invalid("The PDF font exceeds its character limit"))?;
                    self.used.push((character, glyph));
                    self.by_character.insert(character, cid);
                    cid
                }
            };
            bytes.extend_from_slice(&cid.to_be_bytes());
        }
        Ok(bytes)
    }

    fn glyph(&self, character: char) -> Option<(char, GlyphId)> {
        match character {
            '\n' | '\t' => return Some((' ', self.space)),
            character if !is_display_character(character) => return None,
            _ => {}
        }
        Some(match self.face.glyph_index(character) {
            Some(glyph) => (character, glyph),
            None if character.is_whitespace() => (' ', self.space),
            // Java uses one question mark per unsupported code point.
            None => ('?', self.question),
        })
    }

    fn advance(&self, glyph: GlyphId) -> u16 {
        self.face.glyph_hor_advance(glyph).unwrap_or_default()
    }

    pub(super) fn write(&self, pdf: &mut Pdf, ids: &mut Ref) -> io::Result<Option<Ref>> {
        if self.used.is_empty() {
            return Ok(None);
        }
        let type0 = ids.bump();
        let descendant = ids.bump();
        let descriptor = ids.bump();
        let font_stream = ids.bump();
        let cid_map = ids.bump();
        let unicode = ids.bump();
        let name = self.font.subset_name();
        let mut remapper = GlyphRemapper::new();
        let mut mapping = vec![0, 0];
        let mut cmap = UnicodeCmap::new(Name(b"Adobe-Identity-UCS"), UNICODE);
        for (index, &(character, glyph)) in self.used.iter().enumerate() {
            mapping.extend_from_slice(&remapper.remap(glyph.0).to_be_bytes());
            cmap.pair((index + 1) as u16, character);
        }
        let subset = subsetter::subset(self.font.bytes(), 0, &remapper)
            .map_err(|_| invalid("Could not subset the bundled PDF font"))?;
        pdf.type0_font(type0)
            .base_font(name)
            .encoding_predefined(Name(b"Identity-H"))
            .descendant_font(descendant)
            .to_unicode(unicode);
        let scale = 1000.0 / f32::from(self.face.units_per_em());
        pdf.cid_font(descendant)
            .subtype(CidFontType::Type2)
            .base_font(name)
            .system_info(IDENTITY)
            .font_descriptor(descriptor)
            .cid_to_gid_map_stream(cid_map)
            .widths()
            .consecutive(
                1,
                self.used
                    .iter()
                    .map(|(_, glyph)| f32::from(self.advance(*glyph)) * scale),
            );
        self.write_descriptor(pdf, descriptor, name, font_stream, scale);
        pdf.stream(font_stream, &deflate(&subset)?)
            .filter(pdf_writer::Filter::FlateDecode)
            .pair(Name(b"Length1"), subset.len() as i32);
        pdf.stream(cid_map, &deflate(&mapping)?)
            .filter(pdf_writer::Filter::FlateDecode);
        pdf.stream(unicode, &deflate(&cmap.finish())?)
            .filter(pdf_writer::Filter::FlateDecode);
        Ok(Some(type0))
    }

    fn write_descriptor(&self, pdf: &mut Pdf, id: Ref, name: Name<'_>, stream: Ref, scale: f32) {
        let bounds = self.face.global_bounding_box();
        let units = |value: i16| f32::from(value) * scale;
        let mut flags = FontFlags::SYMBOLIC;
        flags.set(FontFlags::FIXED_PITCH, self.face.is_monospaced());
        let cap = self
            .face
            .capital_height()
            .unwrap_or_else(|| self.face.ascender());
        let stem = if self.font == Font::Bold { 120.0 } else { 80.0 };
        let mut descriptor = pdf.font_descriptor(id);
        descriptor
            .name(name)
            .flags(flags)
            .bbox(Rect::new(
                units(bounds.x_min),
                units(bounds.y_min),
                units(bounds.x_max),
                units(bounds.y_max),
            ))
            .italic_angle(self.face.italic_angle())
            .ascent(units(self.face.ascender()))
            .descent(units(self.face.descender()))
            .cap_height(units(cap))
            .stem_v(stem)
            .font_file2(stream);
        descriptor.finish();
    }
}
