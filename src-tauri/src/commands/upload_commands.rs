//! Drag&drop upload of local files into an SSH terminal session.
//!
//! Port of the Java `TerminalView.copyDroppedFilesToServer` flow including the
//! static drag&drop path helpers (`resolveDragDropRemoteDirectory`,
//! `appendRemotePath`, `parentRemotePath`, `needsSftpStartDirectory`) verified
//! by `TerminalViewDragDropPathTest`.
//!
//! The remote target directory is resolved from the session's tracked cwd
//! (OSC-7/cd tracking in `ssh::session`) with the SFTP start directory
//! (`canonicalize "."`) as fallback for `~`-style tracked values. Progress is
//! reported via the `terminal://upload-progress` event and uploads can be
//! cancelled with `cancel_dropped_upload`.

use crate::sftp::exec_fallback;
use crate::ssh::{SSHManager, SESSION_LOCK_TIMEOUT};
use russh_sftp::protocol::OpenFlags;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Tauri event emitted while a dropped upload runs.
pub const UPLOAD_PROGRESS_EVENT: &str = "terminal://upload-progress";

/// Emit byte-level progress at most every this many bytes (plus once per file).
const PROGRESS_STEP_BYTES: u64 = 256 * 1024;
/// Chunk size for SFTP streaming uploads.
const UPLOAD_CHUNK_SIZE: usize = 64 * 1024;

/// Cancellation flags of running dropped uploads, keyed by upload id.
#[derive(Default)]
pub struct DroppedUploadCancelStore(pub StdMutex<HashMap<String, Arc<AtomicBool>>>);

/// Payload of the `terminal://upload-progress` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedUploadProgress {
    pub upload_id: String,
    pub target_dir: String,
    pub current_file: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub file_index: usize,
    pub file_count: usize,
}

/// Result returned when a dropped upload finishes (or was cancelled).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedUploadResult {
    pub target_dir: String,
    pub file_count: usize,
    pub uploaded_count: usize,
    pub cancelled: bool,
}

/// One local path with its remote-relative target (Java `TerminalView.PathPair`).
#[derive(Debug, Clone)]
struct UploadEntry {
    local: PathBuf,
    remote: String,
    is_dir: bool,
    size: u64,
}

/// Java `TerminalView.appendRemotePath`.
fn append_remote_path(base_path: &str, relative_path: &str) -> String {
    let base = {
        let trimmed = base_path.trim();
        if trimmed.is_empty() {
            "."
        } else {
            trimmed
        }
    };
    let relative = relative_path.trim();
    if relative.is_empty() {
        return base.to_string();
    }
    if relative.starts_with('/') {
        return relative.to_string();
    }
    if base == "/" {
        return format!("/{relative}");
    }
    if base.ends_with('/') {
        format!("{base}{relative}")
    } else {
        format!("{base}/{relative}")
    }
}

/// Java `TerminalView.parentRemotePath`.
fn parent_remote_path(remote_path: &str) -> String {
    let normalized = remote_path.trim();
    if normalized.is_empty() {
        return ".".to_string();
    }
    match normalized.rfind('/') {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(index) => normalized[..index].to_string(),
    }
}

/// Java `TerminalView.needsSftpStartDirectory`.
fn needs_sftp_start_directory(tracked_directory: &str) -> bool {
    let tracked = tracked_directory.trim();
    tracked.is_empty() || tracked == "~" || tracked.starts_with("~/")
}

/// Java `TerminalView.normalizedRemoteDirectoryOrCurrent`.
fn normalized_remote_directory_or_current(remote_directory: Option<&str>) -> String {
    match remote_directory {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => ".".to_string(),
    }
}

/// Java `TerminalView.resolveDragDropRemoteDirectory`.
pub(crate) fn resolve_drag_drop_remote_directory(
    tracked_directory: Option<&str>,
    sftp_start_directory: Option<&str>,
) -> String {
    let fallback = normalized_remote_directory_or_current(sftp_start_directory);
    let Some(tracked) = tracked_directory.map(str::trim).filter(|t| !t.is_empty()) else {
        return fallback;
    };
    if tracked.starts_with('/') {
        return tracked.to_string();
    }
    if tracked == "~" {
        return fallback;
    }
    if let Some(relative_to_home) = tracked.strip_prefix("~/") {
        if relative_to_home.trim().is_empty() {
            return fallback;
        }
        return if fallback.starts_with('/') {
            append_remote_path(&fallback, relative_to_home)
        } else {
            relative_to_home.to_string()
        };
    }
    tracked.to_string()
}

/// Java `TerminalView.collectFiles`: walks a dropped path into upload entries.
fn collect_files(local: &Path, remote_dir: &str, out: &mut Vec<UploadEntry>) {
    if local.is_file() {
        let Some(name) = local.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return;
        };
        let remote = if remote_dir.is_empty() {
            name
        } else {
            format!("{remote_dir}/{name}")
        };
        let size = std::fs::metadata(local).map(|meta| meta.len()).unwrap_or(0);
        out.push(UploadEntry {
            local: local.to_path_buf(),
            remote,
            is_dir: false,
            size,
        });
    } else if local.is_dir() {
        let Some(dir_name) = local.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return;
        };
        let sub_remote = if remote_dir.is_empty() {
            dir_name
        } else {
            format!("{remote_dir}/{dir_name}")
        };
        out.push(UploadEntry {
            local: local.to_path_buf(),
            remote: sub_remote.clone(),
            is_dir: true,
            size: 0,
        });
        if let Ok(entries) = std::fs::read_dir(local) {
            let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
            children.sort();
            for child in children {
                collect_files(&child, &sub_remote, out);
            }
        }
    }
}

struct ProgressEmitter {
    app: AppHandle,
    upload_id: String,
    target_dir: String,
    bytes_total: u64,
    file_count: usize,
    last_emitted_bytes: u64,
}

impl ProgressEmitter {
    fn emit(&mut self, current_file: &str, bytes_done: u64, file_index: usize, force: bool) {
        if !force && bytes_done.saturating_sub(self.last_emitted_bytes) < PROGRESS_STEP_BYTES {
            return;
        }
        self.last_emitted_bytes = bytes_done;
        let _ = self.app.emit(
            UPLOAD_PROGRESS_EVENT,
            DroppedUploadProgress {
                upload_id: self.upload_id.clone(),
                target_dir: self.target_dir.clone(),
                current_file: current_file.to_string(),
                bytes_done,
                bytes_total: self.bytes_total,
                file_index,
                file_count: self.file_count,
            },
        );
    }
}

fn register_cancel_flag(
    store: &DroppedUploadCancelStore,
    upload_id: &str,
) -> Result<Arc<AtomicBool>, String> {
    let flag = Arc::new(AtomicBool::new(false));
    let mut flags = store
        .0
        .lock()
        .map_err(|_| "Upload cancellation store unavailable".to_string())?;
    flags.insert(upload_id.to_string(), flag.clone());
    Ok(flag)
}

fn remove_cancel_flag(store: &DroppedUploadCancelStore, upload_id: &str) {
    if let Ok(mut flags) = store.0.lock() {
        flags.remove(upload_id);
    }
}

/// Streams one local file to the remote path via SFTP with byte-level progress.
async fn sftp_upload_with_progress(
    sftp: &russh_sftp::client::SftpSession,
    entry: &UploadEntry,
    full_remote: &str,
    cancel: &AtomicBool,
    bytes_done: &mut u64,
    progress: &mut ProgressEmitter,
    file_index: usize,
) -> Result<(), String> {
    let mut local = tokio::fs::File::open(&entry.local)
        .await
        .map_err(|error| format!("Failed to read local file: {error}"))?;
    let mut remote = sftp
        .open_with_flags(
            full_remote,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        )
        .await
        .map_err(|error| format!("Failed to open remote file {full_remote}: {error}"))?;
    let mut buffer = vec![0u8; UPLOAD_CHUNK_SIZE];
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = remote.shutdown().await;
            return Ok(());
        }
        let read = local
            .read(&mut buffer)
            .await
            .map_err(|error| format!("Failed to read local file: {error}"))?;
        if read == 0 {
            break;
        }
        remote
            .write_all(&buffer[..read])
            .await
            .map_err(|error| format!("Upload failed: {error}"))?;
        *bytes_done += read as u64;
        progress.emit(&entry.remote, *bytes_done, file_index, false);
    }
    remote
        .shutdown()
        .await
        .map_err(|error| format!("Upload failed while finalizing {full_remote}: {error}"))?;
    Ok(())
}

/// Uploads dropped local paths into the tracked remote working directory of
/// an SSH session (Java `TerminalView.copyDroppedFilesToServer`).
///
/// Emits `terminal://upload-progress` events with
/// `{uploadId, targetDir, currentFile, bytesDone, bytesTotal, fileIndex, fileCount}`.
#[tauri::command]
pub async fn ssh_upload_dropped_paths(
    app: AppHandle,
    state: State<'_, SSHManager>,
    cancel_store: State<'_, DroppedUploadCancelStore>,
    session_id: String,
    paths: Vec<String>,
    upload_id: String,
) -> Result<DroppedUploadResult, String> {
    let cancel_flag = register_cancel_flag(&cancel_store, &upload_id)?;
    let result =
        run_dropped_upload(&app, &state, &session_id, paths, &upload_id, &cancel_flag).await;
    remove_cancel_flag(&cancel_store, &upload_id);
    result
}

async fn run_dropped_upload(
    app: &AppHandle,
    state: &State<'_, SSHManager>,
    session_id: &str,
    paths: Vec<String>,
    upload_id: &str,
    cancel_flag: &AtomicBool,
) -> Result<DroppedUploadResult, String> {
    let session_arc = state
        .get_session(session_id)
        .await
        .ok_or_else(|| "Session not found".to_string())?;

    let mut to_upload: Vec<UploadEntry> = Vec::new();
    for path in &paths {
        collect_files(Path::new(path), "", &mut to_upload);
    }
    if to_upload.is_empty() {
        return Err("No files to upload".to_string());
    }
    let file_count = to_upload.iter().filter(|entry| !entry.is_dir).count();
    let bytes_total: u64 = to_upload.iter().map(|entry| entry.size).sum();

    // Resolve tracked directory, fallback start directory and transfer backend
    // while holding the session lock, then release it for the transfer itself.
    let (target_dir, sftp) = {
        let session = tokio::time::timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
            .await
            .map_err(|_| "Session busy or unavailable".to_string())?;
        if !session.is_connected() {
            return Err("Session not connected".to_string());
        }
        let tracked = session.current_remote_directory();
        let (_, home_hint) = session.remote_directory_hints();
        let sftp = session.open_sftp().await.ok();
        let start_dir = if needs_sftp_start_directory(&tracked) {
            if let Some(sftp) = sftp.as_ref() {
                match sftp.canonicalize(".").await {
                    Ok(directory) if !directory.trim().is_empty() => {
                        Some(directory.trim().to_string())
                    }
                    _ => home_hint.clone(),
                }
            } else if home_hint.is_some() {
                home_hint.clone()
            } else {
                // Exec channels start in the login directory, equivalent to
                // the SFTP start directory resolved via canonicalize(".").
                match session.exec_command("pwd").await {
                    Ok(output) if !output.trim().is_empty() => Some(output.trim().to_string()),
                    _ => None,
                }
            }
        } else {
            None
        };
        let target_dir =
            resolve_drag_drop_remote_directory(Some(tracked.as_str()), start_dir.as_deref());
        (target_dir, sftp)
    };

    let mut progress = ProgressEmitter {
        app: app.clone(),
        upload_id: upload_id.to_string(),
        target_dir: target_dir.clone(),
        bytes_total,
        file_count,
        last_emitted_bytes: 0,
    };
    progress.emit("", 0, 0, true);

    let mut bytes_done: u64 = 0;
    let mut uploaded_count = 0usize;
    let mut file_index = 0usize;
    let mut cancelled = false;

    for entry in &to_upload {
        if cancel_flag.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let full_remote = append_remote_path(&target_dir, &entry.remote);
        if entry.is_dir {
            match sftp.as_ref() {
                Some(sftp) => crate::sftp::manager::mkdir(sftp, &full_remote).await?,
                None => {
                    let session = tokio::time::timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
                        .await
                        .map_err(|_| "Session busy or unavailable".to_string())?;
                    exec_fallback::mkdir(&session, &full_remote).await?;
                }
            }
            continue;
        }

        file_index += 1;
        progress.emit(&entry.remote, bytes_done, file_index, true);
        match sftp.as_ref() {
            Some(sftp) => {
                let parent = parent_remote_path(&full_remote);
                if parent != "." && parent != "/" {
                    crate::sftp::manager::mkdir(sftp, &parent).await?;
                }
                sftp_upload_with_progress(
                    sftp,
                    entry,
                    &full_remote,
                    cancel_flag,
                    &mut bytes_done,
                    &mut progress,
                    file_index,
                )
                .await?;
            }
            None => {
                let session = tokio::time::timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
                    .await
                    .map_err(|_| "Session busy or unavailable".to_string())?;
                let parent = parent_remote_path(&full_remote);
                if parent != "." && parent != "/" {
                    exec_fallback::mkdir(&session, &parent).await?;
                }
                exec_fallback::upload(&session, &entry.local.to_string_lossy(), &full_remote)
                    .await?;
                bytes_done += entry.size;
            }
        }
        if cancel_flag.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        uploaded_count += 1;
        progress.emit(&entry.remote, bytes_done, file_index, true);
    }

    progress.emit("", bytes_done, file_index, true);
    Ok(DroppedUploadResult {
        target_dir,
        file_count,
        uploaded_count,
        cancelled,
    })
}

/// Requests cancellation of a running dropped upload.
#[tauri::command]
pub fn cancel_dropped_upload(
    cancel_store: State<'_, DroppedUploadCancelStore>,
    upload_id: String,
) -> Result<(), String> {
    let flags = cancel_store
        .0
        .lock()
        .map_err(|_| "Upload cancellation store unavailable".to_string())?;
    if let Some(flag) = flags.get(&upload_id) {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Updates the tracked remote working directory of a session from the UI
/// prompt heuristic (fallback of the cwd tracking chain after OSC-7/`cd`
/// tracking; Java `SshTtyConnector.updateCurrentRemoteDirectoryHint`).
#[tauri::command]
pub async fn ssh_update_remote_directory_hint(
    state: State<'_, SSHManager>,
    session_id: String,
    directory: String,
) -> Result<(), String> {
    let Some(session_arc) = state.get_session(&session_id).await else {
        return Ok(());
    };
    let session = tokio::time::timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| "Session busy or unavailable".to_string())?;
    session.update_current_remote_directory_hint(&directory);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ports of TerminalViewDragDropPathTest.

    #[test]
    fn append_remote_path_handles_absolute_base() {
        assert_eq!(
            append_remote_path("/home/daniel", "file.txt"),
            "/home/daniel/file.txt"
        );
        assert_eq!(append_remote_path("/", "file.txt"), "/file.txt");
    }

    #[test]
    fn append_remote_path_handles_relative_base() {
        assert_eq!(append_remote_path(".", "file.txt"), "./file.txt");
        assert_eq!(append_remote_path("work", "file.txt"), "work/file.txt");
    }

    #[test]
    fn append_remote_path_keeps_absolute_relative_and_empty_values() {
        assert_eq!(append_remote_path("/home", "/etc/hosts"), "/etc/hosts");
        assert_eq!(append_remote_path("/home/daniel", ""), "/home/daniel");
        assert_eq!(append_remote_path("", "file.txt"), "./file.txt");
    }

    #[test]
    fn parent_remote_path_handles_root_and_relative_paths() {
        assert_eq!(parent_remote_path("/home/daniel/file.txt"), "/home/daniel");
        assert_eq!(parent_remote_path("/file.txt"), "/");
        assert_eq!(parent_remote_path("file.txt"), ".");
        assert_eq!(parent_remote_path("dir/file.txt"), "dir");
    }

    #[test]
    fn resolves_home_relative_tracked_directory_against_sftp_start_directory() {
        assert_eq!(
            resolve_drag_drop_remote_directory(Some("~/Dokumente"), Some("/home/daniel")),
            "/home/daniel/Dokumente"
        );
    }

    #[test]
    fn resolves_unknown_tracked_directory_to_sftp_start_directory() {
        assert_eq!(
            resolve_drag_drop_remote_directory(None, Some("/home/daniel")),
            "/home/daniel"
        );
        assert_eq!(
            resolve_drag_drop_remote_directory(Some("~"), Some("/home/daniel")),
            "/home/daniel"
        );
    }

    #[test]
    fn keeps_absolute_tracked_directory() {
        assert_eq!(
            resolve_drag_drop_remote_directory(Some("/var/tmp"), Some("/home/daniel")),
            "/var/tmp"
        );
    }

    #[test]
    fn falls_back_to_current_directory_without_start_directory() {
        assert_eq!(resolve_drag_drop_remote_directory(None, None), ".");
        assert_eq!(
            resolve_drag_drop_remote_directory(Some("~/work"), None),
            "work"
        );
        assert_eq!(
            resolve_drag_drop_remote_directory(Some("~/"), Some("/home/daniel")),
            "/home/daniel"
        );
    }

    #[test]
    fn needs_sftp_start_directory_detects_tilde_and_blank_values() {
        assert!(needs_sftp_start_directory(""));
        assert!(needs_sftp_start_directory("   "));
        assert!(needs_sftp_start_directory("~"));
        assert!(needs_sftp_start_directory("~/Dokumente"));
        assert!(!needs_sftp_start_directory("/var/tmp"));
        assert!(!needs_sftp_start_directory("relative"));
    }
}
