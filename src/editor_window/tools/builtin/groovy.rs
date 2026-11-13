//! Four-column Groovy indentation with opaque source ranges preserved.

use super::Output;

const UNSUPPORTED: &str = "Groovy indentation cannot be formatted safely with an incomplete or ambiguous string or comment.";
const SPACES: &str = "                                                                ";
const INDENT: usize = 4;
const MAX_NESTING: usize = 256;

pub(super) fn format(source: &str, output: &mut Output<'_>) -> Result<(), String> {
    let mut lexer = Lexer::default();
    for line in source.split_inclusive('\n') {
        let start = line.trim_start_matches([' ', '\t']);
        if lexer.opaque.is_empty() && !start.starts_with("#!") {
            let indent = line[..line.len() - start.len()]
                .bytes()
                .map(|byte| if byte == b'\t' { INDENT } else { 1 })
                .sum::<usize>();
            let mut indent = indent / INDENT * INDENT;
            while indent >= SPACES.len() {
                output.write(SPACES)?;
                indent -= SPACES.len();
            }
            output.write(&SPACES[..indent])?;
            output.write(start)?;
        } else {
            output.write(line)?;
        }
        lexer.line(line)?;
    }
    if lexer.opaque.is_empty() {
        Ok(())
    } else {
        Err(UNSUPPORTED.into())
    }
}

#[derive(Clone, Copy)]
enum Opaque {
    Comment,
    Quote { delimiter: u8, triple: bool },
    Slashy,
    DollarSlashy,
    Interpolation { group_count: usize },
}

#[derive(Default)]
struct Lexer {
    opaque: Vec<Opaque>,
    groups: Vec<Group>,
    after_operand: bool,
    member: bool,
    try_resources: bool,
}

struct Group {
    delimiter: u8,
    hides_newline: bool,
}

impl Lexer {
    fn push(&mut self, state: Opaque) -> Result<(), String> {
        if self.opaque.len() == MAX_NESTING {
            return Err(UNSUPPORTED.into());
        }
        self.opaque.push(state);
        Ok(())
    }

    fn line(&mut self, line: &str) -> Result<(), String> {
        let mut bytes = line.as_bytes();
        if self.opaque.is_empty() && bytes.starts_with(b"#!") {
            return Ok(());
        }
        while let Some(&byte) = bytes.first() {
            let consumed = match self.opaque.last().copied() {
                Some(Opaque::Comment) if bytes.starts_with(b"*/") => {
                    self.opaque.pop();
                    if bytes[2..].iter().all(u8::is_ascii_whitespace) {
                        self.newline();
                    }
                    2
                }
                Some(Opaque::Comment) => 1,
                Some(Opaque::Quote { delimiter, triple }) => {
                    self.quoted(bytes, delimiter, triple)?
                }
                Some(Opaque::Slashy) if bytes.starts_with(b"\\/") => 2,
                Some(Opaque::Slashy) if byte == b'/' => self.close_string(1),
                Some(Opaque::DollarSlashy) if bytes.starts_with(b"/$") => self.close_string(2),
                Some(Opaque::DollarSlashy) if bytes.starts_with(b"$/$") => 3,
                Some(Opaque::DollarSlashy)
                    if bytes.starts_with(b"$$") || bytes.starts_with(b"$/") =>
                {
                    2
                }
                Some(Opaque::Slashy | Opaque::DollarSlashy) => self.interpolation(bytes)?,
                Some(Opaque::Interpolation { group_count })
                    if byte == b'}' && self.groups.len() == group_count + 1 =>
                {
                    self.opaque.pop();
                    self.groups.pop();
                    1
                }
                _ => self.code(bytes)?,
            };
            bytes = &bytes[consumed..];
        }
        Ok(())
    }

    fn close_string(&mut self, len: usize) -> usize {
        self.opaque.pop();
        self.token(true);
        len
    }

    fn interpolation(&mut self, bytes: &[u8]) -> Result<usize, String> {
        if bytes.starts_with(b"${") {
            self.push(Opaque::Interpolation {
                group_count: self.groups.len(),
            })?;
            self.open_group(b'{')?;
            return Ok(2);
        }
        Ok(1)
    }

    fn quoted(&mut self, bytes: &[u8], delimiter: u8, triple: bool) -> Result<usize, String> {
        let first = bytes[0];
        if first == b'\\' {
            return Ok(escaped_width(bytes));
        }
        if first == delimiter {
            if !triple {
                return Ok(self.close_string(1));
            }
            let count = bytes.iter().take_while(|&&byte| byte == delimiter).count();
            return Ok(match count {
                3..=5 => self.close_string(count),
                6.. => self.close_string(3),
                _ => count,
            });
        }
        if !triple && matches!(first, b'\r' | b'\n') {
            return Err(UNSUPPORTED.into());
        }
        if delimiter == b'"' {
            return self.interpolation(bytes);
        }
        Ok(1)
    }

    fn token(&mut self, after_operand: bool) {
        self.after_operand = after_operand;
        self.member = false;
        self.try_resources = false;
    }

    fn newline(&mut self) {
        if !self.groups.last().is_some_and(|group| group.hides_newline) {
            self.token(false);
        }
    }

    fn open_group(&mut self, delimiter: u8) -> Result<(), String> {
        if self.groups.len() == MAX_NESTING {
            return Err(UNSUPPORTED.into());
        }
        self.groups.push(Group {
            delimiter,
            hides_newline: delimiter == b'[' || (delimiter == b'(' && !self.try_resources),
        });
        self.token(false);
        Ok(())
    }

    fn code(&mut self, bytes: &[u8]) -> Result<usize, String> {
        let first = bytes[0];
        let rest = &bytes[1..];
        match first {
            b' ' | b'\t' => Ok(1),
            b'\r' | b'\n' => {
                if first == b'\r' && !rest.starts_with(b"\n") {
                    return Err("Groovy indentation supports LF and CRLF line endings.".into());
                }
                self.newline();
                Ok(1)
            }
            b'\\' if matches!(rest, b"\n" | b"\r\n") => Ok(bytes.len()),
            b'/' if rest.starts_with(b"/") => {
                self.newline();
                Ok(bytes.len())
            }
            b'/' if rest.starts_with(b"*") => {
                self.push(Opaque::Comment)?;
                Ok(2)
            }
            b'$' if rest.starts_with(b"/") => {
                self.push(Opaque::DollarSlashy)?;
                Ok(2)
            }
            b'/' if !self.after_operand => {
                self.push(Opaque::Slashy)?;
                Ok(1)
            }
            b'\'' | b'"' => {
                let triple = rest.starts_with(&[first; 2]);
                self.push(Opaque::Quote {
                    delimiter: first,
                    triple,
                })?;
                Ok(if triple { 3 } else { 1 })
            }
            b'(' | b'[' | b'{' => {
                self.open_group(first)?;
                Ok(1)
            }
            b')' | b']' | b'}' => {
                let opening = match first {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if self
                    .groups
                    .last()
                    .is_some_and(|group| group.delimiter == opening)
                {
                    self.groups.pop();
                } else if !self.opaque.is_empty() {
                    return Err(UNSUPPORTED.into());
                }
                self.token(true);
                Ok(1)
            }
            b'+' | b'-' if rest.first() == Some(&first) => {
                self.token(true);
                Ok(2)
            }
            first if word_byte(first) => {
                let len = bytes.iter().take_while(|&&byte| word_byte(byte)).count();
                let word = &bytes[..len];
                let try_resources = !self.member && word == b"try";
                self.token(self.member || !expects_operand(word));
                self.try_resources = try_resources;
                Ok(len)
            }
            b'.' if rest.starts_with(b"&") => {
                self.token(false);
                self.member = true;
                Ok(2)
            }
            b'.' if rest.starts_with(b".") => {
                self.token(false);
                Ok(if rest.starts_with(b"..") { 3 } else { 2 })
            }
            _ => {
                self.token(false);
                self.member = matches!(first, b'.' | b'@') || bytes.starts_with(b"::");
                Ok(if bytes.starts_with(b"::") { 2 } else { 1 })
            }
        }
    }
}

fn expects_operand(word: &[u8]) -> bool {
    matches!(
        word,
        b"assert"
            | b"await"
            | b"case"
            | b"defer"
            | b"do"
            | b"else"
            | b"in"
            | b"return"
            | b"throw"
            | b"try"
            | b"yield"
    )
}

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

fn escaped_width(bytes: &[u8]) -> usize {
    if bytes.starts_with(b"\\\r\n") {
        3
    } else {
        bytes.len().min(2)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::super::{CANCELLED, Format, TOO_LARGE};
    use super::UNSUPPORTED;

    fn format(source: &str) -> String {
        Format::Groovy
            .run(source, 8192, &AtomicBool::new(false))
            .unwrap()
    }

    #[test]
    fn groovy_formatter_matches_four_column_java_indentation() {
        let source = "#!/usr/bin/env groovy\r\n  def café = 8\r\n\t  def quotient = café++ / 2\r\n   \t  println(quotient)  \r\n\t \r\n  done()";
        let expected = "#!/usr/bin/env groovy\r\ndef café = 8\r\n    def quotient = café++ / 2\r\n        println(quotient)  \r\n    \r\ndone()";
        assert_eq!(format(source), expected);
        assert_eq!(format(expected), expected);
    }

    #[test]
    fn groovy_formatter_preserves_multiline_strings_comments_and_interpolation() {
        let source = concat!(
            "\t  def literal = '''line one\r\n",
            "\t   line two /* literal */\r\n",
            "   '''\r\n",
            "\t  def g = \"\"\"outer ${ [name: \"inner ${1 + 2}\"] }\n",
            "\t  gstring\n",
            "    \"\"\"\n",
            "   def pattern = /first\\/part ${ { -> 12 / 2 }() }\n",
            "\t  second/\n",
            "\t def literalDollars = $/first $/$$ and $$$/\n",
            "\t  second /$\n",
            "  /* This is a comment\n",
            "\t   body / $/\n",
            "  */\n",
            "\t  def continued = 'first\\\n",
            "\t   second'\n",
            "\t  println g // unchanged \"\"\"\n",
        );
        let expected = source
            .replace("\t  def literal =", "    def literal =")
            .replace("\t  def g =", "    def g =")
            .replace("   def pattern =", "def pattern =")
            .replace("\t def literalDollars =", "    def literalDollars =")
            .replace("  /* This is a comment", "/* This is a comment")
            .replace("\t  def continued =", "    def continued =")
            .replace("\t  println g", "    println g");
        assert_eq!(format(source), expected);
        assert_eq!(format(&expected), expected);
    }

    #[test]
    fn groovy_formatter_handles_division_and_slashy_operand_positions() {
        let source = concat!(
            "   def result = (20 /* divisor */\n",
            "\t / 5)\n",
            " \tdef regex = /=leading equals\n",
            "\t   preserved/\n",
            "\t return /first\n",
            "\t  second/\n",
        );
        assert_eq!(
            format(source),
            concat!(
                "def result = (20 /* divisor */\n",
                "    / 5)\n",
                "    def regex = /=leading equals\n",
                "\t   preserved/\n",
                "    return /first\n",
                "\t  second/\n",
            )
        );
    }

    #[test]
    fn groovy_formatter_rejects_open_opaque_ranges_and_obeys_editor_limits() {
        let active = AtomicBool::new(false);
        for source in ["\tdef s = '''\n\tunfinished", "\t/* open", "def s = $/\n"] {
            assert_eq!(
                Format::Groovy.run(source, 100, &active).unwrap_err(),
                UNSUPPORTED
            );
        }
        assert_eq!(
            Format::Groovy.run("\tx", 3, &active).unwrap_err(),
            TOO_LARGE
        );
        assert_eq!(
            Format::Groovy
                .run("\tx", 100, &AtomicBool::new(true))
                .unwrap_err(),
            CANCELLED,
        );
    }
}
