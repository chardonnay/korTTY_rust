//! Shared code formatter service for snippet and file editors.
//!
//! Port of the Java `de.kortty.core.CodeFormatterService`. Built-in formatters
//! (JSON, XML, YAML, TOML, INI/properties, Groovy indent normalization) are
//! implemented natively, external formatters (shfmt, prettier, black, perltidy,
//! gofmt, rustfmt, sql-formatter, google-java-format, rubocop, terraform fmt)
//! are resolved from bundled formatter roots or the PATH.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const GOOGLE_JAVA_FORMAT_VERSION: &str = "1.35.0";
pub const SHFMT_VERSION: &str = "3.13.1";
pub const PRETTIER_VERSION: &str = "3.6.2";
pub const SQL_FORMATTER_VERSION: &str = "15.7.3";
pub const PERL_TIDY_VERSION: &str = "20260204";

const JSON_INDENT: usize = 2;
const DEFAULT_INDENT: usize = 4;
const FORMATTER_TIMEOUT_SECONDS: u64 = 15;
const MIN_LINE_WIDTH: u32 = 20;
const MAX_LINE_WIDTH: u32 = 240;
const MAX_STDERR_EXCERPT_CHARS: usize = 2000;

/// Embedded copy of the bundled formatter manifest (versions + sources).
pub const FORMATTER_MANIFEST: &str =
    include_str!("../resources/formatters/formatter-manifest.properties");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormatterProviderType {
    BuiltIn,
    Bundled,
    ExternalFallback,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormatterExecutionMode {
    Stdin,
    FileAppend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatterInfo {
    pub language: String,
    #[serde(default)]
    pub formatter_id: Option<String>,
    pub display_name: String,
    pub provider_type: FormatterProviderType,
    #[serde(default)]
    pub execution_mode: Option<FormatterExecutionMode>,
    #[serde(default)]
    pub command_line: Vec<String>,
    #[serde(default)]
    pub command_name: Option<String>,
    #[serde(default)]
    pub install_hint: Option<String>,
    #[serde(default)]
    pub file_extension: Option<String>,
    pub available: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

impl FormatterInfo {
    pub fn is_supported(&self) -> bool {
        self.provider_type != FormatterProviderType::Unavailable || self.formatter_id.is_some()
    }

    pub fn is_bundled_or_built_in(&self) -> bool {
        matches!(
            self.provider_type,
            FormatterProviderType::BuiltIn | FormatterProviderType::Bundled
        )
    }
}

pub fn is_supported(language: &str) -> bool {
    get_formatter_info(language).is_some()
}

pub fn is_formatter_available(info: Option<&FormatterInfo>) -> bool {
    info.map(|value| value.available).unwrap_or(false)
}

pub fn supports_line_width(language: &str) -> bool {
    get_formatter_info(language)
        .as_ref()
        .map(supports_line_width_info)
        .unwrap_or(false)
}

fn supports_line_width_info(info: &FormatterInfo) -> bool {
    matches!(
        info.formatter_id.as_deref(),
        Some("prettier") | Some("black") | Some("perltidy")
    )
}

pub fn get_formatter_info(language: &str) -> Option<FormatterInfo> {
    get_formatter_info_with_roots(language, &formatter_roots())
}

fn get_formatter_info_with_roots(language: &str, roots: &[PathBuf]) -> Option<FormatterInfo> {
    let lang = normalize_language(language);
    match lang.as_str() {
        "json" => Some(built_in(&lang, "json", ".json")),
        "xml" => Some(built_in(&lang, "xml", ".xml")),
        "yaml" | "yml" => Some(built_in(&lang, "yaml", ".yml")),
        "toml" => Some(built_in(&lang, "toml", ".toml")),
        "ini" | "properties" => Some(built_in(&lang, "ini", ".properties")),
        "groovy" => Some(built_in(&lang, "groovy", ".groovy")),
        "bash" | "shell" | "sh" | "zsh" => Some(shell_formatter(&lang, roots)),
        "html" => Some(prettier_formatter(&lang, "html", ".html", roots)),
        "css" => Some(prettier_formatter(&lang, "css", ".css", roots)),
        "javascript" | "typescript" => Some(prettier_formatter(&lang, "typescript", ".js", roots)),
        "java" => Some(java_formatter(&lang, roots)),
        "perl" | "pl" => Some(perl_formatter(&lang, roots)),
        "python" => Some(external_only(
            &lang,
            "black",
            vec!["black".into(), "-q".into(), "-".into()],
            FormatterExecutionMode::Stdin,
            "Install black for developer fallback: pip install black  /  brew install black",
            ".py",
        )),
        "ruby" => Some(external_only(
            &lang,
            "rubocop",
            // RuboCop's `--stdin` reads the source from stdin and writes the
            // autocorrected source to stdout, using the trailing path only as
            // the logical file name for cop configuration. Driving it through
            // the stdin pipe (instead of appending a temp file) avoids the
            // deadlock where RuboCop blocks reading stdin while we wait for it.
            vec![
                "rubocop".into(),
                "-a".into(),
                "--stderr".into(),
                "--stdin".into(),
                "snippet.rb".into(),
            ],
            FormatterExecutionMode::Stdin,
            "Install RuboCop for developer fallback: gem install rubocop",
            ".rb",
        )),
        "go" => Some(external_only(
            &lang,
            "gofmt",
            vec!["gofmt".into()],
            FormatterExecutionMode::Stdin,
            "Install Go for developer fallback; gofmt is included with Go.",
            ".go",
        )),
        "rust" => Some(external_only(
            &lang,
            "rustfmt",
            vec!["rustfmt".into()],
            FormatterExecutionMode::Stdin,
            "Install rustfmt for developer fallback: rustup component add rustfmt",
            ".rs",
        )),
        "sql" => Some(sql_formatter(&lang, roots)),
        "terraform" => Some(external_only(
            &lang,
            "terraform",
            vec!["terraform".into(), "fmt".into()],
            FormatterExecutionMode::FileAppend,
            "Install Terraform for developer fallback: https://developer.hashicorp.com/terraform/install",
            ".tf",
        )),
        _ => None,
    }
}

/// Formats `content` with the formatter configured for `language`.
///
/// Built-in formatters run in-process, bundled and external formatters are
/// executed as child processes with a 15 second timeout.
pub async fn format(language: &str, content: &str, max_line_length: Option<u32>) -> Result<String> {
    format_with_roots(language, content, max_line_length, &formatter_roots()).await
}

async fn format_with_roots(
    language: &str,
    content: &str,
    max_line_length: Option<u32>,
    roots: &[PathBuf],
) -> Result<String> {
    if content.trim().is_empty() {
        bail!("There is no content to format");
    }
    let info = get_formatter_info_with_roots(language, roots).ok_or_else(|| {
        anyhow!(
            "No formatter is configured for {}",
            normalize_language(language)
        )
    })?;
    let safe_line_width = normalize_line_width(max_line_length);
    if safe_line_width.is_some() && !supports_line_width_info(&info) {
        bail!(
            "Formatter does not support a configurable line width for {}",
            info.language
        );
    }
    if !info.available {
        bail!(
            "{}",
            info.unavailable_reason
                .clone()
                .unwrap_or_else(|| format!("Formatter unavailable for {}", info.language))
        );
    }
    if info.provider_type == FormatterProviderType::BuiltIn {
        return format_built_in(content, &info.language)
            .ok_or_else(|| anyhow!("Could not format {} content", info.language));
    }
    let command_line = command_line_with_line_width(&info, safe_line_width);
    match info.execution_mode {
        Some(FormatterExecutionMode::Stdin) => run_stdin_formatter(&command_line, content).await,
        Some(FormatterExecutionMode::FileAppend) => {
            run_file_formatter(&command_line, content, info.file_extension.as_deref()).await
        }
        None => bail!(
            "No formatter execution configured for {}",
            info.display_name
        ),
    }
}

/// Clamps a requested line width into the supported 20..=240 range.
pub fn normalize_line_width(max_line_length: Option<u32>) -> Option<u32> {
    max_line_length.map(|value| value.clamp(MIN_LINE_WIDTH, MAX_LINE_WIDTH))
}

/// Returns the command line of `info` extended with the formatter-specific
/// line-width option. Only prettier (`--print-width`), black (`--line-length`)
/// and perltidy (`-l=`) support a configurable width.
pub fn command_line_with_line_width(
    info: &FormatterInfo,
    max_line_length: Option<u32>,
) -> Vec<String> {
    let mut command_line = info.command_line.clone();
    let Some(width) = max_line_length else {
        return command_line;
    };
    match info.formatter_id.as_deref() {
        Some("prettier") => {
            command_line.push("--print-width".into());
            command_line.push(width.to_string());
        }
        Some("black") => {
            let insert_at = if command_line.last().map(String::as_str) == Some("-") {
                command_line.len() - 1
            } else {
                command_line.len()
            };
            command_line.insert(insert_at, "--line-length".into());
            command_line.insert(insert_at + 1, width.to_string());
        }
        Some("perltidy") => command_line.push(format!("-l={width}")),
        _ => {
            // The caller guards unsupported formatter ids before this point.
        }
    }
    command_line
}

/// Parses the embedded formatter manifest into a key/value map.
pub fn formatter_manifest() -> HashMap<String, String> {
    parse_formatter_manifest(FORMATTER_MANIFEST)
}

pub fn parse_formatter_manifest(content: &str) -> HashMap<String, String> {
    let mut manifest = HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        if let Some(eq) = trimmed.find('=') {
            let key = trimmed[..eq].trim();
            let value = trimmed[eq + 1..].trim();
            if !key.is_empty() {
                manifest.insert(key.to_string(), value.to_string());
            }
        }
    }
    manifest
}

// --- Formatter table helpers -------------------------------------------------

fn built_in(language: &str, formatter_id: &str, file_extension: &str) -> FormatterInfo {
    FormatterInfo {
        language: language.to_string(),
        formatter_id: Some(formatter_id.to_string()),
        display_name: "integrated".to_string(),
        provider_type: FormatterProviderType::BuiltIn,
        execution_mode: None,
        command_line: Vec::new(),
        command_name: None,
        install_hint: None,
        file_extension: Some(file_extension.to_string()),
        available: true,
        unavailable_reason: None,
    }
}

fn java_formatter(language: &str, roots: &[PathBuf]) -> FormatterInfo {
    // The Java build ships google-java-format as an embedded library. The Rust
    // build can only run it as a bundled executable from a formatter root.
    if let Some(bundled) = find_bundled_executable(
        roots,
        "google-java-format",
        &executable_name("google-java-format"),
    ) {
        return bundled_process(
            language,
            "google-java-format",
            &format!("google-java-format {GOOGLE_JAVA_FORMAT_VERSION} (bundled)"),
            vec![bundled.to_string_lossy().to_string(), "-".into()],
            FormatterExecutionMode::Stdin,
            ".java",
        );
    }
    let external = external_candidate(
        language,
        "google-java-format",
        vec!["google-java-format".into(), "-".into()],
        FormatterExecutionMode::Stdin,
        "Install google-java-format for developer fallback.",
        ".java",
    );
    if external.available {
        return external;
    }
    unavailable(
        language,
        "google-java-format",
        ".java",
        "Bundled google-java-format is unavailable and optional external fallback was not found in PATH.",
    )
}

fn shell_formatter(language: &str, roots: &[PathBuf]) -> FormatterInfo {
    if let Some(bundled) = find_bundled_executable(roots, "shfmt", &executable_name("shfmt")) {
        return bundled_process(
            language,
            "shfmt",
            &format!("shfmt {SHFMT_VERSION} (bundled)"),
            vec![bundled.to_string_lossy().to_string()],
            FormatterExecutionMode::Stdin,
            ".sh",
        );
    }
    let external = external_candidate(
        language,
        "shfmt",
        vec!["shfmt".into()],
        FormatterExecutionMode::Stdin,
        "Install shfmt for developer fallback: brew install shfmt  /  go install mvdan.cc/sh/v3/cmd/shfmt@latest",
        ".sh",
    );
    if external.available {
        return external;
    }
    unavailable(
        language,
        "shfmt",
        ".sh",
        "Bundled shfmt is missing from this KorTTY build and optional external fallback was not found in PATH.",
    )
}

fn prettier_formatter(
    language: &str,
    parser: &str,
    file_extension: &str,
    roots: &[PathBuf],
) -> FormatterInfo {
    let node = find_bundled_node(roots);
    let prettier = find_bundled_file(roots, "prettier", Path::new("bin/prettier.cjs"));
    if let (Some(node), Some(prettier)) = (node, prettier) {
        return bundled_process(
            language,
            "prettier",
            &format!("Prettier {PRETTIER_VERSION} (bundled)"),
            vec![
                node.to_string_lossy().to_string(),
                prettier.to_string_lossy().to_string(),
                "--parser".into(),
                parser.to_string(),
            ],
            FormatterExecutionMode::Stdin,
            file_extension,
        );
    }
    let external = external_candidate(
        language,
        "prettier",
        vec!["prettier".into(), "--parser".into(), parser.to_string()],
        FormatterExecutionMode::Stdin,
        "Install Prettier for developer fallback: npm install -g prettier",
        file_extension,
    );
    if external.available {
        return external;
    }
    unavailable(
        language,
        "prettier",
        file_extension,
        "Bundled Prettier runtime is missing from this KorTTY build and optional external fallback was not found in PATH.",
    )
}

fn perl_formatter(language: &str, roots: &[PathBuf]) -> FormatterInfo {
    let perl = find_command_on_path("perl");
    let perltidy_bin = find_bundled_file(roots, "perltidy", Path::new("bin/perltidy"));
    let perltidy_lib = find_bundled_file(roots, "perltidy", Path::new("lib"));
    if let (Some(perl), Some(bin), Some(lib)) = (&perl, &perltidy_bin, &perltidy_lib) {
        return bundled_process(
            language,
            "perltidy",
            &format!("Perl::Tidy {PERL_TIDY_VERSION} (bundled)"),
            vec![
                perl.to_string_lossy().to_string(),
                "-I".into(),
                lib.to_string_lossy().to_string(),
                bin.to_string_lossy().to_string(),
                "-st".into(),
            ],
            FormatterExecutionMode::Stdin,
            ".pl",
        );
    }
    let external = external_candidate(
        language,
        "perltidy",
        vec!["perltidy".into(), "-st".into()],
        FormatterExecutionMode::Stdin,
        "Install perltidy for developer fallback: brew install perltidy  /  cpan Perl::Tidy",
        ".pl",
    );
    if external.available {
        return external;
    }
    let reason = if perltidy_bin.is_some() && perltidy_lib.is_some() && perl.is_none() {
        "Bundled Perl::Tidy is available, but no local perl runtime was found in PATH"
    } else {
        "Bundled Perl::Tidy is missing from this KorTTY build"
    };
    unavailable(
        language,
        "perltidy",
        ".pl",
        &format!("{reason} and optional external fallback was not found in PATH."),
    )
}

fn sql_formatter(language: &str, roots: &[PathBuf]) -> FormatterInfo {
    let node = find_bundled_node(roots);
    let sql = find_bundled_file(
        roots,
        "sql-formatter",
        Path::new("kortty-sql-formatter.cjs"),
    );
    if let (Some(node), Some(sql)) = (node, sql) {
        return bundled_process(
            language,
            "sql-formatter",
            &format!("sql-formatter {SQL_FORMATTER_VERSION} (bundled)"),
            vec![
                node.to_string_lossy().to_string(),
                sql.to_string_lossy().to_string(),
            ],
            FormatterExecutionMode::Stdin,
            ".sql",
        );
    }
    let external = external_candidate(
        language,
        "sql-formatter",
        vec!["sql-formatter".into()],
        FormatterExecutionMode::Stdin,
        "Install sql-formatter for developer fallback: npm install -g sql-formatter",
        ".sql",
    );
    if external.available {
        return external;
    }
    unavailable(
        language,
        "sql-formatter",
        ".sql",
        "Bundled sql-formatter runtime is missing from this KorTTY build and optional external fallback was not found in PATH.",
    )
}

fn external_only(
    language: &str,
    command_name: &str,
    command_line: Vec<String>,
    mode: FormatterExecutionMode,
    install_hint: &str,
    file_extension: &str,
) -> FormatterInfo {
    let external = external_candidate(
        language,
        command_name,
        command_line,
        mode,
        install_hint,
        file_extension,
    );
    if external.available {
        return external;
    }
    unavailable(
        language,
        command_name,
        file_extension,
        &format!(
            "No bundled formatter is configured for {language} yet and optional external fallback '{command_name}' was not found in PATH."
        ),
    )
}

fn external_candidate(
    language: &str,
    command_name: &str,
    command_line: Vec<String>,
    mode: FormatterExecutionMode,
    install_hint: &str,
    file_extension: &str,
) -> FormatterInfo {
    let Some(command) = find_command_on_path(command_name) else {
        return unavailable(
            language,
            command_name,
            file_extension,
            &format!("Optional external formatter was not found in PATH: {command_name}"),
        );
    };
    let mut resolved = command_line;
    if !resolved.is_empty() {
        resolved[0] = command.to_string_lossy().to_string();
    }
    FormatterInfo {
        language: language.to_string(),
        formatter_id: Some(command_name.to_string()),
        display_name: format!("{command_name} (external fallback)"),
        provider_type: FormatterProviderType::ExternalFallback,
        execution_mode: Some(mode),
        command_line: resolved,
        command_name: Some(command_name.to_string()),
        install_hint: Some(install_hint.to_string()),
        file_extension: Some(file_extension.to_string()),
        available: true,
        unavailable_reason: None,
    }
}

fn bundled_process(
    language: &str,
    formatter_id: &str,
    display_name: &str,
    command_line: Vec<String>,
    mode: FormatterExecutionMode,
    file_extension: &str,
) -> FormatterInfo {
    FormatterInfo {
        language: language.to_string(),
        formatter_id: Some(formatter_id.to_string()),
        display_name: display_name.to_string(),
        provider_type: FormatterProviderType::Bundled,
        execution_mode: Some(mode),
        command_line,
        command_name: Some(formatter_id.to_string()),
        install_hint: None,
        file_extension: Some(file_extension.to_string()),
        available: true,
        unavailable_reason: None,
    }
}

fn unavailable(
    language: &str,
    formatter_id: &str,
    file_extension: &str,
    reason: &str,
) -> FormatterInfo {
    FormatterInfo {
        language: language.to_string(),
        formatter_id: Some(formatter_id.to_string()),
        display_name: formatter_id.to_string(),
        provider_type: FormatterProviderType::Unavailable,
        execution_mode: None,
        command_line: Vec::new(),
        command_name: Some(formatter_id.to_string()),
        install_hint: None,
        file_extension: Some(file_extension.to_string()),
        available: false,
        unavailable_reason: Some(reason.to_string()),
    }
}

fn normalize_language(language: &str) -> String {
    let trimmed = language.trim();
    if trimmed.is_empty() {
        "plain".to_string()
    } else {
        trimmed.to_lowercase()
    }
}

// --- Built-in formatters -----------------------------------------------------

fn format_built_in(text: &str, language: &str) -> Option<String> {
    match normalize_language(language).as_str() {
        "json" => Some(pretty_print_json(text.trim())),
        "xml" => format_xml(text),
        "yaml" | "yml" => Some(format_yaml(text)),
        "toml" => Some(format_toml(text)),
        "ini" | "properties" => Some(format_ini(text)),
        "groovy" => Some(normalize_indent(text, DEFAULT_INDENT)),
        _ => None,
    }
}

fn pretty_print_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut indent: i32 = 0;
    let mut in_string = false;
    let mut string_char = '\0';
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                out.push(c);
                escaped = true;
            } else if c == string_char {
                out.push(c);
                in_string = false;
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                string_char = c;
                out.push(c);
                i += 1;
            }
            '{' | '[' => {
                out.push(c);
                indent += JSON_INDENT as i32;
                i += 1;
                if i < chars.len() && !is_space(chars[i]) {
                    out.push('\n');
                    push_spaces(&mut out, indent);
                }
            }
            '}' | ']' => {
                indent -= JSON_INDENT as i32;
                if indent < 0 {
                    indent = 0;
                }
                let last = out.chars().last();
                if !out.is_empty() && last != Some(',') && last != Some('{') && last != Some('[') {
                    out.push('\n');
                    push_spaces(&mut out, indent);
                }
                out.push(c);
                i += 1;
            }
            ',' => {
                out.push(c);
                out.push('\n');
                push_spaces(&mut out, indent);
                i += 1;
            }
            ':' => {
                out.push(c);
                if i + 1 < chars.len() && chars[i + 1] != ' ' {
                    out.push(' ');
                }
                i += 1;
            }
            _ => {
                if !c.is_whitespace()
                    || (!out.is_empty() && !out.chars().last().unwrap().is_whitespace())
                {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    out
}

fn push_spaces(out: &mut String, indent: i32) {
    for _ in 0..indent.max(0) {
        out.push(' ');
    }
}

fn is_space(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

#[derive(Debug, Clone, PartialEq)]
enum XmlToken {
    Open(String),
    Close(String),
    SelfClose(String),
    Declaration(String),
    ProcessingInstruction(String),
    Comment(String),
    Cdata(String),
    Text(String),
}

/// Re-indents XML with two spaces per level, mirroring the Java port's JAXP
/// transformer output (XML declaration omitted, text-only elements inline).
/// Returns `None` for content that cannot be tokenized.
fn format_xml(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('<') {
        return None;
    }
    let tokens = tokenize_xml(trimmed)?;
    if tokens.is_empty() {
        return None;
    }
    Some(render_xml(&tokens))
}

fn tokenize_xml(input: &str) -> Option<Vec<XmlToken>> {
    let mut tokens = Vec::new();
    let mut rest = input;
    while !rest.is_empty() {
        let Some(start) = rest.find('<') else {
            if !rest.trim().is_empty() {
                tokens.push(XmlToken::Text(rest.trim().to_string()));
            }
            break;
        };
        let text = &rest[..start];
        if !text.trim().is_empty() {
            tokens.push(XmlToken::Text(text.trim().to_string()));
        }
        rest = &rest[start..];
        if rest.starts_with("<!--") {
            let end = rest.find("-->")? + 3;
            tokens.push(XmlToken::Comment(rest[..end].to_string()));
            rest = &rest[end..];
        } else if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>")? + 3;
            tokens.push(XmlToken::Cdata(rest[..end].to_string()));
            rest = &rest[end..];
        } else {
            let end = rest.find('>')? + 1;
            let tag = rest[..end].to_string();
            if tag.starts_with("<?") {
                tokens.push(XmlToken::ProcessingInstruction(tag));
            } else if tag.starts_with("<!") {
                tokens.push(XmlToken::Declaration(tag));
            } else if tag.starts_with("</") {
                tokens.push(XmlToken::Close(tag));
            } else if tag.ends_with("/>") {
                tokens.push(XmlToken::SelfClose(tag));
            } else {
                tokens.push(XmlToken::Open(tag));
            }
            rest = &rest[end..];
        }
    }
    Some(tokens)
}

fn render_xml(tokens: &[XmlToken]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut indent: usize = 0;
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            XmlToken::ProcessingInstruction(tag) => {
                // Mirrors OMIT_XML_DECLARATION=yes from the Java transformer.
                if !tag.to_lowercase().starts_with("<?xml") {
                    lines.push(format!("{}{}", "  ".repeat(indent), tag));
                }
                i += 1;
            }
            XmlToken::Declaration(tag)
            | XmlToken::Comment(tag)
            | XmlToken::Cdata(tag)
            | XmlToken::SelfClose(tag)
            | XmlToken::Text(tag) => {
                lines.push(format!("{}{}", "  ".repeat(indent), tag));
                i += 1;
            }
            XmlToken::Close(tag) => {
                indent = indent.saturating_sub(1);
                lines.push(format!("{}{}", "  ".repeat(indent), tag));
                i += 1;
            }
            XmlToken::Open(tag) => {
                if i + 2 < tokens.len() {
                    if let (XmlToken::Text(text), XmlToken::Close(close)) =
                        (&tokens[i + 1], &tokens[i + 2])
                    {
                        lines.push(format!("{}{}{}{}", "  ".repeat(indent), tag, text, close));
                        i += 3;
                        continue;
                    }
                }
                lines.push(format!("{}{}", "  ".repeat(indent), tag));
                indent += 1;
                i += 1;
            }
        }
    }
    lines.join("\n")
}

fn format_yaml(text: &str) -> String {
    let mut out = String::new();
    let mut prev_indent: i32 = -1;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        let mut spaces: i32 = 0;
        for ch in line.chars() {
            if ch == ' ' || ch == '\t' {
                spaces += 1;
            } else {
                break;
            }
        }
        let mut indent = spaces;
        if indent % 2 != 0 {
            indent = (indent / 2 + 1) * 2;
        }
        indent = indent.min(prev_indent + 2);
        if !trimmed.starts_with('-')
            && !trimmed.starts_with('#')
            && prev_indent >= 0
            && indent > prev_indent + 2
        {
            indent = prev_indent + 2;
        }
        prev_indent = indent;
        push_spaces(&mut out, indent);
        out.push_str(trimmed);
        out.push('\n');
    }
    out.trim().to_string()
}

fn format_toml(text: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !first && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        if trimmed.starts_with('[') {
            if !first {
                out.push('\n');
            }
            out.push_str(trimmed);
            out.push('\n');
            first = false;
            continue;
        }
        if trimmed.starts_with('#') {
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        match trimmed.find('=') {
            Some(eq) if eq > 0 => {
                let key = trimmed[..eq].trim();
                let value = trimmed[eq + 1..].trim();
                out.push_str(key);
                out.push_str(" = ");
                out.push_str(value);
                out.push('\n');
            }
            _ => {
                out.push_str(trimmed);
                out.push('\n');
            }
        }
        first = false;
    }
    out.trim().to_string()
}

fn format_ini(text: &str) -> String {
    let mut out = String::new();
    let mut need_newline = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            need_newline = true;
            continue;
        }
        if need_newline && !out.is_empty() {
            out.push('\n');
        }
        need_newline = false;
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        match trimmed.find('=') {
            Some(eq) if eq > 0 => {
                let key = trimmed[..eq].trim();
                let value = trimmed[eq + 1..].trim();
                out.push_str(key);
                out.push_str(" = ");
                out.push_str(value);
                out.push('\n');
            }
            _ => {
                out.push_str(trimmed);
                out.push('\n');
            }
        }
    }
    out.trim().to_string()
}

fn normalize_indent(text: &str, indent_size: usize) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut col = 0;
        while col < chars.len() && (chars[col] == ' ' || chars[col] == '\t') {
            col += 1;
        }
        let mut indent = 0usize;
        for ch in chars.iter().take(col) {
            indent += if *ch == '\t' { indent_size } else { 1 };
        }
        indent = (indent / indent_size) * indent_size;
        for _ in 0..indent {
            out.push(' ');
        }
        out.extend(chars[col..].iter());
        if index < lines.len() - 1 {
            out.push('\n');
        }
    }
    out
}

// --- Process execution -------------------------------------------------------

async fn run_stdin_formatter(args: &[String], input: &str) -> Result<String> {
    let (program, rest) = args
        .split_first()
        .ok_or_else(|| anyhow!("Formatter command line is empty"))?;
    let mut child = tokio::process::Command::new(program)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| anyhow!("Could not start formatter: {program}: {error}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Could not open formatter stdin: {program}"))?;
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Could not open formatter stdout: {program}"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Could not open formatter stderr: {program}"))?;

    let stdout_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        stdout_pipe.read_to_end(&mut buffer).await.map(|_| buffer)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        stderr_pipe.read_to_end(&mut buffer).await.map(|_| buffer)
    });
    let payload = input.as_bytes().to_vec();
    let stdin_task = tokio::spawn(async move {
        let _ = stdin.write_all(&payload).await;
        let _ = stdin.shutdown().await;
    });

    let status = wait_with_timeout(&mut child).await?;
    let _ = stdin_task.await;
    let stdout = stdout_task
        .await
        .map_err(|error| anyhow!("Could not read formatter output: {error}"))?
        .map_err(|error| anyhow!("Could not read formatter output: {error}"))?;
    let stderr = stderr_task
        .await
        .unwrap_or(Ok(Vec::new()))
        .unwrap_or_default();
    ensure_successful_exit(&status, &stderr)?;
    Ok(String::from_utf8_lossy(&stdout).to_string())
}

async fn run_file_formatter(
    args: &[String],
    input: &str,
    extension: Option<&str>,
) -> Result<String> {
    let extension = extension.unwrap_or(".txt");
    let temp_file = std::env::temp_dir().join(format!(
        "kortty_format_{}{}",
        uuid::Uuid::new_v4().simple(),
        extension
    ));
    tokio::fs::write(&temp_file, input)
        .await
        .map_err(|error| anyhow!("Could not write formatter temp file: {error}"))?;
    let outcome = run_file_formatter_process(args, &temp_file).await;
    let result = match outcome {
        Ok(()) => tokio::fs::read_to_string(&temp_file)
            .await
            .map_err(|error| anyhow!("Could not read formatter temp file: {error}")),
        Err(error) => Err(error),
    };
    let _ = tokio::fs::remove_file(&temp_file).await;
    result
}

async fn run_file_formatter_process(args: &[String], temp_file: &Path) -> Result<()> {
    let (program, rest) = args
        .split_first()
        .ok_or_else(|| anyhow!("Formatter command line is empty"))?;
    let mut child = tokio::process::Command::new(program)
        .args(rest)
        .arg(temp_file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| anyhow!("Could not run file formatter: {program}: {error}"))?;

    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Could not open formatter stdout: {program}"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Could not open formatter stderr: {program}"))?;
    let stdout_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buffer).await;
    });
    let stderr_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        stderr_pipe.read_to_end(&mut buffer).await.map(|_| buffer)
    });

    let status = wait_with_timeout(&mut child).await?;
    let _ = stdout_task.await;
    let stderr = stderr_task
        .await
        .unwrap_or(Ok(Vec::new()))
        .unwrap_or_default();
    ensure_successful_exit(&status, &stderr)
}

async fn wait_with_timeout(child: &mut tokio::process::Child) -> Result<std::process::ExitStatus> {
    match tokio::time::timeout(Duration::from_secs(FORMATTER_TIMEOUT_SECONDS), child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            Err(anyhow!("Formatter failed: {error}"))
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(anyhow!(
                "Formatter timed out after {FORMATTER_TIMEOUT_SECONDS} seconds"
            ))
        }
    }
}

fn ensure_successful_exit(status: &std::process::ExitStatus, stderr: &[u8]) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        bail!(
            "Formatter failed with exit code {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }
    bail!("{}", stderr_excerpt(&message));
}

fn stderr_excerpt(message: &str) -> String {
    if message.chars().count() <= MAX_STDERR_EXCERPT_CHARS {
        return message.to_string();
    }
    let mut excerpt: String = message.chars().take(MAX_STDERR_EXCERPT_CHARS).collect();
    excerpt.push_str(" …");
    excerpt
}

// --- Bundled formatter discovery ----------------------------------------------

/// Roots searched for bundled formatter runtimes, in priority order:
/// `KORTTY_FORMATTERS_DIR`, the executable directory, the Tauri resource
/// directory layouts next to it, and the working directory.
pub fn formatter_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(configured) = std::env::var_os("KORTTY_FORMATTERS_DIR") {
        let value = PathBuf::from(configured);
        if !value.as_os_str().is_empty() {
            roots.push(value);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            roots.push(exe_dir.join("formatters"));
            if let Some(parent) = exe_dir.parent() {
                // macOS bundle: Contents/MacOS/<exe> -> Contents/Resources (Tauri resource dir).
                roots.push(parent.join("Resources").join("formatters"));
                // Linux deb/rpm: /usr/bin/<exe> -> /usr/lib/KorTTY (Tauri resource dir).
                roots.push(parent.join("lib").join("KorTTY").join("formatters"));
                roots.push(parent.join("formatters"));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("formatters"));
    }
    let mut seen = std::collections::HashSet::new();
    roots
        .into_iter()
        .filter(|root| seen.insert(root.clone()))
        .collect()
}

fn find_bundled_node(roots: &[PathBuf]) -> Option<PathBuf> {
    let node_executable = executable_name("node");
    for root in roots {
        let candidates = [
            root.join("node").join("bin").join(&node_executable),
            root.join("node").join(&node_executable),
        ];
        for candidate in candidates {
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_bundled_executable(
    roots: &[PathBuf],
    formatter_dir: &str,
    executable: &str,
) -> Option<PathBuf> {
    for root in roots {
        let candidates = [
            root.join("bin").join(executable),
            root.join(formatter_dir).join(executable),
            root.join(formatter_dir).join("bin").join(executable),
        ];
        for candidate in candidates {
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_bundled_file(roots: &[PathBuf], formatter_dir: &str, relative: &Path) -> Option<PathBuf> {
    for root in roots {
        let candidate = root.join(formatter_dir).join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn find_command_on_path(command: &str) -> Option<PathBuf> {
    if command.trim().is_empty() {
        return None;
    }
    if command.contains(std::path::MAIN_SEPARATOR) || command.contains('/') {
        let path = PathBuf::from(command);
        return if is_executable(&path) {
            Some(path)
        } else {
            None
        };
    }
    let path_var = std::env::var_os("PATH")?;
    let suffixes = executable_suffixes();
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for suffix in &suffixes {
            let candidate = dir.join(format!("{command}{suffix}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_suffixes() -> Vec<String> {
    match std::env::var("PATHEXT") {
        Ok(pathext) if !pathext.trim().is_empty() => {
            let separator = if cfg!(windows) { ';' } else { ':' };
            let mut suffixes = vec![String::new()];
            suffixes.extend(
                pathext
                    .split(separator)
                    .filter(|part| !part.trim().is_empty())
                    .map(|part| part.to_lowercase()),
            );
            suffixes
        }
        _ => vec![String::new()],
    }
}

fn executable_name(base_name: &str) -> String {
    if cfg!(windows) {
        format!("{base_name}.exe")
    } else {
        base_name.to_string()
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_json_with_built_in_formatter() {
        let formatted = format_built_in("{\"name\":\"demo\",\"items\":[1,2]}", "json").unwrap();
        assert_eq!(
            formatted,
            "{\n  \"name\": \"demo\",\n  \"items\": [\n    1,\n    2\n  ]\n}"
        );
    }

    #[test]
    fn json_formatter_preserves_key_order_and_string_content() {
        let formatted =
            format_built_in("{\"z\":\"a {b} [c]\",\"a\":{\"x\":true}}", "json").unwrap();
        assert_eq!(
            formatted,
            "{\n  \"z\": \"a {b} [c]\",\n  \"a\": {\n    \"x\": true\n  }\n}"
        );
    }

    #[test]
    fn formats_xml_with_two_space_indent_and_inline_text() {
        let formatted = format_built_in(
            "<?xml version=\"1.0\"?><a><b>text</b><c attr=\"1\"/></a>",
            "xml",
        )
        .unwrap();
        assert_eq!(formatted, "<a>\n  <b>text</b>\n  <c attr=\"1\"/>\n</a>");
    }

    #[test]
    fn xml_formatter_rejects_non_markup_content() {
        assert!(format_built_in("not xml at all", "xml").is_none());
        assert!(format_built_in("<unterminated", "xml").is_none());
    }

    #[test]
    fn normalizes_yaml_indentation_to_two_spaces() {
        let formatted = format_built_in("root:\n    child: 1\n  other: 2", "yaml").unwrap();
        assert_eq!(formatted, "root:\n  child: 1\n  other: 2");
    }

    #[test]
    fn normalizes_toml_sections_and_key_value_spacing() {
        let formatted =
            format_built_in("[section]\nkey=value\n\n[other]\nfoo =  bar", "toml").unwrap();
        assert_eq!(formatted, "[section]\nkey = value\n\n[other]\nfoo = bar");
    }

    #[test]
    fn normalizes_ini_key_value_spacing_and_keeps_comments() {
        let formatted =
            format_built_in("[sec]\nname=value\n; comment\n\nbare", "properties").unwrap();
        assert_eq!(formatted, "[sec]\nname = value\n; comment\n\nbare");
    }

    #[test]
    fn normalizes_groovy_indent_to_four_spaces() {
        let formatted =
            format_built_in("def x() {\n\tprintln 1\n   println 2\n}", "groovy").unwrap();
        assert_eq!(formatted, "def x() {\n    println 1\nprintln 2\n}");
    }

    #[test]
    fn built_in_formatter_is_always_available() {
        let formatter = get_formatter_info("json").unwrap();
        assert_eq!(formatter.provider_type, FormatterProviderType::BuiltIn);
        assert!(formatter.available);
        assert_eq!(formatter.display_name, "integrated");
        assert!(is_formatter_available(Some(&formatter)));
    }

    #[test]
    fn reports_unsupported_plain_text() {
        assert!(!is_supported("plain"));
        assert!(get_formatter_info("plain").is_none());
        assert!(get_formatter_info("").is_none());
    }

    #[test]
    fn exposes_local_shell_formatter_configuration() {
        let formatter = get_formatter_info("bash").unwrap();
        assert_eq!(formatter.command_name.as_deref(), Some("shfmt"));
        assert_eq!(formatter.file_extension.as_deref(), Some(".sh"));
        assert!(matches!(
            formatter.provider_type,
            FormatterProviderType::Bundled
                | FormatterProviderType::ExternalFallback
                | FormatterProviderType::Unavailable
        ));
    }

    #[test]
    fn normalize_line_width_clamps_into_supported_range() {
        assert_eq!(normalize_line_width(None), None);
        assert_eq!(normalize_line_width(Some(10)), Some(MIN_LINE_WIDTH));
        assert_eq!(normalize_line_width(Some(20)), Some(20));
        assert_eq!(normalize_line_width(Some(80)), Some(80));
        assert_eq!(normalize_line_width(Some(240)), Some(240));
        assert_eq!(normalize_line_width(Some(999)), Some(MAX_LINE_WIDTH));
    }

    #[test]
    fn exposes_line_width_support_only_for_formatters_with_native_width_options() {
        assert!(supports_line_width("javascript"));
        assert!(supports_line_width("html"));
        assert!(supports_line_width("python"));
        assert!(supports_line_width("perl"));
        assert!(!supports_line_width("java"));
        assert!(!supports_line_width("json"));
        assert!(!supports_line_width("plain"));
    }

    #[test]
    fn command_line_with_line_width_extends_supported_formatters() {
        let prettier = FormatterInfo {
            command_line: vec!["prettier".into(), "--parser".into(), "typescript".into()],
            ..built_in("javascript", "prettier", ".js")
        };
        assert_eq!(
            command_line_with_line_width(&prettier, Some(72)),
            vec!["prettier", "--parser", "typescript", "--print-width", "72"]
        );

        let black = FormatterInfo {
            command_line: vec!["black".into(), "-q".into(), "-".into()],
            ..built_in("python", "black", ".py")
        };
        assert_eq!(
            command_line_with_line_width(&black, Some(100)),
            vec!["black", "-q", "--line-length", "100", "-"]
        );

        let perltidy = FormatterInfo {
            command_line: vec!["perltidy".into(), "-st".into()],
            ..built_in("perl", "perltidy", ".pl")
        };
        assert_eq!(
            command_line_with_line_width(&perltidy, Some(120)),
            vec!["perltidy", "-st", "-l=120"]
        );

        assert_eq!(
            command_line_with_line_width(&prettier, None),
            vec!["prettier", "--parser", "typescript"]
        );
    }

    #[test]
    fn parses_formatter_manifest_key_value_pairs() {
        let parsed = parse_formatter_manifest(
            "# comment\n! also comment\n\nkey=value\nspaced = value two\nnokey\n=empty\n",
        );
        assert_eq!(parsed.get("key").map(String::as_str), Some("value"));
        assert_eq!(parsed.get("spaced").map(String::as_str), Some("value two"));
        assert!(!parsed.contains_key("nokey"));
        assert!(!parsed.contains_key(""));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn embedded_manifest_contains_bundled_formatter_versions() {
        let manifest = formatter_manifest();
        assert_eq!(
            manifest.get("shfmt.version").map(String::as_str),
            Some(SHFMT_VERSION)
        );
        assert_eq!(
            manifest.get("prettier.version").map(String::as_str),
            Some(PRETTIER_VERSION)
        );
        assert_eq!(
            manifest.get("sql-formatter.version").map(String::as_str),
            Some(SQL_FORMATTER_VERSION)
        );
        assert_eq!(
            manifest.get("perltidy.version").map(String::as_str),
            Some(PERL_TIDY_VERSION)
        );
        assert_eq!(
            manifest
                .get("google-java-format.version")
                .map(String::as_str),
            Some(GOOGLE_JAVA_FORMAT_VERSION)
        );
    }

    #[tokio::test]
    async fn rejects_line_width_for_formatter_without_native_width_option() {
        let error = format("json", "{\"a\":1}", Some(80)).await.unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }

    #[tokio::test]
    async fn rejects_blank_content() {
        let error = format("json", "   \n", None).await.unwrap_err();
        assert!(error.to_string().contains("no content"));
    }

    #[tokio::test]
    async fn formats_json_via_public_format_entry_point() {
        let formatted = format("json", "{\"name\":\"demo\",\"items\":[1,2]}", None)
            .await
            .unwrap();
        assert_eq!(
            formatted,
            "{\n  \"name\": \"demo\",\n  \"items\": [\n    1,\n    2\n  ]\n}"
        );
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, content: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kortty-formatters-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prefers_bundled_formatter_over_external_fallback() {
        let root = temp_root("shfmt");
        write_executable(&root.join("bin/shfmt"), "#!/bin/sh\nsed 's/foo/bar/g'\n");
        let roots = vec![root.clone()];

        let formatter = get_formatter_info_with_roots("bash", &roots).unwrap();
        assert_eq!(formatter.provider_type, FormatterProviderType::Bundled);
        assert!(formatter.display_name.contains("bundled"));

        let formatted = format_with_roots("bash", "echo foo\n", None, &roots)
            .await
            .unwrap();
        assert_eq!(formatted, "echo bar\n");

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn detects_bundled_node_based_formatters() {
        let root = temp_root("node");
        write_executable(&root.join("node/bin/node"), "#!/bin/sh\nexit 0\n");
        std::fs::create_dir_all(root.join("prettier/bin")).unwrap();
        std::fs::write(root.join("prettier/bin/prettier.cjs"), "").unwrap();
        std::fs::create_dir_all(root.join("sql-formatter")).unwrap();
        std::fs::write(root.join("sql-formatter/kortty-sql-formatter.cjs"), "").unwrap();
        let roots = vec![root.clone()];

        let javascript = get_formatter_info_with_roots("javascript", &roots).unwrap();
        let sql = get_formatter_info_with_roots("sql", &roots).unwrap();
        assert_eq!(javascript.provider_type, FormatterProviderType::Bundled);
        assert_eq!(sql.provider_type, FormatterProviderType::Bundled);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn line_width_formatting_adds_prettier_print_width() {
        let root = temp_root("prettier-width");
        let args_file = root.join("args.txt");
        write_executable(
            &root.join("node/bin/node"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat\n",
                args_file.display()
            ),
        );
        std::fs::create_dir_all(root.join("prettier/bin")).unwrap();
        std::fs::write(root.join("prettier/bin/prettier.cjs"), "").unwrap();
        let roots = vec![root.clone()];

        let formatted =
            format_with_roots("javascript", "const name = 'korTTY';\n", Some(72), &roots)
                .await
                .unwrap();
        assert_eq!(formatted, "const name = 'korTTY';\n");
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.contains("--parser\ntypescript"));
        assert!(args.contains("--print-width\n72"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_append_formatter_rewrites_temp_file_in_place() {
        let root = temp_root("file-append");
        let script = root.join("bin/fake-fmt");
        write_executable(&script, "#!/bin/sh\nprintf 'formatted\\n' > \"$1\"\n");

        let formatted = run_file_formatter(
            &[script.to_string_lossy().to_string()],
            "original\n",
            Some(".txt"),
        )
        .await
        .unwrap();
        assert_eq!(formatted, "formatted\n");

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_formatter_reports_stderr_excerpt_on_failure() {
        let root = temp_root("stderr");
        let script = root.join("bin/fail-fmt");
        write_executable(&script, "#!/bin/sh\necho 'boom: bad input' >&2\nexit 3\n");

        let error = run_stdin_formatter(&[script.to_string_lossy().to_string()], "x\n")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("boom: bad input"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn formatter_info_serializes_with_camel_case_fields() {
        let info = get_formatter_info("json").unwrap();
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["language"], "json");
        assert_eq!(json["formatterId"], "json");
        assert_eq!(json["displayName"], "integrated");
        assert_eq!(json["providerType"], "builtIn");
        assert_eq!(json["fileExtension"], ".json");
        assert_eq!(json["available"], true);
        assert!(json["executionMode"].is_null());
    }
}
