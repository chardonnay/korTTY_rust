use crate::sftp::exec_fallback::{self, shell_escape};
use crate::sftp::manager::{self, FileEntry, FileType};
use crate::sftp::paths;
use crate::sftp::{
    SftpModeReason, SftpModeStatus, SftpModeStore, SftpTransferMode, MAX_TEXT_EDITOR_BYTES,
};
use crate::ssh::session::SSHSession;
use crate::ssh::SSHManager;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

/// Tauri event emitted when the transfer mode of a session is first determined
/// (and again only when it changes, e.g. after a reconnect).
const SFTP_MODE_EVENT: &str = "kortty-sftp-mode";

/// Transfer backend resolved for one remote file operation.
enum TransferBackend {
    /// Native SFTP subsystem; the Arc is independent of the session lock.
    Sftp(Arc<russh_sftp::client::SftpSession>),
    /// Exec + base64 command-line fallback.
    ExecFallback,
}

async fn get_session(
    state: &State<'_, SSHManager>,
    session_id: &str,
) -> Result<Arc<Mutex<SSHSession>>, String> {
    state
        .get_session(session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())
}

/// Resolves the transfer backend for a session: tries the SFTP subsystem first
/// and falls back to the exec transport on startup errors. The open_sftp()
/// outcome is cached inside the session, so a rejected subsystem is not retried
/// on every operation. Reports the determined mode via the
/// "kortty-sftp-mode" event when it is first established or changes.
async fn resolve_transfer_backend(
    app: &AppHandle,
    mode_store: &SftpModeStore,
    session: &SSHSession,
    session_id: &str,
) -> TransferBackend {
    let outcome = session.open_sftp().await;
    let status = match &outcome {
        Ok(_) => SftpModeStatus {
            session_id: session_id.to_string(),
            mode: SftpTransferMode::Sftp,
            reason: None,
        },
        Err(error) => SftpModeStatus {
            session_id: session_id.to_string(),
            mode: SftpTransferMode::ExecFallback,
            reason: Some(SftpModeReason::from_startup_error(error)),
        },
    };
    report_transfer_mode(app, mode_store, status).await;
    match outcome {
        Ok(sftp) => TransferBackend::Sftp(sftp),
        Err(_) => TransferBackend::ExecFallback,
    }
}

async fn report_transfer_mode(app: &AppHandle, mode_store: &SftpModeStore, status: SftpModeStatus) {
    let mut modes = mode_store.0.lock().await;
    let unchanged = modes
        .get(&status.session_id)
        .map(|previous| previous.mode == status.mode)
        .unwrap_or(false);
    if unchanged {
        return;
    }
    modes.insert(status.session_id.clone(), status.clone());
    drop(modes);
    let _ = app.emit(SFTP_MODE_EVENT, &status);
}

/// Determines (and caches) the transfer mode of a session so the frontend can
/// show the fallback diagnosis even when it missed the "kortty-sftp-mode" event.
#[tauri::command]
pub async fn sftp_transfer_mode(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
) -> Result<SftpModeStatus, String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    resolve_transfer_backend(&app, &mode_store, &session, &session_id).await;
    drop(session);
    let modes = mode_store.0.lock().await;
    modes
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "Transfer mode not determined".to_string())
}

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

#[tauri::command]
pub async fn sftp_list_dir(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    path: String,
) -> Result<Vec<FileEntry>, String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::list_dir(&sftp, &path).await
        }
        TransferBackend::ExecFallback => exec_fallback::list_dir(&session, &path).await,
    }
}

#[tauri::command]
pub async fn read_local_text_file(path: String) -> Result<String, String> {
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Local path is not a file".into());
    }
    if metadata.len() > MAX_TEXT_EDITOR_BYTES {
        return Err(format!(
            "Local file is too large for the snippet editor ({} bytes)",
            metadata.len()
        ));
    }
    fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn write_local_text_file(path: String, content: String) -> Result<(), String> {
    if content.len() as u64 > MAX_TEXT_EDITOR_BYTES {
        return Err("Content is too large for the snippet editor handoff".into());
    }
    if let Some(parent) = Path::new(&path).parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn local_mkdir(path: String) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn local_rename(old_path: String, new_path: String) -> Result<(), String> {
    fs::rename(old_path, new_path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn local_delete(path: String) -> Result<(), String> {
    let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if metadata.is_dir() {
        fs::remove_dir(path).map_err(|e| e.to_string())
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub async fn sftp_upload(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    local_path: String,
    remote_path: String,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::upload(&sftp, &local_path, &remote_path).await
        }
        TransferBackend::ExecFallback => {
            exec_fallback::upload(&session, &local_path, &remote_path).await
        }
    }
}

#[tauri::command]
pub async fn read_remote_text_file(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    remote_path: String,
) -> Result<String, String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::read_text_file(&sftp, &remote_path).await
        }
        TransferBackend::ExecFallback => {
            exec_fallback::read_remote_text_file(&session, &remote_path).await
        }
    }
}

#[tauri::command]
pub async fn write_remote_text_file(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    remote_path: String,
    content: String,
) -> Result<(), String> {
    if content.len() as u64 > MAX_TEXT_EDITOR_BYTES {
        return Err("Content is too large for the snippet editor handoff".into());
    }
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::write_text_file(&sftp, &remote_path, &content).await
        }
        TransferBackend::ExecFallback => {
            exec_fallback::write_remote_text_file(&session, &remote_path, &content).await
        }
    }
}

/// Writes the snippet-editor content as a sibling of an existing remote file
/// ("Save as…" flow). The new file name is validated against the same rules as
/// the Java `SftpFileTransferService.validateRemoteSiblingFileName` and the
/// target path is resolved next to `original_path`. Returns the new path.
#[tauri::command]
pub async fn write_remote_text_file_as(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    original_path: String,
    new_file_name: String,
    content: String,
) -> Result<String, String> {
    if content.len() as u64 > MAX_TEXT_EDITOR_BYTES {
        return Err("Content is too large for the snippet editor handoff".into());
    }
    let target_path = paths::resolve_sibling_remote_file_path(&original_path, &new_file_name)?;
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::write_text_file(&sftp, &target_path, &content).await?;
        }
        TransferBackend::ExecFallback => {
            exec_fallback::write_remote_text_file(&session, &target_path, &content).await?;
        }
    }
    Ok(target_path)
}

#[tauri::command]
pub async fn sftp_download(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    remote_path: String,
    local_path: String,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::download(&sftp, &remote_path, &local_path).await
        }
        TransferBackend::ExecFallback => {
            exec_fallback::download(&session, &remote_path, &local_path).await
        }
    }
}

#[tauri::command]
pub async fn sftp_delete(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::delete(&sftp, &path).await
        }
        TransferBackend::ExecFallback => exec_fallback::delete(&session, &path).await,
    }
}

#[tauri::command]
pub async fn sftp_rename(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::rename(&sftp, &old_path, &new_path).await
        }
        TransferBackend::ExecFallback => {
            exec_fallback::rename(&session, &old_path, &new_path).await
        }
    }
}

#[tauri::command]
pub async fn sftp_chmod(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    path: String,
    mode: u32,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::chmod(&sftp, &path, mode).await
        }
        TransferBackend::ExecFallback => exec_fallback::chmod(&session, &path, mode).await,
    }
}

#[tauri::command]
pub async fn sftp_mkdir(
    app: AppHandle,
    state: State<'_, SSHManager>,
    mode_store: State<'_, SftpModeStore>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    match resolve_transfer_backend(&app, &mode_store, &session, &session_id).await {
        TransferBackend::Sftp(sftp) => {
            drop(session);
            manager::mkdir(&sftp, &path).await
        }
        TransferBackend::ExecFallback => exec_fallback::mkdir(&session, &path).await,
    }
}

/// Intentionally exec-only: chown by user/group *names* (with recursion) has no
/// SFTP-protocol equivalent — SETSTAT only accepts numeric uid/gid and is not
/// recursive. The remote `chown` binary resolves names and handles -R.
#[tauri::command]
pub async fn sftp_chown(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
    owner: String,
    group: String,
    recursive: bool,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
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

/// Intentionally exec-only: symbolic modes ("u+x", "a-w") and recursive
/// application are features of the `chmod` binary, not of the SFTP protocol.
#[tauri::command]
pub async fn sftp_chmod_str(
    state: State<'_, SSHManager>,
    session_id: String,
    path: String,
    mode: String,
    recursive: bool,
) -> Result<(), String> {
    let session_arc = get_session(&state, &session_id).await?;
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

/// Intentionally exec-only: probes for archive binaries on the remote host.
#[tauri::command]
pub async fn sftp_check_archive_tools(
    state: State<'_, SSHManager>,
    session_id: String,
) -> Result<ArchiveToolsAvailable, String> {
    let session_arc = get_session(&state, &session_id).await?;
    let session = session_arc.lock().await;
    let cmd = "echo ZIP=$(which zip 2>/dev/null && echo ok || echo no) TAR=$(which tar 2>/dev/null && echo ok || echo no) 7Z=$(which 7z 2>/dev/null || which 7za 2>/dev/null && echo ok || echo no)";
    let output = session.exec_command(cmd).await.map_err(|e| e.to_string())?;
    Ok(ArchiveToolsAvailable {
        zip: output.contains("ZIP=") && output.contains("zip") && output.contains("ok"),
        tar_bz2: output.contains("TAR=") && output.contains("tar") && output.contains("ok"),
        seven_zip: output.contains("7Z=") && (output.contains("7z") || output.contains("7za")),
    })
}

/// Intentionally exec-only: archives are created by zip/tar/7z on the remote
/// host; there is no SFTP equivalent.
#[tauri::command]
pub async fn sftp_create_archive(
    state: State<'_, SSHManager>,
    session_id: String,
    request: CreateArchiveRequest,
) -> Result<String, String> {
    let session_arc = get_session(&state, &session_id).await?;
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
