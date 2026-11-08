//! Bounded lexical structure analysis with coordinates for the native editor.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::Path;

use iced::advanced::graphics::text::cosmic_text::LineIter;

use super::{Language, SourceState, declaration, identifier, leading_keyword};

const MAX_UTF16_UNITS: usize = 512_000;
const MAX_ENTRIES: usize = 2_000;
const MAX_HEADER_LINES: usize = 6;
const MAX_QUALIFIED_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Status {
    #[default]
    Ok,
    Unsupported,
    TooLarge,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Analysis {
    pub(crate) status: Status,
    pub(crate) nodes: Vec<Node>,
    pub(crate) variables: Vec<Variable>,
    pub(crate) limited: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) qualified: String,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) end_column: usize,
    pub(crate) end_line: usize,
    pub(crate) parent: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct Variable {
    pub(crate) name: String,
    pub(crate) scope: String,
    pub(crate) kind: &'static str,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) end_column: usize,
    pub(crate) uses: usize,
}

enum Syntax {
    Code(Language),
    Markup(Markup),
}

impl Syntax {
    fn detect(language: &str, source: &str) -> Option<Self> {
        Some(match language.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => Self::Code(Language::Rust),
            "python" | "python3" | "py" => Self::Code(Language::Python),
            "bash" | "sh" | "shell" | "shellscript" | "zsh" | "ksh" | "dash" | "ash" => {
                Self::Code(Language::Shell)
            }
            "markdown" | "md" => Self::Markup(Markup::Markdown),
            "asciidoctor" | "asciidoc" | "adoc" => Self::Markup(Markup::AsciiDoc),
            "" | "plain" | "text" | "plaintext" => {
                let language = Language::detect(Path::new(""), source);
                (language != Language::Other).then_some(Self::Code(language))?
            }
            _ => return None,
        })
    }
}

/// Analyze the selected language without executing any source. Lines and UTF-8
/// byte columns are zero-based, and a node's last line is inclusive.
pub(crate) fn analyze(source: &str, language: &str) -> Analysis {
    let Some(syntax) = Syntax::detect(language, source) else {
        return Analysis {
            status: Status::Unsupported,
            ..Analysis::default()
        };
    };
    if source.encode_utf16().take(MAX_UTF16_UNITS + 1).count() > MAX_UTF16_UNITS {
        return Analysis {
            status: Status::TooLarge,
            ..Analysis::default()
        };
    }
    match syntax {
        Syntax::Markup(markup) => scan_markup(source, markup),
        Syntax::Code(language) => {
            let mut scan = Scan::default();
            if language == Language::Python {
                scan.indented(source);
            } else {
                scan.braced(source, language);
            }
            scan.count_uses(source, language);
            scan.analysis
        }
    }
}

#[derive(Default)]
struct Scan {
    analysis: Analysis,
    binding_scopes: Vec<Option<usize>>,
    binding_names: HashMap<Option<usize>, HashSet<String>>,
}

#[derive(Clone, Copy)]
struct Scope {
    node: usize,
    level: usize,
}

impl Scan {
    fn node(
        &mut self,
        kind: &'static str,
        name: &str,
        line: usize,
        column: usize,
        parent: Option<usize>,
        separator: &str,
    ) -> Option<usize> {
        let index = self.analysis.nodes.len();
        if index == MAX_ENTRIES {
            self.analysis.limited = true;
            return None;
        }
        let kind = match kind {
            "fn" | "def" | "function" => {
                if parent.is_some_and(|index| {
                    matches!(self.analysis.nodes[index].kind, "class" | "trait" | "impl")
                }) {
                    "method"
                } else {
                    "function"
                }
            }
            "mod" => "module",
            _ => kind,
        };
        let mut qualified = match parent {
            Some(index) => format!("{}{separator}{name}", self.analysis.nodes[index].qualified),
            None => name.to_owned(),
        };
        // Keep deeply nested display labels bounded independently of source ranges.
        if qualified.len() > MAX_QUALIFIED_BYTES {
            let mut keep = qualified.len() - (MAX_QUALIFIED_BYTES - '…'.len_utf8());
            while !qualified.is_char_boundary(keep) {
                keep += 1;
            }
            qualified.replace_range(..keep, "…");
        }
        self.analysis.nodes.push(Node {
            name: name.to_owned(),
            kind,
            qualified,
            line,
            column,
            end_column: column + name.len(),
            end_line: line,
            parent,
        });
        Some(index)
    }

    fn binding(
        &mut self,
        kind: &'static str,
        name: &str,
        line: usize,
        column: usize,
        scope: Option<usize>,
        language: Language,
    ) {
        let reuse_binding = matches!(language, Language::Python | Language::Shell);
        if reuse_binding
            && self
                .binding_names
                .get(&scope)
                .is_some_and(|names| names.contains(name))
        {
            return;
        }
        if self.analysis.variables.len() == MAX_ENTRIES {
            self.analysis.limited = true;
            return;
        }
        if reuse_binding {
            self.binding_names
                .entry(scope)
                .or_default()
                .insert(name.to_owned());
        }
        self.analysis.variables.push(Variable {
            name: name.to_owned(),
            scope: scope.map_or_else(String::new, |index| {
                self.analysis.nodes[index].qualified.clone()
            }),
            kind,
            line,
            column,
            end_column: column + name.len(),
            uses: 0,
        });
        self.binding_scopes.push(scope);
    }

    fn close_scopes(&mut self, scopes: &mut Vec<Scope>, level: usize, line: usize) {
        while let Some(scope) = scopes.last().copied().filter(|scope| scope.level >= level) {
            self.analysis.nodes[scope.node].end_line =
                line.max(self.analysis.nodes[scope.node].line);
            scopes.pop();
        }
    }

    fn parameters(&mut self, code: &str, node: usize, language: Language) {
        let declaration = &self.analysis.nodes[node];
        if !matches!(declaration.kind, "function" | "method") || language == Language::Shell {
            return;
        }
        let line = declaration.line;
        let tail = &code[declaration.end_column..];
        let mut signature = Header::new(node);
        let Some(open) = tail.bytes().enumerate().find_map(|(column, byte)| {
            if byte == b'(' && signature.group == 0 {
                Some(column)
            } else {
                signature.observe(byte, column, tail, language);
                None
            }
        }) else {
            return;
        };
        let arguments = &tail[open + 1..];
        let mut start = 0;
        let mut nesting = Header::new(node);
        for (column, byte) in arguments.bytes().enumerate() {
            match byte {
                b',' | b')' if nesting.group == 0 => {
                    let mut argument =
                        arguments[start..column].trim_start_matches([' ', '\t', '*', '&']);
                    if language == Language::Rust {
                        while let Some((_, tail)) = leading_keyword(argument, &["mut", "ref"]) {
                            argument = tail;
                        }
                    }
                    if let Some((name, _)) = identifier(argument) {
                        self.binding(
                            "parameter",
                            name,
                            line,
                            column_of(code, name),
                            Some(node),
                            language,
                        );
                    }
                    if byte == b')' {
                        break;
                    }
                    start = column + 1;
                }
                _ => nesting.observe(byte, column, arguments, language),
            }
        }
    }

    fn indented(&mut self, source: &str) {
        let mut lexer = SourceState::default();
        let mut scopes: Vec<Scope> = Vec::new();
        let mut last = 0;
        for (line, raw) in lines(source).enumerate() {
            last = line;
            let (starts_in_code, code) = masked(raw, &mut lexer, Language::Python, false);
            if !starts_in_code || code.trim().is_empty() {
                continue;
            }
            let level = indentation(raw);
            self.close_scopes(&mut scopes, level, line.saturating_sub(1));
            let parent = scopes.last().map(|scope| scope.node);
            if let Some((kind, name)) = declaration(raw.trim_start(), Language::Python) {
                let column = column_of(raw, name);
                if kind == "variable" {
                    self.binding(kind, name, line, column, parent, Language::Python);
                } else if let Some(node) = self.node(kind, name, line, column, parent, ".") {
                    scopes.push(Scope { node, level });
                    self.parameters(&code, node, Language::Python);
                }
            }
        }
        self.close_scopes(&mut scopes, 0, last);
    }

    fn braced(&mut self, source: &str, language: Language) {
        let mut lexer = SourceState::default();
        let mut scopes: Vec<Scope> = Vec::new();
        let mut header: Option<Header> = None;
        let mut depth = 0usize;
        let mut last = 0;
        for (line, raw) in lines(source).enumerate() {
            last = line;
            let (starts_in_code, code) = masked(raw, &mut lexer, language, false);
            if header.is_some_and(|header| {
                line > self.analysis.nodes[header.node].line + MAX_HEADER_LINES
            }) {
                header = None;
            }
            if starts_in_code
                && let Some((kind, name)) = braced_declaration(raw.trim_start(), language)
            {
                let parent = scopes.last().map(|scope| scope.node);
                let column = column_of(raw, name);
                if kind == "variable" {
                    let kind = binding_kind(&raw[..column], language);
                    self.binding(kind, name, line, column, parent, language);
                } else {
                    let separator = if language == Language::Rust {
                        "::"
                    } else {
                        "."
                    };
                    header = self
                        .node(kind, name, line, column, parent, separator)
                        .map(Header::new);
                    if let Some(header) = header {
                        self.parameters(&code, header.node, language);
                    }
                }
            }
            for (column, byte) in code.bytes().enumerate() {
                let brace = structural_brace(&code, column, byte, language);
                if let Some(pending) = &mut header {
                    let node = &self.analysis.nodes[pending.node];
                    if line > node.line || column >= node.end_column {
                        if pending.group == 0 && byte == b'{' && brace {
                            scopes.push(Scope {
                                node: pending.node,
                                level: depth,
                            });
                            header = None;
                        } else if pending.group == 0 && byte == b';' {
                            header = None;
                        } else {
                            pending.observe(byte, column, &code, language);
                        }
                    }
                }
                if brace {
                    if byte == b'{' {
                        depth += 1;
                    } else {
                        depth = depth.saturating_sub(1);
                        self.close_scopes(&mut scopes, depth, line);
                    }
                }
            }
        }
        self.close_scopes(&mut scopes, 0, last);
    }

    fn count_uses(&mut self, source: &str, language: Language) {
        let bindings = &self.analysis.variables;
        if bindings.is_empty() {
            return;
        }
        let mut upcoming = (0..bindings.len()).peekable();
        let mut active: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut expiry: BinaryHeap<Reverse<(usize, usize)>> = BinaryHeap::new();
        let mut uses = vec![0; bindings.len()];
        let mut lexer = SourceState::default();
        for (line, raw) in lines(source).enumerate() {
            while let Some(&Reverse((end, index))) = expiry.peek() {
                if end >= line {
                    break;
                }
                expiry.pop();
                if let Some(indices) = active.get_mut(bindings[index].name.as_str()) {
                    indices.retain(|&active| active != index);
                }
            }
            let (_, code) = masked(raw, &mut lexer, language, true);
            for (column, name) in references(&code, language) {
                let site = (line, column);
                while let Some(&index) = upcoming.peek() {
                    let binding = &bindings[index];
                    if (binding.line, binding.column) > site {
                        break;
                    }
                    upcoming.next();
                    if let Some(scope) = self.binding_scopes[index] {
                        let end = self.analysis.nodes[scope].end_line;
                        if end < line {
                            continue;
                        }
                        expiry.push(Reverse((end, index)));
                    }
                    active.entry(&binding.name).or_default().push(index);
                }
                if let Some(&index) = active.get(name).and_then(|indices| indices.last()) {
                    let binding = &bindings[index];
                    if (binding.line, binding.column) != site {
                        uses[index] += 1;
                    }
                }
            }
        }
        for (binding, uses) in self.analysis.variables.iter_mut().zip(uses) {
            binding.uses = uses;
        }
    }
}

#[derive(Clone, Copy)]
struct Header {
    node: usize,
    group: usize,
}

impl Header {
    fn new(node: usize) -> Self {
        Self { node, group: 0 }
    }

    fn observe(&mut self, byte: u8, column: usize, line: &str, language: Language) {
        match byte {
            b'(' | b'[' | b'{' => self.group += 1,
            b')' | b']' | b'}' => self.group = self.group.saturating_sub(1),
            b'<' if language == Language::Rust => self.group += 1,
            b'>' if language == Language::Rust
                && (column == 0 || line.as_bytes()[column - 1] != b'-') =>
            {
                self.group = self.group.saturating_sub(1);
            }
            _ => {}
        }
    }
}

fn braced_declaration(text: &str, language: Language) -> Option<(&'static str, &str)> {
    declaration(text, language).or_else(|| {
        if language != Language::Rust {
            return None;
        }
        let tail = text.strip_prefix("impl")?;
        let tail = if let Some(tail) = tail.strip_prefix('<') {
            tail.split_once('>')?.1
        } else if tail.starts_with(char::is_whitespace) {
            tail
        } else {
            return None;
        };
        identifier(tail.trim_start()).map(|(name, _)| ("impl", name))
    })
}

fn binding_kind(prefix: &str, language: Language) -> &'static str {
    let mut words = prefix.split_whitespace();
    match language {
        Language::Shell => match words.next() {
            Some("export") => "exported",
            Some("readonly") => "constant",
            _ => "variable",
        },
        Language::Rust if words.any(|word| word == "const") => "constant",
        _ => "variable",
    }
}

fn structural_brace(line: &str, column: usize, byte: u8, language: Language) -> bool {
    if !matches!(byte, b'{' | b'}') {
        return false;
    }
    if language != Language::Shell {
        return true;
    }
    let separator = |byte: u8| byte.is_ascii_whitespace() || b";|&()<>".contains(&byte);
    let bytes = line.as_bytes();
    (column == 0 || separator(bytes[column - 1]))
        && bytes.get(column + 1).is_none_or(|&byte| separator(byte))
}

fn indentation(line: &str) -> usize {
    line.bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .fold(0, |width, byte| {
            if byte == b'\t' {
                (width / 8 + 1) * 8
            } else {
                width + 1
            }
        })
}

fn lines(source: &str) -> impl Iterator<Item = &str> {
    LineIter::new(source).map(|(range, _)| &source[range])
}

fn column_of(line: &str, name: &str) -> usize {
    // The declaration parser returns a subslice of the original line.
    name.as_ptr().addr() - line.as_ptr().addr()
}

fn masked(
    line: &str,
    lexer: &mut SourceState,
    language: Language,
    include_shell_expansions: bool,
) -> (bool, String) {
    let mut code_bytes = vec![false; line.len()];
    let starts_in_code = lexer.scan_line(line, language, |offset, quoted| {
        code_bytes[offset] = !quoted || include_shell_expansions;
    });
    let mut code = String::with_capacity(line.len());
    for (offset, character) in line.char_indices() {
        if code_bytes[offset] {
            code.push(character);
        } else {
            code.extend(std::iter::repeat_n(' ', character.len_utf8()));
        }
    }
    (starts_in_code, code)
}

fn references(text: &str, language: Language) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    std::iter::from_fn(move || {
        loop {
            let start = if language == Language::Shell {
                offset += text[offset..].find('$')? + 1;
                if text.as_bytes().get(offset) == Some(&b'$') {
                    offset += 1;
                    continue;
                }
                offset + usize::from(text.as_bytes().get(offset) == Some(&b'{'))
            } else {
                offset += text[offset..].find(|ch: char| ch == '_' || ch.is_alphanumeric())?;
                offset
            };
            if let Some((name, _)) = identifier(&text[start..]) {
                offset = start + name.len();
                return Some((start, name));
            }
            if language != Language::Shell {
                let tail = &text[start..];
                offset += tail
                    .find(|ch: char| ch != '_' && !ch.is_alphanumeric())
                    .unwrap_or(tail.len());
            }
        }
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Markup {
    Markdown,
    AsciiDoc,
}

fn scan_markup(source: &str, markup: Markup) -> Analysis {
    let mut scan = Scan::default();
    let mut scopes: Vec<Scope> = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut last = 0;
    for (line, raw) in lines(source).enumerate() {
        last = line;
        let text = if markup == Markup::Markdown {
            let text = raw.trim_start_matches(' ');
            if raw.len() - text.len() > 3 {
                continue;
            }
            text
        } else {
            raw
        };
        if let Some((marker, count, rest)) = fence_marker(text, markup) {
            match fence {
                None => fence = Some((marker, count)),
                Some((open, length))
                    if open == marker
                        && rest.trim().is_empty()
                        && if markup == Markup::Markdown {
                            count >= length
                        } else {
                            count == length
                        } =>
                {
                    fence = None;
                }
                _ => {}
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let Some((level, name)) = heading(text, markup) else {
            continue;
        };
        scan.close_scopes(&mut scopes, level, line.saturating_sub(1));
        let parent = scopes.last().map(|scope| scope.node);
        if let Some(node) = scan.node("section", name, line, column_of(raw, name), parent, " › ")
        {
            scopes.push(Scope { node, level });
        }
    }
    scan.close_scopes(&mut scopes, 0, last);
    scan.analysis
}

fn fence_marker(text: &str, markup: Markup) -> Option<(u8, usize, &str)> {
    let marker = *text.as_bytes().first()?;
    let minimum = match (markup, marker) {
        (Markup::Markdown, b'~' | b'\x60') | (Markup::AsciiDoc, b'\x60') => 3,
        (Markup::AsciiDoc, b'-' | b'.' | b'=' | b'*' | b'+' | b'/' | b'_') => 4,
        _ => return None,
    };
    let count = text.bytes().take_while(|&byte| byte == marker).count();
    let rest = &text[count..];
    (count >= minimum && (markup == Markup::Markdown || rest.trim().is_empty()))
        .then_some((marker, count, rest))
}

fn heading(text: &str, markup: Markup) -> Option<(usize, &str)> {
    let marker = *text.as_bytes().first()?;
    if marker != b'#' && !(marker == b'=' && markup == Markup::AsciiDoc) {
        return None;
    }
    let level = text.bytes().take_while(|&byte| byte == marker).count();
    let tail = &text[level..];
    if level > 6 || !tail.starts_with([' ', '\t']) {
        return None;
    }
    let mut name = tail.trim();
    if markup == Markup::Markdown {
        let before = name.trim_end_matches('#');
        if before.ends_with([' ', '\t']) {
            name = before.trim_end();
        }
    }
    (!name.is_empty()).then_some((level, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_nesting_uses_editor_line_endings_and_ignores_literal_braces() {
        let source = concat!(
            "mod tooling {\r\n",
            "    impl Widget {\r",
            "        fn render(\n\r",
            "            argument: usize,\n",
            "        ) {\n",
            "            let café = 1;\n",
            "            let literal = \"}\";\n",
            "            /* } fn hidden() {} café */\n",
            "            consume(café);\n",
            "        }\n",
            "    }\n",
            "}\n",
            "fn after(value: usize) { consume(value); }\n",
        );
        let result = analyze(source, "Rust");
        assert_eq!(result.status, Status::Ok);
        let names: Vec<_> = result
            .nodes
            .iter()
            .map(|node| {
                (
                    node.qualified.as_str(),
                    node.kind,
                    node.parent,
                    node.line,
                    node.end_line,
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ("tooling", "module", None, 0, 11),
                ("tooling::Widget", "impl", Some(0), 1, 10),
                ("tooling::Widget::render", "method", Some(1), 2, 9),
                ("after", "function", None, 12, 12),
            ]
        );
        let variable = result
            .variables
            .iter()
            .find(|variable| variable.name == "café")
            .unwrap();
        assert_eq!(variable.scope, "tooling::Widget::render");
        assert_eq!(variable.uses, 1);
        let source_line = lines(source).nth(variable.line).unwrap();
        assert_eq!(&source_line[variable.column..variable.end_column], "café");
        let parameter = result
            .variables
            .iter()
            .find(|variable| variable.name == "value")
            .unwrap();
        assert_eq!(
            (parameter.kind, parameter.scope.as_str(), parameter.uses),
            ("parameter", "after", 1)
        );
    }

    #[test]
    fn python_dedent_and_scoped_references_ignore_multiline_strings() {
        let source = concat!(
            "value = 0\n",
            "class Gerät:\n",
            "    def run(self, scale=(1, 2)):\n",
            "        value: int = 2\n",
            "        \"\"\"docstring\n",
            "def hidden():\n",
            "value = 99\n",
            "\"\"\"\n",
            "        return value * scale\n",
            "print(value)\n",
        );
        let result = analyze(source, "python");
        assert_eq!(result.nodes.len(), 2);
        let method = &result.nodes[1];
        assert_eq!(method.kind, "method");
        assert_eq!(method.qualified, "Gerät.run");
        assert_eq!(method.parent, Some(0));
        assert_eq!(method.end_line, 8);
        let values: Vec<_> = result
            .variables
            .iter()
            .filter(|variable| variable.name == "value")
            .map(|variable| (variable.scope.as_str(), variable.uses))
            .collect();
        assert_eq!(values, vec![("", 1), ("Gerät.run", 1)]);
        let scale = result
            .variables
            .iter()
            .find(|variable| variable.name == "scale")
            .unwrap();
        assert_eq!((scale.kind, scale.uses), ("parameter", 1));
    }

    #[test]
    fn shell_shebang_preserves_scopes_and_only_counts_expansions() {
        let source = concat!(
            "#!/usr/bin/env bash\n",
            "export TARGET=local\n",
            "function send() {\n",
            "    local target=peer\n",
            "    printf '%s' \"$target\"\n",
            "    printf '%s' '\u{24}{target}'\n",
            "    printf '%s' \u{24}\u{24}{target}\n",
            "    printf '%s' target\n",
            "    cat <<'BODY'\n",
            "function hidden() { }\n",
            "}\n",
            "BODY\n",
            "}\n",
            "printf '%s' \"\u{24}{TARGET}\"\n",
        );
        let result = analyze(source, "plain");
        assert_eq!(result.status, Status::Ok);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(
            (&*result.nodes[0].name, result.nodes[0].end_line),
            ("send", 12)
        );
        assert_eq!(result.variables.len(), 2);
        let global = &result.variables[0];
        let local = &result.variables[1];
        assert_eq!(
            (global.kind, global.scope.as_str(), global.uses),
            ("exported", "", 1)
        );
        assert_eq!(
            (local.kind, local.scope.as_str(), local.uses),
            ("variable", "send", 1)
        );
        assert_eq!(analyze(source, "unsupported").status, Status::Unsupported);
    }

    #[test]
    fn markup_headings_are_nested_and_code_blocks_are_skipped() {
        let markdown = concat!(
            "# Intro\n",
            "\u{60}\u{60}\u{60}\u{60}sh\n",
            "# Hidden\n",
            "\u{60}\u{60}\u{60}\n",
            "## Still hidden\n",
            "\u{60}\u{60}\u{60}\u{60}\n",
            "  ## Über ###\n",
            "# End\n",
        );
        let result = analyze(markdown, "markdown");
        assert_eq!(result.nodes.len(), 3);
        let child = &result.nodes[1];
        assert_eq!(child.qualified, "Intro › Über");
        assert_eq!(child.parent, Some(0));
        assert_eq!(result.nodes[0].end_line, 6);
        let source_line = lines(markdown).nth(child.line).unwrap();
        assert_eq!(&source_line[child.column..child.end_column], "Über");

        let asciidoc = "= Guide\n----\n== Hidden\n----\n== Config\n=== Detail\n= End";
        let result = analyze(asciidoc, "asciidoctor");
        assert_eq!(result.nodes.len(), 4);
        assert_eq!(result.nodes[2].qualified, "Guide › Config › Detail");
        assert_eq!(result.nodes[2].parent, Some(1));
        assert!(result.variables.is_empty());
    }

    #[test]
    fn utf16_and_per_section_limits_are_enforced() {
        let source = "🙂".repeat(MAX_UTF16_UNITS / 2);
        assert_eq!(analyze(&source, "markdown").status, Status::Ok);
        assert_eq!(
            analyze(&(source + "a"), "markdown").status,
            Status::TooLarge
        );

        let mut source = String::new();
        use std::fmt::Write as _;
        for index in 0..=MAX_ENTRIES {
            writeln!(source, "fn f{index}() {{}}\nlet v{index} = 0;").unwrap();
        }
        let result = analyze(&source, "rust");
        assert!(result.limited);
        assert_eq!(result.nodes.len(), MAX_ENTRIES);
        assert_eq!(result.variables.len(), MAX_ENTRIES);
    }

    #[test]
    fn reassignment_reuses_the_first_binding_but_rust_shadowing_stays_distinct() {
        let shell = analyze("n=1\nn=2\necho \"$n\"\n", "bash");
        assert_eq!(shell.variables.len(), 1);
        assert_eq!(shell.variables[0].line, 0);
        assert_eq!(shell.variables[0].uses, 1);

        let python = analyze("n = 1\nn = n + 1\nprint(n)\n", "python");
        assert_eq!(python.variables.len(), 1);
        assert_eq!(python.variables[0].line, 0);
        // As in Java, the later assignment's name is among the word references.
        assert_eq!(python.variables[0].uses, 3);

        let rust = analyze(
            "fn f() {\n    let n = 1;\n    use_value(n);\n    let n = 2;\n    use_value(n);\n}\n",
            "rust",
        );
        assert_eq!(rust.variables.len(), 2);
        assert_eq!((rust.variables[0].line, rust.variables[0].uses), (1, 1));
        assert_eq!((rust.variables[1].line, rust.variables[1].uses), (3, 1));
    }

    #[test]
    fn repeated_assignments_do_not_consume_the_binding_limit() {
        let source = "n=1\n".repeat(MAX_ENTRIES + 1) + "other=2\n";
        let result = analyze(&source, "bash");
        assert!(!result.limited);
        assert_eq!(result.variables.len(), 2);
        assert_eq!(result.variables[0].name, "n");
        assert_eq!(result.variables[1].name, "other");
    }
}
