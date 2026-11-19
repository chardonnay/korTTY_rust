//! Same-document completion for typed TypeScript bindings.

use std::sync::LazyLock;
use std::{borrow::Cow, collections::HashSet};

use regex::Regex;

use super::instance_members;

mod class;
mod composition;
mod destructure;
mod generics;
mod index_keys;
mod mapped;
mod numeric_keys;

use composition::Join;
use destructure::{Selection, Sequence};
use generics::Type;

const MAX_LOOKUPS: usize = 12;
const MAX_TYPE_RESOLUTIONS: usize = 128;
const MAX_RETURN_TYPE_TOKENS: usize = 256;
const MAX_ARROW_EXPRESSION_TOKENS: usize = 1024;

pub(super) fn members(source: &str) -> Option<Vec<Cow<'_, str>>> {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?s:/\*.*?(?:\*/|\z))|//[^\r\n]*|"([^"\\]|\\.)*"|'([^'\\]|\\.)*'|`([^`\\]|\\.)*`|\?\.|=>|[\p{L}_$][\w$]*|[0-9]+|[^\s]"#,
        )
        .expect("static TypeScript token pattern")
    });
    let tokens: Vec<_> = TOKEN
        .find_iter(source)
        .filter(|token| {
            let text = token.as_str();
            !text.starts_with("//") && !text.starts_with("/*")
        })
        .map(|token| Token {
            text: token.as_str(),
            start: token.start(),
        })
        .collect();
    let (name, path, start) = receiver(&tokens)?;
    let visible = visible_indices(&tokens[..start]);
    let binding = binding(source, &tokens, start, &visible, name)?;
    if binding.annotation.is_empty() {
        // An untyped parameter can shadow a typed binding in an outer scope.
        return Some(Vec::new());
    }
    let resolved_binding = !binding.selections.is_empty()
        || path.iter().any(|step| {
            matches!(
                step,
                Selection::Index(_)
                    | Selection::NumericKey(_)
                    | Selection::Variable(..)
                    | Selection::OwnedField(_)
                    | Selection::Unsupported
            )
        });
    let path = path.into_iter().map(|step| match step {
        Selection::Variable(name, stringify) => {
            index_keys::resolve(source, &tokens, start, name, stringify)
                .unwrap_or(Selection::Unsupported)
        }
        step => step,
    });
    let steps = binding.selections.into_iter().chain(path);
    let candidates = member_candidates(
        source,
        &tokens,
        &visible,
        Type::new(binding.annotation),
        steps,
    );
    if resolved_binding {
        return Some(candidates.unwrap_or_default());
    }
    candidates
}

fn member_candidates<'tokens, 'source>(
    source: &'source str,
    tokens: &'tokens [Token<'source>],
    visible: &[usize],
    subject: Type<'tokens, 'source>,
    steps: impl Iterator<Item = Selection<'source>>,
) -> Option<mapped::Keys<'source>> {
    let mut budget = MAX_TYPE_RESOLUTIONS;
    let mut value = resolve_type(source, subject, tokens, visible, MAX_LOOKUPS, &mut budget)?;
    for selection in steps {
        let Some(next) = project(source, tokens, visible, value, &selection, &mut budget) else {
            return Some(Vec::new());
        };
        value = next;
    }
    Some(resolved_names(source, tokens, value))
}

fn project<'tokens, 'source>(
    source: &'source str,
    tokens: &'tokens [Token<'source>],
    visible: &[usize],
    value: Resolved<'tokens, 'source>,
    selection: &Selection<'source>,
    budget: &mut usize,
) -> Option<Resolved<'tokens, 'source>> {
    let subject = match (value, selection) {
        (Resolved::Joined(join, parts), _) => {
            let projected = parts
                .into_iter()
                .map(|part| project(source, tokens, visible, part, selection, budget));
            let parts: Vec<_> = match join {
                Join::Union => projected.collect::<Option<_>>()?,
                Join::Intersection => projected.flatten().collect(),
            };
            return (!parts.is_empty()).then_some(Resolved::Joined(join, parts));
        }
        (
            Resolved::Fields(body),
            Selection::Field(_)
            | Selection::OwnedField(_)
            | Selection::Index(_)
            | Selection::NumericKey(_),
        ) => field_type(source, &body, selection)?,
        (Resolved::Filtered(base, filter), _)
            if selection
                .property_name()
                .is_some_and(|key| filter.keeps(&key)) =>
        {
            return project(source, tokens, visible, *base, selection, budget);
        }
        (Resolved::Mapped(keys, item), _)
            if selection
                .property_name()
                .is_some_and(|key| keys.contains(&key)) =>
        {
            item
        }
        (Resolved::Remapped(keys, base), _) if let Some(field) = selection.property_name() => {
            let mut parts = keys
                .into_iter()
                .filter(|key| key.name == field)
                .map(|key| {
                    let value =
                        resolve_type(source, base.clone(), tokens, visible, MAX_LOOKUPS, budget)?;
                    let original = Selection::OwnedField(key.original.into_owned());
                    project(source, tokens, visible, value, &original, budget)
                })
                .collect::<Option<Vec<_>>>()?;
            return match parts.len() {
                0 => None,
                1 => parts.pop(),
                _ => Some(Resolved::Joined(Join::Union, parts)),
            };
        }
        (Resolved::Indexed(domain, item), _) if domain.matches(selection) => item,
        (Resolved::Class(index, class), _) => {
            let field = selection.property_name()?;
            class_members(source, tokens, index, class)
                .into_iter()
                .find(|member| member.name == field.as_ref())?
                .annotation?
        }
        (Resolved::Sequence(sequence), Selection::Index(index)) => {
            sequence.at(*index, source, tokens, visible)?
        }
        (Resolved::Sequence(sequence), Selection::Field(_) | Selection::OwnedField(_)) => {
            let key = selection.property_name()?;
            if !destructure::decimal_key(&key) {
                return None;
            }
            let index: u32 = key.parse().ok()?;
            if index == u32::MAX {
                return None;
            }
            sequence.at(index as usize, source, tokens, visible)?
        }
        (Resolved::Sequence(sequence), Selection::ArrayRest(index)) => {
            return Some(Resolved::Sequence(sequence.rest(*index)?));
        }
        (
            value @ (Resolved::Fields(_)
            | Resolved::Filtered(..)
            | Resolved::Mapped(..)
            | Resolved::Remapped(..)
            | Resolved::Indexed(..)),
            Selection::ObjectRest(excluded),
        ) => {
            return Some(Resolved::Filtered(
                Box::new(value),
                mapped::Filter::Omit(excluded.iter().copied().map(Cow::Borrowed).collect()),
            ));
        }
        _ => return None,
    };
    resolve_type(source, subject, tokens, visible, MAX_LOOKUPS, budget)
}

fn resolved_names<'tokens, 'source>(
    source: &'source str,
    tokens: &'tokens [Token<'source>],
    value: Resolved<'tokens, 'source>,
) -> mapped::Keys<'source> {
    let borrowed = Cow::Borrowed;
    match value {
        Resolved::Joined(join, parts) => join.combine_names(
            parts
                .into_iter()
                .map(|part| resolved_names(source, tokens, part)),
        ),
        Resolved::Fields(body) => fields(source, body.tokens)
            .map(|(name, _)| name)
            .filter(|name| destructure::dot_name(name))
            .map(Cow::Borrowed)
            .collect(),
        Resolved::Filtered(base, filter) => {
            let mut names = resolved_names(source, tokens, *base);
            names.retain(|name| filter.keeps(name));
            names
        }
        Resolved::Mapped(keys, _) => keys
            .into_iter()
            .filter(|name| destructure::dot_name(name))
            .collect(),
        Resolved::Remapped(keys, _) => keys
            .into_iter()
            .map(|key| key.name)
            .filter(|name| destructure::dot_name(name))
            .collect(),
        Resolved::Class(index, class) => class_members(source, tokens, index, class)
            .into_iter()
            .map(|member| Cow::Borrowed(member.name))
            .collect(),
        Resolved::Sequence(sequence) => sequence.members().into_iter().map(borrowed).collect(),
        Resolved::Builtin(members) => members.iter().copied().map(borrowed).collect(),
        Resolved::Primitive(domain) => instance_members(domain.name(), "")
            .iter()
            .copied()
            .map(borrowed)
            .collect(),
        Resolved::KeySet(keys) if !keys.is_empty() => instance_members("string", "")
            .iter()
            .copied()
            .map(borrowed)
            .collect(),
        Resolved::Unknown | Resolved::KeySet(_) | Resolved::Indexed(..) => Vec::new(),
    }
}

fn field_type<'tokens, 'source>(
    source: &'source str,
    body: &Type<'tokens, 'source>,
    selection: &Selection<'_>,
) -> Option<Type<'tokens, 'source>> {
    let name = selection.property_name()?;
    if let Some((_, annotation)) =
        fields(source, body.tokens).find(|(field, _)| *field == name.as_ref())
    {
        return Some(body.with_tokens(annotation?));
    }
    if numeric_keys::is_numeric_name(&name)
        && let Some(annotation) = body_members(source, body.tokens).find_map(|member| {
            let separator = member
                .iter()
                .position(|token| matches!(token.text, ":" | "?"))?;
            let value = numeric_keys::literal(&member[..separator])?;
            if numeric_keys::property_name(value) != name {
                return None;
            }
            let colon = separator + usize::from(member[separator].text == "?");
            (member.get(colon)?.text == ":").then(|| &member[colon + 1..])
        })
    {
        return Some(body.with_tokens(annotation));
    }
    let (_, annotation) = body_members(source, body.tokens)
        .filter_map(index_signature)
        .filter(|(domain, _)| domain.matches(selection))
        .min_by_key(|(domain, _)| *domain)?;
    Some(body.with_tokens(annotation))
}

struct Binding<'tokens, 'source> {
    annotation: &'tokens [Token<'source>],
    selections: Vec<Selection<'source>>,
}

impl<'tokens, 'source> Binding<'tokens, 'source> {
    fn plain(annotation: &'tokens [Token<'source>]) -> Self {
        Self {
            annotation,
            selections: Vec::new(),
        }
    }
}

fn destructured_binding<'tokens, 'source>(
    tokens: &'tokens [Token<'source>],
    receiver: &str,
) -> Option<Binding<'tokens, 'source>> {
    let opener = tokens.first()?.text;
    let closer = match opener {
        "{" => "}",
        "[" => "]",
        _ => return None,
    };
    let end = after_group(tokens, opener, closer)?;
    let selections = destructure::path(&tokens[..end], receiver)?;
    Some(Binding {
        annotation: annotation(&tokens[end..]).unwrap_or_default(),
        selections,
    })
}

fn binding<'tokens, 'source>(
    source: &str,
    tokens: &'tokens [Token<'source>],
    receiver_index: usize,
    visible: &[usize],
    receiver: &str,
) -> Option<Binding<'tokens, 'source>> {
    for &index in visible {
        if let [keyword, name, rest @ ..] = &tokens[index..receiver_index]
            && matches!(keyword.text, "const" | "let" | "var")
        {
            if name.text == receiver {
                return annotation(rest)
                    .or_else(|| {
                        let [equals, new, name, ..] = rest else {
                            return None;
                        };
                        (equals.text == "=" && new.text == "new" && identifier(name.text))
                            .then_some(&rest[2..])
                    })
                    .map(Binding::plain);
            }
            if let Some(binding) =
                destructured_binding(&tokens[index + 1..receiver_index], receiver)
            {
                return Some(binding);
            }
        }
        let header_end = match tokens[index].text {
            "{" => index,
            "=>" if within_arrow_expression(source, &tokens[index + 1..=receiver_index]) => {
                index + 1
            }
            _ => continue,
        };
        if let Some(parameters) = parameters(&tokens[..header_end])
            && let Some(parameter) = parameter(parameters, receiver)
        {
            return Some(parameter);
        }
    }
    None
}

fn within_arrow_expression(source: &str, expression: &[Token<'_>]) -> bool {
    if expression.len() > MAX_ARROW_EXPRESSION_TOKENS
        || expression.first().is_none_or(|token| token.text == "{")
    {
        return false;
    }
    let mut index = 0;
    let mut conditional_depth = 0usize;
    while let Some(token) = expression.get(index) {
        let text = token.text;
        match text {
            ";" | "," | ")" | "]" | "}" => return false,
            ":" if conditional_depth == 0 => return false,
            ":" => conditional_depth -= 1,
            "?" => conditional_depth += 1,
            _ => {}
        }
        if index > 0 {
            let previous = expression[index - 1];
            let line_break =
                source[previous.start + previous.text.len()..token.start].contains(['\r', '\n']);
            if line_break && ends_expression(previous.text) && !continues_expression(text) {
                return false;
            }
        }
        let Some(length) = skip_group(&expression[index..]) else {
            // The receiver is inside an unfinished nested group in this expression.
            return true;
        };
        index += length;
    }
    true
}

fn ends_expression(token: &str) -> bool {
    matches!(token, ")" | "]" | "}")
        || token.starts_with(|ch: char| ch.is_alphanumeric() || "_$\"'`".contains(ch))
            && !matches!(
                token,
                "await"
                    | "new"
                    | "typeof"
                    | "void"
                    | "delete"
                    | "yield"
                    | "as"
                    | "satisfies"
                    | "in"
                    | "instanceof"
            )
}

fn continues_expression(token: &str) -> bool {
    matches!(
        token,
        "." | "?."
            | "["
            | "("
            | "?"
            | ":"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | ">"
            | "<"
            | "="
            | "!"
            | "&"
            | "|"
            | "^"
            | "as"
            | "satisfies"
            | "in"
            | "instanceof"
    ) || token.starts_with('`')
}

fn parameters<'tokens, 'source>(
    header: &'tokens [Token<'source>],
) -> Option<&'tokens [Token<'source>]> {
    let arrow = header.last()?.text == "=>";
    let end = header.len() - usize::from(arrow);
    if arrow
        && let Some(last) = header[..end].last()
        && identifier(last.text)
        && header[..end - 1].last().is_none_or(|previous| {
            matches!(
                previous.text,
                "=" | "(" | "," | "return" | "=>" | "?" | "async"
            )
        })
    {
        return Some(&header[end - 1..end]);
    }
    let mut index = end;
    let close = loop {
        index = index.checked_sub(1)?;
        if end - index > MAX_RETURN_TYPE_TOKENS {
            return None;
        }
        match header[index].text {
            ")" if index + 1 == end || header[index + 1].text == ":" => break index,
            "}" => index = before_group(&header[..=index], "{", "}")?,
            ";" | "{" | "=" | "=>" => return None,
            _ => {}
        }
    };
    let open = before_group(&header[..=close], "(", ")")?;
    let head = &header[..open];
    if (arrow && function_type_head(head)) || (!arrow && !function_keyword(head)) {
        return None;
    }
    Some(&header[open + 1..close])
}

fn function_type_head(mut head: &[Token<'_>]) -> bool {
    if head.last().is_some_and(|last| last.text == ">")
        && let Some(open) = before_group(head, "<", ">")
    {
        head = &head[..open];
    }
    matches!(head, [.., kind, name, operator] if identifier(name.text)
        && matches!((kind.text, operator.text), ("type", "=") | ("const" | "let" | "var", ":")))
}

fn function_keyword(mut head: &[Token<'_>]) -> bool {
    if head.last().is_some_and(|last| last.text == ">") {
        let Some(open) = before_group(head, "<", ">") else {
            return false;
        };
        head = &head[..open];
    }
    if head
        .last()
        .is_some_and(|last| last.text != "function" && identifier(last.text))
    {
        head = &head[..head.len() - 1];
    }
    if head.last().is_some_and(|last| last.text == "*") {
        head = &head[..head.len() - 1];
    }
    head.last().is_some_and(|last| last.text == "function")
}

fn parameter<'tokens, 'source>(
    parameters: &'tokens [Token<'source>],
    receiver: &str,
) -> Option<Binding<'tokens, 'source>> {
    parameter_groups(parameters).find_map(|item| {
        let name = item.iter().position(|token| token.text != ".")?;
        if item[name].text == receiver {
            Some(Binding::plain(
                annotation(&item[name + 1..]).unwrap_or_default(),
            ))
        } else {
            destructured_binding(item, receiver)
        }
    })
}

fn parameter_groups<'tokens, 'source>(
    parameters: &'tokens [Token<'source>],
) -> impl Iterator<Item = &'tokens [Token<'source>]> {
    comma_groups(parameters, false)
}

fn comma_groups<'tokens, 'source>(
    mut parameters: &'tokens [Token<'source>],
    types_only: bool,
) -> impl Iterator<Item = &'tokens [Token<'source>]> {
    std::iter::from_fn(move || {
        if parameters.is_empty() {
            return None;
        }
        let mut end = 0;
        let mut default_value = false;
        while let Some(token) = parameters.get(end) {
            if token.text == "," {
                break;
            }
            default_value |= token.text == "=";
            end += if token.text == "<" && (types_only || !default_value) {
                after_group(&parameters[end..], "<", ">")
            } else {
                skip_group(&parameters[end..])
            }
            .unwrap_or(parameters.len() - end);
        }
        let item = &parameters[..end];
        parameters = parameters.get(end + 1..).unwrap_or_default();
        Some(item)
    })
}

fn annotation<'tokens, 'source>(
    tokens: &'tokens [Token<'source>],
) -> Option<&'tokens [Token<'source>]> {
    let start = usize::from(tokens.first()?.text == "?");
    (tokens.get(start)?.text == ":").then(|| &tokens[start + 1..])
}

#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    start: usize,
}

fn receiver<'source>(
    tokens: &[Token<'source>],
) -> Option<(&'source str, Vec<Selection<'source>>, usize)> {
    let (last, preceding) = tokens.split_last()?;
    if !matches!(last.text, "." | "?.") {
        return None;
    }
    let mut end = preceding.len();
    let mut path = Vec::new();
    while path.len() < MAX_LOOKUPS {
        let start = end.checked_sub(1)?;
        let name = tokens[start].text;
        if name == "]" {
            let open = before_group(&tokens[..end], "[", "]")?;
            path.push(destructure::subscript(&tokens[open + 1..start]));
            end = open;
            if end > 0 && tokens[end - 1].text == "?." {
                end -= 1;
            }
        } else if identifier(name) {
            if start == 0 || !matches!(tokens[start - 1].text, "." | "?.") {
                path.reverse();
                return Some((name, path, start));
            }
            path.push(Selection::Field(name));
            end = start - 1;
        } else {
            return None;
        }
    }
    None
}

fn visible_indices(tokens: &[Token<'_>]) -> Vec<usize> {
    let mut closed_scopes = 0usize;
    let mut visible = Vec::new();
    for (index, token) in tokens.iter().enumerate().rev() {
        match token.text {
            "}" => closed_scopes += 1,
            "{" if closed_scopes == 0 => visible.push(index),
            "{" => closed_scopes = closed_scopes.saturating_sub(1),
            _ if closed_scopes == 0 => visible.push(index),
            _ => {}
        }
    }
    visible
}

fn before_group(tokens: &[Token<'_>], open: &str, close: &str) -> Option<usize> {
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate().rev() {
        if token.text == close {
            depth += 1;
        } else if token.text == open {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

enum Resolved<'tokens, 'source> {
    Joined(Join, Vec<Resolved<'tokens, 'source>>),
    Fields(Type<'tokens, 'source>),
    Filtered(Box<Resolved<'tokens, 'source>>, mapped::Filter<'source>),
    Mapped(mapped::Keys<'source>, Type<'tokens, 'source>),
    Remapped(Vec<mapped::RenamedKey<'source>>, Type<'tokens, 'source>),
    Indexed(KeyDomain, Type<'tokens, 'source>),
    Primitive(KeyDomain),
    KeySet(mapped::Keys<'source>),
    Class(usize, Type<'tokens, 'source>),
    Sequence(Sequence<'tokens, 'source>),
    Builtin(&'static [&'static str]),
    Unknown,
}

fn resolve<'tokens, 'source>(
    source: &'source str,
    subject: Type<'tokens, 'source>,
    tokens: &'tokens [Token<'source>],
    visible: &[usize],
    remaining: usize,
) -> Option<Resolved<'tokens, 'source>> {
    let mut budget = MAX_TYPE_RESOLUTIONS;
    resolve_type(source, subject, tokens, visible, remaining, &mut budget)
}

fn resolve_type<'tokens, 'source>(
    source: &'source str,
    mut subject: Type<'tokens, 'source>,
    tokens: &'tokens [Token<'source>],
    visible: &[usize],
    mut remaining: usize,
    budget: &mut usize,
) -> Option<Resolved<'tokens, 'source>> {
    loop {
        remaining = remaining.checked_sub(1)?;
        *budget = budget.checked_sub(1)?;
        let annotation = subject.tokens;
        let [first, rest @ ..] = annotation else {
            return None;
        };
        let Ok(joined) = composition::read(annotation) else {
            return Some(Resolved::Unknown);
        };
        if let Some(joined) = joined {
            let parts = joined
                .parts
                .into_iter()
                .map(|part| {
                    resolve_type(
                        source,
                        subject.with_tokens(part),
                        tokens,
                        visible,
                        remaining,
                        budget,
                    )
                })
                .collect::<Option<_>>();
            return Some(parts.map_or(Resolved::Unknown, |parts| {
                Resolved::Joined(joined.join, parts)
            }));
        }
        if first.text == "(" {
            let after = after_group(annotation, "(", ")")?;
            if annotation
                .get(after)
                .is_some_and(|token| matches!(token.text, "=>" | "["))
            {
                return None;
            }
            subject = subject.with_tokens(&annotation[1..after - 1]);
            continue;
        }
        if let Some(sequence) = destructure::sequence(&subject) {
            return Some(Resolved::Sequence(sequence));
        }
        if first.text == "{" {
            let after = after_group(annotation, "{", "}")?;
            let body = subject.with_tokens(&annotation[1..after - 1]);
            if let Some(mapped) = mapped::declaration(&body) {
                return Some(
                    mapped::resolve_declaration(source, mapped, |ty| {
                        resolve_type(source, ty, tokens, visible, remaining, budget)
                    })
                    .unwrap_or(Resolved::Unknown),
                );
            }
            return Some(Resolved::Fields(body));
        }
        if first.text == "keyof" {
            let base = resolve_type(
                source,
                subject.with_tokens(rest),
                tokens,
                visible,
                remaining,
                budget,
            )?;
            let keys = mapped::shape_keys(source, base)?;
            return Some(Resolved::KeySet(keys));
        }
        if let Some(key) = destructure::quoted_key(first.text) {
            return Some(Resolved::KeySet(vec![Cow::Borrowed(key)]));
        }
        let (arguments, suffix) = generics::arguments(rest)?;
        if composed(suffix.first()) {
            return None;
        }
        if arguments.is_empty()
            && let Some(bound) = subject.parameter(first.text)
        {
            subject = bound?;
            continue;
        }
        if let Some(domain) = KeyDomain::parse(first.text) {
            return Some(Resolved::Primitive(domain));
        }
        let builtin_name = match first.text {
            literal if literal.starts_with(['"', '\'']) => "string",
            literal if literal.starts_with(|ch: char| ch.is_ascii_digit()) => "number",
            "true" | "false" => "boolean",
            name => name,
        };
        let builtin = instance_members(builtin_name, "");
        if !builtin.is_empty() {
            return Some(Resolved::Builtin(builtin));
        }
        let named = visible.iter().find_map(|&index| {
            let [kind, name, operator, ..] = &tokens[index..] else {
                return None;
            };
            if name.text != first.text {
                return None;
            }
            match (kind.text, operator.text) {
                ("type", "=" | "<")
                | ("interface", "{" | "<")
                | ("class", "{" | "<" | "extends" | "implements") => Some((kind.text, index)),
                _ => None,
            }
        });
        let Some((kind, index)) = named else {
            return mapped::supports(first.text).then(|| {
                mapped::utility(first.text, subject.with_tokens(arguments), |ty| {
                    resolve_type(source, ty, tokens, visible, remaining, budget)
                })
                .unwrap_or(Resolved::Unknown)
            });
        };
        let (parameters, definition) = generics::arguments(&tokens[index + 2..])?;
        if kind == "class" {
            return Some(Resolved::Class(
                index,
                subject.apply(parameters, arguments, definition),
            ));
        }
        if kind == "interface" {
            if definition.first()?.text != "{" {
                return None;
            }
            let after = after_group(definition, "{", "}")?;
            return Some(Resolved::Fields(subject.apply(
                parameters,
                arguments,
                &definition[1..after - 1],
            )));
        }
        let [equals, alias @ ..] = definition else {
            return None;
        };
        if equals.text != "=" {
            return None;
        }
        subject = subject.apply(parameters, arguments, alias);
    }
}

struct TypedMember<'tokens, 'source> {
    name: &'source str,
    annotation: Option<Type<'tokens, 'source>>,
}

fn class_members<'tokens, 'source>(
    source: &'source str,
    tokens: &'tokens [Token<'source>],
    mut index: usize,
    mut subject: Type<'tokens, 'source>,
) -> Vec<TypedMember<'tokens, 'source>> {
    let mut seen = HashSet::new();
    let mut members = Vec::new();
    for _ in 0..MAX_LOOKUPS {
        let Some(declaration) = class::declaration(subject.tokens) else {
            break;
        };
        members.extend(
            class::members(source, declaration.body)
                .filter(|member| seen.insert(member.name) && member.public)
                .map(|member| TypedMember {
                    name: member.name,
                    annotation: member
                        .annotation
                        .map(|annotation| subject.with_tokens(annotation)),
                }),
        );
        let Some(base) = declaration.base else {
            break;
        };
        let Some(base_index) = base_class(tokens, index, base[0].text) else {
            break;
        };
        let Some((arguments, _)) = generics::arguments(&base[1..]) else {
            break;
        };
        let Some((parameters, definition)) = generics::arguments(&tokens[base_index + 2..]) else {
            break;
        };
        subject = subject.apply(parameters, arguments, definition);
        index = base_index;
    }
    members
}

fn base_class(tokens: &[Token<'_>], declaration_index: usize, base: &str) -> Option<usize> {
    // A base is a value evaluated at the derived class declaration.
    for index in visible_indices(&tokens[..declaration_index]) {
        let [kind, name, operator, ..] = &tokens[index..declaration_index] else {
            continue;
        };
        if name.text != base {
            continue;
        }
        match (kind.text, operator.text) {
            ("class", "{" | "<" | "extends" | "implements") => return Some(index),
            ("const" | "let" | "var" | "function", _) => return None,
            _ => {}
        }
    }
    None
}

fn composed(next: Option<&Token<'_>>) -> bool {
    next.is_some_and(|token| matches!(token.text, "|" | "&" | "."))
}

fn fields<'tokens, 'source>(
    source: &'source str,
    body: &'tokens [Token<'source>],
) -> impl Iterator<Item = (&'source str, Option<&'tokens [Token<'source>]>)> {
    body_members(source, body).filter_map(|member| {
        let name = destructure::field_name(member.first()?.text)?;
        let operator = if member.get(1)?.text == "?" { 2 } else { 1 };
        match member.get(operator)?.text {
            ":" => Some((name, member.get(operator + 1..))),
            "(" => Some((name, None)),
            _ => None,
        }
    })
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum KeyDomain {
    Number,
    String,
}

impl KeyDomain {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "number" => Some(Self::Number),
            "string" => Some(Self::String),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::String => "string",
        }
    }

    fn matches(self, selection: &Selection<'_>) -> bool {
        match selection {
            Selection::Index(_) | Selection::NumericKey(_) => true,
            _ => selection
                .property_name()
                .is_some_and(|key| self == Self::String || numeric_keys::is_numeric_name(&key)),
        }
    }
}

fn body_members<'tokens, 'source>(
    source: &'source str,
    mut body: &'tokens [Token<'source>],
) -> impl Iterator<Item = &'tokens [Token<'source>]> {
    std::iter::from_fn(move || {
        while body
            .first()
            .is_some_and(|token| matches!(token.text, ";" | ","))
        {
            body = &body[1..];
        }
        if body.first()?.text == "readonly" && !matches!(body.get(1)?.text, ":" | "?" | "(") {
            body = &body[1..];
        }
        let end = field_end(source, body);
        let member = body.get(..end)?;
        body = &body[end..];
        Some(member)
    })
}

fn index_signature<'tokens, 'source>(
    member: &'tokens [Token<'source>],
) -> Option<(KeyDomain, &'tokens [Token<'source>])> {
    let [open, name, colon, domain, close, value, annotation @ ..] = member else {
        return None;
    };
    if open.text != "["
        || !identifier(name.text)
        || colon.text != ":"
        || close.text != "]"
        || value.text != ":"
        || annotation.is_empty()
    {
        return None;
    }
    Some((KeyDomain::parse(domain.text)?, annotation))
}

fn new_member(tokens: &[Token<'_>]) -> bool {
    let tokens = match tokens {
        [modifier, rest @ ..]
            if modifier.text == "readonly"
                && rest
                    .first()
                    .is_some_and(|next| !matches!(next.text, ":" | "?" | "(")) =>
        {
            rest
        }
        _ => tokens,
    };
    match tokens {
        [open, name, colon, ..] if open.text == "[" => identifier(name.text) && colon.text == ":",
        [name, next, ..] => {
            destructure::field_name(name.text).is_some() && matches!(next.text, ":" | "?" | "(")
        }
        _ => false,
    }
}

fn field_end(source: &str, field: &[Token<'_>]) -> usize {
    let mut index = 0;
    while let Some(token) = field.get(index) {
        if matches!(token.text, "," | ";")
            || index > 1
                && source[field[index - 1].start..token.start].contains(['\r', '\n'])
                && new_member(&field[index..])
        {
            break;
        }
        index += if token.text == "<" {
            after_group(&field[index..], "<", ">")
        } else {
            skip_group(&field[index..])
        }
        .unwrap_or(field.len() - index);
    }
    index.max(1)
}

fn skip_group(tokens: &[Token<'_>]) -> Option<usize> {
    match tokens.first()?.text {
        "{" => after_group(tokens, "{", "}"),
        "(" => after_group(tokens, "(", ")"),
        "[" => after_group(tokens, "[", "]"),
        _ => Some(1),
    }
}

fn after_group(tokens: &[Token<'_>], open: &str, close: &str) -> Option<usize> {
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate() {
        if token.text == open {
            depth += 1;
        } else if token.text == close {
            depth -= 1;
            if depth == 0 {
                return Some(index + 1);
            }
        }
    }
    None
}

fn identifier(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_alphabetic() || matches!(ch, '_' | '$'))
}
