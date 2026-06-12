//! SHA-256 verified asset download, ported from Java `UpdateAssetDownloader`.
//!
//! The asset is streamed to `<target>.part` while the SHA-256 digest is
//! computed on the fly; on success the part file is atomically renamed to the
//! final target. Name collisions resolve to `"name (1).ext"` and so on.

use super::UpdateAsset;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Mirrors Java `DownloadException` (plus transport errors).
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("{0}")]
    Digest(String),
    #[error("{0}")]
    Http(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Downloads `asset` into `download_directory`, verifying its SHA-256 digest.
/// `on_progress` receives `(bytes_done, bytes_total)` after every chunk;
/// `bytes_total` is the advertised asset size (`-1` when unknown).
pub async fn download<F>(
    asset: &UpdateAsset,
    download_directory: &Path,
    mut on_progress: F,
) -> Result<PathBuf, DownloadError>
where
    F: FnMut(u64, i64),
{
    let expected_sha256 = expected_sha256(asset)?;
    fs::create_dir_all(download_directory)?;
    let mut part = PartFile::create(download_directory, &asset.name)?;

    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| DownloadError::Http(format!("Could not create HTTP client: {e}")))?;
    let response = match client
        .get(&asset.download_uri)
        .header("User-Agent", super::github::USER_AGENT)
        .send()
        .await
    {
        Ok(response) => response,
        Err(e) => {
            part.abort();
            return Err(DownloadError::Http(format!("Download failed: {e}")));
        }
    };
    let status = response.status();
    if !status.is_success() {
        part.abort();
        return Err(DownloadError::Http(format!(
            "Download failed with HTTP {}",
            status.as_u16()
        )));
    }

    let mut response = response;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if let Err(e) = part.write_chunk(&chunk) {
                    part.abort();
                    return Err(DownloadError::Io(e));
                }
                on_progress(part.bytes_done(), asset.size);
            }
            Ok(None) => break,
            Err(e) => {
                part.abort();
                return Err(DownloadError::Http(format!("Download failed: {e}")));
            }
        }
    }

    part.finalize(&expected_sha256, &asset.name)
}

/// Extracts and validates the expected `sha256:<64 hex>` digest of an asset
/// (mirrors Java `UpdateAssetDownloader.expectedSha256`). A missing digest is
/// an error, exactly like in Java.
pub fn expected_sha256(asset: &UpdateAsset) -> Result<String, DownloadError> {
    let digest = asset.digest.as_deref().map(str::trim).unwrap_or("");
    if !digest.to_lowercase().starts_with("sha256:") {
        return Err(DownloadError::Digest(format!(
            "Asset {} has no SHA-256 digest.",
            asset.name
        )));
    }
    let value = digest["sha256:".len()..].trim().to_lowercase();
    let valid = value.len() == 64 && value.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    if !valid {
        return Err(DownloadError::Digest(format!(
            "Asset {} has an invalid SHA-256 digest.",
            asset.name
        )));
    }
    Ok(value)
}

/// Streams chunks into `<target>.part` while hashing, then verifies and
/// atomically renames in [`PartFile::finalize`].
pub(crate) struct PartFile {
    part_path: PathBuf,
    final_path: PathBuf,
    file: fs::File,
    hasher: Sha256,
    bytes_done: u64,
}

impl PartFile {
    pub(crate) fn create(directory: &Path, asset_name: &str) -> std::io::Result<PartFile> {
        let final_path = unique_target(directory, &safe_file_name(asset_name));
        let part_name = format!(
            "{}.part",
            final_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let part_path = final_path.with_file_name(part_name);
        if part_path.exists() {
            fs::remove_file(&part_path)?;
        }
        let file = fs::File::create(&part_path)?;
        Ok(PartFile {
            part_path,
            final_path,
            file,
            hasher: Sha256::new(),
            bytes_done: 0,
        })
    }

    pub(crate) fn write_chunk(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.file.write_all(chunk)?;
        self.hasher.update(chunk);
        self.bytes_done += chunk.len() as u64;
        Ok(())
    }

    pub(crate) fn bytes_done(&self) -> u64 {
        self.bytes_done
    }

    /// Verifies the digest and atomically renames the part file to the final
    /// target. On any failure the part file is removed.
    pub(crate) fn finalize(
        self,
        expected_sha256: &str,
        asset_name: &str,
    ) -> Result<PathBuf, DownloadError> {
        let PartFile {
            part_path,
            final_path,
            mut file,
            hasher,
            ..
        } = self;
        if let Err(e) = file.flush() {
            drop(file);
            let _ = fs::remove_file(&part_path);
            return Err(DownloadError::Io(e));
        }
        drop(file);
        let actual_sha256: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if actual_sha256 != expected_sha256 {
            let _ = fs::remove_file(&part_path);
            return Err(DownloadError::Digest(format!(
                "SHA-256 mismatch for {asset_name}."
            )));
        }
        if let Err(e) = fs::rename(&part_path, &final_path) {
            let _ = fs::remove_file(&part_path);
            return Err(DownloadError::Io(e));
        }
        Ok(final_path)
    }

    /// Removes the part file after a failed download.
    pub(crate) fn abort(self) {
        let PartFile {
            part_path, file, ..
        } = self;
        drop(file);
        let _ = fs::remove_file(&part_path);
    }
}

/// Returns a non-existing target path, appending ` (1)`, ` (2)`, ... before
/// the extension on collisions (mirrors Java `uniqueTarget`).
pub(crate) fn unique_target(directory: &Path, file_name: &str) -> PathBuf {
    let candidate = directory.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let (base_name, extension) = match file_name.rfind('.') {
        Some(dot) if dot > 0 => (&file_name[..dot], &file_name[dot..]),
        _ => (file_name, ""),
    };
    for i in 1..10_000 {
        let candidate = directory.join(format!("{base_name} ({i}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    let millis = chrono::Utc::now().timestamp_millis();
    directory.join(format!("{base_name}-{millis}{extension}"))
}

/// Mirrors Java `safeFileName`: path separators become `_`; blank names fall
/// back to `kortty-update`.
pub(crate) fn safe_file_name(file_name: &str) -> String {
    let sanitized = file_name.replace(['\\', '/'], "_");
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "kortty-update".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha256_hex(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn asset(name: &str, content: &[u8]) -> UpdateAsset {
        UpdateAsset {
            name: name.to_string(),
            download_uri: format!("https://example.test/{name}"),
            size: content.len() as i64,
            digest: Some(format!("sha256:{}", sha256_hex(content))),
        }
    }

    #[test]
    fn downloads_asset_and_verifies_sha256() {
        let content = b"installer";
        let asset = asset("korTTY-Windows-2.3.0-x86_64.msi", content);
        let dir = temp_dir("kortty-update-download");

        let mut part = PartFile::create(&dir, &asset.name).unwrap();
        part.write_chunk(content).unwrap();
        assert_eq!(part.bytes_done(), content.len() as u64);
        let downloaded = part
            .finalize(&expected_sha256(&asset).unwrap(), &asset.name)
            .unwrap();

        assert_eq!(
            downloaded.file_name().unwrap().to_string_lossy(),
            asset.name
        );
        assert_eq!(fs::read_to_string(&downloaded).unwrap(), "installer");
        assert!(!downloaded
            .with_file_name(format!("{}.part", asset.name))
            .exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_unique_file_name_when_target_exists() {
        let content = b"installer";
        let asset = asset("korTTY-Windows-2.3.0-x86_64.msi", content);
        let dir = temp_dir("kortty-update-download-collision");
        fs::write(dir.join(&asset.name), "existing").unwrap();

        let mut part = PartFile::create(&dir, &asset.name).unwrap();
        part.write_chunk(content).unwrap();
        let downloaded = part
            .finalize(&expected_sha256(&asset).unwrap(), &asset.name)
            .unwrap();

        assert_eq!(
            downloaded.file_name().unwrap().to_string_lossy(),
            "korTTY-Windows-2.3.0-x86_64 (1).msi"
        );
        assert_eq!(
            fs::read_to_string(dir.join(&asset.name)).unwrap(),
            "existing"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deletes_partial_file_when_sha256_does_not_match() {
        let content = b"installer";
        let asset = UpdateAsset {
            name: "korTTY-Windows-2.3.0-x86_64.msi".to_string(),
            download_uri: "https://example.test/update.msi".to_string(),
            size: content.len() as i64,
            digest: Some(
                "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                    .to_string(),
            ),
        };
        let dir = temp_dir("kortty-update-download-mismatch");

        let mut part = PartFile::create(&dir, &asset.name).unwrap();
        part.write_chunk(content).unwrap();
        let result = part.finalize(&expected_sha256(&asset).unwrap(), &asset.name);

        assert!(matches!(result, Err(DownloadError::Digest(_))));
        assert!(!dir.join(&asset.name).exists());
        assert!(!dir.join(format!("{}.part", asset.name)).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_missing_or_invalid_digest() {
        let mut asset = asset("a.msi", b"x");

        asset.digest = None;
        assert!(matches!(
            expected_sha256(&asset),
            Err(DownloadError::Digest(message)) if message.contains("has no SHA-256 digest")
        ));

        asset.digest = Some("md5:abc".to_string());
        assert!(matches!(
            expected_sha256(&asset),
            Err(DownloadError::Digest(message)) if message.contains("has no SHA-256 digest")
        ));

        asset.digest = Some("sha256:1234".to_string());
        assert!(matches!(
            expected_sha256(&asset),
            Err(DownloadError::Digest(message)) if message.contains("invalid SHA-256 digest")
        ));

        asset.digest = Some(format!("sha256:{}", "g".repeat(64)));
        assert!(matches!(
            expected_sha256(&asset),
            Err(DownloadError::Digest(_))
        ));
    }

    #[test]
    fn accepts_digest_with_uppercase_prefix_and_hex() {
        let asset = UpdateAsset {
            name: "a.msi".to_string(),
            download_uri: "https://example.test/a.msi".to_string(),
            size: 1,
            digest: Some(format!("SHA256:{}", "AB".repeat(32))),
        };

        assert_eq!(expected_sha256(&asset).unwrap(), "ab".repeat(32));
    }

    #[test]
    fn unique_target_splits_on_last_dot() {
        let dir = temp_dir("kortty-update-unique-target");
        fs::write(dir.join("kortty-2.3.0.pkg.tar.zst"), "existing").unwrap();

        let target = unique_target(&dir, "kortty-2.3.0.pkg.tar.zst");
        assert_eq!(
            target.file_name().unwrap().to_string_lossy(),
            "kortty-2.3.0.pkg.tar (1).zst"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizes_file_names() {
        assert_eq!(safe_file_name("a/b\\c.msi"), "a_b_c.msi");
        assert_eq!(safe_file_name("   "), "kortty-update");
        assert_eq!(safe_file_name("//"), "__");
    }
}
