//! Canonical archive members and extraction limits shared by review and apply.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;

use serde::{Deserialize, Serialize};

pub const MAX_MEMBERS: usize = 4096;
pub const MAX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_MANIFEST_PATH_BYTES: usize = 512 * 1024;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_DEPTH: usize = 32;
pub const MAX_COMPONENT_BYTES: usize = 255;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub members: Vec<Member>,
    pub expanded_bytes: u64,
}

impl Manifest {
    /// Checks a canonical manifest received from the extraction helper.
    pub fn validate(&self) -> io::Result<()> {
        if self.members.len() > MAX_MEMBERS || self.expanded_bytes > MAX_EXPANDED_BYTES {
            return Err(invalid("The archive manifest exceeds its limits"));
        }
        let mut checked = ManifestBuilder::new();
        for member in &self.members {
            if normalize(&member.path, Vec::new(), false, false, |_| Ok(()))? != member.path {
                return Err(invalid("The archive manifest member path is not canonical"));
            }
            let target = match &member.kind {
                MemberKind::Symlink { target } if !target.is_empty() => Some(target),
                MemberKind::Hardlink { target } => Some(target),
                _ => None,
            };
            if let Some(target) = target
                && normalize(target, Vec::new(), false, false, |_| Ok(()))? != *target
            {
                return Err(invalid("The archive manifest link target is not canonical"));
            }
            let expanded = checked.expansion(&member.kind, member.size)?;
            checked.insert_member(&member.path, &member.kind, member.size)?;
            checked.expanded_bytes = expanded;
        }
        if checked.finish()? != *self {
            return Err(invalid(
                "The archive manifest does not match its canonical entries",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Member {
    pub path: String,
    #[serde(flatten)]
    pub kind: MemberKind,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemberKind {
    Directory,
    File,
    Symlink { target: String },
    Hardlink { target: String },
}

impl MemberKind {
    fn target_bytes(&self) -> usize {
        match self {
            Self::Symlink { target } | Self::Hardlink { target } => target.len(),
            Self::Directory | Self::File => 0,
        }
    }
}

struct Entry {
    kind: MemberKind,
    size: u64,
    explicit: bool,
}

#[derive(Default)]
pub(crate) struct ManifestBuilder {
    entries: BTreeMap<String, Entry>,
    expanded_bytes: u64,
    path_bytes: usize,
    #[cfg(any(feature = "decoder", test))]
    explicit_root: bool,
    required_directories: HashSet<String>,
    #[cfg(any(feature = "decoder", test))]
    required_directory_bytes: usize,
}

impl ManifestBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds an explicit member. A directory record for the archive root has no output path.
    /// Callers must defer all link creation until the completed manifest is validated.
    #[cfg(any(feature = "decoder", test))]
    pub(crate) fn add(
        &mut self,
        raw_path: &str,
        kind: MemberKind,
        size: u64,
    ) -> io::Result<Option<Member>> {
        let expanded_bytes = self.expansion(&kind, size)?;
        let path = normalize(
            raw_path,
            Vec::new(),
            false,
            kind == MemberKind::Directory,
            |_| Ok(()),
        )?;
        if path.is_empty() {
            if kind != MemberKind::Directory || self.explicit_root {
                return Err(invalid("Invalid or duplicate archive root member"));
            }
            self.explicit_root = true;
            return Ok(None);
        }
        let mut required = HashSet::new();
        let mut required_bytes = self.required_directory_bytes;
        let kind = match kind {
            MemberKind::Symlink { target } => {
                let parent = path
                    .rsplit_once('/')
                    .map_or(Vec::new(), |(parent, _)| parent.split('/').collect());
                MemberKind::Symlink {
                    target: normalize(&target, parent, true, true, |prefix| {
                        if !self.required_directories.contains(&prefix)
                            && !required.contains(&prefix)
                        {
                            required_bytes += prefix.len();
                            if self.required_directories.len() + required.len() == MAX_MEMBERS
                                || required_bytes > MAX_MANIFEST_PATH_BYTES
                            {
                                return Err(invalid(
                                    "Archive symbolic link directory checks exceed their budget",
                                ));
                            }
                            required.insert(prefix);
                        }
                        Ok(())
                    })?,
                }
            }
            MemberKind::Hardlink { target } => {
                let target = normalize(&target, Vec::new(), false, false, |_| Ok(()))?;
                if target.is_empty() {
                    return Err(invalid("An archive hard link cannot target the root"));
                }
                MemberKind::Hardlink { target }
            }
            kind => kind,
        };
        self.insert_member(&path, &kind, size)?;
        self.expanded_bytes = expanded_bytes;
        self.required_directories.extend(required);
        self.required_directory_bytes = required_bytes;
        Ok(Some(Member { path, kind, size }))
    }

    fn expansion(&self, kind: &MemberKind, size: u64) -> io::Result<u64> {
        if *kind != MemberKind::File && size != 0 {
            return Err(invalid("A directory or link declares a file payload"));
        }
        self.expanded_bytes
            .checked_add(size)
            .filter(|&bytes| bytes <= MAX_EXPANDED_BYTES)
            .ok_or_else(|| invalid("The archive exceeds the 256 MiB expanded size limit"))
    }

    fn insert_member(&mut self, path: &str, kind: &MemberKind, size: u64) -> io::Result<()> {
        let directory = MemberKind::Directory;
        let pending = parents(path)
            .map(|parent| (parent, &directory, false))
            .chain(std::iter::once((path, kind, true)));
        let mut entries = self.entries.len();
        let mut path_bytes = self.path_bytes;
        for (path, kind, explicit) in pending {
            if let Some(previous) = self.entries.get(path) {
                if previous.kind != MemberKind::Directory
                    || *kind != MemberKind::Directory
                    || previous.explicit && explicit
                {
                    return Err(invalid(
                        "Archive members duplicate or conflict at the same path",
                    ));
                }
            } else {
                if entries == MAX_MEMBERS {
                    return Err(invalid("The archive exceeds the 4096-member limit"));
                }
                entries += 1;
                path_bytes = add_path_bytes(path_bytes, path, kind)?;
            }
        }
        for parent in parents(path) {
            self.entries.entry(parent.to_owned()).or_insert(Entry {
                kind: MemberKind::Directory,
                size: 0,
                explicit: false,
            });
        }
        self.entries.insert(
            path.to_owned(),
            Entry {
                kind: kind.clone(),
                size,
                explicit: true,
            },
        );
        self.path_bytes = path_bytes;
        Ok(())
    }

    pub(crate) fn finish(self) -> io::Result<Manifest> {
        if self
            .required_directories
            .iter()
            .any(|path| self.kind(path) != Some(&MemberKind::Directory))
        {
            return Err(invalid(
                "An archive symbolic link traverses a path that is not an archive directory",
            ));
        }
        self.verify_symlinks()?;
        let mut hardlinks = HashMap::new();
        let mut members = Vec::with_capacity(self.entries.len());
        let mut path_bytes = 0;
        for (path, entry) in &self.entries {
            let kind = if matches!(entry.kind, MemberKind::Hardlink { .. }) {
                MemberKind::Hardlink {
                    target: self.hardlink_file(path, &mut hardlinks)?.to_owned(),
                }
            } else {
                entry.kind.clone()
            };
            path_bytes = add_path_bytes(path_bytes, path, &kind)?;
            members.push(Member {
                path: path.clone(),
                kind,
                size: entry.size,
            });
        }
        Ok(Manifest {
            members,
            expanded_bytes: self.expanded_bytes,
        })
    }

    fn verify_symlinks(&self) -> io::Result<()> {
        let mut checked = HashSet::new();
        for path in self.entries.keys() {
            let mut pending = HashSet::new();
            let mut next = path.as_str();
            while let Some(MemberKind::Symlink { target }) = self.kind(next) {
                if checked.contains(next) {
                    break;
                }
                if !pending.insert(next) {
                    return Err(invalid("Archive symbolic links form a cycle"));
                }
                self.verify_target_ancestors(target)?;
                next = target;
            }
            checked.extend(pending);
        }
        Ok(())
    }

    fn hardlink_file<'a>(
        &'a self,
        path: &'a str,
        resolved: &mut HashMap<&'a str, &'a str>,
    ) -> io::Result<&'a str> {
        let mut pending = HashSet::new();
        let mut next = path;
        let file = loop {
            if let Some(&file) = resolved.get(next) {
                break file;
            }
            match self.kind(next) {
                Some(MemberKind::File) => break next,
                Some(MemberKind::Hardlink { target }) => {
                    if !pending.insert(next) {
                        return Err(invalid("Archive hard links form a cycle"));
                    }
                    self.verify_target_ancestors(target)?;
                    next = target;
                }
                _ => return Err(invalid("An archive hard link must target a regular file")),
            }
        };
        resolved.extend(pending.into_iter().map(|link| (link, file)));
        Ok(file)
    }

    fn verify_target_ancestors(&self, target: &str) -> io::Result<()> {
        for parent in parents(target) {
            if self
                .kind(parent)
                .is_some_and(|kind| *kind != MemberKind::Directory)
            {
                return Err(invalid(
                    "An archive link target passes through a non-directory",
                ));
            }
        }
        Ok(())
    }

    fn kind(&self, path: &str) -> Option<&MemberKind> {
        self.entries.get(path).map(|entry| &entry.kind)
    }
}

fn parents(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .map(move |(separator, _)| &path[..separator])
}

fn add_path_bytes(bytes: usize, path: &str, kind: &MemberKind) -> io::Result<usize> {
    let total = bytes + path.len() + kind.target_bytes();
    if total > MAX_MANIFEST_PATH_BYTES {
        return Err(invalid("The archive exceeds the 512 KiB path budget"));
    }
    Ok(total)
}

fn normalize<'a>(
    raw: &'a str,
    mut parts: Vec<&'a str>,
    allow_parent: bool,
    allow_trailing_slash: bool,
    mut require_directory: impl FnMut(String) -> io::Result<()>,
) -> io::Result<String> {
    if raw.is_empty()
        || raw.len() > MAX_PATH_BYTES
        || raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
    {
        return Err(invalid("The archive contains an invalid path"));
    }
    let requires_directory = raw.ends_with('/') || raw.rsplit('/').next() == Some(".");
    if requires_directory && !allow_trailing_slash {
        return Err(invalid("An archive file path denotes a directory"));
    }
    let raw = if allow_trailing_slash {
        raw.strip_suffix('/').unwrap_or(raw)
    } else {
        raw
    };
    let mut checked_ancestors = parts.len();
    let mut intermediate = 0;
    let mut first = true;
    for part in raw.split('/') {
        match part {
            "." => continue,
            ".." if allow_parent => {
                // A prefix discarded by normalization must really be a directory.
                // Otherwise a linked or missing prefix could give the raw target another meaning.
                if parts.len() > checked_ancestors {
                    intermediate += 1;
                    if intermediate > MAX_DEPTH {
                        return Err(invalid(
                            "An archive symbolic link has too many intermediate directories",
                        ));
                    }
                    require_directory(joined(&parts)?)?;
                }
                if parts.pop().is_none() {
                    return Err(invalid(
                        "An archive symbolic link escapes the extraction root",
                    ));
                }
                checked_ancestors = checked_ancestors.min(parts.len());
            }
            "" | ".." => return Err(invalid("The archive contains an invalid path component")),
            _ => {
                if part.len() > MAX_COMPONENT_BYTES
                    || (first || parts.is_empty()) && drive_path(part)
                {
                    return Err(invalid("The archive contains an invalid path component"));
                }
                parts.push(part);
            }
        }
        first = false;
    }
    let path = joined(&parts)?;
    if allow_parent && requires_directory && parts.len() > checked_ancestors {
        require_directory(path.clone())?;
    }
    Ok(path)
}

fn joined(parts: &[&str]) -> io::Result<String> {
    if parts.len() > MAX_DEPTH {
        return Err(invalid(
            "The archive exceeds the 32-component path depth limit",
        ));
    }
    let path = parts.join("/");
    if path.len() > MAX_PATH_BYTES {
        return Err(invalid("The archive exceeds the 4096-byte path limit"));
    }
    Ok(path)
}

fn drive_path(component: &str) -> bool {
    matches!(component.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symlink(target: &str) -> MemberKind {
        MemberKind::Symlink {
            target: target.into(),
        }
    }

    fn hardlink(target: &str) -> MemberKind {
        MemberKind::Hardlink {
            target: target.into(),
        }
    }

    #[test]
    fn manifest_normalizes_paths_and_counts_implicit_directories() {
        let mut builder = ManifestBuilder::new();
        assert_eq!(builder.add("./", MemberKind::Directory, 0).unwrap(), None);
        let file = builder
            .add("./docs/./chapter/page.txt", MemberKind::File, 7)
            .unwrap()
            .unwrap();
        assert_eq!(file.path, "docs/chapter/page.txt");
        builder.add("docs/", MemberKind::Directory, 0).unwrap();
        let manifest = builder.finish().unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.expanded_bytes, 7);
        assert_eq!(
            manifest
                .members
                .iter()
                .map(|member| (member.path.as_str(), &member.kind))
                .collect::<Vec<_>>(),
            [
                ("docs", &MemberKind::Directory),
                ("docs/chapter", &MemberKind::Directory),
                ("docs/chapter/page.txt", &MemberKind::File),
            ]
        );
    }

    #[test]
    fn manifest_validation_rejects_noncanonical_receipts() {
        let mut builder = ManifestBuilder::new();
        builder.add("dir/file", MemberKind::File, 5).unwrap();
        builder.add("dir/root", symlink(".."), 0).unwrap();
        builder.add("copy", hardlink("dir/file"), 0).unwrap();
        let manifest = builder.finish().unwrap();
        manifest.validate().unwrap();

        let mut missing_directory = manifest.clone();
        missing_directory
            .members
            .retain(|member| member.path != "dir");
        assert!(missing_directory.validate().is_err());
        let mut unordered = manifest.clone();
        unordered.members.reverse();
        assert!(unordered.validate().is_err());
        let mut incorrect_size = manifest.clone();
        incorrect_size.expanded_bytes += 1;
        assert!(incorrect_size.validate().is_err());
        let mut alias = manifest;
        alias.members[0].path.insert_str(0, "./");
        assert!(alias.validate().is_err());
    }

    #[test]
    fn manifest_rejects_invalid_paths_duplicate_aliases_and_non_directory_parents() {
        for path in [
            "",
            "/absolute",
            "./C:/drive",
            "C:drive",
            "foo\\bar",
            "a\u{7f}",
            "a/../b",
            "a//b",
            "file/",
        ] {
            let mut builder = ManifestBuilder::new();
            assert!(builder.add(path, MemberKind::File, 0).is_err(), "{path:?}");
        }
        let mut duplicate = ManifestBuilder::new();
        duplicate.add("./dir/", MemberKind::Directory, 0).unwrap();
        assert!(duplicate.add("dir", MemberKind::Directory, 0).is_err());

        let mut linked_parent = ManifestBuilder::new();
        linked_parent.add("dir", symlink("target"), 0).unwrap();
        assert!(linked_parent.add("dir/child", MemberKind::File, 0).is_err());

        let mut implicit_parent = ManifestBuilder::new();
        implicit_parent
            .add("dir/child", MemberKind::File, 0)
            .unwrap();
        assert!(implicit_parent.add("dir", MemberKind::File, 0).is_err());
    }

    #[test]
    fn manifest_confines_symbolic_links_and_resolves_hard_link_chains() {
        let mut builder = ManifestBuilder::new();
        builder.add("data/file", MemberKind::File, 11).unwrap();
        for (path, link) in [
            ("links/nested/root", symlink("../..")),
            ("links/nested/sibling", symlink("../../data/file")),
            ("links/nested/chained", symlink("sibling")),
            ("links/nested/traversed", symlink("walk/../sibling")),
            ("links/nested/dangling", symlink("../../missing/file")),
            ("copy", hardlink("./data/file")),
            ("copy-chain", hardlink("copy")),
        ] {
            builder.add(path, link, 0).unwrap();
        }
        builder
            .add("links/nested/walk", MemberKind::Directory, 0)
            .unwrap();
        let manifest = builder.finish().unwrap();
        let kinds: BTreeMap<_, _> = manifest
            .members
            .iter()
            .map(|member| (member.path.as_str(), &member.kind))
            .collect();
        assert_eq!(kinds["links/nested/root"], &symlink(""));
        assert_eq!(kinds["links/nested/sibling"], &symlink("data/file"));
        assert_eq!(
            kinds["links/nested/chained"],
            &symlink("links/nested/sibling")
        );
        assert_eq!(
            kinds["links/nested/traversed"],
            &symlink("links/nested/sibling")
        );
        assert_eq!(kinds["links/nested/dangling"], &symlink("missing/file"));
        assert_eq!(kinds["copy-chain"], &hardlink("data/file"));
        assert_eq!(manifest.expanded_bytes, 11);
    }

    #[test]
    fn manifest_refuses_escaping_cyclic_and_ambiguous_link_targets() {
        let mut escaping = ManifestBuilder::new();
        assert!(
            escaping
                .add("folder/link", symlink("../../elsewhere"), 0)
                .is_err()
        );
        assert!(escaping.add("folder/link", hardlink("../file"), 0).is_err());
        assert!(escaping.add("folder/link", symlink("/outside"), 0).is_err());

        for links in [
            [symlink("b"), symlink("a")],
            [hardlink("b"), hardlink("a")],
            [symlink("b/child"), symlink("missing")],
            [symlink("b/../target"), symlink("missing")],
            [symlink("missing/../target"), symlink("other")],
            [hardlink("b"), symlink("missing")],
            [hardlink("missing"), symlink("a")],
        ] {
            let mut builder = ManifestBuilder::new();
            for (name, kind) in ["a", "b"].into_iter().zip(links) {
                builder.add(name, kind, 0).unwrap();
            }
            assert!(builder.finish().is_err());
        }
    }

    #[test]
    fn manifest_enforces_declared_size_depth_member_and_path_budgets() {
        let mut bytes = ManifestBuilder::new();
        bytes
            .add("large", MemberKind::File, MAX_EXPANDED_BYTES)
            .unwrap();
        assert!(bytes.add("extra", MemberKind::File, 1).is_err());
        assert!(bytes.add("bad", MemberKind::Directory, 1).is_err());
        let too_deep = ["d"; MAX_DEPTH + 1].join("/");
        assert!(bytes.add(&too_deep, MemberKind::Directory, 0).is_err());

        let mut members = ManifestBuilder::new();
        for index in 0..MAX_MEMBERS / 2 - 1 {
            members
                .add(&format!("{index}/file"), MemberKind::File, 0)
                .unwrap();
        }
        members.add("second-last", MemberKind::File, 0).unwrap();
        assert!(members.add("extra/child", MemberKind::File, 0).is_err());
        members.add("last", MemberKind::File, 0).unwrap();
        assert_eq!(members.finish().unwrap().members.len(), MAX_MEMBERS);

        let mut paths = ManifestBuilder::new();
        let target = "x".repeat(MAX_COMPONENT_BYTES);
        let mut remaining = MAX_MANIFEST_PATH_BYTES;
        for index in 0..MAX_MEMBERS {
            let path = format!("link-{index}");
            let cost = path.len() + target.len();
            let result = paths.add(&path, symlink(&target), 0);
            if cost > remaining {
                assert!(result.is_err());
                return;
            }
            result.unwrap();
            remaining -= cost;
        }
        panic!("the path budget must be reached before the member limit");
    }
}
