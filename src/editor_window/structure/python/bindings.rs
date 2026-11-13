//! Python assignment targets and aliases that can span physical lines.

use super::targets::{MAX_GROUPS, MAX_TARGETS, Names, Pattern, Site, Step, Token, tokens};
use super::{Language, Scan, identifier, local_kind};

const AUGMENTED: [&str; 13] = [
    "+=", "-=", "*=", "/=", "%=", "@=", "&=", "|=", "^=", "//=", "**=", "<<=", ">>=",
];

pub(super) fn augmented(tail: &str) -> bool {
    let tail = tail.trim_start();
    AUGMENTED
        .into_iter()
        .any(|operator| tail.starts_with(operator))
}

pub(super) fn right_hand_side(tail: &str) -> Option<&str> {
    let tail = tail.trim_start();
    if let Some(annotation) = tail.strip_prefix(':') {
        let mut depth = 0_usize;
        for (column, token) in tokens(annotation) {
            match token {
                Token::Punct('(' | '[' | '{') => depth += 1,
                Token::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
                Token::Punct('=')
                    if depth == 0
                        && column > 0
                        && !annotation[column..].starts_with("==")
                        && !annotation[..column].ends_with(['=', '!', '<', '>', ':']) =>
                {
                    return Some(&annotation[column + 1..]);
                }
                _ => {}
            }
        }
        return None;
    }
    if tail.starts_with("==") {
        return None;
    }
    std::iter::once("=")
        .chain(AUGMENTED)
        .find_map(|operator| tail.strip_prefix(operator))
}

pub(super) fn shape(rhs: &str) -> Option<&'static str> {
    let (constructor, arguments) = identifier(rhs.trim_start())?;
    if !arguments.trim_start().starts_with('(') {
        return None;
    }
    match constructor {
        "dict" => Some("hash"),
        "tuple" | "list" | "set" | "frozenset" => Some("array"),
        _ => None,
    }
}

pub(super) struct Statement {
    scope: Option<usize>,
    body: Body,
}

enum Body {
    Assignment(Pattern),
    Header(Header),
}

impl Statement {
    pub(super) fn start(
        scan: &mut Scan,
        line: usize,
        code: &str,
        scope: Option<usize>,
    ) -> Option<Self> {
        let code_start = code.trim_start();
        let lead = identifier(code_start);
        let context = match lead {
            Some(("with", tail)) => Some((true, tail)),
            Some(("except", tail)) => Some((false, tail)),
            Some(("async", tail)) => identifier(tail.trim_start())
                .filter(|(word, _)| *word == "with")
                .map(|(_, tail)| (true, tail)),
            _ => None,
        };
        let (body, tail) = match context {
            Some((with, tail)) => (Body::Header(Header::new(with)), tail),
            None => (Body::Assignment(Pattern::default()), code_start),
        };
        let mut statement = Self { scope, body };
        statement
            .read_line(scan, line, code, code.len() - tail.len())
            .then_some(statement)
    }

    pub(super) fn read_line(
        &mut self,
        scan: &mut Scan,
        line: usize,
        code: &str,
        start: usize,
    ) -> bool {
        for (offset, token) in tokens(&code[start..]) {
            let column = start + offset;
            if matches!(token, Token::Punct('\\')) && column + 1 == code.len() {
                continue;
            }
            let site = (line, column);
            let step = match &mut self.body {
                Body::Assignment(pattern)
                    if matches!(token, Token::Punct('='))
                        && !code[column..].starts_with("==")
                        && pattern.complete() =>
                {
                    Step::Complete
                }
                Body::Assignment(pattern) => pattern.read(token, site),
                Body::Header(header) => header.read(token, site),
            };
            match step {
                Step::Continue => continue,
                Step::Invalid => {}
                Step::TooLarge => scan.analysis.limited = true,
                Step::Complete => {
                    let names = match &mut self.body {
                        Body::Assignment(pattern) => &mut pattern.names,
                        Body::Header(header) => &mut header.names,
                    };
                    let kind = local_kind(scan, self.scope);
                    for (name, (line, column)) in names.drain(..) {
                        scan.binding(
                            (kind, None),
                            &name,
                            line,
                            column,
                            self.scope,
                            Language::Python,
                        );
                    }
                }
            }
            return false;
        }
        true
    }
}

struct Header {
    with: bool,
    first: bool,
    wrapped: bool,
    groups: Vec<char>,
    alias: Option<Pattern>,
    names: Names,
}

impl Header {
    fn new(with: bool) -> Self {
        Self {
            with,
            first: true,
            wrapped: false,
            groups: Vec::new(),
            alias: None,
            names: Vec::new(),
        }
    }

    fn read(&mut self, token: Token<'_>, site: Site) -> Step {
        let depth = self.groups.len();
        let in_item = depth == usize::from(self.wrapped);
        if let Some(alias) = &mut self.alias {
            let ends_item = match token {
                Token::Punct(':') => depth == 0,
                Token::Punct(',') => self.with && in_item,
                Token::Punct(')') => self.wrapped && in_item,
                _ => false,
            };
            if !ends_item || !alias.complete() {
                return alias.read(token, site);
            }
            if self.names.len() + alias.names.len() > MAX_TARGETS {
                return Step::TooLarge;
            }
            self.names.append(&mut alias.names);
            self.alias = None;
        }
        if self.first {
            // Parenthesized with-items keep their aliases one level deeper.
            self.wrapped = self.with && matches!(token, Token::Punct('('));
            self.first = false;
        }
        if let Some(close) = token.close() {
            if depth == MAX_GROUPS {
                return Step::TooLarge;
            }
            self.groups.push(close);
        } else {
            match token {
                Token::Punct(close @ (')' | ']' | '}')) => {
                    if self.groups.pop() != Some(close) {
                        return Step::Invalid;
                    }
                    if self.groups.is_empty() {
                        self.wrapped = false;
                    }
                }
                Token::Word("as") if in_item => self.alias = Some(Pattern::default()),
                Token::Punct(':') if depth == 0 => return Step::Complete,
                _ => {}
            }
        }
        Step::Continue
    }
}
