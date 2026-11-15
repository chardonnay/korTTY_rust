//! Stage one bounded rsync directory and verify every entry through pinned SFTP.

use std::collections::BTreeMap;

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use kortty_server_archive::rsync_tree::{self as protocol, projection};

use crate::scheduler_window::{ConnectionRef, RemotePath};
use crate::sftp_window::VerifiedTreeStage;
use crate::sftp_window::scheduled::Client;

use super::*;

const MAX_PATH_BYTES: usize = 2 * 1024 * 1024;
const MAX_LISTING_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const OUTPUT_POLL: Duration = Duration::from_millis(100);
const TREE_LIMIT: &str =
    "The rsync source exceeds 4,096 entries, 256 MiB, or the tree path budget.";
const CHECK_FAILED: &str = "The staged rsync directory differs from the checked server tree.";
pub(super) const UNCERTAIN: &str =
    "Rsync publication could not be confirmed. Check the destination and private stage.";

pub(super) struct Destination(VerifiedTreeStage);

pub(super) struct Output<'a> {
    pub(super) directory: &'a local_tree::Directory,
    pub(super) owner_only: bool,
}

impl<'a> Output<'a> {
    pub(super) fn inherited(directory: &'a local_tree::Directory) -> Self {
        Self {
            directory,
            owner_only: false,
        }
    }
}

impl Destination {
    pub(super) fn create(path: &Path) -> Result<Self, Failure> {
        let mut cleanup_failed = false;
        VerifiedTreeStage::create(path, &mut cleanup_failed)
            .map(Self)
            .map_err(|_| {
                if cleanup_failed {
                    Failure::failed(
                        "Could not prepare the rsync destination. A private stage may remain.",
                    )
                } else {
                    Failure::blocked("Choose a new local folder in an existing checked folder.")
                }
            })
    }
}

pub(super) fn apply<C: Fn() -> bool>(
    client: &mut Client,
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    reference: &ConnectionRef,
    Destination(mut destination): Destination,
    preflight: Option<Manifest>,
) -> Result<String, Failure> {
    let stopped = transfer.stopped;
    let guard = transfer.execution_guard.clone();
    let result = (|| {
        destination.verify().map_err(|_| changed_destination())?;
        let manifest = receive(
            client,
            transfer,
            listener,
            bridge_endpoint,
            reference,
            Output::inherited(destination.root()),
            preflight,
        )?;
        destination
            .publish(&|| stopped() || require_authority(guard.as_ref()).is_err())
            .map_err(|error| {
                if error.uncertain {
                    Failure::failed(UNCERTAIN)
                } else if error.error.kind() == io::ErrorKind::AlreadyExists {
                    changed_destination()
                } else {
                    Failure::failed("Could not publish the verified rsync directory.")
                }
            })?;
        Ok(manifest.download_detail())
    })();
    match (result, destination.cleanup()) {
        (Ok(detail), Ok(())) => Ok(detail),
        (Ok(detail), Err(_)) => Ok(format!("{detail} A private staging folder may remain.")),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(_)) if error.detail == UNCERTAIN => Err(error),
        (Err(error), Err(_)) => Err(Failure {
            outcome: error.outcome,
            detail: "The rsync directory transfer did not complete. A private staging folder may remain.",
        }),
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) fn receive_existing<C: Fn() -> bool>(
    client: &mut Client,
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    reference: &ConnectionRef,
    preflight: Option<Manifest>,
) -> Result<(local_tree::Directory, Manifest), Failure> {
    let output = create_scratch_child(
        transfer.temp.directory().map_err(|_| scratch_failed())?,
        OsStr::new("verified"),
    )
    .map_err(|_| scratch_failed())?;
    let manifest = receive(
        client,
        transfer,
        listener,
        bridge_endpoint,
        reference,
        Output {
            directory: &output,
            owner_only: true,
        },
        preflight,
    )?;
    Ok((output, manifest))
}

pub(super) fn receive<C: Fn() -> bool>(
    client: &mut Client,
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    reference: &ConnectionRef,
    output: Output<'_>,
    preflight: Option<Manifest>,
) -> Result<Manifest, Failure> {
    let stopped = transfer.stopped;
    let path = &transfer.action.remote;
    let temp = transfer.temp;
    let guard = transfer.execution_guard.clone();
    authorize(reference)?;
    let before = match preflight {
        Some(before) => before,
        None => Manifest::read(client, path, stopped)?,
    };
    let scratch = temp.directory().map_err(|_| scratch_failed())?;
    let stage = create_scratch_child(scratch, OsStr::new(PAYLOAD)).map_err(|_| scratch_failed())?;
    authorize(reference)?;
    let mut last_checked = Instant::now();
    let check_output = || {
        if last_checked.elapsed() >= OUTPUT_POLL {
            last_checked = Instant::now();
            let mut budget = OutputBudget::default();
            budget.directory(&stage, 0, stopped).map_err(|_| {
                Failure::failed("Rsync stopped because its private output exceeded the receive limit or contained an unsafe entry.")
            })?;
        }
        Ok(())
    };
    transfer.run_checked(
        listener,
        bridge_endpoint,
        path.as_str(),
        false,
        ReceiveCheck {
            protocol_limit: before.budget.bytes + PROTOCOL_OVERHEAD,
            check_output,
        },
    )?;
    let local_tree::Entry::Directory(received) = local_tree::entry(
        temp.directory().map_err(|_| scratch_failed())?,
        OsStr::new(PAYLOAD),
    )
    .map_err(|_| bad_tree())?
    else {
        return Err(bad_tree());
    };
    if !stage.same_as(&received).map_err(|_| bad_tree())? {
        return Err(bad_tree());
    }
    let mut copy = VerifiedCopy { client, stopped };
    copy.directory(&before.root, &received, output.directory, output.owner_only)?;
    before.require_current(client, stopped)?;
    require_running(stopped).map_err(|_| transfer_stopped())?;
    authorize(reference)?;
    require_authority(guard.as_ref())?;
    Ok(before)
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) fn receive_projected_file<C: Fn() -> bool>(
    client: &mut Client,
    transfer: Transfer<'_, C>,
    listener: Listener,
    bridge_endpoint: &str,
    before: &Manifest,
    output: File,
) -> Result<(), Failure> {
    let temp = transfer.temp;
    let stopped = transfer.stopped;
    transfer.run_checked(
        listener,
        bridge_endpoint,
        before.root.path.as_str(),
        false,
        ReceiveCheck {
            protocol_limit: before.bytes() + PROTOCOL_OVERHEAD,
            check_output: || Ok(()),
        },
    )?;
    let payload = temp.payload().map_err(|_| bad_tree())?;
    VerifiedCopy { client, stopped }.file(&before.root, payload, output)?;
    before.require_current(client, stopped)
}

#[derive(Default)]
struct OutputBudget {
    entries: usize,
    bytes: u64,
}

impl OutputBudget {
    fn directory(
        &mut self,
        directory: &local_tree::Directory,
        depth: usize,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<()> {
        let limit = || io::Error::other("Rsync receive tree limit exceeded");
        if depth > MAX_DEPTH {
            return Err(limit());
        }
        // A receiver can briefly hold a temporary name next to the final name.
        let remaining = (rsync::MAX_TREE_ENTRIES + 2)
            .checked_sub(self.entries)
            .ok_or_else(limit)?;
        for name in directory.names(remaining)? {
            require_running(stopped)?;
            self.entries += 1;
            if self.entries > rsync::MAX_TREE_ENTRIES + 2 {
                return Err(limit());
            }
            match local_tree::entry(directory, &name) {
                Ok(local_tree::Entry::Directory(child)) => {
                    self.directory(&child, depth + 1, stopped)?
                }
                Ok(local_tree::Entry::File(file)) => {
                    require_single_link(&file)?;
                    self.bytes = self.bytes.saturating_add(file.metadata()?.len());
                    if self.bytes > MAX_FILE {
                        return Err(limit());
                    }
                }
                // Rsync can rename a temporary file between listing and opening it.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
                Ok(local_tree::Entry::Skip) => return Err(limit()),
            }
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq)]
pub(super) struct Manifest {
    root: Node,
    budget: Budget,
}

#[derive(Default, Eq, PartialEq)]
struct Budget {
    entries: usize,
    files: usize,
    paths: usize,
    bytes: u64,
}

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub(super) entries: usize,
    pub(super) bytes: u64,
}

impl Budget {
    fn add(&mut self, path: &RemotePath, file: Option<u64>, limits: Limits) -> Result<(), Failure> {
        self.entries += 1;
        self.paths += path.as_str().len();
        if let Some(bytes) = file {
            self.bytes = self.bytes.saturating_add(bytes);
            self.files += 1;
        }
        if self.entries > limits.entries || self.paths > MAX_PATH_BYTES || self.bytes > limits.bytes
        {
            return Err(Failure::blocked(TREE_LIMIT));
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq)]
struct Node {
    path: RemotePath,
    attrs: Attributes,
    digest: Option<[u8; 32]>,
    children: BTreeMap<OsString, Node>,
}

impl Manifest {
    fn require_current(
        &self,
        client: &mut Client,
        stopped: &impl Fn() -> bool,
    ) -> Result<(), Failure> {
        let limits = Limits {
            entries: self.entries(),
            bytes: self.bytes(),
        };
        let current =
            Self::read_limited(client, &self.root.path, limits, stopped).map_err(|_| bad_tree())?;
        if current != *self {
            return Err(bad_tree());
        }
        Ok(())
    }

    pub(super) fn download_detail(&self) -> String {
        format!(
            "Downloaded {} verified files and {} folders ({} bytes) via native rsync and SSH.",
            self.budget.files,
            self.budget.entries - self.budget.files,
            self.budget.bytes
        )
    }

    pub(super) fn read(
        client: &mut Client,
        path: &RemotePath,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        let result = Self::read_entry(client, path, stopped)?;
        if !result.is_directory() {
            return Err(Failure::blocked(
                "Choose an ordinary server directory for the rsync download.",
            ));
        }
        Ok(result)
    }

    pub(super) fn read_entry(
        client: &mut Client,
        path: &RemotePath,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        Self::read_limited(
            client,
            path,
            Limits {
                entries: rsync::MAX_TREE_ENTRIES,
                bytes: MAX_FILE,
            },
            stopped,
        )
    }

    pub(super) fn read_limited(
        client: &mut Client,
        path: &RemotePath,
        limits: Limits,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        let mut budget = Budget::default();
        let mut root = Node::scan(client, path.clone(), &mut budget, limits, 0, stopped)?;
        root.measure(client, stopped)?;
        Ok(Self { root, budget })
    }

    pub(super) fn is_directory(&self) -> bool {
        self.root.attrs.kind() == FileKind::Directory
    }

    pub(super) fn bytes(&self) -> u64 {
        self.budget.bytes
    }

    pub(super) fn entries(&self) -> usize {
        self.budget.entries
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(super) fn inventory(&self) -> Result<protocol::Inventory, Failure> {
        let mut manifest = protocol::Manifest::default();
        if !self.is_directory() {
            manifest
                .add("", self.root.content()?)
                .map_err(|_| bad_tree())?;
            return Ok(manifest.finish());
        }
        self.root
            .add_children(self.root.path.as_str(), &mut |relative, file| {
                manifest.add(relative, file).map_err(|_| bad_tree())
            })?;
        Ok(manifest.finish())
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(super) fn projection_entries(&self) -> Result<Vec<projection::Entry>, Failure> {
        let mut entries = Vec::with_capacity(self.entries());
        if self.is_directory() {
            self.root
                .add_children(self.root.path.as_str(), &mut |relative, file| {
                    entries.push(projection::Entry {
                        relative: relative.into(),
                        file,
                    });
                    Ok(())
                })?;
        } else {
            entries.push(projection::Entry {
                relative: String::new(),
                file: self.root.content()?,
            });
        }
        Ok(entries)
    }
}

impl Node {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    fn add_children(
        &self,
        root: &str,
        add: &mut impl FnMut(&str, Option<(u64, [u8; 32])>) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let mut children: Vec<_> = self.children.values().collect();
        children.sort_unstable_by_key(|child| child.path.basename());
        for child in children {
            let relative = child
                .path
                .as_str()
                .strip_prefix(root)
                .and_then(|suffix| suffix.strip_prefix('/'))
                .ok_or_else(bad_tree)?;
            if !protocol::valid_name(child.path.basename()) {
                return Err(Failure::blocked(
                    "The rsync source contains a name that cannot be merged safely.",
                ));
            }
            add(relative, child.content()?)?;
            child.add_children(root, add)?;
        }
        Ok(())
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    fn content(&self) -> Result<Option<(u64, [u8; 32])>, Failure> {
        match self.attrs.kind() {
            FileKind::File => self
                .attrs
                .size
                .zip(self.digest)
                .map(Some)
                .ok_or_else(bad_tree),
            FileKind::Directory => Ok(None),
            _ => Err(bad_tree()),
        }
    }

    fn scan(
        client: &mut Client,
        path: RemotePath,
        budget: &mut Budget,
        limits: Limits,
        depth: usize,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        if depth > MAX_DEPTH {
            return Err(Failure::blocked(TREE_LIMIT));
        }
        let mut attrs = remote::verify_entry(client, path.as_str())?;
        if attrs.modified.is_none() {
            return Err(Failure::blocked(
                "The rsync source requires server modification times.",
            ));
        }
        // Reads can update atime on both the SFTP and rsync server sides.
        attrs.accessed = None;
        let mut children = BTreeMap::new();
        if attrs.kind() == FileKind::File {
            budget.add(&path, Some(checked_size(attrs)?), limits)?;
        } else {
            budget.add(&path, None, limits)?;
            let remaining = limits.entries - budget.entries;
            let listing = client
                .read_dir_limited(path.as_str(), remaining + 2, MAX_LISTING_BYTES)
                .map_err(|_| {
                    Failure::failed("Could not read the bounded rsync source directory.")
                })?;
            if listing.len() > remaining {
                return Err(Failure::blocked(TREE_LIMIT));
            }
            #[cfg(windows)]
            let mut local_names = std::collections::BTreeSet::new();
            for entry in listing {
                let child = path.child(&entry.name).map_err(Failure::blocked)?;
                #[cfg(windows)]
                let name = {
                    let name = crate::sftp_window::checked_local_component(&entry.name)
                        .map_err(|_| Failure::blocked("The rsync source contains a filename that Windows cannot represent safely."))?;
                    if !local_names.insert(name.to_string_lossy().to_lowercase()) {
                        return Err(Failure::blocked(
                            "The rsync source contains filenames that collide on Windows.",
                        ));
                    }
                    name
                };
                #[cfg(unix)]
                let name = OsString::from(child.basename());
                if children.contains_key(&name) {
                    return Err(Failure::failed(
                        "The server listed duplicate rsync source names.",
                    ));
                }
                children.insert(
                    name,
                    Self::scan(client, child, budget, limits, depth + 1, stopped)?,
                );
            }
        }
        Ok(Self {
            path,
            attrs,
            digest: None,
            children,
        })
    }

    fn measure(&mut self, client: &mut Client, stopped: &impl Fn() -> bool) -> Result<(), Failure> {
        require_running(stopped).map_err(|_| transfer_stopped())?;
        if self.attrs.kind() == FileKind::File {
            let mut digest = HashAlgorithm::Sha256.new_hasher().map_err(|_| bad_tree())?;
            self.read_file(
                client,
                DigestCopy {
                    stopped,
                    digest: digest.as_mut(),
                    output: io::sink(),
                },
            )?;
            self.digest = Some(
                digest
                    .finalize()
                    .as_ref()
                    .try_into()
                    .expect("SHA-256 produces 32 bytes"),
            );
        }
        for child in self.children.values_mut() {
            child.measure(client, stopped)?;
        }
        Ok(())
    }

    fn read_file(&self, client: &mut Client, destination: impl Write) -> Result<(), Failure> {
        remote::verify_canonical(client, self.path.as_str())?;
        client
            .download_regular(self.path.as_str().as_bytes(), self.attrs, destination)
            .map_err(|_| bad_tree())?;
        remote::verify_canonical(client, self.path.as_str())
    }
}

struct DigestCopy<'a, C, W> {
    stopped: &'a C,
    digest: &'a mut dyn rsa::signature::digest::DynDigest,
    output: W,
}

impl<C: Fn() -> bool, W: Write> Write for DigestCopy<'_, C, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        require_running(self.stopped)?;
        self.output.write_all(bytes)?;
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

struct VerifiedCopy<'a, C> {
    client: &'a mut Client,
    stopped: &'a C,
}

impl<C: Fn() -> bool> VerifiedCopy<'_, C> {
    fn directory(
        &mut self,
        expected: &Node,
        received: &local_tree::Directory,
        output: &local_tree::Directory,
        owner_only: bool,
    ) -> Result<(), Failure> {
        self.checkpoint()?;
        let mut actual = received
            .names(expected.children.len())
            .map_err(|_| bad_tree())?;
        actual.sort_unstable();
        if !actual.iter().eq(expected.children.keys()) {
            return Err(bad_tree());
        }
        for (name, child) in &expected.children {
            self.checkpoint()?;
            match local_tree::entry(received, name).map_err(|_| bad_tree())? {
                local_tree::Entry::Directory(directory)
                    if child.attrs.kind() == FileKind::Directory =>
                {
                    let destination = if owner_only {
                        output.create_child(name)
                    } else {
                        output.create_transfer_child(name)
                    }
                    .map_err(|_| scratch_failed())?;
                    self.directory(child, &directory, &destination, owner_only)?;
                }
                local_tree::Entry::File(file) if child.attrs.kind() == FileKind::File => {
                    let destination = if owner_only {
                        output.create_file(name)
                    } else {
                        output.create_ordinary_file(name)
                    }
                    .map_err(|_| scratch_failed())?;
                    self.file(child, file, destination)?;
                }
                _ => return Err(bad_tree()),
            }
        }
        #[cfg(unix)]
        output.file().sync_all().map_err(|_| scratch_failed())?;
        Ok(())
    }

    fn file(
        &mut self,
        expected: &Node,
        mut received: File,
        mut output: File,
    ) -> Result<(), Failure> {
        require_single_link(&received).map_err(|_| bad_tree())?;
        if Some(received.metadata().map_err(|_| bad_tree())?.len()) != expected.attrs.size {
            return Err(bad_tree());
        }
        let stamp = FileStamp::read(&received).map_err(|_| bad_tree())?;
        let mut digest = HashAlgorithm::Sha256.new_hasher().map_err(|_| bad_tree())?;
        expected.read_file(
            self.client,
            DigestCopy {
                stopped: self.stopped,
                digest: digest.as_mut(),
                output: VerifyAndCopy {
                    source: &mut received,
                    output: &mut output,
                    cancelled: self.stopped,
                },
            },
        )?;
        if Some(digest.finalize().as_ref())
            != expected.digest.as_ref().map(|value| value.as_slice())
        {
            return Err(bad_tree());
        }
        stamp.require_unchanged(&received).map_err(|_| bad_tree())?;
        output.sync_all().map_err(|_| scratch_failed())?;
        Ok(())
    }

    fn checkpoint(&self) -> Result<(), Failure> {
        require_running(self.stopped).map_err(|_| transfer_stopped())
    }
}

fn scratch_failed() -> Failure {
    Failure::failed("Could not prepare or write the private rsync directory stage.")
}

fn bad_tree() -> Failure {
    Failure::failed(CHECK_FAILED)
}

fn changed_destination() -> Failure {
    Failure::blocked("The new local rsync directory destination changed or already exists.")
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn rsync_receive_budget_rejects_sparse_excess_and_links() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let stage = local_tree::Directory::open_path(&root).unwrap();
        let part = root.join("partial");
        File::create(&part).unwrap().set_len(MAX_FILE + 1).unwrap();
        assert!(
            OutputBudget::default()
                .directory(&stage, 0, &|| false)
                .is_err()
        );
        fs::remove_file(&part).unwrap();
        fs::write(&part, b"small").unwrap();
        assert!(
            OutputBudget::default()
                .directory(&stage, 0, &|| false)
                .is_ok()
        );
        symlink(&part, root.join("link")).unwrap();
        assert!(
            OutputBudget::default()
                .directory(&stage, 0, &|| false)
                .is_err()
        );
    }
}
