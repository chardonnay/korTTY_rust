//! Union and intersection syntax for same-document types.

use std::{collections::HashSet, hash::Hash};

use super::{MAX_LOOKUPS, Token, generics, identifier, skip_group};

#[derive(Clone, Copy)]
pub(super) enum Join {
    Union,
    Intersection,
}

impl Join {
    pub(super) fn combine_names<T: Clone + Eq + Hash>(
        self,
        mut names: impl Iterator<Item = Vec<T>>,
    ) -> Vec<T> {
        let mut result = names.next().unwrap_or_default();
        match self {
            Self::Union => {
                for other in names {
                    let other: HashSet<_> = other.into_iter().collect();
                    result.retain(|name| other.contains(name));
                }
            }
            Self::Intersection => {
                let mut seen: HashSet<_> = result.iter().cloned().collect();
                result.extend(names.flatten().filter(|name| seen.insert(name.clone())));
            }
        }
        result
    }
}

pub(super) struct Composition<'tokens, 'source> {
    pub(super) join: Join,
    pub(super) parts: Vec<&'tokens [Token<'source>]>,
}

pub(super) fn read<'tokens, 'source>(
    tokens: &'tokens [Token<'source>],
) -> Result<Option<Composition<'tokens, 'source>>, ()> {
    let Some(mut end) = atom_end(tokens) else {
        return Ok(None);
    };
    let mut start = 0;
    let mut union_start = 0;
    let mut unions = Vec::new();
    let mut intersections = Vec::new();
    while let Some(operator) = tokens.get(end).map(|token| token.text) {
        match operator {
            "|" => {
                unions.push(&tokens[union_start..end]);
                union_start = end + 1;
            }
            "&" => intersections.push(&tokens[start..end]),
            "extends" | "." | "[" => return Err(()),
            _ => break,
        }
        if unions.len() + intersections.len() >= MAX_LOOKUPS {
            return Err(());
        }
        start = end + 1;
        end = start + atom_end(&tokens[start..]).ok_or(())?;
    }
    if !unions.is_empty() {
        unions.push(&tokens[union_start..end]);
        Ok(Some(Composition {
            join: Join::Union,
            parts: unions,
        }))
    } else if !intersections.is_empty() {
        intersections.push(&tokens[start..end]);
        Ok(Some(Composition {
            join: Join::Intersection,
            parts: intersections,
        }))
    } else {
        Ok(None)
    }
}

fn atom_end(tokens: &[Token<'_>]) -> Option<usize> {
    let start = usize::from(matches!(tokens.first()?.text, "readonly" | "keyof"));
    let first = tokens.get(start)?.text;
    let mut end = start
        + match first {
            "(" | "[" | "{" => skip_group(&tokens[start..])?,
            _ if identifier(first) => {
                let (_, suffix) = generics::arguments(&tokens[start + 1..])?;
                tokens.len() - suffix.len() - start
            }
            _ if first.starts_with(['"', '\''])
                || first.starts_with(|ch: char| ch.is_ascii_digit()) =>
            {
                1
            }
            _ => return None,
        };
    while tokens
        .get(end..end + 2)
        .is_some_and(|pair| pair[0].text == "[" && pair[1].text == "]")
    {
        end += 2;
    }
    Some(end)
}
