//! Names bound by Python loops and bracketed comprehensions.

use std::collections::HashMap;

use super::targets::{MAX_GROUPS, Pattern, Site, Step, Token, tokens};
use super::{Language, Scan};
use crate::editor_window::structure::MAX_ENTRIES;

#[derive(Default)]
pub(in crate::editor_window::structure) struct Loops {
    frames: Vec<Frame>,
    excess_groups: usize,
    target: Option<Target>,
    pub(super) comprehensions: Vec<Comprehension>,
}

struct Frame {
    open: Site,
    close: char,
    comprehension: Option<usize>,
}

pub(super) struct Comprehension {
    pub(super) open: Site,
    pub(super) end: Site,
    pub(super) first_iterable: Site,
    pub(super) next_clause: Option<Site>,
    pub(super) names: HashMap<String, usize>,
}

impl Loops {
    pub(super) fn line(
        &mut self,
        scan: &mut Scan,
        line: usize,
        code: &str,
        starts_in_code: bool,
        scope: Option<usize>,
    ) {
        for (column, token) in tokens(code) {
            let site = (line, column);
            if matches!(token, Token::Punct('\\')) && column + 1 == code.len() {
                continue;
            }
            if let Some(mut target) = self.target.take() {
                let step = if matches!(token, Token::Word("in")) && target.pattern.complete() {
                    Step::Complete
                } else {
                    target.pattern.read(token, site)
                };
                match step {
                    Step::Continue => self.target = Some(target),
                    Step::Complete => self.bind(scan, target, (line, column + 2)),
                    Step::TooLarge => scan.analysis.limited = true,
                    Step::Invalid => {}
                }
            }
            if let Some(close) = token.close() {
                if self.frames.len() == MAX_GROUPS {
                    self.excess_groups += 1;
                } else {
                    self.frames.push(Frame {
                        open: site,
                        close,
                        comprehension: None,
                    });
                }
            } else if let Token::Punct(close @ (')' | ']' | '}')) = token {
                if self.excess_groups > 0 {
                    self.excess_groups -= 1;
                } else if self.frames.last().is_some_and(|frame| frame.close == close)
                    && let Some(frame) = self.frames.pop()
                    && let Some(id) = frame.comprehension
                {
                    self.comprehensions[id].end = site;
                }
            } else if self.excess_groups == 0 && matches!(token, Token::Word("for" | "if")) {
                if let Some(id) = self.frames.last().and_then(|frame| frame.comprehension) {
                    self.comprehensions[id].next_clause.get_or_insert(site);
                }
                if matches!(token, Token::Word("for")) && self.target.is_none() {
                    let at_statement =
                        starts_in_code && matches!(code[..column].trim(), "" | "async");
                    if at_statement || !self.frames.is_empty() {
                        self.target = Some(Target::new(scope, self.frames.len()));
                    }
                }
            }
        }
        if self.frames.is_empty() && !code.ends_with('\\') {
            self.target = None;
        }
    }

    fn bind(&mut self, scan: &mut Scan, target: Target, iterable: Site) {
        if target.depth == 0 {
            let kind = super::local_kind(scan, target.scope);
            for (name, (line, column)) in target.pattern.names {
                scan.binding(
                    (kind, None),
                    &name,
                    line,
                    column,
                    target.scope,
                    Language::Python,
                );
            }
            return;
        }
        let Some(frame) = self.frames.get_mut(target.depth - 1) else {
            return;
        };
        let id = match frame.comprehension {
            Some(id) => id,
            None if self.comprehensions.len() == MAX_ENTRIES => {
                scan.analysis.limited = true;
                return;
            }
            None => {
                let id = self.comprehensions.len();
                self.comprehensions.push(Comprehension {
                    open: frame.open,
                    end: (usize::MAX, 0),
                    first_iterable: iterable,
                    next_clause: None,
                    names: HashMap::new(),
                });
                frame.comprehension = Some(id);
                id
            }
        };
        let comprehension = &mut self.comprehensions[id];
        for (name, (line, column)) in target.pattern.names {
            if !comprehension.names.contains_key(&name)
                && let Some(binding) = scan.push_binding(
                    ("variable", None),
                    &name,
                    line,
                    column,
                    target.scope,
                    Language::Python,
                )
            {
                comprehension.names.insert(name, binding);
            }
        }
    }
}

struct Target {
    scope: Option<usize>,
    depth: usize,
    pattern: Pattern,
}

impl Target {
    fn new(scope: Option<usize>, depth: usize) -> Self {
        Self {
            scope,
            depth,
            pattern: Pattern::default(),
        }
    }
}
