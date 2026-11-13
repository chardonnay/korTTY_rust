//! Private MLX activation, leases, and recovery independent of llama.cpp.

pub(super) mod channel;

use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ed25519_dalek::VerifyingKey;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::local_ai::runtime_package::{
    copy_archive, extract,
    install::remove_installation_tree,
    online::{available_space, lock_downloads},
    read_bounded, with_mlx_maintenance,
};
use crate::native_executable::ExecutableTarget;
use crate::private_files::{
    Directory, EntryIdentity, FileLock, FileStamp, nonce, require_single_link,
};

use super::index::{
    Index, LAUNCHER, MAX_REVOKED, OsVersion, PYTHON, Package, digest, identifier, release_key,
};
use super::{CatalogReview, Receipt, Review, check_cancelled, host_version, invalid};

const ROOT: &str = "mlx-runtime";
const PACKAGES: &str = "installations";
const DOWNLOADS: &str = "downloads";
const POINTER: &str = "activation.json";
const ARCHIVE: &str = "package.zip";
const INDEX: &str = "mlx-runtime-index-v1.json";
const SIGNATURE: &str = "mlx-runtime-index-v1.sig";
const OPERATION: &str = "operation.lock";
const USAGE: &str = "usage.lock";
const SESSIONS: &str = "sessions";
const TOKEN: &str = "api-key";
const MAX_SESSION_NODES: usize = 4_096;
const MAX_PACKAGES: usize = 16;
const MAX_ENTRIES: usize = 60_000;
const MAX_NODES: usize = 75_000;
const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    accepted: Option<Accepted>,
    active: Option<Installed>,
    previous: Option<Installed>,
    pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pinned_runtime_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    generated_at: String,
    sha256: String,
    revoked: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Installed {
    directory: String,
    runtime_id: String,
    installation_id: String,
    archive_sha256: String,
    index_sha256: String,
}

impl Installed {
    fn validate(&self) -> bool {
        installation_name(&self.directory)
            && identifier(&self.runtime_id)
            && identifier(&self.installation_id)
            && digest(&self.archive_sha256)
            && digest(&self.index_sha256)
    }

    fn matches(&self, package: &Package) -> bool {
        self.runtime_id == package.runtime_id
            && self.installation_id == package.installation_id
            && self.archive_sha256 == package.sha256
    }
}

impl State {
    fn read(root: &Directory) -> io::Result<(Self, Option<FileStamp>)> {
        let file = match root.open_private_file(POINTER) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((Self::default(), None));
            }
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let state: Self = serde_json::from_slice(&read_bounded(file, MAX_STATE_BYTES)?)?;
        let accepted = state
            .accepted
            .as_ref()
            .ok_or_else(|| invalid("The MLX trust state is missing"))?;
        if accepted.generated_at.parse::<Timestamp>().is_err()
            || !digest(&accepted.sha256)
            || accepted.revoked.len() > MAX_REVOKED
            || accepted.revoked.iter().any(|id| !identifier(id))
            || state
                .pinned_runtime_id
                .as_deref()
                .is_some_and(|id| !identifier(id))
            || state.pending && state.active.is_none()
            || state.previous.is_some() && state.active.is_none()
            || state
                .active
                .iter()
                .chain(&state.previous)
                .any(|entry| !entry.validate())
            || state
                .active
                .as_ref()
                .zip(state.previous.as_ref())
                .is_some_and(|(active, previous)| active.directory == previous.directory)
        {
            return Err(invalid("Invalid MLX activation state"));
        }
        stamp.require_unchanged(&root.open_private_file(POINTER)?)?;
        Ok((state, Some(stamp)))
    }

    fn accept(&mut self, index: &Index) -> io::Result<()> {
        let mut revoked = index.revoked.clone();
        if let Some(accepted) = &self.accepted {
            let current: Timestamp = accepted.generated_at.parse().map_err(io::Error::other)?;
            if index.generated_at < current
                || index.generated_at == current && index.sha256 != accepted.sha256
            {
                return Err(invalid(
                    "This signed MLX catalog is older than or conflicts with the accepted catalog",
                ));
            }
            revoked.extend(accepted.revoked.iter().cloned());
        }
        if revoked.len() > MAX_REVOKED {
            return Err(invalid("The MLX withdrawal list exceeds its storage limit"));
        }
        if self
            .pinned_runtime_id
            .as_ref()
            .is_some_and(|pin| revoked.contains(pin))
        {
            self.pinned_runtime_id = None;
        }
        self.accepted = Some(Accepted {
            generated_at: index.generated_at.to_string(),
            sha256: index.sha256.clone(),
            revoked,
        });
        Ok(())
    }

    fn withdrawn(&self, runtime_id: &str, installation_id: &str) -> bool {
        self.accepted.as_ref().is_none_or(|accepted| {
            accepted.revoked.contains(runtime_id) || accepted.revoked.contains(installation_id)
        })
    }

    fn selectable<'a>(&self, index: &'a Index, host: OsVersion) -> Vec<&'a Package> {
        let mut choices: Vec<_> = index
            .packages
            .iter()
            .filter(|package| {
                !self.withdrawn(&package.runtime_id, &package.installation_id)
                    && index
                        .select(package.size_bytes, &package.sha256, host)
                        .is_ok()
            })
            .collect();
        choices.sort_by(|left, right| {
            right
                .version_key()
                .cmp(&left.version_key())
                .then_with(|| right.runtime_id.cmp(&left.runtime_id))
        });
        choices
    }

    fn clear_unavailable_pin(&mut self, index: &Index, host: OsVersion) {
        if let Some(pin) = &self.pinned_runtime_id
            && !self
                .selectable(index, host)
                .iter()
                .any(|package| package.runtime_id == *pin)
        {
            self.pinned_runtime_id = None;
        }
    }

    fn apply_selected_version(
        &mut self,
        index: &Index,
        package: &Package,
        selection: SelectedVersion,
    ) -> io::Result<()> {
        let pin = self
            .selectable(index, selection.host)
            .first()
            .map(|newest| newest.runtime_id != package.runtime_id);
        if pin != Some(selection.pin) {
            return Err(invalid(
                "The signed MLX version choices changed. Check and review the release again.",
            ));
        }
        self.pinned_runtime_id = selection.pin.then(|| package.runtime_id.clone());
        Ok(())
    }

    fn installation(&self) -> InstallationState {
        let active = self
            .active
            .as_ref()
            .filter(|entry| !self.withdrawn(&entry.runtime_id, &entry.installation_id));
        InstallationState {
            active: active.map(|entry| (entry.installation_id.clone(), entry.runtime_id.clone())),
            pending: active.is_some() && self.pending,
            pinned_runtime_id: self.pinned_runtime_id.clone(),
        }
    }

    fn publish(
        &self,
        root: &Directory,
        prior: Option<&FileStamp>,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        use std::io::Write as _;

        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(invalid("The MLX activation state exceeds its size limit"));
        }
        root.replace_with(
            POINTER,
            |file| file.write_all(&bytes),
            || {
                check_cancelled(cancelled)?;
                require_state(root, prior)
            },
        )
    }
}

pub(super) fn install(
    profile: &Path,
    review: &Review,
    host: OsVersion,
    key: &VerifyingKey,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    check_cancelled(cancelled)?;
    let receipt = review.catalog.reopen(key)?;
    let input = review.archive.reopen()?;
    let package = receipt
        .index
        .select(review.package.size_bytes, &review.package.sha256, host)
        .map_err(io::Error::other)?;
    install_verified(
        profile,
        &receipt,
        package,
        input,
        &review.archive.stamp,
        cancelled,
        InstallOptions::default(),
    )
}

type CommitGuard<'a> = &'a dyn Fn(&Directory, Option<&FileStamp>) -> io::Result<File>;

#[derive(Clone, Copy)]
pub(super) struct SelectedVersion {
    pub(super) host: OsVersion,
    pub(super) pin: bool,
    pub(super) requires_download_policy: bool,
}

#[derive(Default)]
struct InstallOptions<'a> {
    selected_version: Option<SelectedVersion>,
    commit_guard: Option<CommitGuard<'a>>,
    requires_download_policy: bool,
}

fn install_verified(
    profile: &Path,
    receipt: &Receipt,
    package: &Package,
    mut input: File,
    stamp: &FileStamp,
    cancelled: &AtomicBool,
    options: InstallOptions<'_>,
) -> io::Result<String> {
    check_cancelled(cancelled)?;
    let root = root(profile, true)?;
    let _operation = lock(&root, OPERATION, false)?;
    let _usage = lock(&root, USAGE, false)?;
    let (mut state, prior) = State::read(&root)?;
    state.accept(&receipt.index)?;
    if state.withdrawn(&package.runtime_id, &package.installation_id) {
        return Err(invalid("This signed MLX runtime has been withdrawn"));
    }
    if let Some(selection) = options.selected_version {
        state.apply_selected_version(&receipt.index, package, selection)?;
    }
    let installations = root.private_child(PACKAGES, true)?;
    prune(&installations, &state);
    let mut stage = Stage::new(&installations)?;
    let directory = stage.directory();
    directory.publish_with(ARCHIVE, |output| {
        if copy_archive(&mut input, output, package.size_bytes, cancelled)? != package.sha256 {
            return Err(invalid("The reviewed MLX ZIP digest changed"));
        }
        stamp.require_unchanged(&input)
    })?;
    verify_archive(directory, package, cancelled, true)?;
    directory.publish_copy(INDEX, &receipt.bytes)?;
    directory.publish_copy(SIGNATURE, &receipt.signature)?;
    directory.sync()?;
    installations.sync()?;
    let previous = if state.pending {
        state.previous.take()
    } else {
        state.active.take()
    };
    state.previous =
        previous.filter(|entry| !state.withdrawn(&entry.runtime_id, &entry.installation_id));
    state.active = Some(Installed {
        directory: stage.name.clone(),
        runtime_id: package.runtime_id.clone(),
        installation_id: package.installation_id.clone(),
        archive_sha256: package.sha256.clone(),
        index_sha256: receipt.index.sha256.clone(),
    });
    state.pending = true;
    {
        let _permission = options
            .commit_guard
            .map(|check| check(&root, prior.as_ref()))
            .transpose()?;
        require_download_policy(options.requires_download_policy)?;
        state.publish(&root, prior.as_ref(), cancelled)?;
    }
    stage.keep = true;
    root.sync()?;
    prune(&installations, &state);
    Ok(package.installation_id.clone())
}

pub(super) fn compatible_packages<'a>(
    profile: &Path,
    index: &'a Index,
    host: OsVersion,
) -> io::Result<Vec<&'a Package>> {
    let mut state = match root(profile, false) {
        Ok(root) => {
            let _operation = lock(&root, OPERATION, true)?;
            State::read(&root)?.0
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => State::default(),
        Err(error) => return Err(error),
    };
    state.accept(index)?;
    Ok(state.selectable(index, host))
}

pub(super) struct StagedArchive {
    stage: Stage<Directory>,
    stamp: FileStamp,
    _lock: FileLock,
}

impl StagedArchive {
    pub(super) fn input(&self) -> io::Result<(File, FileStamp)> {
        let input = self.stage.directory().open_private_file(ARCHIVE)?;
        require_single_link(&input)?;
        self.stamp.require_unchanged(&input)?;
        Ok((input, self.stamp.clone()))
    }
}

pub(super) fn stage_download(
    profile: &Path,
    package: &Package,
    cancelled: &AtomicBool,
    transfer: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<StagedArchive> {
    check_cancelled(cancelled)?;
    let runtime = root(profile, true)?;
    let downloads = runtime
        .private_deletion_child(OsStr::new(DOWNLOADS))
        .or_else(|error| {
            if error.kind() != io::ErrorKind::NotFound {
                return Err(error);
            }
            runtime.private_child(DOWNLOADS, true)?;
            runtime.private_deletion_child(OsStr::new(DOWNLOADS))
        })?;
    let lock = lock_downloads(&downloads)?;
    prune(&downloads, &State::default());
    if available_space(&profile.join(ROOT).join(DOWNLOADS))? < package.size_bytes.saturating_mul(2)
    {
        return Err(io::Error::other(
            "Insufficient space for the MLX archive and installation copy",
        ));
    }
    let stage = Stage::new(downloads)?;
    stage
        .directory()
        .publish_checked(ARCHIVE, transfer, || check_cancelled(cancelled))?;
    let input = stage.directory().open_private_file(ARCHIVE)?;
    require_single_link(&input)?;
    let stamp = FileStamp::read(&input)?;
    check_cancelled(cancelled)?;
    Ok(StagedArchive {
        stage,
        stamp,
        _lock: lock,
    })
}

pub(super) fn install_download(
    profile: &Path,
    receipt: &Receipt,
    package: &Package,
    selection: SelectedVersion,
    cancelled: &AtomicBool,
    transfer: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<String> {
    let staged = stage_download(profile, package, cancelled, transfer)?;
    let (input, stamp) = staged.input()?;
    with_mlx_maintenance(profile, || {
        install_verified(
            profile,
            receipt,
            package,
            input,
            &stamp,
            cancelled,
            InstallOptions {
                selected_version: Some(selection),
                requires_download_policy: selection.requires_download_policy,
                ..InstallOptions::default()
            },
        )
        .map_err(|error| error.to_string())
    })
    .map_err(io::Error::other)
}

pub(super) fn accept_catalog(
    profile: &Path,
    review: &CatalogReview,
    host: OsVersion,
    key: &VerifyingKey,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    check_cancelled(cancelled)?;
    let receipt = review.reopen(key)?;
    let root = root(profile, true)?;
    let _operation = lock(&root, OPERATION, false)?;
    let _usage = lock(&root, USAGE, false)?;
    let (mut state, prior) = State::read(&root)?;
    state.accept(&receipt.index)?;
    state.clear_unavailable_pin(&receipt.index, host);
    let installations = root.private_child(PACKAGES, true)?;
    if state
        .previous
        .as_ref()
        .is_some_and(|entry| state.withdrawn(&entry.runtime_id, &entry.installation_id))
    {
        state.previous = None;
    }
    if state
        .active
        .as_ref()
        .is_some_and(|entry| state.withdrawn(&entry.runtime_id, &entry.installation_id))
    {
        restore_previous(profile, &installations, &mut state, host, key, cancelled)?;
    }
    state.publish(&root, prior.as_ref(), cancelled)?;
    root.sync()?;
    prune(&installations, &state);
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InstallationState {
    active: Option<(String, String)>,
    pending: bool,
    pinned_runtime_id: Option<String>,
}

impl InstallationState {
    pub(super) fn downloaded(id: String, package: &Package, pin: bool) -> Self {
        Self {
            active: Some((id, package.runtime_id.clone())),
            pending: true,
            pinned_runtime_id: pin.then(|| package.runtime_id.clone()),
        }
    }

    pub(crate) fn active_installation_id(&self) -> Option<&str> {
        self.active.as_ref().map(|(id, _)| id.as_str())
    }

    pub(crate) fn active_runtime_id(&self) -> Option<&str> {
        self.active.as_ref().map(|(_, id)| id.as_str())
    }

    pub(crate) fn pending_first_launch(&self) -> bool {
        self.pending
    }

    pub(crate) fn pinned_runtime_id(&self) -> Option<&str> {
        self.pinned_runtime_id.as_deref()
    }
}

pub(super) fn installed(profile: &Path) -> io::Result<InstallationState> {
    let root = match root(profile, false) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(InstallationState::default());
        }
        Err(error) => return Err(error),
    };
    let _operation = lock(&root, OPERATION, true)?;
    let (state, _) = State::read(&root)?;
    Ok(state.installation())
}

#[cfg(test)]
pub(super) fn active(profile: &Path) -> io::Result<Option<String>> {
    installed(profile).map(|state| state.active_installation_id().map(str::to_owned))
}

pub(super) fn unpin(
    profile: &Path,
    expected: &str,
    cancelled: &AtomicBool,
) -> io::Result<InstallationState> {
    check_cancelled(cancelled)?;
    let root = root(profile, false)?;
    let _operation = lock(&root, OPERATION, false)?;
    let (mut state, prior) = State::read(&root)?;
    if state.pinned_runtime_id.as_deref() != Some(expected) {
        return Err(invalid(
            "The pinned MLX release changed. Refresh MLX and try again.",
        ));
    }
    let _pending_usage = pending_usage(&root, &state)?;
    state.pinned_runtime_id = None;
    state.publish(&root, prior.as_ref(), cancelled)?;
    root.sync()?;
    Ok(state.installation())
}

pub(super) fn select_installed(
    profile: &Path,
    receipt: &Receipt,
    package: &Package,
    key: &VerifyingKey,
    selection: SelectedVersion,
    cancelled: &AtomicBool,
) -> io::Result<Option<InstallationState>> {
    let root = match root(profile, false) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let _operation = lock(&root, OPERATION, false)?;
    let (mut state, prior) = State::read(&root)?;
    let Some(active) = state
        .active
        .as_ref()
        .filter(|active| active.matches(package))
        .cloned()
    else {
        return Ok(None);
    };
    let _pending_usage = pending_usage(&root, &state)?;
    let prior_catalog = state
        .accepted
        .as_ref()
        .map(|accepted| accepted.sha256.clone());
    let prior_pin = state.pinned_runtime_id.clone();
    state.accept(&receipt.index)?;
    let installations = root.private_child(PACKAGES, false)?;
    verified_installation(
        profile,
        &installations,
        &active,
        &state,
        selection.host,
        key,
        cancelled,
    )?;
    state.apply_selected_version(&receipt.index, package, selection)?;
    require_download_policy(selection.requires_download_policy)?;
    if prior_catalog.as_deref() != Some(receipt.index.sha256.as_str())
        || prior_pin != state.pinned_runtime_id
    {
        state.publish(&root, prior.as_ref(), cancelled)?;
        root.sync()?;
    }
    Ok(Some(state.installation()))
}

fn pending_usage(root: &Directory, state: &State) -> io::Result<Option<FileLock>> {
    state.pending.then(|| lock(root, USAGE, false)).transpose()
}

fn require_download_policy(required: bool) -> io::Result<()> {
    if required {
        crate::clipboard_policy::ClipboardPolicy::load().require_ai_runtime_downloads()?;
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RemovalPlan {
    root: EntryIdentity,
    state: Option<FileStamp>,
    installations: Option<EntryIdentity>,
    packages: Vec<(OsString, EntryIdentity)>,
}

impl RemovalPlan {
    pub(crate) fn package_count(&self) -> usize {
        self.packages.len()
    }
}

pub(crate) fn review_removal(
    profile: &Path,
    cancelled: &AtomicBool,
) -> io::Result<Option<RemovalPlan>> {
    check_cancelled(cancelled)?;
    let root = match removal_root(profile) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let _operation = lock(&root, OPERATION, true)?;
    let (state, stamp) = State::read(&root)?;
    let plan = removal_plan(&root, &state, stamp.as_ref())?;
    check_cancelled(cancelled)?;
    Ok(plan)
}

fn removal_root(profile: &Path) -> io::Result<Directory> {
    Directory::open(profile, false)?.private_deletion_child(OsStr::new(ROOT))
}

fn removal_plan(
    root: &Directory,
    state: &State,
    stamp: Option<&FileStamp>,
) -> io::Result<Option<RemovalPlan>> {
    let mut packages = Vec::new();
    let installations = match root.private_deletion_child(OsStr::new(PACKAGES)) {
        Ok(installations) => {
            for name in installations.names(MAX_PACKAGES)? {
                if name.to_str().is_some_and(installation_name) {
                    let directory = installations.private_deletion_child(&name)?;
                    packages.push((name, directory.identity()?));
                }
            }
            Some(installations.identity()?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if state.active.is_none() && state.pinned_runtime_id.is_none() && packages.is_empty() {
        return Ok(None);
    }
    packages.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(Some(RemovalPlan {
        root: root.identity()?,
        state: stamp.cloned(),
        installations,
        packages,
    }))
}

fn require_removal_plan(
    root: &Directory,
    plan: &RemovalPlan,
) -> io::Result<(State, Option<FileStamp>)> {
    let (state, stamp) = State::read(root)?;
    if removal_plan(root, &state, stamp.as_ref())?.as_ref() != Some(plan) {
        return Err(invalid(
            "The installed MLX runtime changed. Review removal again.",
        ));
    }
    Ok((state, stamp))
}

pub(super) fn uninstall(
    profile: &Path,
    plan: &RemovalPlan,
    cancelled: &AtomicBool,
) -> io::Result<Option<String>> {
    check_cancelled(cancelled)?;
    let root = removal_root(profile)?;
    let _operation = lock(&root, OPERATION, false)?;
    require_removal_plan(&root, plan)?;
    check_cancelled(cancelled)?;
    crate::local_ai::runtime_package::with_mlx_maintenance(profile, || {
        uninstall_locked(&root, plan, cancelled).map_err(|error| error.to_string())
    })
    .map_err(io::Error::other)
}

fn uninstall_locked(
    root: &Directory,
    plan: &RemovalPlan,
    cancelled: &AtomicBool,
) -> io::Result<Option<String>> {
    let _usage = lock(root, USAGE, false).map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            io::Error::other("Finish active MLX model requests before removing the runtime")
        } else {
            error
        }
    })?;
    let (mut state, stamp) = require_removal_plan(root, plan)?;
    check_cancelled(cancelled)?;
    let had_active = state.active.take().is_some();
    let had_pin = state.pinned_runtime_id.take().is_some();
    if had_active || had_pin {
        state.previous = None;
        state.pending = false;
        // The same record carries the accepted catalog and withdrawals.
        state.publish(root, stamp.as_ref(), cancelled)?;
    }
    Ok(root
        .sync()
        .and_then(|()| remove_reviewed_packages(root, plan))
        .err()
        .map(|error| {
            format!("MLX runtime deactivated. Some package files could not be removed or synchronized ({error}). Review removal again to retry.")
        }))
}

fn remove_reviewed_packages(root: &Directory, plan: &RemovalPlan) -> io::Result<()> {
    if plan.packages.is_empty() {
        return Ok(());
    }
    let installations = root.private_deletion_child(OsStr::new(PACKAGES))?;
    if Some(installations.identity()?) != plan.installations {
        return Err(invalid(
            "The MLX installation directory changed during removal",
        ));
    }
    let mut failure = None;
    for (name, identity) in &plan.packages {
        let remove = || {
            let directory = installations.private_deletion_child(name)?;
            if directory.identity()? != *identity {
                return Err(invalid("An installed MLX package changed during removal"));
            }
            remove_installation_tree(&installations, name, directory, MAX_NODES + 8)
        };
        if let Err(error) = remove() {
            failure.get_or_insert(error);
        }
    }
    installations.sync()?;
    failure.map_or(Ok(()), Err)
}

pub(crate) struct Runtime {
    profile: PathBuf,
    root: Directory,
    installations: Directory,
    usage: FileLock,
    state: State,
    state_stamp: FileStamp,
    checked: Checked,
    host: OsVersion,
    key: VerifyingKey,
}

impl Runtime {
    pub(crate) fn open(profile: &Path, cancelled: &AtomicBool) -> io::Result<Self> {
        Self::open_on(
            profile,
            host_version().map_err(io::Error::other)?,
            &release_key(),
            cancelled,
        )
    }

    pub(super) fn open_on(
        profile: &Path,
        host: OsVersion,
        key: &VerifyingKey,
        cancelled: &AtomicBool,
    ) -> io::Result<Self> {
        check_cancelled(cancelled)?;
        let root = root(profile, false).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                invalid("Install a signed MLX runtime first")
            } else {
                error
            }
        })?;
        let _operation = lock(&root, OPERATION, false)?;
        let (mut state, state_stamp) = State::read(&root)?;
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| invalid("Install a signed MLX runtime first"))?;
        let state_stamp =
            state_stamp.ok_or_else(|| invalid("The MLX activation record is missing"))?;
        let usage = lock(&root, USAGE, !state.pending)?;
        let installations = root.private_child(PACKAGES, false)?;
        let checked = match verified_installation(
            profile,
            &installations,
            active,
            &state,
            host,
            key,
            cancelled,
        ) {
            Ok(checked) => checked,
            Err(error) if state.pending && error.kind() != io::ErrorKind::Interrupted => {
                restore_previous(profile, &installations, &mut state, host, key, cancelled)?;
                state.publish(&root, Some(&state_stamp), cancelled)?;
                root.sync()?;
                prune(&installations, &state);
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            profile: profile.to_owned(),
            root,
            installations,
            usage,
            state,
            state_stamp,
            checked,
            host,
            key: *key,
        })
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.checked.executable
    }
    pub(crate) fn launcher(&self) -> &Path {
        &self.checked.launcher
    }
    pub(crate) fn payload(&self) -> &Path {
        &self.checked.payload
    }

    pub(crate) fn revalidate(&self) -> io::Result<()> {
        if !self.root.same_as(&root(&self.profile, false)?)? {
            return Err(invalid("The private MLX runtime directory changed"));
        }
        if let Err(error) = require_state(&self.root, Some(&self.state_stamp)) {
            if self.state.pending {
                return Err(error);
            }
            let (current, _) = State::read(&self.root)?;
            if !self.same_ready_installation(&current)? {
                return Err(error);
            }
        }
        self.checked.revalidate()
    }

    fn same_ready_installation(&self, current: &State) -> io::Result<bool> {
        let Some(active) = self.state.active.as_ref() else {
            return Ok(false);
        };
        if current.pending
            || current.active.as_ref() != Some(active)
            || current.withdrawn(&active.runtime_id, &active.installation_id)
        {
            return Ok(false);
        }
        let Some((before, after)) = self.state.accepted.as_ref().zip(current.accepted.as_ref())
        else {
            return Ok(false);
        };
        let before_time: Timestamp = before.generated_at.parse().map_err(io::Error::other)?;
        let after_time: Timestamp = after.generated_at.parse().map_err(io::Error::other)?;
        Ok(after.revoked.is_superset(&before.revoked)
            && (after_time > before_time
                || after_time == before_time && after.sha256 == before.sha256))
    }

    pub(crate) fn ready(&mut self) -> io::Result<()> {
        if !self.state.pending {
            return Ok(());
        }
        let operation = self.root.open_lock(OPERATION)?;
        require_single_link(&operation)?;
        operation.lock()?;
        let _operation = FileLock::new(operation);
        self.revalidate()?;
        self.state.pending = false;
        self.state
            .publish(&self.root, Some(&self.state_stamp), &AtomicBool::new(false))?;
        self.state_stamp = FileStamp::read(&self.root.open_private_file(POINTER)?)?;
        self.root.sync()?;
        self.usage.unlock()?;
        self.usage.try_lock_shared().map_err(io::Error::from)
    }

    pub(crate) fn recover(&mut self, cancelled: &AtomicBool) -> io::Result<bool> {
        if !self.state.pending {
            return Ok(false);
        }
        check_cancelled(cancelled)?;
        let operation = self.root.open_lock(OPERATION)?;
        require_single_link(&operation)?;
        operation.lock()?;
        let _operation = FileLock::new(operation);
        require_state(&self.root, Some(&self.state_stamp))?;
        restore_previous(
            &self.profile,
            &self.installations,
            &mut self.state,
            self.host,
            &self.key,
            cancelled,
        )?;
        self.state
            .publish(&self.root, Some(&self.state_stamp), cancelled)?;
        self.root.sync()?;
        prune(&self.installations, &self.state);
        Ok(self.state.active.is_some())
    }

    pub(crate) fn key_file(&self, token: &str) -> io::Result<KeyFile> {
        self.revalidate()?;
        if !(16..=128).contains(&token.len())
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid("Invalid MLX sidecar key"));
        }
        let root = root(&self.profile, false)?;
        if !root.same_as(&self.root)? {
            return Err(invalid("The private MLX runtime directory changed"));
        }
        let parent = root.private_child(SESSIONS, true)?;
        let name = format!("run-{}", nonce()?);
        let directory = parent.create_private_subdirectory(OsStr::new(&name))?;
        let mut key = KeyFile {
            path: self
                .profile
                .join(ROOT)
                .join(SESSIONS)
                .join(&name)
                .join(TOKEN),
            parent,
            directory: Some(directory),
            name,
            stamp: None,
        };
        let directory = key.directory.as_ref().expect("live MLX session directory");
        directory
            .private_child("huggingface", true)?
            .private_child("hub", true)?;
        directory.publish_copy(TOKEN, token.as_bytes())?;
        let file = directory.open_private_file(TOKEN)?;
        require_single_link(&file)?;
        key.stamp = Some(FileStamp::read(&file)?);
        Ok(key)
    }
}

pub(crate) struct KeyFile {
    path: PathBuf,
    parent: Directory,
    directory: Option<Directory>,
    name: String,
    stamp: Option<FileStamp>,
}

impl KeyFile {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn clear_token(&self) -> io::Result<()> {
        if let Some(stamp) = &self.stamp {
            let directory = self.directory.as_ref().expect("live MLX session directory");
            directory.remove_unchanged(TOKEN, stamp)?;
        }
        Ok(())
    }
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take() {
            let _ = remove_installation_tree(
                &self.parent,
                OsStr::new(&self.name),
                directory,
                MAX_SESSION_NODES,
            );
        }
    }
}

struct Checked {
    payload: PathBuf,
    directory: Directory,
    executable: PathBuf,
    executable_stamp: FileStamp,
    launcher: PathBuf,
    launcher_stamp: FileStamp,
}

impl Checked {
    fn revalidate(&self) -> io::Result<()> {
        let directory = Directory::open(&self.payload, false)?;
        if !directory.same_as(&self.directory)? {
            return Err(invalid("The verified MLX runtime payload changed"));
        }
        let (python, launcher) = entrypoints(&directory)?;
        self.executable_stamp.require_unchanged(&python)?;
        self.launcher_stamp.require_unchanged(&launcher)
    }
}

fn verified_installation(
    profile: &Path,
    installations: &Directory,
    installed: &Installed,
    state: &State,
    host: OsVersion,
    key: &VerifyingKey,
    cancelled: &AtomicBool,
) -> io::Result<Checked> {
    if state.withdrawn(&installed.runtime_id, &installed.installation_id) {
        return Err(invalid("The signed MLX runtime has been withdrawn"));
    }
    let directory = installations.private_child(&installed.directory, false)?;
    let receipt = Receipt::from_files(
        directory.open_private_file(INDEX)?,
        directory.open_private_file(SIGNATURE)?,
        key,
    )?;
    let accepted = state
        .accepted
        .as_ref()
        .ok_or_else(|| invalid("The MLX trust state is missing"))?;
    let latest: Timestamp = accepted.generated_at.parse().map_err(io::Error::other)?;
    if receipt.index.sha256 != installed.index_sha256
        || receipt.index.generated_at > latest
        || receipt.index.generated_at == latest && receipt.index.sha256 != accepted.sha256
    {
        return Err(invalid("The installed MLX signed receipt changed"));
    }
    let package = receipt
        .index
        .packages
        .iter()
        .find(|package| installed.matches(package))
        .ok_or_else(|| invalid("The installed MLX package is absent from its signed receipt"))?;
    receipt
        .index
        .select(package.size_bytes, &package.sha256, host)
        .map_err(io::Error::other)?;
    let (directory, executable_stamp, launcher_stamp) =
        verify_archive(&directory, package, cancelled, false)?;
    let payload = profile
        .join(ROOT)
        .join(PACKAGES)
        .join(&installed.directory)
        .join(extract::PAYLOAD)
        .join(&package.installation_id);
    let checked = Checked {
        executable: payload.join(PYTHON),
        launcher: payload.join(LAUNCHER),
        payload,
        directory,
        executable_stamp,
        launcher_stamp,
    };
    checked.revalidate()?;
    Ok(checked)
}

fn verify_archive(
    directory: &Directory,
    package: &Package,
    cancelled: &AtomicBool,
    create: bool,
) -> io::Result<(Directory, FileStamp, FileStamp)> {
    let python = format!("{}/{PYTHON}", package.installation_id);
    let launcher = format!("{}/{LAUNCHER}", package.installation_id);
    let layout = extract::Layout {
        size: package.size_bytes,
        sha256: &package.sha256,
        executable: &python,
        companion: Some(&launcher),
        wrapper: Some(&package.installation_id),
        entries: MAX_ENTRIES,
        nodes: MAX_NODES,
    };
    extract::apply_archive(
        directory.open_private_file(ARCHIVE)?,
        directory,
        layout,
        cancelled,
        create,
    )?;
    let payload = directory
        .private_child(extract::PAYLOAD, false)?
        .private_child(&package.installation_id, false)?;
    let (mut executable, launcher) = entrypoints(&payload)?;
    let executable_stamp = FileStamp::read(&executable)?;
    if !ExecutableTarget::parse("aarch64-apple-darwin")?.matches(&mut executable)? {
        return Err(invalid(
            "The signed MLX interpreter is not a macOS ARM64 executable",
        ));
    }
    executable_stamp.require_unchanged(&executable)?;
    let launcher_stamp = FileStamp::read(&launcher)?;
    Ok((payload, executable_stamp, launcher_stamp))
}

fn entrypoints(payload: &Directory) -> io::Result<(File, File)> {
    let python = payload
        .private_child("python", false)?
        .private_child("bin", false)?
        .open_private_file("python3")?;
    let launcher = payload.open_private_file(LAUNCHER)?;
    require_single_link(&python)?;
    require_single_link(&launcher)?;
    Ok((python, launcher))
}

fn restore_previous(
    profile: &Path,
    installations: &Directory,
    state: &mut State,
    host: OsVersion,
    key: &VerifyingKey,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    state.active = if let Some(previous) = state.previous.take() {
        match verified_installation(
            profile,
            installations,
            &previous,
            state,
            host,
            key,
            cancelled,
        ) {
            Ok(_) => Some(previous),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(_) => None,
        }
    } else {
        None
    };
    state.pending = false;
    check_cancelled(cancelled)
}

fn root(profile: &Path, create: bool) -> io::Result<Directory> {
    Directory::open(profile, false)?.private_child(ROOT, create)
}

fn lock(directory: &Directory, name: &str, shared: bool) -> io::Result<FileLock> {
    let file = directory.open_lock(name)?;
    require_single_link(&file)?;
    if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    }
    .map_err(io::Error::from)?;
    Ok(FileLock::new(file))
}

fn require_state(root: &Directory, prior: Option<&FileStamp>) -> io::Result<()> {
    match (prior, root.open_private_file(POINTER)) {
        (Some(stamp), Ok(file)) => {
            require_single_link(&file)?;
            stamp.require_unchanged(&file)
        }
        (None, Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err(invalid("The MLX activation changed")),
    }
}

fn installation_name(name: &str) -> bool {
    name.strip_prefix("package-")
        .is_some_and(crate::local_ai::managed_models::valid_id)
}

struct Stage<P: Borrow<Directory>> {
    installations: P,
    directory: Option<Directory>,
    name: String,
    keep: bool,
}

impl<P: Borrow<Directory>> Stage<P> {
    fn new(installations: P) -> io::Result<Self> {
        let parent = installations.borrow();
        if parent.names(MAX_PACKAGES)?.len() >= MAX_PACKAGES {
            return Err(invalid("The private MLX installation directory is full"));
        }
        let name = format!("package-{}", nonce()?);
        let directory = parent.create_private_subdirectory(OsStr::new(&name))?;
        Ok(Self {
            installations,
            directory: Some(directory),
            name,
            keep: false,
        })
    }

    fn directory(&self) -> &Directory {
        self.directory
            .as_ref()
            .expect("active MLX installation stage")
    }
}

impl<P: Borrow<Directory>> Drop for Stage<P> {
    fn drop(&mut self) {
        if !self.keep
            && let Some(directory) = self.directory.take()
        {
            let _ = remove_installation_tree(
                self.installations.borrow(),
                OsStr::new(&self.name),
                directory,
                MAX_NODES + 8,
            );
        }
    }
}

fn prune(installations: &Directory, state: &State) {
    let Ok(names) = installations.names(MAX_PACKAGES) else {
        return;
    };
    for name in names {
        let Some(value) = name.to_str().filter(|name| installation_name(name)) else {
            continue;
        };
        if state
            .active
            .iter()
            .chain(&state.previous)
            .any(|entry| entry.directory == value)
        {
            continue;
        }
        if let Ok(directory) = installations.private_deletion_child(&name) {
            let _ = remove_installation_tree(installations, &name, directory, MAX_NODES + 8);
        }
    }
}
