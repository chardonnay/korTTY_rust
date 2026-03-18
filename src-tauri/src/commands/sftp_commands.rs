use crate::sftp::manager::{FileEntry, FileType};
use crate::ssh::SSHManager;
use base64::Engine as _;
use std::fs;
use std::path::Path;
use tauri::State;

#[tauri::command]
pub async fn get_home_dir() -> Result<String, String> {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine home directory".to_string())
}

#[tauri::command]
pub async fn list_local_dir(path: String) -> Result<Vec<FileEntry>, String> {
    let path_buf = Path::new(&path);
    if !path_buf.exists() {
        return Err(format!("Path does not exist: {}", path));
    }
    if !path_buf.is_dir() {
        return Err(format!("Not a directory: {}", path));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = if meta.is_dir() {
            FileType::Directory
        } else if meta.is_symlink() {
            FileType::Symlink
        } else {
            FileType::File
        };
        let modified = meta.modified().ok().map(|t| format!("{:?}", t));
        let (owner, group, permissions) = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                (
                    Some(meta.uid().to_string()),
                    Some(meta.gid().to_string()),
                    Some(format!("{:o}", meta.mode() & 0o777)),
                )
            }
            #[cfg(not(unix))]
            {
                (None, None, None)
            }
        };
        entries.push(FileEntry {
            name,
            file_type,
            size: meta.len(),
            modified,
            owner,
            group,
            permissions,
        });
    }
    Ok(entries)
}

fn parse_ls_output(output: &str) -> Vec<FileEntry> {
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

#[tauri::command]
pub async fn sftp_list_dir(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
) -> Result<Vec<FileEntry>, String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let command = format!(
        "LC_ALL=C ls -la --time-style=long-iso {} 2>/dev/null || LC_ALL=C ls -la {}",
        shell_escape(&path),
        shell_escape(&path)
    );
    let output = session
        .exec_command(&command)
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_ls_output(&output))
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

const UPLOAD_CHUNK_SIZE: usize = 32 * 1024;

#[tauri::command]
pub async fn sftp_upload(
    state: State<'_, SSHManager>,
    session_id: String,
    local_path: String,
    remote_path: String,
) -> Result<(), String> {
    let data = fs::read(&local_path).map_err(|e| format!("Failed to read local file: {}", e))?;
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;

    let escaped = shell_escape(&remote_path);
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

#[tauri::command]
pub async fn sftp_download(
    state: State<'_, SSHManager>,
    session_id: String,
    remote_path: String,
    local_path: String,
) -> Result<(), String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let cmd = format!("cat {} | base64", shell_escape(&remote_path));
    let output = session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    let cleaned: String = output.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err("Download failed: empty response (file may not exist or is empty)".to_string());
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .map_err(|e| format!("Base64 decode error: {}", e))?;
    if let Some(parent) = Path::new(&local_path).parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&local_path, &data).map_err(|e| format!("Failed to write local file: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn sftp_delete(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let cmd = format!("rm -rf {}", shell_escape(&path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, SSHManager>,
    session_id: String,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let cmd = format!("mv {} {}", shell_escape(&old_path), shell_escape(&new_path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn sftp_chmod(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
    mode: u32,
) -> Result<(), String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let cmd = format!("chmod {:o} {}", mode, shell_escape(&path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn sftp_mkdir(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let cmd = format!("mkdir -p {}", shell_escape(&path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn sftp_chown(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
    owner: String,
    group: String,
    recursive: bool,
) -> Result<(), String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let flag = if recursive { "-R " } else { "" };
    let owner_group = if group.is_empty() {
        owner
    } else {
        format!("{}:{}", owner, group)
    };
    let cmd = format!("chown {}{} {}", flag, owner_group, shell_escape(&path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn sftp_chmod_str(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
    mode: String,
    recursive: bool,
) -> Result<(), String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let flag = if recursive { "-R " } else { "" };
    let cmd = format!("chmod {}{} {}", flag, mode, shell_escape(&path));
    session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveToolsAvailable {
    pub zip: bool,
    pub tar_bz2: bool,
    pub seven_zip: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateArchiveRequest {
    pub format: String,
    pub archive_path: String,
    pub files: Vec<String>,
    pub base_dir: String,
    pub compression: u32,
    pub password: Option<String>,
    pub owner: Option<String>,
    pub permissions: Option<String>,
}

#[tauri::command]
pub async fn sftp_check_archive_tools(
    state: State<'_, SSHManager>,
    session_id: String,
) -> Result<ArchiveToolsAvailable, String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;
    let cmd = "echo ZIP=$(which zip 2>/dev/null && echo ok || echo no) TAR=$(which tar 2>/dev/null && echo ok || echo no) 7Z=$(which 7z 2>/dev/null || which 7za 2>/dev/null && echo ok || echo no)";
    let output = session.exec_command(cmd).await.map_err(|e| e.to_string())?;
    Ok(ArchiveToolsAvailable {
        zip: output.contains("ZIP=") && output.contains("zip") && output.contains("ok"),
        tar_bz2: output.contains("TAR=") && output.contains("tar") && output.contains("ok"),
        seven_zip: output.contains("7Z=") && (output.contains("7z") || output.contains("7za")),
    })
}

#[tauri::command]
pub async fn sftp_create_archive(
    state: State<'_, SSHManager>,
    session_id: String,
    request: CreateArchiveRequest,
) -> Result<String, String> {
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;
    let session = session_arc.lock().await;

    let file_args = request
        .files
        .iter()
        .map(|f| shell_escape(f))
        .collect::<Vec<_>>()
        .join(" ");

    let cmd = match request.format.as_str() {
        "zip" => {
            let pw = request
                .password
                .filter(|p| !p.is_empty())
                .map(|p| format!("-P {}", shell_escape(&p)))
                .unwrap_or_default();
            format!(
                "cd {} && zip -r -{} {} {} {}",
                shell_escape(&request.base_dir),
                request.compression,
                pw,
                shell_escape(&request.archive_path),
                file_args
            )
        }
        "tar.bz2" => {
            format!(
                "cd {} && BZIP2=-{} tar -cjf {} {}",
                shell_escape(&request.base_dir),
                request.compression,
                shell_escape(&request.archive_path),
                file_args
            )
        }
        "7z" => {
            let pw = request
                .password
                .filter(|p| !p.is_empty())
                .map(|p| format!("-p{}", shell_escape(&p)))
                .unwrap_or_default();
            let tool = session
                .exec_command("which 7z 2>/dev/null || which 7za 2>/dev/null")
                .await
                .map_err(|e| e.to_string())?
                .trim()
                .to_string();
            let tool = if tool.is_empty() {
                "7z".to_string()
            } else {
                tool
            };
            format!(
                "cd {} && {} a -mx={} {} {} {}",
                shell_escape(&request.base_dir),
                tool,
                request.compression,
                pw,
                shell_escape(&request.archive_path),
                file_args
            )
        }
        _ => return Err(format!("Unknown format: {}", request.format)),
    };

    let output = session
        .exec_command(&cmd)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(own) = request.owner.filter(|o| !o.is_empty()) {
        let chown_cmd = format!("chown {} {}", own, shell_escape(&request.archive_path));
        let _ = session.exec_command(&chown_cmd).await;
    }
    if let Some(perm) = request.permissions.filter(|p| !p.is_empty()) {
        let chmod_cmd = format!("chmod {} {}", perm, shell_escape(&request.archive_path));
        let _ = session.exec_command(&chmod_cmd).await;
    }

    let size_output = session
        .exec_command(&format!(
            "du -h {} | cut -f1",
            shell_escape(&request.archive_path)
        ))
        .await
        .unwrap_or_default();

    Ok(format!(
        "Archive created: {} ({})\n{}",
        request.archive_path,
        size_output.trim(),
        output
    ))
}
