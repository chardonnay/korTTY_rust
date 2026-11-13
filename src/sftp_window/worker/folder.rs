//! Expands one ordinary browser folder into independently queued children.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;

use unicode_normalization::UnicodeNormalization as _;

use super::{
    Attributes, Client, ClipboardPolicy, Control, EntryKind, EntryName, Failure, FileKind,
    FolderExpansion, Location, Path, PathBuf, Result, Transfer, files, io, remote_existing,
    source_link, unchanged,
};
use crate::sftp_window::tree::local;

const MAX_ENTRIES: usize = 20_000;
const MAX_LISTING_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn expand(
    client: &mut Client,
    transfer: &Transfer,
    target: &Location,
    control: &Control,
    allow_merge: bool,
) -> Result<FolderExpansion> {
    check(control)?;
    match (&transfer.source, target) {
        (Location::Remote(source), Location::Local(target)) => {
            download(client, source, target, control, allow_merge)
        }
        (Location::Local(source), Location::Remote(target)) => {
            upload(client, source, target, control, allow_merge)
        }
        _ => Err(invalid("Invalid SFTP folder transfer direction")),
    }
}

fn download(
    client: &mut Client,
    source: &[u8],
    target: &Path,
    control: &Control,
    allow_merge: bool,
) -> Result<FolderExpansion> {
    let original = remote_directory(client, source)?;
    check(control)?;
    let destination = LocalDestination::prepare(target, allow_merge)?;
    let existing = local_names(&destination.directory)?;
    let entries = client.read_dir_limited(source, MAX_ENTRIES, MAX_LISTING_BYTES)?;
    let mut names = Names::default();
    let mut children = VecDeque::new();
    let mut skipped = 0;
    for entry in entries {
        check(control)?;
        names.insert(&entry.name)?;
        existing.require_exact_alias(&entry.name)?;
        // Validate the destination component even for entries that will be skipped.
        files::local_name(&entry.name)?;
        let path = files::remote_join(source, &entry.name);
        let attributes = client.lstat(&path)?;
        let (path, kind, size) = match attributes.kind() {
            FileKind::File => (
                path,
                EntryKind::File,
                Some(
                    attributes
                        .size
                        .ok_or_else(|| invalid("Remote file size is missing"))?,
                ),
            ),
            FileKind::Directory => (path, EntryKind::Directory, None),
            FileKind::Symlink => {
                let Some((target, size)) = source_link::remote(client, &path, attributes)? else {
                    skipped += 1;
                    continue;
                };
                names.reserve_path_bytes(target.len())?;
                (target, EntryKind::File, Some(size))
            }
            FileKind::Special => {
                skipped += 1;
                continue;
            }
            FileKind::Unknown => return Err(invalid("Remote child type is missing")),
        };
        children.push_back(Transfer {
            source: Location::Remote(path),
            target_directory: Location::Local(target.to_path_buf()),
            title: files::display(&String::from_utf8_lossy(&entry.name)),
            name: EntryName::Remote(entry.name),
            kind,
            size,
        });
    }
    if !unchanged(original, remote_directory(client, source)?) {
        return Err(invalid(
            "The remote source folder changed during its listing",
        ));
    }
    destination.verify()?;
    check(control)?;
    Ok(completed(children, skipped))
}

fn upload(
    client: &mut Client,
    source: &Path,
    target: &[u8],
    control: &Control,
    allow_merge: bool,
) -> Result<FolderExpansion> {
    let directory = local::Directory::open_path(source)?;
    let modified = directory_modified(&directory)?;
    let parent_path = remote_parent(target)?;
    remote_directory(client, parent_path)?;
    let parent_names = remote_names(client, parent_path)?;
    let name = &target[parent_path.len() + usize::from(parent_path != b"/")..];
    files::validate_transfer_name(name)?;
    parent_names.require_exact_alias(name)?;
    match remote_existing(client, target)? {
        Some(_) if !allow_merge => {
            return Err(invalid("The destination folder name is no longer vacant"));
        }
        Some(_) => {}
        None => {
            check(control)?;
            if let Err(error) = client.mkdir(target) {
                // A concurrent creator may supply an ordinary merge destination.
                // Keep Both must never merge into that newly arrived entry.
                if !allow_merge || error.connection_lost() {
                    return Err(error.into());
                }
                if remote_existing(client, target)?.is_none() {
                    return Err(error.into());
                }
            }
        }
    }
    let original = remote_directory(client, target)?;
    let existing = remote_names(client, target)?;
    let mut names = Names::default();
    let mut children = VecDeque::new();
    let mut skipped = 0;
    let mut entries = directory.names(MAX_ENTRIES)?;
    entries.sort();
    for name in entries {
        check(control)?;
        let wire_name = files::remote_name(&name)?;
        names.insert(&wire_name)?;
        existing.require_exact_alias(&wire_name)?;
        let path = source.join(&name);
        let child = match local_attributes(&directory, &name)? {
            Some((EntryKind::Link, _)) => {
                require_local_path(&directory, source)?;
                let target = source_link::local(&path)?;
                require_local_path(&directory, source)?;
                if let Some((path, size)) = target {
                    names.reserve_path_bytes(path.as_os_str().as_encoded_bytes().len())?;
                    Some((path, EntryKind::File, Some(size)))
                } else {
                    None
                }
            }
            attributes => attributes.map(|(kind, size)| (path, kind, size)),
        };
        let Some((path, kind, size)) = child else {
            skipped += 1;
            continue;
        };
        children.push_back(Transfer {
            source: Location::Local(path),
            target_directory: Location::Remote(target.to_vec()),
            title: files::display(&name.to_string_lossy()),
            name: EntryName::Local(name),
            kind,
            size,
        });
    }
    require_local_path(&directory, source)?;
    if modified != directory_modified(&directory)? {
        return Err(invalid(
            "The local source folder changed during its listing",
        ));
    }
    remote_directory(client, parent_path)?;
    if !unchanged(original, remote_directory(client, target)?) {
        return Err(invalid(
            "The remote destination folder changed during its listing",
        ));
    }
    check(control)?;
    Ok(completed(children, skipped))
}

struct LocalDestination {
    directory: local::Directory,
    parent: local::Directory,
    path: PathBuf,
}

impl LocalDestination {
    fn prepare(path: &Path, allow_merge: bool) -> Result<Self> {
        let parent_path = path
            .parent()
            .ok_or_else(|| invalid("Destination folder has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| invalid("Destination folder has no name"))?;
        let wire_name = files::remote_name(name)?;
        let parent = local::Directory::open_path(parent_path)?;
        local_names(&parent)?.require_exact_alias(&wire_name)?;
        let directory = match parent.create_transfer_child(name) {
            Ok(created) => created,
            Err(error) if allow_merge && error.kind() == io::ErrorKind::AlreadyExists => {
                match local::entry(&parent, name)? {
                    local::Entry::Directory(directory) => directory,
                    _ => return Err(invalid("The destination is not an ordinary folder")),
                }
            }
            Err(error) => return Err(error.into()),
        };
        let destination = Self {
            directory,
            parent,
            path: path.to_path_buf(),
        };
        destination.verify()?;
        Ok(destination)
    }

    fn verify(&self) -> Result<()> {
        require_local_path(&self.parent, self.path.parent().expect("validated parent"))?;
        require_local_path(&self.directory, &self.path)
    }
}

fn require_local_path(directory: &local::Directory, path: &Path) -> Result<()> {
    if directory.same_as(&local::Directory::open_path(path)?)? {
        Ok(())
    } else {
        Err(invalid("The local folder path changed during its listing"))
    }
}

fn directory_modified(directory: &local::Directory) -> io::Result<std::time::SystemTime> {
    #[cfg(unix)]
    return directory.file().metadata()?.modified();
    #[cfg(windows)]
    return directory.metadata()?.modified();
}

#[cfg(windows)]
fn local_attributes(
    directory: &local::Directory,
    name: &OsStr,
) -> Result<Option<(EntryKind, Option<u64>)>> {
    let Some(file) = directory.open_entry_metadata(name)? else {
        // The metadata-only open also rejects non-link reparse points. The
        // source resolver checks the link type before attempting resolution.
        return Ok(Some((EntryKind::Link, None)));
    };
    let metadata = file.metadata()?;
    if metadata.is_dir() {
        Ok(Some((EntryKind::Directory, None)))
    } else if metadata.is_file() {
        Ok(Some((EntryKind::File, Some(metadata.len()))))
    } else {
        Ok(None)
    }
}

#[cfg(unix)]
fn local_attributes(
    directory: &local::Directory,
    name: &OsStr,
) -> Result<Option<(EntryKind, Option<u64>)>> {
    use rustix::fs::{AtFlags, FileType, statat};
    let metadata =
        statat(directory.file(), name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    match FileType::from_raw_mode(metadata.st_mode) {
        FileType::Directory => Ok(Some((EntryKind::Directory, None))),
        FileType::RegularFile => Ok(Some((
            EntryKind::File,
            Some(
                u64::try_from(metadata.st_size)
                    .map_err(|_| invalid("Local file size is invalid"))?,
            ),
        ))),
        FileType::Symlink => Ok(Some((EntryKind::Link, None))),
        _ => Ok(None),
    }
}

fn remote_directory(client: &mut Client, path: &[u8]) -> Result<Attributes> {
    let attributes = client.lstat(path)?;
    if !path.starts_with(b"/")
        || attributes.kind() != FileKind::Directory
        || client.canonicalize(path)? != path
    {
        return Err(invalid(
            "Remote folder is missing, a link, or no longer at its expected path",
        ));
    }
    Ok(attributes)
}

fn remote_parent(path: &[u8]) -> Result<&[u8]> {
    let slash = path
        .iter()
        .rposition(|byte| *byte == b'/')
        .ok_or_else(|| invalid("Remote destination has no parent"))?;
    if !path.starts_with(b"/") || slash + 1 == path.len() {
        return Err(invalid(
            "Remote destination is not an absolute named folder",
        ));
    }
    Ok(&path[..slash.max(1)])
}

fn remote_names(client: &mut Client, path: &[u8]) -> Result<Names> {
    let mut names = Names::default();
    for entry in client.read_dir_limited(path, MAX_ENTRIES, MAX_LISTING_BYTES)? {
        names.insert(&entry.name)?;
    }
    Ok(names)
}

fn local_names(directory: &local::Directory) -> Result<Names> {
    let mut names = Names::default();
    for name in directory.names(MAX_ENTRIES)? {
        names.insert(&files::remote_name(&name)?)?;
    }
    Ok(names)
}

#[derive(Default)]
struct Names {
    aliases: HashMap<Vec<u8>, Vec<u8>>,
    bytes: usize,
}

impl Names {
    fn checked_bytes(&self, additional: usize) -> Result<usize> {
        self.bytes
            .checked_add(additional)
            .filter(|bytes| *bytes <= MAX_LISTING_BYTES)
            .ok_or_else(listing_limit)
    }

    fn reserve_path_bytes(&mut self, length: usize) -> Result<()> {
        self.bytes = self.checked_bytes(length)?;
        Ok(())
    }

    fn insert(&mut self, name: &[u8]) -> Result<()> {
        files::validate_transfer_name(name)?;
        let bytes = self.checked_bytes(name.len())?;
        if self.aliases.len() >= MAX_ENTRIES {
            return Err(listing_limit());
        }
        let key = alias(name);
        if self.aliases.contains_key(&key) {
            return Err(invalid(
                "Folder contains duplicate or ambiguous destination names",
            ));
        }
        self.aliases.insert(key, name.to_vec());
        self.bytes = bytes;
        Ok(())
    }

    fn require_exact_alias(&self, name: &[u8]) -> Result<()> {
        if self
            .aliases
            .get(&alias(name))
            .is_some_and(|existing| existing != name)
        {
            Err(invalid(
                "Destination contains a different spelling of the requested name",
            ))
        } else {
            Ok(())
        }
    }
}

fn alias(name: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(name) {
        Ok(name) => {
            let folded: String = name.nfc().flat_map(char::to_uppercase).nfc().collect();
            [vec![0], folded.into_bytes()].concat()
        }
        Err(_) => [vec![1], name.to_ascii_uppercase()].concat(),
    }
}

fn completed(children: VecDeque<Transfer>, skipped: usize) -> FolderExpansion {
    let note = format!(
        "Folder ready: {} queued, {skipped} links or special entries skipped",
        children.len()
    );
    FolderExpansion {
        children,
        skipped,
        note,
    }
}

fn check(control: &Control) -> Result<()> {
    if control.is_closed() {
        return Err(Failure::disconnected(
            "Folder expansion was cancelled or the connection closed",
        ));
    }
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some() || !policy.file_transfer_allowed {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Application policy disables file transfer",
        )
        .into());
    }
    Ok(())
}

fn invalid(message: &str) -> Failure {
    io::Error::other(message).into()
}

fn listing_limit() -> Failure {
    invalid("Folder listing exceeds 20,000 entries or 2 MiB")
}

#[cfg(test)]
mod tests;
