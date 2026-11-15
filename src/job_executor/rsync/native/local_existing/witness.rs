//! Exact, bounded observations and copies of ordinary local directory trees.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::hash::Hasher;
use std::io::{self, Read, Write};

use kortty_server_archive::rsync_tree::{self as protocol, DeleteScope, Inventory, Manifest};
use sha2::{Digest as _, Sha256};

use crate::private_files::require_single_link;
use crate::sftp_window::local_tree::{self, Directory, Entry, ReviewedStamp, ReviewedVolume};

use super::{changed, owned::CreatedTree, require_running};

#[derive(Eq, PartialEq)]
pub(super) struct Tree {
    pub(super) stamp: ReviewedStamp,
    children: BTreeMap<String, Node>,
}

#[derive(Eq, PartialEq)]
enum Node {
    Directory(Tree),
    File(FileRecord),
}

#[derive(Eq, PartialEq)]
struct FileRecord {
    stamp: ReviewedStamp,
    bytes: u64,
    digest: [u8; 32],
}

impl Tree {
    pub(super) fn accepts_delete(&self, delete: &DeleteScope) -> bool {
        delete.valid_for(|name| matches!(self.children.get(name), Some(Node::Directory(_))))
    }

    pub(super) fn capture(
        directory: &Directory,
        volume: ReviewedVolume,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<(Self, Inventory)> {
        let mut manifest = Manifest::default();
        let result = Self::scan(directory, volume, "", 0, &mut manifest, stopped)?;
        Ok((result, manifest.finish()))
    }

    fn scan(
        directory: &Directory,
        volume: ReviewedVolume,
        prefix: &str,
        depth: usize,
        manifest: &mut Manifest,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        require_running(stopped)?;
        if depth > protocol::MAX_DEPTH {
            return Err(io::Error::other(
                "The reviewed local rsync tree exceeds its depth limit",
            ));
        }
        let stamp = local_tree::reviewed_directory_stamp(directory)?;
        require_volume(&stamp, &volume)?;
        let mut entries = directory
            .names(manifest.remaining_entries())?
            .into_iter()
            .map(|name| checked_name(&name).map(str::to_owned))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_unstable();
        let mut children = BTreeMap::new();
        for name in entries {
            require_running(stopped)?;
            let path = child_path(prefix, &name);
            let leaf = OsStr::new(&name);
            let node = match local_tree::entry(directory, leaf)? {
                Entry::Directory(child) => {
                    manifest.add(&path, None)?;
                    let checked = Self::scan(&child, volume, &path, depth + 1, manifest, stopped)?;
                    require_child(directory, &name, &checked.stamp)?;
                    Node::Directory(checked)
                }
                Entry::File(file) => {
                    require_single_link(&file)?;
                    let stamp = local_tree::reviewed_file_stamp(&file)?;
                    require_volume(&stamp, &volume)?;
                    let bytes = file.metadata()?.len();
                    if bytes > manifest.remaining_bytes() {
                        return Err(io::Error::other(
                            "The reviewed local rsync tree exceeds its byte limit",
                        ));
                    }
                    let digest = copy_file(&file, io::sink(), bytes, stopped)?;
                    if local_tree::reviewed_file_stamp(&file)? != stamp {
                        return Err(changed());
                    }
                    let record = FileRecord {
                        stamp,
                        bytes,
                        digest,
                    };
                    let _ = record.open(directory, &name)?;
                    manifest.add(&path, Some((bytes, digest)))?;
                    Node::File(record)
                }
                Entry::Skip => return Err(changed()),
            };
            if children.insert(name, node).is_some() {
                return Err(changed());
            }
        }
        if local_tree::reviewed_directory_stamp(directory)? != stamp {
            return Err(changed());
        }
        Ok(Self { stamp, children })
    }

    pub(super) fn require_current(
        &self,
        directory: &Directory,
        moved: bool,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<()> {
        self.recapture(directory, moved, stopped).map(|_| ())
    }

    pub(super) fn recapture(
        &self,
        directory: &Directory,
        moved: bool,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<(Self, Inventory)> {
        let (current, inventory) = Self::capture(directory, self.stamp.volume(), stopped)?;
        let root_matches = if moved {
            self.stamp.same_after_move(&current.stamp)
        } else {
            self.stamp == current.stamp
        };
        if root_matches && self.children == current.children {
            Ok((current, inventory))
        } else {
            Err(changed())
        }
    }

    pub(super) fn removed_paths(&self, successor: &Self) -> u32 {
        self.children
            .iter()
            .map(
                |(name, original)| match (original, successor.children.get(name)) {
                    (Node::Directory(original), Some(Node::Directory(successor))) => {
                        original.removed_paths(successor)
                    }
                    (Node::File(_), Some(Node::File(_))) => 0,
                    _ => original.entry_count(),
                },
            )
            .sum()
    }

    pub(super) fn hash_into(&self, fingerprint: &mut Fingerprint) {
        self.stamp.hash_into(fingerprint);
        fingerprint.write_u32(self.children.len() as u32);
        for (name, node) in &self.children {
            fingerprint.write_u32(name.len() as u32);
            fingerprint.write(name.as_bytes());
            match node {
                Node::Directory(child) => {
                    fingerprint.write_u8(0);
                    child.hash_into(fingerprint);
                }
                Node::File(record) => {
                    fingerprint.write_u8(1);
                    record.stamp.hash_into(fingerprint);
                    fingerprint.write(&record.digest);
                }
            }
        }
    }
}

impl Node {
    fn entry_count(&self) -> u32 {
        match self {
            Self::Directory(tree) => 1 + tree.children.values().map(Self::entry_count).sum::<u32>(),
            Self::File(_) => 1,
        }
    }
}

impl FileRecord {
    fn open(&self, parent: &Directory, name: &str) -> io::Result<File> {
        let Entry::File(file) = local_tree::entry(parent, OsStr::new(name))? else {
            return Err(changed());
        };
        self.require_current(&file)?;
        Ok(file)
    }

    fn require_current(&self, file: &File) -> io::Result<()> {
        require_single_link(file)?;
        if local_tree::reviewed_file_stamp(file)? == self.stamp {
            Ok(())
        } else {
            Err(changed())
        }
    }
}

pub(super) struct Fingerprint(Sha256);

impl Fingerprint {
    pub(super) fn new(domain: &[u8]) -> Self {
        Self(Sha256::new_with_prefix(domain))
    }

    pub(super) fn digest(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

impl Hasher for Fingerprint {
    fn finish(&self) -> u64 {
        u64::from_be_bytes(
            self.0.clone().finalize()[..8]
                .try_into()
                .expect("SHA-256 prefix"),
        )
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
}

pub(super) fn checked_name(name: &OsStr) -> io::Result<&str> {
    let name = name
        .to_str()
        .filter(|name| protocol::valid_name(name))
        .ok_or_else(changed)?;
    #[cfg(windows)]
    crate::sftp_window::checked_local_component(name.as_bytes()).map_err(|_| changed())?;
    Ok(name)
}

fn require_volume(stamp: &ReviewedStamp, volume: &ReviewedVolume) -> io::Result<()> {
    if &stamp.volume() == volume {
        Ok(())
    } else {
        Err(io::Error::other(
            "The reviewed local rsync tree crosses a filesystem",
        ))
    }
}

fn require_child(parent: &Directory, name: &str, stamp: &ReviewedStamp) -> io::Result<Directory> {
    let Entry::Directory(directory) = local_tree::entry(parent, OsStr::new(name))? else {
        return Err(changed());
    };
    if local_tree::reviewed_directory_stamp(&directory)? == *stamp {
        Ok(directory)
    } else {
        Err(changed())
    }
}

fn child_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

fn copy_file(
    mut input: &File,
    mut output: impl Write,
    mut bytes: u64,
    stopped: &impl Fn() -> bool,
) -> io::Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    while bytes != 0 {
        require_running(stopped)?;
        let length = usize::try_from(bytes.min(buffer.len() as u64)).expect("bounded read");
        let chunk = &mut buffer[..length];
        input.read_exact(chunk)?;
        output.write_all(chunk)?;
        digest.update(chunk);
        bytes -= length as u64;
    }
    if input.read(&mut buffer[..1])? != 0 {
        return Err(changed());
    }
    Ok(digest.finalize().into())
}

pub(super) fn compose(
    destination: &Directory,
    created: &mut CreatedTree,
    original: View<'_>,
    incoming: View<'_>,
    delete: &DeleteScope,
    stopped: &impl Fn() -> bool,
) -> io::Result<Inventory> {
    let mut writer = Composer {
        manifest: Manifest::default(),
        delete,
        stopped,
    };
    writer.directory(destination, created, Some(original), Some(incoming), "")?;
    Ok(writer.manifest.finish())
}

#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    pub(super) directory: &'a Directory,
    pub(super) tree: &'a Tree,
}

#[derive(Clone, Copy)]
struct Child<'a> {
    parent: &'a Directory,
    node: &'a Node,
}

impl<'a> Child<'a> {
    fn directory(self, name: &str) -> io::Result<Option<(Directory, &'a Tree)>> {
        match self.node {
            Node::Directory(tree) => require_child(self.parent, name, &tree.stamp)
                .map(|directory| Some((directory, tree))),
            Node::File(_) => Ok(None),
        }
    }
}

struct Composer<'a, F> {
    stopped: &'a F,
    manifest: Manifest,
    delete: &'a DeleteScope,
}

impl<F: Fn() -> bool> Composer<'_, F> {
    fn directory(
        &mut self,
        destination: &Directory,
        created: &mut CreatedTree,
        original: Option<View<'_>>,
        incoming: Option<View<'_>>,
        prefix: &str,
    ) -> io::Result<()> {
        require_running(self.stopped)?;
        let mut entries = BTreeMap::<&str, [Option<Child<'_>>; 2]>::new();
        for (index, view) in [original, incoming].into_iter().enumerate() {
            if let Some(view) = view {
                for (name, node) in &view.tree.children {
                    entries.entry(name).or_default()[index] = Some(Child {
                        parent: view.directory,
                        node,
                    });
                }
            }
        }
        for (name, [old, new]) in entries {
            require_running(self.stopped)?;
            let path = child_path(prefix, name);
            let Some(selected) = new.or(old.filter(|_| !self.delete.allows(&path))) else {
                continue;
            };
            let leaf = OsStr::new(name);
            match selected.node {
                Node::Directory(_) => {
                    self.manifest.add(&path, None)?;
                    let target = local_tree::create_reviewed_tree_stage(destination, leaf)?;
                    let created = created.directory(name, &target)?;
                    let original = old
                        .map(|child| child.directory(name))
                        .transpose()?
                        .flatten();
                    let incoming = new
                        .map(|child| child.directory(name))
                        .transpose()?
                        .flatten();
                    self.directory(
                        &target,
                        created,
                        original
                            .as_ref()
                            .map(|(directory, tree)| View { directory, tree }),
                        incoming
                            .as_ref()
                            .map(|(directory, tree)| View { directory, tree }),
                        &path,
                    )?;
                }
                Node::File(record) => {
                    if !self.delete.replaces_nonempty()
                        && matches!(old, Some(Child { node: Node::Directory(tree), .. }) if !tree.children.is_empty())
                    {
                        return Err(io::Error::other(
                            "An incoming rsync file would remove a nonempty local directory",
                        ));
                    }
                    self.manifest
                        .add(&path, Some((record.bytes, record.digest)))?;
                    let input = record.open(selected.parent, name)?;
                    let mut output = local_tree::create_reviewed_tree_file(destination, leaf)?;
                    created.file(name, &output)?;
                    if copy_file(&input, &mut output, record.bytes, self.stopped)? != record.digest
                    {
                        return Err(changed());
                    }
                    if let Some(Child {
                        parent,
                        node: Node::File(old),
                    }) = old
                    {
                        let metadata = old.open(parent, name)?;
                        local_tree::copy_reviewed_file_metadata(&metadata, &output)?;
                        old.require_current(&metadata)?;
                    }
                    record.require_current(&input)?;
                    output.sync_all()?;
                }
            }
        }
        if let Some(original) = original {
            if local_tree::reviewed_directory_stamp(original.directory)? != original.tree.stamp {
                return Err(changed());
            }
            local_tree::copy_reviewed_dir_metadata(original.directory, destination)?;
        }
        local_tree::sync_reviewed_tree_directory(destination)
    }
}
