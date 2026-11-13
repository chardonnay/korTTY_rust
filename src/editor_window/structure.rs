//! Bounded lexical structure analysis with coordinates for the native editor.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::Path;

use iced::advanced::graphics::text::cosmic_text::LineIter;
use iced::widget::text_editor;

use super::{Language, SourceState, declaration, identifier, leading_keyword};

mod docker;
mod extended;
mod python;
mod shell;
mod sql;

pub(super) fn additional_language(path: &Path, source: &str) -> Option<&'static str> {
    extended::detect(path, source)
}

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
    pub(crate) stage_index: Option<usize>,
    pub(crate) kind: &'static str,
    pub(crate) container: Option<&'static str>,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) end_column: usize,
    pub(crate) uses: usize,
}

enum Syntax {
    Code(Language),
    Extended(extended::Language),
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
            "dockerfile" => Self::Code(Language::Dockerfile),
            "sql" => Self::Code(Language::Sql),
            language if extended::Language::from_name(language).is_some() => {
                Self::Extended(extended::Language::from_name(language)?)
            }
            "" | "plain" | "text" | "plaintext" => {
                if let Some(language) = extended::Language::from_shebang(source) {
                    return Some(Self::Extended(language));
                }
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
        Syntax::Extended(language) => extended::scan(source, language),
        Syntax::Markup(markup) => scan_markup(source, markup),
        Syntax::Code(Language::Dockerfile) => docker::scan(source),
        Syntax::Code(Language::Python) => python::scan(source, true),
        Syntax::Code(Language::Sql) => sql::scan(source),
        Syntax::Code(language) => {
            let mut scan = Scan::default();
            scan.braced(source, language);
            scan.count_uses(source, language, None);
            scan.analysis
        }
    }
}

pub(super) fn shell_outline(source: &str) -> Analysis {
    // The file editor already bounds its source. The outline does not display
    // use counts, so it can scan that source with the ordinary entry limit.
    let mut scan = Scan::default();
    scan.braced(source, Language::Shell);
    scan.analysis
}

pub(super) fn python_outline(source: &str) -> Analysis {
    python::scan(source, false)
}

#[derive(Default)]
struct Scan {
    analysis: Analysis,
    binding_scopes: Vec<Option<usize>>,
    binding_names: HashMap<Option<usize>, HashSet<String>>,
    python_field_writes: Option<HashSet<(usize, usize)>>,
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
        (kind, container): (&'static str, Option<&'static str>),
        name: &str,
        line: usize,
        column: usize,
        scope: Option<usize>,
        language: Language,
    ) {
        if kind == "field"
            && let Some(writes) = &mut self.python_field_writes
        {
            // Keep repeated class assignments even when the field already has an outline row.
            writes.insert((line, column));
        }
        let reuse_binding = matches!(
            language,
            Language::Python | Language::Shell | Language::Dockerfile | Language::Sql
        );
        if reuse_binding
            && self
                .binding_names
                .get(&scope)
                .is_some_and(|names| names.contains(name))
        {
            return;
        }
        if self
            .push_binding((kind, container), name, line, column, scope, language)
            .is_some()
            && reuse_binding
        {
            self.binding_names
                .entry(scope)
                .or_default()
                .insert(name.to_owned());
        }
    }

    fn push_binding(
        &mut self,
        (kind, container): (&'static str, Option<&'static str>),
        name: &str,
        line: usize,
        column: usize,
        scope: Option<usize>,
        language: Language,
    ) -> Option<usize> {
        let index = self.analysis.variables.len();
        if index == MAX_ENTRIES {
            self.analysis.limited = true;
            return None;
        }
        self.analysis.variables.push(Variable {
            name: name.to_owned(),
            scope: scope.map_or_else(String::new, |index| {
                self.analysis.nodes[index].qualified.clone()
            }),
            stage_index: scope.filter(|_| language == Language::Dockerfile),
            kind,
            container,
            line,
            column,
            end_column: column + name.len(),
            uses: 0,
        });
        self.binding_scopes.push(scope);
        Some(index)
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
                            ("parameter", None),
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

    fn braced(&mut self, source: &str, language: Language) {
        let mut lexer = Masker::new(source, language);
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
            let parent = scopes.last().map(|scope| scope.node);
            if starts_in_code && language == Language::Shell {
                shell::bindings(raw, |binding| {
                    if binding.scope == shell::Scope::Default
                        && scopes.iter().rev().any(|scope| {
                            self.binding_names
                                .get(&Some(scope.node))
                                .is_some_and(|names| names.contains(binding.name))
                        })
                    {
                        return;
                    }
                    self.binding(
                        (binding.kind, binding.container),
                        binding.name,
                        line,
                        column_of(raw, binding.name),
                        parent.filter(|_| binding.scope == shell::Scope::Local),
                        language,
                    );
                });
            }
            let declaration_source = if language == Language::Shell {
                &code
            } else {
                raw
            };
            let column = raw.len() - raw.trim_start().len();
            if starts_in_code
                && let Some((kind, name)) =
                    braced_declaration(&declaration_source[column..], language)
            {
                let column = column_of(declaration_source, name);
                if kind == "variable" {
                    let kind = if raw[..column].split_whitespace().any(|word| word == "const") {
                        "constant"
                    } else {
                        "variable"
                    };
                    self.binding((kind, None), name, line, column, parent, language);
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

    fn count_uses(&mut self, source: &str, language: Language, loops: Option<&python::Loops>) {
        let bindings = &self.analysis.variables;
        if bindings.is_empty() {
            return;
        }
        let start = |index: usize| {
            let binding = &bindings[index];
            if language == Language::Shell {
                let scope = self.binding_scopes[index];
                let line = scope.map_or(0, |scope| self.analysis.nodes[scope].line);
                (line, 0)
            } else {
                (binding.line, binding.column)
            }
        };
        let mut python = loops.map(|loops| {
            python::References::new(
                loops,
                &self.analysis,
                &self.binding_scopes,
                self.python_field_writes.as_ref(),
            )
        });
        let mut order: Vec<_> = (0..bindings.len())
            .filter(|&index| {
                python
                    .as_ref()
                    .is_none_or(|references| !references.special(index))
            })
            .collect();
        if language == Language::Shell {
            // The reference attributes shell expansions to their lexical function
            // even when the expansion precedes the declaration on the page.
            order.sort_by_key(|&index| (start(index), self.binding_scopes[index]));
        } else if language == Language::Python {
            order.sort_by_key(|&index| start(index));
        }
        let mut upcoming = order.into_iter().peekable();
        let mut active: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut expiry: BinaryHeap<Reverse<(usize, usize)>> = BinaryHeap::new();
        let mut uses = vec![0; bindings.len()];
        let mut lexer = Masker::new(source, language);
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
                if let Some(references) = &mut python {
                    match references.lookup(site, &code[..column], name) {
                        python::Reference::Plain => {}
                        python::Reference::Binding(index) => {
                            let binding = &bindings[index];
                            if (binding.line, binding.column) != site {
                                uses[index] += 1;
                            }
                            continue;
                        }
                        python::Reference::Ignored => continue,
                    }
                }
                while let Some(&index) = upcoming.peek() {
                    let binding = &bindings[index];
                    if start(index) > site {
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
    if language == Language::Shell {
        return shell::function(text).map(|name| ("function", name));
    }
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

pub(super) fn lines(source: &str) -> impl Iterator<Item = &str> {
    LineIter::new(source).map(|(range, _)| &source[range])
}

pub(super) fn column_of(line: &str, name: &str) -> usize {
    // The declaration parser returns a subslice of the supplied line.
    name.as_ptr().addr() - line.as_ptr().addr()
}

pub(crate) fn select_name(
    content: &mut text_editor::Content,
    line: usize,
    columns: Range<usize>,
) -> bool {
    if columns.is_empty()
        || content
            .line(line)
            .is_none_or(|line| line.text.get(columns.clone()).is_none())
    {
        return false;
    }
    content.move_to(text_editor::Cursor {
        position: text_editor::Position {
            line,
            column: columns.end,
        },
        selection: Some(text_editor::Position {
            line,
            column: columns.start,
        }),
    });
    true
}

#[derive(Default)]
struct Masker<'a> {
    source: SourceState,
    heredocs: VecDeque<(shell::HereDocument, usize)>,
    terminators: Option<shell::Terminators<'a>>,
    position: usize,
}

impl<'a> Masker<'a> {
    fn new(source: &'a str, language: Language) -> Self {
        Self {
            terminators: (language == Language::Shell)
                .then(|| shell::Terminators::new(lines(source))),
            ..Self::default()
        }
    }
}

fn masked(
    line: &str,
    lexer: &mut Masker<'_>,
    language: Language,
    include_shell_expansions: bool,
) -> (bool, String) {
    let position = lexer.position;
    lexer.position += 1;
    while lexer
        .heredocs
        .front()
        .is_some_and(|(_, last)| *last < position)
    {
        lexer.heredocs.pop_front();
    }
    if let Some((heredoc, _)) = lexer.heredocs.front() {
        if !heredoc.ends_at(line) {
            return (false, heredoc.mask_body(line, include_shell_expansions));
        }
        lexer.heredocs.pop_front();
        return (false, " ".repeat(line.len()));
    }

    let mut code_bytes = vec![false; line.len()];
    let mut delimiters = Vec::new();
    let starts_in_code = lexer.source.scan_line(line, language, |offset, quoted| {
        code_bytes[offset] = !quoted || include_shell_expansions;
        if !quoted
            && let Some(terminators) = &lexer.terminators
            && line.as_bytes()[offset..].starts_with(b"<<")
            && let Some((heredoc, length)) = shell::HereDocument::parse(&line[offset + 2..])
            && let Some(last) = terminators.last_after(&heredoc, position)
        {
            lexer.heredocs.push_back((heredoc, last));
            delimiters.push(offset + 2..offset + 2 + length);
        }
    });
    if language == Language::Shell {
        for delimiter in delimiters {
            code_bytes[delimiter].fill(false);
        }
    }
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
    let dollar_expansions = matches!(language, Language::Shell | Language::Dockerfile);
    std::iter::from_fn(move || {
        loop {
            let start = if dollar_expansions {
                offset += text[offset..].find('$')? + 1;
                if text.as_bytes().get(offset) == Some(&b'$') {
                    offset += 1;
                    continue;
                }
                let braced = text.as_bytes().get(offset) == Some(&b'{');
                let name = offset + usize::from(braced);
                name + usize::from(
                    (braced || language == Language::Dockerfile)
                        && matches!(text.as_bytes().get(name), Some(b'#' | b'!')),
                )
            } else {
                offset += text[offset..].find(|ch: char| ch == '_' || ch.is_alphanumeric())?;
                offset
            };
            if let Some((name, _)) = identifier(&text[start..]) {
                offset = start + name.len();
                return Some((start, name));
            }
            if !dollar_expansions {
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
    fn bash_bindings_follow_reference_scopes_and_point_at_each_argument() {
        let source = r#"#!/bin/bash
readonly BASE_DIR=/opt/app
export TARGET_HOST=web01
declare -A config=( [a]=1 )
hosts=(a b c)
count=0
deploy() {
  local target="$1"
  local -a parts
  declare -g FLAG=1
  status=ok
  echo "${target} $count"
}
function cleanup {
  rm -f "$tmp"
}
for host in "${hosts[@]}"; do deploy "$host"; done
"#;
        let report = analyze(source, "bash");
        assert_eq!(
            report
                .nodes
                .iter()
                .map(|node| (node.name.as_str(), node.line + 1, node.end_line + 1))
                .collect::<Vec<_>>(),
            [("deploy", 7, 13), ("cleanup", 14, 16)]
        );
        let bindings: Vec<_> = report
            .variables
            .iter()
            .map(|var| {
                let line = lines(source).nth(var.line).unwrap();
                assert_eq!(&line[var.column..var.end_column], var.name);
                (var.name.as_str(), var.kind, var.scope.as_str(), var.uses)
            })
            .collect();
        assert_eq!(
            bindings,
            [
                ("BASE_DIR", "constant", "", 0),
                ("TARGET_HOST", "exported", "", 0),
                ("config", "hash", "", 0),
                ("hosts", "array", "", 1),
                ("count", "variable", "", 1),
                ("target", "variable", "deploy", 1),
                ("parts", "array", "deploy", 0),
                ("FLAG", "variable", "", 0),
                ("status", "variable", "", 0),
                ("host", "variable", "", 1),
            ]
        );
    }

    #[test]
    fn bash_bindings_find_command_outputs_and_resolve_nested_references() {
        let source = r#"value=outside
echo "$later_global"
outer() {
  echo ${#value}
  local value=outer
  local private_value=outer
  private_value=still_local
  typeset -r first=one second="two names"
  local -rA frozen=([a]=1)
  declare -gx envname=1
  declare -f ignored
  later_global=new
  inner-task() {
    local value=inner
    private_value=still_outer
    echo "$value"
  }
  echo "$value"
}
echo "$value $private_value" # $value
while IFS= read -r -p "User name:" user role; do true; done
read -r -a columns < values
readarray -t -n 2 -C callback -c 1 loaded
mapfile -C another_callback
while getopts 'ab:' flag; do true; done
for ((counter=0; counter<3; counter++)); do true; done
indexed[0]+=value
"#;
        let report = analyze(source, "bash");
        assert_eq!(report.nodes[1].qualified, "outer.inner-task");
        let frozen = report
            .variables
            .iter()
            .find(|variable| variable.name == "frozen")
            .unwrap();
        assert_eq!(frozen.container, Some("hash"));
        assert_eq!(
            report
                .variables
                .iter()
                .map(|var| (var.name.as_str(), var.kind, var.scope.as_str(), var.uses))
                .collect::<Vec<_>>(),
            [
                ("value", "variable", "", 1),
                ("value", "variable", "outer", 2),
                ("private_value", "variable", "outer", 0),
                ("first", "constant", "outer", 0),
                ("second", "constant", "outer", 0),
                ("frozen", "constant", "outer", 0),
                ("envname", "exported", "", 0),
                ("later_global", "variable", "", 1),
                ("value", "variable", "outer.inner-task", 1),
                ("user", "variable", "", 0),
                ("role", "variable", "", 0),
                ("columns", "array", "", 0),
                ("loaded", "array", "", 0),
                ("flag", "variable", "", 0),
                ("counter", "variable", "", 0),
                ("indexed", "array", "", 0),
            ]
        );
    }

    #[test]
    fn bash_arithmetic_shift_is_not_a_here_document_with_a_later_matching_line() {
        let source = "n=1\nshifted=$((n << SHIFT ))\nvisible=2\nSHIFT\ncat <<SHIFT\nhidden=3\nSHIFT\necho $visible\n";
        let report = analyze(source, "bash");
        assert_eq!(
            report
                .variables
                .iter()
                .map(|var| (var.name.as_str(), var.uses))
                .collect::<Vec<_>>(),
            [("n", 0), ("shifted", 0), ("visible", 1)]
        );
    }

    #[test]
    fn bash_unquoted_heredoc_counts_expansions_without_outline_nodes() {
        let source = concat!(
            "count=1\n",
            "cat <<-EOF;\n",
            "\tfunction hidden() { }\n",
            "\tignored=1\n",
            "\tcafé '$count' \\$count \\\\$count ${#count} ${!count}\n",
            "\tEOF\n",
            "after() {\n",
            "  printf '%s' \"$count\"\n",
            "}\n",
        );
        let result = analyze(source, "bash");
        assert_eq!(result.status, Status::Ok);
        let [variable] = result.variables.as_slice() else {
            panic!("unexpected variables: {:?}", result.variables);
        };
        assert_eq!((variable.name.as_str(), variable.uses), ("count", 5));
        let [after] = result.nodes.as_slice() else {
            panic!("unexpected nodes: {:?}", result.nodes);
        };
        assert_eq!(
            (after.name.as_str(), after.line, after.end_line),
            ("after", 6, 8)
        );
        let line = lines(source).nth(after.line).unwrap();
        assert_eq!(&line[after.column..after.end_column], "after");
    }

    #[test]
    fn bash_quoted_and_backslash_heredocs_keep_body_and_delimiter_literal() {
        let source = r#"count=1
cat <<OPEN>/dev/null <<'SINGLE' <<"$count" <<\BACK <<MIX'ED'
$count
OPEN
single() { echo "$count"; }
SINGLE
double() { echo "$count"; }
$count
backslash() { echo "$count"; }
BACK
mixed() { echo "$count"; }
MIXED
cat <<< "$count"
cat <<< HERESTRING
((1 << count ))
# cat <<COMMENT
printf '%s' '<<LITERAL'
after() { echo "$count"; }
"#;
        let result = analyze(source, "bash");
        assert_eq!(result.status, Status::Ok);
        let [variable] = result.variables.as_slice() else {
            panic!("unexpected variables: {:?}", result.variables);
        };
        assert_eq!((variable.name.as_str(), variable.uses), ("count", 3));
        let [after] = result.nodes.as_slice() else {
            panic!("unexpected nodes: {:?}", result.nodes);
        };
        assert_eq!(
            (after.name.as_str(), after.line, after.end_line),
            ("after", 17, 17)
        );
    }

    #[test]
    fn bash_heredoc_requires_a_later_terminator_with_matching_indentation() {
        let source = concat!(
            "count=1\n",
            "END\n",
            "cat <<END\n",
            "\tEND\n",
            "cat <<STOP <<STOP\n",
            "$count\n",
            "STOP\n",
            "after() { echo \"$count\"; }\n",
        );
        let result = analyze(source, "bash");
        let [variable] = result.variables.as_slice() else {
            panic!("unexpected variables: {:?}", result.variables);
        };
        assert_eq!((variable.name.as_str(), variable.uses), ("count", 2));
        let [after] = result.nodes.as_slice() else {
            panic!("unexpected nodes: {:?}", result.nodes);
        };
        assert_eq!((after.name.as_str(), after.line), ("after", 7));
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
