use crate::model::snippet::Snippet;
use anyhow::{bail, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

const MAX_EMBEDDED_ECHO_LINE_CHARS: usize = 48_000;
const EMBEDDED_HEREDOC_DELIM: &str = "KORTTY_B64_EOF";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetGlobalVariable {
    pub name: String,
    pub value: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetOneLinerRequest {
    pub snippet: Snippet,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub global_variables: Vec<SnippetGlobalVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetOneLinerResult {
    pub line: String,
}

pub fn build_embedded_one_liner(request: &SnippetOneLinerRequest) -> Result<SnippetOneLinerResult> {
    let lang = normalize_language(request.snippet.language.as_deref())
        .ok_or_else(|| anyhow::anyhow!("Snippet language is not supported for one-liners"))?;
    let mut content = request.snippet.content.trim().to_string();
    if content.is_empty() {
        bail!("Snippet content is empty");
    }
    content = apply_global_variables(&content, &request.global_variables);
    if content.trim().is_empty() {
        bail!("Snippet content is empty after variable expansion");
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let interpreter = interpreter_command(lang, !request.arguments.is_empty());
    let args = shell_arguments(&request.arguments);
    let command = format!("{interpreter}{args}");
    let line = format!("echo '{encoded}' | base64 -d | {command}");
    if line.len() > MAX_EMBEDDED_ECHO_LINE_CHARS {
        return Ok(SnippetOneLinerResult {
            line: format!(
                "base64 -d <<'{EMBEDDED_HEREDOC_DELIM}' | {command}\n{encoded}\n{EMBEDDED_HEREDOC_DELIM}\n"
            ),
        });
    }
    Ok(SnippetOneLinerResult { line })
}

pub fn apply_global_variables(content: &str, variables: &[SnippetGlobalVariable]) -> String {
    let mut result = content.to_string();
    for variable in variables {
        let name = variable.name.trim();
        if name.is_empty() {
            continue;
        }
        result = result.replace(&format!("{{{{{name}}}}}"), &variable.value);
        result = result.replace(&format!("${{{name}}}"), &variable.value);
    }
    result
}

fn normalize_language(language: Option<&str>) -> Option<&'static str> {
    match language.unwrap_or("").trim().to_lowercase().as_str() {
        "bash" | "shell" => Some("bash"),
        "python" => Some("python"),
        "perl" => Some("perl"),
        "ruby" => Some("ruby"),
        _ => None,
    }
}

fn interpreter_command(language: &str, has_arguments: bool) -> &'static str {
    match language {
        "bash" if has_arguments => "bash -s --",
        "bash" => "bash",
        "python" if has_arguments => "python3 -",
        "python" => "python3",
        "perl" if has_arguments => "perl -",
        "perl" => "perl",
        "ruby" if has_arguments => "ruby -",
        "ruby" => "ruby",
        _ => "bash",
    }
}

fn shell_arguments(arguments: &[String]) -> String {
    if arguments.is_empty() {
        return String::new();
    }
    arguments
        .iter()
        .map(|argument| format!(" '{}'", shell_escape_single_quoted(argument)))
        .collect::<Vec<_>>()
        .join("")
}

pub fn shell_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "'\\''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_one_liner_passes_arguments_as_argv() {
        let snippet = Snippet {
            content: "printf '%s\\n' \"$1\"".into(),
            language: Some("bash".into()),
            ..Snippet::default()
        };
        let result = build_embedded_one_liner(&SnippetOneLinerRequest {
            snippet,
            arguments: vec!["hello world".into(), "a'b".into()],
            global_variables: Vec::new(),
        })
        .unwrap();

        assert!(result.line.contains("bash -s -- 'hello world' 'a'\\''b'"));
    }

    #[test]
    fn global_variables_expand_known_placeholders() {
        let expanded = apply_global_variables(
            "echo {{HOST}} ${PORT}",
            &[
                SnippetGlobalVariable {
                    name: "HOST".into(),
                    value: "example.test".into(),
                    description: None,
                },
                SnippetGlobalVariable {
                    name: "PORT".into(),
                    value: "22".into(),
                    description: None,
                },
            ],
        );

        assert_eq!(expanded, "echo example.test 22");
    }
}
