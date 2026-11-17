//! Grapheme matching for Java patterns using reference Unicode 16.0 classes.

use crate::java_grapheme_data::RANGES;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Kind {
    Other,
    Cr,
    Lf,
    Control,
    Extend,
    Zwj,
    Regional,
    Prepend,
    SpacingMark,
    L,
    V,
    T,
    Lv,
    Lvt,
    Pictographic,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Indic {
    None,
    Consonant,
    Linker,
    Extend,
}

fn properties(ch: char) -> (Kind, Indic) {
    let code = u32::from(ch);
    if (0xAC00..=0xD7A3).contains(&code) {
        let kind = if (code - 0xAC00) % 28 == 0 {
            Kind::Lv
        } else {
            Kind::Lvt
        };
        return (kind, Indic::None);
    }
    let index = RANGES.partition_point(|&(_, last, _, _)| last < code);
    match RANGES.get(index) {
        Some(&(first, _, kind, indic)) if first <= code => (kind, indic),
        _ => (Kind::Other, Indic::None),
    }
}

fn joined(previous: Kind, next: Kind, starts_pictographic: bool, odd_regional: bool) -> bool {
    use Kind::*;
    match (previous, next) {
        (Cr, Lf) => true,
        (Cr | Lf | Control, _) | (_, Cr | Lf | Control) => false,
        (_, Extend | Zwj | SpacingMark) | (Prepend, _) => true,
        (L, L | V | Lv | Lvt) | (Lv | V, V | T) | (Lvt | T, T) => true,
        (Zwj, Pictographic) => starts_pictographic,
        (Regional, Regional) => odd_regional,
        _ => false,
    }
}

fn indic_conjunct(text: &str) -> Option<usize> {
    let mut linked = false;
    for (offset, ch) in text.char_indices() {
        match properties(ch).1 {
            Indic::Linker => linked = true,
            Indic::Extend => {}
            Indic::Consonant if linked => return Some(offset + ch.len_utf8()),
            _ => break,
        }
    }
    None
}

pub(super) fn cluster_len(text: &str) -> Option<usize> {
    let first = text.chars().next()?;
    let (mut previous, mut indic) = properties(first);
    let starts_pictographic = previous == Kind::Pictographic;
    let mut odd_regional = previous == Kind::Regional;
    let mut end = first.len_utf8();
    while let Some(ch) = text[end..].chars().next() {
        if indic == Indic::Consonant {
            if let Some(length) = indic_conjunct(&text[end..]) {
                end += length;
                continue;
            }
        }
        let (next, next_indic) = properties(ch);
        if !joined(previous, next, starts_pictographic, odd_regional) {
            break;
        }
        if next == Kind::Regional {
            odd_regional = !odd_regional;
        }
        previous = next;
        indic = next_indic;
        end += ch.len_utf8();
    }
    Some(end)
}
