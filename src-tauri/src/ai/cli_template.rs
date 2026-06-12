//! Parses and expands CLI argument templates without invoking a shell.
//!
//! Port of the Java `AiCliArgumentTemplate`.

use std::collections::HashMap;

pub const MODEL: &str = "{model}";
pub const REASONING: &str = "{reasoning}";
pub const PROMPT_FILE: &str = "{promptFile}";
pub const SYSTEM_PROMPT_FILE: &str = "{systemPromptFile}";
pub const USER_PROMPT_FILE: &str = "{userPromptFile}";
pub const STDIN_PROMPT: &str = "{stdinPrompt}";

/// Parsed CLI argument template.
#[derive(Debug, Clone)]
pub struct AiCliArgumentTemplate {
    source: String,
    arguments: Vec<String>,
}

/// Result of expanding a template for execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedArguments {
    pub arguments: Vec<String>,
    pub prompt_on_stdin: bool,
}

impl AiCliArgumentTemplate {
    /// Parses a template. Multi-line templates use one argument per non-blank
    /// line; single-line templates are split with quote/escape-aware parsing.
    pub fn parse(template: &str) -> Result<Self, String> {
        let source = template.trim().to_string();
        if source.is_empty() {
            return Err("AI CLI argument template must be configured.".to_string());
        }
        let arguments = if source.contains('\n') || source.contains('\r') {
            parse_line_arguments(&source)
        } else {
            parse_inline_arguments(&source)?
        };
        if arguments.is_empty() {
            return Err("AI CLI argument template must contain at least one argument.".to_string());
        }
        Ok(Self { source, arguments })
    }

    pub fn contains_prompt_placeholder(&self) -> bool {
        self.source.contains(PROMPT_FILE)
            || self.source.contains(SYSTEM_PROMPT_FILE)
            || self.source.contains(USER_PROMPT_FILE)
            || self.source.contains(STDIN_PROMPT)
    }

    pub fn contains_model_placeholder(&self) -> bool {
        self.source.contains(MODEL)
    }

    pub fn requires_model(template: &str) -> bool {
        template.contains(MODEL)
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn expand(&self, values: &HashMap<&str, String>) -> Result<Vec<String>, String> {
        Ok(self.expand_for_execution(values)?.arguments)
    }

    /// Expands placeholders; a standalone `{stdinPrompt}` argument is removed
    /// and reported via `prompt_on_stdin`.
    pub fn expand_for_execution(
        &self,
        values: &HashMap<&str, String>,
    ) -> Result<ExpandedArguments, String> {
        let mut expanded = Vec::new();
        let mut prompt_on_stdin = false;
        for argument in &self.arguments {
            if argument == STDIN_PROMPT {
                prompt_on_stdin = true;
                continue;
            }
            if argument.contains(STDIN_PROMPT) {
                return Err(
                    "AI CLI stdin prompt placeholder must be a standalone argument.".to_string(),
                );
            }
            let mut resolved = argument.clone();
            for (key, value) in values {
                resolved = resolved.replace(key, value);
            }
            expanded.push(resolved);
        }
        Ok(ExpandedArguments {
            arguments: expanded,
            prompt_on_stdin,
        })
    }
}

fn parse_line_arguments(template: &str) -> Vec<String> {
    template
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_inline_arguments(template: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaping = false;
    for character in template.chars() {
        if escaping {
            current.push(character);
            escaping = false;
            continue;
        }
        if character == '\\' && !in_single_quote {
            escaping = true;
            continue;
        }
        if character == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }
        if character == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }
        if character.is_whitespace() && !in_single_quote && !in_double_quote {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(character);
    }
    if escaping {
        current.push('\\');
    }
    if in_single_quote || in_double_quote {
        return Err("AI CLI argument template contains an unterminated quote.".to_string());
    }
    if !current.is_empty() {
        result.push(current);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_line_arguments_and_expands_placeholders_without_shell() {
        let template =
            AiCliArgumentTemplate::parse("--model\n{model}\n--prompt-file\n{promptFile}\n")
                .expect("template should parse");

        assert!(template.contains_prompt_placeholder());
        let mut values = HashMap::new();
        values.insert(MODEL, "custom-model".to_string());
        values.insert(PROMPT_FILE, "/tmp/prompt.txt".to_string());
        assert_eq!(
            template.expand(&values).expect("expand"),
            vec![
                "--model",
                "custom-model",
                "--prompt-file",
                "/tmp/prompt.txt"
            ]
        );
    }

    #[test]
    fn parses_quoted_inline_arguments_without_invoking_shell() {
        let template = AiCliArgumentTemplate::parse("--flag \"two words\" '{promptFile}'")
            .expect("template should parse");

        assert_eq!(
            template.arguments(),
            ["--flag", "two words", "{promptFile}"]
        );
    }

    #[test]
    fn expands_standalone_stdin_prompt_placeholder_for_execution() {
        let template = AiCliArgumentTemplate::parse("exec\n-\n{stdinPrompt}\n")
            .expect("template should parse");

        let expanded = template
            .expand_for_execution(&HashMap::new())
            .expect("expand");

        assert!(template.contains_prompt_placeholder());
        assert_eq!(expanded.arguments, vec!["exec", "-"]);
        assert!(expanded.prompt_on_stdin);
    }

    #[test]
    fn rejects_blank_template() {
        let error = AiCliArgumentTemplate::parse(" ").expect_err("blank template should fail");
        assert!(error.contains("must be configured"));
    }

    #[test]
    fn rejects_unterminated_quote() {
        let error = AiCliArgumentTemplate::parse("--flag \"unterminated")
            .expect_err("unterminated quote should fail");
        assert!(error.contains("unterminated quote"));
    }

    #[test]
    fn rejects_embedded_stdin_prompt_placeholder() {
        let template =
            AiCliArgumentTemplate::parse("--input={stdinPrompt}").expect("template should parse");
        let error = template
            .expand_for_execution(&HashMap::new())
            .expect_err("embedded stdin placeholder should fail");
        assert!(error.contains("standalone argument"));
    }

    #[test]
    fn backslash_escapes_outside_single_quotes() {
        let template =
            AiCliArgumentTemplate::parse(r"a\ b '\literal' end").expect("template should parse");
        assert_eq!(template.arguments(), ["a b", r"\literal", "end"]);
    }

    #[test]
    fn trailing_backslash_is_kept_literal() {
        let template = AiCliArgumentTemplate::parse(r"value\").expect("template should parse");
        assert_eq!(template.arguments(), [r"value\"]);
    }

    #[test]
    fn requires_model_detects_model_placeholder() {
        assert!(AiCliArgumentTemplate::requires_model("--model {model}"));
        assert!(!AiCliArgumentTemplate::requires_model("{promptFile}"));
    }
}
