use crate::sftp::manager::FileEntry;
use std::fs;
use std::path::Path;

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
            crate::sftp::manager::FileType::Directory
        } else if meta.is_symlink() {
            crate::sftp::manager::FileType::Symlink
        } else {
            crate::sftp::manager::FileType::File
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
pub async fn sftp_list_dir(_session_id: String, _path: String) -> Result<Vec<FileEntry>, String> {
    // Will be implemented in Phase 8
    Ok(Vec::new())
}

#[tauri::command]
pub async fn sftp_upload(
    _session_id: String,
    _local_path: String,
    _remote_path: String,
) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn sftp_download(
    _session_id: String,
    _remote_path: String,
    _local_path: String,
) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn sftp_delete(_session_id: String, _path: String) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn sftp_rename(
    _session_id: String,
    _old_path: String,
    _new_path: String,
) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn sftp_chmod(_session_id: String, _path: String, _mode: u32) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn sftp_mkdir(_session_id: String, _path: String) -> Result<(), String> {
    Ok(())
}
