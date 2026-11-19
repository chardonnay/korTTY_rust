//! Numeric literals and their JavaScript property names.

use std::sync::LazyLock;

use num_bigint::BigUint;
use regex::Regex;

use super::destructure::Selection;
use super::{Token, after_group};

pub(super) fn literal(tokens: &[Token<'_>]) -> Option<f64> {
    static DECIMAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:(?:0|[1-9](?:_?[0-9])*)(?:\.(?:[0-9](?:_?[0-9])*)?)?|\.[0-9](?:_?[0-9])*)(?:[eE][+-]?[0-9](?:_?[0-9])*)?$").expect("static numeric pattern")
    });
    let (sign, raw) = literal_parts(tokens)?;
    let value = match base_parts(&raw) {
        (10, raw) if DECIMAL.is_match(raw) => raw.replace('_', "").parse().ok()?,
        (10, _) => return None,
        (base, raw) => radix(raw, base)?,
    };
    value
        .is_finite()
        .then_some(if sign == "-" { -value } else { value })
}

fn literal_parts<'source>(tokens: &[Token<'source>]) -> Option<(&'source str, String)> {
    let (sign, digits) = match tokens {
        [sign, rest @ ..] if matches!(sign.text, "+" | "-") => (sign.text, rest),
        rest => ("", rest),
    };
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    let mut raw = String::new();
    let mut end = digits[0].start;
    for token in digits {
        if token.start != end || raw.len() + token.text.len() > 128 {
            return None;
        }
        raw.push_str(token.text);
        end += token.text.len();
    }
    Some((sign, raw))
}

fn base_parts(raw: &str) -> (u32, &str) {
    match raw.get(..2) {
        Some("0x" | "0X") => (16, &raw[2..]),
        Some("0o" | "0O") => (8, &raw[2..]),
        Some("0b" | "0B") => (2, &raw[2..]),
        _ => (10, raw),
    }
}

pub(super) fn bigint_name(tokens: &[Token<'_>]) -> Option<String> {
    let (sign, raw) = literal_parts(tokens)?;
    let (base, digits) = base_parts(raw.strip_suffix('n')?);
    if sign == "+"
        || (base == 10 && digits.len() > 1 && digits.starts_with('0'))
        || !valid_separators(digits)
        || !digits
            .bytes()
            .all(|byte| byte == b'_' || char::from(byte).is_digit(base))
    {
        return None;
    }
    let parsed = BigUint::parse_bytes(digits.replace('_', "").as_bytes(), base)?;
    let mut name = parsed.to_str_radix(10);
    if sign == "-" && name != "0" {
        name.insert(0, '-');
    }
    Some(name)
}

pub(super) fn string_conversion<'tokens, 'source>(
    tokens: &'tokens [Token<'source>],
) -> Option<&'tokens [Token<'source>]> {
    let [receiver @ .., dot, method, open, close] = tokens else {
        return None;
    };
    if (dot.text, method.text, open.text, close.text) != (".", "toString", "(", ")") {
        return None;
    }
    if receiver.first()?.text == "(" && after_group(receiver, "(", ")")? == receiver.len() {
        Some(&receiver[1..receiver.len() - 1])
    } else if !matches!(receiver.first()?.text, "-" | "+") {
        Some(receiver)
    } else {
        None
    }
}

fn radix(raw: &str, base: u32) -> Option<f64> {
    if !valid_separators(raw) {
        return None;
    }
    let value = u64::from_str_radix(&raw.replace('_', ""), base).ok()?;
    // Preserve exact integer keys when converting based literals.
    (value < (1u64 << 53)).then_some(value as f64)
}

fn valid_separators(raw: &str) -> bool {
    !raw.is_empty() && !raw.starts_with('_') && !raw.ends_with('_') && !raw.contains("__")
}

pub(super) fn property_name(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let magnitude = value.abs();
    if (1e-6..1e21).contains(&magnitude) {
        return value.to_string();
    }
    let scientific = format!("{value:e}");
    if magnitude >= 1e21
        && let Some((coefficient, exponent)) = scientific.split_once('e')
    {
        return format!("{coefficient}e+{exponent}");
    }
    scientific
}

pub(super) fn is_numeric_name(name: &str) -> bool {
    name.parse::<f64>()
        .is_ok_and(|value| value.is_finite() && property_name(value) == name)
}

pub(super) fn selection(value: f64) -> Selection<'static> {
    if (0.0..4_294_967_295.0).contains(&value) && value.fract() == 0.0 {
        Selection::Index(value as usize)
    } else {
        Selection::NumericKey(property_name(value))
    }
}
