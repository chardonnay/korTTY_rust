//! Public instance members for same-document class declarations.

use super::{Token, after_group, annotation, identifier, parameter_groups, skip_group};

pub(super) struct Declaration<'tokens, 'source> {
    pub(super) body: &'tokens [Token<'source>],
    pub(super) base: Option<&'tokens [Token<'source>]>,
}

pub(super) fn declaration<'tokens, 'source>(
    header: &'tokens [Token<'source>],
) -> Option<Declaration<'tokens, 'source>> {
    let mut index = 0;
    let mut base = None;
    while let Some(token) = header.get(index) {
        match token.text {
            "{" => {
                let end = after_group(&header[index..], "{", "}")?;
                return Some(Declaration {
                    body: &header[index + 1..index + end - 1],
                    base,
                });
            }
            ";" | "=" => return None,
            "<" => index += after_group(&header[index..], "<", ">")?,
            "extends" => {
                if let [_, name, operator, ..] = &header[index..]
                    && identifier(name.text)
                    && matches!(operator.text, "{" | "<" | "implements")
                {
                    let length = if operator.text == "<" {
                        1 + after_group(&header[index + 2..], "<", ">")?
                    } else {
                        1
                    };
                    base = Some(&header[index + 1..index + 1 + length]);
                }
                index += 1;
            }
            _ => index += skip_group(&header[index..])?,
        }
    }
    None
}

pub(super) struct Member<'tokens, 'source> {
    pub(super) name: &'source str,
    pub(super) annotation: Option<&'tokens [Token<'source>]>,
    pub(super) public: bool,
}

pub(super) fn members<'tokens, 'source>(
    source: &'source str,
    mut body: &'tokens [Token<'source>],
) -> impl Iterator<Item = Member<'tokens, 'source>> {
    let mut properties = parameter_groups(&[]);
    std::iter::from_fn(move || {
        loop {
            if let Some(property) = properties.find_map(parameter_property) {
                return Some(property);
            }
            let first = body.first()?;
            if matches!(first.text, ";" | ",") {
                body = &body[1..];
                continue;
            }
            let mut public = true;
            let mut instance = true;
            let mut getter = false;
            while let [modifier, next, ..] = body
                && member_modifier(modifier.text)
                && (identifier(next.text) || matches!(next.text, "#" | "{"))
            {
                public &= !matches!(modifier.text, "private" | "protected");
                instance &= modifier.text != "static";
                getter |= modifier.text == "get";
                body = &body[1..];
            }
            if body.first()?.text == "#" {
                instance = false;
                body = &body[1..];
            }
            let name = body.first()?.text;
            if !identifier(name) {
                body = &body[skip_group(body)?..];
                continue;
            }
            let mut operator = 1;
            if body
                .get(operator)
                .is_some_and(|token| matches!(token.text, "?" | "!"))
            {
                operator += 1;
            }
            if body.get(operator).is_some_and(|token| token.text == "<") {
                operator += after_group(&body[operator..], "<", ">").unwrap_or(0);
            }
            let (end, annotation) = if body.get(operator).is_some_and(|token| token.text == "(") {
                let after = operator + after_group(&body[operator..], "(", ")")?;
                if name == "constructor"
                    && instance
                    && body.get(after).is_some_and(|token| token.text == "{")
                {
                    properties = parameter_groups(&body[operator + 1..after - 1]);
                }
                let (length, return_type) = method_tail(source, &body[after..]);
                (after + length, getter.then_some(return_type).flatten())
            } else {
                let end = member_end(source, body, 1, false);
                let annotation = body
                    .get(operator)
                    .filter(|token| token.text == ":")
                    .and_then(|_| body.get(operator + 1..end));
                (end, annotation)
            };
            body = &body[end..];
            if instance && name != "constructor" {
                return Some(Member {
                    name,
                    annotation,
                    public,
                });
            }
        }
    })
}

fn parameter_property<'tokens, 'source>(
    mut tokens: &'tokens [Token<'source>],
) -> Option<Member<'tokens, 'source>> {
    let mut property = false;
    let mut public = true;
    while let [modifier, next, ..] = tokens
        && identifier(next.text)
        && matches!(
            modifier.text,
            "private" | "protected" | "public" | "readonly" | "override"
        )
    {
        property |= modifier.text != "override";
        public &= !matches!(modifier.text, "private" | "protected");
        tokens = &tokens[1..];
    }
    let [name, rest @ ..] = tokens else {
        return None;
    };
    if !property || !identifier(name.text) {
        return None;
    }
    Some(Member {
        name: name.text,
        annotation: annotation(rest),
        public,
    })
}

fn method_tail<'tokens, 'source>(
    source: &str,
    tail: &'tokens [Token<'source>],
) -> (usize, Option<&'tokens [Token<'source>]>) {
    let (mut end, annotation) = if tail.first().is_some_and(|token| token.text == ":") {
        let first_type = if tail.get(1).is_some_and(|token| token.text == "{") {
            1 + after_group(&tail[1..], "{", "}").unwrap_or(tail.len() - 1)
        } else {
            1
        };
        let end = member_end(source, tail, first_type, true);
        (end, Some(&tail[1..end]))
    } else {
        (0, None)
    };
    if tail.get(end).is_some_and(|token| token.text == "{") {
        end += after_group(&tail[end..], "{", "}").unwrap_or(tail.len() - end);
    }
    (end, annotation)
}

fn member_end(source: &str, tokens: &[Token<'_>], mut index: usize, method: bool) -> usize {
    while let Some(token) = tokens.get(index) {
        if matches!(token.text, ";" | ",") || (method && token.text == "{") {
            break;
        }
        let previous = &tokens[index - 1];
        if (identifier(token.text) || token.text == "#")
            && !matches!(previous.text, ":" | "=" | "." | "?" | "|" | "&")
            && source[previous.start + previous.text.len()..token.start].contains('\n')
            && (token.text == "#"
                || member_modifier(token.text)
                || tokens.get(index + 1).is_none_or(|next| {
                    matches!(next.text, ":" | "?" | "!" | "=" | "(" | ";" | "<")
                }))
        {
            break;
        }
        index += if token.text == "<" {
            after_group(&tokens[index..], "<", ">")
        } else {
            skip_group(&tokens[index..])
        }
        .unwrap_or(1);
    }
    index
}

fn member_modifier(name: &str) -> bool {
    matches!(
        name,
        "abstract"
            | "accessor"
            | "async"
            | "declare"
            | "get"
            | "override"
            | "private"
            | "protected"
            | "public"
            | "readonly"
            | "set"
            | "static"
    )
}
