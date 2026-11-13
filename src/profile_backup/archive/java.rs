//! Member classification and ZIP decoding for Java kortty backups.

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read};
use std::path::Path;

use kortty_vault::Secret;
use unicode_normalization::UnicodeNormalization as _;
use zeroize::Zeroizing;
use zip::{AesMode, CompressionMethod, ZipArchive};

use super::{
    MAX_ENTRIES, MAX_FILE_BYTES, MAX_PATH_BYTES, MAX_PAYLOAD_BYTES, invalid, invalid_component,
    require_eof, u16_at, u32_at,
};

const EOCD_BYTES: usize = 22;
pub(in crate::profile_backup) const MAX_ZIP_BYTES: usize = MAX_PAYLOAD_BYTES + 16 * 1024 * 1024;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::profile_backup) enum Protection {
    Aes256,
    LegacyZipCrypto,
    OpenPgp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::profile_backup) enum MemberKind {
    Connections,
    Credentials,
    OpenPgpKeys,
    SshKeyCatalog,
    GlobalSettings,
    Scheduler,
    MasterKey,
    HostPins,
    Snippets,
    SnippetVariables,
    AiChats,
    LlamaModels,
    KnowledgeStores,
    Themes,
    Environments,
    SwarmChats,
    MlxModels,
    Project,
    SshKeyFile,
    SnippetAnalysis,
    Memory,
    SkillStatistics,
}

pub(in crate::profile_backup) struct Member {
    pub kind: MemberKind,
    pub bytes: Zeroizing<Vec<u8>>,
}

/// Validated Java data. Conversion into native stores is a separate step.
pub(in crate::profile_backup) struct Members {
    pub protection: Protection,
    pub members: BTreeMap<String, Member>,
    pub unsupported: Vec<String>,
}

pub(in crate::profile_backup) fn read_password_zip(
    source: &Path,
    password: &Secret,
) -> io::Result<Members> {
    let source = crate::profile::open_preference(source)?;
    let bytes = read_zip(&source)?;
    decode_password_zip(&bytes, password)
}

/// Reads the ZIP carried inside a fully integrity-checked OpenPGP message.
pub(in crate::profile_backup) fn decode_openpgp_zip(bytes: &[u8]) -> io::Result<Members> {
    decode_zip(bytes, None)
}

pub(in crate::profile_backup) fn recognizes(source: &std::fs::File) -> io::Result<bool> {
    let bytes = read_zip(source)?;
    let (archive, count) = open_zip(&bytes)?;
    for index in 0..count {
        let header = archive.by_index_data(index)?;
        let path = std::str::from_utf8(header.name_raw()).ok();
        if header.encrypted() && path.and_then(classify).is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_zip(mut source: &std::fs::File) -> io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};

    let metadata = source.metadata()?;
    if !metadata.is_file() || !(EOCD_BYTES as u64..=MAX_ZIP_BYTES as u64).contains(&metadata.len())
    {
        return Err(invalid("invalid Java profile backup file size or type"));
    }
    source.seek(SeekFrom::Start(0))?;
    let mut bytes = vec![0; metadata.len() as usize];
    source.read_exact(&mut bytes)?;
    require_eof(&mut source)?;
    Ok(bytes)
}

fn decode_password_zip(bytes: &[u8], password: &Secret) -> io::Result<Members> {
    if password.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The Java backup password is empty.",
        ));
    }

    decode_zip(bytes, Some(password))
}

fn decode_zip(bytes: &[u8], password: Option<&Secret>) -> io::Result<Members> {
    let (mut archive, count) = open_zip(bytes)?;

    let mut paths = BTreeMap::new();
    let mut entries = Vec::with_capacity(count);
    let mut remaining = MAX_PAYLOAD_BYTES;
    let mut protection = password.is_none().then_some(Protection::OpenPgp);
    for index in 0..count {
        let header = archive.by_index_data(index)?;
        let raw_name = std::str::from_utf8(header.name_raw())
            .map_err(|_| invalid("invalid UTF-8 path in Java profile backup"))?;
        if password.is_none() && raw_name == "profile.snapshot" {
            return Err(invalid("unexpected native snapshot in Java backup ZIP"));
        }
        if raw_name != header.name()?.as_ref() {
            return Err(invalid("conflicting ZIP paths in Java profile backup"));
        }
        let directory = header.is_dir();
        let path = if directory {
            raw_name.strip_suffix('/').ok_or_else(unsafe_path)?
        } else {
            raw_name
        };
        if path.len() > MAX_PATH_BYTES || path.split('/').any(invalid_component) {
            return Err(unsafe_path());
        }
        let portable_path: String = path.nfd().flat_map(char::to_lowercase).collect();
        if paths.insert(portable_path, directory).is_some() {
            return Err(invalid("duplicate or aliased path in Java profile backup"));
        }

        let file_type = header.unix_mode().unwrap_or_default() & 0o170000;
        if file_type != 0 && file_type != if directory { 0o040000 } else { 0o100000 } {
            return Err(invalid("unsupported ZIP entry type in Java profile backup"));
        }
        if directory {
            if header.size() != 0 || header.compressed_size() != 0 || header.encrypted() {
                return Err(invalid("invalid directory marker in Java profile backup"));
            }
            continue;
        }
        if header.encrypted() != password.is_some() {
            return Err(invalid("unexpected ZIP file protection in Java backup"));
        }
        if !matches!(
            header.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(invalid("unsupported compression in Java profile backup"));
        }
        let size = usize::try_from(header.size())
            .ok()
            .filter(|size| *size <= MAX_FILE_BYTES && *size <= remaining)
            .ok_or_else(|| invalid("Java profile backup exceeds the expanded size limit"))?;
        remaining -= size;
        entries.push((index, path.to_owned(), size));

        if password.is_some() {
            let member_protection = match archive.get_aes_verification_key_and_salt(index)? {
                Some(info) if info.aes_mode == AesMode::Aes256 => Protection::Aes256,
                Some(_) => return Err(invalid("unsupported AES strength in Java profile backup")),
                None => Protection::LegacyZipCrypto,
            };
            if protection
                .replace(member_protection)
                .is_some_and(|kind| kind != member_protection)
            {
                return Err(invalid("mixed file encryption in Java profile backup"));
            }
        }
    }

    for path in paths.keys() {
        if path
            .match_indices('/')
            .any(|(offset, _)| paths.get(&path[..offset]) == Some(&false))
        {
            return Err(invalid(
                "Java profile backup path conflicts with a parent file",
            ));
        }
    }

    let mut members = BTreeMap::new();
    let mut unsupported = Vec::new();
    for (index, path, size) in entries {
        let mut input = match password {
            Some(password) => archive.by_index_decrypt(index, password.as_bytes())?,
            None => archive.by_index(index)?,
        };
        let mut bytes = Zeroizing::new(vec![0; size]);
        input.read_exact(&mut bytes)?;
        // For both stored and deflated entries, force the integrity check at EOF.
        require_eof(&mut input)?;
        if let Some(kind) = classify(&path) {
            members.insert(path, Member { kind, bytes });
        } else {
            unsupported.push(path);
        }
    }
    if members.is_empty() {
        return Err(invalid(
            "the ZIP contains no known Java profile backup members",
        ));
    }
    let protection =
        protection.ok_or_else(|| invalid("the Java profile backup contains no files"))?;
    Ok(Members {
        protection,
        members,
        unsupported,
    })
}

fn validate_footer(bytes: &[u8]) -> io::Result<usize> {
    let invalid = || invalid("invalid Java profile backup ZIP directory");
    let footer_start = bytes.len().checked_sub(EOCD_BYTES).ok_or_else(invalid)?;
    let footer = &bytes[footer_start..];
    let count = usize::from(u16_at(footer, 10));
    let directory_size = u32_at(footer, 12);
    let directory_start = u32_at(footer, 16);
    // Java's producer writes a single-disk ZIP without comments. The archive
    // and file-count bounds keep both file data and directory offsets below ZIP64 limits.
    if bytes.len() > MAX_ZIP_BYTES
        || !bytes.starts_with(b"PK\x03\x04")
        || !footer.starts_with(b"PK\x05\x06")
        || footer[4..8] != [0; 4]
        || footer[20..] != [0; 2]
        || usize::from(u16_at(footer, 8)) != count
        || !(1..=MAX_ENTRIES).contains(&count)
        || directory_start.checked_add(directory_size) != Some(footer_start)
    {
        return Err(invalid());
    }
    Ok(count)
}

fn open_zip(bytes: &[u8]) -> io::Result<(ZipArchive<Cursor<&[u8]>>, usize)> {
    let count = validate_footer(bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    if archive.offset() != 0 || archive.len() != count {
        return Err(invalid("invalid Java profile backup ZIP structure"));
    }
    archive.validate_header_consistency()?;
    Ok((archive, count))
}

fn unsafe_path() -> io::Error {
    invalid("unsafe relative path in Java profile backup")
}

fn classify(path: &str) -> Option<MemberKind> {
    use MemberKind::*;

    Some(match path {
        "connections.xml" => Connections,
        "credentials.xml" => Credentials,
        "gpg-keys.xml" => OpenPgpKeys,
        "ssh-keys.xml" => SshKeyCatalog,
        "global-settings.xml" => GlobalSettings,
        "job-scheduler.xml" => Scheduler,
        "master.key" => MasterKey,
        "ssh-host-keys.properties" => HostPins,
        "snippets.xml" => Snippets,
        "snippet-variables.xml" => SnippetVariables,
        "ai-chats.xml" => AiChats,
        "llm/models.xml" => LlamaModels,
        "rag/stores.json" => KnowledgeStores,
        "themes.xml" => Themes,
        "environments.xml" => Environments,
        "swarm-chats.xml" => SwarmChats,
        "llm/mlx-models.json" => MlxModels,
        _ => match path.split_once('/')?.0 {
            "projects" => Project,
            "ssh-keys" => SshKeyFile,
            "snippet-analyses" => SnippetAnalysis,
            "memory" => Memory,
            "stats" => SkillStatistics,
            _ => return None,
        },
    })
}
