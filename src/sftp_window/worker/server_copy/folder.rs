use std::collections::{BTreeMap, BTreeSet};

use crate::sftp_window::tree::snapshot::{self, Entry, Manifest, Relative};

use super::*;

const MAX_ENTRIES: usize = 256;
const MAX_REVIEW_BYTES: usize = 256 * 1024;

pub(super) struct Plan {
    source: Manifest,
    pub(super) files: usize,
    pub(super) entries: Vec<(String, bool)>,
}

pub(super) fn prepare(
    client: &mut Client,
    source: &[u8],
    destination: &[u8],
    owner: u32,
    stopped: &impl Fn() -> bool,
) -> Result<(Plan, u64)> {
    let source = read_source(client, source, owner, stopped)?;
    let mut bytes = 0u64;
    let mut files = 0;
    let mut review_bytes = 0;
    let mut entries = Vec::with_capacity(source.entries.len());
    for (relative, entry) in &source.entries {
        let Entry::Remote(attributes) = entry else {
            return Err(io::Error::other("A source name cannot be copied unambiguously").into());
        };
        let folder = attributes.kind() == FileKind::Directory;
        if !folder {
            bytes = bytes
                .checked_add(protected_file(*attributes, owner)?)
                .filter(|&sum| sum <= MAX_BYTES)
                .ok_or_else(|| io::Error::other("Server folder copy exceeds 1 GiB"))?;
            files += 1;
        }
        review_root(&source.location(relative)?)?;
        review_root(&Location::Remote(files::remote_join(
            destination,
            relative.bytes(),
        )))?;
        let label = relative.review_label();
        review_bytes += label.len();
        if review_bytes > MAX_REVIEW_BYTES {
            return Err(
                io::Error::other("The exact folder paths exceed the copy review limit").into(),
            );
        }
        entries.push((label, folder));
    }
    Ok((
        Plan {
            source,
            files,
            entries,
        },
        bytes,
    ))
}

fn read_source(
    client: &mut Client,
    root: &[u8],
    owner: u32,
    stopped: &impl Fn() -> bool,
) -> Result<Manifest> {
    snapshot::read_remote_checked(
        client,
        root.to_vec(),
        MAX_ENTRIES,
        &|attributes| match attributes.kind() {
            FileKind::Directory if attributes.size.is_some() && protected(attributes, owner) => {
                Ok(())
            }
            FileKind::Directory => Err(unsafe_parent()),
            FileKind::File => protected_file(attributes, owner).map(|_| ()),
            _ => Err(io::Error::other(
                "Server folder copy requires regular files and directories without links",
            )),
        },
        stopped,
    )
}

pub(super) fn apply(
    client: &mut Client,
    plan: &super::Plan,
    tree: &Plan,
    progress: &Progress,
    stopped: &impl Fn() -> bool,
    disconnected: &impl Fn() -> bool,
) -> Result<(String, bool)> {
    let mut copy = Execution {
        plan,
        tree,
        progress,
        stopped,
        disconnected,
        directories: BTreeMap::new(),
        stage: None,
        staged_bytes: 0,
        outcome: Outcome::default(),
    };
    let result = copy.run(client);
    let retained = copy.stage.as_ref().and_then(|stage| {
        let reachable = !disconnected()
            && result
                .as_ref()
                .err()
                .is_none_or(|error| !error.disconnected);
        if reachable {
            match cleanup(client, &plan.parents[1], stage, copy.staged_bytes) {
                Ok(()) => return None,
                Err(error) => return Some((stage.label(), error.disconnected)),
            }
        }
        Some((stage.label(), false))
    });
    copy.outcome.report(result, &plan.labels[1], retained)
}

struct Execution<'a> {
    plan: &'a super::Plan,
    tree: &'a Plan,
    progress: &'a Progress,
    stopped: &'a dyn Fn() -> bool,
    disconnected: &'a dyn Fn() -> bool,
    directories: BTreeMap<Option<Relative>, Directory>,
    stage: Option<RemoteStage>,
    staged_bytes: u64,
    outcome: Outcome,
}

impl Execution<'_> {
    fn run(&mut self, client: &mut Client) -> Result<()> {
        let [source, target] = &self.plan.parents;
        running(&self.stopped)?;
        supported(client)?;
        source.verify(client, true)?;
        target.verify(client, true)?;
        let current = read_source(client, &self.plan.source, target.owner(), &self.stopped)?;
        if !self.tree.source.matches(&current) {
            return Err(Error::SourceChanged.into());
        }
        source.verify(client, true)?;
        target.verify(client, true)?;
        absent(client, &self.plan.destination)?;
        running(&self.stopped)?;
        self.mkdir(client, None, self.plan.destination.clone())?;
        if self.tree.files != 0 {
            running(&self.stopped)?;
            target.verify(client, false)?;
            self.stage = Some(RemoteStage::claim(client, &target.path, target.owner())?);
        }
        for (relative, entry) in &self.tree.source.entries {
            let Entry::Remote(attributes) = entry else {
                return Err(Error::SourceChanged.into());
            };
            self.copy_entry(client, relative, *attributes)
                .map_err(|error| error.context(format!(" · {}", relative.review_label())))?;
        }
        for directory in self.directories.values() {
            running(&self.stopped)?;
            directory.verify(client)?;
        }
        Ok(())
    }

    fn copy_entry(
        &mut self,
        client: &mut Client,
        relative: &Relative,
        attributes: Attributes,
    ) -> Result<()> {
        running(&self.stopped)?;
        let source = files::remote_join(&self.plan.source, relative.bytes());
        let destination = files::remote_join(&self.plan.destination, relative.bytes());
        let parent = relative.parent();
        self.verify_source_parent(client, parent.clone())?;
        source_current(client, &source, attributes)?;
        self.verify_target_parent(client, parent.as_ref())?;
        absent(client, &destination)?;
        running(&self.stopped)?;
        if attributes.kind() == FileKind::Directory {
            self.mkdir(client, Some(relative.clone()), destination)?;
        } else {
            self.copy_file(client, &source, destination, attributes, parent.as_ref())?;
        }
        self.directories
            .get_mut(&parent)
            .ok_or_else(changed_destination)?
            .add_child(client, relative.basename())
    }

    fn mkdir(&mut self, client: &mut Client, key: Option<Relative>, path: Vec<u8>) -> Result<()> {
        self.outcome
            .mutation(client.mkdir_private(&path), &path, self.disconnected)?;
        self.outcome.folders += 1;
        let directory = Directory::created(client, path, self.plan.parents[1].owner())?;
        self.directories.insert(key, directory);
        Ok(())
    }

    fn verify_source_parent(
        &self,
        client: &mut Client,
        mut parent: Option<Relative>,
    ) -> Result<()> {
        self.plan.parents[0].verify(client, false)?;
        loop {
            running(&self.stopped)?;
            let Some(path) = parent else {
                let (root, stamp) = self.tree.source.root_directory();
                return stamp.verify(client, &root, true).map(|_| ());
            };
            let (location, stamp) = self
                .tree
                .source
                .directory(&path)
                .ok_or(Error::SourceChanged)?;
            stamp.verify(client, &location, true)?;
            parent = path.parent();
        }
    }

    fn verify_target_parent(&self, client: &mut Client, parent: Option<&Relative>) -> Result<()> {
        self.plan.parents[1].verify(client, false)?;
        let mut key = parent.cloned();
        loop {
            running(&self.stopped)?;
            let current = self.directories.get(&key).ok_or_else(changed_destination)?;
            if key.as_ref() == parent {
                current.verify(client)?;
            } else {
                current.checked_stamp(client, true)?;
            }
            let Some(path) = key else { return Ok(()) };
            key = path.parent();
        }
    }

    fn copy_file(
        &mut self,
        client: &mut Client,
        source: &[u8],
        destination: Vec<u8>,
        attributes: Attributes,
        parent: Option<&Relative>,
    ) -> Result<()> {
        let stage = self.stage.as_ref().expect("stage for reviewed files");
        let bytes = attributes.size.expect("reviewed file size");
        self.staged_bytes = bytes;
        stage.verify(client)?;
        client.copy_regular_new_with_progress(
            source,
            attributes,
            &stage.payload,
            self.stopped,
            |bytes| {
                self.progress
                    .bytes
                    .store(self.outcome.bytes + bytes, Ordering::Relaxed);
            },
        )?;
        self.verify_source_parent(client, parent.cloned())?;
        source_current(client, source, attributes)?;
        self.verify_target_parent(client, parent)?;
        stage.verify(client)?;
        let payload = client.lstat(&stage.payload)?;
        if !private_payload(payload, stage) || payload.size != Some(bytes) {
            return Err(io::Error::other("The private copy payload changed").into());
        }
        running(&self.stopped)?;
        self.outcome.mutation(
            client.hardlink(&stage.payload, &destination),
            &destination,
            self.disconnected,
        )?;
        self.outcome.files += 1;
        self.outcome.bytes += bytes;
        self.plan.parents[1].verify(client, false)?;
        stage.verify(client)?;
        if !unchanged(payload, client.lstat(&stage.payload)?) {
            return Err(io::Error::other("The private copy payload changed").into());
        }
        client.remove_file(&stage.payload)?;
        Ok(())
    }
}

struct Directory {
    path: Vec<u8>,
    stamp: Attributes,
    children: BTreeSet<Vec<u8>>,
}

impl Directory {
    fn created(client: &mut Client, path: Vec<u8>, owner: u32) -> Result<Self> {
        let stamp = client.lstat(&path)?;
        if stamp.kind() != FileKind::Directory
            || stamp.uid != Some(owner)
            || stamp.gid.is_none()
            || stamp.size.is_none()
            || stamp.modified.is_none()
            || stamp.permissions.is_none_or(|mode| mode & 0o7777 != 0o700)
        {
            return Err(changed_destination());
        }
        let directory = Self {
            path,
            stamp,
            children: BTreeSet::new(),
        };
        directory.verify(client)?;
        Ok(directory)
    }

    fn checked_stamp(&self, client: &mut Client, strict: bool) -> Result<Attributes> {
        let current = client.lstat(&self.path)?;
        if current.kind() != FileKind::Directory
            || current.uid != self.stamp.uid
            || current.gid != self.stamp.gid
            || current.permissions != self.stamp.permissions
            || current.size.is_none()
            || current.modified.is_none()
            || strict && !unchanged(self.stamp, current)
            || client.canonicalize(&self.path)? != self.path
        {
            return Err(changed_destination());
        }
        Ok(current)
    }

    fn verify_children(&self, client: &mut Client) -> Result<()> {
        let children =
            client.read_dir_limited(&self.path, self.children.len() + 2, MAX_REVIEW_BYTES)?;
        let count = children.len();
        let names = children
            .into_iter()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        if count != names.len() || names != self.children {
            return Err(changed_destination());
        }
        Ok(())
    }

    fn verify(&self, client: &mut Client) -> Result<()> {
        self.checked_stamp(client, true)?;
        self.verify_children(client)?;
        self.checked_stamp(client, true)?;
        Ok(())
    }

    fn add_child(&mut self, client: &mut Client, name: &[u8]) -> Result<()> {
        self.children.insert(name.to_vec());
        // Refresh the expected size/mtime only after our own acknowledged change.
        let stamp = self.checked_stamp(client, false)?;
        self.verify_children(client)?;
        if !unchanged(stamp, self.checked_stamp(client, false)?) {
            return Err(changed_destination());
        }
        self.stamp = stamp;
        Ok(())
    }
}

#[derive(Default)]
struct Outcome {
    folders: usize,
    files: usize,
    bytes: u64,
    uncertain: Option<String>,
}

impl Outcome {
    fn mutation(
        &mut self,
        result: std::result::Result<(), Error>,
        path: &[u8],
        disconnected: &dyn Fn() -> bool,
    ) -> Result<()> {
        result.map_err(|error| {
            let error = Failure::from(error);
            if error.disconnected || disconnected() {
                self.uncertain = Some(path.escape_ascii().to_string());
            }
            error
        })
    }

    fn report(
        self,
        result: Result<()>,
        destination: &str,
        stage: Option<(String, bool)>,
    ) -> Result<(String, bool)> {
        let counts = format!(
            "{} folders · {} files · {} bytes",
            self.folders, self.files, self.bytes
        );
        let disconnected = stage
            .as_ref()
            .is_some_and(|(_, disconnected)| *disconnected);
        let retained = stage.map_or_else(String::new, |(path, _)| {
            format!(" · Check private staging path {path}")
        });
        match result {
            Ok(()) => Ok((
                format!("Copied {counts} on the server{retained}"),
                disconnected,
            )),
            Err(mut error) => {
                error.disconnected |= disconnected;
                let partial = if self.folders == 0 && self.uncertain.is_none() {
                    String::new()
                } else {
                    format!(
                        " · Confirmed {counts} · Partial destination may remain at {destination}"
                    )
                };
                let uncertain = self.uncertain.map_or_else(String::new, |path| {
                    format!(" · Server publication could not be confirmed at {path}")
                });
                Err(error.context(format!("{partial}{uncertain}{retained}")))
            }
        }
    }
}

fn changed_destination() -> Failure {
    io::Error::other("A new server copy directory changed or could not be verified").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_directory_ack_reports_exact_uncertain_path_without_counting_it() {
        let mut outcome = Outcome::default();
        let result = outcome.mutation(
            Err(Error::Transport(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "lost reply",
            ))),
            b"/destination/folder\xff",
            &|| false,
        );
        let report = outcome
            .report(result, r"Server · /destination/folder\xff", None)
            .unwrap_err();
        assert!(report.disconnected);
        assert!(
            report
                .message
                .contains("Confirmed 0 folders · 0 files · 0 bytes")
        );
        assert!(
            report
                .message
                .contains(r"Server publication could not be confirmed at /destination/folder\xff")
        );
    }
}
