//! Literal keys from visible, immutable local bindings.

use super::destructure::{Selection, subscript};
use super::{
    MAX_LOOKUPS, Token, after_group, before_group, comma_groups, continues_expression,
    destructured_binding, ends_expression, identifier, numeric_keys, parameter, parameters,
    skip_group, visible_indices, within_arrow_expression,
};

const MAX_DECLARATION_TOKENS: usize = 1024;

pub(super) fn resolve<'source>(
    source: &str,
    tokens: &[Token<'source>],
    mut before: usize,
    mut name: &'source str,
    mut stringify: bool,
) -> Option<Selection<'source>> {
    for _ in 0..MAX_LOOKUPS {
        let (declaration, value) = initializer(source, tokens, before, name)?;
        if stringify && let Some(key) = numeric_keys::bigint_name(value) {
            return Some(Selection::OwnedField(key));
        }
        match subscript(value) {
            key @ (Selection::Field(_)
            | Selection::OwnedField(_)
            | Selection::Index(_)
            | Selection::NumericKey(_))
                if !stringify =>
            {
                return Some(key);
            }
            Selection::Variable(alias, conversion)
                if alias != name && !(stringify && conversion) =>
            {
                name = alias;
                before = declaration;
                stringify |= conversion;
            }
            _ => return None,
        }
    }
    None
}

fn initializer<'tokens, 'source>(
    source: &str,
    tokens: &'tokens [Token<'source>],
    before: usize,
    name: &str,
) -> Option<(usize, &'tokens [Token<'source>])> {
    for index in visible_indices(&tokens[..before]) {
        let token = tokens[index].text;
        if matches!(token, "const" | "let" | "var") {
            let rest = &tokens[index + 1..before];
            let Some(items) = declaration_list(source, rest) else {
                if rest.first().is_some_and(|token| token.text == name) {
                    return None;
                }
                continue;
            };
            for item in comma_groups(items, false) {
                if item.first().is_some_and(|key| key.text == name) {
                    if token != "const" {
                        return None;
                    }
                    let declaration =
                        index + 1 + items.partition_point(|key| key.start < item[0].start);
                    return Some((declaration, literal_initializer(&item[1..])?));
                }
                if destructured_binding(item, name).is_some() {
                    return None;
                }
            }
        }
        if matches!(token, "function" | "class" | "enum")
            && tokens.get(index + 1).is_some_and(|key| key.text == name)
        {
            return None;
        }
        let header_end = match token {
            "{" => index,
            "=>" if within_arrow_expression(source, &tokens[index + 1..=before]) => index + 1,
            _ => continue,
        };
        let header = &tokens[..header_end];
        if parameters(header)
            .or_else(|| catch_parameter(header))
            .is_some_and(|values| parameter(values, name).is_some())
        {
            return None;
        }
    }
    None
}

fn catch_parameter<'tokens, 'source>(
    header: &'tokens [Token<'source>],
) -> Option<&'tokens [Token<'source>]> {
    let close = header.len().checked_sub(1)?;
    if header[close].text != ")" {
        return None;
    }
    let open = before_group(header, "(", ")")?;
    (header.get(open.checked_sub(1)?)?.text == "catch").then_some(&header[open + 1..close])
}

fn declaration_list<'tokens, 'source>(
    source: &str,
    tokens: &'tokens [Token<'source>],
) -> Option<&'tokens [Token<'source>]> {
    let mut end = 0;
    let mut assigned = false;
    while let Some(token) = tokens.get(end) {
        if matches!(token.text, ";" | "}" | ")" | "in" | "of") {
            break;
        }
        if end > 0 {
            let previous = tokens[end - 1];
            if assigned
                && ends_expression(previous.text)
                && !continues_expression(token.text)
                && token.text != ","
                && source[previous.start + previous.text.len()..token.start].contains(['\r', '\n'])
            {
                break;
            }
        }
        if end >= MAX_DECLARATION_TOKENS {
            return None;
        }
        assigned |= token.text == "=";
        if token.text == "," {
            assigned = false;
        }
        end += if token.text == "<" && !assigned {
            after_group(&tokens[end..], "<", ">")?
        } else {
            skip_group(&tokens[end..])?
        };
    }
    Some(&tokens[..end])
}

fn literal_initializer<'tokens, 'source>(
    tokens: &'tokens [Token<'source>],
) -> Option<&'tokens [Token<'source>]> {
    let start = match tokens.first()?.text {
        "=" => 0,
        ":" => {
            let mut index = 1;
            while tokens.get(index)?.text != "=" {
                index += if tokens[index].text == "<" {
                    after_group(&tokens[index..], "<", ">")?
                } else {
                    skip_group(&tokens[index..])?
                };
            }
            index
        }
        _ => return None,
    };
    match &tokens[start + 1..] {
        value @ [literal] if !literal.text.is_empty() => Some(value),
        [value, keyword, modifier]
            if keyword.text == "as" && modifier.text == "const" && !identifier(value.text) =>
        {
            Some(&tokens[start + 1..start + 2])
        }
        [value @ .., keyword, modifier]
            if keyword.text == "as"
                && modifier.text == "const"
                && (numeric_keys::literal(value).is_some()
                    || numeric_keys::bigint_name(value).is_some()) =>
        {
            Some(value)
        }
        value
            if numeric_keys::literal(value).is_some()
                || numeric_keys::bigint_name(value).is_some()
                || numeric_keys::string_conversion(value).is_some() =>
        {
            Some(value)
        }
        _ => None,
    }
}
