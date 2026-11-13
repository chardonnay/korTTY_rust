use super::{Language, lines};

pub(super) struct Line {
    pub(super) code: String,
    pub(super) references: String,
}

#[derive(Default)]
struct State {
    block: Option<Block>,
    quote: Option<u8>,
    triple: bool,
    heredoc: Option<(String, bool)>,
    pod: bool,
    percent: Option<(u8, u8, usize)>,
}

enum Block {
    Slash,
    PowerShell,
    Lua(usize),
}

pub(super) fn mask(source: &str, language: Language) -> Vec<Line> {
    let mut state = State::default();
    lines(source)
        .map(|line| state.line(line, language))
        .collect()
}

impl State {
    fn line(&mut self, line: &str, language: Language) -> Line {
        let mut code = vec![b' '; line.len()];
        let mut references = code.clone();
        if language == Language::Perl && line.starts_with('=') {
            self.pod = !line.starts_with("=cut");
            return output(code, references);
        }
        if language == Language::Ruby && line.starts_with("=begin") {
            self.pod = true;
        }
        if language == Language::Ruby && line.starts_with("=end") {
            self.pod = false;
            return output(code, references);
        }
        if self.pod {
            return output(code, references);
        }
        if let Some((end, interpolate)) = &self.heredoc {
            if line.trim() == end {
                self.heredoc = None;
            } else if *interpolate {
                copy_interpolations(line.as_bytes(), &mut references, language);
            }
            return output(code, references);
        }
        let bytes = line.as_bytes();
        let mut offset = 0;
        while offset < bytes.len() {
            if let Some((open, close, depth)) = self.percent {
                if bytes[offset] == b'\\' {
                    offset = (offset + 2).min(bytes.len());
                    continue;
                }
                let depth = if open != close && bytes[offset] == open {
                    depth + 1
                } else {
                    depth
                };
                let depth = if bytes[offset] == close {
                    depth.saturating_sub(1)
                } else {
                    depth
                };
                self.percent = (depth > 0).then_some((open, close, depth));
                offset += 1;
                continue;
            }
            if let Some(block) = &self.block {
                let end = match block {
                    Block::Slash => bytes[offset..].starts_with(b"*/").then_some(2),
                    Block::PowerShell => bytes[offset..].starts_with(b"#>").then_some(2),
                    Block::Lua(level) => lua_close(&bytes[offset..], *level),
                };
                if let Some(length) = end {
                    self.block = None;
                    offset += length;
                } else {
                    offset += 1;
                }
                continue;
            }
            if let Some(quote) = self.quote {
                let escape = if language == Language::PowerShell {
                    b'`'
                } else {
                    b'\\'
                };
                if bytes[offset] == escape {
                    offset = (offset + 2).min(bytes.len());
                    continue;
                }
                if self.triple && bytes[offset..].starts_with(&[quote; 3]) {
                    self.quote = None;
                    self.triple = false;
                    offset += 3;
                    continue;
                }
                if !self.triple && bytes[offset] == quote {
                    if language == Language::PowerShell
                        && quote == b'\''
                        && bytes.get(offset + 1) == Some(&quote)
                    {
                        offset += 2;
                        continue;
                    }
                    self.quote = None;
                    offset += 1;
                    continue;
                }
                if quote != b'\'' {
                    let count = interpolation(&bytes[offset..], language);
                    if count > 0 {
                        references[offset..offset + count]
                            .copy_from_slice(&bytes[offset..offset + count]);
                        offset += count;
                        continue;
                    }
                }
                offset += 1;
                continue;
            }
            let rest = &bytes[offset..];
            if language == Language::Ruby
                && let Some((length, open, close)) = percent_open(rest)
            {
                self.percent = Some((open, close, 1));
                offset += length;
                continue;
            }
            if (language.jvm() || language.javascript()) && rest.starts_with(b"//") {
                break;
            }
            if (language.jvm() || language.javascript()) && rest.starts_with(b"/*") {
                self.block = Some(Block::Slash);
                offset += 2;
                continue;
            }
            if language == Language::PowerShell && rest.starts_with(b"<#") {
                self.block = Some(Block::PowerShell);
                offset += 2;
                continue;
            }
            if language == Language::Lua && rest.starts_with(b"--") {
                if let Some((level, length)) = lua_open(&rest[2..]) {
                    self.block = Some(Block::Lua(level));
                    offset += 2 + length;
                    continue;
                }
                break;
            }
            if language == Language::Lua
                && let Some((level, length)) = lua_open(rest)
            {
                self.block = Some(Block::Lua(level));
                offset += length;
                continue;
            }
            if matches!(
                language,
                Language::Perl | Language::Ruby | Language::PowerShell
            ) && rest[0] == b'#'
            {
                break;
            }
            if language == Language::PowerShell
                && (rest.starts_with(b"@\"") || rest.starts_with(b"@'"))
                && rest[2..].iter().all(u8::is_ascii_whitespace)
            {
                self.heredoc = Some((format!("{}@", rest[1] as char), rest[1] == b'"'));
                break;
            }
            if matches!(language, Language::Perl | Language::Ruby)
                && rest.starts_with(b"<<")
                && let Some((length, end, interpolate)) = heredoc(rest)
            {
                self.heredoc = Some((end, interpolate));
                offset += length;
                continue;
            }
            if language == Language::Perl
                && (offset == 0
                    || !bytes[offset - 1].is_ascii_alphanumeric() && bytes[offset - 1] != b'_')
                && let Some(length) = perl_quote(rest)
            {
                offset += length;
                continue;
            }
            if matches!(rest[0], b'\'' | b'"') || language.javascript() && rest[0] == b'`' {
                self.quote = Some(rest[0]);
                self.triple = language.jvm() && rest.starts_with(&[rest[0]; 3]);
                offset += if self.triple { 3 } else { 1 };
                continue;
            }
            if matches!(
                language,
                Language::Ruby | Language::JavaScript | Language::TypeScript | Language::Groovy
            ) && rest[0] == b'/'
                && regex_position(bytes, offset)
            {
                offset += regex_length(rest);
                continue;
            }
            code[offset] = bytes[offset];
            references[offset] = bytes[offset];
            offset += 1;
        }
        // Ordinary C-family string literals cannot run onto the next physical line.
        if self.quote.is_some()
            && !self.triple
            && (language.jvm() || language.javascript())
            && self.quote != Some(b'`')
            && !line.ends_with('\\')
        {
            self.quote = None;
        }
        output(code, references)
    }
}

fn percent_open(bytes: &[u8]) -> Option<(usize, u8, u8)> {
    if bytes.first() != Some(&b'%') {
        return None;
    }
    let index = if bytes.get(1).is_some_and(|byte| b"qQrwWiIsx".contains(byte)) {
        2
    } else {
        1
    };
    let open = *bytes.get(index)?;
    if index == 1 && !b"([{<".contains(&open) {
        return None;
    }
    if open.is_ascii_alphanumeric() || open.is_ascii_whitespace() {
        return None;
    }
    let close = match open {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        b'<' => b'>',
        _ => open,
    };
    Some((index + 1, open, close))
}

fn output(code: Vec<u8>, references: Vec<u8>) -> Line {
    // Every removed byte became ASCII space, while copied UTF-8 bytes retain their sequence.
    Line {
        code: String::from_utf8(code).expect("source-preserving code mask"),
        references: String::from_utf8(references).expect("source-preserving reference mask"),
    }
}

fn interpolation(bytes: &[u8], language: Language) -> usize {
    if !matches!(
        language,
        Language::Perl | Language::PowerShell | Language::JavaScript | Language::TypeScript
    ) || bytes.first() != Some(&b'$')
    {
        return 0;
    }
    let mut end = 1;
    if bytes.get(end) == Some(&b'{') {
        end += 1;
    }
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || b"_:".contains(byte))
    {
        end += 1;
    }
    if end > 1 { end } else { 0 }
}

fn copy_interpolations(bytes: &[u8], refs: &mut [u8], language: Language) {
    let mut offset = 0;
    while offset < bytes.len() {
        let count = interpolation(&bytes[offset..], language);
        if count == 0 {
            offset += 1;
        } else {
            refs[offset..offset + count].copy_from_slice(&bytes[offset..offset + count]);
            offset += count;
        }
    }
}

fn lua_open(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let level = bytes[1..].iter().take_while(|&&byte| byte == b'=').count();
    (bytes.get(level + 1) == Some(&b'[')).then_some((level, level + 2))
}

fn lua_close(bytes: &[u8], level: usize) -> Option<usize> {
    (bytes.first() == Some(&b']')
        && bytes
            .get(1..level + 1)
            .is_some_and(|middle| middle.iter().all(|&byte| byte == b'='))
        && bytes.get(level + 1) == Some(&b']'))
    .then_some(level + 2)
}

fn heredoc(bytes: &[u8]) -> Option<(usize, String, bool)> {
    let mut start = 2;
    while bytes
        .get(start)
        .is_some_and(|byte| byte.is_ascii_whitespace() || b"-~".contains(byte))
    {
        start += 1;
    }
    let quote = bytes
        .get(start)
        .copied()
        .filter(|byte| b"'\"".contains(byte));
    if quote.is_some() {
        start += 1;
    }
    let mut end = start;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        end += 1;
    }
    if end == start {
        return None;
    }
    Some((
        end + usize::from(quote.is_some()),
        String::from_utf8(bytes[start..end].to_vec()).ok()?,
        quote != Some(b'\''),
    ))
}

fn perl_quote(bytes: &[u8]) -> Option<usize> {
    let prefix = [b"qq".as_slice(), b"qw", b"qx", b"qr", b"q"]
        .into_iter()
        .find(|prefix| bytes.starts_with(prefix))?;
    let open = *bytes.get(prefix.len())?;
    if open.is_ascii_alphanumeric() || open.is_ascii_whitespace() {
        return None;
    }
    let close = match open {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        b'<' => b'>',
        _ => open,
    };
    let mut depth = 1;
    let mut offset = prefix.len() + 1;
    while offset < bytes.len() {
        if bytes[offset] == b'\\' {
            offset += 2;
            continue;
        }
        if open != close && bytes[offset] == open {
            depth += 1;
        }
        if bytes[offset] == close {
            depth -= 1;
            if depth == 0 {
                return Some(offset + 1);
            }
        }
        offset += 1;
    }
    Some(bytes.len())
}

fn regex_position(bytes: &[u8], offset: usize) -> bool {
    let previous = bytes[..offset]
        .iter()
        .rfind(|byte| !byte.is_ascii_whitespace());
    previous.is_none_or(|&byte| b"=(:,[!&|?;{".contains(&byte))
}

fn regex_length(bytes: &[u8]) -> usize {
    let mut offset = 1;
    let mut class = false;
    while offset < bytes.len() {
        match bytes[offset] {
            b'\\' => {
                offset += 2;
                continue;
            }
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => return offset + 1,
            _ => {}
        }
        offset += 1;
    }
    bytes.len()
}
