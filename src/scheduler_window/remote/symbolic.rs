//! Explicit-class symbolic modes evaluated against verified server metadata.

use std::fmt;

use serde::{Deserialize, Serialize};

const MAX_MODE_BYTES: usize = 128;
const INVALID: &str =
    "Enter an octal mode or a symbolic mode with explicit u, g, o or a (for example u+rw,o-w)";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String")]
pub(crate) struct SymbolicMode(String);

impl TryFrom<String> for SymbolicMode {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if text.is_empty() || text.len() > MAX_MODE_BYTES || !text.is_ascii() {
            return Err(INVALID);
        }
        let candidate = Self(text);
        candidate.adjust(0, false)?;
        Ok(candidate)
    }
}

impl fmt::Display for SymbolicMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl SymbolicMode {
    pub(crate) fn ordinary_only(&self) -> bool {
        !self.0.contains(['s', 't'])
    }

    pub(crate) fn adjust(&self, old: u32, directory: bool) -> Result<u32, &'static str> {
        const SET_ID: u32 = 0o6000;
        let mut result = old & 0o7777;
        for change in self.0.split(',') {
            let start = change.find(['+', '-', '=']).ok_or(INVALID)?;
            let (who, mut actions) = change.split_at(start);
            let classes = class_mask(who)?;
            while !actions.is_empty() {
                let (operation, rest) = actions.split_at(1);
                let end = rest.find(['+', '-', '=']).unwrap_or(rest.len());
                let (permissions, next) = rest.split_at(end);
                let bits = permission_bits(permissions, result, directory)? & classes;
                // Symbolic assignment preserves a directory's set-ID bits unless
                // the expression explicitly selects them with s.
                let preserve = if directory { SET_ID & !bits } else { 0 };
                result = match operation {
                    "+" => result | bits,
                    "-" => result & !bits,
                    "=" => (result & !(classes & !preserve)) | bits,
                    _ => return Err(INVALID),
                };
                actions = next;
            }
        }
        Ok(result)
    }
}

fn class_mask(who: &str) -> Result<u32, &'static str> {
    if who.is_empty() {
        return Err(INVALID);
    }
    let mut mask = 0;
    for class in who.bytes() {
        mask |= match class {
            b'u' => 0o4700,
            b'g' => 0o2070,
            b'o' => 0o1007,
            b'a' => 0o7777,
            _ => return Err(INVALID),
        };
    }
    Ok(mask)
}

fn permission_bits(text: &str, mode: u32, directory: bool) -> Result<u32, &'static str> {
    let copied = match text.as_bytes() {
        b"u" => Some(mode >> 6),
        b"g" => Some(mode >> 3),
        b"o" => Some(mode),
        _ => None,
    };
    if let Some(bits) = copied {
        return Ok((bits & 7) * 0o111);
    }
    let mut bits = 0;
    for permission in text.bytes() {
        bits |= match permission {
            b'r' => 0o444,
            b'w' => 0o222,
            b'x' => 0o111,
            b'X' if directory || mode & 0o111 != 0 => 0o111,
            b'X' => 0,
            b's' => 0o6000,
            b't' => 0o1000,
            _ => return Err(INVALID),
        };
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_permissions_apply_in_order_and_respect_entry_type() {
        for (expression, old, directory, expected) in [
            ("u+rw,o-w", 0o426, false, 0o624),
            ("g=u,u-x+rw,o=", 0o750, false, 0o670),
            ("a+X", 0o640, false, 0o640),
            ("a+X", 0o640, true, 0o751),
            ("g=u,o=g", 0o704, false, 0o777),
            ("g=rw", 0o2755, true, 0o2765),
            ("g-s,g=rw", 0o2755, true, 0o765),
            ("a=rw,o+t", 0o7000, false, 0o1666),
        ] {
            let mode = SymbolicMode::try_from(expression.to_owned()).unwrap();
            assert_eq!(mode.adjust(old, directory), Ok(expected), "{expression}");
        }
        for invalid in [
            "+x",
            "u+x,",
            ",u+x",
            "u+x;id",
            "u+x g+w",
            "u+ug",
            "u+gr",
            "--reference",
        ] {
            assert!(
                SymbolicMode::try_from(invalid.to_owned()).is_err(),
                "{invalid}"
            );
        }
        assert!(serde_json::from_str::<SymbolicMode>(r#""+x""#).is_err());
    }
}
