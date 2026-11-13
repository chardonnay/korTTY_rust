//! Bounded text-only formatters that preserve source tokens.

use std::sync::atomic::{AtomicBool, Ordering};

const TOO_LARGE: &str = "The tool output exceeds the editor size limit.";
const CANCELLED: &str = "The editor tool was cancelled.";

mod groovy;

#[derive(Clone, Copy)]
pub(super) enum Format {
    Json,
    Ini,
    Groovy,
}

impl Format {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Json => "the built-in JSON formatter",
            Self::Ini => "the built-in INI and Properties formatter",
            Self::Groovy => "the built-in Groovy indentation formatter",
        }
    }

    pub(super) fn run(
        self,
        source: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<String, String> {
        let mut output = Output {
            text: String::with_capacity(source.len().min(limit)),
            limit,
            cancel,
        };
        match self {
            Self::Json => json(source, &mut output)?,
            Self::Ini => ini(source, &mut output)?,
            Self::Groovy => groovy::format(source, &mut output)?,
        }
        Ok(output.text)
    }
}

struct Output<'a> {
    text: String,
    limit: usize,
    cancel: &'a AtomicBool,
}

impl Output<'_> {
    fn write(&mut self, text: &str) -> Result<(), String> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(CANCELLED.into());
        }
        if text.len() > self.limit.saturating_sub(self.text.len()) {
            return Err(TOO_LARGE.into());
        }
        self.text.push_str(text);
        Ok(())
    }

    fn newline(&mut self, depth: usize) -> Result<(), String> {
        const INDENT: &str = match std::str::from_utf8(&[b' '; 256]) {
            Ok(spaces) => spaces,
            Err(_) => unreachable!(),
        };
        let spaces = depth.checked_mul(2).and_then(|size| INDENT.get(..size));
        self.write("\n")?;
        self.write(spaces.ok_or("JSON nesting exceeds the formatter's 128-level limit.")?)
    }
}

fn json(source: &str, output: &mut Output<'_>) -> Result<(), String> {
    // A borrowed RawValue validates the grammar without sorting keys, collapsing
    // duplicates, or converting strings and numeric literals into Rust values.
    let source: &serde_json::value::RawValue =
        serde_json::from_str(source).map_err(|error| format!("Invalid JSON. {error}"))?;
    let mut remaining = source.get();
    let mut depth = 0;
    let mut previous = None;
    while let Some(&first) = remaining.as_bytes().first() {
        let end = match first {
            b'{' | b'[' => {
                depth += 1;
                output.write(&remaining[..1])?;
                remaining = remaining[1..].trim_ascii_start();
                if !matches!(remaining.as_bytes().first(), Some(b'}' | b']')) {
                    output.newline(depth)?;
                }
                previous = Some(first);
                continue;
            }
            b'}' | b']' => {
                depth -= 1;
                if !matches!(previous, Some(b'{' | b'[')) {
                    output.newline(depth)?;
                }
                output.write(&remaining[..1])?;
                1
            }
            b',' => {
                output.write(",")?;
                output.newline(depth)?;
                1
            }
            b':' => {
                output.write(": ")?;
                1
            }
            b'"' => {
                let mut escaped = false;
                let end = remaining
                    .bytes()
                    .enumerate()
                    .skip(1)
                    .find(|&(_, byte)| {
                        let closing = !escaped && byte == b'"';
                        escaped = !escaped && byte == b'\\';
                        closing
                    })
                    .map(|(index, _)| index + 1)
                    .expect("validated JSON string");
                output.write(&remaining[..end])?;
                end
            }
            _ => {
                let end = remaining
                    .bytes()
                    .position(|byte| {
                        byte.is_ascii_whitespace() || matches!(byte, b',' | b'}' | b']')
                    })
                    .unwrap_or(remaining.len());
                output.write(&remaining[..end])?;
                end
            }
        };
        previous = Some(first);
        remaining = remaining[end..].trim_ascii_start();
    }
    output.write("\n")
}

fn ini(source: &str, output: &mut Output<'_>) -> Result<(), String> {
    let mut continued = false;
    for line in source.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let entry = body.trim_start_matches([' ', '\t']);
        let comment = entry.starts_with(['[', '#', ';', '!']);
        let continues = body.bytes().rev().take_while(|&byte| byte == b'\\').count() % 2 != 0;
        if !continued
            && !comment
            && let Some((left, _)) = entry.split_once('=')
        {
            let key = left.trim_end_matches([' ', '\t']);
            if !key.is_empty()
                && !key
                    .chars()
                    .any(|ch| ch.is_whitespace() || matches!(ch, '\\' | ':' | '\r'))
            {
                let end = body.len() - entry.len() + key.len();
                let separator = body.len() - entry.len() + left.len();
                output.write(&body[..end])?;
                output.write(" ")?;
                output.write(&line[separator..])?;
            } else {
                output.write(line)?;
            }
        } else {
            output.write(line)?;
        }
        continued = continues && (continued || !comment);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_editor_formatter_keeps_order_duplicate_keys_and_exact_literals() {
        let input = r#"{ "b": 1E+2000, "b": -0, "a": [ {}, [], "café \\\" x" ] }"#;
        let result = Format::Json
            .run(input, 1024, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            result,
            "{\n  \"b\": 1E+2000,\n  \"b\": -0,\n  \"a\": [\n    {},\n    [],\n    \"café \\\\\\\" x\"\n  ]\n}\n"
        );
        assert_eq!(
            Format::Json
                .run(&result, 1024, &AtomicBool::new(false))
                .unwrap(),
            result
        );
        assert!(
            Format::Json
                .run("{invalid}", 1024, &AtomicBool::new(false))
                .is_err()
        );
        assert!(
            Format::Json
                .run(input, input.len(), &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(
            Format::Json
                .run(input, 1024, &AtomicBool::new(true))
                .unwrap_err(),
            CANCELLED
        );
    }

    #[test]
    fn ini_editor_formatter_preserves_values_escapes_comments_and_continuations() {
        let input = "[section]\r\n# comment=\\\r\n  key\t= value  \r\ntrail=first\\\r\n  tail=second\r\nkey\\=escaped=value\r\nkey:literal=value\r\nblank=\r\n";
        let result = Format::Ini
            .run(input, 1024, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            result,
            "[section]\r\n# comment=\\\r\n  key = value  \r\ntrail =first\\\r\n  tail=second\r\nkey\\=escaped=value\r\nkey:literal=value\r\nblank =\r\n"
        );
        assert_eq!(
            Format::Ini
                .run(&result, 1024, &AtomicBool::new(false))
                .unwrap(),
            result
        );
    }
}
