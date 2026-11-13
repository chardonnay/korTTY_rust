//! Private runtime installations and their atomically published activation pointer.

use std::borrow::Borrow;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::private_files::{
    Directory, EntryIdentity, FileLock, FileStamp, nonce, require_single_link,
};

use super::super::runtime_index::{Host, Package, PackageBackend, VerifiedIndex};
use super::{
    API_CONTRACT_VERSION, AcceptedIndex, Authorization, COMPATIBILITY_VERSION, DIRECTORY,
    MAX_ARCHIVE_BYTES, MAX_INDEX_BYTES, MAX_SIGNATURE_BYTES, check_cancelled, copy_archive,
    extract, online::Choice, pool, read_bounded,
};

const INSTALLATIONS: &str = "installations";
const POINTER: &str = "activation.json";
const ARCHIVE: &str = "package.zip";
const INDEX: &str = "runtime-index-v1.json";
const SIGNATURE: &str = "runtime-index-v1.sig";
const KEY_PREFIX: &str = "model-key-";
const MAX_INSTALLATIONS: usize = 32;
const MAX_TREE_NODES: usize = 25_000;
const MAX_TREE_DEPTH: usize = 64;

pub(super) struct Receipt<'a> {
    pub(super) index: &'a VerifiedIndex,
    pub(super) bytes: &'a [u8],
    pub(super) signature: &'a str,
    pub(super) automatic: Option<Automatic<'a>>,
    pub(super) online: bool,
    pub(super) pinned: bool,
}

pub(super) struct Automatic<'a> {
    pub(super) prior: &'a Installed,
    pub(super) authorize: &'a Authorization,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstalledRef {
    directory: String,
    package: String,
    runtime_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pinned: bool,
}

impl InstalledRef {
    fn validate(&self) -> io::Result<()> {
        if !installation_name(&self.directory)
            || self.package.is_empty()
            || self.runtime_id.is_empty()
        {
            return Err(io::Error::other("Invalid installed runtime reference"));
        }
        Ok(())
    }

    fn withdrawn(&self, accepted: &AcceptedIndex) -> bool {
        accepted.revoked_ids.contains(&self.package)
            || accepted.revoked_ids.contains(&self.runtime_id)
    }
}

#[derive(Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    active: InstalledRef,
    previous: Option<InstalledRef>,
    pending_first_launch: bool,
}

impl Activation {
    fn read(root: &Directory) -> io::Result<Option<Self>> {
        let file = match root.open_private_file(POINTER) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let state: Self = serde_json::from_slice(&read_bounded(file, 16 * 1024)?)?;
        state.active.validate()?;
        if let Some(previous) = &state.previous {
            previous.validate()?;
            if previous.directory == state.active.directory {
                return Err(io::Error::other("Invalid previous runtime reference"));
            }
        }
        Ok(Some(state))
    }

    fn publish(&self, root: &Directory, check: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        root.replace_with(POINTER, |file| file.write_all(&bytes), check)
    }
}

pub(super) fn activate(
    profile: &Path,
    receipt: Receipt<'_>,
    host: Host,
    mut archive: File,
    expected_backend: Option<PackageBackend>,
    cancelled: &AtomicBool,
    check_version: impl FnOnce(&Path, &Path, &AtomicBool) -> io::Result<()>,
) -> io::Result<String> {
    check_cancelled(cancelled)?;
    let require_downloads = || {
        if receipt.online {
            crate::clipboard_policy::ClipboardPolicy::load().require_ai_runtime_downloads()?;
        }
        Ok(())
    };
    require_downloads()?;
    let profile_directory = Directory::open(profile, true)?;
    let root = profile_directory.private_child(DIRECTORY, true)?;
    let operation = root.open_lock("operation.lock")?;
    operation.try_lock().map_err(io::Error::from)?;
    let _operation = FileLock::new(operation);
    let accepted = if let Some(automatic) = &receipt.automatic {
        let _permission = (automatic.authorize)()?;
        AcceptedIndex::read(&root)?
            .filter(|accepted| accepted.sha256 == receipt.index.sha256())
            .ok_or_else(|| {
                io::Error::other("The signed channel changed. Check for updates again")
            })?
    } else {
        AcceptedIndex::accept(&root, receipt.index, cancelled)?
    };
    pool::trust_changed();
    let usage = root.open_lock("usage.lock")?;
    let usage_error = |error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Stop the running managed model before installing a runtime",
        ),
        std::fs::TryLockError::Error(error) => error,
    };
    // A shared reservation permits resident models while ruling out first-launch
    // rollback during the pointer read and pruning. The operation lock bars new launches.
    usage.try_lock_shared().map_err(usage_error)?;
    let usage = FileLock::new(usage);
    let prior = Activation::read(&root)?;
    if let Some(automatic) = &receipt.automatic {
        if prior != automatic.prior.activation {
            return Err(io::Error::other(
                "The installed runtime changed. Check for updates again",
            ));
        }
        pool::require_idle_for_install(profile)?;
    }
    let installations = root.private_child(INSTALLATIONS, true)?;
    prune(&profile_directory, &installations, prior.as_ref());

    let size = archive.metadata()?.len();
    let stamp = FileStamp::read(&archive)?;
    if size == 0 || size > MAX_ARCHIVE_BYTES {
        return Err(io::Error::other(
            "Runtime archive exceeds its size limit or is empty",
        ));
    }
    let candidates: Vec<_> = receipt
        .index
        .packages()
        .iter()
        .filter(|package| package.size() == size)
        .collect();
    if candidates.is_empty() {
        return Err(io::Error::other(
            "No signed runtime package has this archive size",
        ));
    }

    let mut stage = Stage::new(&installations)?;
    let mut selected = None;
    stage.directory().publish_checked(
        ARCHIVE,
        |file| {
            let digest = copy_archive(&mut archive, file, size, cancelled)?;
            stamp.require_unchanged(&archive)?;
            selected = Some(select_package(
                receipt.index,
                &accepted,
                host,
                candidates,
                &digest,
                expected_backend,
            )?);
            Ok(())
        },
        || check_cancelled(cancelled),
    )?;
    let package =
        selected.ok_or_else(|| io::Error::other("No signed runtime package was selected"))?;
    usage.unlock()?;
    check_cancelled(cancelled)?;
    require_downloads()?;
    if let Some(automatic) = &receipt.automatic {
        (automatic.authorize)()?;
    }
    // A verified, matching package may now displace resident models for activation.
    let _maintenance = pool::prepare_install(profile)?;
    let idle = usage;
    idle.try_lock().map_err(usage_error)?;
    let directory = stage.directory();
    let archive = directory.open_private_file(ARCHIVE)?;
    require_single_link(&archive)?;
    extract::unpack(archive, directory, package, cancelled)?;
    directory.publish_copy(INDEX, receipt.bytes)?;
    directory.publish_copy(SIGNATURE, receipt.signature.as_bytes())?;
    directory.sync()?;
    installations.sync()?;

    let payload = profile
        .join(DIRECTORY)
        .join(INSTALLATIONS)
        .join(&stage.name)
        .join(extract::PAYLOAD);
    let executable = executable_path(&payload, package);
    revalidate_payload(&payload, directory)?;
    check_cancelled(cancelled)?;
    check_version(&executable, &payload, cancelled)?;

    let previous = prior
        .and_then(|state| {
            if state.pending_first_launch {
                state.previous
            } else {
                Some(state.active)
            }
        })
        .filter(|prior| !prior.withdrawn(&accepted));
    let activation = Activation {
        active: InstalledRef {
            directory: stage.name.clone(),
            package: package.installation_id().to_owned(),
            runtime_id: package.runtime_id().to_owned(),
            pinned: receipt.pinned,
        },
        previous,
        pending_first_launch: true,
    };
    {
        let _permission = receipt
            .automatic
            .map(|automatic| (automatic.authorize)())
            .transpose()?;
        activation.publish(&root, || {
            check_cancelled(cancelled)?;
            require_downloads()
        })?;
    }
    stage.keep = true;
    root.sync()?;
    prune(&profile_directory, &installations, Some(&activation));
    Ok(activation.active.package)
}

fn select_package<'a>(
    index: &VerifiedIndex,
    accepted: &AcceptedIndex,
    host: Host,
    candidates: Vec<&'a Package>,
    digest: &str,
    expected_backend: Option<PackageBackend>,
) -> io::Result<&'a Package> {
    let mut matches = candidates
        .into_iter()
        .filter(|package| package.sha256() == digest);
    let package = matches.next().ok_or_else(|| {
        io::Error::other("Archive digest is absent from the signed runtime index")
    })?;
    if matches.next().is_some() {
        return Err(io::Error::other(
            "Archive digest identifies multiple signed runtime packages",
        ));
    }
    if index.is_revoked(package)
        || accepted.revoked_ids.contains(package.runtime_id())
        || accepted.revoked_ids.contains(package.installation_id())
    {
        return Err(io::Error::other(
            "The signed runtime channel has revoked this package",
        ));
    }
    if !index
        .is_compatible(package, host, API_CONTRACT_VERSION, COMPATIBILITY_VERSION)
        .map_err(io::Error::other)?
    {
        return Err(io::Error::other(
            "The signed package is incompatible with this korTTY build or host",
        ));
    }
    if let Some(expected) = expected_backend
        && package.backend() != expected
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "The selected signed runtime package is {}. This repair requires {expected}.",
                package.backend()
            ),
        ));
    }
    Ok(package)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemovalPlan {
    root: EntryIdentity,
    activation: Option<FileStamp>,
    installations: Option<EntryIdentity>,
    packages: Vec<(String, EntryIdentity)>,
}

pub(crate) fn review_removal(profile: &Path) -> io::Result<Option<RemovalPlan>> {
    match Directory::open(profile, false)
        .and_then(|root| root.private_deletion_child(OsStr::new(DIRECTORY)))
    {
        Ok(root) => removal_plan(&root),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn removal_plan(root: &Directory) -> io::Result<Option<RemovalPlan>> {
    let activation = match root.open_private_file(POINTER) {
        Ok(file) => {
            require_single_link(&file)?;
            Some(FileStamp::read(&file)?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut packages = Vec::new();
    let installations = match root.private_deletion_child(OsStr::new(INSTALLATIONS)) {
        Ok(installations) => {
            for name in installations.names(MAX_INSTALLATIONS)? {
                if let Some(text) = name.to_str().filter(|text| installation_name(text)) {
                    let package = installations.private_deletion_child(&name)?;
                    packages.push((text.to_owned(), package.identity()?));
                }
            }
            Some(installations.identity()?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if activation.is_none() && packages.is_empty() {
        return Ok(None);
    }
    packages.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(Some(RemovalPlan {
        root: root.identity()?,
        activation,
        installations,
        packages,
    }))
}

pub(super) fn uninstall(
    profile: &Path,
    plan: &RemovalPlan,
    cancelled: &AtomicBool,
) -> io::Result<Option<String>> {
    check_cancelled(cancelled)?;
    let root = Directory::open(profile, false)?.private_deletion_child(OsStr::new(DIRECTORY))?;
    let operation = root.open_lock("operation.lock")?;
    require_single_link(&operation)?;
    operation.try_lock().map_err(io::Error::from)?;
    let _operation = FileLock::new(operation);
    let require_reviewed = || {
        if removal_plan(&root)?.as_ref() == Some(plan) {
            Ok(())
        } else {
            Err(io::Error::other(
                "The installed runtime changed. Review removal again.",
            ))
        }
    };
    require_reviewed()?;
    let _maintenance = pool::prepare_install(profile)?;
    let usage = root.open_lock("usage.lock")?;
    require_single_link(&usage)?;
    usage.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::other("Finish active managed model requests before removing the runtime")
        }
        std::fs::TryLockError::Error(error) => error,
    })?;
    let _usage = FileLock::new(usage);
    require_reviewed()?;
    check_cancelled(cancelled)?;
    if let Some(activation) = &plan.activation {
        root.remove_unchanged(POINTER, activation)?;
    }
    // After deactivation, finish cleanup without discarding the signed trust floor.
    Ok(root
        .sync()
        .and_then(|()| remove_reviewed_packages(&root, plan))
        .err()
        .map(|error| {
            format!("Runtime deactivated. Some runtime files could not be removed or synchronized ({error}). Review removal again to retry.")
        }))
}

fn remove_reviewed_packages(root: &Directory, plan: &RemovalPlan) -> io::Result<()> {
    if plan.packages.is_empty() {
        return Ok(());
    }
    let installations = root.private_deletion_child(OsStr::new(INSTALLATIONS))?;
    if Some(installations.identity()?) != plan.installations {
        return Err(io::Error::other(
            "The runtime installation directory changed during removal",
        ));
    }
    let mut failure = None;
    for (name, identity) in &plan.packages {
        let remove = || {
            let name = OsStr::new(name);
            let directory = installations.private_deletion_child(name)?;
            if directory.identity()? != *identity {
                return Err(io::Error::other(
                    "An installed package changed during removal",
                ));
            }
            remove_installation(&installations, name, directory)
        };
        if let Err(error) = remove() {
            failure.get_or_insert(error);
        }
    }
    installations.sync()?;
    failure.map_or(Ok(()), Err)
}

pub(super) struct Runtime {
    root: Directory,
    installations: Directory,
    usage: FileLock,
    state: Activation,
    payload: PathBuf,
    executable: PathBuf,
    directory: Directory,
    choice: Choice,
}

impl Runtime {
    pub(super) fn open(profile: &Path, cancelled: &AtomicBool) -> io::Result<Self> {
        let root = Directory::open(profile, false)?.private_child(DIRECTORY, false)?;
        let operation = root.open_lock("operation.lock")?;
        operation.try_lock().map_err(io::Error::from)?;
        let operation = FileLock::new(operation);
        let installations = root.private_child(INSTALLATIONS, false)?;
        let state = Activation::read(&root)?
            .ok_or_else(|| io::Error::other("Install a signed llama.cpp runtime first"))?;
        let usage = root.open_lock("usage.lock")?;
        if state.pending_first_launch {
            usage.try_lock().map_err(io::Error::from)?;
        } else {
            usage.try_lock_shared().map_err(io::Error::from)?;
        }
        let usage = FileLock::new(usage);
        let accepted = AcceptedIndex::read(&root)?
            .ok_or_else(|| io::Error::other("The runtime trust state is missing"))?;
        drop(operation);
        let (directory, payload, executable, choice) =
            match verify_installation(profile, &installations, &state.active, &accepted, cancelled)
            {
                Ok(runtime) => runtime,
                Err(error)
                    if state.pending_first_launch && error.kind() != io::ErrorKind::Interrupted =>
                {
                    let note = recover(&root, &state, cancelled, |previous| {
                        verify_installation(profile, &installations, previous, &accepted, cancelled)
                            .map(|_| ())
                    })?;
                    return Err(io::Error::other(format!("{error}.{note}")));
                }
                Err(error) => return Err(error),
            };
        Ok(Self {
            root,
            installations,
            usage,
            state,
            payload,
            executable,
            directory,
            choice,
        })
    }

    pub(super) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(super) fn payload(&self) -> &Path {
        &self.payload
    }

    pub(super) fn backend(&self) -> PackageBackend {
        self.choice.backend
    }

    pub(super) fn choice(&self) -> &Choice {
        &self.choice
    }

    pub(super) fn key_file(&self, profile: &Path, token: &str) -> io::Result<KeyFile> {
        let path = profile.join(DIRECTORY);
        let directory = Directory::open(&path, false)?;
        if !self.root.same_as(&directory)? {
            return Err(io::Error::other("The managed runtime directory changed"));
        }
        let name = format!("{KEY_PREFIX}{}", nonce()?);
        directory.publish_copy(&name, token.as_bytes())?;
        let file = directory.open_private_file(&name)?;
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        Ok(KeyFile {
            path: path.join(&name),
            name,
            directory,
            stamp,
        })
    }

    pub(super) fn revalidate(&self) -> io::Result<()> {
        let accepted = AcceptedIndex::read(&self.root)?
            .ok_or_else(|| io::Error::other("The runtime trust state is missing"))?;
        if self.state.active.withdrawn(&accepted) {
            return Err(io::Error::other(
                "The signed runtime channel has revoked the installed package",
            ));
        }
        revalidate_payload(&self.payload, &self.directory)
    }

    pub(super) fn ready(&mut self) -> io::Result<()> {
        if self.state.pending_first_launch {
            let operation = self.root.open_lock("operation.lock")?;
            operation.lock()?;
            let _operation = FileLock::new(operation);
            self.revalidate()?;
            self.state.pending_first_launch = false;
            self.state.publish(&self.root, || Ok(()))?;
            self.root.sync()?;
            // Installers and new launches take the operation lock before the usage lock.
            self.usage.unlock()?;
            self.usage.try_lock_shared().map_err(io::Error::from)?;
        }
        Ok(())
    }

    pub(super) fn rollback(
        &self,
        profile: &Path,
        cancelled: &AtomicBool,
    ) -> io::Result<&'static str> {
        let accepted = AcceptedIndex::read(&self.root)?
            .ok_or_else(|| io::Error::other("The runtime trust state is missing"))?;
        recover(&self.root, &self.state, cancelled, |previous| {
            verify_installation(profile, &self.installations, previous, &accepted, cancelled)
                .map(|_| ())
        })
    }
}

fn recover(
    root: &Directory,
    state: &Activation,
    cancelled: &AtomicBool,
    verify_previous: impl FnOnce(&InstalledRef) -> io::Result<()>,
) -> io::Result<&'static str> {
    if !state.pending_first_launch {
        return Ok("");
    }
    check_cancelled(cancelled)?;
    if let Some(previous) = &state.previous {
        match verify_previous(previous) {
            Ok(()) => {
                Activation {
                    active: previous.clone(),
                    previous: None,
                    pending_first_launch: false,
                }
                .publish(root, || check_cancelled(cancelled))?;
                root.sync()?;
                return Ok(" The previous verified runtime was restored.");
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(_) => {}
        }
    }
    check_cancelled(cancelled)?;
    root.remove_file(POINTER)?;
    root.sync()?;
    Ok(" The failed runtime was deactivated.")
}

pub(super) struct KeyFile {
    pub(super) path: PathBuf,
    name: String,
    directory: Directory,
    stamp: FileStamp,
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = self.directory.remove_unchanged(&self.name, &self.stamp);
    }
}

fn verify_installation(
    profile: &Path,
    installations: &Directory,
    installed: &InstalledRef,
    accepted: &AcceptedIndex,
    cancelled: &AtomicBool,
) -> io::Result<(Directory, PathBuf, PathBuf, Choice)> {
    check_cancelled(cancelled)?;
    let directory = installations.private_child(&installed.directory, false)?;
    let package = verify_package(&directory, installed, accepted)?;
    let archive = directory.open_private_file(ARCHIVE)?;
    require_single_link(&archive)?;
    extract::verify(archive, &directory, &package, cancelled)?;
    let payload = profile
        .join(DIRECTORY)
        .join(INSTALLATIONS)
        .join(&installed.directory)
        .join(extract::PAYLOAD);
    let executable = executable_path(&payload, &package);
    revalidate_payload(&payload, &directory)?;
    Ok((
        directory,
        payload,
        executable,
        Choice::from_package(&package),
    ))
}

pub(super) struct Installed {
    activation: Option<Activation>,
    package: Option<Package>,
    revoked: bool,
}

impl Installed {
    pub(super) fn policy_status(&self) -> Option<super::online::policy::Installed<'_>> {
        let activation = self.activation.as_ref()?;
        Some(super::online::policy::Installed {
            id: &activation.active.package,
            backend: self.package.as_ref()?.backend(),
            revoked: self.revoked,
            pending_first_launch: activation.pending_first_launch,
        })
    }

    pub(super) fn pinned_runtime(&self) -> Option<&str> {
        let active = &self.activation.as_ref()?.active;
        active.pinned.then_some(active.runtime_id.as_str())
    }

    pub(super) fn clear_unavailable_pin(
        &mut self,
        profile: &Path,
        catalog: &VerifiedIndex,
        authorize: &Authorization,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let root = Directory::open(profile, false)?.private_child(DIRECTORY, false)?;
        let operation = root.open_lock("operation.lock")?;
        require_single_link(&operation)?;
        operation.try_lock().map_err(io::Error::from)?;
        let _operation = FileLock::new(operation);
        let usage = root.open_lock("usage.lock")?;
        require_single_link(&usage)?;
        usage.try_lock_shared().map_err(io::Error::from)?;
        let _usage = FileLock::new(usage);
        let mut current = Activation::read(&root)?;
        if current != self.activation
            || AcceptedIndex::read(&root)?
                .is_none_or(|accepted| accepted.sha256 != catalog.sha256())
        {
            return Err(io::Error::other(
                "The installed runtime or signed channel changed. Check again",
            ));
        }
        if let Some(current) = &mut current {
            let _permission = authorize()?;
            current.active.pinned = false;
            current.publish(&root, || check_cancelled(cancelled))?;
            root.sync()?;
        }
        self.activation = current;
        Ok(())
    }
}

pub(super) fn installed(profile: &Path, include_revoked: bool) -> io::Result<Installed> {
    let root = match Directory::open(profile, false)
        .and_then(|root| root.private_child(DIRECTORY, false))
    {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(empty_installation()),
        Err(error) => return Err(error),
    };
    installed_in(&root, include_revoked)
}

fn empty_installation() -> Installed {
    Installed {
        activation: None,
        package: None,
        revoked: false,
    }
}

fn installed_in(root: &Directory, include_revoked: bool) -> io::Result<Installed> {
    let Some(state) = Activation::read(root)? else {
        return Ok(empty_installation());
    };
    let accepted = AcceptedIndex::read(root)?
        .ok_or_else(|| io::Error::other("The runtime trust state is missing"))?;
    let revoked = state.active.withdrawn(&accepted);
    let directory = root
        .private_child(INSTALLATIONS, false)?
        .private_child(&state.active.directory, false)?;
    let package = if include_revoked {
        verify_receipt(&directory, &state.active, &accepted)?
    } else {
        verify_package(&directory, &state.active, &accepted)?
    };
    Ok(Installed {
        activation: Some(state),
        package: Some(package),
        revoked,
    })
}

pub(crate) fn active_package_backend(profile: &Path) -> io::Result<Option<PackageBackend>> {
    let installed = installed(profile, false)?;
    Ok(installed.package.map(|package| package.backend()))
}

pub(crate) fn active_package(profile: &Path) -> io::Result<Option<Choice>> {
    let installed = installed(profile, true)?;
    Ok(installed
        .package
        .as_ref()
        .filter(|_| !installed.revoked)
        .map(Choice::from_package))
}

#[derive(Debug)]
pub(crate) struct ActivePackageLock {
    _usage: FileLock,
    _operation: FileLock,
}

pub(crate) fn lock_active_package(
    profile: &Path,
    expected: &Choice,
) -> io::Result<ActivePackageLock> {
    let root = Directory::open(profile, false)?.private_child(DIRECTORY, false)?;
    // Follow the install lock order. Usage also excludes first-launch rollback.
    let lock = |name| {
        let file = root.open_lock(name)?;
        require_single_link(&file)?;
        file.try_lock_shared().map_err(io::Error::from)?;
        Ok::<_, io::Error>(FileLock::new(file))
    };
    let operation = lock("operation.lock")?;
    let usage = lock("usage.lock")?;
    let installed = installed_in(&root, false)?;
    if installed
        .package
        .as_ref()
        .map(Choice::from_package)
        .as_ref()
        != Some(expected)
    {
        return Err(io::Error::other(
            "The tested runtime changed. Review and test the models again.",
        ));
    }
    Ok(ActivePackageLock {
        _usage: usage,
        _operation: operation,
    })
}

fn verify_package(
    directory: &Directory,
    installed: &InstalledRef,
    accepted: &AcceptedIndex,
) -> io::Result<Package> {
    if installed.withdrawn(accepted) {
        return Err(revoked_error());
    }
    verify_receipt(directory, installed, accepted)
}

fn revoked_error() -> io::Error {
    io::Error::other("The signed runtime channel has revoked the installed package")
}

fn verify_receipt(
    directory: &Directory,
    installed: &InstalledRef,
    accepted: &AcceptedIndex,
) -> io::Result<Package> {
    let index = read_receipt(directory, INDEX, MAX_INDEX_BYTES)?;
    let signature = read_receipt(directory, SIGNATURE, MAX_SIGNATURE_BYTES)?;
    let signature = std::str::from_utf8(&signature).map_err(io::Error::other)?;
    let index = VerifiedIndex::verify(&index, signature).map_err(io::Error::other)?;
    let latest: jiff::Timestamp = accepted.generated_at.parse().map_err(io::Error::other)?;
    if index.generated_at() > latest
        || (index.generated_at() == latest && index.sha256() != accepted.sha256)
    {
        return Err(io::Error::other(
            "The installed runtime receipt conflicts with the accepted index",
        ));
    }
    let package = index
        .packages()
        .iter()
        .find(|package| {
            package.installation_id() == installed.package
                && package.runtime_id() == installed.runtime_id
        })
        .ok_or_else(|| io::Error::other("The installed package is absent from its signed index"))?;
    let host = Host::current().map_err(io::Error::other)?;
    if !index
        .is_compatible(package, host, API_CONTRACT_VERSION, COMPATIBILITY_VERSION)
        .map_err(io::Error::other)?
    {
        return Err(io::Error::other(
            "The installed runtime is incompatible with this korTTY build or host",
        ));
    }
    Ok(package.clone())
}

fn read_receipt(directory: &Directory, name: &str, limit: usize) -> io::Result<Vec<u8>> {
    let file = directory.open_private_file(name)?;
    require_single_link(&file)?;
    read_bounded(file, limit as u64)
}

fn revalidate_payload(path: &Path, installation: &Directory) -> io::Result<()> {
    let expected = installation.private_child(extract::PAYLOAD, false)?;
    if !expected.same_as(&Directory::open(path, false)?)? {
        return Err(io::Error::other("The runtime payload directory changed"));
    }
    Ok(())
}

fn executable_path(payload: &Path, package: &Package) -> PathBuf {
    package
        .entrypoint()
        .split('/')
        .fold(payload.to_path_buf(), |path, part| path.join(part))
}

fn installation_name(name: &str) -> bool {
    name.strip_prefix("package-").is_some_and(|nonce| {
        nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub(super) struct Stage<P: Borrow<Directory>> {
    parent: P,
    name: String,
    directory: Option<Directory>,
    keep: bool,
}

impl<P: Borrow<Directory>> Stage<P> {
    pub(super) fn new(parent: P) -> io::Result<Self> {
        if parent.borrow().names(MAX_INSTALLATIONS)?.len() >= MAX_INSTALLATIONS {
            return Err(io::Error::other(
                "The runtime installation directory is full",
            ));
        }
        let name = format!("package-{}", nonce()?);
        let directory = parent
            .borrow()
            .create_private_subdirectory(OsStr::new(&name))?;
        Ok(Self {
            parent,
            name,
            directory: Some(directory),
            keep: false,
        })
    }

    pub(super) fn directory(&self) -> &Directory {
        self.directory.as_ref().expect("active installation stage")
    }
}

impl<P: Borrow<Directory>> Drop for Stage<P> {
    fn drop(&mut self) {
        if !self.keep
            && let Some(directory) = self.directory.take()
        {
            let _ = remove_installation(self.parent.borrow(), OsStr::new(&self.name), directory);
        }
    }
}

fn prune(profile: &Directory, installations: &Directory, active: Option<&Activation>) {
    let Ok(guarded) = profile
        .private_deletion_child(OsStr::new(DIRECTORY))
        .and_then(|root| root.private_deletion_child(OsStr::new(INSTALLATIONS)))
    else {
        return;
    };
    if !guarded.same_as(installations).is_ok_and(|same| same) {
        return;
    }
    let installations = &guarded;
    let Ok(names) = installations.names(MAX_INSTALLATIONS) else {
        return;
    };
    for name in names {
        let Some(text) = name.to_str().filter(|name| installation_name(name)) else {
            continue;
        };
        if active.is_some_and(|state| {
            state.active.directory == text
                || state
                    .previous
                    .as_ref()
                    .is_some_and(|previous| previous.directory == text)
        }) {
            continue;
        }
        if let Ok(directory) = installations.private_deletion_child(&name) {
            let _ = remove_installation(installations, &name, directory);
        }
    }
}

fn remove_installation(parent: &Directory, name: &OsStr, directory: Directory) -> io::Result<()> {
    remove_installation_tree(parent, name, directory, MAX_TREE_NODES)
}

pub(in crate::local_ai) fn remove_installation_tree(
    parent: &Directory,
    name: &OsStr,
    directory: Directory,
    mut remaining: usize,
) -> io::Result<()> {
    let identity = directory.identity()?;
    clear_tree(&directory, 0, &mut remaining)?;
    drop(directory);
    parent.remove_deletion_entry(name, identity, true)
}

fn clear_tree(directory: &Directory, depth: usize, remaining: &mut usize) -> io::Result<()> {
    if depth > MAX_TREE_DEPTH {
        return Err(io::Error::other(
            "The runtime installation exceeds its directory depth limit",
        ));
    }
    for name in directory.names(*remaining)? {
        *remaining = remaining.checked_sub(1).ok_or_else(|| {
            io::Error::other("The runtime installation exceeds its file count limit")
        })?;
        let (identity, is_directory) = directory.deletion_entry(&name)?;
        if is_directory {
            let child = directory.deletion_child(&name)?;
            if child.identity()? != identity {
                return Err(io::Error::other("Runtime directory changed during cleanup"));
            }
            clear_tree(&child, depth + 1, remaining)?;
        }
        directory.remove_deletion_entry(&name, identity, is_directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};
    use pgp::crypto::hash::HashAlgorithm;
    use serde_json::json;

    struct TestProfile(PathBuf);

    impl TestProfile {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("kortty-runtime-{}", nonce().unwrap()));
            std::fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            let path = path.canonicalize().unwrap();
            Self(path)
        }
    }

    impl Drop for TestProfile {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    struct Signed {
        index: VerifiedIndex,
        bytes: Vec<u8>,
        signature: String,
    }

    impl Signed {
        fn new(time: &str, archive: &[u8], runtime: &str, revoked: &[&str]) -> Self {
            Self::for_backend(time, archive, runtime, revoked, "cpu")
        }

        fn for_backend(
            time: &str,
            archive: &[u8],
            runtime: &str,
            revoked: &[&str],
            backend: &str,
        ) -> Self {
            let hash: String = HashAlgorithm::Sha256
                .digest(archive)
                .unwrap()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let bytes = serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "generatedAt": time,
                "revokedRuntimeIds": revoked,
                "packages": [{
                    "runtimeId": runtime,
                    "llamaTag": "b12040",
                    "commit": "00ef136649bd9955bfe11fb9c5a6028aee1a1185",
                    "apiContractVersion": 1,
                    "minimumKorttyVersion": "2.26.0",
                    "platform": "linux",
                    "architecture": "x86_64",
                    "backend": backend,
                    "size": archive.len(),
                    "sha256": hash,
                    "downloadUrl": "https://downloads.example.test/package.zip",
                    "entrypoint": "bin/llama-server"
                }]
            }))
            .unwrap();
            let key = SigningKey::from_bytes(&[0x29; 32]);
            let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
            let index =
                VerifiedIndex::verify_with_key(&bytes, &signature, &key.verifying_key()).unwrap();
            Self {
                index,
                bytes,
                signature,
            }
        }

        fn receipt(&self) -> Receipt<'_> {
            Receipt {
                index: &self.index,
                bytes: &self.bytes,
                signature: &self.signature,
                automatic: None,
                online: false,
                pinned: false,
            }
        }
    }

    #[test]
    fn reviewed_runtime_publication_respects_install_and_first_launch_locks() {
        let profile = TestProfile::new();
        let root = Directory::open(&profile.0, false)
            .unwrap()
            .private_child(DIRECTORY, true)
            .unwrap();
        let signed = Signed::new(
            "2026-08-01T00:00:00Z",
            b"package",
            "llama-b12040-kortty2",
            &[],
        );
        let choice = Choice::from_package(&signed.index.packages()[0]);
        for name in ["operation.lock", "usage.lock"] {
            let exclusive = root.open_lock(name).unwrap();
            exclusive.try_lock().unwrap();
            assert_eq!(
                lock_active_package(&profile.0, &choice).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            exclusive.unlock().unwrap();
        }
        assert!(
            lock_active_package(&profile.0, &choice)
                .unwrap_err()
                .to_string()
                .contains("tested runtime changed")
        );
    }

    #[test]
    fn runtime_pin_is_backward_compatible_and_clears_only_the_current_verified_selection() {
        let profile = TestProfile::new();
        let root = Directory::open(&profile.0, false)
            .unwrap()
            .private_child(DIRECTORY, true)
            .unwrap();
        let mut active: InstalledRef = serde_json::from_value(json!({
            "directory": format!("package-{}", "a".repeat(32)),
            "package": "llama-b12040-kortty1-linux-x86_64-cpu",
            "runtime_id": "llama-b12040-kortty1"
        }))
        .unwrap();
        assert!(!active.pinned);
        active.pinned = true;
        let state = Activation {
            active,
            previous: None,
            pending_first_launch: false,
        };
        let first = Signed::new(
            "2026-01-01T00:00:00Z",
            b"first",
            "llama-b12040-kortty1",
            &[],
        );
        let next = Signed::new("2026-01-02T00:00:00Z", b"next", "llama-b12041-kortty1", &[]);
        let cancelled = AtomicBool::new(false);
        AcceptedIndex::accept(&root, &next.index, &cancelled).unwrap();
        state.publish(&root, || Ok(())).unwrap();
        let mut installed = Installed {
            activation: Activation::read(&root).unwrap(),
            package: None,
            revoked: false,
        };
        let path = profile.0.clone();
        let authorize = move || Directory::open(&path, false)?.open_lock("fixture-permission.lock");
        assert!(
            installed
                .clear_unavailable_pin(&profile.0, &first.index, &authorize, &cancelled)
                .is_err()
        );
        assert_eq!(installed.pinned_runtime(), Some("llama-b12040-kortty1"));
        installed
            .clear_unavailable_pin(&profile.0, &next.index, &authorize, &cancelled)
            .unwrap();
        assert!(installed.pinned_runtime().is_none());
        assert!(!Activation::read(&root).unwrap().unwrap().active.pinned);
    }

    #[test]
    fn guided_backend_install_rejects_wrong_signed_archive_before_probe_and_preserves_pointer() {
        let profile = TestProfile::new();
        let input = profile.0.join("source.zip");
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("bin/llama-server", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"fixture executable").unwrap();
        let archive = writer.finish().unwrap().into_inner();
        std::fs::write(&input, &archive).unwrap();
        let cpu = Signed::new(
            "2026-08-01T00:00:00Z",
            &archive,
            "llama-b12040-kortty2",
            &[],
        );
        let vulkan = Signed::for_backend(
            "2026-08-02T00:00:00Z",
            &archive,
            "llama-b12040-kortty3",
            &[],
            "vulkan",
        );
        let host = Host::new("linux", "x86_64").unwrap();
        let cancelled = AtomicBool::new(false);
        let install =
            |receipt, expected, probe: fn(&Path, &Path, &AtomicBool) -> io::Result<()>| {
                activate(
                    &profile.0,
                    receipt,
                    host,
                    File::open(&input).unwrap(),
                    expected,
                    &cancelled,
                    probe,
                )
            };
        let first = install(cpu.receipt(), None, |_, _, _| Ok(())).unwrap();
        let runtime = Directory::open(&profile.0, false)
            .unwrap()
            .private_child(DIRECTORY, false)
            .unwrap();
        let mut ready = Activation::read(&runtime).unwrap().unwrap();
        ready.pending_first_launch = false;
        ready.publish(&runtime, || Ok(())).unwrap();
        let pointer = profile.0.join(DIRECTORY).join(POINTER);
        let baseline = std::fs::read(&pointer).unwrap();

        let resident = runtime.open_lock("usage.lock").unwrap();
        resident.try_lock_shared().unwrap();
        let error = install(cpu.receipt(), Some(PackageBackend::Vulkan), |_, _, _| {
            panic!("a mismatched package must not reach the version probe")
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("This repair requires Vulkan"));
        assert_eq!(std::fs::read(&pointer).unwrap(), baseline);
        assert_eq!(
            std::fs::read_dir(profile.0.join(DIRECTORY).join(INSTALLATIONS))
                .unwrap()
                .count(),
            1
        );

        let busy = install(vulkan.receipt(), Some(PackageBackend::Vulkan), |_, _, _| {
            panic!("a package must not reach the version probe while another runtime is in use")
        })
        .unwrap_err();
        assert!(busy.to_string().contains("Stop the running managed model"));
        assert_eq!(std::fs::read(&pointer).unwrap(), baseline);
        assert_eq!(
            std::fs::read_dir(profile.0.join(DIRECTORY).join(INSTALLATIONS))
                .unwrap()
                .count(),
            1
        );
        drop(resident);

        let next = install(
            vulkan.receipt(),
            Some(PackageBackend::Vulkan),
            |entry, _, _| {
                assert_eq!(std::fs::read(entry).unwrap(), b"fixture executable");
                Ok(())
            },
        )
        .unwrap();
        let state = Activation::read(&runtime).unwrap().unwrap();
        assert!(state.pending_first_launch);
        assert!(next.ends_with("-vulkan"));
        assert_eq!(state.active.package, next);
        assert_eq!(state.previous.unwrap().package, first);
    }

    #[test]
    fn signed_install_keeps_the_previous_pointer_after_health_failure_and_retains_revocations() {
        let profile = TestProfile::new();
        let input = profile.0.join("source.zip");
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("bin/llama-server", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"fixture executable").unwrap();
        let archive = writer.finish().unwrap().into_inner();
        std::fs::write(&input, &archive).unwrap();

        let version = "llama-b12040-kortty2";
        let first_time = "2026-08-01T00:00:00Z";
        let first = Signed::new(first_time, &archive, version, &[]);
        let host = Host::new("linux", "x86_64").unwrap();
        let cancelled = AtomicBool::new(false);
        let activate_index =
            |receipt, host, check: fn(&Path, &Path, &AtomicBool) -> io::Result<()>| {
                activate(
                    &profile.0,
                    receipt,
                    host,
                    File::open(&input).unwrap(),
                    None,
                    &cancelled,
                    check,
                )
            };
        let activate_healthy = |receipt, host| {
            activate_index(receipt, host, |entry, payload, _| {
                assert!(entry.starts_with(payload));
                assert_eq!(std::fs::read(entry).unwrap(), b"fixture executable");
                Ok(())
            })
        };
        let root = profile.0.join(DIRECTORY);
        let pointer = root.join(POINTER);
        let published = activate_healthy(first.receipt(), host).unwrap();
        assert_eq!(published, "llama-b12040-kortty2-linux-x86_64-cpu");
        let state_bytes = std::fs::read(&pointer).unwrap();
        let state: Activation = serde_json::from_slice(&state_bytes).unwrap();
        let installed = root.join(INSTALLATIONS).join(&state.active.directory);
        assert_eq!(std::fs::read(installed.join(ARCHIVE)).unwrap(), archive);
        assert_eq!(std::fs::read(installed.join(INDEX)).unwrap(), first.bytes);
        assert_eq!(
            std::fs::read(installed.join(SIGNATURE)).unwrap(),
            first.signature.as_bytes()
        );

        assert!(
            activate_healthy(first.receipt(), Host::new("linux", "aarch64").unwrap())
                .unwrap_err()
                .to_string()
                .contains("incompatible")
        );
        let mut tampered = archive.clone();
        tampered[0] ^= 1;
        std::fs::write(&input, tampered).unwrap();
        assert!(
            activate_healthy(first.receipt(), host)
                .unwrap_err()
                .to_string()
                .contains("digest")
        );
        std::fs::write(&input, &archive).unwrap();

        let second = Signed::new(
            "2026-08-02T00:00:00Z",
            &archive,
            "llama-b12040-kortty3",
            &[],
        );
        assert!(
            activate_index(second.receipt(), host, |_, _, _| Err(io::Error::other(
                "version fixture failed"
            )))
            .unwrap_err()
            .to_string()
            .contains("version fixture failed")
        );
        assert_eq!(std::fs::read(&pointer).unwrap(), state_bytes);
        assert_eq!(
            std::fs::read_dir(root.join(INSTALLATIONS)).unwrap().count(),
            1
        );
        assert!(
            activate_healthy(first.receipt(), host)
                .unwrap_err()
                .to_string()
                .contains("replay")
        );

        let revoked = [version];
        let collision = Signed::new("2026-08-02T00:00:00Z", &archive, version, &revoked);
        assert!(
            activate_healthy(collision.receipt(), host)
                .unwrap_err()
                .to_string()
                .contains("collision")
        );
        let withdrawal = Signed::new("2026-08-03T00:00:00Z", &archive, version, &revoked);
        assert!(
            activate_healthy(withdrawal.receipt(), host)
                .unwrap_err()
                .to_string()
                .contains("revoked")
        );
        let omitted = Signed::new("2026-08-04T00:00:00Z", &archive, version, &[]);
        assert!(
            activate_healthy(omitted.receipt(), host)
                .unwrap_err()
                .to_string()
                .contains("revoked")
        );
        assert_eq!(std::fs::read(pointer).unwrap(), state_bytes);
    }

    #[test]
    fn runtime_removal_preserves_trust_and_models_and_refuses_busy_or_stale_review() {
        let profile = TestProfile::new();
        assert!(review_removal(&profile.0).unwrap().is_none());
        let input = profile.0.join("source.zip");
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("bin/llama-server", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"fixture executable").unwrap();
        let archive = writer.finish().unwrap().into_inner();
        std::fs::write(&input, &archive).unwrap();
        let first = Signed::new(
            "2026-08-01T00:00:00Z",
            &archive,
            "llama-b12040-kortty2",
            &[],
        );
        let second = Signed::new(
            "2026-08-02T00:00:00Z",
            &archive,
            "llama-b12040-kortty3",
            &["llama-b12040-kortty1"],
        );
        let cancelled = AtomicBool::new(false);
        let install = |signed: &Signed| {
            activate(
                &profile.0,
                signed.receipt(),
                Host::new("linux", "x86_64").unwrap(),
                File::open(&input).unwrap(),
                None,
                &cancelled,
                |_, _, _| Ok(()),
            )
        };
        install(&first).unwrap();
        let directory = Directory::open(&profile.0, false).unwrap();
        let root = directory.private_child(DIRECTORY, false).unwrap();
        let models = directory.private_child("llama-models", true).unwrap();
        models
            .publish_copy("fixture.gguf", b"retained model")
            .unwrap();
        let mut ready = Activation::read(&root).unwrap().unwrap();
        ready.pending_first_launch = false;
        ready.publish(&root, || Ok(())).unwrap();
        let old = review_removal(&profile.0).unwrap().unwrap();

        install(&second).unwrap();
        let trust = profile.0.join(DIRECTORY).join(super::super::INDEX_STATE);
        let accepted = std::fs::read(&trust).unwrap();
        let plan = review_removal(&profile.0).unwrap().unwrap();
        assert_eq!(plan.packages.len(), 2);
        assert!(
            uninstall(&profile.0, &old, &cancelled)
                .unwrap_err()
                .to_string()
                .contains("Review removal again")
        );
        let resident = root.open_lock("usage.lock").unwrap();
        resident.try_lock_shared().unwrap();
        assert!(
            uninstall(&profile.0, &plan, &cancelled)
                .unwrap_err()
                .to_string()
                .contains("Finish active managed model requests")
        );
        assert_eq!(review_removal(&profile.0).unwrap(), Some(plan.clone()));
        drop(resident);
        let cancelled_early = AtomicBool::new(true);
        assert_eq!(
            uninstall(&profile.0, &plan, &cancelled_early)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(review_removal(&profile.0).unwrap(), Some(plan.clone()));

        assert_eq!(uninstall(&profile.0, &plan, &cancelled).unwrap(), None);
        assert!(review_removal(&profile.0).unwrap().is_none());
        assert!(Activation::read(&root).unwrap().is_none());
        let installations = root.private_child(INSTALLATIONS, false).unwrap();
        assert!(installations.names(MAX_INSTALLATIONS).unwrap().is_empty());
        assert_eq!(std::fs::read(&trust).unwrap(), accepted);
        assert_eq!(
            std::fs::read(profile.0.join("llama-models").join("fixture.gguf")).unwrap(),
            b"retained model"
        );
        assert!(
            install(&first).unwrap_err().to_string().contains("replay"),
            "runtime removal must retain the signed release floor"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_removal_directory_guard_refuses_to_cross_a_mount() {
        let root = Directory::open(Path::new("/"), false).unwrap();
        let Ok(procfs) = Directory::open(Path::new("/proc"), false) else {
            return;
        };
        if root.identity().unwrap().0 == procfs.identity().unwrap().0 {
            return;
        }
        let error = root
            .private_deletion_child(OsStr::new("proc"))
            .err()
            .expect("the deletion guard must reject the mounted proc directory");
        assert_eq!(
            error.raw_os_error(),
            io::Error::from(rustix::io::Errno::XDEV).raw_os_error()
        );
    }

    #[test]
    fn runtime_removal_requires_the_same_installations_directory() {
        let profile = TestProfile::new();
        let root = Directory::open(&profile.0, false)
            .unwrap()
            .private_child(DIRECTORY, true)
            .unwrap();
        root.publish_copy(POINTER, b"reviewed pointer").unwrap();
        root.private_child(INSTALLATIONS, true).unwrap();
        let plan = review_removal(&profile.0).unwrap().unwrap();
        let path = profile.0.join(DIRECTORY);
        std::fs::rename(path.join(INSTALLATIONS), path.join("old-installations")).unwrap();
        root.private_child(INSTALLATIONS, true).unwrap();

        let error = uninstall(&profile.0, &plan, &AtomicBool::new(false)).unwrap_err();
        assert!(error.to_string().contains("Review removal again"));
        assert_eq!(
            std::fs::read(path.join(POINTER)).unwrap(),
            b"reviewed pointer"
        );
    }

    struct StopModelOnDrop<'a>(&'a Path, &'a str);

    impl Drop for StopModelOnDrop<'_> {
        fn drop(&mut self) {
            let _ = super::super::stop_model(self.0, self.1);
        }
    }

    struct StopProfileModelsOnDrop<'a>(&'a Path);

    impl Drop for StopProfileModelsOnDrop<'_> {
        fn drop(&mut self) {
            use std::time::{Duration, Instant};

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match pool::prepare_install(self.0) {
                    Ok(_) => break,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => {
                        if !std::thread::panicking() {
                            panic!("Could not stop the private policy model sidecar ({error})");
                        }
                        break;
                    }
                }
            }
        }
    }

    fn smoke_inputs(expected_backend: Option<PackageBackend>) -> super::super::Import {
        let asset = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
        super::super::Import {
            index: asset("KORTTY_SMOKE_RUNTIME_INDEX"),
            signature: asset("KORTTY_SMOKE_RUNTIME_SIGNATURE"),
            archive: asset("KORTTY_SMOKE_RUNTIME_ZIP"),
            expected_backend,
        }
    }

    fn smoke_profile(profile: &Path, id: &str) -> crate::local_ai::Profile {
        use crate::local_ai::{Profile, ProfileOptions};

        let mut route =
            Profile::managed(profile.to_owned(), id, ProfileOptions::default()).unwrap();
        route.options.max_output_tokens = Some(24);
        route
    }

    fn smoke_completion(
        session: &crate::local_ai::Session,
    ) -> Result<String, crate::local_ai::Error> {
        use std::time::{Duration, Instant};

        use crate::local_ai::RequestContent;

        session.client().complete_request(
            "Reply briefly.",
            RequestContent::Text("Reply OK."),
            false,
            Some(Instant::now() + Duration::from_secs(60)),
            false,
        )
    }

    #[test]
    #[ignore = "requires a private executable with a matching installation policy, a trusted HTTPS GGUF, and a signed CPU runtime for this target"]
    fn stable_channel_remote_policy_model_round_trip() {
        use crate::clipboard_policy::{ClipboardPolicy, PolicyModelRuntime};
        use crate::local_ai::{Session, managed_models::Registry, policy_models};

        const ID: &str = "policy-remote-inference-fixture";

        let source = std::env::var("KORTTY_SMOKE_POLICY_HTTPS").expect("configured HTTPS fixture");
        assert!(source.starts_with("https://"));
        let digest = std::env::var("KORTTY_SMOKE_POLICY_SHA256").expect("fixture SHA-256");
        let policy = ClipboardPolicy::load();
        assert!(policy.problem.is_none() && policy.ai_allowed);
        assert!(!policy.ai_user_model_files_allowed());
        let definition = policy
            .ai_runtime_models()
            .iter()
            .find(|definition| definition.id == ID)
            .expect("the runner's adjacent installation policy must declare the fixture");
        assert_eq!(definition.runtime, PolicyModelRuntime::Gguf);
        assert_eq!(definition.source, source);
        let declared: String = definition
            .digest
            .expect("policy fixture SHA-256 pin")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(declared.eq_ignore_ascii_case(&digest));

        // The runner owns this profile and removes it after the test executable
        // releases its process-local policy cache leases.
        let profile = PathBuf::from(
            std::env::var_os("KORTTY_SMOKE_POLICY_PROFILE").expect("runner's private profile"),
        );
        assert!(profile.is_absolute());
        let profile = profile.canonicalize().expect("existing private profile");
        assert!(profile.read_dir().unwrap().next().is_none());
        let cancelled = AtomicBool::new(false);
        let inputs = smoke_inputs(Some(PackageBackend::Cpu));
        let installed = super::super::import(&profile, &inputs, &cancelled).unwrap();
        println!("Activated release-signed CPU runtime {installed}");
        assert!(policy_models::available(definition, &profile, &cancelled));
        assert!(Registry::new(profile.clone()).list().unwrap().is_empty());

        {
            let _running = StopProfileModelsOnDrop(&profile);
            let route = smoke_profile(&profile, ID);
            route.require_current_policy_model().unwrap();
            let (session, _cancel) = Session::begin(route).unwrap();
            assert!(!smoke_completion(&session).unwrap().trim().is_empty());
            let owner = session
                .client()
                .transport(None)
                .unwrap()
                .lease
                .as_ref()
                .unwrap()
                .sidecar();
            assert!(kortty_pty::owns_loopback_listener(owner.pid(), owner.address()).unwrap());
        }
        println!(
            "The remote policy alias returned an authenticated completion and stopped its owned sidecar"
        );
    }

    #[test]
    #[ignore = "requires the published signed runtime assets for this target and an optional GGUF fixture"]
    fn stable_channel_runtime_round_trip() {
        let profile = TestProfile::new();
        let inputs = smoke_inputs(None);
        let cancelled = AtomicBool::new(false);
        let installed = super::super::import(&profile.0, &inputs, &cancelled).unwrap();
        println!("Activated release-signed runtime {installed}");
        let selected = active_package(&profile.0).unwrap().unwrap();
        assert_eq!(selected.id, installed);
        assert_eq!(
            active_package_backend(&profile.0).unwrap(),
            Some(selected.backend)
        );
        let guard = lock_active_package(&profile.0, &selected).unwrap();
        let runtime_root = Directory::open(&profile.0, false)
            .unwrap()
            .private_child(DIRECTORY, false)
            .unwrap();
        for name in ["operation.lock", "usage.lock"] {
            let lock = runtime_root.open_lock(name).unwrap();
            assert!(matches!(
                lock.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
        }
        drop(guard);
        let runtime = Runtime::open(&profile.0, &cancelled).unwrap();
        runtime.revalidate().unwrap();
        drop(runtime);
        let Some(path) = std::env::var_os("KORTTY_SMOKE_GGUF") else {
            return;
        };

        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use super::super::{self as package, ModelStatus, process::Sidecar};
        use crate::local_ai::{
            Error, Session,
            managed_models::{ModelBackend, Purpose},
        };

        fn sidecar(session: &Session) -> &Arc<Sidecar> {
            session
                .client()
                .transport(None)
                .unwrap()
                .lease
                .as_ref()
                .unwrap()
                .sidecar()
        }

        let registry = super::super::super::managed_models::Registry::new(profile.0.clone());
        let model = registry
            .import(Path::new(&path), Purpose::Chat, &cancelled)
            .unwrap();
        let route = |id: &str| smoke_profile(&profile.0, id);
        let request = smoke_completion;
        let reply = |session: &Session| assert!(!request(session).unwrap().trim().is_empty());
        let _running = StopModelOnDrop(&profile.0, &model.id);
        let status = |id: &str| package::model_status(&profile.0, id);
        let verify_sleep = std::env::var_os("KORTTY_SMOKE_GGUF_SLEEP").is_some();
        let verify_cpu_options = std::env::var("KORTTY_SMOKE_CPU_OPTIONS").as_deref() == Ok("1");
        if verify_cpu_options {
            assert_eq!(
                active_package_backend(&profile.0).unwrap(),
                Some(PackageBackend::Cpu)
            );
            let pointer = profile.0.join(DIRECTORY).join(POINTER);
            let pending = std::fs::read(&pointer).unwrap();
            let mut gpu = model.clone();
            gpu.backend = if cfg!(target_os = "macos") {
                ModelBackend::Metal
            } else {
                ModelBackend::Vulkan
            };
            registry.configure(gpu).unwrap();
            let reason = "The active signed runtime package is CPU";
            assert!(
                package::start_model(&profile.0, &model.id, &cancelled)
                    .unwrap_err()
                    .contains(reason)
            );
            assert!(
                package::process::test_model(&profile.0, &model.id, None, &cancelled)
                    .unwrap_err()
                    .contains(reason)
            );
            assert_eq!(status(&model.id), ModelStatus::Stopped);
            assert_eq!(std::fs::read(&pointer).unwrap(), pending);

            let (fallback, cancel) = Session::begin(route(&model.id)).unwrap();
            reply(&fallback);
            assert_eq!(status(&model.id), ModelStatus::Busy);
            assert!(
                package::start_model(&profile.0, &model.id, &cancelled)
                    .unwrap_err()
                    .contains(reason)
            );
            assert!(
                package::process::test_model(&profile.0, &model.id, None, &cancelled)
                    .unwrap_err()
                    .contains(reason)
            );
            drop(cancel);
            drop(fallback);
            package::stop_model(&profile.0, &model.id).unwrap();
            println!(
                "Manual backend mismatches refused before launch and before reusing a CPU fallback"
            );
        }
        if verify_sleep || verify_cpu_options {
            let mut configured = model.clone();
            if verify_sleep {
                configured.idle_timeout_minutes = 1;
            }
            if verify_cpu_options {
                configured.backend = ModelBackend::Cpu;
                configured.cpu_threads = 2;
                configured.gpu_layers = 0;
            }
            registry.configure(configured).unwrap();
        }
        assert_eq!(status(&model.id), ModelStatus::Stopped);
        package::start_model(&profile.0, &model.id, &cancelled).unwrap();
        assert_eq!(status(&model.id), ModelStatus::Ready);
        let mut different = selected.clone();
        different.id.push_str("-other");
        assert!(
            package::process::test_model(&profile.0, &model.id, Some(different), &cancelled)
                .unwrap_err()
                .contains("runtime changed before the model test")
        );
        let result =
            package::process::test_model(&profile.0, &model.id, Some(selected), &cancelled)
                .unwrap();
        assert_eq!(result, model.name);

        let (first, first_cancel) = Session::begin(route(&model.id)).unwrap();
        let (second, second_cancel) = Session::begin(route(&model.id)).unwrap();
        reply(&first);
        reply(&second);
        let owner = sidecar(&first);
        let shared = sidecar(&second);
        assert!(Arc::ptr_eq(owner, shared));
        let (pid, address) = (owner.pid(), owner.address());
        if verify_cpu_options {
            #[cfg(target_os = "linux")]
            {
                let command = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
                let args: Vec<_> = command.split(|&byte| byte == 0).collect();
                assert!(
                    args.windows(2)
                        .any(|pair| pair[0] == b"--threads" && pair[1] == b"2")
                );
                assert!(
                    args.windows(2)
                        .any(|pair| pair[0] == b"--n-gpu-layers" && pair[1] == b"0")
                );
            }
            println!("The signed runtime answered with CPU threads 2 and GPU layers 0");
        }
        assert_eq!(status(&model.id), ModelStatus::Busy);
        assert!(package::stop_model(&profile.0, &model.id).is_err());
        drop(first_cancel);
        assert!(matches!(request(&first), Err(Error::Cancelled)));
        drop(first);
        reply(&second);
        let sleep_probe = verify_sleep.then(|| {
            (
                shared.agent(Duration::from_secs(3)),
                shared.authorization().clone(),
                Arc::downgrade(shared),
            )
        });
        drop(second_cancel);
        drop(second);
        assert_eq!(status(&model.id), ModelStatus::Ready);
        assert!(kortty_pty::owns_loopback_listener(pid, address).unwrap());

        if let Some((agent, authorization, owner)) = sleep_probe {
            // llama.cpp b11727 reports its real unload state on /props without waking.
            let props_url = format!("http://{address}/props");
            let is_sleeping = || {
                let mut response = agent
                    .get(&props_url)
                    .header("authorization", authorization.clone())
                    .call()
                    .unwrap();
                let bytes = response
                    .body_mut()
                    .with_config()
                    .limit(1024 * 1024)
                    .read_to_vec()
                    .unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["is_sleeping"]
                    .as_bool()
                    .expect("llama-server /props omitted is_sleeping")
            };
            assert!(!is_sleeping());
            let deadline = Instant::now() + Duration::from_secs(75);
            loop {
                let sleeping = is_sleeping();
                let observed = status(&model.id);
                if sleeping && observed == ModelStatus::Sleeping {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "llama-server sleep state was {sleeping}, managed model status was {observed:?}"
                );
                std::thread::sleep(Duration::from_millis(500));
            }
            assert!(kortty_pty::owns_loopback_listener(pid, address).unwrap());
            let (woken, woken_cancel) = Session::begin(route(&model.id)).unwrap();
            reply(&woken);
            assert!(owner.ptr_eq(&Arc::downgrade(sidecar(&woken))));
            assert!(!is_sleeping());
            drop(woken_cancel);
            drop(woken);
            assert_eq!(status(&model.id), ModelStatus::Ready);
            println!("The verified runtime unloaded the model and reloaded it on the same process");
        }

        let other_model = registry
            .import(Path::new(&path), Purpose::Chat, &cancelled)
            .unwrap();
        let _other_running = StopModelOnDrop(&profile.0, &other_model.id);
        let (other, other_cancel) = Session::begin(route(&other_model.id)).unwrap();
        reply(&other);
        let other_owner = sidecar(&other);
        let (other_pid, other_address) = (other_owner.pid(), other_owner.address());
        assert_ne!(pid, other_pid);
        assert!(kortty_pty::owns_loopback_listener(pid, address).unwrap());

        let root = Directory::open(&profile.0, false)
            .unwrap()
            .private_child(DIRECTORY, false)
            .unwrap();
        assert!(
            !Activation::read(&root)
                .unwrap()
                .unwrap()
                .pending_first_launch
        );
        assert!(
            !root
                .names(64)
                .unwrap()
                .iter()
                .any(|name| name.to_string_lossy().starts_with(KEY_PREFIX))
        );
        package::stop_model(&profile.0, &model.id).unwrap();
        assert_eq!(status(&model.id), ModelStatus::Stopped);
        assert!(!kortty_pty::owns_loopback_listener(pid, address).unwrap());
        reply(&other);
        drop(other_cancel);
        drop(other);
        assert_eq!(status(&other_model.id), ModelStatus::Ready);
        assert!(kortty_pty::owns_loopback_listener(other_pid, other_address).unwrap());
        package::stop_model(&profile.0, &other_model.id).unwrap();
        assert_eq!(status(&other_model.id), ModelStatus::Stopped);
        assert!(!kortty_pty::owns_loopback_listener(other_pid, other_address).unwrap());
        println!(
            "Shared model leases survived one client's cancellation, and both models served independently until explicitly stopped"
        );
    }
}
