//! Stable Rust desktop release discovery and verified archive downloads.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::{Agent, Body, config::Config, http::Response};
use url::Url;

use crate::clipboard_policy::ClipboardPolicy;
use crate::native_executable::ExecutableTarget;
use crate::private_files::Directory;

mod archive;

pub(crate) const CHANNEL: &str = "chardonnay/korTTY_rust";
const RELEASE: &str = "https://api.github.com/repos/chardonnay/korTTY_rust/releases/latest";
const ASSETS: &str = "https://github.com/chardonnay/korTTY_rust/releases/download/";
const MAX_METADATA: u64 = 512 * 1024;
const MAX_ASSETS: usize = 128;
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) enabled: bool,
    pub(crate) interval_days: u8,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_days: 1,
        }
    }
}

impl Preferences {
    pub(crate) const INTERVAL_DAYS: std::ops::RangeInclusive<u8> = 1..=30;

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn valid(self) -> bool {
        Self::INTERVAL_DAYS.contains(&self.interval_days)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Check {
    NoStableRelease,
    Current,
    NoCompatible(String),
    MissingDigest(String),
    Available(Offer),
}

#[derive(Clone)]
pub(crate) struct Offer {
    version: String,
    name: String,
    size: u64,
    digest: [u8; 32],
    url: Url,
    feed: Option<Arc<Url>>,
    target: ExecutableTarget,
}

impl Offer {
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn source(&self) -> &str {
        self.url.as_str()
    }

    pub(crate) fn checksum(&self) -> String {
        hex(&self.digest)
    }
}

impl fmt::Debug for Offer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Application update offer")
            .field("version", &self.version)
            .field("name", &self.name)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Saved {
    pub(crate) path: PathBuf,
    pub(crate) sync_failed: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Progress {
    pub(crate) received: u64,
    pub(crate) total: u64,
}

pub(crate) struct Pending<T> {
    pub(crate) reply: async_channel::Receiver<Result<T, String>>,
    pub(crate) done: async_channel::Receiver<()>,
    pub(crate) cancel: Cancel,
}

pub(crate) struct Download {
    pub(crate) pending: Pending<Saved>,
    pub(crate) progress: async_channel::Receiver<Progress>,
}

pub(crate) struct Cancel {
    agent: Agent,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.agent.clone().shutdown();
    }
}

pub(crate) fn check() -> Result<Pending<Check>, String> {
    check_authorized(managed_feed)
}

pub(crate) fn parse_feed(value: &str) -> Option<Url> {
    if value.len() > 4096
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return None;
    }
    let url = Url::parse(value).ok()?;
    let (_, authority) = value.split_once("://")?;
    let authority = authority.split(['/', '?', '#']).next()?;
    (matches!(url.scheme(), "http" | "https")
        && !authority.is_empty()
        && !authority.contains('@')
        && url.host_str().is_some()
        && url.fragment().is_none())
    .then_some(url)
}

pub(crate) fn managed_feed() -> Result<Option<Url>, String> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some() || !policy.updates_allowed {
        return Err("Application updates are disabled by managed policy.".into());
    }
    Ok(policy.update_feed)
}

fn authorize_offer(offer: &Offer) -> Result<(), String> {
    if managed_feed()?.as_ref() != offer.feed.as_deref() {
        return Err("The managed application update feed changed. Check for updates again.".into());
    }
    Ok(())
}

pub(crate) fn check_authorized(
    authorize: impl Fn() -> Result<Option<Url>, String> + Send + 'static,
) -> Result<Pending<Check>, String> {
    let target =
        PackageTarget::current().ok_or("There is no native update format for this build.")?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .ok_or("The current application version is invalid.")?;
    start(Duration::from_secs(25), move |agent, cancelled| {
        let feed = authorize()?;
        let default = Url::parse(RELEASE).expect("fixed release endpoint");
        let url = feed.as_ref().unwrap_or(&default);
        let mut response = request(agent, url, feed.is_none(), cancelled)?;
        match response.status().as_u16() {
            404 => return Ok(Check::NoStableRelease),
            200 => {}
            403 | 429 if feed.is_none() => {
                return Err("GitHub limited the update check. Try again later.".into());
            }
            code => return Err(format!("The Rust release channel returned HTTP {code}.")),
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_METADATA + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| network_failure(cancelled))?;
        active(cancelled).map_err(|_| network_failure(cancelled))?;
        if bytes.len() as u64 > MAX_METADATA {
            return Err("The Rust release metadata exceeds its size limit.".into());
        }
        if authorize()? != feed {
            return Err("The managed application update feed changed during the check.".into());
        }
        select(&bytes, current, &target, feed)
    })
}

pub(crate) fn download(offer: Offer, folder: PathBuf) -> Result<Download, String> {
    if !folder.is_absolute()
        || folder.components().any(|part| part == Component::ParentDir)
        || folder.as_os_str().as_encoded_bytes().len() > 4096
    {
        return Err("Enter an existing absolute download directory.".into());
    }
    let (send, progress) = async_channel::bounded(2);
    let pending = start(Duration::from_secs(10 * 60), move |agent, cancelled| {
        publish_archive(
            &offer,
            &folder,
            cancelled,
            || authorize_offer(&offer),
            || asset_response(agent, &offer, cancelled),
            |value| {
                let _ = send.try_send(value);
            },
        )
    })?;
    Ok(Download { pending, progress })
}

fn start<T: Send + 'static>(
    timeout: Duration,
    run: impl FnOnce(&Agent, &AtomicBool) -> Result<T, String> + Send + 'static,
) -> Result<Pending<T>, String> {
    let agent = Agent::new_with_config(
        Config::builder()
            .proxy(None)
            .max_redirects(0)
            .max_response_header_size(16 * 1024)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(timeout))
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::NativeTls)
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build(),
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = Cancel {
        agent: agent.clone(),
        cancelled: Arc::clone(&cancelled),
    };
    let (send, reply) = async_channel::bounded(1);
    let (finished, done) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kortty-application-update".into())
        .spawn(move || {
            let _ = send.try_send(run(&agent, &cancelled));
            drop(finished);
        })
        .map_err(|_| "Could not start the application update worker.")?;
    Ok(Pending {
        reply,
        done,
        cancel,
    })
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Version {
    numbers: [u64; 3],
    stable: bool,
}

impl Version {
    fn parse(input: &str) -> Option<Self> {
        if input.len() > 96 {
            return None;
        }
        let (input, build) = input
            .split_once('+')
            .map_or((input, None), |(core, build)| (core, Some(build)));
        let (core, prerelease) = input
            .split_once('-')
            .map_or((input, None), |(core, prerelease)| (core, Some(prerelease)));
        let identifiers = |value: &str, reject_leading_zero: bool| {
            value.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    && (!reject_leading_zero
                        || part.len() == 1
                        || !part.starts_with('0')
                        || !part.bytes().all(|byte| byte.is_ascii_digit()))
            })
        };
        if build.is_some_and(|value| !identifiers(value, false))
            || prerelease.is_some_and(|value| !identifiers(value, true))
        {
            return None;
        }
        let mut components = core.split('.');
        let mut numbers = [0; 3];
        for number in &mut numbers {
            let part = components.next()?;
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            *number = part.parse().ok()?;
        }
        components.next().is_none().then_some(Self {
            numbers,
            stable: prerelease.is_none(),
        })
    }
}

struct PackageTarget {
    triple: String,
    extension: &'static str,
}

impl PackageTarget {
    fn current() -> Option<Self> {
        let arch = match std::env::consts::ARCH {
            arch @ ("aarch64" | "x86_64") => arch,
            _ => return None,
        };
        let (platform, extension) = if cfg!(all(target_os = "linux", target_env = "gnu")) {
            ("unknown-linux-gnu", "tar.gz")
        } else if cfg!(target_os = "macos") {
            ("apple-darwin", "zip")
        } else if cfg!(all(target_os = "windows", target_env = "msvc")) {
            ("pc-windows-msvc", "zip")
        } else if cfg!(all(
            target_os = "windows",
            target_env = "gnu",
            target_abi = "llvm"
        )) {
            ("pc-windows-gnullvm", "zip")
        } else if cfg!(all(
            target_os = "windows",
            target_env = "gnu",
            target_arch = "x86_64"
        )) {
            ("pc-windows-gnu", "zip")
        } else {
            return None;
        };
        Some(Self {
            triple: format!("{arch}-{platform}"),
            extension,
        })
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
    state: String,
}

fn select(
    bytes: &[u8],
    current: Version,
    target: &PackageTarget,
    feed: Option<Url>,
) -> Result<Check, String> {
    let release: Release =
        serde_json::from_slice(bytes).map_err(|_| "The Rust release metadata is invalid.")?;
    if release.assets.len() > MAX_ASSETS {
        return Err("The Rust release lists too many assets.".into());
    }
    if release.draft || release.prerelease {
        return Ok(Check::NoStableRelease);
    }
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let latest = Version::parse(version).ok_or("The Rust release version is invalid.")?;
    if !latest.stable {
        return Ok(Check::NoStableRelease);
    }
    if latest <= current {
        return Ok(Check::Current);
    }
    let name = format!("kortty-{version}-{}.{}", target.triple, target.extension);
    let mut matches = release
        .assets
        .into_iter()
        .filter(|asset| asset.name == name);
    let Some(asset) = matches.next() else {
        return Ok(Check::NoCompatible(version.into()));
    };
    if matches.next().is_some() {
        return Err("The Rust release lists duplicate packages for this target.".into());
    }
    if asset.state != "uploaded" || !(1..=MAX_ARCHIVE).contains(&asset.size) {
        return Err("The Rust update archive is incomplete or outside its size limit.".into());
    }
    let mut url = Url::parse(ASSETS).expect("fixed package origin");
    url.path_segments_mut()
        .expect("hierarchical package origin")
        .pop_if_empty()
        .extend([release.tag_name.as_str(), name.as_str()]);
    if asset.browser_download_url != url.as_str() {
        url = parse_feed(&asset.browser_download_url)
            .filter(|asset| managed_destination(feed.as_ref(), asset))
            .ok_or("The Rust update package is outside the approved release destination.")?;
    }
    let Some(digest) = asset.digest.as_deref().and_then(digest) else {
        return Ok(Check::MissingDigest(version.into()));
    };
    Ok(Check::Available(Offer {
        version: version.into(),
        name,
        size: asset.size,
        digest,
        url,
        feed: feed.map(Arc::new),
        target: ExecutableTarget::parse(&target.triple)
            .map_err(|_| "There is no native update format for this build.")?,
    }))
}

fn digest(value: &str) -> Option<[u8; 32]> {
    let value = value.strip_prefix("sha256:")?;
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(result)
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn request(
    agent: &Agent,
    url: &Url,
    metadata: bool,
    cancelled: &AtomicBool,
) -> Result<Response<Body>, String> {
    active(cancelled).map_err(|_| network_failure(cancelled))?;
    let mut request = agent
        .get(url.as_str())
        .header("User-Agent", "kortty-rust-updater")
        .header("Accept-Encoding", "identity");
    if metadata {
        request = request
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
    }
    request
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|_| network_failure(cancelled))
}

fn asset_response(
    agent: &Agent,
    offer: &Offer,
    cancelled: &AtomicBool,
) -> Result<Response<Body>, String> {
    let start = &offer.url;
    let mirror = managed_destination(offer.feed.as_deref(), start);
    let mut current = start.clone();
    for hops in 0..=MAX_REDIRECTS {
        authorize_offer(offer)?;
        let response = request(agent, &current, false, cancelled)?;
        let code = response.status().as_u16();
        if code == 200 {
            return Ok(response);
        }
        if !matches!(code, 301 | 302 | 303 | 307 | 308) {
            return Err(format!("The Rust update download returned HTTP {code}."));
        }
        if hops == MAX_REDIRECTS {
            return Err("The Rust update redirected too many times.".into());
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .filter(|value| {
                value.len() <= 4096
                    && !value
                        .bytes()
                        .any(|byte| byte.is_ascii_control() || byte == b'\\')
            })
            .ok_or("The Rust update supplied an invalid redirect.")?;
        let next = current
            .join(location)
            .map_err(|_| "The Rust update supplied an invalid redirect.")?;
        let allowed = if mirror {
            parse_feed(next.as_str()).is_some() && managed_destination(offer.feed.as_deref(), &next)
        } else {
            redirect_allowed(start, &current, &next)
        };
        if !allowed {
            return Err(
                "The Rust update redirected outside the approved release destination.".into(),
            );
        }
        current = next;
    }
    unreachable!("bounded redirect loop")
}

fn managed_destination(feed: Option<&Url>, asset: &Url) -> bool {
    asset.scheme() == "https" && feed.is_some_and(|feed| feed.origin() == asset.origin())
}

fn redirect_allowed(start: &Url, current: &Url, next: &Url) -> bool {
    if next.scheme() != "https"
        || next.port().is_some()
        || !next.username().is_empty()
        || next.password().is_some()
        || next.fragment().is_some()
    {
        return false;
    }
    let cdn = |url: &Url| {
        matches!(
            url.host_str(),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
        )
    };
    if current.origin() == start.origin() {
        next == start || cdn(next)
    } else {
        cdn(current) && next.origin() == current.origin()
    }
}

fn publish_archive(
    offer: &Offer,
    folder: &Path,
    cancelled: &AtomicBool,
    authorize: impl Fn() -> Result<(), String>,
    open: impl FnOnce() -> Result<Response<Body>, String>,
    progress: impl FnMut(Progress),
) -> Result<Saved, String> {
    let destination = Directory::open(folder, false)
        .map_err(|_| "Could not open the selected download directory.")?;
    if destination
        .has_entry(&offer.name)
        .map_err(|_| "Could not check the destination archive path.")?
    {
        return Err("An archive with this name already exists in the selected directory.".into());
    }
    destination
        .publish_checked_readable(
            &offer.name,
            |file| {
                active(cancelled)?;
                authorize().map_err(io::Error::other)?;
                let mut response = open().map_err(io::Error::other)?;
                let headers = response.headers();
                if response.status().as_u16() != 200
                    || headers.get_all("content-length").iter().count() > 1
                    || headers.get_all("content-encoding").iter().count() > 1
                    || headers.get("content-length").is_some_and(|length| {
                        length
                            .to_str()
                            .ok()
                            .and_then(|text| text.parse::<u64>().ok())
                            != Some(offer.size)
                    })
                    || headers.get("content-encoding").is_some_and(|encoding| {
                        !encoding.as_bytes().eq_ignore_ascii_case(b"identity")
                    })
                    || headers.contains_key("content-range")
                {
                    return Err(io::Error::other(
                        "The download does not match the reviewed archive size or encoding",
                    ));
                }
                copy_archive(
                    response.body_mut().as_reader(),
                    file,
                    offer,
                    cancelled,
                    progress,
                )?;
                archive::verify(file, offer, cancelled)
            },
            || {
                active(cancelled)?;
                authorize().map_err(io::Error::other)?;
                if !destination.same_as(&Directory::open(folder, false)?)? {
                    return Err(io::Error::other("The selected download directory changed"));
                }
                active(cancelled)
            },
        )
        .map_err(|error| match error.kind() {
            _ if cancelled.load(Ordering::Acquire) => "The application update was canceled.".into(),
            io::ErrorKind::AlreadyExists => {
                "An archive with this name already exists in the selected directory.".into()
            }
            _ => format!("Could not save the verified update archive ({error})."),
        })?;
    Ok(Saved {
        path: folder.join(&offer.name),
        sync_failed: destination.sync().is_err(),
    })
}

fn copy_archive(
    mut input: impl Read,
    output: &mut impl Write,
    offer: &Offer,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> io::Result<()> {
    let magic: &[u8] = if offer.name.ends_with(".zip") {
        b"PK\x03\x04"
    } else {
        b"\x1f\x8b\x08"
    };
    if offer.size < magic.len() as u64 {
        return Err(io::Error::other("The reviewed archive size is invalid"));
    }
    let mut head = [0; 4];
    input
        .read_exact(&mut head[..magic.len()])
        .map_err(|_| io::Error::other("The update archive ended before its header"))?;
    if &head[..magic.len()] != magic {
        return Err(io::Error::other(
            "The download has the wrong archive format",
        ));
    }
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    output.write_all(magic)?;
    hash.update(magic);
    let mut received = magic.len() as u64;
    let mut emitted = Instant::now();
    let mut buffer = vec![0; 64 * 1024];
    while received < offer.size {
        active(cancelled)?;
        let limit = (offer.size - received).min(buffer.len() as u64) as usize;
        let count = input
            .read(&mut buffer[..limit])
            .map_err(|_| io::Error::other("The update download was interrupted"))?;
        if count == 0 {
            return Err(io::Error::other(
                "The update archive ended before its reviewed size",
            ));
        }
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        received += count as u64;
        if received == offer.size || emitted.elapsed() >= Duration::from_millis(100) {
            progress(Progress {
                received,
                total: offer.size,
            });
            emitted = Instant::now();
        }
    }
    active(cancelled)?;
    if input
        .read(&mut buffer[..1])
        .map_err(|_| io::Error::other("The update download was interrupted"))?
        != 0
    {
        return Err(io::Error::other(
            "The update archive exceeds its reviewed size",
        ));
    }
    if hash.finalize().as_ref() != offer.digest {
        return Err(io::Error::other(
            "SHA-256 verification failed for the update archive",
        ));
    }
    Ok(())
}

fn active(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}

fn network_failure(cancelled: &AtomicBool) -> String {
    if cancelled.load(Ordering::Acquire) {
        "The application update was canceled.".into()
    } else {
        "Could not reach the Rust release channel. Try again when online.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const TAG: &str = "v0.2.0";
    const ARCHIVE: &str = "kortty-0.2.0-aarch64-pc-windows-msvc.zip";

    fn target() -> PackageTarget {
        PackageTarget {
            triple: "aarch64-pc-windows-msvc".into(),
            extension: "zip",
        }
    }

    fn asset(name: &str, checksum: Option<&str>) -> Value {
        json!({
            "name": name, "size": 4096, "state": "uploaded", "digest": checksum,
            "browser_download_url": format!("{ASSETS}{TAG}/{name}")
        })
    }

    fn release(tag: &str, assets: Vec<Value>) -> Value {
        json!({"tag_name": tag, "draft": false, "prerelease": false, "assets": assets})
    }

    fn check_fixture(release: Value) -> Result<Check, String> {
        select(
            &serde_json::to_vec(&release).unwrap(),
            Version::parse("0.1.0").unwrap(),
            &target(),
            None,
        )
    }

    #[test]
    fn managed_feed_requires_a_digest_and_an_approved_https_archive_origin() {
        let checksum = format!("sha256:{}", "ab".repeat(32));
        let check = |feed, destination, digest| {
            let mut package = asset(ARCHIVE, digest);
            package["browser_download_url"] = json!(destination);
            select(
                &serde_json::to_vec(&release(TAG, vec![package])).unwrap(),
                Version::parse("0.1.0").unwrap(),
                &target(),
                parse_feed(feed),
            )
        };
        let https = "https://mirror.example.test/latest";
        let archive = "https://mirror.example.test/archives/native.zip";
        assert!(
            matches!(check(https, archive, Some(&checksum)).unwrap(), Check::Available(offer) if offer.source() == archive)
        );
        assert!(matches!(
            check(https, archive, None).unwrap(),
            Check::MissingDigest(_)
        ));
        assert!(
            check(
                https,
                "https://other.example.test/native.zip",
                Some(&checksum)
            )
            .is_err()
        );
        assert!(
            check(
                "http://mirror.example.test/latest",
                "http://mirror.example.test/native.zip",
                Some(&checksum)
            )
            .is_err()
        );
        assert!(matches!(
            check(
                "http://mirror.example.test/latest",
                &format!("{ASSETS}{TAG}/{ARCHIVE}"),
                Some(&checksum)
            )
            .unwrap(),
            Check::Available(_)
        ));
    }

    #[test]
    fn update_worker_queries_the_managed_endpoint_and_rechecks_it_before_accepting() {
        use crate::local_ai::test_support::{accept, listener, wire_request};
        use std::sync::atomic::AtomicUsize;

        let (listener, _) = listener();
        let feed = Url::parse(&format!(
            "http://{}/managed/latest",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            wire_request(&mut stream, "GET /managed/latest HTTP/1.1");
            let body = release("v0.0.1", vec![]).to_string();
            write!(
                stream.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let checks = AtomicUsize::new(0);
        let pending = check_authorized(move || {
            Ok((checks.fetch_add(1, Ordering::Relaxed) == 0).then(|| feed.clone()))
        })
        .unwrap();
        let result = pending.reply.recv_blocking().unwrap();
        let _ = pending.done.recv_blocking();
        worker.join().unwrap();
        assert!(
            result
                .unwrap_err()
                .contains("feed changed during the check")
        );
    }

    fn fixture_zip(path: &str, executable: &[u8]) -> Vec<u8> {
        use std::io::Cursor;
        use zip::write::SimpleFileOptions;

        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file(path, SimpleFileOptions::default().unix_permissions(0o755))
            .unwrap();
        archive.write_all(executable).unwrap();
        archive.finish().unwrap().into_inner()
    }

    fn fixture_pe(machine: u16) -> Vec<u8> {
        let mut header = vec![0; 90];
        header[..2].copy_from_slice(b"MZ");
        header[60..64].copy_from_slice(&64u32.to_le_bytes());
        header[64..68].copy_from_slice(b"PE\0\0");
        header[68..70].copy_from_slice(&machine.to_le_bytes());
        header[86..88].copy_from_slice(&2u16.to_le_bytes());
        header[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        header
    }

    #[test]
    fn linux_and_macos_archives_use_the_shared_native_executable_headers() {
        use std::io::Cursor;

        let target_offer = |triple, suffix| Offer {
            version: "0.2.0".into(),
            name: format!("kortty-0.2.0-{triple}.{suffix}"),
            size: 0,
            digest: [0; 32],
            url: Url::parse(RELEASE).unwrap(),
            feed: None,
            target: ExecutableTarget::parse(triple).unwrap(),
        };
        let mut linux = target_offer("x86_64-unknown-linux-gnu", "tar.gz");
        let mut elf = [0; 64];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[16..18].copy_from_slice(&3u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(gzip);
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(elf.len() as u64);
        let linux_stem = linux.name.strip_suffix(".tar.gz").unwrap();
        archive
            .append_data(
                &mut header,
                format!("{linux_stem}/bin/kortty"),
                elf.as_slice(),
            )
            .unwrap();
        let linux_archive = archive.into_inner().unwrap().finish().unwrap();
        linux.size = linux_archive.len() as u64;
        let cancel = AtomicBool::new(false);
        archive::verify(&mut Cursor::new(linux_archive), &linux, &cancel).unwrap();

        let mut macos = target_offer("aarch64-apple-darwin", "zip");
        let mut macho = [0; 64];
        macho[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
        macho[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        macho[12..16].copy_from_slice(&2u32.to_le_bytes());
        let macos_stem = macos.name.strip_suffix(".zip").unwrap();
        let executable = format!("{macos_stem}/kortty.app/Contents/MacOS/kortty");
        let macos_archive = fixture_zip(&executable, &macho);
        macos.size = macos_archive.len() as u64;
        archive::verify(&mut Cursor::new(macos_archive), &macos, &cancel).unwrap();
        macho[4..8].copy_from_slice(&0x0100_0007u32.to_le_bytes());
        let wrong_macos = fixture_zip(&executable, &macho);
        macos.size = wrong_macos.len() as u64;
        assert!(
            archive::verify(&mut Cursor::new(wrong_macos), &macos, &cancel)
                .unwrap_err()
                .to_string()
                .contains("wrong native architecture")
        );
    }

    #[test]
    fn update_selection_uses_the_exact_rust_archive_and_requires_the_github_digest() {
        let sha = format!("sha256:{}", "ab".repeat(32));
        let wrong = vec![
            asset("korTTY-java-0.2.0.zip", Some(&sha)),
            asset("kortty-0.2.0-x86_64-pc-windows-msvc.zip", Some(&sha)),
            asset("kortty-0.2.0-aarch64-pc-windows-gnullvm.zip", Some(&sha)),
            asset(
                "kortty-0.2.0-aarch64-pc-windows-msvc.zip.sha256",
                Some(&sha),
            ),
        ];
        assert!(
            matches!(check_fixture(release(TAG, wrong.clone())).unwrap(), Check::NoCompatible(version) if version == "0.2.0")
        );
        assert!(matches!(
            check_fixture(release(TAG, vec![asset(ARCHIVE, None)])).unwrap(),
            Check::MissingDigest(_)
        ));
        assert!(matches!(
            check_fixture(release(TAG, vec![asset(ARCHIVE, Some("sha256:bad"))])).unwrap(),
            Check::MissingDigest(_)
        ));
        let mut assets = wrong;
        assets.push(asset(ARCHIVE, Some(&sha)));
        let Check::Available(offer) = check_fixture(release(TAG, assets)).unwrap() else {
            panic!("expected the exact Rust package");
        };
        assert_eq!(offer.name(), ARCHIVE);
        assert_eq!(offer.checksum(), "ab".repeat(32));
        assert_eq!(offer.source(), format!("{ASSETS}{TAG}/{ARCHIVE}"));
        let mut foreign = asset(ARCHIVE, Some(&sha));
        foreign["browser_download_url"] = json!(format!(
            "https://github.com/chardonnay/korTTY/releases/download/{TAG}/{ARCHIVE}"
        ));
        assert!(check_fixture(release(TAG, vec![foreign])).is_err());
        assert!(check_fixture(release(TAG, vec![asset(ARCHIVE, Some(&sha)); 2])).is_err());
    }

    #[test]
    fn application_update_rejects_nonstable_and_invalid_versions() {
        assert!(matches!(
            check_fixture(release("v0.1.0", vec![])).unwrap(),
            Check::Current
        ));
        assert!(matches!(
            check_fixture(release("0.0.9", vec![])).unwrap(),
            Check::Current
        ));
        assert!(matches!(
            check_fixture(release("v0.2.0-rc.1", vec![])).unwrap(),
            Check::NoStableRelease
        ));
        let mut draft = release(TAG, vec![]);
        draft["draft"] = json!(true);
        assert!(matches!(
            check_fixture(draft).unwrap(),
            Check::NoStableRelease
        ));
        for invalid in ["v1.02.3", "v1.2", "v1.2.3+", "1.2.3/other", "1.2.3-01"] {
            assert!(check_fixture(release(invalid, vec![])).is_err());
        }
        assert!(Version::parse("1.2.3").unwrap() > Version::parse("1.2.3-rc.1").unwrap());
    }

    #[test]
    fn application_update_redirects_remain_on_the_exact_asset_and_github_cdn() {
        let source = Url::parse(&format!("{ASSETS}{TAG}/{ARCHIVE}")).unwrap();
        let cdn =
            Url::parse("https://release-assets.githubusercontent.com/asset?temporary=fixture")
                .unwrap();
        assert!(redirect_allowed(&source, &source, &cdn));
        assert!(redirect_allowed(
            &source,
            &cdn,
            &cdn.join("another-part").unwrap()
        ));
        assert!(!redirect_allowed(&source, &cdn, &source));
        for url in [
            "http://release-assets.githubusercontent.com/asset",
            "https://release-assets.githubusercontent.com:444/asset",
            "https://release-assets.githubusercontent.com.evil.test/asset",
            "https://github.com/chardonnay/korTTY_rust/releases/download/v0.2.1/another.zip",
            "https://github.com/chardonnay/korTTY/releases/download/v0.2.0/archive.zip",
            "https://user@release-assets.githubusercontent.com/asset",
        ] {
            assert!(
                !redirect_allowed(&source, &source, &Url::parse(url).unwrap()),
                "{url}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn application_update_refuses_a_shared_writable_destination_before_connecting() {
        use std::os::unix::fs::PermissionsExt as _;

        let profile = crate::snippets::tests::Profile::new();
        let folder = profile.0.join("shared");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o770)).unwrap();
        let folder = folder.canonicalize().unwrap();
        let offer = Offer {
            version: "0.2.0".into(),
            name: ARCHIVE.into(),
            size: 64,
            digest: [0; 32],
            url: Url::parse(RELEASE).unwrap(),
            feed: None,
            target: ExecutableTarget::parse(&target().triple).unwrap(),
        };
        let result = publish_archive(
            &offer,
            &folder,
            &AtomicBool::new(false),
            || Ok(()),
            || panic!("a shared writable directory must be rejected before connecting"),
            |_| {},
        );
        assert!(result.unwrap_err().contains("owner-controlled"));
        assert_eq!(std::fs::read_dir(folder).unwrap().count(), 0);
    }

    #[test]
    fn verified_application_download_is_atomic_and_never_replaces_a_racing_file() {
        use crate::local_ai::test_support::{accept, listener, wire_request};

        let stem = ARCHIVE.strip_suffix(".zip").unwrap();
        let path = format!("{stem}/kortty.exe");
        let data = fixture_zip(&path, &fixture_pe(0xaa64));
        let wrong_architecture = fixture_zip(&path, &fixture_pe(0x8664));
        let sha256 = |bytes: &[u8]| -> [u8; 32] {
            HashAlgorithm::Sha256
                .digest(bytes)
                .unwrap()
                .try_into()
                .unwrap()
        };
        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.canonicalize().unwrap();
        let destination = directory.join(ARCHIVE);
        let (listener, _) = listener();
        let url = Url::parse(&format!(
            "http://{}/archive",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let responses = [
            data.clone(),
            data.clone(),
            wrong_architecture.clone(),
            data.clone(),
            data.clone(),
            data.clone(),
        ];
        let worker = std::thread::spawn(move || {
            for bytes in responses {
                let mut stream = accept(&listener);
                wire_request(&mut stream, "GET /archive HTTP/1.1");
                write!(
                    stream.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.get_mut().write_all(&bytes).unwrap();
            }
        });
        let mut offer = Offer {
            version: "0.2.0".into(),
            name: ARCHIVE.into(),
            size: data.len() as u64,
            digest: sha256(&data),
            url: url.clone(),
            feed: None,
            target: ExecutableTarget::parse(&target().triple).unwrap(),
        };
        let cancel = AtomicBool::new(false);
        let agent = Agent::new_with_config(
            Config::builder()
                .proxy(None)
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let open = || request(&agent, &url, false, &cancel);
        let authorized = || Ok(());
        let checksum = offer.digest;
        offer.digest = [0; 32];
        assert!(
            publish_archive(&offer, &directory, &cancel, authorized, open, |_| {})
                .unwrap_err()
                .contains("SHA-256")
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        offer.digest = checksum;
        let collision = publish_archive(
            &offer,
            &directory,
            &cancel,
            authorized,
            || {
                std::fs::write(&destination, b"existing").unwrap();
                open()
            },
            |_| {},
        );
        assert!(collision.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_file(&destination).unwrap();
        offer.size = wrong_architecture.len() as u64;
        offer.digest = sha256(&wrong_architecture);
        assert!(
            publish_archive(&offer, &directory, &cancel, authorized, open, |_| {})
                .unwrap_err()
                .contains("wrong native architecture")
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        offer.size = data.len() as u64;
        offer.digest = checksum;
        cancel.store(true, Ordering::Release);
        assert!(
            publish_archive(
                &offer,
                &directory,
                &cancel,
                authorized,
                || panic!("canceled update must not connect"),
                |_| {}
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        cancel.store(false, Ordering::Release);
        let revoked = std::cell::Cell::new(false);
        let denied = publish_archive(
            &offer,
            &directory,
            &cancel,
            || {
                if revoked.get() {
                    Err("fixture policy revoked".into())
                } else {
                    Ok(())
                }
            },
            || {
                revoked.set(true);
                open()
            },
            |_| {},
        );
        assert!(denied.unwrap_err().contains("fixture policy revoked"));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        let downloaded = std::cell::Cell::new(false);
        let canceled = publish_archive(
            &offer,
            &directory,
            &cancel,
            || {
                if downloaded.get() {
                    cancel.store(true, Ordering::Release);
                }
                Ok(())
            },
            || {
                downloaded.set(true);
                open()
            },
            |_| {},
        );
        assert!(canceled.unwrap_err().contains("canceled"));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        cancel.store(false, Ordering::Release);
        let saved = publish_archive(&offer, &directory, &cancel, authorized, open, |_| {}).unwrap();
        assert_eq!(saved.path, destination);
        assert_eq!(std::fs::read(saved.path).unwrap(), data);
        worker.join().unwrap();
    }
}
