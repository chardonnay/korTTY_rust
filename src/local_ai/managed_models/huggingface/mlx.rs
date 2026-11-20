//! Immutable asset review for a Hugging Face MLX model directory.

use std::collections::BTreeMap;
use std::io::{self, Read as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use kortty_vault::Secret;
use pgp::crypto::hash::HashAlgorithm;
use sha2::{Digest, Sha256};

use crate::local_ai::runtime_package::{Pending, work_with_agent};
use crate::local_ai::{mlx_models, mlx_runtime};
mod download;

pub(crate) use download::{Download, Downloaded, discard_paused, download, paused};

use super::{
    Artifact, Hub, ModelWire, Repository, api_url, lowercase_hex, mlx_asset_sizes, network_error,
    resolve_url, transfer,
};

const REVIEW_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageReview {
    pub(crate) repository: Repository,
    pub(crate) revision: String,
    pub(crate) files: Vec<Artifact>,
    pub(crate) bytes: u64,
}

impl PackageReview {
    fn validate(&self) -> io::Result<()> {
        Repository::parse(self.repository.id()).map_err(io::Error::other)?;
        if !lowercase_hex(&self.revision, 40) {
            return Err(io::Error::other(
                "Review an immutable MLX commit before downloading.",
            ));
        }
        let mut sizes = BTreeMap::new();
        for file in &self.files {
            if !lowercase_hex(file.digest(), 64)
                || sizes.insert(file.path().to_owned(), file.size()).is_some()
            {
                return Err(io::Error::other(
                    "The reviewed MLX asset manifest is invalid.",
                ));
            }
        }
        mlx_models::require_asset_layout(&sizes)?;
        if mlx_models::size_of_assets(&sizes)? != self.bytes {
            return Err(io::Error::other("The reviewed MLX byte total changed."));
        }
        Ok(())
    }
}

pub(crate) fn review(
    repository: Repository,
    credentials: Option<(Secret, Weak<()>)>,
) -> Result<Pending<Arc<PackageReview>>, String> {
    if !mlx_runtime::supported() {
        return Err("MLX requires Apple Silicon and macOS 14 or later.".into());
    }
    Repository::parse(repository.id())?;
    let (token, revision) = credentials.unzip();
    let mut hub = Hub::new(token, Duration::from_secs(20))?;
    hub.bind_credential_revision(revision);
    work_with_agent(Some(hub.agent.clone()), move |cancelled| {
        hub.review_mlx(repository, cancelled).map(Arc::new)
    })
}

impl Hub {
    pub(super) fn mlx_metadata(
        &self,
        repository: &Repository,
        cancelled: &AtomicBool,
    ) -> Result<ModelWire, String> {
        let segments: Vec<_> = repository.id().split('/').collect();
        let mut url = api_url(&self.origin, &segments);
        url.query_pairs_mut().append_pair("blobs", "true");
        let wire: ModelWire = self.json(url, cancelled)?;
        if wire.repository.id() != Some(repository.id())
            || !wire.sha.as_deref().is_some_and(|id| lowercase_hex(id, 40))
        {
            return Err("The Hub did not identify an immutable MLX repository.".into());
        }
        Ok(wire)
    }

    fn review_mlx(
        &self,
        repository: Repository,
        cancelled: &AtomicBool,
    ) -> Result<PackageReview, String> {
        let deadline = Instant::now() + REVIEW_TIMEOUT;
        let wire = self.mlx_metadata(&repository, cancelled)?;
        let revision = wire
            .sha
            .ok_or("The MLX repository has no immutable commit.")?;
        let sizes = mlx_asset_sizes(&wire.siblings)
            .ok_or("The Hub did not provide all required MLX assets and sizes.")?;
        let bytes = mlx_models::size_of_assets(&sizes).map_err(|error| error.to_string())?;
        let mut files = Vec::with_capacity(sizes.len());
        let mut metadata = Vec::new();
        for file in wire.siblings {
            let Some(&size) = sizes.get(&file.rfilename) else {
                continue;
            };
            if let Some(lfs) = file.lfs {
                let digest = super::lfs_digest(lfs)
                    .ok_or("An MLX large file has no published SHA-256 digest.")?;
                files.push(Artifact {
                    path: file.rfilename,
                    size,
                    digest,
                });
            } else {
                if file.rfilename.ends_with(".safetensors") {
                    return Err("An MLX weight file has no published SHA-256 digest.".into());
                }
                let blob = file
                    .blob_id
                    .filter(|id| lowercase_hex(id, 40))
                    .ok_or("An MLX metadata file has no Git object identity.")?;
                metadata.push((file.rfilename, size, blob));
            }
        }
        for (path, size, blob) in metadata {
            let url = resolve_url(&self.origin, &repository, &revision, &path);
            let mut response = self.get(url, None, true, Some(deadline), cancelled)?;
            transfer::validate_response(&response, 0, size, None)
                .map_err(|error| error.to_string())?;
            let header = format!("blob {size}\0");
            let mut object = header.into_bytes();
            let start = object.len();
            response
                .body_mut()
                .as_reader()
                .take(size + 1)
                .read_to_end(&mut object)
                .map_err(|_| network_error(cancelled))?;
            if (object.len() - start) as u64 != size {
                return Err("An MLX metadata file differs from the reviewed byte size.".into());
            }
            let git_digest = HashAlgorithm::Sha1
                .digest(&object)
                .map_err(|error| error.to_string())?;
            if transfer::hex(&git_digest) != blob {
                return Err("An MLX metadata file differs from its Git object identity.".into());
            }
            files.push(Artifact {
                path,
                size,
                digest: transfer::hex(&Sha256::digest(&object[start..])),
            });
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(network_error(cancelled));
        }
        self.require_downloads()
            .map_err(|error| error.to_string())?;
        files.sort_unstable_by(|left, right| left.path().cmp(right.path()));
        Ok(PackageReview {
            repository,
            revision,
            files,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::download::{LOCK, STAGING, discard_saved, install, read_paused};
    use super::*;
    use crate::private_files::Directory;
    use std::io::Write as _;

    use crate::local_ai::test_support::{accept, fixture_http_response, listener, wire_request};
    use serde_json::json;

    #[test]
    fn direct_mlx_hub_install_verifies_all_assets_and_cleans_up_after_failure() {
        for corrupt in [false, true] {
            let profile = crate::snippets::tests::Profile::new();
            let root = profile.0.canonicalize().unwrap();
            let source = mlx_models::test_fixture(&root);
            let repository = Repository::parse("example/model").unwrap();
            let revision = "a".repeat(40);
            let payloads: Vec<_> = ["config.json", "model.safetensors", "tokenizer.json"]
                .into_iter()
                .map(|name| (name, std::fs::read(source.join(name)).unwrap()))
                .collect();
            let files: Vec<_> = payloads
                .iter()
                .map(|(path, data)| Artifact {
                    path: (*path).into(),
                    size: data.len() as u64,
                    digest: transfer::hex(&Sha256::digest(data)),
                })
                .collect();
            let bytes = files.iter().map(Artifact::size).sum();
            let review = Arc::new(PackageReview {
                repository,
                revision: revision.clone(),
                files,
                bytes,
            });
            let (server, _) = listener();
            let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
            hub.origin =
                url::Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
            let worker = std::thread::spawn(move || {
                for (name, mut data) in payloads {
                    let mut stream = accept(&server);
                    wire_request(
                        &mut stream,
                        &format!("GET /example/model/resolve/{revision}/{name} HTTP/1.1"),
                    );
                    let changed = corrupt && name == "model.safetensors";
                    if changed {
                        data[0] ^= 1;
                    }
                    write!(
                        stream.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len()
                    )
                    .unwrap();
                    stream.get_mut().write_all(&data).unwrap();
                    if changed {
                        break;
                    }
                }
            });
            let mut progress = Vec::new();
            let stop = AtomicBool::new(false);
            let result = install(&root, &review, &hub, false, &stop, &stop, |update| {
                progress.push(update.received)
            });
            worker.join().unwrap();
            let registry = mlx_models::Registry::new(&root);
            if corrupt {
                assert!(result.unwrap_err().to_string().contains("reviewed SHA-256"));
                assert!(registry.list().unwrap().is_empty());
            } else {
                let Downloaded::Installed {
                    model,
                    cleanup_warning,
                    sync_warning,
                } = result.unwrap()
                else {
                    panic!("The complete fixture was not installed");
                };
                assert_eq!(model.name, "example/model");
                assert_eq!(registry.list().unwrap(), [model]);
                assert!(!cleanup_warning);
                assert!(!sync_warning);
                assert_eq!(progress.last(), Some(&bytes));
            }
            let downloads = Directory::open(&root, false)
                .unwrap()
                .private_child(STAGING, false)
                .unwrap();
            assert_eq!(
                downloads.names(2).unwrap(),
                [std::ffi::OsString::from(LOCK)]
            );
        }
    }

    #[test]
    fn mlx_hub_resume_reads_the_saved_manifest_and_checks_completed_files_and_ranges() {
        use std::io::BufRead as _;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = mlx_models::test_fixture(&root);
        let payloads: Vec<_> = ["config.json", "model.safetensors", "tokenizer.json"]
            .into_iter()
            .map(|name| (name, std::fs::read(source.join(name)).unwrap()))
            .collect();
        let files: Vec<_> = payloads
            .iter()
            .map(|(path, data)| Artifact {
                path: (*path).into(),
                size: data.len() as u64,
                digest: transfer::hex(&Sha256::digest(data)),
            })
            .collect();
        let bytes = files.iter().map(Artifact::size).sum();
        let revision = "a".repeat(40);
        let review = Arc::new(PackageReview {
            repository: Repository::parse("example/model").unwrap(),
            revision: revision.clone(),
            files,
            bytes,
        });
        let (server, _) = listener();
        let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
        hub.origin = url::Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
        let prefix = 16;
        let worker = std::thread::spawn(move || {
            let mut first = accept(&server);
            let path = format!("/example/model/resolve/{revision}");
            wire_request(&mut first, &format!("GET {path}/config.json HTTP/1.1"));
            let config = &payloads[0].1;
            write!(
                first.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                config.len()
            )
            .unwrap();
            first.get_mut().write_all(config).unwrap();
            drop(first);
            let mut interrupted = accept(&server);
            let weights = &payloads[1].1;
            wire_request(
                &mut interrupted,
                &format!("GET {path}/model.safetensors HTTP/1.1"),
            );
            write!(interrupted.get_mut(), "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"weights\"\r\nConnection: close\r\n\r\n", weights.len()).unwrap();
            interrupted.get_mut().write_all(&weights[..prefix]).unwrap();
            drop(interrupted);

            let mut resumed = accept(&server);
            let mut request = String::new();
            loop {
                let mut line = String::new();
                assert!(resumed.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            assert!(request.starts_with(&format!("GET {path}/model.safetensors HTTP/1.1\r\n")));
            let headers = request.to_ascii_lowercase();
            assert!(headers.contains("range: bytes=16-\r\n"));
            assert!(headers.contains("if-range: \"weights\"\r\n"));
            write!(resumed.get_mut(), "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {prefix}-{}/{}\r\nETag: \"weights\"\r\nConnection: close\r\n\r\n",
                weights.len() - prefix, weights.len() - 1, weights.len()).unwrap();
            resumed.get_mut().write_all(&weights[prefix..]).unwrap();
            drop(resumed);

            let mut last = accept(&server);
            wire_request(&mut last, &format!("GET {path}/tokenizer.json HTTP/1.1"));
            let tokenizer = &payloads[2].1;
            write!(
                last.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                tokenizer.len()
            )
            .unwrap();
            last.get_mut().write_all(tokenizer).unwrap();
        });
        let stop = AtomicBool::new(false);
        let first = install(&root, &review, &hub, false, &stop, &stop, |_| {}).unwrap();
        assert!(matches!(
            first,
            Downloaded::Paused {
                interrupted: true,
                ..
            }
        ));
        let saved = read_paused(&root).unwrap().unwrap();
        assert_eq!(saved, review);
        let staged_config = root.join(STAGING).join("paused").join("config.json");
        let original = std::fs::read(&staged_config).unwrap();
        let mut tampered = original.clone();
        tampered[0] ^= 1;
        std::fs::write(&staged_config, tampered).unwrap();
        let error = install(&root, &saved, &hub, true, &stop, &stop, |_| {}).unwrap_err();
        assert!(error.to_string().contains("reviewed SHA-256"));
        std::fs::write(&staged_config, original).unwrap();
        let resumed = install(&root, &saved, &hub, true, &stop, &stop, |_| {}).unwrap();
        worker.join().unwrap();
        assert!(matches!(resumed, Downloaded::Installed { .. }));
        assert!(read_paused(&root).unwrap().is_none());
        assert_eq!(mlx_models::Registry::new(&root).list().unwrap().len(), 1);
    }

    #[test]
    fn mlx_hub_pause_preserves_a_checkpoint_until_explicit_discard() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = mlx_models::test_fixture(&root);
        let files: Vec<_> = ["config.json", "model.safetensors", "tokenizer.json"]
            .into_iter()
            .map(|name| {
                let data = std::fs::read(source.join(name)).unwrap();
                Artifact {
                    path: name.into(),
                    size: data.len() as u64,
                    digest: transfer::hex(&Sha256::digest(data)),
                }
            })
            .collect();
        let config = std::fs::read(source.join("config.json")).unwrap();
        let config_bytes = config.len() as u64;
        let revision = "a".repeat(40);
        let review = Arc::new(PackageReview {
            repository: Repository::parse("example/model").unwrap(),
            revision: revision.clone(),
            bytes: files.iter().map(Artifact::size).sum(),
            files,
        });
        let (server, _) = listener();
        let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
        hub.origin = url::Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = accept(&server);
            wire_request(
                &mut stream,
                &format!("GET /example/model/resolve/{revision}/config.json HTTP/1.1"),
            );
            write!(
                stream.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {config_bytes}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.get_mut().write_all(&config).unwrap();
            server
        });
        let pause = AtomicBool::new(false);
        let cancelled = AtomicBool::new(false);
        let outcome = install(
            &root,
            &review,
            &hub,
            false,
            &pause,
            &cancelled,
            |progress| {
                if progress.received == config_bytes {
                    pause.store(true, Ordering::Release);
                }
            },
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Downloaded::Paused {
                interrupted: false,
                ..
            }
        ));
        assert!(read_paused(&root).unwrap().is_some());
        assert!(install(&root, &review, &hub, false, &pause, &cancelled, |_| {}).is_err());
        assert!(discard_saved(&root).unwrap());
        assert!(!discard_saved(&root).unwrap());
        assert!(read_paused(&root).unwrap().is_none());
        assert!(mlx_models::Registry::new(&root).list().unwrap().is_empty());
        let server = worker.join().unwrap();
        assert_eq!(
            server.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn mlx_review_binds_git_metadata_and_published_weight_digests_to_the_commit() {
        for corrupt in [false, true] {
            let (server, _) = listener();
            let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
            hub.origin =
                url::Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
            let revision = "a".repeat(40);
            let artifact_digest = "b".repeat(64);
            let git_digest = transfer::hex(&HashAlgorithm::Sha1.digest(b"blob 2\0{}").unwrap());
            let expected_revision = revision.clone();
            let expected_digest = artifact_digest.clone();
            let worker = std::thread::spawn(move || {
                let mut stream = accept(&server);
                wire_request(
                    &mut stream,
                    "GET /api/models/example/model?blobs=true HTTP/1.1",
                );
                let wire = json!({"id":"example/model","sha":revision,"siblings":[
                    {"rfilename":"model.safetensors","lfs":{"size":100,"sha256":artifact_digest}},
                    {"rfilename":"config.json","size":2,"blobId":git_digest},
                    {"rfilename":"tokenizer.json","size":2,"blobId":git_digest},
                    {"rfilename":"not-model-code.py","size":1}
                ]});
                stream
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", &wire.to_string()).as_bytes())
                    .unwrap();
                for name in ["config.json", "tokenizer.json"] {
                    let mut stream = accept(&server);
                    wire_request(
                        &mut stream,
                        &format!("GET /example/model/resolve/{revision}/{name} HTTP/1.1"),
                    );
                    let bytes = if corrupt { "[]" } else { "{}" };
                    stream
                        .get_mut()
                        .write_all(fixture_http_response("200 OK", "", bytes).as_bytes())
                        .unwrap();
                    if corrupt {
                        break;
                    }
                }
                server
            });
            let result = hub.review_mlx(
                Repository::parse("example/model").unwrap(),
                &AtomicBool::new(false),
            );
            if corrupt {
                assert!(result.unwrap_err().contains("Git object identity"));
            } else {
                let review = result.unwrap();
                assert_eq!(review.revision, expected_revision);
                assert_eq!(review.bytes, 104);
                assert_eq!(review.files.len(), 3);
                assert_eq!(
                    review.files[0].digest(),
                    transfer::hex(&Sha256::digest(b"{}"))
                );
                assert_eq!(review.files[1].digest(), expected_digest);
            }
            let server = worker.join().unwrap();
            assert_eq!(
                server.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
}
