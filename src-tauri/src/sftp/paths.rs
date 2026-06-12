//! Remote path helpers for the SFTP snippet-editor "save as" flow.
//!
//! Exact ports of the static path helpers in the Java
//! `de.kortty.core.SftpFileTransferService` (normalizeRemotePath,
//! validateRemoteSiblingFileName, resolveSiblingRemoteFilePath and
//! appendRemoteName). Error messages match the Java originals so the
//! frontend can rely on stable texts.

/// Joins a remote directory path and a child name.
/// Port of Java `SftpFileTransferService.appendRemoteName`.
pub fn append_remote_name(base_path: &str, name: &str) -> String {
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

/// Trims a remote path and strips trailing slashes (keeping the root "/").
/// Port of Java `SftpFileTransferService.normalizeRemotePath`.
pub fn normalize_remote_path(remote_path: &str) -> Result<String, String> {
    let mut normalized = remote_path.trim();
    if normalized.is_empty() {
        return Err("Remote path must not be empty".to_string());
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized = &normalized[..normalized.len() - 1];
    }
    Ok(normalized.to_string())
}

/// Validates a file name that should be created next to an existing remote
/// file. Port of Java `SftpFileTransferService.validateRemoteSiblingFileName`.
pub fn validate_remote_sibling_file_name(new_file_name: &str) -> Result<String, String> {
    let normalized = new_file_name.trim();
    if normalized.is_empty() {
        return Err("File name must not be empty".to_string());
    }
    if normalized == "." || normalized == ".." {
        return Err("File name must not be '.' or '..'".to_string());
    }
    if normalized.contains('/') || normalized.contains('\\') {
        return Err("File name must not contain path separators".to_string());
    }
    Ok(normalized.to_string())
}

/// Resolves the sibling path of `original_remote_path` for `new_file_name`.
/// Port of Java `SftpFileTransferService.resolveSiblingRemoteFilePath`.
pub fn resolve_sibling_remote_file_path(
    original_remote_path: &str,
    new_file_name: &str,
) -> Result<String, String> {
    let normalized_original_path = normalize_remote_path(original_remote_path)?;
    let normalized_file_name = validate_remote_sibling_file_name(new_file_name)?;

    let parent_path = match normalized_original_path.rfind('/') {
        None => String::new(),
        Some(0) => "/".to_string(),
        Some(index) => normalized_original_path[..index].to_string(),
    };

    Ok(append_remote_name(&parent_path, &normalized_file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn normalize_remote_path_trims_and_strips_trailing_slashes() {
        assert_eq!(
            normalize_remote_path(" /etc/app.conf "),
            Ok("/etc/app.conf".to_string())
        );
        assert_eq!(
            normalize_remote_path("/var/log///"),
            Ok("/var/log".to_string())
        );
        assert_eq!(normalize_remote_path("/"), Ok("/".to_string()));
        assert_eq!(
            normalize_remote_path("   "),
            Err("Remote path must not be empty".to_string())
        );
    }

    #[test]
    fn resolve_sibling_remote_file_path_keeps_target_in_same_directory() {
        // Mirrors SftpFileTransferServiceTest.resolveSiblingRemoteFilePathKeepsTargetInSameDirectory.
        assert_eq!(
            resolve_sibling_remote_file_path("/etc/app.conf", "app2.conf"),
            Ok("/etc/app2.conf".to_string())
        );
        assert_eq!(
            resolve_sibling_remote_file_path("/app.conf", "app2.conf"),
            Ok("/app2.conf".to_string())
        );
        assert_eq!(
            resolve_sibling_remote_file_path("app.conf", "app2.conf"),
            Ok("app2.conf".to_string())
        );
    }

    #[test]
    fn resolve_sibling_remote_file_path_trims_inputs() {
        assert_eq!(
            resolve_sibling_remote_file_path("/etc/app.conf/", "  app2.conf  "),
            Ok("/etc/app2.conf".to_string())
        );
    }

    #[test]
    fn resolve_sibling_remote_file_path_rejects_unsafe_file_names() {
        // Mirrors SftpFileTransferServiceTest.resolveSiblingRemoteFilePathRejectsUnsafeFileNames.
        assert_eq!(
            resolve_sibling_remote_file_path("/etc/app.conf", ""),
            Err("File name must not be empty".to_string())
        );
        assert_eq!(
            resolve_sibling_remote_file_path("/etc/app.conf", "/tmp/app.conf"),
            Err("File name must not contain path separators".to_string())
        );
        assert_eq!(
            resolve_sibling_remote_file_path("/etc/app.conf", "tmp\\app.conf"),
            Err("File name must not contain path separators".to_string())
        );
        assert_eq!(
            resolve_sibling_remote_file_path("/etc/app.conf", ".."),
            Err("File name must not be '.' or '..'".to_string())
        );
        assert_eq!(
            resolve_sibling_remote_file_path("/etc/app.conf", "."),
            Err("File name must not be '.' or '..'".to_string())
        );
    }

    #[test]
    fn validate_remote_sibling_file_name_returns_trimmed_name() {
        assert_eq!(
            validate_remote_sibling_file_name("  notes.txt "),
            Ok("notes.txt".to_string())
        );
    }
}
