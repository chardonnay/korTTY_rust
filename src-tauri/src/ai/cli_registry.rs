//! Registry of AI CLI providers shown in the profile editor.
//!
//! Port of the Java `AiCliProviderRegistry` (incl. `AiCliProviderDescriptor`,
//! `AiCliArgumentPreset` and `AiCliModelPreset`).

use crate::model::ai::AiReasoningEffort;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Optional model metadata for providers with verified model/reasoning pairs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiCliModelPreset {
    pub model: String,
    #[serde(default)]
    pub reasoning_efforts: Vec<AiReasoningEffort>,
}

/// Verified CLI argument template shown for a local AI CLI provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiCliArgumentPreset {
    pub label: String,
    pub template: String,
}

/// Metadata for a selectable local AI CLI provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiCliProviderDescriptor {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub command_candidates: Vec<String>,
    #[serde(default)]
    pub model_presets: Vec<AiCliModelPreset>,
    #[serde(default)]
    pub argument_presets: Vec<AiCliArgumentPreset>,
    #[serde(default)]
    pub supports_rate_limit_probe: bool,
}

const CUSTOM_MODEL_EFFORTS: [AiReasoningEffort; 7] = [
    AiReasoningEffort::Disabled,
    AiReasoningEffort::None,
    AiReasoningEffort::Minimal,
    AiReasoningEffort::Low,
    AiReasoningEffort::Medium,
    AiReasoningEffort::High,
    AiReasoningEffort::Xhigh,
];

const LEGACY_CODEX_EXEC_READ_ONLY_STDIN_TEMPLATE: &str = "exec\n--model\n{model}\n--sandbox\nread-only\n--skip-git-repo-check\n--ephemeral\n-\n{stdinPrompt}";

const CODEX_EXEC_READ_ONLY_STDIN_TEMPLATE: &str =
    "exec\n--sandbox\nread-only\n--skip-git-repo-check\n--ephemeral\n-\n{stdinPrompt}";

fn provider(
    id: &str,
    display_name: &str,
    command_candidates: &[&str],
    argument_presets: Vec<AiCliArgumentPreset>,
) -> AiCliProviderDescriptor {
    AiCliProviderDescriptor {
        id: id.to_string(),
        display_name: display_name.to_string(),
        command_candidates: command_candidates.iter().map(ToString::to_string).collect(),
        model_presets: Vec::new(),
        argument_presets,
        supports_rate_limit_probe: false,
    }
}

fn build_providers() -> Vec<AiCliProviderDescriptor> {
    let codex_exec_read_only_stdin = AiCliArgumentPreset {
        label: "codex exec read-only stdin".to_string(),
        template: CODEX_EXEC_READ_ONLY_STDIN_TEMPLATE.to_string(),
    };
    vec![
        provider("claude-code", "Claude Code", &["claude"], Vec::new()),
        provider(
            "codex-cli",
            "Codex CLI",
            &["codex"],
            vec![codex_exec_read_only_stdin],
        ),
        provider("devin-terminal", "Devin for Terminal", &[], Vec::new()),
        provider("gemini-cli", "Gemini CLI", &["gemini"], Vec::new()),
        provider("opencode", "OpenCode", &["opencode"], Vec::new()),
        provider("hermes", "Hermes", &[], Vec::new()),
        provider("kimi-cli", "Kimi CLI", &[], Vec::new()),
        provider(
            "cursor-agent",
            "Cursor Agent",
            &["cursor-agent"],
            Vec::new(),
        ),
        provider("qwen-code", "Qwen Code", &[], Vec::new()),
        provider("qoder-cli", "Qoder CLI", &[], Vec::new()),
        provider("github-copilot-cli", "GitHub Copilot CLI", &[], Vec::new()),
        provider("pi", "Pi", &[], Vec::new()),
        provider("kiro-cli", "Kiro CLI", &[], Vec::new()),
        provider("kilo", "Kilo", &[], Vec::new()),
        provider("mistral-vibe-cli", "Mistral Vibe CLI", &[], Vec::new()),
        provider("deepseek-tui", "DeepSeek TUI", &[], Vec::new()),
        provider("minimax", "MiniMAX", &[], Vec::new()),
    ]
}

/// All known AI CLI providers in their fixed display order.
pub fn providers() -> &'static [AiCliProviderDescriptor] {
    static PROVIDERS: OnceLock<Vec<AiCliProviderDescriptor>> = OnceLock::new();
    PROVIDERS.get_or_init(build_providers)
}

/// Finds a provider by its (case-insensitively normalized) id.
pub fn find(provider_id: &str) -> Option<&'static AiCliProviderDescriptor> {
    let normalized = normalize_id(provider_id);
    if normalized.is_empty() {
        return None;
    }
    providers()
        .iter()
        .find(|provider| provider.id == normalized)
}

/// The provider preselected for new CLI profiles.
pub fn default_provider() -> &'static AiCliProviderDescriptor {
    &providers()[0]
}

/// First verified argument preset of a provider, if any.
pub fn default_argument_preset(provider_id: &str) -> Option<AiCliArgumentPreset> {
    find(provider_id).and_then(|provider| provider.argument_presets.first().cloned())
}

/// True when the template matches a deprecated bundled default for the provider.
pub fn is_deprecated_default_argument_template(provider_id: &str, template: &str) -> bool {
    if normalize_id(provider_id) != "codex-cli" {
        return false;
    }
    normalize_template(LEGACY_CODEX_EXEC_READ_ONLY_STDIN_TEMPLATE) == normalize_template(template)
}

/// True when the template matches a current or deprecated bundled default.
pub fn is_known_default_argument_template(provider_id: &str, template: &str) -> bool {
    let normalized_template = normalize_template(template);
    if normalized_template.is_empty() {
        return false;
    }
    if is_deprecated_default_argument_template(provider_id, template) {
        return true;
    }
    find(provider_id).is_some_and(|provider| {
        provider
            .argument_presets
            .iter()
            .any(|preset| normalize_template(&preset.template) == normalized_template)
    })
}

/// Reasoning efforts KorTTY can safely offer for the given provider/model pair.
pub fn available_reasoning_efforts(provider_id: &str, model: &str) -> Vec<AiReasoningEffort> {
    let normalized_model = model.trim();
    if !normalized_model.is_empty() {
        if let Some(provider) = find(provider_id) {
            for preset in &provider.model_presets {
                if preset.model.eq_ignore_ascii_case(normalized_model) {
                    return preset.reasoning_efforts.clone();
                }
            }
        }
    }
    CUSTOM_MODEL_EFFORTS.to_vec()
}

/// Resolves the executable for a provider via its command candidates.
pub fn find_provider_executable(provider_id: &str) -> Option<String> {
    find(provider_id)
        .and_then(|provider| find_executable_from_candidates(&provider.command_candidates))
}

/// Resolves the first command candidate that exists as an executable.
pub fn find_executable_from_candidates(command_candidates: &[String]) -> Option<String> {
    command_candidates
        .iter()
        .find_map(|candidate| find_executable(candidate))
}

/// Resolves a command name or path to an executable file.
///
/// Direct paths (anything containing a path separator or an absolute path) are
/// checked as-is; plain names are scanned on `PATH` plus common macOS install
/// directories (`~/.local/bin`, `~/.npm-global/bin`, `/opt/homebrew/bin`,
/// `/usr/local/bin`). On Windows the `PATHEXT` extensions are applied.
pub fn find_executable(command: &str) -> Option<String> {
    let candidate = command.trim();
    if candidate.is_empty() {
        return None;
    }
    let direct_path = Path::new(candidate);
    if candidate.contains('/') || candidate.contains('\\') || direct_path.is_absolute() {
        return is_executable(direct_path).then(|| direct_path.to_string_lossy().into_owned());
    }
    let extensions = executable_extensions();
    for dir in search_directories() {
        for extension in &extensions {
            let path = dir.join(format!("{candidate}{extension}"));
            if is_executable(&path) {
                return Some(path.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn search_directories() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|dir| !dir.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        let mut extras: Vec<PathBuf> = Vec::new();
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            extras.push(home.join(".local").join("bin"));
            extras.push(home.join(".npm-global").join("bin"));
        }
        extras.push(PathBuf::from("/opt/homebrew/bin"));
        extras.push(PathBuf::from("/usr/local/bin"));
        for extra in extras {
            if !dirs.contains(&extra) {
                dirs.push(extra);
            }
        }
    }
    dirs
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(windows)]
fn executable_extensions() -> Vec<String> {
    match std::env::var("PATHEXT") {
        Ok(pathext) if !pathext.trim().is_empty() => {
            let mut extensions: Vec<String> = Vec::new();
            for extension in pathext.split(';') {
                let extension = extension.trim();
                if extension.is_empty() {
                    continue;
                }
                let normalized = if extension.starts_with('.') {
                    extension.to_string()
                } else {
                    format!(".{extension}")
                };
                if !extensions.contains(&normalized) {
                    extensions.push(normalized);
                }
            }
            extensions
        }
        _ => vec![
            String::new(),
            ".exe".to_string(),
            ".cmd".to_string(),
            ".bat".to_string(),
        ],
    }
}

#[cfg(not(windows))]
fn executable_extensions() -> Vec<String> {
    vec![String::new()]
}

pub(crate) fn normalize_id(provider_id: &str) -> String {
    provider_id.trim().to_lowercase()
}

fn normalize_template(template: &str) -> String {
    template
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_screenshot_providers_and_minimax() {
        let display_names: Vec<&str> = providers()
            .iter()
            .map(|provider| provider.display_name.as_str())
            .collect();
        for expected in [
            "Claude Code",
            "Codex CLI",
            "Devin for Terminal",
            "Gemini CLI",
            "OpenCode",
            "Hermes",
            "Kimi CLI",
            "Cursor Agent",
            "Qwen Code",
            "Qoder CLI",
            "GitHub Copilot CLI",
            "Pi",
            "Kiro CLI",
            "Kilo",
            "Mistral Vibe CLI",
            "DeepSeek TUI",
            "MiniMAX",
        ] {
            assert!(
                display_names.contains(&expected),
                "missing provider {expected}"
            );
        }
        assert_eq!(providers().len(), 17);
        assert_eq!(default_provider().id, "claude-code");
    }

    #[test]
    fn minimax_has_no_invented_executable_or_models() {
        let minimax = find("minimax").expect("minimax provider");
        assert!(minimax.command_candidates.is_empty());
        assert!(minimax.model_presets.is_empty());
        assert!(minimax.argument_presets.is_empty());
        assert!(!minimax.supports_rate_limit_probe);
    }

    #[test]
    fn codex_cli_includes_verified_read_only_stdin_template() {
        let codex = find("codex-cli").expect("codex provider");
        assert_eq!(codex.argument_presets.len(), 1);
        let template = &codex.argument_presets[0].template;
        assert!(template.contains("exec"));
        assert!(template.contains("--sandbox"));
        assert!(template.contains("read-only"));
        assert!(template.contains('-'));
        assert!(template.contains("{stdinPrompt}"));
        assert!(!template.contains("{model}"));
    }

    #[test]
    fn detects_deprecated_codex_template_that_forced_model_argument() {
        let legacy_template = "exec\n--model\n{model}\n--sandbox\nread-only\n--skip-git-repo-check\n--ephemeral\n-\n{stdinPrompt}\n";
        assert!(is_deprecated_default_argument_template(
            "codex-cli",
            legacy_template
        ));
        assert!(!is_deprecated_default_argument_template(
            "claude-code",
            legacy_template
        ));
    }

    #[test]
    fn recognizes_current_and_deprecated_default_argument_templates() {
        let codex = find("codex-cli").expect("codex provider");
        let current_template = codex.argument_presets[0].template.clone();
        let legacy_template = "exec\n--model\n{model}\n--sandbox\nread-only\n--skip-git-repo-check\n--ephemeral\n-\n{stdinPrompt}\n";

        assert!(is_known_default_argument_template(
            "codex-cli",
            &current_template
        ));
        assert!(is_known_default_argument_template(
            "codex-cli",
            legacy_template
        ));
        assert!(!is_known_default_argument_template(
            "codex-cli",
            "custom {promptFile}"
        ));
    }

    #[test]
    fn finds_provider_by_case_insensitive_id() {
        assert_eq!(
            find(" Claude-Code ").map(|p| p.id.as_str()),
            Some("claude-code")
        );
        assert!(find("").is_none());
        assert!(find("unknown").is_none());
    }

    #[test]
    fn available_reasoning_efforts_falls_back_to_custom_model_efforts() {
        let efforts = available_reasoning_efforts("claude-code", "some-custom-model");
        assert_eq!(
            efforts,
            vec![
                AiReasoningEffort::Disabled,
                AiReasoningEffort::None,
                AiReasoningEffort::Minimal,
                AiReasoningEffort::Low,
                AiReasoningEffort::Medium,
                AiReasoningEffort::High,
                AiReasoningEffort::Xhigh,
            ]
        );
        assert_eq!(
            available_reasoning_efforts("unknown", ""),
            CUSTOM_MODEL_EFFORTS.to_vec()
        );
    }

    #[test]
    fn default_argument_preset_returns_first_preset_only_for_codex() {
        assert_eq!(
            default_argument_preset("codex-cli").map(|preset| preset.label),
            Some("codex exec read-only stdin".to_string())
        );
        assert!(default_argument_preset("claude-code").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn find_executable_resolves_direct_path() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "kortty-cli-registry-test-{}.sh",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write script");
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");

        let resolved = find_executable(&path.to_string_lossy());
        assert_eq!(resolved, Some(path.to_string_lossy().into_owned()));

        let _ = std::fs::remove_file(&path);
        assert!(find_executable("kortty-definitely-missing-cli").is_none());
        assert!(find_executable("  ").is_none());
    }
}
