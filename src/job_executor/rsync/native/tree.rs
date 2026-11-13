//! Stage one bounded rsync directory and verify every entry through pinned SFTP.

use std::collections::BTreeMap;

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

pub(super) struct Destination(VerifiedTreeStage);

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
    listener: UnixListener,
    socket: &Path,
    reference: &ConnectionRef,
    Destination(mut destination): Destination,
) -> Result<String, Failure> {
    let stopped = transfer.stopped;
    let path = &transfer.action.remote;
    let temp = transfer.temp;
    let result = (|| {
        authorize(reference)?;
        destination.verify().map_err(|_| changed_destination())?;
        let before = Manifest::read(client, path, stopped)?;
        let scratch = temp.directory().map_err(|_| scratch_failed())?;
        let stage = scratch
            .create_child(OsStr::new(PAYLOAD))
            .map_err(|_| scratch_failed())?;
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
            socket,
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
        copy.directory(&before.root, &received, destination.root())?;
        if Manifest::read(client, path, stopped)? != before {
            return Err(bad_tree());
        }
        require_running(stopped).map_err(|_| transfer_stopped())?;
        authorize(reference)?;
        destination.publish(stopped).map_err(|error| {
            if error.uncertain {
                Failure::failed("Rsync publication could not be confirmed. Check the destination and private stage.")
            } else if error.error.kind() == io::ErrorKind::AlreadyExists {
                changed_destination()
            } else {
                Failure::failed("Could not publish the verified rsync directory.")
            }
        })?;
        Ok(format!(
            "Downloaded {} verified files and {} folders ({} bytes) via native rsync and SSH.",
            before.budget.files,
            before.budget.entries - before.budget.files,
            before.budget.bytes
        ))
    })();
    match (result, destination.cleanup()) {
        (Ok(detail), Ok(())) => Ok(detail),
        (Ok(detail), Err(_)) => Ok(format!("{detail} A private staging folder may remain.")),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(_)) => Err(Failure {
            outcome: error.outcome,
            detail: "The rsync directory transfer did not complete. A private staging folder may remain.",
        }),
    }
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
struct Manifest {
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

impl Budget {
    fn add(&mut self, path: &RemotePath, file: Option<u64>) -> Result<(), Failure> {
        self.entries += 1;
        self.paths += path.as_str().len();
        if let Some(bytes) = file {
            self.bytes = self.bytes.saturating_add(bytes);
            self.files += 1;
        }
        if self.entries > rsync::MAX_TREE_ENTRIES
            || self.paths > MAX_PATH_BYTES
            || self.bytes > MAX_FILE
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
    fn read(
        client: &mut Client,
        path: &RemotePath,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        let mut budget = Budget::default();
        let mut root = Node::scan(client, path.clone(), &mut budget, stopped)?;
        if root.attrs.kind() != FileKind::Directory {
            return Err(Failure::blocked(
                "Choose an ordinary server directory for the rsync download.",
            ));
        }
        root.measure(client, stopped)?;
        Ok(Self { root, budget })
    }
}

impl Node {
    fn scan(
        client: &mut Client,
        path: RemotePath,
        budget: &mut Budget,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        require_running(stopped).map_err(|_| transfer_stopped())?;
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
            budget.add(&path, Some(checked_size(attrs)?))?;
        } else {
            budget.add(&path, None)?;
            let remaining = rsync::MAX_TREE_ENTRIES - budget.entries;
            let listing = client
                .read_dir_limited(path.as_str(), remaining + 2, MAX_LISTING_BYTES)
                .map_err(|_| {
                    Failure::failed("Could not read the bounded rsync source directory.")
                })?;
            if listing.len() > remaining {
                return Err(Failure::blocked(TREE_LIMIT));
            }
            for entry in listing {
                let child = path.child(&entry.name).map_err(Failure::blocked)?;
                let name = OsString::from(child.basename());
                if children.contains_key(&name) {
                    return Err(Failure::failed(
                        "The server listed duplicate rsync source names.",
                    ));
                }
                children.insert(name, Self::scan(client, child, budget, stopped)?);
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
                    let destination = output
                        .create_transfer_child(name)
                        .map_err(|_| scratch_failed())?;
                    self.directory(child, &directory, &destination)?;
                }
                local_tree::Entry::File(file) if child.attrs.kind() == FileKind::File => {
                    self.file(
                        child,
                        file,
                        output
                            .create_ordinary_file(name)
                            .map_err(|_| scratch_failed())?,
                    )?;
                }
                _ => return Err(bad_tree()),
            }
        }
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

#[cfg(test)]
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
