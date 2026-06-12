//! Local filesystem commands for the LocalFileBrowser.
//!
//! Port of the file operations from the Java `de.kortty.ui.LocalFileBrowser`:
//! copy/cut/paste with recursive directory handling, archive creation
//! (ZIP/TAR/TAR.GZ), owner/permission management (chmod/chown), file details
//! and principal discovery from /etc/passwd and /etc/group.

use serde::Serialize;
use std::fs;
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};

/// Detailed metadata for one local file, shown in the properties dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileStat {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified: Option<String>,
    pub permissions_octal: String,
    pub permissions_rwx: String,
    pub owner: String,
    pub group: String,
    pub is_dir: bool,
}

/// Users and groups discovered on the local system (best effort).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPrincipals {
    pub users: Vec<String>,
    pub groups: Vec<String>,
}

/// Outcome of a chown/chmod batch, mirroring the Java status reporting
/// (`sftp.setOwner.success` / `sftp.setOwner.errorCount`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerPermissionsResult {
    pub changed: usize,
    pub failed: usize,
    pub last_error: String,
}

// ---------------------------------------------------------------------------
// Octal permission helpers (port of LocalFileBrowser.isValidOctalPermissions,
// octalToPosix and permissionsToOctal)
// ---------------------------------------------------------------------------

/// Strict Java-parity check: exactly three octal digits ("755", not "0755").
pub fn is_valid_octal_permissions(permissions: &str) -> bool {
    permissions.len() == 3 && permissions.chars().all(|c| ('0'..='7').contains(&c))
}

/// Accepts three or four octal digits (the optional leading digit carries the
/// setuid/setgid/sticky bits) and returns the numeric mode.
pub fn parse_octal_mode(permissions: &str) -> Option<u32> {
    if !(3..=4).contains(&permissions.len()) {
        return None;
    }
    if !permissions.chars().all(|c| ('0'..='7').contains(&c)) {
        return None;
    }
    u32::from_str_radix(permissions, 8).ok()
}

/// Converts three octal digits into the nine-character rwx form
/// ("755" -> "rwxr-xr-x").
pub fn octal_to_posix(octal: &str) -> Result<String, String> {
    if !is_valid_octal_permissions(octal) {
        return Err("Expected three octal permission digits".to_string());
    }
    let mut posix = String::with_capacity(9);
    for c in octal.chars() {
        let value = c.to_digit(8).unwrap_or(0);
        posix.push(if value & 4 != 0 { 'r' } else { '-' });
        posix.push(if value & 2 != 0 { 'w' } else { '-' });
        posix.push(if value & 1 != 0 { 'x' } else { '-' });
    }
    Ok(posix)
}

/// Formats the permission bits of a raw st_mode as three octal digits.
pub fn mode_to_octal(mode: u32) -> String {
    format!("{:03o}", mode & 0o777)
}

/// Formats the permission bits of a raw st_mode as the rwx string.
pub fn mode_to_rwx(mode: u32) -> String {
    octal_to_posix(&mode_to_octal(mode)).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Principal discovery (port of LocalFileBrowser.readUnixPrincipalNames)
// ---------------------------------------------------------------------------

/// Parses principal names from /etc/passwd or /etc/group style content:
/// skips blank lines and comments, takes the text before the first colon,
/// removes case-insensitive duplicates and sorts case-insensitively.
pub fn parse_principal_names(content: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let name = trimmed.split(':').next().unwrap_or("");
        if !name.is_empty()
            && !names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            names.push(name.to_string());
        }
    }
    names.sort_by_key(|a| a.to_lowercase());
    names
}

fn read_principal_names(file: &Path) -> Vec<String> {
    match fs::read_to_string(file) {
        Ok(content) => parse_principal_names(&content),
        Err(_) => Vec::new(),
    }
}

#[cfg(unix)]
fn merge_principal(names: &mut Vec<String>, candidate: Option<String>) {
    if let Some(candidate) = candidate {
        if !candidate.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(&candidate)) {
            names.push(candidate);
            names.sort_by_key(|a| a.to_lowercase());
        }
    }
}

#[cfg(unix)]
fn current_user_name() -> Option<String> {
    // Safety: getpwuid returns a pointer into static storage; only read pw_name.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr((*pw).pw_name)
                .to_string_lossy()
                .to_string(),
        )
    }
}

#[cfg(unix)]
fn current_group_name() -> Option<String> {
    // Safety: getgrgid returns a pointer into static storage; only read gr_name.
    unsafe {
        let gr = libc::getgrgid(libc::getgid());
        if gr.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr((*gr).gr_name)
                .to_string_lossy()
                .to_string(),
        )
    }
}

#[cfg(unix)]
fn user_name_for_uid(uid: u32) -> String {
    // Safety: see current_user_name.
    unsafe {
        let pw = libc::getpwuid(uid as libc::uid_t);
        if pw.is_null() {
            return uid.to_string();
        }
        std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_string_lossy()
            .to_string()
    }
}

#[cfg(unix)]
fn group_name_for_gid(gid: u32) -> String {
    // Safety: see current_group_name.
    unsafe {
        let gr = libc::getgrgid(gid as libc::gid_t);
        if gr.is_null() {
            return gid.to_string();
        }
        std::ffi::CStr::from_ptr((*gr).gr_name)
            .to_string_lossy()
            .to_string()
    }
}

/// Looks up the principal id (uid/gid) for a name in a colon separated file
/// (name:pass:id:...). Fallback for systems where getpwnam is unavailable
/// for the given name.
fn principal_id_from_file(file: &Path, name: &str) -> Option<u32> {
    let content = fs::read_to_string(file).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split(':');
        let entry_name = parts.next()?;
        if entry_name != name {
            continue;
        }
        let _password = parts.next();
        if let Some(id) = parts.next().and_then(|v| v.parse::<u32>().ok()) {
            return Some(id);
        }
    }
    None
}

#[cfg(unix)]
fn lookup_uid(name: &str) -> Result<libc::uid_t, String> {
    if let Ok(numeric) = name.parse::<u32>() {
        return Ok(numeric as libc::uid_t);
    }
    let cname = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
    // Safety: getpwnam returns a pointer into static storage; only read pw_uid.
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if !pw.is_null() {
        return Ok(unsafe { (*pw).pw_uid });
    }
    principal_id_from_file(Path::new("/etc/passwd"), name)
        .map(|id| id as libc::uid_t)
        .ok_or_else(|| format!("Unknown user: {}", name))
}

#[cfg(unix)]
fn lookup_gid(name: &str) -> Result<libc::gid_t, String> {
    if let Ok(numeric) = name.parse::<u32>() {
        return Ok(numeric as libc::gid_t);
    }
    let cname = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
    // Safety: getgrnam returns a pointer into static storage; only read gr_gid.
    let gr = unsafe { libc::getgrnam(cname.as_ptr()) };
    if !gr.is_null() {
        return Ok(unsafe { (*gr).gr_gid });
    }
    principal_id_from_file(Path::new("/etc/group"), name)
        .map(|id| id as libc::gid_t)
        .ok_or_else(|| format!("Unknown group: {}", name))
}

#[cfg(unix)]
fn lchown_path(
    path: &Path,
    uid: Option<libc::uid_t>,
    gid: Option<libc::gid_t>,
) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // (uid_t)-1 / (gid_t)-1 leave the respective id unchanged.
    let uid = uid.unwrap_or(libc::uid_t::MAX);
    let gid = gid.unwrap_or(libc::gid_t::MAX);
    // Safety: cpath is a valid NUL terminated path; lchown does not retain it.
    let result = unsafe { libc::lchown(cpath.as_ptr(), uid, gid) };
    if result != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(unix)]
fn chmod_path(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Copy/move (port of pasteFiles, copyDirectory, moveDirectory)
// ---------------------------------------------------------------------------

fn split_file_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(idx) if idx > 0 => (&name[..idx], &name[idx..]),
        _ => (name, ""),
    }
}

/// Resolves the destination path inside `target_dir` for `name`, appending a
/// " (1)", " (2)", ... suffix when the name is already taken. For files the
/// counter is inserted before the extension ("file (1).txt"); directories use
/// the full name as stem ("folder (1)").
pub fn unique_target_path(target_dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let direct = target_dir.join(name);
    if fs::symlink_metadata(&direct).is_err() {
        return direct;
    }
    let (stem, extension) = if is_dir {
        (name, "")
    } else {
        split_file_name(name)
    };
    let mut counter: u64 = 1;
    loop {
        let candidate = target_dir.join(format!("{} ({}){}", stem, counter, extension));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        counter += 1;
    }
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let entry_path = entry.path();
        let target = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&entry_path).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            copy_dir_recursive(&entry_path, &target)?;
        } else {
            fs::copy(&entry_path, &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn delete_recursive_impl(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).map_err(|e| e.to_string())
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}

/// Moves a path; falls back to copy+delete when a direct rename is not
/// possible (e.g. across devices), mirroring Java moveDirectory.
fn move_path(source: &Path, destination: &Path) -> Result<(), String> {
    if fs::rename(source, destination).is_ok() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if metadata.is_dir() {
        copy_dir_recursive(source, destination)?;
    } else {
        fs::copy(source, destination).map_err(|e| e.to_string())?;
    }
    delete_recursive_impl(source).map_err(|e| {
        format!(
            "Copied to {} but deleting source {} failed; source may remain: {}",
            destination.display(),
            source.display(),
            e
        )
    })
}

/// Copies or moves all sources into `target_dir`, resolving name collisions
/// with the " (n)" suffix pattern. Returns the created destination paths.
pub fn copy_paths_impl(
    sources: &[String],
    target_dir: &Path,
    move_files: bool,
) -> Result<Vec<String>, String> {
    if !target_dir.is_dir() {
        return Err(format!("Not a directory: {}", target_dir.display()));
    }
    let canonical_target = fs::canonicalize(target_dir).map_err(|e| e.to_string())?;
    let mut created = Vec::new();
    for source in sources {
        let source_path = Path::new(source);
        let metadata = fs::symlink_metadata(source_path).map_err(|e| e.to_string())?;
        let name = source_path
            .file_name()
            .ok_or_else(|| format!("Invalid source path: {}", source))?
            .to_string_lossy()
            .to_string();
        if metadata.is_dir() {
            if let Ok(canonical_source) = fs::canonicalize(source_path) {
                if canonical_target.starts_with(&canonical_source) {
                    return Err(format!(
                        "Cannot copy a folder into itself: {}",
                        source_path.display()
                    ));
                }
            }
        }
        if move_files {
            // Moving into the directory the source already lives in is a no-op.
            if let Some(parent) = source_path.parent() {
                if let Ok(canonical_parent) = fs::canonicalize(parent) {
                    if canonical_parent == canonical_target {
                        continue;
                    }
                }
            }
        }
        let destination = unique_target_path(&canonical_target, &name, metadata.is_dir());
        if move_files {
            move_path(source_path, &destination)?;
        } else if metadata.is_dir() {
            copy_dir_recursive(source_path, &destination)?;
        } else {
            fs::copy(source_path, &destination).map_err(|e| e.to_string())?;
        }
        created.push(destination.to_string_lossy().to_string());
    }
    Ok(created)
}

// ---------------------------------------------------------------------------
// Archive creation (port of archiveToZip / archiveToTar; the manual TAR
// header logic from Java is replaced by the tar crate)
// ---------------------------------------------------------------------------

fn archive_entry_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("Invalid path: {}", path.display()))
}

fn add_to_zip<W: Write + Seek>(
    writer: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
) -> Result<(), String> {
    let options = zip::write::SimpleFileOptions::default();
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.is_dir() {
        writer
            .add_directory(name, options)
            .map_err(|e| e.to_string())?;
        let mut children: Vec<PathBuf> = fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        children.sort();
        for child in children {
            let child_name = archive_entry_name(&child)?;
            add_to_zip(writer, &child, &format!("{}/{}", name, child_name))?;
        }
    } else {
        writer
            .start_file(name, options)
            .map_err(|e| e.to_string())?;
        let mut input = fs::File::open(path).map_err(|e| e.to_string())?;
        io::copy(&mut input, writer).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn create_zip(paths: &[String], target: &Path) -> Result<(), String> {
    let file = fs::File::create(target).map_err(|e| e.to_string())?;
    let mut writer = zip::ZipWriter::new(file);
    for source in paths {
        let path = Path::new(source);
        let name = archive_entry_name(path)?;
        add_to_zip(&mut writer, path, &name)?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn append_tar_entries<W: Write>(writer: W, paths: &[String]) -> Result<W, String> {
    let mut builder = tar::Builder::new(writer);
    builder.follow_symlinks(false);
    for source in paths {
        let path = Path::new(source);
        let name = archive_entry_name(path)?;
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            builder
                .append_dir_all(&name, path)
                .map_err(|e| e.to_string())?;
        } else {
            builder
                .append_path_with_name(path, &name)
                .map_err(|e| e.to_string())?;
        }
    }
    builder.into_inner().map_err(|e| e.to_string())
}

fn create_tar(paths: &[String], target: &Path, gzip: bool) -> Result<(), String> {
    let file = fs::File::create(target).map_err(|e| e.to_string())?;
    if gzip {
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let encoder = append_tar_entries(encoder, paths)?;
        encoder.finish().map_err(|e| e.to_string())?;
    } else {
        let file = append_tar_entries(file, paths)?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Creates an archive of the given paths. `format` is one of "zip", "tar" or
/// "tarGz".
pub fn create_archive_impl(
    paths: &[String],
    target_path: &Path,
    format: &str,
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("No files selected".to_string());
    }
    // The archive must not be created inside a folder that is being archived,
    // otherwise the walk would pick up the partially written archive.
    for source in paths {
        let source_path = Path::new(source);
        if source_path.is_dir() {
            let canonical_source =
                fs::canonicalize(source_path).unwrap_or_else(|_| source_path.to_path_buf());
            let target_parent = target_path
                .parent()
                .map(|p| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
            if let Some(target_parent) = target_parent {
                if target_parent.starts_with(&canonical_source) {
                    return Err("Archive target cannot be inside a selected folder".to_string());
                }
            }
        }
    }
    match format {
        "zip" => create_zip(paths, target_path),
        "tar" => create_tar(paths, target_path, false),
        "tarGz" | "tgz" => create_tar(paths, target_path, true),
        other => Err(format!("Unsupported archive format: {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Owner/permissions application
// ---------------------------------------------------------------------------

fn collect_paths_recursive(path: &Path, accumulator: &mut Vec<PathBuf>) {
    accumulator.push(path.to_path_buf());
    let is_real_dir = fs::symlink_metadata(path)
        .map(|m| m.is_dir())
        .unwrap_or(false);
    if is_real_dir {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                collect_paths_recursive(&entry.path(), accumulator);
            }
        }
    }
}

#[cfg(unix)]
fn apply_owner_permissions(
    paths: &[String],
    owner: Option<&str>,
    group: Option<&str>,
    mode: Option<u32>,
    recursive: bool,
) -> Result<OwnerPermissionsResult, String> {
    let uid = owner.map(lookup_uid).transpose()?;
    let gid = group.map(lookup_gid).transpose()?;

    let mut changed = 0usize;
    let mut failed = 0usize;
    let mut last_error = String::new();

    for path in paths {
        let root = Path::new(path);
        let mut targets = Vec::new();
        if recursive {
            collect_paths_recursive(root, &mut targets);
        } else {
            targets.push(root.to_path_buf());
        }

        let mut item_failed = false;
        for target in &targets {
            if uid.is_some() || gid.is_some() {
                if let Err(error) = lchown_path(target, uid, gid) {
                    item_failed = true;
                    last_error = error;
                    continue;
                }
            }
            if let Some(mode) = mode {
                // chmod follows symlinks; skip them to avoid changing targets.
                let is_symlink = fs::symlink_metadata(target)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                if !is_symlink {
                    if let Err(error) = chmod_path(target, mode) {
                        item_failed = true;
                        last_error = error;
                    }
                }
            }
        }
        if item_failed {
            failed += 1;
        } else {
            changed += 1;
        }
    }

    Ok(OwnerPermissionsResult {
        changed,
        failed,
        last_error,
    })
}

#[cfg(not(unix))]
fn apply_owner_permissions(
    _paths: &[String],
    _owner: Option<&str>,
    _group: Option<&str>,
    _mode: Option<u32>,
    _recursive: bool,
) -> Result<OwnerPermissionsResult, String> {
    Err("Owner or POSIX permissions are not supported on this platform".to_string())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Copies (or moves, when `move_files` is true) the source paths into the
/// target directory. Name collisions are resolved with the " (n)" pattern.
/// Returns the destination paths that were created.
#[tauri::command]
pub async fn local_copy_paths(
    sources: Vec<String>,
    target_dir: String,
    move_files: bool,
) -> Result<Vec<String>, String> {
    copy_paths_impl(&sources, Path::new(&target_dir), move_files)
}

/// Creates a new empty file; fails when the path already exists.
#[tauri::command]
pub async fn local_create_file(path: String) -> Result<(), String> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Returns detailed metadata for one local path (properties dialog).
#[tauri::command]
pub async fn local_stat(path: String) -> Result<LocalFileStat, String> {
    let path_buf = PathBuf::from(&path);
    let metadata = fs::symlink_metadata(&path_buf).map_err(|e| e.to_string())?;
    let name = path_buf
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    let modified = metadata.modified().ok().map(|time| {
        chrono::DateTime::<chrono::Local>::from(time)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    });

    #[cfg(unix)]
    let (permissions_octal, permissions_rwx, owner, group) = {
        use std::os::unix::fs::MetadataExt;
        let mode = metadata.mode();
        (
            mode_to_octal(mode),
            mode_to_rwx(mode),
            user_name_for_uid(metadata.uid()),
            group_name_for_gid(metadata.gid()),
        )
    };
    #[cfg(not(unix))]
    let (permissions_octal, permissions_rwx, owner, group) = {
        // Best effort on Windows: only the read-only flag is available.
        let readonly = metadata.permissions().readonly();
        let rwx = if readonly { "r--" } else { "rw-" };
        (String::new(), rwx.to_string(), String::new(), String::new())
    };

    Ok(LocalFileStat {
        name,
        path,
        size: if metadata.is_dir() { 0 } else { metadata.len() },
        modified,
        permissions_octal,
        permissions_rwx,
        owner,
        group,
        is_dir: metadata.is_dir(),
    })
}

/// Applies owner/group (chown) and/or permissions (chmod) to the given paths,
/// optionally recursing into directories. `octal` accepts 3-4 octal digits.
#[tauri::command]
pub async fn local_set_owner_permissions(
    paths: Vec<String>,
    owner: Option<String>,
    group: Option<String>,
    octal: Option<String>,
    recursive: bool,
) -> Result<OwnerPermissionsResult, String> {
    let owner = owner
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let group = group
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let octal = octal
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if owner.is_none() && group.is_none() && octal.is_none() {
        return Err("Please enter owner or permissions.".to_string());
    }
    let mode = match &octal {
        Some(value) => Some(
            parse_octal_mode(value)
                .ok_or_else(|| format!("Invalid octal permissions: {}", value))?,
        ),
        None => None,
    };
    apply_owner_permissions(&paths, owner.as_deref(), group.as_deref(), mode, recursive)
}

/// Lists users and groups known to the local system (best effort: parsed from
/// /etc/passwd and /etc/group, plus the current uid/gid names which on macOS
/// are managed by Directory Services rather than /etc/passwd).
#[tauri::command]
pub async fn local_list_principals() -> Result<LocalPrincipals, String> {
    #[cfg(unix)]
    {
        let mut users = read_principal_names(Path::new("/etc/passwd"));
        let mut groups = read_principal_names(Path::new("/etc/group"));
        merge_principal(&mut users, current_user_name());
        merge_principal(&mut groups, current_group_name());
        Ok(LocalPrincipals { users, groups })
    }
    #[cfg(not(unix))]
    {
        Ok(LocalPrincipals {
            users: Vec::new(),
            groups: Vec::new(),
        })
    }
}

/// Creates an archive ("zip", "tar" or "tarGz") of the given paths.
#[tauri::command]
pub async fn local_create_archive(
    paths: Vec<String>,
    target_path: String,
    format: String,
) -> Result<(), String> {
    create_archive_impl(&paths, Path::new(&target_path), &format)
}

/// Deletes a file or directory tree (the existing `local_delete` command only
/// removes empty directories).
#[tauri::command]
pub async fn local_delete_recursive(path: String) -> Result<(), String> {
    delete_recursive_impl(Path::new(&path))
}

/// Opens a path with the platform default application (Java Desktop.open).
#[tauri::command]
pub async fn local_open_path(path: String) -> Result<(), String> {
    let resolved = fs::canonicalize(&path).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&resolved).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(&resolved)
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(&resolved)
        .spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests (port of LocalFileBrowserPermissionsTest plus archive/copy coverage)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kortty-localfs-{}-{}", label, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn converts_octal_permissions_to_posix_string() {
        assert_eq!(octal_to_posix("755").unwrap(), "rwxr-xr-x");
        assert_eq!(octal_to_posix("640").unwrap(), "rw-r-----");
    }

    #[test]
    fn validates_only_three_octal_permission_digits() {
        assert!(is_valid_octal_permissions("755"));
        assert!(!is_valid_octal_permissions("0755"));
        assert!(!is_valid_octal_permissions("888"));
        assert!(!is_valid_octal_permissions("rw-r--r--"));
    }

    #[test]
    fn parses_three_or_four_octal_digits_as_mode() {
        assert_eq!(parse_octal_mode("755"), Some(0o755));
        assert_eq!(parse_octal_mode("0755"), Some(0o755));
        assert_eq!(parse_octal_mode("4755"), Some(0o4755));
        assert_eq!(parse_octal_mode("75"), None);
        assert_eq!(parse_octal_mode("07555"), None);
        assert_eq!(parse_octal_mode("888"), None);
        assert_eq!(parse_octal_mode("rw-"), None);
    }

    #[test]
    fn converts_mode_to_octal() {
        assert_eq!(mode_to_octal(0o100755), "755");
        assert_eq!(mode_to_octal(0o40750), "750");
        assert_eq!(mode_to_octal(0o644), "644");
    }

    #[test]
    fn converts_mode_to_rwx() {
        assert_eq!(mode_to_rwx(0o100755), "rwxr-xr-x");
        assert_eq!(mode_to_rwx(0o640), "rw-r-----");
    }

    #[test]
    fn reads_unix_principal_names_from_colon_separated_content() {
        let content = "# comment\nroot:x:0:0:root:/root:/bin/sh\ndaniel:x:1000:1000:Daniel:/home/daniel:/bin/zsh\n\n";
        assert_eq!(parse_principal_names(content), vec!["daniel", "root"]);
    }

    #[test]
    fn dedupes_principal_names_case_insensitively() {
        let content = "Root:x:0\nroot:x:0\nadmin:x:1\n";
        assert_eq!(parse_principal_names(content), vec!["admin", "Root"]);
    }

    #[test]
    fn finds_principal_id_in_colon_separated_file() {
        let dir = temp_dir("principal-id");
        let file = dir.join("passwd");
        fs::write(
            &file,
            "# c\nroot:x:0:0:r:/root:/bin/sh\ndaniel:x:1000:1000::/home/daniel:/bin/zsh\n",
        )
        .unwrap();
        assert_eq!(principal_id_from_file(&file, "daniel"), Some(1000));
        assert_eq!(principal_id_from_file(&file, "root"), Some(0));
        assert_eq!(principal_id_from_file(&file, "nobody-here"), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolves_unique_names_with_counter_suffix() {
        let dir = temp_dir("unique");
        fs::write(dir.join("file.txt"), "x").unwrap();
        assert_eq!(
            unique_target_path(&dir, "file.txt", false),
            dir.join("file (1).txt")
        );
        fs::write(dir.join("file (1).txt"), "x").unwrap();
        assert_eq!(
            unique_target_path(&dir, "file.txt", false),
            dir.join("file (2).txt")
        );
        // No collision: keep the name unchanged.
        assert_eq!(
            unique_target_path(&dir, "other.txt", false),
            dir.join("other.txt")
        );
        // Directories use the full name as stem.
        fs::create_dir(dir.join("data.dir")).unwrap();
        assert_eq!(
            unique_target_path(&dir, "data.dir", true),
            dir.join("data.dir (1)")
        );
        // Dotfiles keep their full name as stem too (no empty stem).
        fs::write(dir.join(".bashrc"), "x").unwrap();
        assert_eq!(
            unique_target_path(&dir, ".bashrc", false),
            dir.join(".bashrc (1)")
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copies_directories_recursively_and_resolves_collisions() {
        let dir = temp_dir("copy");
        let source = dir.join("source");
        fs::create_dir_all(source.join("sub")).unwrap();
        fs::write(source.join("a.txt"), "alpha").unwrap();
        fs::write(source.join("sub").join("b.txt"), "beta").unwrap();
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();

        let first =
            copy_paths_impl(&[source.to_string_lossy().to_string()], &target, false).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            fs::read_to_string(target.join("source").join("a.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(target.join("source").join("sub").join("b.txt")).unwrap(),
            "beta"
        );

        // Second paste into the same target: " (1)" suffix instead of overwrite.
        let second =
            copy_paths_impl(&[source.to_string_lossy().to_string()], &target, false).unwrap();
        assert_eq!(second.len(), 1);
        assert!(target.join("source (1)").join("a.txt").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn moves_files_and_removes_source() {
        let dir = temp_dir("move");
        let source = dir.join("from");
        let target = dir.join("to");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("new.txt"), "new").unwrap();

        copy_paths_impl(
            &[source.join("new.txt").to_string_lossy().to_string()],
            &target,
            true,
        )
        .unwrap();
        assert!(!source.join("new.txt").exists());
        assert_eq!(fs::read_to_string(target.join("new.txt")).unwrap(), "new");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_copying_a_directory_into_itself() {
        let dir = temp_dir("self-copy");
        let source = dir.join("folder");
        fs::create_dir_all(&source).unwrap();
        let result = copy_paths_impl(&[source.to_string_lossy().to_string()], &source, false);
        assert!(result.is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn move_into_same_directory_is_a_no_op() {
        let dir = temp_dir("same-dir-move");
        fs::write(dir.join("file.txt"), "data").unwrap();
        let created = copy_paths_impl(
            &[dir.join("file.txt").to_string_lossy().to_string()],
            &dir,
            true,
        )
        .unwrap();
        assert!(created.is_empty());
        assert_eq!(fs::read_to_string(dir.join("file.txt")).unwrap(), "data");
        fs::remove_dir_all(&dir).ok();
    }

    fn build_archive_fixture(dir: &Path) -> Vec<String> {
        let root = dir.join("payload");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), "alpha").unwrap();
        fs::write(root.join("sub").join("b.txt"), "beta").unwrap();
        let single = dir.join("c.txt");
        fs::write(&single, "gamma").unwrap();
        vec![
            root.to_string_lossy().to_string(),
            single.to_string_lossy().to_string(),
        ]
    }

    #[test]
    fn zip_archive_roundtrip() {
        let dir = temp_dir("zip");
        let sources = build_archive_fixture(&dir);
        let archive = dir.join("out").join("archive.zip");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        create_archive_impl(&sources, &archive, "zip").unwrap();

        let file = fs::File::open(&archive).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut read_entry = |name: &str| -> String {
            let mut entry = zip.by_name(name).unwrap();
            let mut content = String::new();
            entry.read_to_string(&mut content).unwrap();
            content
        };
        assert_eq!(read_entry("payload/a.txt"), "alpha");
        assert_eq!(read_entry("payload/sub/b.txt"), "beta");
        assert_eq!(read_entry("c.txt"), "gamma");
        fs::remove_dir_all(&dir).ok();
    }

    fn assert_tar_contents<R: Read>(archive: tar::Archive<R>) {
        let mut archive = archive;
        let mut found = std::collections::HashMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            let mut content = String::new();
            entry.read_to_string(&mut content).ok();
            found.insert(path, content);
        }
        assert_eq!(
            found.get("payload/a.txt").map(String::as_str),
            Some("alpha")
        );
        assert_eq!(
            found.get("payload/sub/b.txt").map(String::as_str),
            Some("beta")
        );
        assert_eq!(found.get("c.txt").map(String::as_str), Some("gamma"));
    }

    #[test]
    fn tar_archive_roundtrip() {
        let dir = temp_dir("tar");
        let sources = build_archive_fixture(&dir);
        let archive = dir.join("out").join("archive.tar");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        create_archive_impl(&sources, &archive, "tar").unwrap();
        assert_tar_contents(tar::Archive::new(fs::File::open(&archive).unwrap()));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tar_gz_archive_roundtrip() {
        let dir = temp_dir("targz");
        let sources = build_archive_fixture(&dir);
        let archive = dir.join("out").join("archive.tar.gz");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        create_archive_impl(&sources, &archive, "tarGz").unwrap();
        let decoder = flate2::read::GzDecoder::new(fs::File::open(&archive).unwrap());
        assert_tar_contents(tar::Archive::new(decoder));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_archive_target_inside_selected_folder() {
        let dir = temp_dir("archive-self");
        let payload = dir.join("payload");
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join("a.txt"), "alpha").unwrap();
        let result = create_archive_impl(
            &[payload.to_string_lossy().to_string()],
            &payload.join("archive.zip"),
            "zip",
        );
        assert!(result.is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_unknown_archive_format() {
        let dir = temp_dir("format");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let result = create_archive_impl(
            &[dir.join("a.txt").to_string_lossy().to_string()],
            &dir.join("archive.rar"),
            "rar",
        );
        assert!(result.is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn applies_octal_permissions_recursively() {
        use std::os::unix::fs::MetadataExt;
        let dir = temp_dir("chmod");
        let root = dir.join("tree");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("file.txt"), "data").unwrap();

        let result = apply_owner_permissions(
            &[root.to_string_lossy().to_string()],
            None,
            None,
            Some(0o750),
            true,
        )
        .unwrap();
        assert_eq!(result.changed, 1);
        assert_eq!(result.failed, 0);
        let mode = fs::metadata(root.join("sub").join("file.txt"))
            .unwrap()
            .mode();
        assert_eq!(mode & 0o777, 0o750);
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_invalid_octal_permissions_input() {
        // Validation happens before any filesystem access in the command; the
        // helper mirrors that contract here.
        assert_eq!(parse_octal_mode("88"), None);
        assert_eq!(parse_octal_mode(""), None);
    }
}
