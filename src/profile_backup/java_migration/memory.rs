//! Converts Java AI Memory documents without changing their scope.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, LazyLock};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use regex::Regex;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::ai_manager::backup as ai;
use crate::connection_store::import::visit_java_connection_ids;
use crate::profile_backup::Snapshot;

const GLOBAL: &str = "memory/MEMORY.md";
pub(in crate::profile_backup) const ARCHIVE: &str = "memory/archive.md";
const ARCHIVE_SCOPE: &str = "<!-- kortty:archived scope=";
const GLOBAL_MARKER: &str = "<!-- kortty:archived scope=global -->";
const CONNECTION_SCOPE: &str = "<!-- kortty:archived scope=connection:";
const MAX_SAFE_NAME: usize = 80;

pub(in crate::profile_backup) struct Source {
    pub(in crate::profile_backup) connections: Arc<Zeroizing<Vec<u8>>>,
    pub(in crate::profile_backup) files: Snapshot,
}

fn safe_name(id: &str) -> String {
    let mut file: String = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if file == id && file.len() <= MAX_SAFE_NAME {
        return file;
    }
    file.truncate(MAX_SAFE_NAME - 9);
    let digest = Sha256::digest(id.as_bytes());
    let prefix = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    format!("{file}-{prefix:08x}")
}

fn java_whitespace(character: char) -> bool {
    matches!(character, '\u{001c}'..='\u{001f}')
        || character.is_whitespace()
            && !matches!(character, '\u{0085}' | '\u{00a0}' | '\u{2007}' | '\u{202f}')
}

#[derive(Default)]
pub(in crate::profile_backup) struct MappedScopes {
    names: BTreeMap<String, u64>,
    ids: BTreeMap<String, u64>,
}

impl MappedScopes {
    pub(in crate::profile_backup) fn read(
        source: &[u8],
        mut lookup_native: impl FnMut(&str) -> Option<u64>,
    ) -> io::Result<Self> {
        let mut aliases = BTreeMap::new();
        visit_java_connection_ids(source, |original| {
            let id = original.trim_matches(java_whitespace);
            if id.is_empty() {
                return;
            }
            let name = safe_name(id);
            // Count filename aliases before considering whether their profiles can
            // be imported. A single backup file might represent either source ID.
            aliases
                .entry(name.to_ascii_lowercase())
                .and_modify(|entry| *entry = None)
                .or_insert_with(|| {
                    lookup_native(original).map(|native| (id.to_owned(), name, native))
                });
        })?;
        let mut mapping = Self::default();
        for (id, name, native) in aliases.into_values().flatten() {
            mapping.names.insert(name, native);
            mapping.ids.insert(id, native);
        }
        Ok(mapping)
    }
}

fn history_name(name: &str) -> Option<(&str, &str)> {
    static HISTORY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([A-Za-z0-9_-]{1,80})(-[0-9]{8}-[0-9]{6}-[0-9]{3}(?:-[0-9]+)?\.md)$")
            .expect("valid Java history filename pattern")
    });
    let captures = HISTORY.captures(name)?;
    let (stem, suffix) = (captures.get(1)?.as_str(), captures.get(2)?.as_str());
    ai::file_limit(&format!("memory/history/connection-1{suffix}"))
        .is_some()
        .then_some((stem, suffix))
}

fn valid_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SAFE_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

enum Scoped<'a> {
    Document(&'a str),
    History(&'a str, &'a str),
    Archive,
}

impl<'a> Scoped<'a> {
    fn parse(path: &'a str) -> Option<Self> {
        if path == ARCHIVE {
            return Some(Self::Archive);
        }
        if let Some(name) = path
            .strip_prefix("memory/connections/")
            .and_then(|path| path.strip_suffix(".md"))
        {
            return valid_safe_name(name).then_some(Self::Document(name));
        }
        let (name, suffix) = history_name(path.strip_prefix("memory/history/connection-")?)?;
        Some(Self::History(name, suffix))
    }

    fn target(&self, id: u64) -> String {
        match self {
            Self::Document(_) => format!("memory/connections/{id}.md"),
            Self::History(_, suffix) => format!("memory/history/connection-{id}{suffix}"),
            Self::Archive => ARCHIVE.into(),
        }
    }
}

/// Separates source-only files from the native additive delta.
pub(in crate::profile_backup) fn scoped_source(path: &str, bytes: &[u8]) -> bool {
    match Scoped::parse(path) {
        Some(Scoped::Archive) => {
            bytes.len() <= ai::file_limit(ARCHIVE).unwrap_or_default()
                && std::str::from_utf8(bytes)
                    .is_ok_and(|text| text.lines().any(|line| line.starts_with(CONNECTION_SCOPE)))
        }
        Some(scope) => ai::safe_memory(&scope.target(1), bytes),
        None => false,
    }
}

pub(in crate::profile_backup) fn bind(
    path: &str,
    contents: Zeroizing<Vec<u8>>,
    scopes: &MappedScopes,
) -> Option<(String, Zeroizing<Vec<u8>>)> {
    let scope = Scoped::parse(path)?;
    let (target, bytes) = match scope {
        Scoped::Document(name) | Scoped::History(name, _) => {
            (scope.target(*scopes.names.get(name)?), contents)
        }
        Scoped::Archive => {
            let text = std::str::from_utf8(&contents).ok()?;
            let archive = convert_archive(text, |encoded| {
                let id = URL_SAFE_NO_PAD.decode(encoded).ok()?;
                let id = std::str::from_utf8(&id).ok()?;
                scopes.ids.get(id).copied()
            })?;
            (ARCHIVE.into(), archive)
        }
    };
    ai::safe_memory(&target, &bytes).then_some((target, bytes))
}

fn archive_lines(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.split_inclusive('\n').map(|line| {
        let marker = line.strip_suffix('\n').map_or(line, |with_cr| {
            with_cr.strip_suffix('\r').unwrap_or(with_cr)
        });
        (marker, &line[marker.len()..])
    })
}

fn archive_scope(marker: &str) -> Option<Option<&str>> {
    if marker.contains('\r') {
        return None;
    }
    let visible = marker.trim().trim_start_matches('\u{feff}').trim();
    if !visible.starts_with(ARCHIVE_SCOPE) || marker == GLOBAL_MARKER {
        return Some(None);
    }
    let token = marker
        .strip_prefix(CONNECTION_SCOPE)?
        .strip_suffix(" -->")?;
    Some(Some(token))
}

fn convert_archive(
    text: &str,
    mut native_id: impl FnMut(&str) -> Option<u64>,
) -> Option<Zeroizing<Vec<u8>>> {
    let mut archive = Zeroizing::new(Vec::with_capacity(text.len()));
    for (marker, ending) in archive_lines(text) {
        if let Some(encoded) = archive_scope(marker)? {
            let id = native_id(encoded)?;
            let native_scope = URL_SAFE_NO_PAD.encode(id.to_string());
            archive.extend_from_slice(format!("{CONNECTION_SCOPE}{native_scope} -->").as_bytes());
        } else {
            archive.extend_from_slice(marker.as_bytes());
        }
        archive.extend_from_slice(ending.as_bytes());
    }
    Some(archive)
}

/// Revalidates paths, contents, and every scope in a queued native projection.
pub(in crate::profile_backup) fn validate_bound(
    path: &str,
    bytes: &[u8],
    mut visit: impl FnMut(u64) -> io::Result<()>,
) -> io::Result<()> {
    if !ai::safe_memory(path, bytes) {
        return Err(invalid("Invalid reviewed AI Memory file"));
    }
    if path == ARCHIVE {
        let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
        let mut seen_connection = false;
        for (marker, _) in archive_lines(text) {
            if let Some(encoded) = archive_scope(marker)
                .ok_or_else(|| invalid("Invalid reviewed AI Memory archive scope"))?
            {
                let id = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .ok()
                    .and_then(|bytes| std::str::from_utf8(&bytes).ok()?.parse::<u64>().ok())
                    .filter(|id| *id != 0 && URL_SAFE_NO_PAD.encode(id.to_string()) == encoded)
                    .ok_or_else(|| invalid("Invalid reviewed AI Memory archive connection"))?;
                seen_connection = true;
                visit(id)?;
            }
        }
        if !seen_connection {
            return Err(invalid(
                "The reviewed AI Memory archive has no connection scope",
            ));
        }
        return Ok(());
    }
    visit(
        ai::memory_connection(path)
            .ok_or_else(|| invalid("Invalid reviewed connection memory path"))?,
    )
}

pub(in crate::profile_backup) fn supported(path: &str) -> bool {
    matches!(path, GLOBAL | ARCHIVE)
        || path.starts_with("memory/history/global-") && ai::file_limit(path).is_some()
}

pub(in crate::profile_backup) fn validate(path: &str, bytes: &[u8]) -> io::Result<()> {
    if !supported(path) {
        let reason = if path.starts_with("memory/connections/")
            || path.starts_with("memory/history/connection-")
        {
            "connection memory needs an exact connection mapping"
        } else {
            "unsupported Java AI Memory member"
        };
        return Err(invalid(reason));
    }
    if !ai::safe_memory(path, bytes) {
        return Err(invalid(
            "AI Memory exceeds its size limit, has invalid UTF-8, or contains a possible credential",
        ));
    }
    if path == ARCHIVE {
        let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
        if text.split(['\r', '\n']).any(|line| {
            let marker = line.trim().trim_start_matches('\u{feff}').trim();
            marker.starts_with(ARCHIVE_SCOPE) && line != GLOBAL_MARKER
        }) {
            return Err(invalid("AI Memory archive contains non-global scopes"));
        }
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_ssh_memory_rejects_case_and_trimmed_aliases_from_unsupported_profiles() {
        let id = "f827d86d-1531-4cc5-9d0f-f6e5c1a2d850";
        let source = |alias: &str| {
            format!(
                "<connections><connection id='{id}'><host>valid.example</host></connection>\
                 <connection id='{alias}'><protocol>UNRECOGNIZED</protocol></connection></connections>"
            )
        };
        for alias in [id.to_ascii_uppercase(), format!("\u{2003}{id} ")] {
            let scopes = MappedScopes::read(source(&alias).as_bytes(), |original| {
                (original == id).then_some(17)
            })
            .unwrap();
            assert!(scopes.names.is_empty());
            assert!(scopes.ids.is_empty());
        }
        let nonbreaking = format!("\u{00a0}{id}");
        let scopes = MappedScopes::read(source(&nonbreaking).as_bytes(), |original| {
            (original == id).then_some(17)
        })
        .unwrap();
        assert_eq!(scopes.names, BTreeMap::from([(id.to_owned(), 17)]));
    }

    #[test]
    fn java_memory_legacy_filenames_reject_a_collision_with_an_unimported_profile() {
        let prefix = "a".repeat(71);
        let first = format!("{prefix}@fixture45694");
        let second = format!("{prefix}@fixture51370");
        let name = format!("{prefix}-81d74748");
        assert_eq!(safe_name(&first), name);
        assert_eq!(safe_name(&second), name);
        assert_eq!(safe_name("Ops Café/📦"), "Ops_Caf___-6880d6da");
        let source = format!(
            "<connections><connection id='{first}'/><connection id='{second}'><protocol>UNRECOGNIZED</protocol></connection></connections>"
        );
        let scopes = MappedScopes::read(source.as_bytes(), |original| {
            (original == first).then_some(17)
        })
        .unwrap();
        assert!(scopes.names.is_empty());
        assert!(scopes.ids.is_empty());
    }

    #[test]
    fn java_ssh_memory_archive_requires_all_scopes_to_map() {
        let first = "9d29d591-deaa-49c8-8434-5a6ff6691014";
        let second = "b69da979-1495-478c-9662-14ee550255b0";
        let source = format!(
            "<connections><connection id='{first}'/><connection id='{second}'/></connections>"
        );
        let scopes = MappedScopes::read(source.as_bytes(), |id| {
            if id == first { Some(17) } else { Some(18) }
        })
        .unwrap();
        let incomplete =
            MappedScopes::read(source.as_bytes(), |id| (id == first).then_some(17)).unwrap();
        let marker = |id: &str| format!("{CONNECTION_SCOPE}{} -->", URL_SAFE_NO_PAD.encode(id));
        let archive = format!("{GLOBAL_MARKER}\n{}\r\n{}\n", marker(first), marker(second));
        let convert = |source: &str, scopes: &MappedScopes| {
            bind(ARCHIVE, Zeroizing::new(source.as_bytes().to_vec()), scopes)
        };
        assert!(convert(&archive, &incomplete).is_none());
        assert!(convert(&archive.replace("\r\n", "\r\r\n"), &scopes).is_none());
        let (path, mapped) = convert(&archive, &scopes).unwrap();
        assert_eq!(path, ARCHIVE);
        assert_eq!(
            mapped.as_slice(),
            format!("{GLOBAL_MARKER}\n{}\r\n{}\n", marker("17"), marker("18")).as_bytes()
        );
        let mut ids = Vec::new();
        validate_bound(&path, &mapped, |id| {
            ids.push(id);
            Ok(())
        })
        .unwrap();
        assert_eq!(ids, [17, 18]);
    }
}
