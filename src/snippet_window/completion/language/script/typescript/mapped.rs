//! Finite property sets for utility types and mapped object declarations.

use std::{borrow::Cow, collections::HashSet};

use super::{
    Join, Resolved, Type, after_group, body_members, comma_groups, destructure::quoted_key, fields,
    identifier, index_signature,
};

pub(super) type Keys<'source> = Vec<Cow<'source, str>>;

pub(super) enum Filter<'source> {
    Pick(Keys<'source>),
    Omit(Keys<'source>),
}

impl Filter<'_> {
    pub(super) fn keeps(&self, field: &str) -> bool {
        match self {
            Self::Pick(keys) => keys.iter().any(|key| key == field),
            Self::Omit(keys) => keys.iter().all(|key| key != field),
        }
    }
}

pub(super) fn supports(name: &str) -> bool {
    matches!(
        name,
        "Partial" | "Required" | "Readonly" | "Pick" | "Omit" | "Record"
    )
}

pub(super) fn utility<'tokens, 'source>(
    name: &str,
    arguments: Type<'tokens, 'source>,
    mut resolve: impl FnMut(Type<'tokens, 'source>) -> Option<Resolved<'tokens, 'source>>,
) -> Option<Resolved<'tokens, 'source>> {
    let mut parts = comma_groups(arguments.tokens, true);
    let first = arguments.with_tokens(parts.next()?);
    let second = parts.next();
    if matches!(name, "Partial" | "Required" | "Readonly") && second.is_none() {
        let base = resolve(first)?;
        return if name == "Readonly" {
            Some(with_readonly(base, true))
        } else {
            Some(base)
        };
    }
    if !matches!(name, "Pick" | "Omit" | "Record") {
        return None;
    }
    let second = arguments.with_tokens(second?);
    if parts.next().is_some() {
        return None;
    }
    if name == "Record" {
        return indexed_value(resolve(first)?, second);
    }
    let base = resolve(first)?;
    let keys = key_values(resolve(second)?)?;
    let filter = match name {
        "Pick" => Filter::Pick(keys),
        _ => Filter::Omit(keys),
    };
    Some(Resolved::Filtered(Box::new(base), filter))
}

fn with_readonly<'tokens, 'source>(
    value: Resolved<'tokens, 'source>,
    readonly: bool,
) -> Resolved<'tokens, 'source> {
    match value {
        Resolved::Joined(join, parts) => Resolved::Joined(
            join,
            parts
                .into_iter()
                .map(|part| with_readonly(part, readonly))
                .collect(),
        ),
        Resolved::Sequence(sequence) => Resolved::Sequence(sequence.with_readonly(readonly)),
        value => value,
    }
}

pub(super) struct Declaration<'tokens, 'source> {
    domain: Type<'tokens, 'source>,
    item: Option<Type<'tokens, 'source>>,
    readonly: Option<bool>,
    remap: Remap<'source>,
}

enum Remap<'source> {
    Identity,
    Empty,
    Rename(Rename<'source>),
}

enum Rename<'source> {
    Literal(&'source str),
    Template {
        prefix: &'source str,
        suffix: &'source str,
    },
}

impl<'source> Remap<'source> {
    fn parse(key: &str, expression: &'source str) -> Option<Self> {
        if expression == key {
            return Some(Self::Identity);
        }
        if expression == "never" {
            return Some(Self::Empty);
        }
        if let Some(literal) = quoted_key(expression) {
            return Some(Self::Rename(Rename::Literal(literal)));
        }
        let literal = expression.strip_prefix('`')?.strip_suffix('`')?;
        if literal.contains('\\') {
            return None;
        }
        let Some((prefix, interpolation)) = literal.split_once("${") else {
            return Some(Self::Rename(Rename::Literal(literal)));
        };
        let (parameter, suffix) = interpolation.split_once('}')?;
        let parameter = parameter.trim();
        let property_key = parameter == key
            || parameter
                .split_once('&')
                .is_some_and(|(parameter, domain)| {
                    parameter.trim() == key
                        && domain.split_whitespace().collect::<String>() == "(string|number)"
                });
        (property_key && !suffix.contains("${"))
            .then_some(Self::Rename(Rename::Template { prefix, suffix }))
    }
}

impl<'source> Rename<'source> {
    fn name(&self, original: &str) -> Cow<'source, str> {
        match self {
            Self::Literal(name) => Cow::Borrowed(name),
            Self::Template { prefix, suffix } => Cow::Owned(format!("{prefix}{original}{suffix}")),
        }
    }
}

pub(super) struct RenamedKey<'source> {
    pub(super) name: Cow<'source, str>,
    pub(super) original: Cow<'source, str>,
}

pub(super) fn declaration<'tokens, 'source>(
    body: &Type<'tokens, 'source>,
) -> Option<Declaration<'tokens, 'source>> {
    let tokens = body.tokens;
    let (tokens, readonly) = match tokens {
        [sign, modifier, rest @ ..]
            if matches!(sign.text, "+" | "-") && modifier.text == "readonly" =>
        {
            (rest, Some(sign.text == "+"))
        }
        [modifier, rest @ ..] if modifier.text == "readonly" => (rest, Some(true)),
        _ => (tokens, None),
    };
    if tokens.first()?.text != "[" {
        return None;
    }
    let end = after_group(tokens, "[", "]")?;
    let [key, operator, domain @ ..] = &tokens[1..end - 1] else {
        return None;
    };
    if !identifier(key.text) || operator.text != "in" {
        return None;
    }
    let (domain, remap) = match domain.iter().skip(1).position(|token| token.text == "as") {
        Some(position) => {
            let (domain, clause) = domain.split_at(position + 1);
            let [_, expression] = clause else {
                return None;
            };
            (domain, Remap::parse(key.text, expression.text)?)
        }
        None => (domain, Remap::Identity),
    };
    let after = match &tokens[end..] {
        [sign, optional, rest @ ..] if matches!(sign.text, "+" | "-") && optional.text == "?" => {
            rest
        }
        [optional, rest @ ..] if optional.text == "?" => rest,
        rest => rest,
    };
    let [colon, item @ ..] = after else {
        return None;
    };
    if colon.text != ":" || item.is_empty() {
        return None;
    }
    let item = match item {
        [rest @ .., last] if matches!(last.text, ";" | ",") => rest,
        item => item,
    };
    if let [prefix, base @ ..] = domain
        && prefix.text == "keyof"
        && let [target @ .., open, index, close] = item
        && open.text == "["
        && index.text == key.text
        && close.text == "]"
        && target
            .iter()
            .map(|token| token.text)
            .eq(base.iter().map(|token| token.text))
    {
        return Some(Declaration {
            domain: body.with_tokens(base),
            item: None,
            readonly,
            remap,
        });
    }
    if item.iter().any(|token| token.text == key.text) {
        return None;
    }
    Some(Declaration {
        domain: body.with_tokens(domain),
        item: Some(body.with_tokens(item)),
        readonly,
        remap,
    })
}

pub(super) fn resolve_declaration<'tokens, 'source>(
    source: &'source str,
    declaration: Declaration<'tokens, 'source>,
    mut resolve: impl FnMut(Type<'tokens, 'source>) -> Option<Resolved<'tokens, 'source>>,
) -> Option<Resolved<'tokens, 'source>> {
    if matches!(declaration.remap, Remap::Empty) {
        return Some(Resolved::Remapped(Vec::new(), declaration.domain));
    }
    let domain = resolve(declaration.domain.clone())?;
    if let Remap::Rename(rename) = declaration.remap {
        return match declaration.item {
            Some(item) => {
                let keys = key_values(domain)?
                    .into_iter()
                    .map(|key| rename.name(&key))
                    .collect();
                Some(Resolved::Mapped(keys, item))
            }
            None => {
                let keys = shape_keys(source, domain)?
                    .into_iter()
                    .map(|original| {
                        let name = rename.name(&original);
                        RenamedKey { name, original }
                    })
                    .collect();
                Some(Resolved::Remapped(keys, declaration.domain))
            }
        };
    }
    match declaration.item {
        Some(item) => indexed_value(domain, item),
        None => Some(match declaration.readonly {
            Some(readonly) => with_readonly(domain, readonly),
            None => domain,
        }),
    }
}

fn indexed_value<'tokens, 'source>(
    domain: Resolved<'tokens, 'source>,
    item: Type<'tokens, 'source>,
) -> Option<Resolved<'tokens, 'source>> {
    if let Resolved::Primitive(domain) = domain {
        return Some(Resolved::Indexed(domain, item));
    }
    Some(Resolved::Mapped(key_values(domain)?, item))
}

fn key_values<'source>(value: Resolved<'_, 'source>) -> Option<Keys<'source>> {
    match value {
        Resolved::KeySet(keys) => Some(keys),
        Resolved::Joined(Join::Union, parts) => {
            let parts = parts
                .into_iter()
                .map(key_values)
                .collect::<Option<Vec<_>>>()?;
            Some(Join::Intersection.combine_names(parts.into_iter()))
        }
        _ => None,
    }
}

pub(super) fn shape_keys<'source>(
    source: &'source str,
    value: Resolved<'_, 'source>,
) -> Option<Keys<'source>> {
    match value {
        Resolved::Fields(body) => {
            if body_members(source, body.tokens).any(|member| index_signature(member).is_some()) {
                return None;
            }
            Some(
                fields(source, body.tokens)
                    .map(|(name, _)| Cow::Borrowed(name))
                    .collect(),
            )
        }
        Resolved::Mapped(keys, _) => Some(keys),
        Resolved::Remapped(keys, _) => {
            let mut seen = HashSet::new();
            Some(
                keys.into_iter()
                    .filter_map(|key| seen.insert(key.name.clone()).then_some(key.name))
                    .collect(),
            )
        }
        Resolved::Filtered(base, filter) => {
            let mut keys = shape_keys(source, *base)?;
            keys.retain(|name| filter.keeps(name));
            Some(keys)
        }
        Resolved::Joined(join, parts) => {
            let parts = parts
                .into_iter()
                .map(|part| shape_keys(source, part))
                .collect::<Option<Vec<_>>>()?;
            Some(join.combine_names(parts.into_iter()))
        }
        _ => None,
    }
}
