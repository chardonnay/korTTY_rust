//! Tokens and name patterns shared by Python binding statements.

use super::identifier;

pub(in crate::editor_window::structure) type Site = (usize, usize);
pub(super) type Names = Vec<(String, Site)>;

pub(super) const MAX_GROUPS: usize = 128;
pub(super) const MAX_TARGETS: usize = 128;

pub(super) struct Pattern {
    groups: Vec<char>,
    pub(super) names: Names,
    expect_name: bool,
    starred: bool,
}

pub(super) enum Step {
    Continue,
    Complete,
    Invalid,
    TooLarge,
}

impl Default for Pattern {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            names: Vec::new(),
            expect_name: true,
            starred: false,
        }
    }
}

impl Pattern {
    pub(super) fn complete(&self) -> bool {
        self.groups.is_empty() && !self.names.is_empty() && !self.starred
    }

    pub(super) fn read(&mut self, token: Token<'_>, site: Site) -> Step {
        match token {
            Token::Word(name) if self.expect_name && !keyword(name) => {
                if self.names.len() == MAX_TARGETS {
                    return Step::TooLarge;
                }
                self.names.push((name.into(), site));
                self.expect_name = false;
                self.starred = false;
            }
            Token::Punct('(' | '[') if self.expect_name => {
                if self.groups.len() == MAX_GROUPS {
                    return Step::TooLarge;
                }
                self.groups.extend(token.close());
                self.starred = false;
            }
            Token::Punct(close @ (')' | ']'))
                if !self.starred && self.groups.last() == Some(&close) =>
            {
                self.groups.pop();
                self.expect_name = false;
            }
            Token::Punct('*') if self.expect_name && !self.starred => self.starred = true,
            Token::Punct(',') if !self.expect_name => self.expect_name = true,
            _ => return Step::Invalid,
        }
        Step::Continue
    }
}

fn keyword(name: &str) -> bool {
    matches!(
        name,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
    )
}

#[derive(Clone, Copy)]
pub(super) enum Token<'a> {
    Word(&'a str),
    Punct(char),
}

impl Token<'_> {
    pub(super) fn close(self) -> Option<char> {
        match self {
            Self::Punct('(') => Some(')'),
            Self::Punct('[') => Some(']'),
            Self::Punct('{') => Some('}'),
            _ => None,
        }
    }
}

pub(super) fn tokens(code: &str) -> impl Iterator<Item = (usize, Token<'_>)> {
    let mut remaining = code;
    std::iter::from_fn(move || {
        remaining = remaining.trim_start();
        let column = code.len() - remaining.len();
        if let Some((word, tail)) = identifier(remaining) {
            remaining = tail;
            Some((column, Token::Word(word)))
        } else {
            let ch = remaining.chars().next()?;
            remaining = &remaining[ch.len_utf8()..];
            Some((column, Token::Punct(ch)))
        }
    })
}
