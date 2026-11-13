//! Explicit acquisition from the signed stable MLX release channel.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ed25519_dalek::VerifyingKey;
use ureq::Agent;

use crate::local_ai::runtime_package::{
    Pending,
    online::{VerifiedAsset, download_verified_asset, fetch_release_metadata},
    work_with_agent,
};

use super::{
    InstallationState, MAX_INDEX_BYTES, MAX_SIGNATURE_BYTES, OsVersion, Package, Receipt,
    check_cancelled, host_version, release_key, store,
};

const INDEX_URL: &str = "https://github.com/chardonnay/kortty-llama-runtimes/releases/latest/download/mlx-runtime-index-v1.json";
const SIGNATURE_URL: &str = "https://github.com/chardonnay/kortty-llama-runtimes/releases/latest/download/mlx-runtime-index-v1.sig";
const LEGACY_INDEX_URL: &str = "https://github.com/chardonnay/kortty-llama-runtimes/releases/download/mlx-stable/mlx-runtime-index-v1.json";
const LEGACY_SIGNATURE_URL: &str = "https://github.com/chardonnay/kortty-llama-runtimes/releases/download/mlx-stable/mlx-runtime-index-v1.sig";

pub(crate) struct Catalog {
    receipt: Receipt,
    key: VerifyingKey,
    choices: Vec<Choice>,
}

impl Catalog {
    fn review(
        profile: &Path,
        receipt: Receipt,
        host: OsVersion,
        key: VerifyingKey,
    ) -> io::Result<Self> {
        let choices = store::compatible_packages(profile, &receipt.index, host)?
            .into_iter()
            .map(Choice::from)
            .collect();
        Ok(Self {
            receipt,
            key,
            choices,
        })
    }

    pub(crate) fn choices(&self) -> &[Choice] {
        &self.choices
    }

    pub(crate) fn pins(&self, choice: &Choice) -> bool {
        self.choices
            .first()
            .is_some_and(|newest| newest.runtime_id != choice.runtime_id)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Choice {
    pub(crate) id: String,
    pub(crate) runtime_id: String,
    version: String,
    pub(crate) size: u64,
    pub(crate) url: String,
    pub(crate) sha256: String,
}

impl From<&Package> for Choice {
    fn from(package: &Package) -> Self {
        Self {
            id: package.installation_id.clone(),
            runtime_id: package.runtime_id.clone(),
            version: package.mlx_lm_version.clone(),
            size: package.size_bytes,
            url: package.download_url.clone(),
            sha256: package.sha256.clone(),
        }
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} · mlx-lm {}", self.id, self.version)
    }
}

pub(crate) fn check(profile: PathBuf) -> Result<Pending<Arc<Catalog>>, String> {
    let host = host_version()?;
    let key = release_key();
    let agent = Agent::new_with_config(crate::local_ai::connection_config(Duration::from_secs(30)));
    work_with_agent(Some(agent.clone()), move |cancelled| {
        let receipt = fetch_catalog(&key, |url, limit, allow_missing| {
            fetch_release_metadata(&agent, url, limit, allow_missing, None, cancelled)
        })?;
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        crate::clipboard_policy::ClipboardPolicy::load()
            .require_ai_runtime_downloads()
            .map_err(|error| error.to_string())?;
        Catalog::review(&profile, receipt, host, key)
            .map(Arc::new)
            .map_err(|error| error.to_string())
    })
}

pub(super) fn fetch_catalog(
    key: &VerifyingKey,
    mut fetch: impl FnMut(&str, usize, bool) -> Result<Option<Vec<u8>>, String>,
) -> Result<Receipt, String> {
    let (bytes, signature_url) = match fetch(INDEX_URL, MAX_INDEX_BYTES, true)? {
        Some(bytes) => (bytes, SIGNATURE_URL),
        None => (
            fetch(LEGACY_INDEX_URL, MAX_INDEX_BYTES, false)?
                .ok_or("The signed stable MLX catalog is unavailable.")?,
            LEGACY_SIGNATURE_URL,
        ),
    };
    let signature = fetch(signature_url, MAX_SIGNATURE_BYTES, false)?
        .ok_or("The MLX catalog signature is unavailable.")?;
    Receipt::from_bytes(bytes, signature, key).map_err(|error| error.to_string())
}

#[derive(Clone)]
pub(crate) struct InstallOutcome {
    pub(crate) runtime: InstallationState,
    pub(crate) activated: bool,
}

pub(crate) fn download(
    profile: PathBuf,
    catalog: Arc<Catalog>,
    choice: Choice,
) -> Result<Pending<InstallOutcome>, String> {
    let host = host_version()?;
    let agent = Agent::new_with_config(crate::local_ai::connection_config(Duration::from_secs(
        2 * 60 * 60,
    )));
    work_with_agent(Some(agent.clone()), move |cancelled| {
        crate::clipboard_policy::ClipboardPolicy::load()
            .require_ai_runtime_downloads()
            .map_err(|error| error.to_string())?;
        download_on(
            &profile,
            &catalog,
            &choice,
            host,
            true,
            cancelled,
            |asset, file| download_verified_asset(&agent, asset, file, None, cancelled),
        )
    })
}

fn download_on(
    profile: &Path,
    catalog: &Catalog,
    choice: &Choice,
    host: OsVersion,
    requires_download_policy: bool,
    cancelled: &AtomicBool,
    transfer: impl FnOnce(VerifiedAsset<'_>, &mut File) -> io::Result<()>,
) -> Result<InstallOutcome, String> {
    check_cancelled(cancelled).map_err(|error| error.to_string())?;
    if !catalog.choices.contains(choice) {
        return Err("Select a package from the verified MLX catalog.".into());
    }
    let choices = store::compatible_packages(profile, &catalog.receipt.index, host)
        .map_err(|error| error.to_string())?;
    let package = choices
        .iter()
        .copied()
        .find(|package| package.installation_id == choice.id)
        .ok_or("The reviewed MLX package is no longer available.")?;
    let pin = catalog.pins(choice);
    if choices
        .first()
        .map(|newest| newest.runtime_id != choice.runtime_id)
        != Some(pin)
    {
        return Err(
            "The signed MLX version choices changed. Check and review the release again.".into(),
        );
    }
    let selection = store::SelectedVersion {
        host,
        pin,
        requires_download_policy,
    };
    if let Some(runtime) = store::select_installed(
        profile,
        &catalog.receipt,
        package,
        &catalog.key,
        selection,
        cancelled,
    )
    .map_err(|error| error.to_string())?
    {
        return Ok(InstallOutcome {
            runtime,
            activated: false,
        });
    }
    let asset = VerifiedAsset {
        url: &package.download_url,
        size: package.size_bytes,
        sha256: &package.sha256,
    };
    let id = store::install_download(
        profile,
        &catalog.receipt,
        package,
        selection,
        cancelled,
        |file| transfer(asset, file),
    )
    .map_err(|error| format!("Could not download and install the MLX runtime ({error})"))?;
    Ok(InstallOutcome {
        runtime: InstallationState::downloaded(id, package, pin),
        activated: true,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::atomic::Ordering;

    use ed25519_dalek::SigningKey;
    use serde_json::json;

    use super::super::tests::{archive, descriptor, write_catalog};
    use super::super::{Runtime, catalog_review};
    use super::*;

    #[test]
    fn mlx_online_catalog_falls_back_only_when_the_primary_index_is_missing() {
        let profile = crate::snippets::tests::Profile::new();
        let key = SigningKey::from_bytes(&[21; 32]);
        let (index, signature) = write_catalog(&profile.0, "signed", 2, &[], &[], &key);
        let bytes = std::fs::read(index).unwrap();
        let signature = std::fs::read(signature).unwrap();
        let mut urls = Vec::new();
        let result = fetch_catalog(&key.verifying_key(), |url, limit, allow_missing| {
            urls.push(url.to_owned());
            assert_eq!(allow_missing, url == INDEX_URL);
            match url {
                INDEX_URL => Ok(None),
                LEGACY_INDEX_URL => {
                    assert_eq!(limit, MAX_INDEX_BYTES);
                    Ok(Some(bytes.clone()))
                }
                LEGACY_SIGNATURE_URL => {
                    assert_eq!(limit, MAX_SIGNATURE_BYTES);
                    Ok(Some(signature.clone()))
                }
                _ => panic!("unapproved catalog fallback request"),
            }
        })
        .unwrap();
        assert_eq!(result.bytes, bytes);
        assert_eq!(urls, [INDEX_URL, LEGACY_INDEX_URL, LEGACY_SIGNATURE_URL]);

        for primary_signature in [None, Some(b"invalid signature".to_vec())] {
            assert!(
                fetch_catalog(&key.verifying_key(), |url, _, _| match url {
                    INDEX_URL => Ok(Some(bytes.clone())),
                    SIGNATURE_URL => Ok(primary_signature.clone()),
                    _ => panic!("a primary signature error cannot authorize fallback"),
                })
                .is_err()
            );
        }
        assert!(
            fetch_catalog(&key.verifying_key(), |url, _, _| {
                assert_eq!(url, INDEX_URL);
                Err("network unavailable".into())
            })
            .is_err()
        );
    }

    #[test]
    fn mlx_online_download_activates_only_the_reviewed_signed_package() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let key = SigningKey::from_bytes(&[22; 32]);
        let public = key.verifying_key();
        let host = OsVersion::parse("15.1").unwrap();
        let cancelled = AtomicBool::new(false);
        let first = "mlx-0.9.0-kortty1-macos-aarch64";
        let latest = "mlx-0.12.0-kortty1-macos-aarch64";
        let withdrawn = "mlx-0.13.0-kortty1-macos-aarch64";
        let future = "mlx-0.14.0-kortty1-macos-aarch64";
        let packages = [
            (first, "0.9.0"),
            (latest, "0.12.0"),
            (withdrawn, "0.13.0"),
            (future, "0.14.0"),
        ];
        let packages: Vec<_> = packages
            .into_iter()
            .map(|(id, version)| {
                let zip = archive(id, true, 0x0100_000c, None);
                let mut descriptor = descriptor(id, &zip);
                descriptor["mlxLmVersion"] = json!(version);
                if id == future {
                    descriptor["minimumOsVersion"] = json!("99.0");
                }
                (descriptor, zip)
            })
            .collect();
        let descriptors: Vec<_> = packages
            .iter()
            .map(|(descriptor, _)| descriptor.clone())
            .collect();
        let (index, signature) =
            write_catalog(&root, "current", 3, &descriptors, &[withdrawn], &key);
        let receipt = Receipt::from_bytes(
            std::fs::read(index).unwrap(),
            std::fs::read(signature).unwrap(),
            &public,
        )
        .unwrap();
        let catalog = Catalog::review(&root, receipt, host, public).unwrap();
        let ids: Vec<_> = catalog
            .choices()
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, [latest, first]);
        assert!(!root.join("mlx-runtime").exists());
        let choice = &catalog.choices()[0];
        let mut changed = choice.clone();
        changed.url = "https://example.invalid/unreviewed.zip".into();
        assert!(
            download_on(
                &root,
                &catalog,
                &changed,
                host,
                false,
                &cancelled,
                |_, _| { panic!("an unreviewed download must never open") }
            )
            .is_err()
        );

        let zip = &packages[1].1;
        let downloads = root.join("mlx-runtime/downloads");
        let stage_is_clean = || {
            std::fs::read_dir(&downloads)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == "download.lock")
        };
        assert!(
            download_on(
                &root,
                &catalog,
                choice,
                host,
                false,
                &cancelled,
                |_, file| { file.write_all(&vec![0; zip.len()]) }
            )
            .is_err()
        );
        assert!(stage_is_clean());
        assert!(store::active(&root).unwrap().is_none());
        assert!(
            download_on(
                &root,
                &catalog,
                choice,
                host,
                false,
                &cancelled,
                |_, file| {
                    file.write_all(zip)?;
                    cancelled.store(true, Ordering::Release);
                    Ok(())
                }
            )
            .is_err()
        );
        cancelled.store(false, Ordering::Release);
        assert!(stage_is_clean());
        assert!(store::active(&root).unwrap().is_none());

        let installed = download_on(
            &root,
            &catalog,
            choice,
            host,
            false,
            &cancelled,
            |asset, file| {
                assert_eq!(asset.url, choice.url);
                assert_eq!(asset.size, zip.len() as u64);
                assert_eq!(asset.sha256, choice.sha256);
                file.write_all(zip)
            },
        )
        .unwrap();
        assert!(installed.activated);
        assert_eq!(installed.runtime.active_installation_id(), Some(latest));
        assert!(installed.runtime.pinned_runtime_id().is_none());
        assert!(stage_is_clean());
        let mut runtime = Runtime::open_on(&root, host, &public, &cancelled).unwrap();
        runtime.ready().unwrap();
        assert!(
            download_on(
                &root,
                &catalog,
                &catalog.choices()[1],
                host,
                false,
                &cancelled,
                |_, file| { file.write_all(&packages[0].1) }
            )
            .is_err()
        );
        assert!(stage_is_clean());
        assert_eq!(store::active(&root).unwrap().as_deref(), Some(latest));
        drop(runtime);

        let (index, signature) =
            write_catalog(&root, "withdrawn", 4, &descriptors, &[latest], &key);
        let (review, _) = catalog_review(index, signature, &public).unwrap();
        store::accept_catalog(&root, &review, host, &public, &cancelled).unwrap();
        assert!(
            download_on(&root, &catalog, choice, host, false, &cancelled, |_, _| {
                panic!("an obsolete signed catalog must be refused before download")
            })
            .is_err()
        );
        assert!(store::active(&root).unwrap().is_none());
    }

    #[test]
    fn mlx_selected_versions_pin_and_follow_stable_without_interrupting_ready_runtime() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let key = SigningKey::from_bytes(&[23; 32]);
        let public = key.verifying_key();
        let host = OsVersion::parse("15.1").unwrap();
        let cancelled = AtomicBool::new(false);
        let older = "mlx-0.32.2-kortty1-macos-aarch64";
        let newest = "mlx-0.32.2-kortty2-macos-aarch64";
        let older_bytes = archive(older, true, 0x0100_000c, None);
        let newest_bytes = archive(newest, true, 0x0100_000c, None);
        let descriptors = [
            descriptor(older, &older_bytes),
            descriptor(newest, &newest_bytes),
        ];
        let read_catalog = |stem, day| {
            let (index, signature) = write_catalog(&root, stem, day, &descriptors, &[], &key);
            let receipt = Receipt::from_bytes(
                std::fs::read(index).unwrap(),
                std::fs::read(signature).unwrap(),
                &public,
            )
            .unwrap();
            Catalog::review(&root, receipt, host, public).unwrap()
        };
        let download = |catalog: &Catalog, position, bytes: &[u8]| {
            download_on(
                &root,
                catalog,
                &catalog.choices()[position],
                host,
                false,
                &cancelled,
                |_, file| file.write_all(bytes),
            )
            .unwrap()
        };
        let initial = read_catalog("initial", 2);
        let oldest = &initial.choices()[1];
        let pin = oldest.runtime_id.as_str();
        assert!(initial.pins(oldest));
        assert!(!initial.pins(&initial.choices()[0]));
        let installed = download(&initial, 1, &older_bytes);
        assert!(installed.activated);
        assert_eq!(installed.runtime.active_installation_id(), Some(older));
        assert_eq!(
            store::installed(&root).unwrap().pinned_runtime_id(),
            Some(pin)
        );
        let mut live = Runtime::open_on(&root, host, &public, &cancelled).unwrap();
        assert!(store::unpin(&root, pin, &cancelled).is_err());
        live.revalidate().unwrap();
        live.ready().unwrap();
        assert!(store::unpin(&root, "stale-review", &cancelled).is_err());
        let stable = store::unpin(&root, pin, &cancelled).unwrap();
        assert_eq!(stable.active_installation_id(), Some(older));
        assert!(stable.pinned_runtime_id().is_none());
        live.revalidate().unwrap();

        let current = read_catalog("current", 3);
        let repinned = download_on(
            &root,
            &current,
            &current.choices()[1],
            host,
            false,
            &cancelled,
            |_, _| panic!("selecting the exact installed version must not download it again"),
        )
        .unwrap();
        assert!(!repinned.activated);
        assert_eq!(repinned.runtime.pinned_runtime_id(), Some(pin));
        live.revalidate().unwrap();
        drop(live);

        let stable = download(&current, 0, &newest_bytes);
        assert!(stable.activated);
        assert_eq!(stable.runtime.active_installation_id(), Some(newest));
        assert!(
            store::installed(&root)
                .unwrap()
                .pinned_runtime_id()
                .is_none()
        );
        download(&current, 1, &older_bytes);
        let removal = store::review_removal(&root, &cancelled).unwrap().unwrap();
        store::uninstall(&root, &removal, &cancelled).unwrap();
        let removed = store::installed(&root).unwrap();
        assert!(removed.active_installation_id().is_none());
        assert!(removed.pinned_runtime_id().is_none());
        assert!(Catalog::review(&root, initial.receipt, host, public).is_err());
    }
}
