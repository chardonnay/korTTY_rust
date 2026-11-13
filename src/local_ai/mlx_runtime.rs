//! Explicit, offline installation of the signed Apple Silicon MLX runtime.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ed25519_dalek::VerifyingKey;

use crate::local_ai::runtime_index::{MAX_INDEX_BYTES, MAX_SIGNATURE_BYTES};
use crate::local_ai::runtime_package::{MAX_ARCHIVE_BYTES, copy_archive, open_input, read_bounded};
use crate::private_files::{FileStamp, require_single_link};

pub(crate) mod background;
mod index;
pub(crate) mod online;
mod store;
#[cfg(test)]
mod tests;

use index::{Index, OsVersion, Package, release_key};
#[cfg(test)]
pub(crate) use store::review_removal as inspect_removal;
pub(crate) use store::{InstallationState, KeyFile, RemovalPlan, Runtime};

#[derive(Clone)]
pub(crate) struct Import {
    pub(crate) index: PathBuf,
    pub(crate) signature: PathBuf,
    pub(crate) archive: PathBuf,
}

#[derive(Clone)]
pub(crate) struct CatalogReview {
    index: Input,
    signature: Input,
    sha256: String,
    revoked_count: usize,
}

impl CatalogReview {
    pub(crate) fn index_sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn revoked_count(&self) -> usize {
        self.revoked_count
    }

    fn reopen(&self, key: &VerifyingKey) -> io::Result<Receipt> {
        let receipt = Receipt::read(&self.index, &self.signature, key)?;
        if receipt.index.sha256 != self.sha256 {
            return Err(invalid("The signed MLX catalog changed after review"));
        }
        Ok(receipt)
    }
}

#[derive(Clone)]
pub(crate) struct Review {
    catalog: CatalogReview,
    archive: Input,
    package: Package,
}

impl Review {
    pub(crate) fn installation_id(&self) -> &str {
        &self.package.installation_id
    }

    pub(crate) fn archive_bytes(&self) -> u64 {
        self.package.size_bytes
    }

    pub(crate) fn archive_sha256(&self) -> &str {
        &self.package.sha256
    }

    pub(crate) fn index_sha256(&self) -> &str {
        self.catalog.index_sha256()
    }

    pub(crate) fn mlx_version(&self) -> &str {
        &self.package.mlx_lm_version
    }
}

pub(crate) fn supported() -> bool {
    host_version().is_ok()
}

pub(crate) fn review(paths: Import, cancelled: &AtomicBool) -> Result<Review, String> {
    review_on(paths, host_version()?, &release_key(), cancelled).map_err(|error| error.to_string())
}

pub(crate) fn review_catalog(
    index: PathBuf,
    signature: PathBuf,
    cancelled: &AtomicBool,
) -> Result<CatalogReview, String> {
    host_version()?;
    check_cancelled(cancelled).map_err(|error| error.to_string())?;
    let (review, _) =
        catalog_review(index, signature, &release_key()).map_err(|error| error.to_string())?;
    check_cancelled(cancelled).map_err(|error| error.to_string())?;
    Ok(review)
}

pub(crate) fn install(
    profile: &Path,
    review: Review,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    let host = host_version()?;
    crate::local_ai::runtime_package::with_mlx_maintenance(profile, || {
        store::install(profile, &review, host, &release_key(), cancelled)
            .map_err(|error| error.to_string())
    })
}

pub(crate) fn accept_catalog(
    profile: &Path,
    review: &CatalogReview,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let host = host_version()?;
    crate::local_ai::runtime_package::with_mlx_maintenance(profile, || {
        store::accept_catalog(profile, review, host, &release_key(), cancelled)
            .map_err(|error| error.to_string())
    })
}

pub(crate) fn installed(profile: &Path) -> io::Result<InstallationState> {
    if !supported() {
        return Ok(InstallationState::default());
    }
    store::installed(profile)
}

pub(crate) fn unpin(
    profile: &Path,
    expected: &str,
    cancelled: &AtomicBool,
) -> Result<InstallationState, String> {
    host_version()?;
    store::unpin(profile, expected, cancelled)
        .map_err(|error| format!("Could not unpin the MLX release ({error})"))
}

pub(crate) fn review_removal(
    profile: &Path,
    cancelled: &AtomicBool,
) -> Result<Option<RemovalPlan>, String> {
    host_version()?;
    store::review_removal(profile, cancelled)
        .map_err(|error| format!("Could not review MLX runtime removal ({error})"))
}

pub(crate) fn remove(
    profile: &Path,
    plan: &RemovalPlan,
    cancelled: &AtomicBool,
) -> Result<Option<String>, String> {
    host_version()?;
    store::uninstall(profile, plan, cancelled)
        .map_err(|error| format!("Could not remove the installed MLX runtime ({error})"))
}

fn review_on(
    paths: Import,
    host: OsVersion,
    key: &VerifyingKey,
    cancelled: &AtomicBool,
) -> io::Result<Review> {
    check_cancelled(cancelled)?;
    let (catalog, receipt) = catalog_review(paths.index, paths.signature, key)?;
    let (archive, mut input) = Input::capture(paths.archive)?;
    let size = input.metadata()?.len();
    if !(1..=MAX_ARCHIVE_BYTES).contains(&size) {
        return Err(invalid("The MLX ZIP is empty or exceeds its size limit"));
    }
    let sha256 = copy_archive(&mut input, &mut io::sink(), size, cancelled)?;
    archive.stamp.require_unchanged(&input)?;
    let package = receipt
        .index
        .select(size, &sha256, host)
        .map_err(io::Error::other)?
        .clone();
    check_cancelled(cancelled)?;
    Ok(Review {
        catalog,
        archive,
        package,
    })
}

fn catalog_review(
    index: PathBuf,
    signature: PathBuf,
    key: &VerifyingKey,
) -> io::Result<(CatalogReview, Receipt)> {
    let (index, index_file) = Input::capture(index)?;
    let (signature, signature_file) = Input::capture(signature)?;
    let receipt = Receipt::from_files(index_file, signature_file, key)?;
    let review = CatalogReview {
        index,
        signature,
        sha256: receipt.index.sha256.clone(),
        revoked_count: receipt.index.revoked.len(),
    };
    Ok((review, receipt))
}

#[derive(Clone)]
struct Input {
    path: PathBuf,
    stamp: FileStamp,
}

impl Input {
    fn capture(path: PathBuf) -> io::Result<(Self, File)> {
        let file = open_input(&path)?;
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        Ok((Self { path, stamp }, file))
    }

    fn reopen(&self) -> io::Result<File> {
        let file = open_input(&self.path)?;
        require_single_link(&file)?;
        self.stamp
            .require_unchanged(&file)
            .map_err(|_| invalid("A reviewed MLX input file changed"))?;
        Ok(file)
    }
}

struct Receipt {
    index: Index,
    bytes: Vec<u8>,
    signature: Vec<u8>,
}

impl Receipt {
    fn read(index: &Input, signature: &Input, key: &VerifyingKey) -> io::Result<Self> {
        Self::from_files(index.reopen()?, signature.reopen()?, key)
    }

    fn from_files(index: File, signature: File, key: &VerifyingKey) -> io::Result<Self> {
        require_single_link(&index)?;
        require_single_link(&signature)?;
        let bytes = read_bounded(index, MAX_INDEX_BYTES as u64)?;
        let signature = read_bounded(signature, MAX_SIGNATURE_BYTES as u64)?;
        Self::from_bytes(bytes, signature, key)
    }

    fn from_bytes(bytes: Vec<u8>, signature: Vec<u8>, key: &VerifyingKey) -> io::Result<Self> {
        let index = Index::verify(&bytes, &signature, key).map_err(io::Error::other)?;
        Ok(Self {
            index,
            bytes,
            signature,
        })
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn host_version() -> Result<OsVersion, String> {
    kortty_macos::operating_system_version()
        .as_deref()
        .and_then(OsVersion::parse)
        .filter(|version| *version >= OsVersion::MINIMUM)
        .ok_or_else(|| "MLX requires Apple Silicon and macOS 14 or later".into())
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn host_version() -> Result<OsVersion, String> {
    Err("MLX requires Apple Silicon and macOS 14 or later".into())
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "MLX operation cancelled",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
