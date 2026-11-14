//! Typed AI settings and memory validation for portable profile backups.

use std::cell::Cell;
use std::io::{self, Write as _};

use zeroize::Zeroizing;

use super::memory::{Scope, store};
use super::profiles::java_backup;
use super::skills::{statistics, usage};
use super::{Configuration, FILE_NAME, MAX_BYTES};
use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::{Directory, FileLock, FileStamp, require_single_link};

pub(crate) const SETTINGS: &str = FILE_NAME;
pub(crate) const STATISTICS: &str = statistics::BACKUP_PATH;
pub(crate) const MEMORY: &str = "memory";
pub(crate) const MEMORY_TOP_LEVEL: [&str; 2] = ["MEMORY.md", "archive.md"];
pub(crate) const MEMORY_DIRECTORIES: [&str; 2] = ["connections", "history"];

pub(crate) fn lock_for_restore(root: &Directory) -> io::Result<FileLock> {
    Configuration::lock_mutation(root)
}

/// Publishes a prepared Java catalogue while the settings mutation lock is held.
pub(crate) fn publish_java_https(root: &Directory, contents: &[u8]) -> io::Result<()> {
    publish_java_https_with_policy(root, contents, || {
        ClipboardPolicy::load().ai_profile_creation_allowed()
    })
}

fn publish_java_https_with_policy(
    root: &Directory,
    contents: &[u8],
    allowed: impl Fn() -> bool,
) -> io::Result<()> {
    if !allowed() {
        return Ok(());
    }
    let stamp = match root.open_file(SETTINGS) {
        Ok(file) => {
            require_single_link(&file)?;
            Some(FileStamp::read(&file)?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let policy_denied = Cell::new(false);
    let check_policy = || {
        if allowed() {
            Ok(())
        } else {
            policy_denied.set(true);
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy prevents Java AI profile creation",
            ))
        }
    };
    let write = |file: &mut std::fs::File| file.write_all(contents);
    let result = match stamp {
        Some(stamp) => root.replace_with(SETTINGS, write, || {
            let file = root.open_file(SETTINGS)?;
            require_single_link(&file)?;
            stamp.require_unchanged(&file)?;
            check_policy()
        }),
        None => root.publish_checked(SETTINGS, write, check_policy),
    };
    if policy_denied.get() {
        return Ok(());
    }
    result
}

pub(crate) struct JavaHttpsReport {
    pub(crate) imported: usize,
    pub(crate) omitted: usize,
    pub(crate) keys_omitted: usize,
}

pub(crate) fn portable_java_https(bytes: &[u8]) -> io::Result<(Vec<u8>, JavaHttpsReport)> {
    let (profiles, report) = java_backup::decode(bytes)?;
    let settings = Configuration {
        profiles,
        ..Configuration::default()
    };
    let encoded = encode_java_https(&settings)?;
    Ok((
        encoded,
        JavaHttpsReport {
            imported: report.imported,
            omitted: report.omitted,
            keys_omitted: report.keys_omitted,
        },
    ))
}

pub(crate) fn merge_java_https(
    local: Option<&[u8]>,
    incoming: &[u8],
    policy: &ClipboardPolicy,
) -> io::Result<Option<Vec<u8>>> {
    let incoming = Configuration::decode(incoming)?;
    let empty = Configuration::default();
    if incoming.general != empty.general
        || incoming.local != empty.local
        || incoming.skills != empty.skills
        || incoming.memory != empty.memory
    {
        return Err(super::invalid("Invalid Java HTTPS AI profile settings"));
    }
    incoming.profiles.validate_java_https()?;
    if !policy.ai_profile_creation_allowed() {
        return Ok(None);
    }
    let mut local = local
        .map(Configuration::decode)
        .transpose()?
        .unwrap_or_default();
    let added = local.profiles.merge_java_https(incoming.profiles)?;
    if added == 0 {
        return Ok(None);
    }
    encode_java_https(&local).map(Some)
}

fn encode_java_https(settings: &Configuration) -> io::Result<Vec<u8>> {
    settings.validate()?;
    let bytes = serde_json::to_vec(settings).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(super::invalid(
            "AI profile settings exceed their size limit",
        ));
    }
    Ok(bytes)
}

pub(crate) fn flush_statistics() {
    usage::flush();
}

pub(crate) fn file_limit(path: &str) -> Option<usize> {
    match path {
        SETTINGS => Some(MAX_BYTES as usize),
        STATISTICS => Some(statistics::MAX_BYTES),
        "memory/MEMORY.md" => Some(store::MAX_BYTES),
        "memory/archive.md" => Some(store::MAX_ARCHIVE),
        _ => {
            let (directory, name) = path.strip_prefix("memory/")?.split_once('/')?;
            match directory {
                "connections" if connection_id(name.strip_suffix(".md")?).is_some() => {
                    Some(store::MAX_BYTES)
                }
                "history" if history_name(name) => Some(store::MAX_BYTES),
                _ => None,
            }
        }
    }
}

pub(crate) fn memory_connection(path: &str) -> Option<u64> {
    let name = path.strip_prefix("memory/")?;
    if let Some(name) = name.strip_prefix("connections/") {
        return connection_id(name.strip_suffix(".md")?);
    }
    connection_id(name.strip_prefix("history/connection-")?.split_once('-')?.0)
}

fn history_name(name: &str) -> bool {
    let scope = if name.starts_with("global-") {
        Scope::global()
    } else {
        let Some(id) = name
            .strip_prefix("connection-")
            .and_then(|tail| tail.split_once('-'))
            .and_then(|(id, _)| connection_id(id))
        else {
            return false;
        };
        Scope::connection(id, String::new())
    };
    store::history_name(name, &scope) && name.len() <= 64
}

fn connection_id(text: &str) -> Option<u64> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

pub(crate) fn validate(path: &str, bytes: &[u8]) -> io::Result<()> {
    match path {
        SETTINGS => Configuration::decode(bytes).map(drop),
        STATISTICS => statistics::decode(bytes).map(drop),
        _ => std::str::from_utf8(bytes)
            .map(drop)
            .map_err(io::Error::other),
    }
}

pub(crate) fn portable_settings(bytes: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut settings = Configuration::decode(bytes)?;
    settings.profiles.prepare_backup();
    settings.local.prepare_backup();
    settings.validate()?;
    serde_json::to_vec(&settings)
        .map(Zeroizing::new)
        .map_err(io::Error::other)
}

pub(crate) fn matching_skills(current: Option<&[u8]>, incoming: Option<&[u8]>) -> bool {
    let decode = |bytes: Option<&[u8]>| {
        bytes.map_or_else(|| Ok(Configuration::default()), Configuration::decode)
    };
    match (decode(current), decode(incoming)) {
        (Ok(current), Ok(incoming)) => current.skills.same_definitions(&incoming.skills),
        _ => false,
    }
}

pub(crate) fn safe_memory(path: &str, bytes: &[u8]) -> bool {
    file_limit(path).is_some_and(|limit| {
        std::str::from_utf8(bytes).is_ok_and(|text| store::validate(text, "", limit).is_ok())
    })
}

#[cfg(test)]
mod java_https_tests {
    use std::path::PathBuf;

    use super::*;

    const FIXTURE: &[u8] = include_bytes!("profiles/fixtures/java-global-settings.xml");

    #[test]
    fn java_https_delta_checks_current_creation_policy_and_native_settings() {
        let (incoming, report) = portable_java_https(FIXTURE).unwrap();
        assert_eq!(report.imported, 2);
        let denied = ClipboardPolicy {
            ai_profile_creation_allowed: false,
            ..ClipboardPolicy::default()
        };
        assert!(
            merge_java_https(None, &incoming, &denied)
                .unwrap()
                .is_none()
        );

        let mut local = Configuration::default();
        local.general.timeout_minutes = 19;
        let local = serde_json::to_vec(&local).unwrap();
        let allowed = ClipboardPolicy::default();
        let merged = merge_java_https(Some(&local), &incoming, &allowed)
            .unwrap()
            .unwrap();
        let merged_config = Configuration::decode(&merged).unwrap();
        assert_eq!(merged_config.general.timeout_minutes, 19);
        assert!(merged_config.profiles.default_profile().is_none());
        assert!(
            merge_java_https(Some(&merged), &incoming, &allowed)
                .unwrap()
                .is_none()
        );
        assert!(merge_java_https(None, &merged, &allowed).is_err());
    }

    #[test]
    fn policy_is_rechecked_before_java_https_settings_are_published() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-java-ai-profile-{}",
            crate::private_files::nonce().unwrap()
        )));
        let root = Directory::open(&scratch.0, true).unwrap();
        let _lock = lock_for_restore(&root).unwrap();
        let baseline = serde_json::to_vec(&Configuration::default()).unwrap();
        root.publish_copy(SETTINGS, &baseline).unwrap();
        let (incoming, _) = portable_java_https(FIXTURE).unwrap();
        let candidate = merge_java_https(Some(&baseline), &incoming, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();

        let checks = Cell::new(0);
        publish_java_https_with_policy(&root, &candidate, || {
            let before = checks.get();
            checks.set(before + 1);
            before == 0
        })
        .unwrap();
        assert_eq!(checks.get(), 2);
        assert_eq!(std::fs::read(scratch.0.join(SETTINGS)).unwrap(), baseline);

        publish_java_https_with_policy(&root, &candidate, || true).unwrap();
        assert_eq!(std::fs::read(scratch.0.join(SETTINGS)).unwrap(), candidate);
    }
}
