use anyhow::Result;
use serde::{Deserialize, Serialize};

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

pub struct SFTPManager;

impl SFTPManager {
    pub async fn list_dir(_session_id: &str, _path: &str) -> Result<Vec<FileEntry>> {
        // Will be implemented in Phase 8
        Ok(Vec::new())
    }

    pub async fn upload(_session_id: &str, _local_path: &str, _remote_path: &str) -> Result<()> {
        Ok(())
    }

    pub async fn download(_session_id: &str, _remote_path: &str, _local_path: &str) -> Result<()> {
        Ok(())
    }

    pub async fn delete(_session_id: &str, _path: &str) -> Result<()> {
        Ok(())
    }

    pub async fn rename(_session_id: &str, _old_path: &str, _new_path: &str) -> Result<()> {
        Ok(())
    }

    pub async fn chmod(_session_id: &str, _path: &str, _mode: u32) -> Result<()> {
        Ok(())
    }

    pub async fn mkdir(_session_id: &str, _path: &str) -> Result<()> {
        Ok(())
    }
}
