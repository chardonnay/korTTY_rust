//! Bounded, read-only manifests of the exact paths below checked folders.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kortty_ssh::sftp::Attributes;

use crate::sftp_window::worker::resume::persist::Identity;
use crate::sftp_window::worker::unchanged;

use super::*;

const MAX_RELATIVE_BYTES: usize = 4096;
const MAX_TOTAL_PATH_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::sftp_window) struct Relative {
    bytes: Vec<u8>,
    local_only: bool,
}

impl Relative {
    fn child(&self, name: &[u8], local_only: bool) -> io::Result<Self> {
        let length = self.bytes.len() + usize::from(!self.bytes.is_empty()) + name.len();
        if length > MAX_RELATIVE_BYTES {
            return Err(io::Error::other(
                "A relative path exceeds the folder comparison limit",
            ));
        }
        Ok(Self {
            bytes: files::remote_join(&self.bytes, name),
            local_only: self.local_only || local_only,
        })
    }

    pub(in crate::sftp_window) fn parent(&self) -> Option<Self> {
        let split = self.bytes.iter().rposition(|&byte| byte == b'/')?;
        let bytes = self.bytes[..split].to_vec();
        let local_only = self.local_only && std::str::from_utf8(&bytes).is_err();
        Some(Self { bytes, local_only })
    }

    pub(in crate::sftp_window) fn display(&self) -> String {
        if let Ok(path) = std::str::from_utf8(&self.bytes) {
            let mut label = String::new();
            for character in path.chars() {
                if character == '\\'
                    || character.is_control()
                    || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                {
                    label.extend(character.escape_default());
                } else {
                    label.push(character);
                }
            }
            label
        } else {
            self.bytes.escape_ascii().to_string()
        }
    }

    pub(in crate::sftp_window) fn review_label(&self) -> String {
        self.bytes.escape_ascii().to_string()
    }

    pub(in crate::sftp_window) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(in crate::sftp_window) fn basename(&self) -> &[u8] {
        self.bytes
            .rsplit(|&byte| byte == b'/')
            .next()
            .unwrap_or_default()
    }
}

#[derive(Clone)]
pub(in crate::sftp_window) enum Entry {
    LocalFolder(Identity),
    LocalFile {
        stamp: LocalStamp,
        identity: Identity,
    },
    Remote(Attributes),
    Blocked,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::sftp_window) enum Kind {
    Folder,
    File,
    Blocked,
}

impl Entry {
    pub(in crate::sftp_window) fn kind(&self) -> Kind {
        match self {
            Self::LocalFolder(_) => Kind::Folder,
            Self::LocalFile { .. } => Kind::File,
            Self::Remote(attributes) => match attributes.kind() {
                FileKind::Directory => Kind::Folder,
                FileKind::File => Kind::File,
                _ => Kind::Blocked,
            },
            Self::Blocked => Kind::Blocked,
        }
    }

    pub(in crate::sftp_window) fn file_metadata(&self) -> Option<(u64, SystemTime)> {
        match self {
            Self::LocalFile { stamp, .. } => Some((stamp.size, stamp.modified?)),
            Self::Remote(attributes) if attributes.kind() == FileKind::File => Some((
                attributes.size?,
                UNIX_EPOCH.checked_add(Duration::from_secs(u64::from(attributes.modified?)))?,
            )),
            _ => None,
        }
    }

    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::LocalFolder(a), Self::LocalFolder(b)) => a == b,
            (
                Self::LocalFile {
                    stamp: a,
                    identity: ai,
                },
                Self::LocalFile {
                    stamp: b,
                    identity: bi,
                },
            ) => a == b && ai == bi,
            (Self::Remote(a), Self::Remote(b)) => unchanged(*a, *b),
            (Self::Blocked, Self::Blocked) => true,
            _ => false,
        }
    }
}

#[derive(Clone)]
pub(in crate::sftp_window) enum DirectoryStamp {
    Local(Identity),
    Remote(Attributes),
}

pub(in crate::sftp_window) struct Manifest {
    pub(in crate::sftp_window) root: Location,
    pub(in crate::sftp_window) entries: BTreeMap<Relative, Entry>,
    stamp: DirectoryStamp,
}

pub(in crate::sftp_window) fn read_pair(
    client: &mut Sftp<impl Read, impl Write>,
    roots: [Location; 2],
    excluded: impl Fn(&[u8]) -> bool,
    cancelled: impl Fn() -> bool,
) -> Result<[Manifest; 2]> {
    let mut walk = Walk::new(&excluded, &cancelled);
    let [left, right] = roots;
    let left = walk.read(client, left)?;
    let right = walk.read(client, right)?;
    walk.check(0)?;
    left.verify(client)?;
    right.verify(client)?;
    Ok([left, right])
}

pub(in crate::sftp_window) fn read_local(
    root: &std::path::Path,
    excluded: &dyn Fn(&[u8]) -> bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Manifest> {
    let mut walk = Walk::new(excluded, cancelled);
    let dir = local::Directory::open_path(root)?;
    let stamp = Identity::directory(&dir)?;
    let mut entries = BTreeMap::new();
    walk.local(&dir, &Relative::default(), 0, &mut entries)?;
    walk.check(0)?;
    if Identity::directory(&local::Directory::open_path(root)?)? != stamp {
        return Err(io::Error::other("The local source folder changed").into());
    }
    Ok(Manifest {
        root: Location::Local(root.to_owned()),
        entries,
        stamp: DirectoryStamp::Local(stamp),
    })
}

pub(in crate::sftp_window) fn read_remote_search(
    client: &mut Sftp<impl Read, impl Write>,
    root: Vec<u8>,
    excluded: impl Fn(&[u8]) -> bool,
    cancelled: impl Fn() -> bool,
) -> Result<Manifest> {
    let mut walk = Walk::new(&excluded, &cancelled);
    walk.remote_search = true;
    walk.read(client, Location::Remote(root))
}

pub(in crate::sftp_window) fn read_remote_checked(
    client: &mut Sftp<impl Read, impl Write>,
    root: Vec<u8>,
    max_entries: usize,
    validate: &dyn Fn(Attributes) -> io::Result<()>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Manifest> {
    let mut walk = Walk::new(&|_| false, cancelled);
    walk.remaining = max_entries.min(MAX_ENTRIES);
    walk.remote_search = true;
    walk.validate_remote = Some(validate);
    walk.read(client, Location::Remote(root))
}

pub(in crate::sftp_window) fn read_folder_sizes(
    client: &mut Sftp<impl Read, impl Write>,
    roots: Vec<Location>,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<Manifest>> {
    let mut walk = Walk::new(&|_| false, &cancelled);
    // Measurement never publishes names into the other filesystem.
    walk.remote_search = true;
    walk.local_read_only = true;
    let manifests = roots
        .into_iter()
        .map(|root| walk.read(client, root))
        .collect::<Result<Vec<_>>>()?;
    for manifest in &manifests {
        walk.check(0)?;
        manifest.verify(client)?;
    }
    Ok(manifests)
}

impl Manifest {
    pub(in crate::sftp_window) fn review_bytes(
        &self,
        mut write: impl FnMut(&[u8]),
    ) -> io::Result<()> {
        fn remote(attributes: Attributes) -> impl serde::Serialize {
            (
                attributes.size,
                attributes.uid,
                attributes.gid,
                attributes.permissions,
                attributes.modified,
            )
        }

        let root = match &self.stamp {
            DirectoryStamp::Local(identity) => serde_json::to_vec(&(0, identity))?,
            DirectoryStamp::Remote(attributes) => serde_json::to_vec(&(1, remote(*attributes)))?,
        };
        write(&root);
        for (path, entry) in &self.entries {
            let fields = (&path.bytes, path.local_only);
            let bytes = match entry {
                Entry::LocalFolder(identity) => serde_json::to_vec(&(fields, 0, identity))?,
                Entry::LocalFile { stamp, identity } => {
                    serde_json::to_vec(&(fields, 1, stamp.review_fields(), identity))?
                }
                Entry::Remote(attributes) => serde_json::to_vec(&(fields, 2, remote(*attributes)))?,
                Entry::Blocked => serde_json::to_vec(&(fields, 3))?,
            };
            write(&bytes);
        }
        Ok(())
    }

    fn verify(&self, client: &mut Sftp<impl Read, impl Write>) -> Result<()> {
        self.stamp.verify(client, &self.root, true).map(|_| ())
    }

    pub(in crate::sftp_window) fn root_directory(&self) -> (Location, DirectoryStamp) {
        (self.root.clone(), self.stamp.clone())
    }

    pub(in crate::sftp_window) fn matches(&self, other: &Self) -> bool {
        self.root == other.root
            && self.stamp.matches(&other.stamp)
            && self.entries.len() == other.entries.len()
            && self.entries.iter().all(|(key, entry)| {
                other
                    .entries
                    .get(key)
                    .is_some_and(|current| entry.matches(current))
            })
    }

    pub(in crate::sftp_window) fn location(&self, relative: &Relative) -> io::Result<Location> {
        if relative.local_only {
            return Err(io::Error::other(
                "The name cannot be copied between these filesystems",
            ));
        }
        match &self.root {
            Location::Local(root) => {
                let mut path = root.clone();
                for part in relative.bytes.split(|&byte| byte == b'/') {
                    path.push(files::local_name(part)?);
                }
                Ok(Location::Local(path))
            }
            Location::Remote(root) => {
                Ok(Location::Remote(files::remote_join(root, &relative.bytes)))
            }
        }
    }

    pub(in crate::sftp_window) fn parent(
        &self,
        relative: &Relative,
    ) -> Option<(Location, DirectoryStamp)> {
        match relative.parent() {
            Some(parent) => self.directory(&parent),
            None => Some((self.root.clone(), self.stamp.clone())),
        }
    }

    pub(in crate::sftp_window) fn directory(
        &self,
        relative: &Relative,
    ) -> Option<(Location, DirectoryStamp)> {
        let stamp = match self.entries.get(relative)? {
            Entry::LocalFolder(stamp) => DirectoryStamp::Local(stamp.clone()),
            Entry::Remote(stamp) if stamp.kind() == FileKind::Directory => {
                DirectoryStamp::Remote(*stamp)
            }
            _ => return None,
        };
        Some((self.location(relative).ok()?, stamp))
    }
}

impl DirectoryStamp {
    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(a), Self::Local(b)) => a == b,
            (Self::Remote(a), Self::Remote(b)) => unchanged(*a, *b),
            _ => false,
        }
    }

    pub(in crate::sftp_window) fn verify(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        location: &Location,
        strict: bool,
    ) -> Result<Self> {
        let current = match (location, self) {
            (Location::Local(path), Self::Local(stamp)) => {
                let identity = Identity::directory(&local::Directory::open_path(path)?)?;
                (identity == *stamp).then_some(Self::Local(identity))
            }
            (Location::Remote(path), Self::Remote(stamp)) => {
                remote_directory(client, path, false)?;
                let current = client.lstat(path)?;
                let same = if strict {
                    unchanged(*stamp, current)
                } else {
                    current.kind() == FileKind::Directory
                        && stamp.uid == current.uid
                        && stamp.gid == current.gid
                        && stamp.permissions == current.permissions
                };
                same.then_some(Self::Remote(current))
            }
            _ => None,
        };
        current.ok_or_else(|| io::Error::other("A folder changed during comparison").into())
    }
}

struct Walk<'a> {
    excluded: &'a dyn Fn(&[u8]) -> bool,
    cancelled: &'a dyn Fn() -> bool,
    remaining: usize,
    path_bytes: usize,
    remote_search: bool,
    local_read_only: bool,
    validate_remote: Option<&'a dyn Fn(Attributes) -> io::Result<()>>,
}

impl<'a> Walk<'a> {
    fn new(excluded: &'a dyn Fn(&[u8]) -> bool, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            excluded,
            cancelled,
            remaining: MAX_ENTRIES,
            path_bytes: MAX_TOTAL_PATH_BYTES,
            remote_search: false,
            local_read_only: false,
            validate_remote: None,
        }
    }

    fn read(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        root: Location,
    ) -> Result<Manifest> {
        self.check(0)?;
        let mut entries = BTreeMap::new();
        let relative = Relative::default();
        let stamp = match &root {
            Location::Local(path) => {
                let dir = local::Directory::open_path(path)?;
                let stamp = Identity::directory(&dir)?;
                self.local(&dir, &relative, 0, &mut entries)?;
                DirectoryStamp::Local(stamp)
            }
            Location::Remote(path) => {
                let drive_path = matches!(path.as_slice(), [letter, b':', b'/', ..] if letter.is_ascii_alphabetic());
                if (!path.starts_with(b"/") && !(self.local_read_only && drive_path))
                    || path.contains(&0)
                    || windows_drive(path) && !self.local_read_only
                {
                    return Err(io::Error::other(
                        "Folder comparison requires a canonical non-drive SFTP path",
                    )
                    .into());
                }
                remote_directory(client, path, false)?;
                let stamp = client.lstat(path)?;
                if stamp.kind() != FileKind::Directory {
                    return Err(io::Error::other("The remote comparison folder changed").into());
                }
                if let Some(validate) = self.validate_remote {
                    validate(stamp)?;
                }
                self.remote(client, path, &relative, 0, &mut entries)?;
                DirectoryStamp::Remote(stamp)
            }
        };
        let manifest = Manifest {
            root,
            entries,
            stamp,
        };
        manifest.verify(client)?;
        Ok(manifest)
    }

    fn local(
        &mut self,
        dir: &local::Directory,
        relative: &Relative,
        depth: usize,
        entries: &mut BTreeMap<Relative, Entry>,
    ) -> Result<()> {
        self.check(depth)?;
        let names = dir.names(self.remaining)?;
        let mut seen = HashSet::with_capacity(names.len());
        for name in &names {
            self.check(depth)?;
            if !seen.insert(name) {
                return Err(io::Error::other("A folder returned a duplicate name").into());
            }
            let bytes = local_name_bytes(name);
            let local_only = cfg!(windows) && std::str::from_utf8(bytes).is_err();
            let key = self.entry(relative, bytes, local_only)?;
            if (self.excluded)(&key.bytes) {
                continue;
            }
            let entry = if !self.local_read_only && !portable(bytes, local_only) {
                Entry::Blocked
            } else {
                match local::entry(dir, name)? {
                    local::Entry::Directory(child) => {
                        self.local(&child, &key, depth + 1, entries)?;
                        let local::Entry::Directory(current) = local::entry(dir, name)? else {
                            return Err(
                                io::Error::other("A local comparison folder changed").into()
                            );
                        };
                        if !child.same_as(&current)? {
                            return Err(
                                io::Error::other("A local comparison folder changed").into()
                            );
                        }
                        Entry::LocalFolder(Identity::directory(&child)?)
                    }
                    local::Entry::File(file) => Entry::LocalFile {
                        stamp: LocalStamp::capture(&file.metadata()?),
                        identity: Identity::capture(&file)?,
                    },
                    local::Entry::Skip => Entry::Blocked,
                }
            };
            entries.insert(key, entry);
        }
        Ok(())
    }

    fn remote(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        path: &[u8],
        relative: &Relative,
        depth: usize,
        entries: &mut BTreeMap<Relative, Entry>,
    ) -> Result<()> {
        self.check(depth)?;
        remote_directory(client, path, false)?;
        let listing = client.read_dir_limited(path, self.remaining + 2, MAX_LISTING_PAYLOAD)?;
        let mut seen = HashSet::with_capacity(listing.len());
        for item in &listing {
            self.check(depth)?;
            if !seen.insert(item.name.as_slice()) {
                return Err(io::Error::other("The server returned a duplicate folder name").into());
            }
            let key = self.entry(relative, &item.name, false)?;
            if (self.excluded)(&key.bytes) {
                continue;
            }
            let safe_name = if self.remote_search {
                files::validate_transfer_name(&item.name).is_ok()
            } else {
                portable(&item.name, false)
            };
            let entry = if safe_name {
                remote_directory(client, path, false)?;
                let child = files::remote_join(path, &item.name);
                let attributes = client.lstat(&child)?;
                if !self.remote_search
                    && files::replacement_stage_id(&item.name).is_some()
                    && attributes.kind() == FileKind::Directory
                    && attributes.permissions.map(|mode| mode & 0o7777) == Some(0o700)
                {
                    continue;
                }
                if let Some(validate) = self.validate_remote {
                    validate(attributes)?;
                }
                if attributes.kind() == FileKind::Directory {
                    self.remote(client, &child, &key, depth + 1, entries)?;
                    if !unchanged(attributes, client.lstat(&child)?) {
                        return Err(io::Error::other("A remote comparison folder changed").into());
                    }
                }
                Entry::Remote(attributes)
            } else {
                Entry::Blocked
            };
            entries.insert(key, entry);
        }
        remote_directory(client, path, false)
    }

    fn entry(&mut self, parent: &Relative, name: &[u8], local_only: bool) -> io::Result<Relative> {
        let relative = parent.child(name, local_only)?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("Folder walk exceeds its entry limit"))?;
        self.path_bytes = self
            .path_bytes
            .checked_sub(relative.bytes.len())
            .ok_or_else(|| io::Error::other("Folder comparison exceeds its path data limit"))?;
        Ok(relative)
    }

    fn check(&self, depth: usize) -> io::Result<()> {
        if (self.cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Folder comparison cancelled",
            ));
        }
        if depth > MAX_DEPTH {
            return Err(io::Error::other(
                "Folder comparison exceeds 32 nested levels",
            ));
        }
        Ok(())
    }
}

fn local_name_bytes(name: &OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        name.as_bytes()
    }
    #[cfg(windows)]
    {
        name.as_encoded_bytes()
    }
}

fn portable(name: &[u8], local_only: bool) -> bool {
    !local_only && files::validate_transfer_name(name).is_ok() && files::local_name(name).is_ok()
}

pub(in crate::sftp_window) fn windows_drive(path: &[u8]) -> bool {
    matches!(path, [b'/', letter, b':', rest @ ..] if letter.is_ascii_alphabetic() && (rest.is_empty() || rest.starts_with(b"/")))
}
