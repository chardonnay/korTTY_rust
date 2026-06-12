//! Executes AI prompts through a locally installed provider CLI.
//!
//! Port of the Java `LocalCliAiService`.

use crate::ai::cli_registry;
use crate::ai::cli_template::{self, AiCliArgumentTemplate};
use crate::ai::AiError;
use crate::model::ai::{AiExecutionResult, AiProfile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);
pub const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECTION_TEST_SYSTEM_PROMPT: &str = "Reply with exactly OK.";
const CONNECTION_TEST_USER_PROMPT: &str = "Connection test.";
const MISSING_MODEL_MESSAGE: &str =
    "AI model must be configured unless local LM Studio model selection is set to Auto.";
const STDERR_TRUNCATE_LIMIT: usize = 800;

struct CliCommand {
    command: Vec<String>,
    stdin: Option<String>,
}

struct CliProcessResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Executes one prompt through the configured local CLI provider.
pub async fn execute(
    profile: &AiProfile,
    system_prompt: &str,
    user_prompt: &str,
    timeout: Duration,
) -> Result<AiExecutionResult, AiError> {
    let temp_dir = std::env::temp_dir().join(format!("kortty-ai-cli-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|error| {
            AiError::Configuration(format!("AI CLI prompt files could not be created: {error}"))
        })?;
    let result =
        execute_with_temp_dir(profile, system_prompt, user_prompt, timeout, &temp_dir).await;
    // Temporary prompt files are best-effort cleanup.
    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    result
}

/// Runs the dedicated connection test prompt and reports whether the CLI
/// produced any output. Failures carry the CLI error message.
pub async fn test_connection(profile: &AiProfile) -> Result<bool, AiError> {
    let result = execute(
        profile,
        CONNECTION_TEST_SYSTEM_PROMPT,
        CONNECTION_TEST_USER_PROMPT,
        TEST_TIMEOUT,
    )
    .await
    .map_err(|error| AiError::Configuration(format!("AI CLI connection test failed: {error}")))?;
    Ok(!result.content.trim().is_empty())
}

async fn execute_with_temp_dir(
    profile: &AiProfile,
    system_prompt: &str,
    user_prompt: &str,
    timeout: Duration,
    temp_dir: &Path,
) -> Result<AiExecutionResult, AiError> {
    let prompt_file = temp_dir.join("prompt.txt");
    let system_prompt_file = temp_dir.join("system-prompt.txt");
    let user_prompt_file = temp_dir.join("user-prompt.txt");
    let combined_prompt = build_combined_prompt(system_prompt, user_prompt);
    write_prompt_file(&system_prompt_file, system_prompt).await?;
    write_prompt_file(&user_prompt_file, user_prompt).await?;
    write_prompt_file(&prompt_file, &combined_prompt).await?;

    let command = build_command(
        profile,
        &prompt_file,
        &system_prompt_file,
        &user_prompt_file,
        &combined_prompt,
    )?;
    let result = run_process(&command.command, command.stdin.as_deref(), timeout).await?;
    if result.exit_code != 0 {
        return Err(AiError::Configuration(build_exit_message(&result)));
    }
    Ok(AiExecutionResult {
        content: result.stdout.trim().to_string(),
        usage: None,
        usage_snapshot: None,
        active_profile_id: None,
        active_profile_name: None,
    })
}

async fn write_prompt_file(path: &PathBuf, content: &str) -> Result<(), AiError> {
    tokio::fs::write(path, content).await.map_err(|error| {
        AiError::Configuration(format!("AI CLI prompt files could not be created: {error}"))
    })
}

fn build_command(
    profile: &AiProfile,
    prompt_file: &Path,
    system_prompt_file: &Path,
    user_prompt_file: &Path,
    combined_prompt: &str,
) -> Result<CliCommand, AiError> {
    if profile
        .cli_provider_id
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Err(AiError::Configuration(
            "AI CLI provider must be configured.".to_string(),
        ));
    }
    let template_source = profile
        .cli_arguments_template
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    if template_source.is_empty() {
        return Err(AiError::Configuration(
            "AI CLI argument template must be configured.".to_string(),
        ));
    }
    let template = AiCliArgumentTemplate::parse(template_source).map_err(AiError::Configuration)?;
    if !template.contains_prompt_placeholder() {
        return Err(AiError::Configuration(
            "AI CLI argument template must include a prompt placeholder.".to_string(),
        ));
    }
    let model = profile.model.trim();
    if AiCliArgumentTemplate::requires_model(template_source) && model.is_empty() {
        return Err(AiError::Configuration(MISSING_MODEL_MESSAGE.to_string()));
    }
    let executable = resolve_executable(profile)?;
    let reasoning = profile.reasoning_effort.api_value().unwrap_or("");
    let mut values = HashMap::new();
    values.insert(cli_template::MODEL, model.to_string());
    values.insert(cli_template::REASONING, reasoning.to_string());
    values.insert(
        cli_template::PROMPT_FILE,
        prompt_file.to_string_lossy().into_owned(),
    );
    values.insert(
        cli_template::SYSTEM_PROMPT_FILE,
        system_prompt_file.to_string_lossy().into_owned(),
    );
    values.insert(
        cli_template::USER_PROMPT_FILE,
        user_prompt_file.to_string_lossy().into_owned(),
    );
    let expanded = template
        .expand_for_execution(&values)
        .map_err(AiError::Configuration)?;
    let mut command = Vec::with_capacity(expanded.arguments.len() + 1);
    command.push(executable);
    command.extend(expanded.arguments);
    Ok(CliCommand {
        command,
        stdin: expanded
            .prompt_on_stdin
            .then(|| combined_prompt.to_string()),
    })
}

fn resolve_executable(profile: &AiProfile) -> Result<String, AiError> {
    let executable_path = profile
        .cli_executable_path
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    if !executable_path.is_empty() {
        return Ok(executable_path.to_string());
    }
    cli_registry::find_provider_executable(profile.cli_provider_id.as_deref().unwrap_or(""))
        .ok_or_else(|| AiError::Configuration("AI CLI executable must be configured.".to_string()))
}

async fn run_process(
    command: &[String],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<CliProcessResult, AiError> {
    let mut process = tokio::process::Command::new(&command[0]);
    process
        .args(&command[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = process
        .spawn()
        .map_err(|error| AiError::Configuration(format!("AI CLI could not be started: {error}")))?;

    let stdin_pipe = child.stdin.take();
    let stdin_payload = stdin.map(ToString::to_string);
    let stdin_task = tokio::spawn(async move {
        if let Some(mut pipe) = stdin_pipe {
            if let Some(payload) = stdin_payload {
                let _ = pipe.write_all(payload.as_bytes()).await;
            }
            let _ = pipe.shutdown().await;
        }
    });

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => result
            .map_err(|error| AiError::Configuration(format!("AI CLI process failed: {error}")))?,
        Err(_) => {
            // Dropping the child future kills the process (kill_on_drop).
            stdin_task.abort();
            return Err(AiError::Configuration(format!(
                "AI CLI request timed out after {} seconds.",
                timeout.as_secs()
            )));
        }
    };
    let _ = stdin_task.await;

    Ok(CliProcessResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn build_combined_prompt(system_prompt: &str, user_prompt: &str) -> String {
    format!("System prompt:\n{system_prompt}\n\nUser prompt:\n{user_prompt}\n")
}

fn build_exit_message(result: &CliProcessResult) -> String {
    let stderr = result.stderr.trim();
    if stderr.is_empty() {
        return format!("AI CLI exited with code {}.", result.exit_code);
    }
    format!(
        "AI CLI exited with code {}: {}",
        result.exit_code,
        truncate(&extract_useful_stderr(stderr), STDERR_TRUNCATE_LIMIT)
    )
}

fn extract_useful_stderr(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    for line in &lines {
        if line.starts_with("ERROR:") {
            return (*line).to_string();
        }
    }
    for line in &lines {
        if line.contains("\"message\"") {
            return (*line).to_string();
        }
    }
    stderr.trim().to_string()
}

fn truncate(value: &str, max_length: usize) -> String {
    if value.chars().count() <= max_length {
        return value.to_string();
    }
    let truncated: String = value.chars().take(max_length).collect();
    format!("{truncated}...")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::model::ai::AiConnectionMode;

    fn create_script(body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("kortty-ai-cli-test-{}.sh", uuid::Uuid::new_v4()));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
        path
    }

    fn cli_profile(script: &Path, template: &str) -> AiProfile {
        AiProfile {
            connection_mode: AiConnectionMode::LocalCli,
            cli_provider_id: Some("test".to_string()),
            cli_executable_path: Some(script.to_string_lossy().into_owned()),
            cli_arguments_template: Some(template.to_string()),
            model: "custom-model".to_string(),
            ..AiProfile::default()
        }
    }

    #[tokio::test]
    async fn executes_cli_and_returns_stdout() {
        let script = create_script("cat \"$1\"");
        let profile = cli_profile(&script, "{promptFile}");

        let result = execute(&profile, "system", "user", Duration::from_secs(5))
            .await
            .expect("execution should succeed");

        assert!(result.content.contains("System prompt:"));
        assert!(result.content.contains("system"));
        assert!(result.content.contains("User prompt:"));
        assert!(result.content.contains("user"));
        assert!(result.usage.is_none());
        let _ = std::fs::remove_file(&script);
    }

    #[tokio::test]
    async fn reports_non_zero_exit_code_with_stderr() {
        let script = create_script("echo failure >&2\nexit 7");
        let profile = cli_profile(&script, "{promptFile}");

        let error = execute(&profile, "system", "user", Duration::from_secs(5))
            .await
            .expect_err("non-zero exit should fail");

        let message = error.to_string();
        assert!(message.contains('7'), "missing exit code: {message}");
        assert!(message.contains("failure"), "missing stderr: {message}");
        let _ = std::fs::remove_file(&script);
    }

    #[tokio::test]
    async fn connection_test_reports_cli_error_message() {
        let script = create_script("echo 'ERROR: unsupported model' >&2\nexit 7");
        let profile = cli_profile(&script, "{promptFile}");

        let error = test_connection(&profile)
            .await
            .expect_err("connection test should fail");

        let message = error.to_string();
        assert!(
            message.contains("unsupported model"),
            "missing stderr: {message}"
        );
        assert!(
            message.contains("AI CLI connection test failed"),
            "missing prefix: {message}"
        );
        let _ = std::fs::remove_file(&script);
    }

    #[tokio::test]
    async fn sends_prompt_to_stdin_when_template_requests_it() {
        let script = create_script("cat");
        let profile = cli_profile(&script, "-\n{stdinPrompt}");

        let result = execute(&profile, "system", "user", Duration::from_secs(5))
            .await
            .expect("execution should succeed");

        assert!(result.content.contains("System prompt:"));
        assert!(result.content.contains("system"));
        assert!(result.content.contains("User prompt:"));
        assert!(result.content.contains("user"));
        let _ = std::fs::remove_file(&script);
    }

    #[tokio::test]
    async fn times_out_long_running_cli() {
        let script = create_script("sleep 2");
        let profile = cli_profile(&script, "{promptFile}");

        let error = execute(&profile, "system", "user", Duration::from_millis(100))
            .await
            .expect_err("long-running CLI should time out");

        assert!(
            error.to_string().contains("timed out"),
            "missing timeout message: {error}"
        );
        let _ = std::fs::remove_file(&script);
    }

    #[tokio::test]
    async fn requires_model_when_template_uses_model_placeholder() {
        let script = create_script("cat \"$1\"");
        let mut profile = cli_profile(&script, "--model {model} {promptFile}");
        profile.model = String::new();

        let error = execute(&profile, "system", "user", Duration::from_secs(5))
            .await
            .expect_err("missing model should fail");

        assert!(error.to_string().contains("AI model must be configured"));
        let _ = std::fs::remove_file(&script);
    }

    #[tokio::test]
    async fn rejects_template_without_prompt_placeholder() {
        let script = create_script("cat");
        let profile = cli_profile(&script, "--verbose");

        let error = execute(&profile, "system", "user", Duration::from_secs(5))
            .await
            .expect_err("template without prompt placeholder should fail");

        assert!(error.to_string().contains("prompt placeholder"));
        let _ = std::fs::remove_file(&script);
    }
}
