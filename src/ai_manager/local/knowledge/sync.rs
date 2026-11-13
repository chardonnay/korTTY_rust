//! Reconciles only saved Automatic knowledge sources through a local embedding profile.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ai_manager::Configuration;
use crate::local_ai::{self, Client};
use crate::private_files::FileStamp;
use crate::vault_window::VaultWindow;

use super::super::{RetiredIndex, SaveIndexes, StagedIndex};
use super::source::SyncMode;
use super::{
    Cache, Index, Scope, SourceKind, Store, UpdateSummary, check_active, folder, qdrant,
    remote_index,
};

pub(crate) struct Cancel {
    stopped: Arc<AtomicBool>,
    _request: local_ai::Cancel,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

pub(crate) struct Attempt {
    pub(crate) next: usize,
    pub(crate) store: String,
    pub(crate) pending: Result<Pending, String>,
}

pub(crate) struct Pending {
    pub(crate) reply: async_channel::Receiver<Result<Report, String>>,
    pub(crate) cancel: Cancel,
}

#[derive(Clone)]
pub(crate) struct Report {
    pub(crate) saved: Option<Arc<Configuration>>,
    pub(crate) status: String,
    pub(crate) changed: bool,
}

impl Report {
    pub(crate) fn failed(error: String) -> Self {
        Self {
            saved: None,
            status: format!("Automatic knowledge sync could not finish ({error})."),
            changed: false,
        }
    }
}

pub(in crate::ai_manager::local) struct CommitGuard {
    stopped: Arc<AtomicBool>,
    previous: Scope,
    stamp: Option<FileStamp>,
}

impl CommitGuard {
    pub(in crate::ai_manager::local) fn active(&self) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Automatic knowledge sync was canceled.",
            ));
        }
        Ok(())
    }

    pub(in crate::ai_manager::local) fn check(&self, cache: &Cache) -> io::Result<()> {
        self.active()?;
        self.previous
            .profile
            .require_current_policy_authority()
            .map_err(io::Error::other)?;
        require_previous(cache, &self.previous, self.stamp.as_ref())
    }
}

impl fmt::Debug for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KnowledgeSyncReport([redacted])")
    }
}

pub(crate) fn available(configuration: &Configuration) -> bool {
    configuration.enabled()
        && configuration.saved_root().is_some()
        && configuration
            .local
            .stores
            .iter()
            .any(|store| configuration.local.automatic_ready(store))
}

pub(crate) fn start(
    configuration: Arc<Configuration>,
    cursor: usize,
    vault: Option<&VaultWindow>,
) -> Option<Attempt> {
    let root = configuration.saved_root()?.to_path_buf();
    if !configuration.enabled() {
        return None;
    }
    let stores = &configuration.local.stores;
    let count = stores.len();
    let position = (0..count)
        .map(|offset| (cursor % count + offset) % count)
        .find(|&position| configuration.local.automatic_ready(&stores[position]))?;
    let store = stores[position].name.clone();
    let approved_policy_model = stores[position].automatic_policy_model;
    let pending = configuration
        .local
        .scope(&stores[position], &root)
        .and_then(|scope| {
            if scope.profile.policy_authority_fingerprint() != approved_policy_model {
                return Err(super::super::POLICY_EMBEDDING_CHANGED.into());
            }
            let vault_key = scope
                .qdrant
                .as_ref()
                .map_or(Ok(None), |settings| settings.resolve_vault_key(vault, None))?;
            let stopped = Arc::new(AtomicBool::new(false));
            let worker = Arc::clone(&stopped);
            let local_ai::Pending { reply, cancel } =
                local_ai::start(scope.profile.clone(), move |client| {
                    reconcile(
                        client,
                        configuration,
                        &root,
                        position,
                        scope,
                        worker,
                        vault_key,
                    )
                })?;
            Ok(Pending {
                reply,
                cancel: Cancel {
                    stopped,
                    _request: cancel,
                },
            })
        });
    Some(Attempt {
        next: (position + 1) % count,
        store,
        pending,
    })
}

fn reconcile(
    client: &Client,
    baseline: Arc<Configuration>,
    root: &Path,
    position: usize,
    previous_scope: Scope,
    stopped: Arc<AtomicBool>,
    vault_key: Option<Arc<qdrant::VaultKey>>,
) -> Result<Report, String> {
    let active = || {
        if stopped.load(Ordering::Acquire) {
            return Err("Automatic knowledge sync was canceled.".into());
        }
        if let Some(key) = &vault_key {
            key.require_current()?;
        }
        check_active(client)
    };
    let authorize = || {
        active()?;
        previous_scope.profile.require_current_policy_authority()?;
        if previous_scope.qdrant.is_some() {
            remote_index::require_policy()?;
        }
        Configuration::require_local_settings(root, &baseline.local)
    };
    authorize()?;
    let cache = Cache::new(root.to_owned());
    let previous = match cache.load_stamped(&previous_scope) {
        Ok(snapshot) => Some(snapshot),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Could not read the saved index ({error}).")),
    };
    let mut candidate = baseline.as_ref().clone();
    let store = &mut candidate.local.stores[position];
    let expanded_qdrant = store.automatic_qdrant_expanded;
    let (included, unavailable, boundary_changed) = collect(store, &stopped, active)?;
    let saved_scope = candidate
        .local
        .scope(&candidate.local.stores[position], root)?;
    if saved_scope.profile != previous_scope.profile {
        return Err(super::super::POLICY_EMBEDDING_CHANGED.into());
    }
    let included: HashSet<_> = included.iter().map(|path| folder::path_key(path)).collect();
    let mut scope = saved_scope.clone();
    for (enabled, path) in scope.enabled.iter_mut().zip(&scope.sources) {
        *enabled &= included.contains(&folder::path_key(path));
    }
    let changed_manifest = !scope.same_snapshot(&previous_scope);
    if !changed_manifest && !scope.enabled.iter().any(|&enabled| enabled) {
        return Ok(Report {
            saved: None,
            status: status(UpdateSummary::default(), unavailable, boundary_changed),
            changed: false,
        });
    }
    let (index, summary) = Index::reconcile(
        client,
        &scope,
        previous.as_ref().map(|(index, _)| (&previous_scope, index)),
        expanded_qdrant,
        authorize,
    )?;
    let coverage = remote_index::automatic_coverage(
        &saved_scope,
        &scope,
        &previous_scope,
        previous.as_ref().map(|(index, _)| index),
    );
    active()?;
    let mut saved = None;
    let mut warning = None;
    let coverage_changed = scope.qdrant.is_some()
        && previous
            .as_ref()
            .and_then(|(index, _)| index.qdrant_receipt.as_ref())
            .is_none_or(|receipt| receipt.sources != coverage);
    let changed_index =
        previous.is_none() || summary.indexed != 0 || summary.removed != 0 || coverage_changed;
    if changed_manifest {
        let indexes = SaveIndexes {
            cache,
            retained: candidate.local.index_scopes(root),
            staged: vec![StagedIndex {
                index: Arc::new(index),
                scope,
                sources: coverage,
                vault_key: vault_key.clone(),
            }],
            retired: previous_scope
                .qdrant
                .is_some()
                .then(|| RetiredIndex {
                    scope: previous_scope.clone(),
                    vault_key: Ok(vault_key.clone()),
                })
                .into_iter()
                .collect(),
            automatic: Some(CommitGuard {
                stopped,
                previous: previous_scope,
                stamp: previous.map(|(_, stamp)| stamp),
            }),
        };
        warning = candidate
            .save_with_indexes(root, &baseline, Some(indexes), Some(client))
            .map_err(|error| format!("Could not save the refreshed folder index ({error})."))?;
        saved = Some(Arc::new(candidate));
    } else if changed_index {
        let publication = Configuration::publish_knowledge_index(root, |current| {
            if current != &baseline.local {
                return Err(io::Error::other("The saved knowledge settings changed."));
            }
            let require_current = || {
                scope
                    .profile
                    .require_current_policy_authority()
                    .map_err(io::Error::other)?;
                require_previous(
                    &cache,
                    &previous_scope,
                    previous.as_ref().map(|(_, stamp)| stamp),
                )
            };
            if scope.qdrant.is_some() {
                require_current()?;
                remote_index::publish(
                    &cache,
                    &scope,
                    &index,
                    coverage,
                    Some(client),
                    vault_key.clone(),
                    authorize,
                )
            } else {
                cache.save(&index, || {
                    require_current()?;
                    index
                        .check_sources(&scope, active)
                        .map_err(io::Error::other)
                })?;
                Ok(remote_index::Publication::default())
            }
        })
        .map_err(|error| format!("Could not publish the refreshed knowledge index ({error})."))?;
        if publication.cleanup_warning {
            warning = Some("The obsolete Qdrant generation could not be removed.".into());
        }
    }
    let mut status = status(summary, unavailable, boundary_changed);
    if let Some(warning) = warning {
        status.push(' ');
        status.push_str(&warning);
    }
    Ok(Report {
        saved,
        status,
        changed: changed_manifest || changed_index,
    })
}

fn collect(
    store: &mut Store,
    stopped: &AtomicBool,
    active: impl Fn() -> Result<(), String>,
) -> Result<(Vec<PathBuf>, usize, usize), String> {
    let mut included = Vec::new();
    let mut unavailable = 0;
    let mut boundary_changed = 0;
    for path in &store.sources {
        if store.file_sync(path) == SyncMode::Automatic
            && store.source_enabled(SourceKind::File, path)
        {
            active()?;
            if super::scan_source(
                path,
                store.file_limit(path),
                || !stopped.load(Ordering::Acquire),
                |_, _| Ok(()),
            )
            .is_ok()
            {
                included.push(path.clone());
            } else {
                unavailable += 1;
            }
        }
    }
    for position in 0..store.folders.len() {
        active()?;
        let source = &store.folders[position];
        if source.options.sync != SyncMode::Automatic
            || !store.source_enabled(SourceKind::Folder, &source.root)
        {
            continue;
        }
        let limit = if source.uses_expanded_approval() {
            super::MAX_SOURCES
        } else {
            super::LEGACY_MAX_SOURCES
        };
        let available = limit
            .saturating_sub(store.source_count() - source.len())
            .max(source.len());
        match folder::refresh(source, available, &store.sources, stopped) {
            Ok(refreshed) => {
                included.extend(refreshed.sources().map(|(path, _)| path));
                store.folders[position] = refreshed;
            }
            Err(folder::RefreshError::Boundary) => boundary_changed += 1,
            Err(folder::RefreshError::Unavailable) => unavailable += 1,
        }
    }
    active()?;
    Ok((included, unavailable, boundary_changed))
}

fn require_previous(cache: &Cache, scope: &Scope, stamp: Option<&FileStamp>) -> io::Result<()> {
    if let Some(stamp) = stamp {
        return cache.require_current(scope, stamp);
    }
    match cache.load(scope) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(
            "The knowledge index was created by another operation.",
        )),
    }
}

fn status(summary: UpdateSummary, unavailable: usize, boundary_changed: usize) -> String {
    let mut status = if summary.indexed == 0 && summary.removed == 0 {
        "Automatic sync checked the available sources.".to_owned()
    } else {
        format!(
            "Automatic sync updated {} files and removed {}.",
            summary.indexed, summary.removed
        )
    };
    if unavailable != 0 {
        status.push_str(&format!(
            " Skipped {unavailable} unavailable sources and kept any saved passages."
        ));
    }
    if boundary_changed != 0 {
        status.push_str(&format!(
            " Review {boundary_changed} folders with changed approval boundaries."
        ));
    }
    status
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read as _, Write as _};

    use serde_json::{Value, json};

    use crate::local_ai::test_support;

    use super::super::super::State;
    use super::super::{Embedding, source};
    use super::*;

    #[test]
    fn automatic_index_keeps_manual_passages_without_reading_changed_manual_text() {
        let _exclusive = test_support::exclusive_run();
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let automatic = root.join("automatic.txt");
        let manual = root.join("manual.txt");
        fs::write(&automatic, "Original automatic text").unwrap();
        fs::write(&manual, "Original manual text").unwrap();
        let (server, _) = test_support::listener();
        let endpoint = server.local_addr().unwrap().to_string();
        let responses = std::thread::spawn(move || {
            for expected in [
                vec!["Original automatic text", "Original manual text"],
                vec!["Edited automatic text"],
            ] {
                let mut socket = test_support::accept(&server);
                let request: Value = serde_json::from_slice(&test_support::wire_request(
                    &mut socket,
                    "POST /api/embed HTTP/1.1",
                ))
                .unwrap();
                assert_eq!(request["input"], json!(expected));
                let vectors = vec![[1.0, 0.0]; expected.len()];
                socket
                    .get_mut()
                    .write_all(
                        test_support::fixture_http_response(
                            "200 OK",
                            "",
                            &json!({"embeddings": vectors}).to_string(),
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let mut store = Store::new("Fixture sources").unwrap();
        store.add_source(automatic.clone()).unwrap();
        store.add_source(manual.clone()).unwrap();
        store
            .set_file_size_limit(automatic.clone(), Some(32))
            .unwrap();
        store.set_file_sync(automatic.clone(), SyncMode::Automatic);
        store.embedding = Some(Embedding {
            endpoint,
            model: "embedding-fixture".into(),
            managed_model: None,
        });
        store.automatic_embedding = store.embedding.clone();
        let baseline = Configuration {
            local: State {
                stores: vec![store],
                ..State::default()
            },
            ..Configuration::default()
        };
        baseline
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let baseline = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let scope = baseline
            .local
            .scope(&baseline.local.stores[0], &root)
            .unwrap();
        let cache = Cache::new(root.clone());
        let index = Index::build(&Client::new(scope.profile.clone()), &scope).unwrap();
        cache.save(&index, || Ok(())).unwrap();

        fs::write(&automatic, "Edited automatic text").unwrap();
        fs::write(&manual, "Invalid\0manual content").unwrap();
        let pending = start(Arc::clone(&baseline), 0, None)
            .unwrap()
            .pending
            .unwrap();
        let result = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(result.changed && result.saved.is_none());
        let refreshed = cache.load(&scope).unwrap();
        assert_eq!(
            refreshed
                .entries
                .iter()
                .map(|(passage, _)| passage.text.as_str())
                .collect::<Vec<_>>(),
            ["Edited automatic text", "Original manual text"]
        );
        assert!(refreshed.check_sources(&scope, || Ok(())).is_err());
        responses.join().unwrap();

        fs::write(&automatic, "x".repeat(33)).unwrap();
        let pending = start(Arc::clone(&baseline), 0, None)
            .unwrap()
            .pending
            .unwrap();
        let result = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(!result.changed && result.status.contains("unavailable"));
        assert_eq!(cache.load(&scope).unwrap().sources, refreshed.sources);

        fs::remove_file(&automatic).unwrap();
        let pending = start(Arc::clone(&baseline), 0, None)
            .unwrap()
            .pending
            .unwrap();
        let result = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(!result.changed && result.status.contains("unavailable"));
        assert_eq!(cache.load(&scope).unwrap().sources, refreshed.sources);

        let mut changed = baseline.as_ref().clone();
        changed.local.stores[0].embedding.as_mut().unwrap().model = "another model".into();
        assert!(!available(&changed));
        assert!(start(Arc::new(changed), 0, None).is_none());
    }

    #[test]
    fn automatic_folder_stops_when_its_root_or_ignore_approval_changes() {
        let profile = crate::snippets::tests::Profile::new();
        let base = profile.0.canonicalize().unwrap();
        let root = base.join("automatic");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("old.txt"), "Approved existing file").unwrap();
        fs::write(root.join("private.txt"), "Ignored file").unwrap();
        fs::write(root.join(".gitignore"), "private.txt\n").unwrap();
        let mut store = Store::new("Approved folder").unwrap();
        let (reply, _cancel) = store
            .folder_review_options(
                root.clone(),
                source::Options {
                    sync: SyncMode::Automatic,
                    ..source::Options::default()
                },
            )
            .unwrap();
        store
            .approve_folder(&reply.recv_blocking().unwrap().unwrap())
            .unwrap();
        let approved = &store.folders[0];
        let stopped = AtomicBool::new(false);
        fs::write(root.join(".gitignore"), "").unwrap();
        assert!(matches!(
            folder::refresh(approved, 8, &[], &stopped),
            Err(folder::RefreshError::Boundary)
        ));
        fs::write(root.join(".gitignore"), "private.txt\n").unwrap();
        fs::remove_file(root.join("old.txt")).unwrap();
        let empty = folder::refresh(approved, 8, &[], &stopped).ok().unwrap();
        assert_eq!(empty.len(), 0);
        fs::write(root.join("new.txt"), "New approved file").unwrap();
        let updated = folder::refresh(approved, 8, &[], &stopped).ok().unwrap();
        assert_eq!(
            updated.sources().map(|(path, _)| path).collect::<Vec<_>>(),
            [root.join("new.txt")]
        );

        fs::rename(&root, base.join("old-root")).unwrap();
        assert!(matches!(
            folder::refresh(approved, 8, &[], &stopped),
            Err(folder::RefreshError::Unavailable)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join(".gitignore"), "private.txt\n").unwrap();
        assert!(matches!(
            folder::refresh(approved, 8, &[], &stopped),
            Err(folder::RefreshError::Boundary)
        ));
    }

    #[test]
    fn automatic_folder_deletion_publishes_an_empty_index_and_retires_the_old_scope() {
        let _exclusive = test_support::exclusive_run();
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let folder = root.join("automatic");
        fs::create_dir(&folder).unwrap();
        let source_file = folder.join("old.txt");
        fs::write(&source_file, "One indexable file").unwrap();
        let mut store = Store::new("Automatic folder").unwrap();
        let (reply, _cancel) = store
            .folder_review_options(
                folder,
                source::Options {
                    sync: SyncMode::Automatic,
                    ..source::Options::default()
                },
            )
            .unwrap();
        store
            .approve_folder(&reply.recv_blocking().unwrap().unwrap())
            .unwrap();
        let (server, _) = test_support::listener();
        let endpoint = server.local_addr().unwrap().to_string();
        let response = std::thread::spawn(move || {
            let mut socket = test_support::accept(&server);
            let request: Value = serde_json::from_slice(&test_support::wire_request(
                &mut socket,
                "POST /api/embed HTTP/1.1",
            ))
            .unwrap();
            assert_eq!(request["input"], json!(["One indexable file"]));
            socket
                .get_mut()
                .write_all(
                    test_support::fixture_http_response(
                        "200 OK",
                        "",
                        r#"{"embeddings":[[1.0,0.0]]}"#,
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        store.embedding = Some(Embedding {
            endpoint,
            model: "embed".into(),
            managed_model: None,
        });
        store.automatic_embedding = store.embedding.clone();
        let baseline = Configuration {
            local: State {
                stores: vec![store],
                ..State::default()
            },
            ..Configuration::default()
        };
        baseline
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let baseline = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let previous = baseline
            .local
            .scope(&baseline.local.stores[0], &root)
            .unwrap();
        let cache = Cache::new(root.clone());
        let index = Index::build(&Client::new(previous.profile.clone()), &previous).unwrap();
        cache.save(&index, || Ok(())).unwrap();
        response.join().unwrap();

        fs::remove_file(source_file).unwrap();
        let pending = start(baseline, 0, None).unwrap().pending.unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(report.changed);
        let saved = report.saved.unwrap();
        assert_eq!(Configuration::load_fixture_from(&root).unwrap(), *saved);
        assert_eq!(saved.local.stores[0].folders[0].len(), 0);
        let refreshed = saved.local.scope(&saved.local.stores[0], &root).unwrap();
        assert!(!refreshed.same_snapshot(&previous));
        assert_eq!(cache.load(&refreshed).unwrap().len(), 0);
        assert_eq!(
            cache.load(&previous).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn canceled_automatic_sync_closes_the_embedding_request_without_publishing() {
        let _exclusive = test_support::exclusive_run();
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let source = root.join("automatic.txt");
        fs::write(&source, "A new source").unwrap();
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let (seen, wait_for_request) = std::sync::mpsc::channel();
        let service = std::thread::spawn(move || {
            let mut socket = test_support::accept(&listener);
            let _ = test_support::wire_request(&mut socket, "POST /api/embed HTTP/1.1");
            seen.send(()).unwrap();
            match socket.read(&mut [0; 1]) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) => {}
                result => panic!("expected the canceled request to close: {result:?}"),
            }
        });
        let mut store = Store::new("Automatic file").unwrap();
        store.add_source(source.clone()).unwrap();
        store.set_file_sync(source, SyncMode::Automatic);
        store.embedding = Some(Embedding {
            endpoint,
            model: "embed".into(),
            managed_model: None,
        });
        store.automatic_embedding = store.embedding.clone();
        let initial = Configuration {
            local: State {
                stores: vec![store],
                ..State::default()
            },
            ..Configuration::default()
        };
        initial
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let initial = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let scope = initial
            .local
            .scope(&initial.local.stores[0], &root)
            .unwrap();
        let pending = start(Arc::clone(&initial), 0, None)
            .unwrap()
            .pending
            .unwrap();
        wait_for_request
            .recv_timeout(std::time::Duration::from_secs(4))
            .unwrap();
        drop(pending.cancel);
        assert!(pending.reply.recv_blocking().unwrap().is_err());
        service.join().unwrap();
        assert_eq!(
            Cache::new(root.clone()).load(&scope).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(Configuration::load_fixture_from(&root).unwrap(), *initial);
    }
}
