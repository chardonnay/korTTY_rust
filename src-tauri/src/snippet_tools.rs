use crate::model::snippet::Snippet;
use anyhow::{bail, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetFormatRequest {
    pub content: String,
    pub language: Option<String>,
    #[serde(default)]
    pub formatter_command: Option<String>,
    #[serde(default)]
    pub formatter_args: Vec<String>,
    #[serde(default)]
    pub max_line_length: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetFormatResult {
    pub content: String,
    pub formatter_name: String,
    pub used_external_formatter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SnippetExportFormat {
    ScriptFiles,
    Zip,
    AesZip,
    GpgZip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetExportRequest {
    pub snippets: Vec<Snippet>,
    pub target_path: String,
    pub format: SnippetExportFormat,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub gpg_recipient: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetExportResult {
    pub target_path: String,
    pub exported_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetPlantUmlRenderRequest {
    pub source: String,
    pub output_path: String,
    #[serde(default = "default_plantuml_format")]
    pub output_format: String,
    #[serde(default)]
    pub plantuml_jar_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetPlantUmlRenderResult {
    pub output_path: String,
    pub content_hash: String,
    pub tool: String,
}

/// WP2.10: in-memory SVG render request used by the snippet diagram dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetPlantUmlSvgRequest {
    pub source: String,
    #[serde(default)]
    pub background_color: Option<String>,
    #[serde(default)]
    pub plantuml_jar_path: Option<String>,
}

/// WP2.10: in-memory SVG render result. `content_hash` is the SHA-256 of the
/// normalized PlantUML source (without the injected background color line).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetPlantUmlSvgResult {
    pub svg: String,
    pub content_hash: String,
    pub tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetToolAvailability {
    pub plantuml: bool,
    pub java: bool,
    pub graphviz_dot: bool,
    pub gpg: bool,
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

pub async fn format_snippet_code(request: &SnippetFormatRequest) -> Result<SnippetFormatResult> {
    // Editor-profile formatter overrides always take precedence.
    if let Some(command) = request
        .formatter_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let content =
            run_external_formatter(command, &request.formatter_args, &request.content).await?;
        return Ok(SnippetFormatResult {
            content,
            formatter_name: command.to_string(),
            used_external_formatter: true,
        });
    }

    if request.content.trim().is_empty() {
        return Ok(SnippetFormatResult {
            content: request.content.clone(),
            formatter_name: "integrated".into(),
            used_external_formatter: false,
        });
    }

    let language = request
        .language
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if let Some(info) = crate::code_formatter::get_formatter_info(&language) {
        let content =
            crate::code_formatter::format(&language, &request.content, request.max_line_length)
                .await?;
        let used_external_formatter =
            info.provider_type != crate::code_formatter::FormatterProviderType::BuiltIn;
        return Ok(SnippetFormatResult {
            content,
            formatter_name: info.display_name,
            used_external_formatter,
        });
    }

    Ok(SnippetFormatResult {
        content: trim_trailing_whitespace(&request.content),
        formatter_name: "built-in whitespace".into(),
        used_external_formatter: false,
    })
}

pub fn export_snippets(request: &SnippetExportRequest) -> Result<SnippetExportResult> {
    if request.snippets.is_empty() {
        bail!("No snippets selected for export");
    }
    let target = PathBuf::from(&request.target_path);
    match request.format {
        SnippetExportFormat::ScriptFiles => write_script_files(&request.snippets, &target)?,
        SnippetExportFormat::Zip => write_zip(&request.snippets, &target, None)?,
        SnippetExportFormat::AesZip => {
            let password = request
                .password
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("AES ZIP export requires a password"))?;
            write_zip(&request.snippets, &target, Some(password))?;
        }
        SnippetExportFormat::GpgZip => {
            let recipient = request
                .gpg_recipient
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("GPG ZIP export requires a recipient"))?;
            export_gpg_zip(&request.snippets, &target, recipient)?;
        }
    }
    Ok(SnippetExportResult {
        target_path: target.to_string_lossy().to_string(),
        exported_count: request.snippets.len(),
    })
}

/// Removes a temporary directory on drop so PlantUML scratch directories are
/// cleaned up even when rendering fails on an early `?` return.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn render_plantuml(
    request: &SnippetPlantUmlRenderRequest,
) -> Result<SnippetPlantUmlRenderResult> {
    let source = normalize_plantuml_source(&request.source);
    let output_path = PathBuf::from(&request.output_path);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let format = normalize_plantuml_format(&request.output_format)?;
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("snippet-diagram");
    let temp_dir =
        std::env::temp_dir().join(format!("kortty-plantuml-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&temp_dir)?;
    let _temp_guard = TempDirGuard(temp_dir.clone());
    let source_path = temp_dir.join(format!("{stem}.puml"));
    fs::write(&source_path, &source)?;
    let tool = if let Some(jar_path) = request
        .plantuml_jar_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        run_plantuml_jar(jar_path, &source_path, &temp_dir, format)?
    } else {
        run_plantuml_binary(&source_path, &temp_dir, format)?
    };
    let generated = temp_dir.join(format!("{stem}.{format}"));
    if !generated.exists() {
        bail!("PlantUML did not create the expected {} output", format);
    }
    fs::copy(&generated, &output_path)?;
    Ok(SnippetPlantUmlRenderResult {
        output_path: output_path.to_string_lossy().to_string(),
        content_hash: content_hash(&source),
        tool,
    })
}

/// Renders PlantUML to an in-memory SVG string for the snippet diagram dialog
/// (WP2.10, variant of `render_plantuml` that never touches a user-chosen
/// output path). The optional background color is injected as a
/// `skinparam backgroundColor` line before rendering; the returned content
/// hash is computed over the normalized source *without* that line so the
/// hash stays stable across background changes.
pub fn render_plantuml_svg_string(
    request: &SnippetPlantUmlSvgRequest,
) -> Result<SnippetPlantUmlSvgResult> {
    let source = normalize_plantuml_source(&request.source);
    let render_source = match request
        .background_color
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(color) => apply_plantuml_background_color(&source, color),
        None => source.clone(),
    };
    let temp_dir = std::env::temp_dir().join(format!(
        "kortty-plantuml-svg-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&temp_dir)?;
    let _temp_guard = TempDirGuard(temp_dir.clone());
    let source_path = temp_dir.join("snippet-diagram.puml");
    fs::write(&source_path, &render_source)?;
    let tool = if let Some(jar_path) = request
        .plantuml_jar_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        run_plantuml_jar(jar_path, &source_path, &temp_dir, "svg")?
    } else {
        run_plantuml_binary(&source_path, &temp_dir, "svg")?
    };
    let generated = temp_dir.join("snippet-diagram.svg");
    if !generated.exists() {
        bail!("PlantUML did not create the expected svg output");
    }
    let svg = fs::read_to_string(&generated)?;
    Ok(SnippetPlantUmlSvgResult {
        svg,
        content_hash: content_hash(&source),
        tool,
    })
}

/// Inserts (or replaces) a `skinparam backgroundColor` line after `@startuml`.
/// Port of the background half of Java `SnippetDiagramSupport.applyBackgroundColor`.
fn apply_plantuml_background_color(normalized_source: &str, color: &str) -> String {
    let color = normalize_hex_color(color, "#FFFFFF");
    let background_line = format!("skinparam backgroundColor {color}");
    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in normalized_source.lines() {
        let trimmed = line.trim();
        if !replaced
            && trimmed.len() > "skinparam backgroundColor".len()
            && trimmed
                .to_lowercase()
                .starts_with("skinparam backgroundcolor")
        {
            lines.push(background_line.clone());
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        let insert_at = lines
            .iter()
            .position(|line| line.trim_start().starts_with("@start"))
            .map(|index| index + 1)
            .unwrap_or(0);
        lines.insert(insert_at, background_line);
    }
    let mut result = lines.join("\n");
    if normalized_source.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Port of Java `SnippetDiagramSupport.normalizeHexColor`.
fn normalize_hex_color(color: &str, fallback: &str) -> String {
    fn is_hex_color(value: &str) -> bool {
        value.len() == 7
            && value.starts_with('#')
            && value[1..].chars().all(|ch| ch.is_ascii_hexdigit())
    }
    let value = color.trim();
    if is_hex_color(value) {
        return value.to_uppercase();
    }
    let fallback_value = fallback.trim();
    if is_hex_color(fallback_value) {
        return fallback_value.to_uppercase();
    }
    "#FFFFFF".to_string()
}

pub fn check_snippet_tools() -> SnippetToolAvailability {
    SnippetToolAvailability {
        plantuml: find_tool("plantuml").is_some(),
        java: find_tool("java").is_some(),
        graphviz_dot: find_tool("dot").is_some(),
        gpg: find_tool("gpg").is_some(),
    }
}

pub fn build_plantuml_preview_source(snippet: &Snippet) -> String {
    let title = if snippet.name.trim().is_empty() {
        "Snippet"
    } else {
        snippet.name.trim()
    };
    let body = snippet.content.trim();
    format!(
        "@startuml\nskinparam monochrome true\ntitle {}\nrectangle \"{}\" as snippet\nnote right of snippet\n{}\nend note\n@enduml\n",
        plantuml_label(title),
        plantuml_label(snippet.language.as_deref().unwrap_or("script")),
        body.lines()
            .take(30)
            .map(plantuml_note_line)
            .collect::<Vec<_>>()
            .join("\n")
    )
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

/// Maximum wall-clock time an editor-profile formatter override may run before
/// it is killed. Matches the shared code-formatter timeout.
const EXTERNAL_FORMATTER_TIMEOUT_SECONDS: u64 = 15;

/// Runs an editor-profile formatter override, feeding `content` on stdin and
/// returning its stdout. stdin is written concurrently with stdout/stderr
/// draining (so a formatter that emits output before consuming all of its
/// input cannot deadlock), and the whole process is bounded by a timeout so a
/// hung formatter cannot block indefinitely.
async fn run_external_formatter(command: &str, args: &[String], content: &str) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut child = tokio::process::Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Could not open formatter stdin"))?;
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Could not open formatter stdout"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Could not open formatter stderr"))?;

    let stdout_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        stdout_pipe.read_to_end(&mut buffer).await.map(|_| buffer)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        stderr_pipe.read_to_end(&mut buffer).await.map(|_| buffer)
    });
    let payload = content.as_bytes().to_vec();
    let stdin_task = tokio::spawn(async move {
        let _ = stdin.write_all(&payload).await;
        let _ = stdin.shutdown().await;
    });

    let status = match tokio::time::timeout(
        std::time::Duration::from_secs(EXTERNAL_FORMATTER_TIMEOUT_SECONDS),
        child.wait(),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            let _ = child.kill().await;
            bail!("Formatter timed out after {EXTERNAL_FORMATTER_TIMEOUT_SECONDS} seconds");
        }
    };
    let _ = stdin_task.await;
    let stdout = stdout_task
        .await
        .map_err(|error| anyhow::anyhow!("Could not read formatter output: {error}"))??;
    let stderr = stderr_task
        .await
        .unwrap_or(Ok(Vec::new()))
        .unwrap_or_default();
    if !status.success() {
        bail!(
            "Formatter failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    Ok(String::from_utf8(stdout)?)
}

fn trim_trailing_whitespace(content: &str) -> String {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_script_files(snippets: &[Snippet], target_dir: &Path) -> Result<()> {
    fs::create_dir_all(target_dir)?;
    let mut used_names = Vec::<String>::new();
    for snippet in snippets {
        let file_name = unique_file_name(snippet, &mut used_names);
        fs::write(target_dir.join(file_name), &snippet.content)?;
    }
    Ok(())
}

fn write_zip(snippets: &[Snippet], target: &Path, password: Option<&str>) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(target)?;
    let mut zip = zip::ZipWriter::new(file);
    let base_options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut used_names = Vec::<String>::new();
    for snippet in snippets {
        let options = if let Some(password) = password {
            base_options.with_aes_encryption(zip::AesMode::Aes256, password)
        } else {
            base_options
        };
        let file_name = unique_file_name(snippet, &mut used_names);
        zip.start_file(file_name, options)?;
        zip.write_all(snippet.content.as_bytes())?;
    }
    zip.finish()?;
    Ok(())
}

fn export_gpg_zip(snippets: &[Snippet], target: &Path, recipient: &str) -> Result<()> {
    let gpg = find_tool("gpg").ok_or_else(|| anyhow::anyhow!("gpg is not available"))?;
    let temp = std::env::temp_dir().join(format!(
        "kortty-snippets-{}.zip",
        uuid::Uuid::new_v4().simple()
    ));
    write_zip(snippets, &temp, None)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let output = Command::new(gpg)
        .arg("--batch")
        .arg("--yes")
        .arg("--output")
        .arg(target)
        .arg("--encrypt")
        .arg("--recipient")
        .arg(recipient)
        .arg(&temp)
        .output()?;
    let _ = fs::remove_file(&temp);
    if !output.status.success() {
        bail!(
            "gpg export failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn unique_file_name(snippet: &Snippet, used_names: &mut Vec<String>) -> String {
    let base = sanitize_file_name(if snippet.name.trim().is_empty() {
        &snippet.id
    } else {
        &snippet.name
    });
    let extension = extension_for_language(snippet.language.as_deref());
    let mut candidate = format!("{base}.{extension}");
    let mut index = 2;
    while used_names.iter().any(|name| name == &candidate) {
        candidate = format!("{base}-{index}.{extension}");
        index += 1;
    }
    used_names.push(candidate.clone());
    candidate
}

fn sanitize_file_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "snippet".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

fn extension_for_language(language: Option<&str>) -> &'static str {
    match language.unwrap_or_default().trim().to_lowercase().as_str() {
        "bash" | "shell" | "sh" | "zsh" => "sh",
        "python" => "py",
        "javascript" => "js",
        "typescript" => "ts",
        "rust" => "rs",
        "java" => "java",
        "perl" => "pl",
        "ruby" => "rb",
        "php" => "php",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "xml" => "xml",
        "sql" => "sql",
        "markdown" => "md",
        "html" => "html",
        "css" => "css",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        _ => "txt",
    }
}

fn normalize_plantuml_source(source: &str) -> String {
    let trimmed = source.trim();
    if trimmed.starts_with("@start") {
        format!("{trimmed}\n")
    } else {
        format!("@startuml\n{trimmed}\n@enduml\n")
    }
}

fn normalize_plantuml_format(format: &str) -> Result<&'static str> {
    match format.trim().to_lowercase().as_str() {
        "" | "png" => Ok("png"),
        "svg" => Ok("svg"),
        other => bail!("Unsupported PlantUML output format: {other}"),
    }
}

fn run_plantuml_binary(source_path: &Path, output_dir: &Path, format: &str) -> Result<String> {
    let plantuml =
        find_tool("plantuml").ok_or_else(|| anyhow::anyhow!("plantuml is not available"))?;
    let output = Command::new(&plantuml)
        .arg(format!("-t{format}"))
        .arg("-o")
        .arg(output_dir)
        .arg(source_path)
        .output()?;
    if !output.status.success() {
        bail!(
            "PlantUML failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(plantuml.to_string_lossy().to_string())
}

fn run_plantuml_jar(
    jar_path: &str,
    source_path: &Path,
    output_dir: &Path,
    format: &str,
) -> Result<String> {
    let java = find_tool("java").ok_or_else(|| anyhow::anyhow!("java is not available"))?;
    let output = Command::new(&java)
        .arg("-jar")
        .arg(jar_path)
        .arg(format!("-t{format}"))
        .arg("-o")
        .arg(output_dir)
        .arg(source_path)
        .output()?;
    if !output.status.success() {
        bail!(
            "PlantUML failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(format!("{} -jar {}", java.to_string_lossy(), jar_path))
}

fn find_tool(name: &str) -> Option<PathBuf> {
    // Reuse the PATHEXT-aware lookup from the code formatter so Windows tools
    // such as `plantuml.exe`/`java.exe`/`gpg.exe` are discovered as well.
    crate::code_formatter::find_command_on_path(name)
}

fn default_plantuml_format() -> String {
    "png".into()
}

fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn plantuml_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn plantuml_note_line(value: &str) -> String {
    value.replace('\\', "\\\\").replace("@end", "@ end")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

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

    #[tokio::test]
    async fn format_snippet_code_delegates_json_to_code_formatter() {
        let result = format_snippet_code(&SnippetFormatRequest {
            content: "{\"name\":\"demo\",\"items\":[1,2]}".into(),
            language: Some("json".into()),
            formatter_command: None,
            formatter_args: Vec::new(),
            max_line_length: None,
        })
        .await
        .unwrap();

        assert_eq!(
            result.content,
            "{\n  \"name\": \"demo\",\n  \"items\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(result.formatter_name, "integrated");
        assert!(!result.used_external_formatter);
    }

    #[tokio::test]
    async fn format_snippet_code_keeps_whitespace_fallback_for_unsupported_languages() {
        let result = format_snippet_code(&SnippetFormatRequest {
            content: "line one   \nline two\t\n".into(),
            language: Some("markdown".into()),
            formatter_command: None,
            formatter_args: Vec::new(),
            max_line_length: None,
        })
        .await
        .unwrap();

        assert_eq!(result.content, "line one\nline two");
        assert_eq!(result.formatter_name, "built-in whitespace");
        assert!(!result.used_external_formatter);
    }

    #[tokio::test]
    async fn format_snippet_code_returns_blank_content_unchanged() {
        let result = format_snippet_code(&SnippetFormatRequest {
            content: "   ".into(),
            language: Some("json".into()),
            formatter_command: None,
            formatter_args: Vec::new(),
            max_line_length: None,
        })
        .await
        .unwrap();

        assert_eq!(result.content, "   ");
        assert!(!result.used_external_formatter);
    }

    #[test]
    fn apply_plantuml_background_color_inserts_skinparam_after_start() {
        let source = "@startuml\nstart\nstop\n@enduml\n";
        let result = apply_plantuml_background_color(source, "#f4f8ff");
        assert!(result.contains("@startuml\nskinparam backgroundColor #F4F8FF\nstart"));
        assert!(result.trim_end().ends_with("@enduml"));
    }

    #[test]
    fn apply_plantuml_background_color_replaces_existing_skinparam() {
        let source = "@startuml\nskinparam backgroundColor #FFFFFF\nstart\nstop\n@enduml\n";
        let result = apply_plantuml_background_color(source, "#202020");
        assert!(result.contains("skinparam backgroundColor #202020"));
        assert!(!result.contains("skinparam backgroundColor #FFFFFF"));
    }

    #[test]
    fn normalize_hex_color_uppercases_and_falls_back() {
        assert_eq!(normalize_hex_color("#a1b2c3", "#FFFFFF"), "#A1B2C3");
        assert_eq!(normalize_hex_color("not-a-color", "#102030"), "#102030");
        assert_eq!(normalize_hex_color("", "also-bad"), "#FFFFFF");
    }

    #[test]
    fn svg_render_request_accepts_camel_case_payload() {
        let request: SnippetPlantUmlSvgRequest = serde_json::from_str(
            "{\"source\":\"@startuml\\nstart\\nstop\\n@enduml\",\"backgroundColor\":\"#FFFFFF\"}",
        )
        .unwrap();
        assert_eq!(request.background_color.as_deref(), Some("#FFFFFF"));
        assert!(request.plantuml_jar_path.is_none());
    }

    #[test]
    fn svg_render_result_serializes_camel_case() {
        let json = serde_json::to_string(&SnippetPlantUmlSvgResult {
            svg: "<svg/>".into(),
            content_hash: "abc".into(),
            tool: "plantuml".into(),
        })
        .unwrap();
        assert!(json.contains("\"contentHash\":\"abc\""));
        assert!(json.contains("\"svg\":\"<svg/>\""));
    }

    #[test]
    fn zip_export_writes_each_snippet_as_separate_script_file() -> Result<()> {
        let temp_dir = std::env::temp_dir().join(format!(
            "kortty-snippet-zip-export-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir)?;
        let target = temp_dir.join("snippets.zip");
        let request = SnippetExportRequest {
            snippets: vec![
                Snippet {
                    id: "one".into(),
                    name: "deploy".into(),
                    content: "echo deploy\n".into(),
                    language: Some("bash".into()),
                    ..Snippet::default()
                },
                Snippet {
                    id: "two".into(),
                    name: "cleanup".into(),
                    content: "echo cleanup\n".into(),
                    language: Some("bash".into()),
                    ..Snippet::default()
                },
            ],
            target_path: target.to_string_lossy().to_string(),
            format: SnippetExportFormat::Zip,
            password: None,
            gpg_recipient: None,
        };

        let result = export_snippets(&request)?;
        assert_eq!(result.exported_count, 2);

        let file = fs::File::open(&target)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let mut entries = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            entries.push((entry.name().to_string(), content));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));

        assert_eq!(
            entries,
            vec![
                ("cleanup.sh".to_string(), "echo cleanup\n".to_string()),
                ("deploy.sh".to_string(), "echo deploy\n".to_string()),
            ]
        );

        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }
}
