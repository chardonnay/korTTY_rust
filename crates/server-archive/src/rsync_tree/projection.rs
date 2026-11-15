//! Projects ordered rsync sources into one bounded, portable incoming tree.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization as _;

use super::{DeleteScope, Inventory, MAX_DEPTH, Manifest, invalid, valid_name};

pub const MAX_SOURCES: usize = 16;
const MAX_RELATIVE_BYTES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    File(String),
    Named(String),
    Contents,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub relative: String,
    pub file: Option<(u64, [u8; 32])>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
    pub placement: Placement,
    /// A regular file has one entry at the empty path. A directory lists only its descendants.
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedEntry {
    pub relative: String,
    pub source: usize,
    pub original: String,
    pub file: Option<(u64, [u8; 32])>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionKind {
    DirectoryCoalesced,
    DirectoryReplacedFile,
    EarlierFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Collision {
    pub relative: String,
    pub winner: usize,
    pub loser: usize,
    pub kind: CollisionKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Projection {
    /// Entries are in the depth-first order used by the held-tree inventory.
    pub entries: Vec<ProjectedEntry>,
    pub inventory: Inventory,
    pub delete: DeleteScope,
    /// One record per losing or coalesced source entry, using the final winning source index.
    pub collisions: Vec<Collision>,
}

/// A contents source supplies the receiver root. A named directory supplies only its own subtree.
/// In mirror mode file-only inputs prune no siblings.
pub fn delete_scope<'a>(
    placements: impl IntoIterator<Item = &'a Placement>,
    mirror: bool,
) -> io::Result<DeleteScope> {
    let mut roots = BTreeMap::new();
    let mut named = BTreeSet::new();
    let mut contents = false;
    let mut count = 0;
    for placement in placements {
        count += 1;
        if count > MAX_SOURCES {
            return Err(invalid("Rsync source list exceeds its limit"));
        }
        match placement {
            Placement::Contents => contents = true,
            Placement::File(name) | Placement::Named(name) => {
                if roots
                    .insert(portable_key(name)?, name)
                    .is_some_and(|previous| previous != name)
                {
                    return Err(invalid("Rsync paths have ambiguous portable spellings"));
                }
                if let Placement::Named(_) = placement {
                    named.insert(name.clone());
                }
            }
        }
    }
    if count == 0 {
        return Err(invalid("Rsync needs at least one source"));
    }
    Ok(match (mirror, contents) {
        (false, _) => DeleteScope::Retain,
        (true, true) => DeleteScope::All,
        (true, false) => DeleteScope::Named(named.into_iter().collect()),
    })
}

pub fn project(sources: &[Source], mirror: bool) -> io::Result<Projection> {
    let delete = delete_scope(sources.iter().map(|source| &source.placement), mirror)?;
    let mut incoming = Incoming::default();
    for (source, input) in sources.iter().enumerate() {
        let origin = |original, file| Origin {
            source,
            original,
            file,
        };
        if let Placement::File(name) = &input.placement {
            let [
                Entry {
                    relative,
                    file: Some(file),
                },
            ] = input.entries.as_slice()
            else {
                return Err(invalid("Rsync file source must contain one regular file"));
            };
            if !relative.is_empty() {
                return Err(invalid("Rsync file source needs a root entry"));
            }
            incoming.add(vec![name], origin("", Some(*file)))?;
            continue;
        }
        let named = match &input.placement {
            Placement::Named(name) => {
                incoming.add(vec![name], origin("", None))?;
                Some(name.as_str())
            }
            _ => None,
        };
        if input.entries.len() > incoming.budget.remaining_entries() {
            return Err(invalid("Combined rsync sources exceed their entry limit"));
        }
        let mut paths = BTreeMap::new();
        for entry in &input.entries {
            if entry.relative.is_empty()
                || entry.relative.len() > MAX_RELATIVE_BYTES
                || paths.insert(entry.relative.as_str(), entry.file).is_some()
            {
                return Err(invalid(
                    "Invalid or repeated relative path in an rsync source",
                ));
            }
        }
        for (relative, file) in paths.iter().map(|(relative, file)| (*relative, *file)) {
            if let Some((parent, _)) = relative.rsplit_once('/')
                && paths.get(parent) != Some(&None)
            {
                return Err(invalid("An rsync source entry has no directory parent"));
            }
            let parts = named.into_iter().chain(relative.split('/')).collect();
            incoming.add(parts, origin(relative, file))?;
        }
    }
    incoming.finish(delete)
}

#[derive(Clone, Copy)]
struct Origin<'a> {
    source: usize,
    original: &'a str,
    file: Option<(u64, [u8; 32])>,
}

#[derive(Default)]
struct Incoming<'a> {
    budget: Manifest,
    // Ordering components separately visits a directory's descendants before its next sibling.
    paths: BTreeMap<Vec<&'a str>, Vec<Origin<'a>>>,
    aliases: BTreeMap<Vec<String>, Vec<&'a str>>,
}

impl<'a> Incoming<'a> {
    fn add(&mut self, parts: Vec<&'a str>, origin: Origin<'a>) -> io::Result<()> {
        // The scanner permits files immediately below a directory at MAX_DEPTH.
        if parts.len() > MAX_DEPTH + usize::from(origin.file.is_some()) {
            return Err(invalid("Combined rsync directory exceeds its depth limit"));
        }
        self.budget.add(&parts.join("/"), origin.file)?;
        let key = parts
            .iter()
            .map(|name| portable_key(name))
            .collect::<io::Result<Vec<_>>>()?;
        match self.aliases.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(parts.clone());
            }
            std::collections::btree_map::Entry::Occupied(entry) if *entry.get() != parts => {
                return Err(invalid("Rsync paths have ambiguous portable spellings"));
            }
            _ => {}
        }
        self.paths.entry(parts).or_default().push(origin);
        Ok(())
    }

    fn finish(self, delete: DeleteScope) -> io::Result<Projection> {
        let mut manifest = Manifest::default();
        let mut entries = Vec::with_capacity(self.paths.len());
        let mut collisions = Vec::new();
        for (path, candidates) in self.paths {
            let relative = path.join("/");
            let first_directory = candidates.iter().find(|origin| origin.file.is_none());
            let winner = first_directory.unwrap_or(&candidates[0]);
            manifest.add(&relative, winner.file)?;
            for loser in candidates
                .iter()
                .filter(|origin| origin.source != winner.source)
            {
                let kind = match (winner.file.is_some(), loser.file.is_some()) {
                    (false, false) => CollisionKind::DirectoryCoalesced,
                    (false, true) => CollisionKind::DirectoryReplacedFile,
                    _ => CollisionKind::EarlierFile,
                };
                collisions.push(Collision {
                    relative: relative.clone(),
                    winner: winner.source,
                    loser: loser.source,
                    kind,
                });
            }
            entries.push(ProjectedEntry {
                relative,
                source: winner.source,
                original: winner.original.into(),
                file: winner.file,
            });
        }
        Ok(Projection {
            entries,
            inventory: manifest.finish(),
            delete,
            collisions,
        })
    }
}

fn portable_key(name: &str) -> io::Result<String> {
    if !valid_name(name) || name.chars().any(ignorable) {
        return Err(invalid("Invalid or nonportable name in an rsync source"));
    }
    // Compare compatible Unicode spellings and case without changing the published name.
    let key: String = name
        .nfkc()
        .flat_map(char::to_lowercase)
        .flat_map(char::to_uppercase)
        .nfkc()
        .collect();
    if !valid_name(&key) || key.contains(['<', '>', '"', '|', '?', '*']) || windows_alias(&key) {
        return Err(invalid("Invalid or nonportable name in an rsync source"));
    }
    Ok(key)
}

fn windows_alias(name: &str) -> bool {
    let (stem, extension) = name.split_once('.').unwrap_or((name, ""));
    let stem = stem.trim_end();
    matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|prefix| {
            matches!(stem.strip_prefix(prefix).map(str::as_bytes), Some([b'1'..=b'9']))
        })
        // A short-name alias may be assigned by Windows according to other directory entries.
        || (stem.encode_utf16().count() <= 8
            && extension.encode_utf16().count() <= 3
            && stem.rsplit_once('~').is_some_and(|(_, suffix)| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            }))
}

fn ignorable(ch: char) -> bool {
    // Unicode Default_Ignorable_Code_Point. Filesystems may ignore some of these in comparisons.
    matches!(
        ch as u32,
        0x00AD | 0x034F | 0x061C | 0x115F..=0x1160 | 0x17B4..=0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
            | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFF8 | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsync_tree::{MAX_BYTES, MAX_ENTRIES};

    const EARLY: (u64, [u8; 32]) = (3, [1; 32]);
    const LATE: (u64, [u8; 32]) = (4, [2; 32]);

    fn entry(relative: &str, file: Option<(u64, [u8; 32])>) -> Entry {
        Entry {
            relative: relative.into(),
            file,
        }
    }

    fn file(name: &str, content: (u64, [u8; 32])) -> Source {
        Source {
            placement: Placement::File(name.into()),
            entries: vec![entry("", Some(content))],
        }
    }

    #[test]
    fn merges_mixed_sources_with_final_collision_winners_and_manifest_order() {
        let sources = [
            file("tree", EARLY),
            file("notes", EARLY),
            Source {
                placement: Placement::Named("tree".into()),
                entries: vec![
                    entry("a-else", Some(EARLY)),
                    entry("a/leaf", Some(EARLY)),
                    entry("a", None),
                ],
            },
            Source {
                placement: Placement::Contents,
                entries: vec![
                    entry("tree/a/leaf", Some(LATE)),
                    entry("tree/a", None),
                    entry("tree", None),
                    entry("notes", Some(LATE)),
                    entry("extra", Some(LATE)),
                ],
            },
            file("tree", LATE),
        ];
        let result = project(&sources, true).unwrap();
        assert_eq!(result.delete, DeleteScope::All);
        let expected = [
            ("extra", 3, "extra", Some(LATE)),
            ("notes", 1, "", Some(EARLY)),
            ("tree", 2, "", None),
            ("tree/a", 2, "a", None),
            ("tree/a/leaf", 2, "a/leaf", Some(EARLY)),
            ("tree/a-else", 2, "a-else", Some(EARLY)),
        ];
        let mut manifest = Manifest::default();
        for (path, _, _, file) in expected {
            manifest.add(path, file).unwrap();
        }
        assert_eq!(result.inventory, manifest.finish());
        assert_eq!(
            result
                .entries
                .iter()
                .map(|entry| (
                    entry.relative.as_str(),
                    entry.source,
                    entry.original.as_str(),
                    entry.file
                ))
                .collect::<Vec<_>>(),
            expected
        );
        use CollisionKind::{DirectoryCoalesced, DirectoryReplacedFile, EarlierFile};
        assert_eq!(
            result
                .collisions
                .iter()
                .map(|collision| (
                    collision.relative.as_str(),
                    collision.winner,
                    collision.loser,
                    collision.kind
                ))
                .collect::<Vec<_>>(),
            [
                ("notes", 1, 3, EarlierFile),
                ("tree", 2, 0, DirectoryReplacedFile),
                ("tree", 2, 3, DirectoryCoalesced),
                ("tree", 2, 4, DirectoryReplacedFile),
                ("tree/a", 2, 3, DirectoryCoalesced),
                ("tree/a/leaf", 2, 3, EarlierFile),
            ]
        );
    }

    #[test]
    fn named_mirror_is_bounded_to_distinct_directory_roots() {
        let placements = [
            Placement::File("regular".into()),
            Placement::Named("z".into()),
            Placement::Named("a".into()),
            Placement::Named("z".into()),
        ];
        assert_eq!(
            delete_scope(&placements, true).unwrap(),
            DeleteScope::Named(vec!["a".into(), "z".into()])
        );
        assert_eq!(
            delete_scope(&placements, false).unwrap(),
            DeleteScope::Retain
        );
        assert_eq!(
            delete_scope(&placements[..1], true).unwrap(),
            DeleteScope::Named(vec![])
        );
        let empty = Source {
            placement: Placement::Named("empty".into()),
            entries: vec![],
        };
        let result = project(&[empty], true).unwrap();
        assert_eq!(result.inventory.entries, 1);
        assert_eq!(result.entries[0].relative, "empty");
        assert_eq!(result.delete, DeleteScope::Named(vec!["empty".into()]));
    }

    #[test]
    fn rejects_portable_aliases_and_different_spelling_inside_sources() {
        for (first, second) in [("A", "a"), ("Café", "Cafe\u{301}"), ("Straße", "STRAẞE")] {
            assert!(project(&[file(first, EARLY), file(second, LATE)], false).is_err());
        }
        for name in [
            "..",
            "COM¹.log",
            "FooBar~1.txt",
            "trailing.",
            "forbidden?",
            "x\u{200d}y",
        ] {
            assert!(project(&[file(name, EARLY)], false).is_err(), "{name:?}");
        }
        let malformed = Source {
            placement: Placement::Contents,
            entries: vec![entry("child", None), entry("CHILD/x", Some(EARLY))],
        };
        assert!(project(&[malformed], false).is_err());
        let malformed = Source {
            placement: Placement::Contents,
            entries: vec![entry("child", Some(EARLY)), entry("child/x", Some(LATE))],
        };
        assert!(project(&[malformed], false).is_err());
        let malformed = Source {
            placement: Placement::Contents,
            entries: vec![entry("duplicate", None), entry("duplicate", None)],
        };
        assert!(project(&[malformed], false).is_err());
        let valid = Source {
            placement: Placement::Contents,
            entries: vec![entry("d", None), entry("d/a", None), entry("d/A", None)],
        };
        assert!(project(&[valid], false).is_err());
    }

    #[test]
    fn rejects_overflow_in_losing_inputs_and_added_named_root() {
        assert!(
            project(
                &[file("same", (MAX_BYTES, [0; 32])), file("same", LATE)],
                false
            )
            .is_err()
        );
        let sources = vec![file("same", EARLY); MAX_SOURCES + 1];
        assert!(project(&sources, false).is_err());
        let mut many = Source {
            placement: Placement::Contents,
            entries: (0..MAX_ENTRIES)
                .map(|index| entry(&format!("f{index}"), None))
                .collect(),
        };
        assert!(project(std::slice::from_ref(&many), false).is_ok());
        many.placement = Placement::Named("wrapper".into());
        assert!(project(&[many], false).is_err());

        let mut deep = Source {
            placement: Placement::Contents,
            entries: (1..=MAX_DEPTH)
                .map(|depth| entry(&format!("{}x", "x/".repeat(depth - 1)), None))
                .collect(),
        };
        deep.entries
            .push(entry(&format!("{}f", "x/".repeat(MAX_DEPTH)), Some(EARLY)));
        assert!(project(std::slice::from_ref(&deep), false).is_ok());
        deep.placement = Placement::Named("wrapper".into());
        assert!(project(&[deep], false).is_err());
    }
}
