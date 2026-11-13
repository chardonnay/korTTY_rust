//! Passive suggestions for the CLI providers listed by the Java profile editor.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::private_files::executable;

const MAX_DIRECTORIES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Provider {
    Custom,
    ClaudeCode,
    CodexCli,
    LmStudioCli,
    DevinTerminal,
    GeminiCli,
    #[serde(rename = "opencode")]
    OpenCode,
    Hermes,
    KimiCli,
    CursorAgent,
    QwenCode,
    QoderCli,
    GithubCopilotCli,
    Pi,
    KiroCli,
    Kilo,
    MistralVibeCli,
    DeepseekTui,
    #[serde(rename = "minimax")]
    MiniMax,
}

impl Provider {
    pub(crate) const ALL: [Self; 19] = [
        Self::Custom,
        Self::ClaudeCode,
        Self::CodexCli,
        Self::LmStudioCli,
        Self::DevinTerminal,
        Self::GeminiCli,
        Self::OpenCode,
        Self::Hermes,
        Self::KimiCli,
        Self::CursorAgent,
        Self::QwenCode,
        Self::QoderCli,
        Self::GithubCopilotCli,
        Self::Pi,
        Self::KiroCli,
        Self::Kilo,
        Self::MistralVibeCli,
        Self::DeepseekTui,
        Self::MiniMax,
    ];

    fn native_command(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some("claude"),
            Self::CodexCli => Some("codex"),
            Self::LmStudioCli => Some("lms"),
            Self::GeminiCli => Some("gemini"),
            Self::OpenCode => Some("opencode"),
            Self::CursorAgent => Some("cursor-agent"),
            // The verified MiniMax route uses Node and the package module.
            Self::MiniMax => Some("node"),
            _ => None,
        }
    }

    pub(crate) fn can_search(self) -> bool {
        self.native_command().is_some()
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Custom => "Custom CLI",
            Self::ClaudeCode => "Claude Code",
            Self::CodexCli => "Codex CLI",
            Self::LmStudioCli => "LM Studio CLI (lms)",
            Self::DevinTerminal => "Devin for Terminal",
            Self::GeminiCli => "Gemini CLI",
            Self::OpenCode => "OpenCode",
            Self::Hermes => "Hermes",
            Self::KimiCli => "Kimi CLI",
            Self::CursorAgent => "Cursor Agent",
            Self::QwenCode => "Qwen Code",
            Self::QoderCli => "Qoder CLI",
            Self::GithubCopilotCli => "GitHub Copilot CLI",
            Self::Pi => "Pi",
            Self::KiroCli => "Kiro CLI",
            Self::Kilo => "Kilo",
            Self::MistralVibeCli => "Mistral Vibe CLI",
            Self::DeepseekTui => "DeepSeek TUI",
            Self::MiniMax => "MiniMax CLI",
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Suggestions {
    pub(crate) executable: Option<String>,
    pub(crate) module: Option<String>,
}

pub(crate) fn search(provider: Provider, cancelled: &AtomicBool) -> Suggestions {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::home_dir();
    let directories = search_directories(std::env::split_paths(&search_path), home.as_deref());
    let executable = find_executable(provider, &directories, cancelled);
    let module = (provider == Provider::MiniMax)
        .then(|| find_minimax_module(&directories, home.as_deref(), cancelled))
        .flatten();
    Suggestions { executable, module }
}

fn search_directories(path: impl Iterator<Item = PathBuf>, home: Option<&Path>) -> Vec<PathBuf> {
    #[cfg(windows)]
    let defaults = [
        ("APPDATA", "npm"),
        ("LOCALAPPDATA", "Programs"),
        ("ProgramFiles", "nodejs"),
    ]
    .into_iter()
    .filter_map(|(name, suffix)| {
        std::env::var_os(name).map(|base| PathBuf::from(base).join(suffix))
    });
    #[cfg(unix)]
    let defaults = [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
        "/opt/local/bin",
    ]
    .into_iter()
    .map(PathBuf::from);
    let user = [
        ".local/bin",
        "bin",
        ".npm-global/bin",
        ".bun/bin",
        ".cargo/bin",
        ".deno/bin",
        "go/bin",
        ".volta/bin",
        ".lmstudio/bin",
    ]
    .into_iter()
    .filter_map(|suffix| home.map(|base| base.join(suffix)));
    let mut seen = HashSet::new();
    path.take(MAX_DIRECTORIES)
        .chain(defaults)
        .chain(user)
        .filter(|directory| valid_directory(directory) && seen.insert(directory.clone()))
        .collect()
}

fn valid_directory(path: &Path) -> bool {
    // Reuse the same path rules as the executable review, including its Windows
    // disk-prefix and alternate-stream checks, before accessing the filesystem.
    let probe = path.join(format!("kortty-probe{}", std::env::consts::EXE_SUFFIX));
    path.components()
        .any(|part| matches!(part, Component::Normal(_)))
        && executable::valid_path(&probe)
}

fn find_executable(
    provider: Provider,
    directories: &[PathBuf],
    cancelled: &AtomicBool,
) -> Option<String> {
    let name = format!(
        "{}{}",
        provider.native_command()?,
        std::env::consts::EXE_SUFFIX
    );
    directories
        .iter()
        .take_while(|_| !cancelled.load(Ordering::Acquire))
        .find_map(|directory| native_path(&directory.join(&name)))
}

fn native_path(candidate: &Path) -> Option<String> {
    let canonical = candidate.canonicalize().ok()?;
    let mut opened = executable::open(&canonical).ok()?;
    if !(1..=executable::MAX_EXECUTABLE).contains(&opened.file.metadata().ok()?.len()) {
        return None;
    }
    #[cfg(windows)]
    if !kortty_files::is_local_reference_file(&opened.file).ok()? {
        return None;
    }
    executable::require_native_image(&mut opened.file).ok()?;
    canonical.into_os_string().into_string().ok()
}

fn find_minimax_module(
    directories: &[PathBuf],
    home: Option<&Path>,
    cancelled: &AtomicBool,
) -> Option<String> {
    let prefix = std::env::var_os("NPM_CONFIG_PREFIX").map(PathBuf::from);
    let prefixes = prefix
        .into_iter()
        .chain(home.map(|path| path.join(".npm-global")))
        .chain(
            directories
                .iter()
                .flat_map(|path| [Some(path.as_path()), path.parent()])
                .flatten()
                .map(Path::to_owned),
        );
    find_module(prefixes, cancelled)
}

fn find_module(prefixes: impl Iterator<Item = PathBuf>, cancelled: &AtomicBool) -> Option<String> {
    let mut seen = HashSet::new();
    prefixes
        .filter(|path| valid_directory(path) && seen.insert(path.clone()))
        .take_while(|_| !cancelled.load(Ordering::Acquire))
        .find_map(|prefix| {
            ["node_modules", "lib/node_modules"]
                .into_iter()
                .find_map(|relative| {
                    let path = prefix
                        .join(relative)
                        .join("mmx-cli/dist/mmx.mjs")
                        .canonicalize()
                        .ok()?;
                    if !super::valid_minimax_module_path(&path) {
                        return None;
                    }
                    let metadata = path.metadata().ok()?;
                    if !metadata.is_file() {
                        return None;
                    }
                    #[cfg(windows)]
                    if !kortty_files::is_local_reference_file(
                        &kortty_files::open_regular_without_reparse(&path).ok()?,
                    )
                    .ok()?
                    {
                        return None;
                    }
                    path.into_os_string().into_string().ok()
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_catalog_preserves_java_identifiers_and_manual_entries() {
        let expected = [
            "custom",
            "claude-code",
            "codex-cli",
            "lm-studio-cli",
            "devin-terminal",
            "gemini-cli",
            "opencode",
            "hermes",
            "kimi-cli",
            "cursor-agent",
            "qwen-code",
            "qoder-cli",
            "github-copilot-cli",
            "pi",
            "kiro-cli",
            "kilo",
            "mistral-vibe-cli",
            "deepseek-tui",
            "minimax",
        ];
        for (provider, id) in Provider::ALL.into_iter().zip(expected) {
            let stored = serde_json::Value::String(id.into());
            assert_eq!(serde_json::to_value(provider).unwrap(), stored);
            assert_eq!(
                serde_json::from_value::<Provider>(stored).unwrap(),
                provider
            );
        }
        assert_eq!(
            Provider::ALL
                .into_iter()
                .filter(|provider| provider.can_search())
                .count(),
            7
        );
        assert_eq!(Provider::MiniMax.native_command(), Some("node"));
    }

    #[test]
    fn cli_catalog_suggests_native_images_without_running_them() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let path = root.join(format!("claude{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            find_executable(
                Provider::ClaudeCode,
                std::slice::from_ref(&root),
                &cancelled
            ),
            None
        );
        let fixture = super::super::fixture_executable(&root);
        std::fs::rename(fixture, &path).unwrap();
        let directories = search_directories(
            [PathBuf::from("relative/bin"), root.clone(), root.clone()].into_iter(),
            None,
        );
        assert_eq!(directories.first(), Some(&root));
        assert_eq!(directories.iter().filter(|path| *path == &root).count(), 1);
        let expected = path
            .canonicalize()
            .unwrap()
            .into_os_string()
            .into_string()
            .unwrap();
        assert_eq!(
            find_executable(Provider::ClaudeCode, &directories, &cancelled),
            Some(expected)
        );
        assert_eq!(
            find_executable(Provider::Hermes, &directories, &cancelled),
            None
        );
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            find_executable(Provider::ClaudeCode, &directories, &cancelled),
            None
        );
        assert!(!root.join("fixture-stdin").exists());
    }

    #[test]
    fn cli_catalog_minimax_suggests_paths_without_approving_package_bytes() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let module = root.join("lib/node_modules/mmx-cli/dist/mmx.mjs");
        std::fs::create_dir_all(module.parent().unwrap()).unwrap();
        std::fs::write(&module, b"fixture without approved package hash").unwrap();
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            find_module([root.clone()].into_iter(), &cancelled).as_deref(),
            module.canonicalize().unwrap().to_str()
        );
        assert!(super::super::Program::inspect_minimax_module(&module).is_err());
        cancelled.store(true, Ordering::Release);
        assert_eq!(find_module([root].into_iter(), &cancelled), None);
    }
}
