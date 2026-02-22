use anyhow::Result;
use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;

pub fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let dir = home.join(".kortty");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

pub fn ensure_subdirs() -> Result<()> {
    let base = config_dir()?;
    for sub in &["history", "projects", "i18n", "ssh-keys"] {
        let path = base.join(sub);
        if !path.exists() {
            std::fs::create_dir_all(&path)?;
        }
    }
    Ok(())
}

pub fn save_xml<T: Serialize>(filename: &str, data: &T) -> Result<()> {
    let path = config_dir()?.join(filename);
    let xml = quick_xml::se::to_string(data)?;
    let formatted = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}", xml);
    std::fs::write(path, formatted)?;
    Ok(())
}

pub fn load_xml<T: DeserializeOwned>(filename: &str) -> Result<Option<T>> {
    let path = config_dir()?.join(filename);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)?;
    let data: T = quick_xml::de::from_str(&content)?;
    Ok(Some(data))
}

pub fn save_json<T: Serialize>(filename: &str, data: &T) -> Result<()> {
    let path = config_dir()?.join(filename);
    let json = serde_json::to_string_pretty(data)?;
    std::fs::write(path, json)?;
    Ok(())
}

pub fn load_json<T: DeserializeOwned>(filename: &str) -> Result<Option<T>> {
    let path = config_dir()?.join(filename);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)?;
    let data: T = serde_json::from_str(&content)?;
    Ok(Some(data))
}
