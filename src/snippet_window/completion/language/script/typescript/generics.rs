//! Type arguments for same-document generic declarations.

use std::rc::Rc;

use super::{Token, after_group, comma_groups, identifier};

const MAX_TYPE_PARAMETERS: usize = 64;

#[derive(Clone)]
pub(super) struct Type<'tokens, 'source> {
    pub(super) tokens: &'tokens [Token<'source>],
    bindings: Rc<Vec<Binding<'tokens, 'source>>>,
}

#[derive(Clone)]
struct Binding<'tokens, 'source> {
    name: &'source str,
    value: Option<Type<'tokens, 'source>>,
}

impl<'tokens, 'source> Type<'tokens, 'source> {
    pub(super) fn new(tokens: &'tokens [Token<'source>]) -> Self {
        Self {
            tokens,
            bindings: Rc::default(),
        }
    }

    pub(super) fn with_tokens(&self, tokens: &'tokens [Token<'source>]) -> Self {
        Self {
            tokens,
            bindings: Rc::clone(&self.bindings),
        }
    }

    pub(super) fn parameter(&self, name: &str) -> Option<Option<Self>> {
        self.bindings
            .iter()
            .find(|binding| binding.name == name)
            .map(|binding| binding.value.clone())
    }

    pub(super) fn apply(
        &self,
        parameters: &'tokens [Token<'source>],
        arguments: &'tokens [Token<'source>],
        definition: &'tokens [Token<'source>],
    ) -> Self {
        let mut bindings = Vec::new();
        let mut arguments = comma_groups(arguments, true);
        for parameter in comma_groups(parameters, true).take(MAX_TYPE_PARAMETERS) {
            let Some(name) = parameter.first().filter(|token| identifier(token.text)) else {
                continue;
            };
            let value = arguments
                .next()
                .filter(|argument| !argument.is_empty())
                .map(|argument| self.with_tokens(argument))
                .or_else(|| {
                    let start = parameter.iter().position(|token| token.text == "=")? + 1;
                    Some(Self {
                        tokens: &parameter[start..],
                        bindings: Rc::new(bindings.clone()),
                    })
                });
            bindings.push(Binding {
                name: name.text,
                value,
            });
        }
        Self {
            tokens: definition,
            bindings: Rc::new(bindings),
        }
    }
}

pub(super) fn arguments<'tokens, 'source>(
    tokens: &'tokens [Token<'source>],
) -> Option<(&'tokens [Token<'source>], &'tokens [Token<'source>])> {
    if tokens.first().is_some_and(|token| token.text == "<") {
        let after = after_group(tokens, "<", ">")?;
        Some((&tokens[1..after - 1], &tokens[after..]))
    } else {
        Some((&[], tokens))
    }
}
