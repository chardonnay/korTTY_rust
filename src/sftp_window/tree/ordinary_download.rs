//! Ordinary Scheduler trees inherit inside an enclosing private staging directory.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use super::*;
use crate::sftp_window::worker::ordinary_access::Access;
use crate::sftp_window::worker::resume::persist::Identity;

const PAYLOAD: &str = "payload";

impl Tree<'_> {
    pub(super) fn download_ordinary(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        source: &[u8],
        target: &Path,
    ) -> Result<()> {
        self.check_cancelled()?;
        remote_directory(client, source, false)?;
        let mut stage = Stage::create(target, &mut self.local_cleanup_failed)?;
        let result = (|| {
            stage.verify()?;
            self.download_contents(client, source, stage.root(), 0, stage.outer.as_ref())?;
            self.check_cancelled()?;
            match stage.publish(self.cancelled) {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.local_publication_uncertain = error.uncertain;
                    Err(Failure::from(error.error))
                }
            }
        })();
        let cleanup = stage.cleanup();
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(cleanup)) => {
                // A confirmed complete publication is not undone by empty-wrapper cleanup.
                self.local_cleanup_note = Some(format!(
                    " · The complete folder was published; private staging folder {} may remain ({cleanup})",
                    files::display(&stage.outer_path().to_string_lossy()),
                ));
                Ok(())
            }
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup)) => {
                self.local_cleanup_failed = true;
                Err(error.context(format!(
                    " · Private staging folder {} may remain ({cleanup})",
                    files::display(&stage.outer_path().to_string_lossy())
                )))
            }
        }
    }
}

pub(crate) struct PublicationError {
    pub(crate) error: io::Error,
    pub(crate) uncertain: bool,
}

pub(crate) struct Stage {
    path: PathBuf,
    parent: local::Directory,
    name: OsString,
    outer: Option<local::Directory>,
    payload: Option<local::Directory>,
    access: Access,
    published: bool,
}

impl Stage {
    pub(crate) fn create(path: &Path, cleanup_failed: &mut bool) -> io::Result<Self> {
        Self::create_checked(path, None, cleanup_failed)
    }

    pub(in crate::sftp_window) fn create_reviewed(
        path: &Path,
        expected: &Identity,
        cleanup_failed: &mut bool,
    ) -> io::Result<Self> {
        Self::create_checked(path, Some(expected), cleanup_failed)
    }

    fn create_checked(
        path: &Path,
        expected: Option<&Identity>,
        cleanup_failed: &mut bool,
    ) -> io::Result<Self> {
        let parent_path = parent(path)?;
        let parent = local::Directory::open_path(parent_path)?;
        let identity = Identity::directory(&parent)?;
        if expected.is_some_and(|expected| expected != &identity) {
            return Err(changed());
        }
        require_absent(path)?;
        let access = Access::capture_directory(parent_path, &identity)?;
        let name = OsString::from(files::nonce()?);
        let outer = parent.create_child(&name)?;
        let mut stage = Self {
            path: path.to_owned(),
            parent,
            name,
            outer: Some(outer),
            payload: None,
            access,
            published: false,
        };
        let result = (|| {
            require_private(stage.outer.as_ref().expect("owned outer stage"))?;
            let root = stage
                .outer
                .as_ref()
                .expect("owned outer stage")
                .create_tree_payload(OsStr::new(PAYLOAD))?;
            stage.payload = Some(root);
            stage.access.apply_to_directory(stage.root())?;
            stage.verify()
        })();
        if let Err(error) = result {
            return Err(match stage.cleanup() {
                Ok(()) => error,
                Err(cleanup) => {
                    *cleanup_failed = true;
                    io::Error::other(format!(
                        "{error}; private staging folder {} may remain ({cleanup})",
                        files::display(&stage.outer_path().to_string_lossy())
                    ))
                }
            });
        }
        Ok(stage)
    }

    pub(crate) fn root(&self) -> &local::Directory {
        self.payload.as_ref().expect("owned tree payload")
    }

    pub(in crate::sftp_window) fn verify_privacy(&self) -> io::Result<()> {
        require_private(self.outer.as_ref().ok_or_else(changed)?)
    }

    fn outer_path(&self) -> PathBuf {
        self.path
            .parent()
            .expect("validated parent")
            .join(&self.name)
    }

    pub(crate) fn verify(&self) -> io::Result<()> {
        self.access.verify_parent()?;
        let current_parent = local::Directory::open_path(parent(&self.path)?)?;
        if !self.parent.same_as(&current_parent)? {
            return Err(changed());
        }
        let outer = self.outer.as_ref().ok_or_else(changed)?;
        require_private(outer)?;
        let local::Entry::Directory(current) = local::entry(&self.parent, &self.name)? else {
            return Err(changed());
        };
        if !outer.same_as(&current)? {
            return Err(changed());
        }
        self.require_source()?;
        require_absent(&self.path)
    }

    fn require_source(&self) -> io::Result<()> {
        let outer = self.outer.as_ref().ok_or_else(changed)?;
        require_private(outer)?;
        let local::Entry::Directory(payload) = local::entry(outer, OsStr::new(PAYLOAD))? else {
            return Err(changed());
        };
        if !self.root().same_as(&payload)? {
            return Err(changed());
        }
        Ok(())
    }

    // Only Windows retries transient directory rename failures.
    #[cfg_attr(not(windows), allow(clippy::never_loop))]
    pub(crate) fn publish(
        &mut self,
        cancelled: &dyn Fn() -> bool,
    ) -> std::result::Result<(), PublicationError> {
        #[cfg(windows)]
        let mut attempt = 0;
        loop {
            self.verify().map_err(|error| PublicationError {
                error,
                uncertain: false,
            })?;
            if cancelled() {
                return Err(PublicationError {
                    error: io::ErrorKind::Interrupted.into(),
                    uncertain: false,
                });
            }
            match publish_once(
                &self.parent,
                self.outer.as_ref().expect("owned outer stage"),
                self.root(),
                basename(&self.path).map_err(|error| PublicationError {
                    error,
                    uncertain: false,
                })?,
            ) {
                Ok(()) => {
                    self.published = true;
                    return Ok(());
                }
                Err(error) => {
                    // A failed system call without its original source name is
                    // uncertain. Never report a guaranteed rollback in that case.
                    if self.require_source().is_err() {
                        return Err(PublicationError {
                            error,
                            uncertain: true,
                        });
                    }
                    #[cfg(windows)]
                    if let Some(delay) =
                        local::Directory::publish_directory_retry_delay(&error, attempt)
                    {
                        attempt += 1;
                        std::thread::sleep(delay);
                        continue;
                    }
                    return Err(PublicationError {
                        error,
                        uncertain: false,
                    });
                }
            }
        }
    }

    pub(crate) fn cleanup(&mut self) -> io::Result<()> {
        let owns_unpublished_payload =
            !self.published && self.payload.is_some() && self.require_source().is_ok();
        drop(self.payload.take());
        if let Some(outer) = self.outer.take() {
            if owns_unpublished_payload {
                local::discard(&self.parent, &self.name, outer)
            } else {
                // A published or unverified payload must never cause recursive
                // deletion of names substituted inside the wrapper.
                remove_empty(&self.parent, &self.name, outer)
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn publish_once(
    parent: &local::Directory,
    outer: &local::Directory,
    payload: &local::Directory,
    name: &OsStr,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = payload;
        rustix::fs::renameat_with(
            outer.file(),
            PAYLOAD,
            parent.file(),
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let _ = outer;
        parent.publish_directory_once(payload, name)
    }
}

fn remove_empty(
    parent: &local::Directory,
    name: &OsStr,
    outer: local::Directory,
) -> io::Result<()> {
    require_private(&outer)?;
    let local::Entry::Directory(current) = local::entry(parent, name)? else {
        return Err(changed());
    };
    if !outer.same_as(&current)? {
        return Err(changed());
    }
    #[cfg(unix)]
    {
        rustix::fs::unlinkat(parent.file(), name, rustix::fs::AtFlags::REMOVEDIR)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        // Native directory removal refuses nonempty directories.
        drop(current);
        outer.discard_stage()
    }
}

pub(super) fn require_private(directory: &local::Directory) -> io::Result<()> {
    #[cfg(windows)]
    return directory.require_owner_only();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = directory.file().metadata()?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o700
        {
            return Err(changed());
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_extended_acl(directory.file())?;
        Ok(())
    }
}

fn require_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The local folder destination already exists",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn changed() -> io::Error {
    io::Error::other("The private folder stage or destination parent changed")
}
