//! Binding paths for destructuring and indexed types.

use std::{borrow::Cow, sync::LazyLock};

use regex::Regex;

use super::{
    MAX_LOOKUPS, Resolved, Token, Type, after_group, comma_groups, generics, identifier,
    instance_members, numeric_keys, resolve,
};

pub(super) enum Selection<'a> {
    Field(&'a str),
    OwnedField(String),
    Index(usize),
    NumericKey(String),
    Variable(&'a str, bool),
    ObjectRest(Vec<&'a str>),
    ArrayRest(usize),
    Unsupported,
}

impl Selection<'_> {
    pub(super) fn property_name(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Field(key) => Some(Cow::Borrowed(key)),
            Self::OwnedField(key) => Some(Cow::Borrowed(key)),
            Self::Index(index) => Some(Cow::Owned(index.to_string())),
            Self::NumericKey(key) => Some(Cow::Borrowed(key)),
            _ => None,
        }
    }
}

pub(super) fn path<'source>(
    tokens: &[Token<'source>],
    receiver: &str,
) -> Option<Vec<Selection<'source>>> {
    let mut path = find(tokens, receiver, MAX_LOOKUPS)?;
    path.reverse();
    Some(path)
}

fn find<'source>(
    tokens: &[Token<'source>],
    receiver: &str,
    depth: usize,
) -> Option<Vec<Selection<'source>>> {
    let depth = depth.checked_sub(1)?;
    match tokens.first()?.text {
        "{" | "[" => {
            let object = tokens[0].text == "{";
            let close = if object { "}" } else { "]" };
            let end = after_group(tokens, tokens[0].text, close)?;
            let mut used_keys = Vec::new();
            let mut unresolved_key = false;
            for (index, entry) in comma_groups(&tokens[1..end - 1], false).enumerate() {
                let (selection, child) = if let Some(rest) = rest_pattern(entry) {
                    let selection = if !object {
                        Selection::ArrayRest(index)
                    } else if unresolved_key {
                        Selection::Unsupported
                    } else {
                        Selection::ObjectRest(std::mem::take(&mut used_keys))
                    };
                    (selection, rest)
                } else if object {
                    let Some(parsed) = object_entry(entry) else {
                        continue;
                    };
                    match parsed.0 {
                        Selection::Field(key) => used_keys.push(key),
                        _ => unresolved_key = true,
                    }
                    parsed
                } else {
                    (Selection::Index(index), entry)
                };
                if let Some(mut child_path) = find(child, receiver, depth) {
                    child_path.push(selection);
                    return Some(child_path);
                }
            }
            None
        }
        name if identifier(name) && name == receiver => Some(Vec::new()),
        _ => None,
    }
}

fn rest_pattern<'tokens, 'source>(
    tokens: &'tokens [Token<'source>],
) -> Option<&'tokens [Token<'source>]> {
    let [first, second, third, rest @ ..] = tokens else {
        return None;
    };
    (first.text == "." && second.text == "." && third.text == ".").then_some(rest)
}

fn object_entry<'tokens, 'source>(
    entry: &'tokens [Token<'source>],
) -> Option<(Selection<'source>, &'tokens [Token<'source>])> {
    let first = entry.first()?.text;
    let end = if first == "[" {
        after_group(entry, "[", "]")?
    } else {
        1
    };
    let key = if first == "[" {
        (end == 3).then(|| quoted_key(entry[1].text)).flatten()
    } else {
        field_name(first)
    };
    let selection = key.map_or(Selection::Unsupported, Selection::Field);
    if entry.get(end).is_some_and(|token| token.text == ":") {
        Some((selection, &entry[end + 1..]))
    } else {
        Some((selection, entry))
    }
}

pub(super) fn quoted_key(text: &str) -> Option<&str> {
    let content = text.strip_prefix(['"', '\''])?.strip_suffix(['"', '\''])?;
    (!content.contains('\\')).then_some(content)
}

pub(super) fn subscript<'source>(tokens: &[Token<'source>]) -> Selection<'source> {
    if let [key] = tokens {
        if let Some(key) = quoted_key(key.text) {
            return Selection::Field(key);
        }
        if identifier(key.text) {
            return Selection::Variable(key.text, false);
        }
    }
    if let Some(receiver) = numeric_keys::string_conversion(tokens) {
        if let Some(key) = numeric_keys::bigint_name(receiver) {
            return Selection::OwnedField(key);
        }
        if let [name] = receiver
            && identifier(name.text)
        {
            return Selection::Variable(name.text, true);
        }
    }
    numeric_keys::literal(tokens).map_or(Selection::Unsupported, numeric_keys::selection)
}

pub(super) fn field_name(text: &str) -> Option<&str> {
    if identifier(text) || decimal_key(text) {
        Some(text)
    } else {
        quoted_key(text)
    }
}

pub(super) fn decimal_key(text: &str) -> bool {
    !text.is_empty()
        && (text == "0" || !text.starts_with('0'))
        && text.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn dot_name(text: &str) -> bool {
    static NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[\p{L}_$][\w$]*$").expect("static TypeScript member pattern")
    });
    NAME.is_match(text)
}

pub(super) struct Sequence<'tokens, 'source> {
    item: Type<'tokens, 'source>,
    tuple: bool,
    readonly: bool,
    offset: usize,
}

enum Position<'tokens, 'source> {
    Item(Type<'tokens, 'source>),
    Beyond(usize),
    Unknown,
}

impl<'tokens, 'source> Sequence<'tokens, 'source> {
    pub(super) fn with_readonly(mut self, readonly: bool) -> Self {
        self.readonly = readonly;
        self
    }

    pub(super) fn rest(mut self, start: usize) -> Option<Self> {
        self.offset = self.offset.checked_add(start)?;
        self.readonly = false;
        Some(self)
    }

    pub(super) fn at(
        &self,
        index: usize,
        source: &'source str,
        tokens: &'tokens [Token<'source>],
        visible: &[usize],
    ) -> Option<Type<'tokens, 'source>> {
        let index = self.offset.checked_add(index)?;
        let mut expansions = MAX_LOOKUPS;
        match self.position(index, false, source, tokens, visible, &mut expansions) {
            Position::Item(item) => Some(item),
            Position::Beyond(_) | Position::Unknown => None,
        }
    }

    fn position(
        &self,
        mut index: usize,
        followed_by_items: bool,
        source: &'source str,
        tokens: &'tokens [Token<'source>],
        visible: &[usize],
        expansions: &mut usize,
    ) -> Position<'tokens, 'source> {
        if !self.tuple {
            return if followed_by_items {
                // A variable-width segment with a suffix cannot identify a single type.
                Position::Unknown
            } else {
                Position::Item(self.item.clone())
            };
        }
        let mut entries = comma_groups(self.item.tokens, true).peekable();
        while let Some(entry) = entries.next() {
            if let Some(spread) = rest_pattern(entry) {
                let Some(budget) = expansions.checked_sub(1) else {
                    return Position::Unknown;
                };
                *expansions = budget;
                let subject = self.item.with_tokens(tuple_item(spread));
                let Some(Resolved::Sequence(inner)) =
                    resolve(source, subject, tokens, visible, MAX_LOOKUPS)
                else {
                    return Position::Unknown;
                };
                let has_suffix = followed_by_items || entries.peek().is_some();
                match inner.position(index, has_suffix, source, tokens, visible, expansions) {
                    Position::Beyond(rest) => index = rest,
                    found => return found,
                }
            } else if index == 0 {
                return Position::Item(self.item.with_tokens(tuple_item(entry)));
            } else {
                index -= 1;
            }
        }
        Position::Beyond(index)
    }

    pub(super) fn members(&self) -> Vec<&'static str> {
        instance_members("Array<>", "")
            .iter()
            .copied()
            .filter(|name| {
                !self.readonly
                    || !matches!(
                        *name,
                        "push" | "pop" | "reverse" | "shift" | "sort" | "splice" | "unshift"
                    )
            })
            .collect()
    }
}

fn tuple_item<'tokens, 'source>(entry: &'tokens [Token<'source>]) -> &'tokens [Token<'source>] {
    if entry.first().is_some_and(|token| identifier(token.text)) {
        let label_end = if entry.get(1).is_some_and(|token| token.text == "?") {
            2
        } else {
            1
        };
        if entry.get(label_end).is_some_and(|token| token.text == ":") {
            return &entry[label_end + 1..];
        }
    }
    entry
}

pub(super) fn sequence<'tokens, 'source>(
    subject: &Type<'tokens, 'source>,
) -> Option<Sequence<'tokens, 'source>> {
    let readonly = subject.tokens.first()?.text == "readonly";
    let tokens = &subject.tokens[usize::from(readonly)..];
    let [first, rest @ ..] = tokens else {
        return None;
    };
    let (arguments, suffix) = match first.text {
        "[" => {
            let end = after_group(tokens, "[", "]")?;
            (&tokens[1..end - 1], &tokens[end..])
        }
        "{" => {
            let end = after_group(tokens, "{", "}")?;
            (&[][..], &tokens[end..])
        }
        _ if identifier(first.text) => generics::arguments(rest)?,
        _ => return None,
    };
    let dimensions = suffix
        .as_chunks::<2>()
        .0
        .iter()
        .take_while(|pair| pair[0].text == "[" && pair[1].text == "]")
        .count();
    if dimensions > 0 {
        let end = tokens.len() - suffix.len() + 2 * (dimensions - 1);
        return Some(Sequence {
            item: subject.with_tokens(&tokens[..end]),
            tuple: false,
            readonly,
            offset: 0,
        });
    }
    if first.text == "["
        || (matches!(first.text, "Array" | "ReadonlyArray") && !arguments.is_empty())
    {
        return Some(Sequence {
            item: subject.with_tokens(arguments),
            tuple: first.text == "[",
            readonly: readonly || first.text == "ReadonlyArray",
            offset: 0,
        });
    }
    None
}
