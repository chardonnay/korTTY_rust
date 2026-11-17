//! Held local directory operations for a reviewed whole-tree replacement.

use std::ffi::OsStr;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Component, Path};

use serde::Serialize;

use super::Directory;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct ReviewedVolume(pub(super) u64, pub(super) u64);

impl ReviewedVolume {
    pub(crate) fn mount_identity(self) -> (u64, u64) {
        (self.0, self.1)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct ReviewedStamp {
    pub(super) volume: ReviewedVolume,
    pub(super) identity: [u8; 16],
    pub(super) permissions: u32,
    pub(super) owner: u32,
    pub(super) group: u32,
    pub(super) attributes: [u8; 32],
    pub(super) size: u64,
    pub(super) created: i64,
    pub(super) modified: (i64, i64),
    pub(super) changed: (i64, i64),
}

impl ReviewedStamp {
    pub(crate) fn volume(&self) -> ReviewedVolume {
        self.volume
    }

    pub(crate) fn for_tree_parent(self) -> Self {
        Self {
            size: 0,
            modified: (0, 0),
            changed: (0, 0),
            ..self
        }
    }

    pub(crate) fn hash_into(&self, hasher: &mut impl Hasher) {
        self.hash(hasher);
    }

    pub(crate) fn hash_identity_into(&self, hasher: &mut impl Hasher) {
        // Bind aliases by their physical device and object, including Linux bind mounts.
        self.volume.0.hash(hasher);
        self.identity.hash(hasher);
    }

    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        self.volume == other.volume && self.identity == other.identity
    }

    pub(crate) fn same_after_move(&self, other: &Self) -> bool {
        Self {
            changed: other.changed,
            ..*self
        } == *other
    }

    pub(crate) fn same_after_first_publication(&self, other: &Self) -> bool {
        // Windows may restore the creation time associated with a recently
        // removed destination name when the new file is renamed into place.
        #[cfg(windows)]
        let expected = Self {
            created: other.created,
            ..*self
        };
        #[cfg(unix)]
        let expected = *self;
        expected.same_after_move(other)
    }
}

pub(crate) fn open_reviewed_tree_parent(path: &Path) -> io::Result<Directory> {
    if path.to_str().is_none_or(crate::ssh_dialog::unsafe_text) {
        return Err(changed());
    }
    #[cfg(unix)]
    let parent = Directory::open_path(path)?;
    #[cfg(windows)]
    let parent = Directory::open_reviewed_removal_parent(path)?;
    #[cfg(windows)]
    {
        let canonical = path.canonicalize()?;
        let spelling = |value: &Path| {
            value
                .to_str()
                .map(|path| {
                    path.strip_prefix(r"\\?\")
                        .unwrap_or(path)
                        .replace('/', "\\")
                })
                .ok_or_else(changed)
        };
        if !spelling(path)?.eq_ignore_ascii_case(&spelling(&canonical)?) {
            return Err(changed());
        }
    }
    reviewed_tree_parent_stamp(&parent)?;
    Ok(parent)
}

pub(crate) fn open_reviewed_tree_child(parent: &Directory, name: &OsStr) -> io::Result<Directory> {
    open_reviewed_child(parent, name, true)
}

pub(crate) fn open_reviewed_tree_child_for_files(
    parent: &Directory,
    name: &OsStr,
) -> io::Result<Directory> {
    // The file publisher holds shared locks on the same ancestors.
    open_reviewed_child(parent, name, false)
}

fn open_reviewed_child(parent: &Directory, name: &OsStr, exclusive: bool) -> io::Result<Directory> {
    check_component(name)?;
    #[cfg(unix)]
    let child = {
        use rustix::fs::FlockOperation;
        let lock = if exclusive {
            FlockOperation::NonBlockingLockExclusive
        } else {
            FlockOperation::NonBlockingLockShared
        };
        super::unix::hold_reviewed_child(parent, name, lock)?
    };
    #[cfg(windows)]
    let child = {
        let directory = if exclusive {
            Directory::from_file(parent.child_for_reviewed_removal(name, true)?)?
        } else {
            parent.open_reviewed_child_for_files(name)?
        };
        directory.require_reviewed_tree_basename(name)?;
        directory
    };
    if reviewed_directory_stamp(&child)?.volume() != reviewed_tree_volume(parent)? {
        return Err(changed());
    }
    Ok(child)
}

pub(crate) fn hold_reviewed_file_ancestors(path: &Path) -> io::Result<Vec<Directory>> {
    #[cfg(unix)]
    return super::unix::hold_reviewed_file_ancestors(path);
    #[cfg(windows)]
    return Directory::hold_reviewed_removal_ancestors(path);
}

pub(crate) fn create_reviewed_tree_stage(
    parent: &Directory,
    name: &OsStr,
) -> io::Result<Directory> {
    check_component(name)?;
    #[cfg(unix)]
    return parent.create_child(name);
    #[cfg(windows)]
    return parent.create_reviewed_tree_stage(name);
}

pub(crate) fn create_reviewed_tree_file(parent: &Directory, name: &OsStr) -> io::Result<File> {
    check_component(name)?;
    #[cfg(unix)]
    return parent.create_file(name);
    #[cfg(windows)]
    return parent.create_replacement_file(name);
}

pub(crate) fn create_reviewed_tree_read_write_file(
    parent: &Directory,
    name: &OsStr,
) -> io::Result<File> {
    check_component(name)?;
    parent.create_read_write_file(name)
}

pub(crate) fn remove_empty_reviewed_tree_stage(
    parent: &Directory,
    name: &OsStr,
    stage: Directory,
) -> io::Result<()> {
    check_component(name)?;
    #[cfg(unix)]
    return super::unix::remove_empty_stage(parent, name, stage);
    #[cfg(windows)]
    {
        require_binding(parent, name, &stage)?;
        stage.require_owner_only()?;
        stage.remove()
    }
}

/// Removes one checked entry from a private stage. Directory removal requires emptiness.
pub(crate) fn remove_reviewed_tree_entry(
    parent: &Directory,
    name: &OsStr,
    expected: &ReviewedStamp,
    directory: bool,
) -> io::Result<()> {
    check_component(name)?;
    if reviewed_tree_volume(parent)? != expected.volume() {
        return Err(changed());
    }
    #[cfg(unix)]
    return super::unix::remove_reviewed_entry(parent, name, expected, directory);
    #[cfg(windows)]
    {
        let file = parent.child_for_reviewed_removal(name, directory)?;
        let (volume, identity) = kortty_files::tree::file_identity(&file)?;
        if identity != expected.identity || ReviewedVolume(volume, 0) != expected.volume() {
            return Err(changed());
        }
        kortty_files::tree::discard_stage_entry(file)
    }
}

/// Renames one held directory without replacing an occupant at the target.
/// After any error, check the actual names before retrying or discarding a stage.
pub(crate) fn move_reviewed_tree(
    source_parent: &Directory,
    source_name: &OsStr,
    source: &Directory,
    destination_parent: &Directory,
    target: &OsStr,
    authorize: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    check_component(source_name)?;
    check_component(target)?;
    let volume = reviewed_tree_volume(source)?;
    if reviewed_tree_volume(source_parent)? != volume
        || reviewed_tree_volume(destination_parent)? != volume
    {
        return Err(changed());
    }
    let recheck = || {
        require_binding(source_parent, source_name, source)?;
        authorize()?;
        require_binding(source_parent, source_name, source)
    };
    #[cfg(unix)]
    {
        recheck()?;
        super::unix::move_directory(source_parent, source_name, destination_parent, target)?;
    }
    #[cfg(windows)]
    {
        let mut attempt = 0;
        loop {
            recheck()?;
            match destination_parent.publish_directory_once(source, target) {
                Ok(()) => break,
                Err(error) => {
                    let Some(delay) = Directory::publish_directory_retry_delay(&error, attempt)
                    else {
                        return Err(error);
                    };
                    attempt += 1;
                    std::thread::sleep(delay);
                }
            }
        }
    }
    require_binding(destination_parent, target, source)
}

fn require_binding(parent: &Directory, name: &OsStr, expected: &Directory) -> io::Result<()> {
    if let super::Entry::Directory(found) = super::entry(parent, name)?
        && found.same_as(expected)?
        && reviewed_tree_volume(&found)? == reviewed_tree_volume(expected)?
    {
        Ok(())
    } else {
        Err(changed())
    }
}

pub(crate) fn sync_reviewed_tree_directory(directory: &Directory) -> io::Result<()> {
    #[cfg(unix)]
    return directory.file().sync_all();
    // Windows synchronously confirms the handle-relative rename. A Windows
    // directory handle does not provide a portable directory fsync operation.
    #[cfg(windows)]
    {
        let _ = directory;
        Ok(())
    }
}

pub(crate) fn reviewed_tree_volume(directory: &Directory) -> io::Result<ReviewedVolume> {
    #[cfg(unix)]
    return super::unix::reviewed_volume(directory.file());
    #[cfg(windows)]
    return Ok(ReviewedVolume(directory.identity()?.0, 0));
}

pub(crate) fn reviewed_tree_parent_stamp(directory: &Directory) -> io::Result<ReviewedStamp> {
    #[cfg(unix)]
    let stamp = super::unix::reviewed_stamp(directory.file(), true, true)?;
    #[cfg(windows)]
    let stamp = reviewed_directory_stamp(directory)?;
    Ok(stamp.for_tree_parent())
}

// A durable shared-parent receipt must distinguish a reused directory identity.
#[cfg(target_os = "linux")]
pub(crate) fn reviewed_tree_parent_birth(
    directory: &Directory,
    _: &ReviewedStamp,
) -> Option<(i64, u32)> {
    use rustix::fs::{AtFlags, StatxFlags, fstatfs, statx};

    let file = directory.file();
    // ext, Btrfs, XFS, F2FS and tmpfs can expose a native birth timestamp.
    let kind = fstatfs(file).ok()?.f_type as u32;
    if !matches!(
        kind,
        0xef53 | 0x9123_683e | 0x5846_5342 | 0xf2f5_2010 | 0x0102_1994
    ) {
        return None;
    }
    let stat = statx(file, "", AtFlags::EMPTY_PATH, StatxFlags::BTIME).ok()?;
    let birth = (stat.stx_btime.tv_sec, stat.stx_btime.tv_nsec);
    (stat.stx_mask & StatxFlags::BTIME.bits() != 0 && birth != (0, 0)).then_some(birth)
}

#[cfg(target_os = "macos")]
pub(crate) fn reviewed_tree_parent_birth(
    directory: &Directory,
    _: &ReviewedStamp,
) -> Option<(i64, u32)> {
    let file = directory.file();
    let filesystem = rustix::fs::fstatfs(file)
        .ok()?
        .f_fstypename
        .map(|byte| byte as u8);
    if std::ffi::CStr::from_bytes_until_nul(&filesystem)
        .ok()?
        .to_bytes()
        != b"apfs"
    {
        return None;
    }
    let birth = file
        .metadata()
        .ok()?
        .created()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some((i64::try_from(birth.as_secs()).ok()?, birth.subsec_nanos()))
}

#[cfg(windows)]
pub(crate) fn reviewed_tree_parent_birth(
    directory: &Directory,
    stamp: &ReviewedStamp,
) -> Option<(i64, u32)> {
    (stamp.created > 0 && directory.supports_reviewed_parent_birth()).then_some((stamp.created, 0))
}

pub(crate) fn reviewed_directory_stamp(directory: &Directory) -> io::Result<ReviewedStamp> {
    #[cfg(unix)]
    return super::unix::reviewed_stamp(directory.file(), true, false);
    #[cfg(windows)]
    return directory.reviewed_tree_metadata().map(windows_stamp);
}

pub(crate) fn reviewed_file_stamp(file: &File) -> io::Result<ReviewedStamp> {
    #[cfg(unix)]
    return super::unix::reviewed_stamp(file, false, false);
    #[cfg(windows)]
    return kortty_files::tree::reviewed_tree_file_metadata(file).map(windows_stamp);
}

#[cfg(windows)]
fn windows_stamp(metadata: kortty_files::tree::ReviewedTreeMetadata) -> ReviewedStamp {
    use sha2::{Digest, Sha256};

    ReviewedStamp {
        volume: ReviewedVolume(metadata.identity.0, 0),
        identity: metadata.identity.1,
        permissions: metadata.attributes,
        owner: 0,
        group: 0,
        attributes: Sha256::digest(metadata.access.review_bytes()).into(),
        size: metadata.size,
        created: metadata.created,
        modified: (metadata.modified, 0),
        changed: (metadata.changed, 0),
    }
}

pub(crate) fn copy_reviewed_file_metadata(from: &File, to: &File) -> io::Result<()> {
    #[cfg(unix)]
    return super::unix::copy_reviewed_metadata(from, to, false);
    #[cfg(windows)]
    return kortty_files::tree::copy_reviewed_tree_file_metadata(from, to);
}

pub(crate) fn copy_reviewed_dir_metadata(from: &Directory, to: &Directory) -> io::Result<()> {
    #[cfg(unix)]
    return super::unix::copy_reviewed_metadata(from.file(), to.file(), true);
    #[cfg(windows)]
    return to.copy_reviewed_tree_metadata(from);
}

fn check_component(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if let (Some(Component::Normal(component)), None) = (components.next(), components.next())
        && component == name
    {
        Ok(())
    } else {
        Err(changed())
    }
}

pub(super) fn changed() -> io::Error {
    io::Error::other("The reviewed local tree, identity, permissions, or volume changed")
}
