//! One-use manual removal plans. Scheduled synchronization remains copy-only.

use std::path::Path;

use super::super::server_copy::Folder;
use super::*;

pub(super) mod local;

pub(super) enum Outcome {
    Removed(Option<Failure>),
    KeptNotEmpty,
}

pub(super) struct Plan {
    root: Location,
    actions: Vec<Remove>,
}

struct Remove {
    path: Relative,
    label: String,
    target: Location,
    parent: (Location, DirectoryStamp),
    expected: SnapshotEntry,
    absent: Location,
    source_parent: (Location, DirectoryStamp),
}

impl Plan {
    pub(super) fn new(review: &Review, to: Side, copy: &CopyPlan) -> io::Result<Self> {
        let (source, target) = review.direction(to).ok_or_else(|| {
            io::Error::other("Directional removal requires a local and a server folder")
        })?;
        let mut actions = Vec::new();
        let mut label_bytes = copy.roots.iter().map(String::len).sum::<usize>()
            + copy
                .actions
                .iter()
                .map(|action| action.label.len())
                .sum::<usize>();
        for path in review.destination_only(to) {
            let expected = target.entries.get(path).ok_or_else(invalid_plan)?;
            if expected.kind() == Kind::Blocked {
                return Err(invalid_plan());
            }
            if copy.actions.len() + actions.len() == MAX_ACTIONS {
                return Err(io::Error::other(
                    "Reviewed synchronization exceeds 256 entries",
                ));
            }
            let label = path.review_label();
            label_bytes += label.len();
            if label.len() > MAX_LABEL_BYTES || label_bytes > MAX_REVIEW_LABEL_BYTES {
                return Err(io::Error::other(
                    "The exact paths exceed the synchronization review display limit",
                ));
            }

            let mut missing = path.clone();
            let source_parent = loop {
                if let Some(parent) = source.parent(&missing) {
                    break parent;
                }
                missing = missing.parent().ok_or_else(invalid_plan)?;
            };
            if source.entries.contains_key(&missing) {
                return Err(invalid_plan());
            }
            actions.push(Remove {
                path: path.clone(),
                label,
                target: target.location(path)?,
                parent: target.parent(path).ok_or_else(invalid_plan)?,
                expected: expected.clone(),
                absent: source.location(&missing)?,
                source_parent,
            });
        }
        if actions.is_empty() {
            return Err(io::Error::other(
                "There are no reviewed destination-only entries to remove",
            ));
        }
        // Comparison order is lexical, where each parent precedes its descendants.
        actions.reverse();
        Ok(Self {
            root: target.root.clone(),
            actions,
        })
    }

    pub(super) fn paths(&self) -> impl Iterator<Item = (bool, &str)> {
        self.actions.iter().map(|action| {
            (
                action.expected.kind() == Kind::Folder,
                action.label.as_str(),
            )
        })
    }

    pub(super) fn len(&self) -> usize {
        self.actions.len()
    }

    pub(super) fn root(&self) -> &Location {
        &self.root
    }

    pub(super) fn verify_destination(
        &self,
        client: &mut Client,
        stopped: &impl Fn() -> bool,
    ) -> Result<()> {
        if let Location::Remote(path) = &self.root {
            if !client.supports_atomic_replace() {
                return Err(io::Error::other(
                    "Reviewed server removals require the SFTP POSIX rename extension",
                )
                .into());
            }
            let folder = Folder::read(client, path.clone(), stopped)?;
            if !folder.trusted(folder.owner()) {
                return Err(io::Error::other(
                    "Choose a protected canonical server destination folder",
                )
                .into());
            }
        }
        Ok(())
    }

    pub(super) fn apply(
        &self,
        client: &mut Client,
        authorize: &impl Fn() -> Result<()>,
        stopped: &impl Fn() -> bool,
        counts: &mut Counts,
    ) -> Result<()> {
        let mut changed_parents = BTreeMap::new();
        for action in &self.actions {
            let result = (|| {
                authorize()?;
                action.remove(
                    client,
                    authorize,
                    stopped,
                    changed_parents.get(&action.path),
                )
            })()
            .map_err(|error| error.context(format!(" · {}", action.label)))?;
            match result {
                Outcome::KeptNotEmpty => counts.kept += 1,
                Outcome::Removed(warning) => {
                    counts.removed += 1;
                    if let Some(error) = warning {
                        return Err(error.context(format!(" · {}", action.label)));
                    }
                    if let (Location::Remote(_), Some(parent)) =
                        (&action.target, action.path.parent())
                    {
                        let current = action.parent.1.verify(client, &action.parent.0, false)?;
                        changed_parents.insert(parent, current);
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Counts {
    removed: usize,
    kept: usize,
}

impl Counts {
    pub(super) fn note(&self, total: usize) -> String {
        let mut note = format!(" · Removed {} of {total} reviewed entries", self.removed);
        if self.kept != 0 {
            let suffix = if self.kept == 1 { "" } else { "s" };
            note.push_str(&format!(" · Kept {} nonempty folder{suffix}", self.kept));
        }
        note
    }
}

impl Remove {
    fn remove(
        &self,
        client: &mut Client,
        authorize: &impl Fn() -> Result<()>,
        stopped: &impl Fn() -> bool,
        updated: Option<&DirectoryStamp>,
    ) -> Result<Outcome> {
        let guard = |client: &mut Client| {
            authorize()?;
            self.require_source_absent(client)?;
            authorize()
        };
        guard(client)?;
        match (&self.parent, &self.target, &self.expected) {
            (
                (Location::Local(parent), DirectoryStamp::Local(identity)),
                Location::Local(path),
                expected,
            ) => {
                let name = local_child(parent, path)?;
                let client = RefCell::new(client);
                local::remove(parent, name, identity, expected, &|| {
                    guard(&mut client.borrow_mut())
                })
            }
            (
                (Location::Remote(parent), DirectoryStamp::Remote(_)),
                Location::Remote(path),
                SnapshotEntry::Remote(expected),
            ) => {
                let expected = match updated {
                    Some(DirectoryStamp::Remote(current))
                        if self.expected.kind() == Kind::Folder =>
                    {
                        *current
                    }
                    None => *expected,
                    _ => return Err(invalid_plan().into()),
                };
                remove_remote(
                    client,
                    parent,
                    path,
                    &self.parent.1,
                    expected,
                    stopped,
                    &guard,
                )
            }
            _ => Err(invalid_plan().into()),
        }
    }

    fn require_source_absent(&self, client: &mut Client) -> Result<()> {
        self.source_parent
            .1
            .verify(client, &self.source_parent.0, true)?;
        let exists = match &self.absent {
            Location::Remote(path) => remote_existing(client, path)?.is_some(),
            Location::Local(path) => match fs::symlink_metadata(path) {
                Ok(_) => true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            },
        };
        if exists {
            return Err(
                io::Error::other("A source entry appeared. Compare the folders again").into(),
            );
        }
        self.source_parent
            .1
            .verify(client, &self.source_parent.0, true)?;
        Ok(())
    }
}

fn local_child<'a>(parent: &Path, path: &'a Path) -> io::Result<&'a std::ffi::OsStr> {
    path.file_name()
        .filter(|_| path.parent() == Some(parent))
        .ok_or_else(invalid_plan)
}

fn remove_remote(
    client: &mut Client,
    parent: &[u8],
    path: &[u8],
    parent_stamp: &DirectoryStamp,
    expected: Attributes,
    stopped: &impl Fn() -> bool,
    guard: &impl Fn(&mut Client) -> Result<()>,
) -> Result<Outcome> {
    let folder = Folder::read(client, parent.to_vec(), stopped)?;
    let owner = folder.owner();
    if !folder.trusted(owner) || !protected_target(expected, owner) {
        return Err(io::Error::other("Choose protected server removal entries and folders").into());
    }
    parent_stamp.verify(client, &Location::Remote(parent.to_vec()), false)?;
    verify_remote_entry(client, path, expected)?;
    let directory = expected.kind() == FileKind::Directory;
    if directory && !empty_remote(client, path)? {
        return Ok(Outcome::KeptNotEmpty);
    }

    guard(client)?;
    let stage = RemoteStage::claim(client, parent, owner)?;
    let recover = || {
        format!(
            " · Check private quarantine {}",
            stage.payload.escape_ascii()
        )
    };
    let moved = (|| -> Result<()> {
        folder.verify(client, false)?;
        stage.verify(client)?;
        verify_remote_entry(client, path, expected)?;
        require_absent(client, &stage.payload)?;
        guard(client)?;
        // POSIX rename moves one object to the empty private quarantine. The
        // public path is never sent to REMOVE or RMDIR.
        client.replace(path, &stage.payload)?;
        Ok(())
    })();
    if let Err(error) = moved {
        if !error.disconnected && remove_empty_stage(client, &folder, &stage).is_ok() {
            return Err(error);
        }
        return Err(error.context(recover()));
    }

    let result = (|| -> Result<()> {
        stage.verify(client)?;
        verify_remote_entry(client, &stage.payload, expected)?;
        guard(client)?;
        stage.verify(client)?;
        verify_remote_entry(client, &stage.payload, expected)?;
        let result = if directory {
            client.remove_dir(&stage.payload)
        } else {
            client.remove_file(&stage.payload)
        };
        result.map_err(|error| {
            let failure = Failure::from(error);
            if failure.disconnected {
                failure.context(" · Server removal could not be confirmed")
            } else {
                failure
            }
        })?;
        Ok(())
    })();
    result.map_err(|error| error.context(recover()))?;
    let warning = remove_empty_stage(client, &folder, &stage)
        .err()
        .map(|error| error.context(recover()));
    Ok(Outcome::Removed(warning))
}

fn protected_target(entry: Attributes, owner: u32) -> bool {
    matches!(entry.kind(), FileKind::File | FileKind::Directory)
        && entry.uid.is_some_and(|uid| uid == owner || uid == 0)
        && entry.gid.is_some()
        && entry.permissions.is_some_and(|mode| mode & 0o022 == 0)
        && entry.modified.is_some()
        && entry.size.is_some()
}

fn verify_remote_entry(client: &mut Client, path: &[u8], expected: Attributes) -> Result<()> {
    if !unchanged(expected, client.lstat(path)?) || client.canonicalize(path)? != path {
        return Err(io::Error::other("The reviewed server removal entry changed").into());
    }
    Ok(())
}

fn empty_remote(client: &mut Client, path: &[u8]) -> Result<bool> {
    match client.read_dir_limited(path, 3, 16 * 1024) {
        Ok(entries) => Ok(entries.is_empty()),
        Err(Error::Limit(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn remove_empty_stage(client: &mut Client, folder: &Folder, stage: &RemoteStage) -> Result<()> {
    folder.verify(client, false)?;
    stage.verify(client)?;
    client.remove_dir(&stage.directory)?;
    Ok(())
}

fn invalid_plan() -> io::Error {
    io::Error::other("The reviewed destination-only entry could not be verified")
}
