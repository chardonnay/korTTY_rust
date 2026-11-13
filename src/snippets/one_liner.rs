//! Compact and embedded commands for the languages supported by Java SnippetOneLiner.

use base64::{Engine as _, engine::general_purpose::STANDARD};

const MAX_ECHO_COMMAND_BYTES: usize = 48_000;
const HEREDOC_END: &str = "KORTTY_B64_EOF";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Compact,
    Embedded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Unsupported,
    Empty,
    NeedsEmbedded,
    Invalid,
}

impl Error {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Unsupported => "Local command conversion supports Bash, Python, Perl and Ruby.",
            Self::Empty => "Enter a script before copying it as a command.",
            Self::NeedsEmbedded => {
                "This script needs the embedded command to preserve its structure."
            }
            Self::Invalid => "The script contains a NUL byte.",
        }
    }

    pub(crate) fn offers_ai(self) -> bool {
        matches!(self, Self::Unsupported | Self::NeedsEmbedded)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Language {
    Bash,
    Python,
    Perl,
    Ruby,
}

impl Language {
    fn parse(language: &str) -> Option<Self> {
        match language.to_ascii_lowercase().as_str() {
            "bash" | "shell" => Some(Self::Bash),
            "python" => Some(Self::Python),
            "perl" => Some(Self::Perl),
            "ruby" => Some(Self::Ruby),
            _ => None,
        }
    }

    fn interpreter(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Python => "python3",
            Self::Perl => "perl",
            Self::Ruby => "ruby",
        }
    }
}

pub(crate) fn supported(language: &str) -> bool {
    Language::parse(language).is_some()
}

pub(crate) fn convert(source: &str, language: &str, mode: Mode) -> Result<String, Error> {
    if source.trim().is_empty() {
        return Err(Error::Empty);
    }
    if source.contains('\0') {
        return Err(Error::Invalid);
    }
    let language = Language::parse(language).ok_or(Error::Unsupported)?;
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned = logical_lines(&source, language);
    if cleaned.as_ref().is_some_and(Vec::is_empty) {
        return Err(Error::Empty);
    }
    match mode {
        Mode::Embedded => {
            let script = cleaned.map_or(source, |lines| lines.join("\n"));
            Ok(embedded(&script, language))
        }
        Mode::Compact => compact(&source, cleaned.as_deref(), language),
    }
}

fn embedded(source: &str, language: Language) -> String {
    let payload = STANDARD.encode(source);
    let interpreter = language.interpreter();
    let suffix = format!("' | base64 -d | {interpreter}");
    if "echo '".len() + payload.len() + suffix.len() <= MAX_ECHO_COMMAND_BYTES {
        return format!("echo '{payload}{suffix}");
    }
    let mut command = format!("base64 -d <<'{HEREDOC_END}' | {interpreter}\n");
    // Wrapping the ASCII payload also keeps individual pasted lines short.
    for start in (0..payload.len()).step_by(76) {
        command.push_str(&payload[start..(start + 76).min(payload.len())]);
        command.push('\n');
    }
    command.push_str(HEREDOC_END);
    command.push('\n');
    command
}

fn compact(source: &str, lines: Option<&[String]>, language: Language) -> Result<String, Error> {
    if language == Language::Bash {
        return lines.map(shell_commands).ok_or(Error::NeedsEmbedded);
    }
    if lines.is_none_or(|lines| lines.iter().any(|line| structured(line, language))) {
        let script = lines.map_or_else(|| source.to_owned(), |lines| lines.join("\n"));
        if script.lines().any(|line| {
            matches!(line.trim(), "__END__" | "__DATA__") && language != Language::Python
        }) {
            return Err(Error::NeedsEmbedded);
        }
        let literal = double_quoted_literal(&script, language);
        let (flag, program) = match language {
            Language::Python => ("-c", format!("exec({literal})")),
            Language::Perl => ("-e", format!("eval {literal}; die $@ if $@")),
            Language::Ruby => ("-e", format!("eval({literal})")),
            Language::Bash => unreachable!("handled above"),
        };
        return Ok(format!(
            "{} {flag} {}",
            language.interpreter(),
            shell_quote(&program)
        ));
    }
    let code = lines
        .expect("simple statements have logical lines")
        .iter()
        .map(|line| line.trim().trim_end_matches(';'))
        .collect::<Vec<_>>()
        .join("; ");
    if language == Language::Python {
        Ok(code)
    } else {
        Ok(format!(
            "{} -e {}",
            language.interpreter(),
            shell_quote(&code)
        ))
    }
}

fn structured(line: &str, language: Language) -> bool {
    let line = line.trim_start();
    let first = line.split_ascii_whitespace().next().unwrap_or_default();
    match language {
        Language::Python => {
            line.starts_with('@')
                || matches!(
                    first,
                    "def"
                        | "async"
                        | "class"
                        | "if"
                        | "elif"
                        | "else:"
                        | "for"
                        | "while"
                        | "try:"
                        | "except"
                        | "with"
                        | "match"
                )
        }
        Language::Perl => matches!(first, "sub" | "package" | "__END__" | "__DATA__"),
        Language::Ruby => {
            line.ends_with(" do")
                || matches!(
                    first,
                    "def"
                        | "class"
                        | "module"
                        | "if"
                        | "unless"
                        | "for"
                        | "while"
                        | "begin"
                        | "case"
                        | "__END__"
                )
        }
        Language::Bash => false,
    }
}

fn shell_commands(lines: &[String]) -> String {
    let mut command = String::new();
    let mut previous = "";
    for line in lines {
        if !command.is_empty() {
            let continues = previous.ends_with(['|', '&', ';'])
                || matches!(previous, "then" | "do" | "else")
                || previous.ends_with("; then")
                || previous.ends_with("; do");
            command.push_str(if continues { " " } else { "; " });
        }
        let line = line.trim();
        command.push_str(line);
        previous = line;
    }
    command
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn double_quoted_literal(value: &str, language: Language) -> String {
    let mut literal = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            '$' | '@' if language == Language::Perl => {
                literal.push('\\');
                literal.push(character);
            }
            '#' if language == Language::Ruby => literal.push_str("\\#"),
            _ => literal.push(character),
        }
    }
    literal.push('"');
    literal
}

fn logical_lines(source: &str, language: Language) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    let mut pending = String::new();
    for raw in source.split('\n') {
        let (body, continues) = without_comment(raw, language)?;
        if pending.ends_with(char::is_whitespace) && body.starts_with(char::is_whitespace) {
            pending.push_str(body.trim_start());
        } else {
            pending.push_str(body);
        }
        if !continues && !pending.trim().is_empty() {
            lines.push(std::mem::take(&mut pending).trim_end().to_owned());
        } else if !continues {
            pending.clear();
        }
    }
    pending.is_empty().then_some(lines)
}

fn without_comment(line: &str, language: Language) -> Option<(&str, bool)> {
    let bytes = line.as_bytes();
    let mut position = 0;
    let mut depth = 0i32;
    while let Some(&byte) = bytes.get(position) {
        let tail = &bytes[position..];
        match byte {
            b'\'' | b'"' => {
                position = quoted_end(bytes, position, language)?;
                continue;
            }
            b'\\' => {
                if tail[1..].iter().all(u8::is_ascii_whitespace) {
                    return Some((&line[..position], true));
                }
                position += 2;
                continue;
            }
            b'#' if language == Language::Perl && position > 0 && bytes[position - 1] == b'$' => {}
            b'#' if language != Language::Bash
                || position == 0
                || b" \t;&|()<>".contains(&bytes[position - 1]) =>
            {
                return (depth == 0).then(|| (line[..position].trim_end(), false));
            }
            b'<' if tail.starts_with(b"<<") => return None,
            b'\x60' if language != Language::Python => return None,
            b'/' | b'%' if matches!(language, Language::Perl | Language::Ruby) => return None,
            b'q' if language == Language::Perl && starts_perl_quote(tail) => return None,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        position += 1;
    }
    (depth == 0).then(|| (line.trim_end(), false))
}

fn starts_perl_quote(tail: &[u8]) -> bool {
    let operator = tail
        .iter()
        .take_while(|byte| byte.is_ascii_alphabetic())
        .count();
    matches!(&tail[..operator], b"q" | b"qq" | b"qw" | b"qx" | b"qr")
        && tail.get(operator).is_some_and(u8::is_ascii_punctuation)
}

fn quoted_end(bytes: &[u8], start: usize, language: Language) -> Option<usize> {
    let quote = bytes[start];
    let triple = language == Language::Python
        && bytes
            .get(start..start + 3)
            .is_some_and(|part| part == [quote; 3]);
    let width = if triple { 3 } else { 1 };
    let mut position = start + width;
    while let Some(&byte) = bytes.get(position) {
        if byte == b'\\' && !(language == Language::Bash && quote == b'\'') {
            position += 2;
            continue;
        }
        if byte == quote
            && (!triple
                || bytes
                    .get(position..position + 3)
                    .is_some_and(|part| part == [quote; 3]))
        {
            return Some(position + width);
        }
        position += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_one_liner_compacts_the_reference_shell_and_interpreter_cases() {
        for (language, source, expected) in [
            (
                "bash",
                "echo start\n# hide\necho mid",
                "echo start; echo mid",
            ),
            ("shell", "echo hello \\\n  world", "echo hello world"),
            (
                "bash",
                "echo a |\ncat\ntrue &&\necho ok",
                "echo a | cat; true && echo ok",
            ),
            (
                "bash",
                "if [ -f file ]; then\n echo ok\nelse\n echo missing\nfi",
                "if [ -f file ]; then echo ok; else echo missing; fi",
            ),
            ("bash", "echo $# a#b # comment", "echo $# a#b"),
            ("bash", "echo then\necho done", "echo then; echo done"),
            ("python", "x = 1 # ignore\nprint(x)", "x = 1; print(x)"),
            (
                "perl",
                "my $n = 1\nprint $n",
                "perl -e 'my $n = 1; print $n'",
            ),
            (
                "ruby",
                "puts 'path\\temp#literal' # ignore",
                "ruby -e 'puts '\\''path\\temp#literal'\\'''",
            ),
        ] {
            assert_eq!(convert(source, language, Mode::Compact).unwrap(), expected);
        }
        assert!(
            convert("def hello():\n    print('hi')", "python", Mode::Compact)
                .unwrap()
                .starts_with("python3 -c 'exec(")
        );
        assert!(
            convert("sub say_hi {\n    print $x;\n}", "perl", Mode::Compact)
                .unwrap()
                .contains(r"\$x")
        );
    }

    #[test]
    fn snippet_one_liner_embeds_utf8_and_keeps_ambiguous_source_losslessly() {
        let command = convert(
            "# skip\r\nprint('Grüße # visible') # hidden",
            "python",
            Mode::Embedded,
        )
        .unwrap();
        let encoded = command
            .strip_prefix("echo '")
            .unwrap()
            .strip_suffix("' | base64 -d | python3")
            .unwrap();
        assert_eq!(
            STANDARD.decode(encoded).unwrap(),
            "print('Grüße # visible')".as_bytes()
        );
        let script = "cat <<'MARK'\n# literal\n\nMARK\n";
        assert_eq!(
            convert(script, "bash", Mode::Compact),
            Err(Error::NeedsEmbedded)
        );
        let command = convert(script, "bash", Mode::Embedded).unwrap();
        let encoded = command
            .strip_prefix("echo '")
            .unwrap()
            .strip_suffix("' | base64 -d | bash")
            .unwrap();
        assert_eq!(STANDARD.decode(encoded).unwrap(), script.as_bytes());

        let script = format!("printf '%s' '{}'", "x".repeat(50_000));
        let command = convert(&script, "bash", Mode::Embedded).unwrap();
        let mut lines = command.lines();
        assert_eq!(lines.next(), Some("base64 -d <<'KORTTY_B64_EOF' | bash"));
        let mut lines: Vec<_> = lines.collect();
        assert_eq!(lines.pop(), Some(HEREDOC_END));
        assert!(lines.iter().all(|line| line.len() <= 76));
        assert_eq!(STANDARD.decode(lines.concat()).unwrap(), script.as_bytes());
    }
}
