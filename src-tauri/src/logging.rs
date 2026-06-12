use anyhow::Result;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Name of the subdirectory below the KorTTY config dir used when no custom
/// log directory is configured (mirrors Java LoggingConfiguration).
pub const DEFAULT_LOG_DIRECTORY_NAME: &str = "logs";
/// Rotated log files older than this are gzip-compressed.
const COMPRESSION_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Upper bound for the retention setting (mirrors GlobalSettings.MAX_LOG_RETENTION_DAYS).
pub const MAX_LOG_RETENTION_DAYS: u32 = 3650;
/// Default retention in days (mirrors GlobalSettings.DEFAULT_LOG_RETENTION_DAYS).
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 7;

/// Returns the default log directory below the given config directory.
pub fn default_log_directory(config_dir: &Path) -> PathBuf {
    config_dir.join(DEFAULT_LOG_DIRECTORY_NAME)
}

/// Resolves the effective log directory from the configured path.
/// Blank/None falls back to `<config_dir>/logs`; a leading `~` is expanded
/// to the user's home directory (port of Java LoggingConfiguration::resolveLogDirectory).
pub fn resolve_log_directory(configured: Option<&str>, config_dir: &Path) -> PathBuf {
    let normalized = configured.map(str::trim).unwrap_or("");
    if normalized.is_empty() {
        return default_log_directory(config_dir);
    }
    if normalized == "~" || normalized.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            let remainder = normalized[1..].trim_start_matches('/');
            return if remainder.is_empty() {
                home
            } else {
                home.join(remainder)
            };
        }
    }
    PathBuf::from(normalized)
}

/// Minimal log settings read directly from the persisted settings JSON.
/// Reading must never fail hard — logging must not prevent application startup.
#[derive(Debug, Clone, Default)]
pub struct PersistedLogSettings {
    pub log_directory_path: Option<String>,
    pub log_retention_days: u32,
}

/// Reads `logDirectoryPath`/`logRetentionDays` straight from
/// `global-settings.json` without deserializing the full settings model so
/// it can run before any other subsystem is initialized.
pub fn read_persisted_log_settings(config_dir: &Path) -> PersistedLogSettings {
    let mut result = PersistedLogSettings {
        log_directory_path: None,
        log_retention_days: DEFAULT_LOG_RETENTION_DAYS,
    };
    let settings_file = config_dir.join("global-settings.json");
    let Ok(content) = std::fs::read_to_string(&settings_file) else {
        return result;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return result;
    };
    if let Some(path) = value.get("logDirectoryPath").and_then(|v| v.as_str()) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            result.log_directory_path = Some(trimmed.to_string());
        }
    }
    if let Some(days) = value.get("logRetentionDays").and_then(|v| v.as_u64()) {
        result.log_retention_days = (days as u32).min(MAX_LOG_RETENTION_DAYS);
    }
    result
}

/// Resolves and creates the log directory from persisted settings.
/// Falls back to the default directory and never fails — logging setup
/// must not prevent application startup.
pub fn prepare_log_directory_from_persisted_settings(config_dir: &Path) -> Option<PathBuf> {
    let settings = read_persisted_log_settings(config_dir);
    let log_dir = resolve_log_directory(settings.log_directory_path.as_deref(), config_dir);
    if std::fs::create_dir_all(&log_dir).is_ok() {
        let _ = maintain_log_directory(&log_dir, settings.log_retention_days);
        return Some(log_dir);
    }
    let fallback = default_log_directory(config_dir);
    if std::fs::create_dir_all(&fallback).is_ok() {
        let _ = maintain_log_directory(&fallback, settings.log_retention_days);
        return Some(fallback);
    }
    None
}

/// Maintains the log directory: deletes rotated log archives older than the
/// retention period (0 = unlimited) and gzip-compresses uncompressed rotated
/// logs older than 24 hours, preserving the modification time.
pub fn maintain_log_directory(log_directory: &Path, retention_days: u32) -> std::io::Result<()> {
    maintain_log_directory_at(log_directory, retention_days, SystemTime::now())
}

fn maintain_log_directory_at(
    log_directory: &Path,
    retention_days: u32,
    now: SystemTime,
) -> std::io::Result<()> {
    if !log_directory.is_dir() {
        return Ok(());
    }
    let normalized_retention_days = retention_days.min(MAX_LOG_RETENTION_DAYS);
    let compression_cutoff = now.checked_sub(COMPRESSION_AGE);
    let retention_cutoff = if normalized_retention_days > 0 {
        now.checked_sub(Duration::from_secs(
            u64::from(normalized_retention_days) * 24 * 60 * 60,
        ))
    } else {
        None
    };

    for entry in std::fs::read_dir(log_directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };

        if is_rotated_log_file(file_name) {
            if let Some(cutoff) = retention_cutoff {
                if modified < cutoff {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            }
        }

        if is_uncompressed_rotated_log_file(file_name) {
            if let Some(cutoff) = compression_cutoff {
                if modified < cutoff {
                    let _ = compress_log_file(&path, modified);
                }
            }
        }
    }
    Ok(())
}

/// Matches `kortty.YYYY-MM-DD.log` and `kortty.YYYY-MM-DD.log.gz`.
fn is_rotated_log_file(file_name: &str) -> bool {
    is_uncompressed_rotated_log_file(file_name)
        || file_name
            .strip_suffix(".gz")
            .map(is_uncompressed_rotated_log_file)
            .unwrap_or(false)
}

/// Matches `kortty.YYYY-MM-DD.log`.
fn is_uncompressed_rotated_log_file(file_name: &str) -> bool {
    let Some(rest) = file_name.strip_prefix("kortty.") else {
        return false;
    };
    let Some(date) = rest.strip_suffix(".log") else {
        return false;
    };
    let bytes = date.as_bytes();
    if bytes.len() != 10 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| match index {
        4 | 7 => *byte == b'-',
        _ => byte.is_ascii_digit(),
    })
}

/// Compresses the given log file to `<name>.gz`, preserving the original
/// modification time. If the target already exists, the original is removed
/// (port of Java LoggingConfiguration::compressLogFile).
fn compress_log_file(log_file: &Path, original_modified: SystemTime) -> std::io::Result<()> {
    let Some(file_name) = log_file.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let compressed_path = log_file.with_file_name(format!("{file_name}.gz"));

    let target = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&compressed_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(log_file);
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    // If encoding fails partway through, the freshly created .gz target would be
    // left behind as a truncated/corrupt archive. Remove it on any encoding error
    // so a later maintenance pass can retry compressing the still-present source.
    let encode = || -> std::io::Result<std::fs::File> {
        let mut input = std::fs::File::open(log_file)?;
        let mut encoder = GzEncoder::new(target, Compression::default());
        std::io::copy(&mut input, &mut encoder)?;
        encoder.finish()
    };
    let target = match encode() {
        Ok(target) => target,
        Err(error) => {
            let _ = std::fs::remove_file(&compressed_path);
            return Err(error);
        }
    };
    // The archive is complete and valid from here on; preserve its mtime and drop
    // the source. A failure removing the source must not delete the good archive.
    let _ = target.set_modified(original_modified);
    drop(target);
    std::fs::remove_file(log_file)?;
    Ok(())
}

pub struct TerminalLogger {
    connection_name: String,
    buffer: Vec<u8>,
}

impl TerminalLogger {
    pub fn new(connection_name: &str) -> Self {
        Self {
            connection_name: connection_name.to_string(),
            buffer: Vec::new(),
        }
    }

    pub fn append(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    pub fn flush_to_file(&mut self) -> Result<PathBuf> {
        let config_dir = crate::persistence::xml_repository::config_dir()?;
        let history_dir = config_dir.join("history");
        if !history_dir.exists() {
            std::fs::create_dir_all(&history_dir)?;
        }

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let filename = format!("{}_{}.log.gz", self.connection_name, timestamp);
        let path = history_dir.join(&filename);

        let file = std::fs::File::create(&path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(&self.buffer)?;
        encoder.finish()?;

        self.buffer.clear();
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{}-{}-{}",
            prefix,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn set_mtime(path: &Path, time: SystemTime) {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open file for mtime update");
        file.set_modified(time).expect("set mtime");
    }

    fn read_gzip(path: &Path) -> String {
        let mut decoder = GzDecoder::new(std::fs::File::open(path).expect("open gz"));
        let mut content = String::new();
        decoder.read_to_string(&mut content).expect("read gz");
        content
    }

    #[test]
    fn default_log_directory_is_logs_subdirectory_of_config_dir() {
        let config_dir = temp_dir("kortty-log-default");
        assert_eq!(default_log_directory(&config_dir), config_dir.join("logs"));
        assert_eq!(
            resolve_log_directory(None, &config_dir),
            config_dir.join("logs")
        );
        assert_eq!(
            resolve_log_directory(Some("   "), &config_dir),
            config_dir.join("logs")
        );
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn resolve_log_directory_expands_tilde() {
        let config_dir = temp_dir("kortty-log-tilde");
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(
            resolve_log_directory(Some("~/kortty-logs"), &config_dir),
            home.join("kortty-logs")
        );
        assert_eq!(resolve_log_directory(Some("~"), &config_dir), home);
        let explicit = config_dir.join("explicit");
        assert_eq!(
            resolve_log_directory(Some(explicit.to_str().unwrap()), &config_dir),
            explicit
        );
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn persisted_log_settings_are_read_before_full_settings_load() {
        let config_dir = temp_dir("kortty-log-settings");
        let custom_logs = config_dir.join("custom-logs");
        std::fs::write(
            config_dir.join("global-settings.json"),
            format!(
                "{{\"logDirectoryPath\": \"{}\", \"logRetentionDays\": 12}}",
                custom_logs.to_str().unwrap()
            ),
        )
        .expect("write settings");

        let settings = read_persisted_log_settings(&config_dir);
        assert_eq!(settings.log_directory_path.as_deref(), custom_logs.to_str());
        assert_eq!(settings.log_retention_days, 12);
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn persisted_log_settings_fall_back_to_defaults() {
        let config_dir = temp_dir("kortty-log-settings-default");
        let settings = read_persisted_log_settings(&config_dir);
        assert_eq!(settings.log_directory_path, None);
        assert_eq!(settings.log_retention_days, DEFAULT_LOG_RETENTION_DAYS);

        std::fs::write(config_dir.join("global-settings.json"), "{not json")
            .expect("write settings");
        let settings = read_persisted_log_settings(&config_dir);
        assert_eq!(settings.log_directory_path, None);
        assert_eq!(settings.log_retention_days, DEFAULT_LOG_RETENTION_DAYS);
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn maintenance_compresses_rotated_logs_older_than_twenty_four_hours() {
        let log_dir = temp_dir("kortty-log-maintenance");
        let old_log = log_dir.join("kortty.2026-05-18.log");
        let active_log = log_dir.join("kortty.log");
        std::fs::write(&old_log, "old log\n").expect("write old log");
        std::fs::write(&active_log, "active log\n").expect("write active log");
        let now = SystemTime::now();
        let old_time = now - Duration::from_secs(25 * 60 * 60);
        set_mtime(&old_log, old_time);
        set_mtime(&active_log, old_time);

        maintain_log_directory_at(&log_dir, DEFAULT_LOG_RETENTION_DAYS, now).expect("maintain");

        let compressed = log_dir.join("kortty.2026-05-18.log.gz");
        assert!(!old_log.exists());
        assert!(compressed.exists());
        assert!(active_log.exists());
        assert_eq!(read_gzip(&compressed), "old log\n");
        // Modification time of the archive matches the original file.
        let archived_mtime = std::fs::metadata(&compressed)
            .expect("metadata")
            .modified()
            .expect("modified");
        let drift = archived_mtime
            .duration_since(old_time)
            .unwrap_or_else(|e| e.duration());
        assert!(drift < Duration::from_secs(2), "mtime not preserved");
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn maintenance_deletes_archives_older_than_retention_days() {
        let log_dir = temp_dir("kortty-log-retention");
        let expired_log = log_dir.join("kortty.2026-05-10.log.gz");
        let unrelated = log_dir.join("notes.txt");
        std::fs::write(&expired_log, "expired").expect("write expired");
        std::fs::write(&unrelated, "keep").expect("write unrelated");
        let now = SystemTime::now();
        let expired_time = now - Duration::from_secs(10 * 24 * 60 * 60);
        set_mtime(&expired_log, expired_time);
        set_mtime(&unrelated, expired_time);

        maintain_log_directory_at(&log_dir, 7, now).expect("maintain");

        assert!(!expired_log.exists());
        assert!(unrelated.exists());
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn maintenance_keeps_everything_when_retention_is_unlimited() {
        let log_dir = temp_dir("kortty-log-unlimited");
        let very_old = log_dir.join("kortty.2020-01-01.log.gz");
        std::fs::write(&very_old, "ancient").expect("write");
        let now = SystemTime::now();
        set_mtime(&very_old, now - Duration::from_secs(2000 * 24 * 60 * 60));

        maintain_log_directory_at(&log_dir, 0, now).expect("maintain");

        assert!(very_old.exists());
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn compression_with_existing_archive_drops_original() {
        let log_dir = temp_dir("kortty-log-existing-gz");
        let old_log = log_dir.join("kortty.2026-05-18.log");
        let existing_gz = log_dir.join("kortty.2026-05-18.log.gz");
        std::fs::write(&old_log, "uncompressed").expect("write log");
        std::fs::write(&existing_gz, "already-there").expect("write gz");
        let now = SystemTime::now();
        set_mtime(&old_log, now - Duration::from_secs(25 * 60 * 60));
        set_mtime(&existing_gz, now);

        maintain_log_directory_at(&log_dir, DEFAULT_LOG_RETENTION_DAYS, now).expect("maintain");

        assert!(!old_log.exists());
        assert!(existing_gz.exists());
        assert_eq!(
            std::fs::read_to_string(&existing_gz).expect("read"),
            "already-there"
        );
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn failed_compression_removes_partial_archive() {
        let log_dir = temp_dir("kortty-log-partial-gz");
        // The source log does not exist, so the .gz target is created first and
        // then opening the source fails — the partial .gz must be cleaned up.
        let missing_log = log_dir.join("kortty.2026-05-18.log");
        let compressed = log_dir.join("kortty.2026-05-18.log.gz");

        let result = compress_log_file(&missing_log, SystemTime::now());

        assert!(
            result.is_err(),
            "compression should fail for a missing source"
        );
        assert!(
            !compressed.exists(),
            "partial .gz archive must be removed on failure"
        );
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn rotated_log_file_pattern_matches_java_naming() {
        assert!(is_uncompressed_rotated_log_file("kortty.2026-05-18.log"));
        assert!(is_rotated_log_file("kortty.2026-05-18.log.gz"));
        assert!(!is_uncompressed_rotated_log_file("kortty.log"));
        assert!(!is_rotated_log_file("other.2026-05-18.log"));
        assert!(!is_rotated_log_file("kortty.2026-5-18.log"));
        assert!(!is_rotated_log_file("kortty.20a6-05-18.log"));
    }
}
