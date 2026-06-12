//! Native SFTP implementations of the remote file operations, backed by the
//! russh-sftp subsystem session obtained through `SSHSession::open_sftp()`.

use crate::sftp::{BINARY_OR_NON_TEXT_CODE, MAX_TEXT_EDITOR_BYTES};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, FileType as RemoteFileType, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub file_type: FileType,
    pub size: u64,
    pub modified: Option<String>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub permissions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileType {
    File,
    Directory,
    Symlink,
}

pub async fn list_dir(sftp: &SftpSession, path: &str) -> Result<Vec<FileEntry>, String> {
    let read_dir = sftp
        .read_dir(path)
        .await
        .map_err(|e| format!("Failed to list directory {path}: {e}"))?;
    Ok(read_dir
        .map(|entry| {
            let metadata = entry.metadata();
            file_entry_from_metadata(entry.file_name(), &metadata)
        })
        .collect())
}

pub async fn download(
    sftp: &SftpSession,
    remote_path: &str,
    local_path: &str,
) -> Result<(), String> {
    let mut remote = sftp
        .open_with_flags(remote_path, OpenFlags::READ)
        .await
        .map_err(|e| format!("Failed to open remote file {remote_path}: {e}"))?;
    if let Some(parent) = Path::new(local_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut local = tokio::fs::File::create(local_path)
        .await
        .map_err(|e| format!("Failed to create local file {local_path}: {e}"))?;
    tokio::io::copy(&mut remote, &mut local)
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    local
        .flush()
        .await
        .map_err(|e| format!("Failed to write local file {local_path}: {e}"))?;
    Ok(())
}

pub async fn upload(sftp: &SftpSession, local_path: &str, remote_path: &str) -> Result<(), String> {
    let mut local = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| format!("Failed to read local file: {e}"))?;
    let mut remote = sftp
        .open_with_flags(
            remote_path,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        )
        .await
        .map_err(|e| format!("Failed to open remote file {remote_path}: {e}"))?;
    tokio::io::copy(&mut local, &mut remote)
        .await
        .map_err(|e| format!("Upload failed: {e}"))?;
    // Drains outstanding write acknowledgements and closes the remote handle.
    remote
        .shutdown()
        .await
        .map_err(|e| format!("Upload failed while finalizing {remote_path}: {e}"))?;
    Ok(())
}

pub async fn read_text_file(sftp: &SftpSession, remote_path: &str) -> Result<String, String> {
    let metadata = sftp
        .metadata(remote_path)
        .await
        .map_err(|e| format!("Failed to stat remote file {remote_path}: {e}"))?;
    let size = metadata.len();
    if size > MAX_TEXT_EDITOR_BYTES {
        return Err(format!(
            "Remote file is too large for the snippet editor ({size} bytes)"
        ));
    }
    let data = sftp
        .read(remote_path)
        .await
        .map_err(|e| format!("Failed to read remote file {remote_path}: {e}"))?;
    String::from_utf8(data)
        .map_err(|e| format!("{BINARY_OR_NON_TEXT_CODE}: remote file is not valid UTF-8 text: {e}"))
}

pub async fn write_text_file(
    sftp: &SftpSession,
    remote_path: &str,
    content: &str,
) -> Result<(), String> {
    let mut remote = sftp
        .open_with_flags(
            remote_path,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        )
        .await
        .map_err(|e| format!("Failed to open remote file {remote_path}: {e}"))?;
    remote
        .write_all(content.as_bytes())
        .await
        .map_err(|e| format!("Failed to write remote file {remote_path}: {e}"))?;
    remote
        .shutdown()
        .await
        .map_err(|e| format!("Failed to finalize remote file {remote_path}: {e}"))?;
    Ok(())
}

/// Deletes a file or directory recursively.
///
/// Port of Java `SftpFileTransferService.deleteRemote`: directory contents are
/// removed depth-first before the directory itself. Unlike the Java original
/// (which used `stat`), this uses lstat semantics so a symlink to a directory
/// removes only the link and never recurses into the link target.
pub async fn delete(sftp: &SftpSession, path: &str) -> Result<(), String> {
    delete_recursive(sftp, path.to_string()).await
}

fn delete_recursive<'a>(
    sftp: &'a SftpSession,
    path: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let metadata = sftp
            .symlink_metadata(&path)
            .await
            .map_err(|e| format!("Failed to stat {path}: {e}"))?;
        if metadata.file_type().is_dir() {
            let children = sftp
                .read_dir(&path)
                .await
                .map_err(|e| format!("Failed to list {path}: {e}"))?;
            for child in children {
                let name = child.file_name();
                // ReadDir already filters "." and ".."; keep the guard for parity with Java.
                if name == "." || name == ".." {
                    continue;
                }
                delete_recursive(sftp, append_remote_name(&path, &name)).await?;
            }
            sftp.remove_dir(&path)
                .await
                .map_err(|e| format!("Failed to remove directory {path}: {e}"))?;
        } else {
            sftp.remove_file(&path)
                .await
                .map_err(|e| format!("Failed to remove file {path}: {e}"))?;
        }
        Ok(())
    })
}

pub async fn rename(sftp: &SftpSession, old_path: &str, new_path: &str) -> Result<(), String> {
    sftp.rename(old_path, new_path)
        .await
        .map_err(|e| format!("Failed to rename {old_path} to {new_path}: {e}"))
}

/// Creates a directory with `mkdir -p` semantics: every missing path component
/// is created iteratively; existing directories along the way are tolerated.
pub async fn mkdir(sftp: &SftpSession, path: &str) -> Result<(), String> {
    for prefix in mkdir_p_prefixes(path) {
        match sftp.metadata(prefix.clone()).await {
            Ok(existing) => {
                if !existing.file_type().is_dir() {
                    return Err(format!(
                        "Cannot create directory, path exists and is not a directory: {prefix}"
                    ));
                }
            }
            Err(_) => {
                if let Err(error) = sftp.create_dir(prefix.clone()).await {
                    // Tolerate races where the directory appeared in the meantime.
                    match sftp.metadata(prefix.clone()).await {
                        Ok(existing) if existing.file_type().is_dir() => {}
                        _ => {
                            return Err(format!("Failed to create directory {prefix}: {error}"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Sets numeric permissions (chmod) while keeping the existing file type bits.
pub async fn chmod(sftp: &SftpSession, path: &str, mode: u32) -> Result<(), String> {
    let existing = sftp
        .metadata(path)
        .await
        .map_err(|e| format!("Failed to stat {path}: {e}"))?;
    // Only the permissions attribute is sent; sending size/uid/gid in SETSTAT
    // could truncate the file or fail on servers that restrict chown.
    let mut attrs = FileAttributes::empty();
    attrs.permissions = Some(merge_permission_bits(existing.permissions, mode));
    sftp.set_metadata(path, attrs)
        .await
        .map_err(|e| format!("Failed to change permissions of {path}: {e}"))
}

fn file_entry_from_metadata(name: String, metadata: &FileAttributes) -> FileEntry {
    let file_type = match metadata.file_type() {
        RemoteFileType::Dir => FileType::Directory,
        RemoteFileType::Symlink => FileType::Symlink,
        _ => FileType::File,
    };
    let owner = metadata
        .user
        .clone()
        .filter(|user| !user.is_empty())
        .or_else(|| metadata.uid.map(|uid| uid.to_string()));
    let group = metadata
        .group
        .clone()
        .filter(|group| !group.is_empty())
        .or_else(|| metadata.gid.map(|gid| gid.to_string()));
    FileEntry {
        name,
        file_type,
        size: metadata.size.unwrap_or(0),
        modified: metadata.mtime.map(format_modified_timestamp),
        owner,
        group,
        permissions: metadata.permissions.map(format_ls_permissions),
    }
}

fn format_modified_timestamp(mtime: u32) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(i64::from(mtime), 0)
        .map(|timestamp| {
            timestamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Formats a numeric mode as an ls-style permission string ("drwxr-xr-x"),
/// including setuid/setgid/sticky markers.
pub(crate) fn format_ls_permissions(mode: u32) -> String {
    let type_char = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o140000 => 's',
        0o060000 => 'b',
        0o020000 => 'c',
        0o010000 => 'p',
        _ => '-',
    };
    let mut out = String::with_capacity(10);
    out.push(type_char);
    push_rwx_triplet(&mut out, mode >> 6, mode & 0o4000 != 0, 's');
    push_rwx_triplet(&mut out, mode >> 3, mode & 0o2000 != 0, 's');
    push_rwx_triplet(&mut out, mode, mode & 0o1000 != 0, 't');
    out
}

fn push_rwx_triplet(out: &mut String, shifted_mode: u32, special: bool, special_char: char) {
    out.push(if shifted_mode & 0o4 != 0 { 'r' } else { '-' });
    out.push(if shifted_mode & 0o2 != 0 { 'w' } else { '-' });
    out.push(match (shifted_mode & 0o1 != 0, special) {
        (true, false) => 'x',
        (false, false) => '-',
        (true, true) => special_char,
        (false, true) => special_char.to_ascii_uppercase(),
    });
}

/// Joins a remote directory path and a child name.
/// Port of Java `SftpFileTransferService.appendRemoteName`.
pub(crate) fn append_remote_name(base_path: &str, name: &str) -> String {
    if base_path.trim().is_empty() {
        return name.to_string();
    }
    if base_path == "/" {
        return format!("/{name}");
    }
    if base_path.ends_with('/') {
        format!("{base_path}{name}")
    } else {
        format!("{base_path}/{name}")
    }
}

/// Expands a path into the list of prefixes that `mkdir -p` would create,
/// shortest first. Empty and "." components are skipped.
pub(crate) fn mkdir_p_prefixes(path: &str) -> Vec<String> {
    let trimmed = path.trim();
    let absolute = trimmed.starts_with('/');
    let mut prefixes = Vec::new();
    let mut current = String::new();
    for part in trimmed.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if current.is_empty() && !absolute {
            current = part.to_string();
        } else {
            current = format!("{current}/{part}");
        }
        prefixes.push(current.clone());
    }
    prefixes
}

/// Combines an existing mode with new permission bits, keeping non-permission
/// (file type) bits of the existing mode.
pub(crate) fn merge_permission_bits(existing: Option<u32>, mode: u32) -> u32 {
    (existing.unwrap_or(0) & !0o7777) | (mode & 0o7777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_regular_file_permissions() {
        assert_eq!(format_ls_permissions(0o100644), "-rw-r--r--");
        assert_eq!(format_ls_permissions(0o100755), "-rwxr-xr-x");
        assert_eq!(format_ls_permissions(0o100000), "----------");
    }

    #[test]
    fn formats_directory_and_symlink_permissions() {
        assert_eq!(format_ls_permissions(0o040755), "drwxr-xr-x");
        assert_eq!(format_ls_permissions(0o120777), "lrwxrwxrwx");
    }

    #[test]
    fn formats_setuid_setgid_and_sticky_bits() {
        assert_eq!(format_ls_permissions(0o104755), "-rwsr-xr-x");
        assert_eq!(format_ls_permissions(0o102644), "-rw-r-Sr--");
        assert_eq!(format_ls_permissions(0o041777), "drwxrwxrwt");
        assert_eq!(format_ls_permissions(0o041666), "drw-rw-rwT");
    }

    #[test]
    fn formats_permissions_without_type_bits_as_regular_file() {
        assert_eq!(format_ls_permissions(0o644), "-rw-r--r--");
    }

    #[test]
    fn append_remote_name_matches_java_semantics() {
        assert_eq!(append_remote_name("", "file"), "file");
        assert_eq!(append_remote_name("   ", "file"), "file");
        assert_eq!(append_remote_name("/", "file"), "/file");
        assert_eq!(append_remote_name("/var", "file"), "/var/file");
        assert_eq!(append_remote_name("/var/", "file"), "/var/file");
        assert_eq!(append_remote_name("relative", "file"), "relative/file");
    }

    #[test]
    fn recursive_delete_builds_nested_child_paths() {
        let root = "/data/project";
        let child_dir = append_remote_name(root, "src");
        let nested_file = append_remote_name(&child_dir, "main.rs");

        assert_eq!(child_dir, "/data/project/src");
        assert_eq!(nested_file, "/data/project/src/main.rs");
    }

    #[test]
    fn mkdir_p_prefixes_for_absolute_path() {
        assert_eq!(
            mkdir_p_prefixes("/a/b/c"),
            vec!["/a".to_string(), "/a/b".to_string(), "/a/b/c".to_string()]
        );
    }

    #[test]
    fn mkdir_p_prefixes_for_relative_path() {
        assert_eq!(
            mkdir_p_prefixes("a/b"),
            vec!["a".to_string(), "a/b".to_string()]
        );
    }

    #[test]
    fn mkdir_p_prefixes_skips_empty_and_dot_components() {
        assert_eq!(
            mkdir_p_prefixes("//a//./b/"),
            vec!["/a".to_string(), "/a/b".to_string()]
        );
        assert!(mkdir_p_prefixes("/").is_empty());
        assert!(mkdir_p_prefixes("").is_empty());
    }

    #[test]
    fn merge_permission_bits_keeps_type_bits_and_applies_mode() {
        assert_eq!(merge_permission_bits(Some(0o100644), 0o755), 0o100755);
        assert_eq!(merge_permission_bits(Some(0o040700), 0o2775), 0o042775);
        assert_eq!(merge_permission_bits(None, 0o644), 0o644);
        // Type bits inside the requested mode are ignored.
        assert_eq!(merge_permission_bits(Some(0o100644), 0o100755), 0o100755);
    }

    #[test]
    fn file_entry_prefers_names_and_falls_back_to_numeric_ids() {
        let mut metadata = FileAttributes::empty();
        metadata.size = Some(42);
        metadata.permissions = Some(0o100640);
        metadata.uid = Some(1000);
        metadata.gid = Some(100);
        metadata.mtime = Some(1_700_000_000);

        let entry = file_entry_from_metadata("file.txt".into(), &metadata);
        assert_eq!(entry.owner.as_deref(), Some("1000"));
        assert_eq!(entry.group.as_deref(), Some("100"));
        assert_eq!(entry.size, 42);
        assert!(matches!(entry.file_type, FileType::File));
        assert_eq!(entry.permissions.as_deref(), Some("-rw-r-----"));
        let modified = entry.modified.expect("mtime should be formatted");
        assert_eq!(modified.len(), "2023-11-14 23:13".len());

        metadata.user = Some("daniel".into());
        metadata.group = Some("staff".into());
        let entry = file_entry_from_metadata("file.txt".into(), &metadata);
        assert_eq!(entry.owner.as_deref(), Some("daniel"));
        assert_eq!(entry.group.as_deref(), Some("staff"));
    }

    #[test]
    fn file_entry_maps_directory_and_symlink_types() {
        let mut metadata = FileAttributes::empty();
        metadata.permissions = Some(0o040755);
        let entry = file_entry_from_metadata("dir".into(), &metadata);
        assert!(matches!(entry.file_type, FileType::Directory));

        metadata.permissions = Some(0o120777);
        let entry = file_entry_from_metadata("link".into(), &metadata);
        assert!(matches!(entry.file_type, FileType::Symlink));
    }
}
