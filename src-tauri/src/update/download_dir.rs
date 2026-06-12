//! Downloads-directory resolution, ported from Java `DownloadDirectoryResolver`.
//!
//! Default is `~/Downloads`; on Linux the XDG download directory is honored
//! (the `XDG_DOWNLOAD_DIR` environment variable first, then
//! `~/.config/user-dirs.dirs` with `$HOME` substitution).

use std::path::{Path, PathBuf};

/// Resolves the user's default downloads directory.
pub fn resolve_default_downloads_directory() -> PathBuf {
    resolve(
        dirs::home_dir(),
        std::env::var("XDG_DOWNLOAD_DIR").ok().as_deref(),
        cfg!(target_os = "linux"),
    )
}

pub(crate) fn resolve(
    home: Option<PathBuf>,
    xdg_env_value: Option<&str>,
    is_linux: bool,
) -> PathBuf {
    let Some(home) = home else {
        // Mirrors Java: Path.of("Downloads").toAbsolutePath()
        return std::env::current_dir()
            .map(|dir| dir.join("Downloads"))
            .unwrap_or_else(|_| PathBuf::from("Downloads"));
    };
    if is_linux {
        if let Some(xdg_downloads) = resolve_linux_xdg_downloads_directory(&home, xdg_env_value) {
            return xdg_downloads;
        }
    }
    home.join("Downloads")
}

fn resolve_linux_xdg_downloads_directory(
    home: &Path,
    xdg_env_value: Option<&str>,
) -> Option<PathBuf> {
    if let Some(value) = xdg_env_value {
        if let Some(path) = resolve_xdg_path(home, value) {
            return Some(path);
        }
    }
    let user_dirs = home.join(".config").join("user-dirs.dirs");
    let content = std::fs::read_to_string(user_dirs).ok()?;
    parse_user_dirs_download(home, &content)
}

/// Extracts `XDG_DOWNLOAD_DIR=` from `user-dirs.dirs` content. Like the Java
/// version, the first matching line wins even if its value is blank.
pub(crate) fn parse_user_dirs_download(home: &Path, content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("XDG_DOWNLOAD_DIR=") {
            return resolve_xdg_path(home, value);
        }
    }
    None
}

/// Strips surrounding quotes, substitutes `$HOME`, and resolves relative paths
/// against the home directory (mirrors Java `resolveXdgPath`).
pub(crate) fn resolve_xdg_path(home: &Path, value: &str) -> Option<PathBuf> {
    let mut normalized = value.trim();
    if normalized.is_empty() {
        return None;
    }
    if normalized.len() >= 2
        && ((normalized.starts_with('"') && normalized.ends_with('"'))
            || (normalized.starts_with('\'') && normalized.ends_with('\'')))
    {
        normalized = &normalized[1..normalized.len() - 1];
    }
    let substituted = normalized.replace("$HOME", &home.to_string_lossy());
    let path = PathBuf::from(substituted);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(home.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\Users\\tester")
        } else {
            PathBuf::from("/home/tester")
        }
    }

    #[test]
    fn falls_back_to_home_downloads() {
        let resolved = resolve(Some(home()), None, false);
        assert_eq!(resolved, home().join("Downloads"));
    }

    #[test]
    fn linux_prefers_xdg_environment_variable() {
        let resolved = resolve(Some(home()), Some("$HOME/Transfers"), true);
        assert_eq!(resolved, home().join("Transfers"));
    }

    #[test]
    fn blank_xdg_environment_value_is_ignored() {
        let resolved = resolve(Some(home()), Some("   "), true);
        assert_eq!(resolved, home().join("Downloads"));
    }

    #[test]
    fn resolves_xdg_path_with_quotes_and_home_substitution() {
        let resolved = resolve_xdg_path(&home(), "\"$HOME/Downloads/Updates\"").unwrap();
        assert_eq!(resolved, home().join("Downloads").join("Updates"));

        let resolved = resolve_xdg_path(&home(), "'$HOME/dl'").unwrap();
        assert_eq!(resolved, home().join("dl"));
    }

    #[test]
    fn resolves_relative_xdg_path_against_home() {
        let resolved = resolve_xdg_path(&home(), "Downloads").unwrap();
        assert_eq!(resolved, home().join("Downloads"));
    }

    #[test]
    fn keeps_absolute_xdg_path() {
        let absolute = if cfg!(windows) {
            "C:\\data\\downloads"
        } else {
            "/data/downloads"
        };
        let resolved = resolve_xdg_path(&home(), absolute).unwrap();
        assert_eq!(resolved, PathBuf::from(absolute));
    }

    #[test]
    fn parses_user_dirs_content() {
        let content = "# user dirs\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Incoming\"\nXDG_DOWNLOAD_DIR=\"$HOME/Other\"\n";
        let resolved = parse_user_dirs_download(&home(), content).unwrap();

        // The first matching line wins, like in Java.
        assert_eq!(resolved, home().join("Incoming"));
    }

    #[test]
    fn user_dirs_without_download_entry_yields_none() {
        let content = "XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n";
        assert!(parse_user_dirs_download(&home(), content).is_none());
    }
}
