//! Compile Java Unicode case-insensitive literals and supported class members.

use alloc::collections::BTreeSet;
use alloc::string::String;
use core::fmt::Write as _;
use regex_syntax::ast::{parse::Parser, Ast, ClassSet, ClassSetItem};
use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};

use super::java_case::{fold, map, CaseRange, FOLD_RANGES, UPPER_RANGES};

/// Whether Java uses its Unicode case predicate for this single literal.
pub fn java_cased_literal(literal: char) -> bool {
    map(u32::from(literal), UPPER_RANGES) != fold(literal)
}

/// Render a Java literal atom or a contiguous run of Java literals.
pub fn write_java_unicode_literals(output: &mut String, literals: &[char]) {
    if literals.is_empty() {
        return;
    }
    output.push_str("(?-i:");
    let mut variants = BTreeSet::new();
    for &literal in literals {
        let code = u32::from(literal);
        let folded = fold(literal);
        if literals.len() > 1 || java_cased_literal(literal) {
            add_folded_variants(folded, u32::MAX, &mut variants);
        }
        variants.insert(code);
        if variants.len() > 1 {
            output.push('[');
        }
        for &variant in &variants {
            write!(output, r"\x{{{:x}}}", variant).expect("string write");
        }
        if variants.len() > 1 {
            output.push(']');
        }
        variants.clear();
    }
    output.push(')');
}

/// Expand class members before applying any negation. The input is produced
/// by the korTTY Java translator and contains only Rust class syntax.
pub fn java_unicode_class(pattern: &str) -> Option<String> {
    let ast = Parser::new().parse(pattern).ok()?;
    let Ast::ClassBracketed(class) = &ast else {
        return None;
    };
    if class.negated {
        return None;
    }
    let ClassSet::Item(items) = &class.kind else {
        return None;
    };
    let mut additions = BTreeSet::new();
    add_variants(items, &mut additions);

    if additions.is_empty() {
        return Some(String::from(pattern));
    }
    // Keep the source operand nested so that its sets remain intact.
    let mut result = String::from("[");
    result.push_str(pattern);
    let ranges = additions
        .into_iter()
        .filter_map(char::from_u32)
        .map(|ch| ClassUnicodeRange::new(ch, ch));
    for range in ClassUnicode::new(ranges).ranges() {
        write!(result, r"\x{{{:x}}}", u32::from(range.start())).ok()?;
        if range.start() != range.end() {
            write!(result, r"-\x{{{:x}}}", u32::from(range.end())).ok()?;
        }
    }
    result.push(']');
    Some(result)
}

fn add_variants(item: &ClassSetItem, additions: &mut BTreeSet<u32>) {
    match item {
        ClassSetItem::Literal(literal) => {
            let code = u32::from(literal.c);
            let upper = map(code, UPPER_RANGES);
            let folded = fold(literal.c);
            if upper != folded {
                // Java's Latin-1 bit set has a handful of characters whose
                // equivalence classes extend outside Latin-1.
                let limit = if code <= 0xff
                    && !matches!(
                        literal.c,
                        'ÿ' | 'µ' | 'I' | 'i' | 'S' | 's' | 'K' | 'k' | 'Å' | 'å'
                    ) {
                    0xff
                } else {
                    u32::MAX
                };
                add_folded_variants(folded, limit, additions);
            }
        }
        ClassSetItem::Range(range) => {
            let first = u32::from(range.start.c);
            let last = u32::from(range.end.c);
            add_sources(UPPER_RANGES, first, last, u32::MAX, additions);
            add_sources(FOLD_RANGES, first, last, u32::MAX, additions);
        }
        ClassSetItem::Union(union) => {
            for item in &union.items {
                add_variants(item, additions);
            }
        }
        // Bracketed items are produced by the translator for shorthands and
        // properties. Their predicates already define the intended set.
        _ => {}
    }
}

fn add_folded_variants(folded: u32, limit: u32, additions: &mut BTreeSet<u32>) {
    additions.insert(folded);
    add_sources(FOLD_RANGES, folded, folded, limit, additions);
}

fn add_sources(
    ranges: &[CaseRange],
    first: u32,
    last: u32,
    limit: u32,
    additions: &mut BTreeSet<u32>,
) {
    for &(start, end, stride, delta) in ranges {
        let end = end.min(limit);
        if start > end
            || end.wrapping_add_signed(delta) < first
            || start.wrapping_add_signed(delta) > last
        {
            continue;
        }
        for code in (start..=end).step_by(stride as usize) {
            if (first..=last).contains(&code.wrapping_add_signed(delta))
                && !(first..=last).contains(&code)
            {
                additions.insert(code);
            }
        }
    }
}
