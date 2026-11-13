//! Lexical outlines for the scripting and JVM languages supported by the reference editor.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::{Analysis, MAX_ENTRIES, MAX_HEADER_LINES, MAX_QUALIFIED_BYTES, Node, Variable, lines};

mod lexer;
#[cfg(test)]
mod tests;

const MAX_DEPTH: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Language {
    Perl,
    Ruby,
    Lua,
    PowerShell,
    Java,
    Groovy,
    JavaScript,
    TypeScript,
}

impl Language {
    fn name(self) -> &'static str {
        match self {
            Self::Perl => "perl",
            Self::Ruby => "ruby",
            Self::Lua => "lua",
            Self::PowerShell => "powershell",
            Self::Java => "java",
            Self::Groovy => "groovy",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
        }
    }
    pub(super) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "perl" | "pl" => Self::Perl,
            "ruby" | "rb" => Self::Ruby,
            "lua" | "luajit" => Self::Lua,
            "powershell" | "pwsh" | "ps1" => Self::PowerShell,
            "java" => Self::Java,
            "groovy" | "gradle" => Self::Groovy,
            "javascript" | "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "typescript" | "ts" | "tsx" => Self::TypeScript,
            _ => return None,
        })
    }

    pub(super) fn from_shebang(source: &str) -> Option<Self> {
        let line = source.lines().next()?.strip_prefix("#!")?;
        line.split_whitespace().find_map(|word| {
            let word = word.rsplit('/').next().unwrap_or(word);
            Self::from_name(word.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.'))
        })
    }

    fn folded(self, text: &str) -> String {
        if self == Self::PowerShell {
            text.to_lowercase()
        } else {
            text.to_owned()
        }
    }

    fn braced(self) -> bool {
        !matches!(self, Self::Lua | Self::Ruby)
    }
    fn jvm(self) -> bool {
        matches!(self, Self::Java | Self::Groovy)
    }
    fn javascript(self) -> bool {
        matches!(self, Self::JavaScript | Self::TypeScript)
    }
}

pub(super) fn detect(path: &std::path::Path, source: &str) -> Option<&'static str> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| Language::from_name(&extension.to_ascii_lowercase()))
        .or_else(|| Language::from_shebang(source))
        .map(Language::name)
}

macro_rules! rx {
    ($name:ident, $pattern:literal) => {{
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("static structure expression"));
        &*$name
    }};
}

#[derive(Clone, Copy)]
struct Pending {
    node: usize,
    after: usize,
    groups: usize,
}

struct Scanner {
    language: Language,
    result: Analysis,
    scopes: Vec<(usize, usize)>,
    blocks: Vec<Option<usize>>,
    block_overflow: usize,
    pending: Option<Pending>,
    depth: usize,
    tables: HashMap<String, usize>,
    table_names: HashSet<String>,
    package: Option<usize>,
    variable_scopes: Vec<Option<usize>>,
    defined: HashSet<(Option<usize>, String)>,
}

pub(super) fn scan(source: &str, language: Language) -> Analysis {
    let masked = lexer::mask(source, language);
    let table_names = if language == Language::Lua {
        masked
            .iter()
            .flat_map(|line| {
                rx!(
                    LUA_OWNERS,
                    r"(?:\bfunction\s+|\b)([A-Za-z_]\w*)[.:][A-Za-z_]\w*\s*(?:\(|=\s*function\b)"
                )
                .captures_iter(&line.code)
                .map(|capture| capture[1].to_owned())
                .collect::<Vec<_>>()
            })
            .collect()
    } else {
        HashSet::new()
    };
    let mut scan = Scanner {
        language,
        result: Analysis::default(),
        scopes: Vec::new(),
        blocks: Vec::new(),
        block_overflow: 0,
        pending: None,
        depth: 0,
        tables: HashMap::new(),
        table_names,
        package: None,
        variable_scopes: Vec::new(),
        defined: HashSet::new(),
    };
    for (line, text) in masked.iter().enumerate() {
        if language.braced() {
            scan.braces(line, &text.code);
        } else if language == Language::Ruby {
            scan.ruby(line, &text.code);
        } else {
            scan.lua(line, &text.code);
        }
    }
    let last = masked.len().saturating_sub(1);
    for &(node, _) in &scan.scopes {
        scan.result.nodes[node].end_line = last;
    }
    for node in scan.blocks.iter().flatten() {
        scan.result.nodes[*node].end_line = last;
    }
    if let Some(node) = scan.package {
        scan.result.nodes[node].end_line = last;
    }
    let owners = scan.owners(masked.len());
    scan.parameters(&masked);
    if language == Language::PowerShell {
        scan.powershell_parameters(&masked, &owners);
    }
    for (line, text) in masked.iter().enumerate() {
        scan.variables(line, &text.code, owners[line]);
    }
    scan.count_references(&masked);
    scan.result
}

impl Scanner {
    fn owner(&self) -> Option<usize> {
        self.scopes
            .last()
            .map(|(node, _)| *node)
            .or_else(|| self.blocks.iter().rev().flatten().next().copied())
            .or(self.package)
    }

    fn node(
        &mut self,
        name: &str,
        kind: &'static str,
        line: usize,
        column: usize,
        parent: Option<usize>,
    ) -> Option<usize> {
        if self.result.nodes.len() == MAX_ENTRIES {
            self.result.limited = true;
            return None;
        }
        let depth = self.node_depth(parent);
        if depth >= MAX_DEPTH {
            self.result.limited = true;
            return None;
        }
        let separator = if self.language == Language::Perl {
            "::"
        } else {
            "."
        };
        let mut qualified = parent.map_or_else(
            || name.to_owned(),
            |parent| format!("{}{separator}{name}", self.result.nodes[parent].qualified),
        );
        if qualified.len() > MAX_QUALIFIED_BYTES {
            let mut start = qualified.len() - MAX_QUALIFIED_BYTES + '…'.len_utf8();
            while !qualified.is_char_boundary(start) {
                start += 1;
            }
            qualified.replace_range(..start, "…");
        }
        let index = self.result.nodes.len();
        self.result.nodes.push(Node {
            name: name.into(),
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

    fn node_depth(&self, mut node: Option<usize>) -> usize {
        let mut depth = 0;
        while let Some(index) = node {
            depth += 1;
            node = self.result.nodes[index].parent;
        }
        depth
    }

    fn enclosing(&self, mut node: Option<usize>, class: bool) -> Option<usize> {
        while let Some(index) = node {
            let kind = self.result.nodes[index].kind;
            if if class {
                matches!(kind, "class" | "module")
            } else {
                matches!(kind, "function" | "method")
            } {
                return Some(index);
            }
            node = self.result.nodes[index].parent;
        }
        None
    }

    fn function_kind(&self, parent: Option<usize>) -> &'static str {
        if parent.is_some_and(|parent| self.result.nodes[parent].kind == "class") {
            "method"
        } else {
            "function"
        }
    }

    fn declaration(&self, text: &str) -> Option<(usize, usize, &'static str)> {
        let rule = match self.language {
            Language::Perl => rx!(PERL_DECL, r"^\s*(package|sub)\s+([A-Za-z_][\w:]*)"),
            Language::PowerShell => rx!(
                PS_DECL,
                r"(?i)^\s*(class|enum|function|filter)\s+([A-Za-z_][\w:-]*)"
            ),
            Language::Java | Language::Groovy => rx!(
                JVM_DECL,
                r"^\s*(?:@[\w.]+(?:\([^)]*\))?\s+)*(?:(?:public|private|protected|static|final|abstract|sealed|non-sealed|strictfp)\s+)*(class|interface|enum|record|trait)\s+([\p{L}_$][\w$]*)"
            ),
            Language::JavaScript | Language::TypeScript => rx!(
                JS_DECL,
                r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(class|interface|enum|namespace|module)\s+([\p{L}_$][\w$.]*)"
            ),
            _ => return None,
        };
        if let Some(capture) = rule.captures(text) {
            let kind = match capture.get(1)?.as_str().to_ascii_lowercase().as_str() {
                "package" | "namespace" | "module" => "module",
                "sub" | "function" | "filter" => "function",
                _ => "class",
            };
            let name = capture.get(2)?;
            return Some((name.start(), name.end(), kind));
        }
        let rule = if self.language.jvm() {
            rx!(
                JVM_METHOD,
                r"^\s*(?:@[\w.]+(?:\([^)]*\))?\s+)*(?:(?:public|private|protected|static|final|abstract|synchronized|native|default|strictfp|override)\s+)*(?:<[^>]*>\s+)?(?:([\w.$]+(?:<[^()]*>)?(?:\[\])*)\s+)?([\p{L}_$][\w$]*)\s*\("
            )
        } else if self.language.javascript() {
            if let Some(capture) = rx!(JS_FUNCTION, r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:async\s+)?function\s*\*?\s*([\p{L}_$][\w$]*)\s*[(<]").captures(text) {
                let name = capture.get(1)?;
                return Some((name.start(), name.end(), "function"));
            }
            if text.contains("=>")
                && let Some(capture) = rx!(JS_ARROW, r"^\s*(?:export\s+)?(?:(?:const|let|var|public|private|protected|static|readonly|override)\s+)*(#?[\p{L}_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\([^;]*\)|[\p{L}_$][\w$]*)\s*(?::[^=]+)?=>").captures(text) {
                let name = capture.get(1)?;
                return Some((name.start(), name.end(), self.function_kind(self.owner())));
            }
            self.enclosing(self.owner(), true)?;
            rx!(
                JS_METHOD,
                r"^\s*(?:(?:public|private|protected|static|async|readonly|abstract|override|declare|get|set)\s+)*\*?\s*(#?[\p{L}_$][\w$]*)\s*[?!]?\s*(?:<[^>]*>)?\s*\("
            )
        } else if self.language == Language::PowerShell
            && self.enclosing(self.owner(), true).is_some()
        {
            rx!(
                PS_METHOD,
                r"(?i)^\s*(?:(?:static|hidden)\s+)*(?:\[[^\]]+\]\s*)?([A-Za-z_][\w-]*)\s*\("
            )
        } else {
            return None;
        };
        let capture = rule.captures(text)?;
        let name = capture.get(if self.language.jvm() { 2 } else { 1 })?;
        if self.language.jvm()
            && capture.get(1).is_some_and(|kind| {
                matches!(
                    kind.as_str(),
                    "return" | "throw" | "new" | "yield" | "assert"
                )
            })
        {
            return None;
        }
        if self.language.jvm()
            && capture.get(1).is_none()
            && !self
                .enclosing(self.owner(), true)
                .is_some_and(|owner| self.result.nodes[owner].name == name.as_str())
        {
            return None;
        }
        if control_word(name.as_str()) {
            return None;
        }
        Some((name.start(), name.end(), self.function_kind(self.owner())))
    }

    fn braces(&mut self, line: usize, text: &str) {
        if self
            .pending
            .is_some_and(|pending| line > self.result.nodes[pending.node].line + MAX_HEADER_LINES)
        {
            self.pending = None;
        }
        if let Some((start, end, mut kind)) = self.declaration(text) {
            if self.language == Language::Perl && kind == "module" {
                if let Some(previous) = self.package.take() {
                    self.result.nodes[previous].end_line = line.saturating_sub(1);
                }
            } else if kind == "function" {
                kind = self.function_kind(self.owner());
            }
            if let Some(node) = self.node(&text[start..end], kind, line, start, self.owner()) {
                if self.language == Language::Perl && kind == "module" && !text[end..].contains('{')
                {
                    self.package = Some(node);
                } else {
                    self.pending = Some(Pending {
                        node,
                        after: end,
                        groups: 0,
                    });
                }
            }
        }
        for (column, byte) in text.bytes().enumerate() {
            if let Some(pending) = &mut self.pending
                && (line > self.result.nodes[pending.node].line || column >= pending.after)
            {
                match byte {
                    b'(' | b'[' => pending.groups += 1,
                    b')' | b']' => pending.groups = pending.groups.saturating_sub(1),
                    b'{' if pending.groups == 0 => {
                        self.scopes.push((pending.node, self.depth));
                        self.pending = None;
                    }
                    b';' if pending.groups == 0 => self.pending = None,
                    _ => {}
                }
            }
            match byte {
                b'{' => self.depth += 1,
                b'}' => {
                    self.depth = self.depth.saturating_sub(1);
                    while self
                        .scopes
                        .last()
                        .is_some_and(|(_, depth)| *depth >= self.depth)
                    {
                        let (node, _) = self.scopes.pop().unwrap();
                        self.result.nodes[node].end_line = line;
                    }
                }
                _ => {}
            }
        }
        // Expression arrows and abstract declarations are single-line leaves.
        if text.contains("=>") && self.pending.is_some() {
            self.pending = None;
        }
    }

    fn ruby(&mut self, line: usize, text: &str) {
        let capture = rx!(
            RUBY_DECL,
            r"^\s*(class|module|def)\s+(?:self\.)?([\p{L}_][\w:]*[?!=]?)"
        )
        .captures(text);
        let mut declaration = None;
        if let Some(capture) = capture {
            let name = capture.get(2).unwrap();
            let kind = match &capture[1] {
                "class" => "class",
                "module" => "module",
                _ => self.function_kind(self.owner()),
            };
            declaration = self.node(name.as_str(), kind, line, name.start(), self.owner());
        }
        let trimmed = text.trim_start();
        let opens_control =
            rx!(RUBY_CONTROL, r"^(?:if|unless|case|begin|while|until|for)\b").is_match(trimmed);
        if declaration.is_some() || opens_control {
            self.push_block(declaration);
        }
        for word in rx!(RUBY_BLOCK, r"\b(do|end)\b").find_iter(text) {
            if word.as_str() == "do" {
                if !opens_control {
                    self.push_block(None);
                }
            } else if let Some(Some(node)) = self.pop_block() {
                self.result.nodes[node].end_line = line;
            }
        }
        if let Some(node) = declaration
            && text.contains('=')
            && !text.contains(';')
            && rx!(RUBY_ENDLESS, r"^\s*(?:\([^)]*\))?\s*=")
                .is_match(&text[self.result.nodes[node].end_column..])
        {
            // Ruby's endless method form has no closing end.
            self.pop_block();
        }
    }

    fn push_block(&mut self, node: Option<usize>) {
        if self.blocks.len() < MAX_DEPTH {
            self.blocks.push(node);
        } else {
            self.result.limited = true;
            self.block_overflow += 1;
        }
    }

    fn pop_block(&mut self) -> Option<Option<usize>> {
        if self.block_overflow > 0 {
            self.block_overflow -= 1;
            None
        } else {
            self.blocks.pop()
        }
    }

    fn lua(&mut self, line: usize, text: &str) {
        if self.blocks.is_empty()
            && let Some(name)=rx!(LUA_TABLE, r"^\s*(?:local\s+)?([A-Za-z_][\w]*)\s*=\s*(?:\{|setmetatable\s*\(\s*\{|[\w]+\s+or\s+\{)").captures(text).and_then(|capture|capture.get(1))
            && self.table_names.contains(name.as_str())
            && let Some(node)=self.node(name.as_str(),"module",line,name.start(),None) {
            self.tables.insert(name.as_str().into(),node);
        }
        for word in rx!(LUA_BLOCK, r"\b(function|if|do|repeat|end|until)\b").find_iter(text) {
            match word.as_str() {
                "function" => {
                    let tail = &text[word.end()..];
                    let named = rx!(
                        LUA_NAMED,
                        r"^\s+([A-Za-z_][\w]*(?:[.:][A-Za-z_][\w]*)*)\s*\("
                    )
                    .captures(tail)
                    .and_then(|capture| capture.get(1))
                    .map(|name| (word.end() + name.start(), word.end() + name.end()));
                    let assigned = rx!(
                        LUA_ASSIGNED,
                        r"(?:^|[\s,{;(])(?:local\s+)?([A-Za-z_][\w]*(?:\.[A-Za-z_][\w]*)*)\s*=\s*$"
                    )
                    .captures(&text[..word.start()])
                    .and_then(|capture| capture.get(1))
                    .map(|name| (name.start(), name.end()));
                    let node = named.or(assigned).and_then(|(start, end)| {
                        let full = &text[start..end];
                        let parent = full
                            .split_once(['.', ':'])
                            .and_then(|(table, _)| self.tables.get(table).copied())
                            .or_else(|| self.owner());
                        let short_start = full
                            .rfind(['.', ':'])
                            .map_or(start, |index| start + index + 1);
                        if full.contains(':')
                            && let Some(parent) = parent
                        {
                            self.result.nodes[parent].kind = "class";
                        }
                        self.node(
                            &text[short_start..end],
                            self.function_kind(parent),
                            line,
                            short_start,
                            parent,
                        )
                    });
                    self.push_block(node);
                }
                "if" | "do" | "repeat" => self.push_block(None),
                "end" | "until" => {
                    if let Some(Some(node)) = self.pop_block() {
                        self.result.nodes[node].end_line = line;
                        if let Some(parent) = self.result.nodes[node].parent {
                            self.result.nodes[parent].end_line =
                                self.result.nodes[parent].end_line.max(line);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn owners(&self, length: usize) -> Vec<Option<usize>> {
        let mut starts: Vec<_> = (0..self.result.nodes.len()).collect();
        starts.sort_by_key(|&index| self.result.nodes[index].line);
        let mut next = starts.into_iter().peekable();
        let mut active = BinaryHeap::new();
        (0..length)
            .map(|line| {
                while let Some(index) = next.next_if(|&index| self.result.nodes[index].line <= line)
                {
                    active.push((
                        self.node_depth(Some(index)),
                        self.result.nodes[index].line,
                        index,
                    ));
                }
                while active
                    .peek()
                    .is_some_and(|&(_, _, index)| self.result.nodes[index].end_line < line)
                {
                    active.pop();
                }
                active.peek().map(|&(_, _, index)| index)
            })
            .collect()
    }

    fn binding(
        &mut self,
        name: &str,
        kind: &'static str,
        line: usize,
        column: usize,
        scope: Option<usize>,
    ) {
        if name.is_empty() || control_word(name) {
            return;
        }
        let key = self.language.folded(canonical(name));
        if !self.defined.insert((scope, key)) {
            return;
        }
        if self.result.variables.len() == MAX_ENTRIES {
            self.result.limited = true;
            return;
        }
        self.result.variables.push(Variable {
            name: name.into(),
            kind,
            container: None,
            line,
            column,
            end_column: column + name.len(),
            scope: scope.map_or_else(String::new, |index| {
                self.result.nodes[index].qualified.clone()
            }),
            stage_index: None,
            uses: 0,
        });
        self.variable_scopes.push(scope);
    }

    fn variable_kind(&self, name: &str, scope: Option<usize>, constant: bool) -> &'static str {
        if constant
            || name.chars().any(char::is_alphabetic)
                && name
                    .chars()
                    .filter(|ch| ch.is_alphabetic())
                    .all(char::is_uppercase)
        {
            "constant"
        } else if scope
            .is_some_and(|scope| matches!(self.result.nodes[scope].kind, "class" | "module"))
        {
            "field"
        } else if name.starts_with('@') && self.language == Language::Perl {
            "array"
        } else if name.starts_with('%') && self.language == Language::Perl {
            "hash"
        } else {
            "variable"
        }
    }

    fn parameters(&mut self, masked: &[lexer::Line]) {
        for node in 0..self.result.nodes.len() {
            if !matches!(self.result.nodes[node].kind, "function" | "method") {
                continue;
            }
            let start_line = self.result.nodes[node].line;
            let after = self.result.nodes[node].end_column;
            let mut open = false;
            let mut nesting = 0usize;
            let mut argument = String::new();
            let mut sites = Vec::new();
            'header: for (line, text) in masked
                .iter()
                .enumerate()
                .skip(start_line)
                .take(MAX_HEADER_LINES + 1)
            {
                let offset = if line == start_line { after } else { 0 };
                for (column, character) in text
                    .code
                    .char_indices()
                    .filter(|(column, _)| *column >= offset)
                {
                    if !open {
                        if character == '(' {
                            open = true;
                        } else if matches!(character, '{' | ';') {
                            break 'header;
                        }
                        continue;
                    }
                    if nesting == 0 && matches!(character, ',' | ')') {
                        self.parameter(&argument, &sites, node);
                        argument.clear();
                        sites.clear();
                        if character == ')' {
                            break 'header;
                        }
                    } else {
                        if matches!(character, '(' | '[' | '{' | '<') {
                            nesting += 1;
                        }
                        if matches!(character, ')' | ']' | '}' | '>') {
                            nesting = nesting.saturating_sub(1);
                        }
                        argument.push(character);
                        sites.extend(std::iter::repeat_n((line, column), character.len_utf8()));
                    }
                }
                if open {
                    argument.push(' ');
                    sites.push((line, text.code.len()));
                }
            }
            if self.language.javascript() && !open {
                let text = &masked[start_line].code;
                if let Some(capture) =
                    rx!(ARROW_PARAM, r"=\s*(?:async\s+)?([\p{L}_$][\w$]*)\s*=>").captures(text)
                {
                    let name = capture.get(1).unwrap();
                    self.binding(
                        name.as_str(),
                        "parameter",
                        start_line,
                        name.start(),
                        Some(node),
                    );
                }
            }
        }
    }

    fn parameter(&mut self, text: &str, sites: &[(usize, usize)], node: usize) {
        let text = text.split('=').next().unwrap_or(text);
        if self.language.javascript() && text.trim_start().starts_with(['{', '[']) {
            for (offset, name) in tokens(text, self.language) {
                if text[offset + name.len()..].trim_start().starts_with(':') {
                    continue;
                }
                if let Some(&(line, column)) = sites.get(offset) {
                    self.binding(name, "parameter", line, column, Some(node));
                }
            }
            return;
        }
        let tokens: Vec<_> = tokens(text, self.language).collect();
        let selected = if self.language.jvm() {
            tokens.last().copied()
        } else {
            tokens.first().copied()
        };
        if let Some((offset, name)) = selected.filter(|(_, name)| !control_word(name))
            && let Some(&(line, column)) = sites.get(offset)
        {
            self.binding(name, "parameter", line, column, Some(node));
        }
    }

    fn powershell_parameters(&mut self, masked: &[lexer::Line], owners: &[Option<usize>]) {
        let mut nesting = 0usize;
        let mut owner = None;
        for (line, text) in masked.iter().enumerate() {
            let mut start = 0;
            if nesting == 0 {
                let Some(found) = rx!(PS_PARAM, r"(?i)\bparam\s*\(").find(&text.code) else {
                    continue;
                };
                start = found.end();
                nesting = 1;
                owner = self.enclosing(owners[line], false);
            }
            let mut end = text.code.len();
            for (offset, byte) in text.code.bytes().enumerate().skip(start) {
                if byte == b'(' {
                    nesting += 1;
                }
                if byte == b')' {
                    nesting = nesting.saturating_sub(1);
                    if nesting == 0 {
                        end = offset;
                        break;
                    }
                }
            }
            for (offset, name) in tokens(&text.code[start..end], self.language) {
                self.binding(name, "parameter", line, start + offset, owner);
            }
        }
    }

    fn variables(&mut self, line: usize, text: &str, owner: Option<usize>) {
        let function = self.enclosing(owner, false);
        let class = self.enclosing(owner, true);
        if self.language == Language::Perl {
            if let Some(capture) =
                rx!(PERL_CONSTANT, r"^\s*use\s+constant\s+([A-Za-z_]\w*)\s*=>").captures(text)
            {
                let name = capture.get(1).unwrap();
                self.binding(name.as_str(), "constant", line, name.start(), None);
            }
            for capture in rx!(
                PERL_LOCAL,
                r"\b(my|our|local)\s+(\([^)]*\)|[$@%][A-Za-z_]\w*)"
            )
            .captures_iter(text)
            {
                let part = capture.get(2).unwrap();
                for (offset, name) in tokens(part.as_str(), self.language) {
                    let scope = if &capture[1] == "our" { None } else { function };
                    self.binding(
                        name,
                        self.variable_kind(name, None, false),
                        line,
                        part.start() + offset,
                        scope,
                    );
                }
            }
        } else if self.language == Language::PowerShell {
            for capture in rx!(PS_VARIABLE, r"(?i)(\$(?:(?:script|global|local|private|env|using):)?[A-Za-z_]\w*)\s*[+]?=(?:[^=]|$)").captures_iter(text) {
                let name = capture.get(1).unwrap();
                let prefix = &text[..name.start()];
                let parameter = prefix.to_ascii_lowercase().contains("param(");
                let global = name.as_str().to_ascii_lowercase().starts_with("$global:") || name.as_str().to_ascii_lowercase().starts_with("$script:");
                let exported = name.as_str().to_ascii_lowercase().starts_with("$env:");
                let scope = if global || exported { None } else { function.or(class) };
                let kind = if exported { "exported" } else if parameter { "parameter" } else { self.variable_kind(name.as_str(), scope, false) };
                self.binding(name.as_str(), kind, line, name.start(), scope);
            }
            if function.is_none() && class.is_some()
                && let Some(capture) = rx!(
                    PS_FIELD,
                    r"(?i)^\s*(?:(?:hidden|static)\s+)*\[[^]]+\]\s*(\$[A-Za-z_]\w*)"
                )
                .captures(text)
                {
                    let name = capture.get(1).unwrap();
                    self.binding(name.as_str(), "field", line, name.start(), class);
            }
        } else if self.language == Language::Ruby {
            for capture in rx!(
                RUBY_VAR,
                r"(@@?[\p{L}_]\w*|\$[\p{L}_]\w*|[\p{L}_]\w*)\s*(?:[+*/-]?=)(?:[^=~>]|$)"
            )
            .captures_iter(text)
            {
                let name = capture.get(1).unwrap();
                let scope = if name.as_str().starts_with('@') {
                    class
                } else if name.as_str().starts_with('$')
                    || name.as_str().chars().next().is_some_and(char::is_uppercase)
                {
                    None
                } else {
                    function
                };
                self.binding(
                    name.as_str(),
                    self.variable_kind(name.as_str(), scope, false),
                    line,
                    name.start(),
                    scope,
                );
            }
        } else if self.language == Language::Lua {
            if let Some(capture) =
                rx!(LUA_FIELD, r"^\s*([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*=\s*(.*)$").captures(text)
                && !capture[3].trim_start().starts_with("function")
            {
                    let scope = if &capture[1] == "self" {
                        class
                    } else {
                        self.tables.get(&capture[1]).copied()
                    };
                    if let Some(scope) = scope {
                        let name = capture.get(2).unwrap();
                        self.binding(
                            name.as_str(),
                            self.variable_kind(name.as_str(), Some(scope), false),
                            line,
                            name.start(),
                            Some(scope),
                        );
                    }
            }
            if let Some(capture) = rx!(
                LUA_VAR,
                r"^\s*(local\s+)?([A-Za-z_]\w*(?:\s*,\s*[A-Za-z_]\w*)*)\s*(?:<const>\s*)?=\s*(.*)$"
            )
            .captures(text)
                && !capture[3].trim_start().starts_with("function")
            {
                    let names = capture.get(2).unwrap();
                    for (offset, name) in tokens(names.as_str(), self.language) {
                        if self.tables.contains_key(name) {
                            continue;
                        }
                        let scope = if capture.get(1).is_some()
                            || self
                                .defined
                                .contains(&(function, self.language.folded(canonical(name))))
                        {
                            function
                        } else {
                            None
                        };
                        let kind = self.variable_kind(name, scope, text.contains("<const>"));
                        self.binding(name, kind, line, names.start() + offset, scope);
                    }
            }
            if let Some(capture) = rx!(
                LUA_FOR,
                r"\bfor\s+([A-Za-z_]\w*(?:\s*,\s*[A-Za-z_]\w*)*)\s*(?:in|=)"
            )
            .captures(text)
            {
                let names = capture.get(1).unwrap();
                for (offset, name) in tokens(names.as_str(), self.language) {
                    self.binding(name, "variable", line, names.start() + offset, function);
                }
            }
        } else if self.language.javascript() {
            if let Some(capture) = rx!(
                JS_VAR,
                r"^\s*(?:export\s+)?(const|let|var)\s+([\p{L}_$][\w$]*)\s*(?::[^=;]+)?\s*[=;]"
            )
            .captures(text)
            {
                let name = capture.get(2).unwrap();
                if !self
                    .result
                    .nodes
                    .iter()
                    .any(|node| node.line == line && node.column == name.start())
                {
                    let scope = function.or(class);
                    self.binding(
                        name.as_str(),
                        self.variable_kind(name.as_str(), scope, false),
                        line,
                        name.start(),
                        scope,
                    );
                }
            }
            if let Some(capture) = rx!(JS_FIELD, r"^\s*(?:(?:public|private|protected|static|readonly|declare|override|accessor)\s+)*(#?[\p{L}_$][\w$]*)\s*[?!]?\s*(?::[^=;(]+)?[=;]").captures(text) {
                let name = capture.get(1).unwrap();
                if function.is_none() && class.is_some() { self.binding(name.as_str(), self.variable_kind(name.as_str(), class, false), line, name.start(), class); }
            }
            for capture in rx!(
                JS_THIS,
                r"\bthis\.(#?[\p{L}_$][\w$]*)\s*(?:[+*/-]?=)(?:[^=>]|$)"
            )
            .captures_iter(text)
            {
                let name = capture.get(1).unwrap();
                self.binding(name.as_str(), "field", line, name.start(), class);
            }
        } else if self.language.jvm()
            && let Some(capture) = rx!(JVM_VAR, r"^\s*(?:(?:public|private|protected|static|final|transient|volatile|def)\s+)*([\w.$]+(?:<[^()]*>)?(?:\[\])*)\s+([\p{L}_$][\w$]*)\s*(?:\[\s*\]\s*)*([=;,])").captures(text)
            && !control_word(&capture[1]) {
                    let scope = function.or(class);
                    let name = capture.get(2).unwrap();
                    self.binding(name.as_str(), self.variable_kind(name.as_str(), scope, text.contains(" final ")), line, name.start(), scope);
                    if &capture[3] == "," {
                        let after = capture.get(0).unwrap().end();
                        for name in rx!(MORE_JVM, r"([\p{L}_$][\w$]*)\s*[,;]").captures_iter(&text[after..]) {
                            let name = name.get(1).unwrap(); self.binding(name.as_str(), self.variable_kind(name.as_str(), scope, false), line, after + name.start(), scope);
                        }
                    }
        }
    }

    fn count_references(&mut self, masked: &[lexer::Line]) {
        let mut starts: Vec<_> = (0..self.result.variables.len()).collect();
        starts.sort_by_key(|&index| {
            self.variable_scopes[index].map_or(0, |scope| self.result.nodes[scope].line)
        });
        let mut starts = starts.into_iter().peekable();
        let mut active: HashMap<String, Vec<usize>> = HashMap::new();
        let mut expiry = BinaryHeap::new();
        for (line, text) in masked.iter().enumerate() {
            while let Some(index) = starts.next_if(|&index| {
                self.variable_scopes[index].map_or(0, |scope| self.result.nodes[scope].line) <= line
            }) {
                let key = self
                    .language
                    .folded(canonical(&self.result.variables[index].name));
                let entries = active.entry(key).or_default();
                entries.push(index);
                entries.sort_by_key(|&index| self.node_depth(self.variable_scopes[index]));
                if let Some(scope) = self.variable_scopes[index] {
                    expiry.push(Reverse((self.result.nodes[scope].end_line, index)));
                }
            }
            while let Some(&Reverse((end, index))) = expiry.peek() {
                if end >= line {
                    break;
                }
                expiry.pop();
                let key = self
                    .language
                    .folded(canonical(&self.result.variables[index].name));
                if let Some(entries) = active.get_mut(&key) {
                    entries.retain(|&active| active != index);
                }
            }
            for (column, name) in tokens(&text.references, self.language) {
                let key = self.language.folded(canonical(name));
                let Some(entries) = active.get(&key) else {
                    continue;
                };
                let before = text.references[..column].trim_end();
                // Perl's dot concatenates values and cannot qualify a member.
                let dotted = self.language != Language::Perl && before.ends_with('.');
                let after = text.references[column + name.len()..].trim_start();
                if !matches!(
                    self.language,
                    Language::Ruby | Language::Perl | Language::PowerShell
                ) && after.starts_with('(')
                {
                    continue;
                }
                let index = entries
                    .iter()
                    .rev()
                    .find(|&&index| {
                        !dotted
                            || self.result.variables[index].kind == "field"
                            || self.result.variables[index].kind == "constant"
                    })
                    .copied();
                if let Some(index) = index {
                    let variable = &mut self.result.variables[index];
                    if (line, column) != (variable.line, variable.column) {
                        variable.uses += 1;
                    }
                }
            }
        }
    }
}

fn canonical(name: &str) -> &str {
    name.trim_start_matches(['$', '@', '%', '#'])
        .rsplit(':')
        .next()
        .unwrap_or(name)
}

fn control_word(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "switch"
            | "catch"
            | "synchronized"
            | "return"
            | "new"
            | "throw"
            | "else"
            | "do"
            | "try"
            | "super"
            | "this"
            | "self"
            | "assert"
            | "foreach"
            | "until"
            | "elseif"
            | "function"
            | "param"
            | "process"
            | "begin"
            | "end"
            | "using"
            | "with"
            | "await"
            | "typeof"
            | "delete"
            | "void"
            | "yield"
            | "case"
            | "when"
            | "in"
            | "of"
            | "not"
            | "and"
            | "or"
            | "class"
            | "module"
            | "package"
            | "import"
            | "export"
    )
}

fn tokens(text: &str, language: Language) -> impl Iterator<Item = (usize, &str)> {
    static WORDS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[@$%#]?[\p{L}_$][\w$]*").unwrap());
    static POWERSHELL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\$(?:(?:script|global|local|private|env|using):)?[\p{L}_][\w]*").unwrap()
    });
    let pattern = if language == Language::PowerShell {
        &*POWERSHELL
    } else {
        &*WORDS
    };
    pattern
        .find_iter(text)
        .filter(move |token| {
            !matches!(language, Language::Perl | Language::PowerShell)
                || token.as_str().starts_with(['$', '@', '%'])
        })
        .map(|token| (token.start(), token.as_str()))
}
