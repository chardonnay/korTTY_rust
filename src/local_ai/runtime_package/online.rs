//! Download of signed stable llama.cpp runtime releases.

pub(crate) mod policy;
mod resume;

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pgp::crypto::hash::HashAlgorithm;
use ureq::{Agent, Body, http::Response};
use url::Url;

use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::{Directory, FileLock, require_single_link};

use super::super::runtime_index::Package;
use super::{
    API_CONTRACT_VERSION, AcceptedIndex, Authorization, COMPATIBILITY_VERSION, DIRECTORY, Host,
    MAX_ARCHIVE_BYTES, MAX_INDEX_BYTES, MAX_SIGNATURE_BYTES, PackageBackend, Pending,
    VerifiedIndex, check_cancelled, install, pool, process, work, work_with_agent,
};

const RELEASE_PATH: &str = "/chardonnay/kortty-llama-runtimes/releases/";
pub(crate) const INDEX_URL: &str = "https://github.com/chardonnay/kortty-llama-runtimes/releases/latest/download/runtime-index-v1.json";
pub(crate) const SIGNATURE_URL: &str = "https://github.com/chardonnay/kortty-llama-runtimes/releases/latest/download/runtime-index-v1.sig";
const MAX_REDIRECTS: usize = 5;
const MAX_CHOICES: usize = 256;
const DOWNLOADS: &str = "runtime-downloads";
const ARCHIVE: &str = "runtime.zip";

pub(crate) struct Catalog {
    index: VerifiedIndex,
    bytes: Vec<u8>,
    signature: String,
    choices: Vec<Choice>,
}

impl fmt::Debug for Catalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Verified runtime catalog")
            .field("choices", &self.choices.len())
            .finish_non_exhaustive()
    }
}

impl Catalog {
    pub(crate) fn choices(&self) -> &[Choice] {
        &self.choices
    }

    pub(crate) fn preferred_choice(&self, required: Option<PackageBackend>) -> Option<&Choice> {
        policy::select(&self.choices, required, None, cfg!(target_os = "macos"))
    }

    pub(crate) fn pins(&self, selected: &Choice) -> bool {
        self.preferred_choice(Some(selected.backend))
            .is_some_and(|newest| newest.id != selected.id)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Choice {
    pub(crate) id: String,
    pub(crate) backend: PackageBackend,
    pub(crate) size: u64,
    pub(crate) url: String,
    digest: String,
    entrypoint: String,
}

impl fmt::Debug for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Signed runtime choice")
            .field("id", &self.id)
            .field("backend", &self.backend)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} · {} · {} MiB",
            self.id,
            self.backend,
            self.size.div_ceil(1024 * 1024)
        )
    }
}

impl Choice {
    pub(super) fn from_package(package: &Package) -> Self {
        Self {
            id: package.installation_id().into(),
            backend: package.backend(),
            size: package.size(),
            url: package.download_url().into(),
            digest: package.sha256().into(),
            entrypoint: package.entrypoint().into(),
        }
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    fn asset(&self) -> VerifiedAsset<'_> {
        VerifiedAsset {
            url: &self.url,
            size: self.size,
            sha256: self.digest(),
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::local_ai) struct VerifiedAsset<'a> {
    pub url: &'a str,
    pub size: u64,
    pub sha256: &'a str,
}

#[derive(Clone, Copy)]
pub(crate) struct Progress {
    pub(crate) received: u64,
    pub(crate) total: u64,
    pub(crate) resumable: bool,
}

pub(crate) struct Download {
    pub(crate) pending: Pending<InstallOutcome>,
    pub(crate) progress: async_channel::Receiver<Progress>,
    pub(crate) pause: Pause,
}

pub(crate) struct Pause(Arc<AtomicBool>, Agent);

impl Pause {
    pub(crate) fn request(&self) {
        self.0.store(true, Ordering::Release);
        self.1.clone().shutdown();
    }
}

pub(crate) enum InstallOutcome {
    Installed { id: String, cleanup_failed: bool },
    Paused { received: u64, interrupted: bool },
}

#[derive(Clone, Debug)]
pub(crate) enum Checked {
    Current { pending_first_launch: bool },
    NoCompatible { revoked: bool },
    Offer { id: String, revoked: bool },
    Stage(Arc<Selection>),
}

pub(crate) struct Selection {
    profile: PathBuf,
    catalog: Catalog,
    selected: Choice,
    prior: install::Installed,
    authorize: Arc<Authorization>,
    pub(crate) revoked: bool,
}

impl fmt::Debug for Selection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Verified runtime selection")
            .finish_non_exhaustive()
    }
}

pub(crate) struct Staged {
    selection: Arc<Selection>,
    archive: Downloaded,
}

impl fmt::Debug for Staged {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Staged runtime archive")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Activation {
    Installed(String),
    Waiting,
}

pub(crate) fn check_policy(
    profile: PathBuf,
    settings: policy::Settings,
    authorize: Arc<Authorization>,
) -> Result<Pending<Checked>, String> {
    if !settings.mode.checks_channel() {
        return Err("Background runtime checks are disabled.".into());
    }
    let agent = Agent::new_with_config(super::super::connection_config(Duration::from_secs(30)));
    work_with_agent(Some(agent.clone()), move |cancelled| {
        let catalog = fetch_catalog(&profile, None, &agent, Some(&*authorize), cancelled)?;
        let mut prior = install::installed(&profile, true)
            .map_err(|error| format!("Could not check the installed runtime ({error})."))?;
        if prior.pinned_runtime().is_some_and(|pin| {
            !settings.pin_available(catalog.choices(), prior.policy_status(), pin)
        }) {
            prior
                .clear_unavailable_pin(&profile, &catalog.index, &*authorize, cancelled)
                .map_err(|error| {
                    format!("Could not clear the unavailable runtime pin ({error}).")
                })?;
        }
        Ok(
            match settings.decision(
                catalog.choices(),
                prior.policy_status(),
                prior.pinned_runtime(),
            ) {
                policy::Decision::Disabled => {
                    return Err("Background runtime checks are disabled.".into());
                }
                policy::Decision::Current {
                    pending_first_launch,
                } => Checked::Current {
                    pending_first_launch,
                },
                policy::Decision::NoCompatible { revoked } => Checked::NoCompatible { revoked },
                policy::Decision::Offer { package, revoked } => Checked::Offer {
                    id: package.id.clone(),
                    revoked,
                },
                policy::Decision::Stage { package, revoked } => {
                    let selected = package.clone();
                    Checked::Stage(Arc::new(Selection {
                        profile,
                        catalog,
                        selected,
                        prior,
                        authorize,
                        revoked,
                    }))
                }
            },
        )
    })
}

pub(crate) fn stage_update(selection: Arc<Selection>) -> Result<Pending<Arc<Staged>>, String> {
    let agent = Agent::new_with_config(super::super::connection_config(Duration::from_secs(
        2 * 60 * 60,
    )));
    work_with_agent(Some(agent.clone()), move |cancelled| {
        let archive = download_archive(
            &selection.profile,
            &selection.catalog,
            &selection.selected,
            &agent,
            &*selection.authorize,
            cancelled,
        )
        .map_err(|error| install_error(error, cancelled))?;
        Ok(Arc::new(Staged { selection, archive }))
    })
}

pub(crate) fn apply_update(staged: Arc<Staged>) -> Result<Pending<Activation>, String> {
    work(move |cancelled| {
        let selection = &staged.selection;
        let automatic = install::Automatic {
            prior: &selection.prior,
            authorize: &*selection.authorize,
        };
        let archive = staged.archive.stage.directory().open_private_file(ARCHIVE);
        match archive.and_then(|archive| {
            activate_archive(
                &selection.profile,
                &selection.catalog,
                &selection.selected,
                archive,
                Some(automatic),
                cancelled,
            )
        }) {
            Ok(id) => Ok(Activation::Installed(id)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(Activation::Waiting),
            Err(error) => Err(install_error(error, cancelled)),
        }
    })
}

pub(crate) fn check(
    profile: PathBuf,
    required: Option<PackageBackend>,
) -> Result<Pending<Arc<Catalog>>, String> {
    let agent = Agent::new_with_config(super::super::connection_config(Duration::from_secs(30)));
    work_with_agent(Some(agent.clone()), move |cancelled| {
        fetch_catalog(&profile, required, &agent, None, cancelled).map(Arc::new)
    })
}

fn fetch_catalog(
    profile: &Path,
    required: Option<PackageBackend>,
    agent: &Agent,
    authorize: Option<&Authorization>,
    cancelled: &AtomicBool,
) -> Result<Catalog, String> {
    let permit = || {
        authorize
            .map(|authorize| authorize())
            .transpose()
            .map_err(|error| error.to_string())
    };
    let bytes = fetch(agent, INDEX_URL, MAX_INDEX_BYTES, authorize, cancelled)?;
    let signature = String::from_utf8(fetch(
        agent,
        SIGNATURE_URL,
        MAX_SIGNATURE_BYTES,
        authorize,
        cancelled,
    )?)
    .map_err(|_| "The runtime signature is not Base64 text.")?;
    let index = VerifiedIndex::verify(&bytes, &signature)?;
    ClipboardPolicy::load()
        .require_ai_runtime_downloads()
        .map_err(|error| error.to_string())?;
    let _permission = permit()?;
    checked_catalog(profile, index, bytes, signature, required, cancelled)
}

fn fetch(
    agent: &Agent,
    url: &str,
    limit: usize,
    authorize: Option<&Authorization>,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    read_metadata(
        get(agent, url, cancelled, false, authorize)?,
        limit,
        cancelled,
    )
}

pub(in crate::local_ai) fn fetch_release_metadata(
    agent: &Agent,
    url: &str,
    limit: usize,
    allow_missing: bool,
    authorize: Option<&Authorization>,
    cancelled: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    let response = get_range(agent, url, cancelled, false, None, authorize, allow_missing)?;
    if response.status() == ureq::http::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    read_metadata(response, limit, cancelled).map(Some)
}

fn read_metadata(
    mut response: Response<Body>,
    limit: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| network_error(cancelled))?;
    check_cancelled(cancelled).map_err(|_| network_error(cancelled))?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err("The runtime channel returned an invalid metadata size.".into());
    }
    Ok(bytes)
}

fn checked_catalog(
    profile: &Path,
    index: VerifiedIndex,
    bytes: Vec<u8>,
    signature: String,
    required: Option<PackageBackend>,
    cancelled: &AtomicBool,
) -> Result<Catalog, String> {
    check_cancelled(cancelled).map_err(|_| network_error(cancelled))?;
    let host = Host::current()?;
    let root = Directory::open(profile, true)
        .and_then(|profile| profile.private_child(DIRECTORY, true))
        .map_err(|error| format!("Could not open runtime trust storage ({error})."))?;
    let lock = root
        .open_lock("operation.lock")
        .map_err(|error| error.to_string())?;
    require_single_link(&lock).map_err(|error| error.to_string())?;
    lock.try_lock()
        .map_err(|_| "Another runtime operation is in progress.")?;
    let _lock = FileLock::new(lock);
    let accepted =
        AcceptedIndex::accept(&root, &index, cancelled).map_err(|error| error.to_string())?;
    pool::trust_changed();
    let mut choices = Vec::new();
    for package in index.packages() {
        if package.size() > MAX_ARCHIVE_BYTES
            || required.is_some_and(|backend| package.backend() != backend)
            || !index.is_compatible(package, host, API_CONTRACT_VERSION, COMPATIBILITY_VERSION)?
            || accepted.revoked_ids.contains(package.runtime_id())
            || accepted.revoked_ids.contains(package.installation_id())
            || initial_url(package.download_url(), false).is_err()
        {
            continue;
        }
        if choices.len() == MAX_CHOICES {
            return Err("The runtime channel contains too many compatible packages.".into());
        }
        choices.push(Choice::from_package(package));
    }
    choices.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    Ok(Catalog {
        index,
        bytes,
        signature,
        choices,
    })
}

pub(crate) fn install(
    profile: PathBuf,
    catalog: Arc<Catalog>,
    selected: Choice,
) -> Result<Download, String> {
    if !catalog.choices.contains(&selected) {
        return Err("Select a package from the verified runtime catalog.".into());
    }
    let (progress, updates) = async_channel::bounded(2);
    let agent = Agent::new_with_config(super::super::connection_config(Duration::from_secs(
        2 * 60 * 60,
    )));
    let pausing = Arc::new(AtomicBool::new(false));
    let pause = Pause(Arc::clone(&pausing), agent.clone());
    let pending = work_with_agent(Some(agent.clone()), move |cancelled| {
        let run = || {
            check_cancelled(cancelled)?;
            ClipboardPolicy::load().require_ai_runtime_downloads()?;
            let downloads = download_root(&profile, &catalog)?;
            let _lock = lock_downloads(&downloads)?;
            let plan = resume::Plan::new(catalog.index.sha256(), &selected);
            let saved = resume::saved(&downloads)?;
            if available_space(&profile.join(DOWNLOADS))? < plan.required_space(saved.as_ref()) {
                return Err(io::Error::other(
                    "Insufficient space for the runtime archive and installation copy",
                ));
            }
            match resume::fetch(
                &downloads,
                &plan,
                cancelled,
                &pausing,
                |range| {
                    get_range(&agent, &selected.url, cancelled, false, range, None, false)
                        .map_err(io::Error::other)
                },
                |update| {
                    let _ = progress.try_send(update);
                },
            )? {
                resume::Outcome::Ready(_) if pausing.load(Ordering::Acquire) => {
                    Ok(InstallOutcome::Paused {
                        received: selected.size,
                        interrupted: false,
                    })
                }
                resume::Outcome::Ready(archive) => {
                    let id =
                        activate_archive(&profile, &catalog, &selected, archive, None, cancelled)?;
                    let cleanup_failed = resume::discard(&downloads).is_err();
                    Ok(InstallOutcome::Installed { id, cleanup_failed })
                }
                resume::Outcome::Paused {
                    received,
                    interrupted,
                } => Ok(InstallOutcome::Paused {
                    received,
                    interrupted,
                }),
            }
        };
        run().map_err(|error| install_error(error, cancelled))
    })?;
    Ok(Download {
        pending,
        progress: updates,
        pause,
    })
}

pub(crate) fn discard_download(profile: PathBuf) -> Result<Pending<bool>, String> {
    work(move |cancelled| {
        let run = || {
            check_cancelled(cancelled)?;
            let downloads = match Directory::open(&profile, false)
                .and_then(|root| root.private_child(DOWNLOADS, false))
            {
                Ok(downloads) => downloads,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            let _lock = lock_downloads(&downloads)?;
            check_cancelled(cancelled)?;
            resume::discard(&downloads)
        };
        run().map_err(|error| format!("Could not discard the saved runtime download ({error})."))
    })
}

fn install_error(error: io::Error, cancelled: &AtomicBool) -> String {
    if cancelled.load(Ordering::Acquire) {
        network_error(cancelled)
    } else {
        format!("Could not install the downloaded runtime ({error}).")
    }
}

struct Downloaded {
    stage: install::Stage<Directory>,
    _lock: FileLock,
}

fn download_archive(
    profile: &Path,
    catalog: &Catalog,
    selected: &Choice,
    agent: &Agent,
    authorize: &Authorization,
    cancelled: &AtomicBool,
) -> io::Result<Downloaded> {
    check_cancelled(cancelled)?;
    let downloads = download_root(profile, catalog)?;
    let lock = lock_downloads(&downloads)?;
    if available_space(&profile.join(DOWNLOADS))? < selected.size.saturating_mul(2) {
        return Err(io::Error::other(
            "Insufficient space for the runtime archive and installation copy",
        ));
    }
    let stage = install::Stage::new(downloads)?;
    let directory = stage.directory();
    directory.publish_checked(
        ARCHIVE,
        |file| download_verified_asset(agent, selected.asset(), file, Some(authorize), cancelled),
        || check_cancelled(cancelled),
    )?;
    Ok(Downloaded { stage, _lock: lock })
}

fn download_root(profile: &Path, catalog: &Catalog) -> io::Result<Directory> {
    let profile = Directory::open(profile, false)?;
    let trust = profile.private_child(DIRECTORY, false)?;
    if AcceptedIndex::read(&trust)?.is_none_or(|accepted| accepted.sha256 != catalog.index.sha256())
    {
        return Err(io::Error::other(
            "The signed channel changed. Check and review the package again",
        ));
    }
    profile.private_child(DOWNLOADS, true)
}

pub(in crate::local_ai) fn lock_downloads(directory: &Directory) -> io::Result<FileLock> {
    let lock = directory.open_lock("download.lock")?;
    require_single_link(&lock)?;
    lock.try_lock().map_err(io::Error::from)?;
    Ok(FileLock::new(lock))
}

#[cfg(unix)]
pub(in crate::local_ai) fn available_space(path: &Path) -> io::Result<u64> {
    let space = rustix::fs::statvfs(path)?;
    Ok(space.f_bavail.saturating_mul(space.f_frsize))
}

#[cfg(windows)]
pub(in crate::local_ai) fn available_space(path: &Path) -> io::Result<u64> {
    kortty_files::available_space(path)
}

fn activate_archive(
    profile: &Path,
    catalog: &Catalog,
    selected: &Choice,
    archive: File,
    automatic: Option<install::Automatic<'_>>,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    check_cancelled(cancelled)?;
    require_single_link(&archive)?;
    let pinned = match &automatic {
        Some(automatic) => automatic
            .prior
            .pinned_runtime()
            .is_some_and(|pin| policy::runtime_id(&selected.id) == Some(pin)),
        None => catalog.pins(selected),
    };
    install::activate(
        profile,
        install::Receipt {
            index: &catalog.index,
            bytes: &catalog.bytes,
            signature: &catalog.signature,
            automatic,
            online: true,
            pinned,
        },
        Host::current().map_err(io::Error::other)?,
        archive,
        Some(selected.backend),
        cancelled,
        process::version,
    )
}

pub(in crate::local_ai) fn download_verified_asset(
    agent: &Agent,
    asset: VerifiedAsset<'_>,
    file: &mut File,
    authorize: Option<&Authorization>,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut response =
        get(agent, asset.url, cancelled, false, authorize).map_err(io::Error::other)?;
    if response
        .headers()
        .get("content-length")
        .is_some_and(|value| {
            value
                .to_str()
                .ok()
                .and_then(|text| text.parse::<u64>().ok())
                != Some(asset.size)
        })
    {
        return Err(io::Error::other(
            "Runtime archive length differs from the signed index",
        ));
    }
    copy_verified(
        response.body_mut().as_reader(),
        file,
        asset,
        cancelled,
        |_| {},
    )
}

fn copy_verified(
    mut input: impl Read,
    output: &mut impl Write,
    asset: VerifiedAsset<'_>,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> io::Result<()> {
    let mut hasher = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut buffer = vec![0; 64 * 1024];
    let mut received = 0;
    let mut emitted = Instant::now();
    while received < asset.size {
        check_cancelled(cancelled)?;
        let limit = (asset.size - received).min(buffer.len() as u64) as usize;
        let count = input.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(io::Error::other(
                "Runtime archive ended before its signed length",
            ));
        }
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        received += count as u64;
        if received == asset.size || emitted.elapsed() >= Duration::from_millis(100) {
            progress(Progress {
                received,
                total: asset.size,
                resumable: false,
            });
            emitted = Instant::now();
        }
    }
    check_cancelled(cancelled)?;
    if input.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::other(
            "Runtime archive exceeds its signed length",
        ));
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != asset.sha256 {
        return Err(io::Error::other(
            "Runtime archive SHA-256 differs from the signed index",
        ));
    }
    Ok(())
}

fn get(
    agent: &Agent,
    text: &str,
    cancelled: &AtomicBool,
    allow_loopback: bool,
    authorize: Option<&Authorization>,
) -> Result<Response<Body>, String> {
    get_range(
        agent,
        text,
        cancelled,
        allow_loopback,
        None,
        authorize,
        false,
    )
}

fn get_range(
    agent: &Agent,
    text: &str,
    cancelled: &AtomicBool,
    allow_loopback: bool,
    range: Option<resume::Range<'_>>,
    authorize: Option<&Authorization>,
    allow_missing: bool,
) -> Result<Response<Body>, String> {
    let first = initial_url(text, allow_loopback)?;
    let mut url = first.clone();
    for hops in 0..=MAX_REDIRECTS {
        check_cancelled(cancelled).map_err(|_| network_error(cancelled))?;
        if !allow_loopback {
            ClipboardPolicy::load()
                .require_ai_runtime_downloads()
                .map_err(|error| error.to_string())?;
        }
        if let Some(authorize) = authorize {
            // Revalidate persisted opt-in before every request, including redirected requests.
            authorize().map_err(|error| error.to_string())?;
        }
        let mut request = agent
            .get(url.as_str())
            .header("Accept-Encoding", "identity")
            .header("User-Agent", "kortty-runtime-updater");
        if let Some(range) = &range {
            request = request
                .header("Range", format!("bytes={}-", range.offset))
                .header("If-Range", range.etag);
        }
        let response = request
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .map_err(|_| network_error(cancelled))?;
        let status = response.status();
        if status == ureq::http::StatusCode::OK
            || (range.is_some() && status == ureq::http::StatusCode::PARTIAL_CONTENT)
            || (allow_missing && status == ureq::http::StatusCode::NOT_FOUND)
        {
            return Ok(response);
        }
        if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Err(format!("The runtime channel returned HTTP {status}."));
        }
        if hops == MAX_REDIRECTS {
            return Err("The runtime channel redirected too many times.".into());
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 4096)
            .ok_or("The runtime channel supplied an invalid redirect.")?;
        let next = url
            .join(location)
            .map_err(|_| "The runtime channel supplied an invalid redirect.")?;
        if !allowed_redirect(&first, &url, &next, allow_loopback) {
            return Err(
                "The runtime channel redirected outside the approved release destination.".into(),
            );
        }
        url = next;
    }
    unreachable!("redirect loop is bounded")
}

fn initial_url(text: &str, allow_loopback: bool) -> Result<Url, String> {
    let invalid = || "The signed runtime URL is invalid or outside the release channel.".to_owned();
    if text.len() > 4096
        || text
            .chars()
            .any(|character| character.is_control() || character == '\\')
    {
        return Err(invalid());
    }
    let url = Url::parse(text).map_err(|_| invalid())?;
    if !secure(&url, allow_loopback)
        || url.query().is_some()
        || (url.host_str() == Some("github.com") && !url.path().starts_with(RELEASE_PATH))
    {
        return Err(invalid());
    }
    Ok(url)
}

fn secure(url: &Url, allow_loopback: bool) -> bool {
    let loopback = allow_loopback
        && url.scheme() == "http"
        && matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback());
    (url.scheme() == "https" || loopback)
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn allowed_redirect(first: &Url, current: &Url, next: &Url, allow_loopback: bool) -> bool {
    if !secure(next, allow_loopback) {
        return false;
    }
    if next.origin() == current.origin() {
        return next.host_str() != Some("github.com") || next.path().starts_with(RELEASE_PATH);
    }
    first.host_str() == Some("github.com")
        && current.host_str() == Some("github.com")
        && next.scheme() == "https"
        && next.port().is_none()
        && matches!(
            next.host_str(),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
        )
}

fn network_error(cancelled: &AtomicBool) -> String {
    if cancelled.load(Ordering::Acquire) {
        "Runtime operation canceled.".into()
    } else {
        "Could not read the signed runtime release channel.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    #[test]
    fn pinned_redirects_stay_in_the_release_repository_and_known_cdn() {
        let initial = initial_url(INDEX_URL, false).unwrap();
        let release = initial
            .join(
                "/chardonnay/kortty-llama-runtimes/releases/download/current/runtime-index-v1.json",
            )
            .unwrap();
        let cdn =
            Url::parse("https://release-assets.githubusercontent.com/path?token=redirect-only")
                .unwrap();
        assert!(allowed_redirect(&initial, &initial, &release, false));
        assert!(allowed_redirect(&initial, &release, &cdn, false));
        for target in [
            "https://github.com/other/repo/releases/download/x/index.json",
            "https://other.githubusercontent.com/a",
            "http://release-assets.githubusercontent.com/a",
            "https://user@release-assets.githubusercontent.com/a",
        ] {
            assert!(!allowed_redirect(
                &initial,
                &release,
                &Url::parse(target).unwrap(),
                false
            ));
        }
    }

    #[test]
    fn real_http_follows_only_the_checked_relative_redirect() {
        use crate::local_ai::test_support::{
            accept, fixture_http_response, listener, wire_request,
        };
        let (listener, _) = listener();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let mut first = accept(&listener);
            wire_request(&mut first, "GET /index HTTP/1.1");
            first
                .get_mut()
                .write_all(
                    fixture_http_response("302 Found", "Location: /signed\r\n", "").as_bytes(),
                )
                .unwrap();
            let mut second = accept(&listener);
            wire_request(&mut second, "GET /signed HTTP/1.1");
            second
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", "fixture").as_bytes())
                .unwrap();
        });
        let agent = Agent::new_with_config(super::super::super::connection_config(
            Duration::from_secs(5),
        ));
        let url = format!("http://{address}/index");
        assert!(initial_url(&url, false).is_err());
        let mut response = get(&agent, &url, &AtomicBool::new(false), true, None).unwrap();
        let mut body = String::new();
        response
            .body_mut()
            .as_reader()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "fixture");
        worker.join().unwrap();
    }

    #[test]
    fn redirect_rechecks_saved_authority_before_contacting_the_next_hop() {
        use crate::local_ai::test_support::{
            accept, fixture_http_response, listener, wire_request,
        };

        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("fixture-permission");
        std::fs::write(&path, b"fixture").unwrap();
        let (listener, _) = listener();
        let url = format!("http://{}/index", listener.local_addr().unwrap());
        let enabled = Arc::new(AtomicBool::new(true));
        let saved = Arc::clone(&enabled);
        let worker = std::thread::spawn(move || {
            let mut first = accept(&listener);
            wire_request(&mut first, "GET /index HTTP/1.1");
            saved.store(false, Ordering::Release);
            first
                .get_mut()
                .write_all(
                    fixture_http_response("302 Found", "Location: /signed\r\n", "").as_bytes(),
                )
                .unwrap();
            listener
        });
        let authorize = move || {
            if enabled.load(Ordering::Acquire) {
                File::open(&path)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Saved policy changed",
                ))
            }
        };
        let agent = Agent::new_with_config(super::super::super::connection_config(
            Duration::from_secs(5),
        ));
        let result = get(
            &agent,
            &url,
            &AtomicBool::new(false),
            true,
            Some(&authorize),
        );
        let listener = worker.join().unwrap();
        assert_eq!(result.unwrap_err(), "Saved policy changed");
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    fn choice(archive: &[u8]) -> Choice {
        let digest = HashAlgorithm::Sha256
            .digest(archive)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Choice {
            id: "fixture-runtime".into(),
            backend: PackageBackend::Cpu,
            size: archive.len() as u64,
            url: "https://fixtures.example.test/runtime.zip".into(),
            digest,
            entrypoint: "bin/llama-server".into(),
        }
    }

    #[test]
    fn archive_copy_requires_signed_length_and_digest_before_publication() {
        let selected = choice(b"valid package");
        let active = AtomicBool::new(false);
        let mut output = Vec::new();
        let mut progress = None;
        copy_verified(
            &b"valid package"[..],
            &mut output,
            selected.asset(),
            &active,
            |update| progress = Some(update),
        )
        .unwrap();
        assert_eq!(output, b"valid package");
        assert_eq!(progress.unwrap().received, selected.size);
        for invalid in [
            &b"valid"[..],
            &b"invalid input"[..],
            &b"valid package extra"[..],
        ] {
            assert!(
                copy_verified(invalid, &mut Vec::new(), selected.asset(), &active, |_| {}).is_err()
            );
        }
        active.store(true, Ordering::Release);
        let mut untouched = Vec::new();
        assert_eq!(
            copy_verified(
                &b"valid package"[..],
                &mut untouched,
                selected.asset(),
                &active,
                |_| {}
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(untouched.is_empty());
    }

    #[test]
    fn catalog_accepts_only_current_signed_compatible_packages() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let runtime_id = "llama-b12040-kortty1";
        let key = SigningKey::from_bytes(&[0x29; 32]);
        let signed = |date, revoked: bool, selected: Choice| {
            let bytes = serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "generatedAt": date,
                "revokedRuntimeIds": if revoked { vec![runtime_id] } else { Vec::new() },
                "packages": [{
                    "runtimeId": runtime_id, "llamaTag": "b12040", "commit": "00ef136649bd9955bfe11fb9c5a6028aee1a1185",
                    "apiContractVersion": API_CONTRACT_VERSION, "minimumKorttyVersion": COMPATIBILITY_VERSION,
                    "platform": std::env::consts::OS, "architecture": std::env::consts::ARCH, "backend": "cpu",
                    "size": selected.size, "sha256": selected.digest, "downloadUrl": selected.url, "entrypoint": "bin/llama-server"
                }]
            })).unwrap();
            let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
            let index =
                VerifiedIndex::verify_with_key(&bytes, &signature, &key.verifying_key()).unwrap();
            (index, bytes, signature)
        };
        let active = AtomicBool::new(false);
        assert!(install::active_package(&root).unwrap().is_none());
        let initial = choice(b"valid package");
        let (index, bytes, signature) = signed("2026-01-01T00:00:00Z", false, initial.clone());
        let ready = checked_catalog(&root, index, bytes, signature, None, &active).unwrap();
        assert_eq!(ready.choices().len(), 1);
        let preferred = ready.preferred_choice(None).unwrap();
        assert_eq!(preferred.backend, PackageBackend::Cpu);
        assert_eq!(preferred.digest(), initial.digest());
        let (changed, _, _) = signed("2026-01-02T00:00:00Z", false, choice(b"other package"));
        let changed = Choice::from_package(&changed.packages()[0]);
        assert_eq!(preferred.id, changed.id);
        assert_eq!(preferred.backend, changed.backend);
        assert_ne!(*preferred, changed);
        assert!(
            ready
                .preferred_choice(Some(PackageBackend::Vulkan))
                .is_none()
        );
        let (index, bytes, signature) = signed("2026-01-02T00:00:00Z", true, initial);
        let revoked = checked_catalog(&root, index, bytes, signature, None, &active).unwrap();
        assert!(revoked.choices().is_empty());
        assert!(revoked.preferred_choice(None).is_none());
        assert!(
            checked_catalog(
                &root,
                ready.index,
                ready.bytes,
                ready.signature,
                None,
                &active
            )
            .unwrap_err()
            .contains("replay")
        );
    }
}
