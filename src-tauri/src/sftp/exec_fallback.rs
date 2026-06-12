//! Exec-based fallback for remote file operations.
//!
//! Used when the SFTP subsystem of a session cannot be started (e.g. the server
//! rejects or closes the subsystem after successful SSH authentication). All
//! operations run shell commands over SSH exec channels and transport binary
//! data base64-encoded. The behaviour matches the original exec-based
//! implementations of the SFTP commands.

use crate::sftp::manager::{FileEntry, FileType};
use crate::sftp::MAX_TEXT_EDITOR_BYTES;
use crate::ssh::session::SSHSession;
use base64::Engine as _;
use std::fs;
use std::path::Path;

const UPLOAD_CHUNK_SIZE: usize = 32 * 1024;

/// Quotes a string for safe use as a single shell word.
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub(crate) fn parse_ls_output(output: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("total") {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let perms = parts[0];
        let owner = parts[2].to_string();
        let group = parts[3].to_string();
        let size: u64 = parts[4].parse().unwrap_or(0);
        let date = format!("{} {} {}", parts[5], parts[6], parts[7]);
        let name_raw = parts[8..].join(" ");

        let name = if perms.starts_with('l') {
            name_raw
                .split(" -> ")
                .next()
                .unwrap_or(&name_raw)
                .to_string()
        } else {
            name_raw
        };

        if name == "." || name == ".." {
            continue;
        }

        let file_type = if perms.starts_with('d') {
            FileType::Directory
        } else if perms.starts_with('l') {
            FileType::Symlink
        } else {
            FileType::File
        };

        entries.push(FileEntry {
            name,
            file_type,
            size,
            modified: Some(date),
            owner: Some(owner),
            group: Some(group),
            permissions: Some(perms.to_string()),
        });
    }
    entries
}

pub async fn list_dir(session: &SSHSession, path: &str) -> Result<Vec<FileEntry>, String> {
    let command = format!(
        "LC_ALL=C ls -la --time-style=long-iso {} 2>/dev/null || LC_ALL=C ls -la {}",
        shell_escape(path),
        shell_escape(path)
    );
    let output = session
        .exec_command(&command)
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_ls_output(&output))
}

pub async fn upload(
    session: &SSHSession,
    local_path: &str,
    remote_path: &str,
) -> Result<(), String> {
    let data = fs::read(local_path).map_err(|e| format!("Failed to read local file: {}", e))?;
    let escaped = shell_escape(remote_path);
    for (i, chunk) in data.chunks(UPLOAD_CHUNK_SIZE).enumerate() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        let redirect = if i == 0 { ">" } else { ">>" };
        let cmd = format!(
            "printf '%s' '{}' | {{ base64 -d 2>/dev/null || base64 -D; }} {} {}",
            b64, redirect, escaped
        );
        session
            .exec_command(&cmd)
            .await
            .map_err(|e| format!("Upload chunk {} failed: {}", i, e))?;
    }
    Ok(())
}

pub async fn download(
    session: &SSHSession,
    remote_path: &str,
    local_path: &str,
) -> Result<(), String> {
    let cmd = format!("cat {} | base64", shell_escape(remote_path));
    let output = session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    let cleaned: String = output.chars().filter(|c| !c.is_whitespace()).collect();
    // A 0-byte remote file produces empty base64 output. That is a valid result
    // (Java SFTPSession#downloadFile writes a 0-byte local file), so treat empty
    // output as empty data and write an empty local file rather than failing.
    let data = if cleaned.is_empty() {
        Vec::new()
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(&cleaned)
            .map_err(|e| format!("Base64 decode error: {}", e))?
    };
    if let Some(parent) = Path::new(local_path).parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(local_path, &data).map_err(|e| format!("Failed to write local file: {}", e))?;
    Ok(())
}

pub async fn read_remote_text_file(
    session: &SSHSession,
    remote_path: &str,
) -> Result<String, String> {
    let size_cmd = format!("wc -c < {} 2>/dev/null", shell_escape(remote_path));
    let size_output = session
        .exec_command(&size_cmd)
        .await
        .map_err(|e| e.to_string())?;
    let size = size_output.trim().parse::<u64>().unwrap_or(0);
    if size > MAX_TEXT_EDITOR_BYTES {
        return Err(format!(
            "Remote file is too large for the snippet editor ({size} bytes)"
        ));
    }
    let cmd = format!("cat {} | base64", shell_escape(remote_path));
    let output = session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    let cleaned: String = output.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Ok(String::new());
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .map_err(|e| format!("Base64 decode error: {}", e))?;
    String::from_utf8(data).map_err(|e| format!("Remote file is not valid UTF-8: {}", e))
}

pub async fn write_remote_text_file(
    session: &SSHSession,
    remote_path: &str,
    content: &str,
) -> Result<(), String> {
    let escaped = shell_escape(remote_path);
    for (i, chunk) in content.as_bytes().chunks(UPLOAD_CHUNK_SIZE).enumerate() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        let redirect = if i == 0 { ">" } else { ">>" };
        let cmd = format!(
            "printf '%s' '{}' | {{ base64 -d 2>/dev/null || base64 -D; }} {} {}",
            b64, redirect, escaped
        );
        session
            .exec_command(&cmd)
            .await
            .map_err(|e| format!("Remote write chunk {} failed: {}", i, e))?;
    }
    if content.is_empty() {
        session
            .exec_command(&format!(": > {}", escaped))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn delete(session: &SSHSession, path: &str) -> Result<(), String> {
    let cmd = format!("rm -rf {}", shell_escape(path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn rename(session: &SSHSession, old_path: &str, new_path: &str) -> Result<(), String> {
    let cmd = format!("mv {} {}", shell_escape(old_path), shell_escape(new_path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn chmod(session: &SSHSession, path: &str, mode: u32) -> Result<(), String> {
    let cmd = format!("chmod {:o} {}", mode, shell_escape(path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn mkdir(session: &SSHSession, path: &str) -> Result<(), String> {
    let cmd = format!("mkdir -p {}", shell_escape(path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_wraps_in_single_quotes_and_escapes_quotes() {
        assert_eq!(shell_escape("plain"), "'plain'");
        assert_eq!(shell_escape("with space"), "'with space'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn parse_ls_output_extracts_entries_and_skips_dot_dirs() {
        let output = "total 12\n\
drwxr-xr-x  2 root root 4096 Jan 15 12:30 .\n\
drwxr-xr-x 18 root root 4096 Jan 15 12:30 ..\n\
-rw-r--r--  1 user staff 1234 Jan 15 12:34 file with space.txt\n\
lrwxrwxrwx  1 user staff    7 Jan 15 12:35 link -> target\n\
drwxr-xr-x  2 user staff 4096 Jan 15 12:36 folder\n";

        let entries = parse_ls_output(output);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "file with space.txt");
        assert!(matches!(entries[0].file_type, FileType::File));
        assert_eq!(entries[0].size, 1234);
        assert_eq!(entries[1].name, "link");
        assert!(matches!(entries[1].file_type, FileType::Symlink));
        assert_eq!(entries[2].name, "folder");
        assert!(matches!(entries[2].file_type, FileType::Directory));
        assert_eq!(entries[2].permissions.as_deref(), Some("drwxr-xr-x"));
        assert_eq!(entries[2].owner.as_deref(), Some("user"));
        assert_eq!(entries[2].group.as_deref(), Some("staff"));
    }
}
