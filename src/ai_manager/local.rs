//! Local model discovery, profile roles and knowledge-store configuration.

pub(super) mod huggingface;
mod knowledge;
mod knowledge_backend;
mod knowledge_options;
mod managed;
mod mlx;
mod online;

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Task};
use serde::{Deserialize, Serialize};

use crate::clipboard_policy::{ClipboardPolicy, PolicyModelRuntime, valid_policy_model_id};
use crate::connection_window::CredentialChoice;
use crate::local_ai::runtime_index::PackageBackend;
use crate::local_ai::runtime_package::online::policy as runtime_policy;
use crate::local_ai::{
    self, Pending, PolicyModelAuthority, Profile, managed_models, policy_models, runtime_package,
};
use crate::vault_window::{Kind as VaultKind, VaultWindow};

use super::profiles::{Catalog, ProfileRef, picker::DismissOnEscape as _};
use super::{
    Configuration, ProfileId,
    model_inventory::ModelInventory,
    search::{Reveal, Setting},
};

use knowledge::folder;
use knowledge::qdrant::{VaultKey, require_vault_revision};
pub(super) use knowledge::retrieval;
use knowledge::source::{DEFAULT_TEXT_BYTES, SyncMode};
pub(crate) use knowledge::sync as knowledge_sync;
use knowledge::{
    Cache, Embedding, Index, Scope, SourceKind, SourceMask, Store, UpdateSummary, remote_index,
};

const MAX_DRAFT_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MANAGED_EMBEDDING_DESTINATION: &str = "Managed llama.cpp on this computer";
const POLICY_EMBEDDING_CHANGED: &str = "The policy-managed embedding model changed. Refresh the managed models and review the destination again.";

fn valid_embedding_model_id(id: &str) -> bool {
    managed_models::valid_id(id) || valid_policy_model_id(id)
}

pub(super) enum Page {
    Models(Vec<CredentialChoice>),
    Knowledge(Vec<CredentialChoice>),
    Preferences,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Text,
    Coding,
    Agent,
    Journal,
}

impl Role {
    pub(super) const ALL: [Self; 4] = [Self::Text, Self::Coding, Self::Agent, Self::Journal];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Text => "Text profile",
            Self::Coding => "Coding profile",
            Self::Agent => "Terminal agent profile",
            Self::Journal => "Session journal profile",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct State {
    roles: [Option<ProfileRef>; 4],
    endpoint: String,
    embedding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    managed_embedding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_managed_model: Option<String>,
    #[serde(skip_serializing_if = "runtime_policy::Settings::is_default")]
    pub(super) runtime_update: runtime_policy::Settings,
    stores: Vec<Store>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            roles: [const { None }; 4],
            endpoint: local_ai::DEFAULT_ENDPOINT.into(),
            embedding: String::new(),
            managed_embedding: None,
            default_managed_model: None,
            runtime_update: runtime_policy::Settings::default(),
            stores: Vec::new(),
        }
    }
}

impl State {
    pub(super) fn set_guided_profile(&mut self, role: Role, reference: ProfileRef) {
        self.roles[role as usize] = Some(reference);
    }

    pub(super) fn set_guided_embedding(&mut self, model: String) {
        self.managed_embedding = Some(model);
    }

    pub(super) fn needs_guided_embedding(&self) -> bool {
        self.managed_embedding.is_none() && self.embedding.trim().is_empty()
    }

    pub(super) fn prepare_backup(&mut self) {
        self.runtime_update.mode = runtime_policy::Mode::Off;
        for store in &mut self.stores {
            store.prepare_backup();
        }
    }

    pub(super) fn profile(&self, role: Role) -> Option<&ProfileRef> {
        self.roles[role as usize].as_ref()
    }

    pub(super) fn journal_embedding(&self, root: Option<&Path>) -> Option<(Profile, String)> {
        if let Some(model) = &self.managed_embedding {
            let profile = Profile::managed_embedding(root?.to_owned(), model).ok()?;
            return Some((
                profile,
                format!("{model} · {MANAGED_EMBEDDING_DESTINATION}"),
            ));
        }
        let profile = Profile::new(&self.endpoint, &self.embedding).ok()?;
        Some((profile, format!("{} · {}", self.embedding, self.endpoint)))
    }

    pub(super) fn retain_profiles(&mut self, has_profile: impl Fn(&ProfileRef) -> bool) {
        for role in &mut self.roles {
            if role
                .as_ref()
                .is_some_and(|reference| !has_profile(reference))
            {
                *role = None;
            }
        }
        for store in &mut self.stores {
            for binding in [&mut store.text_profile, &mut store.coding_profile] {
                if binding
                    .as_ref()
                    .is_some_and(|reference| !has_profile(reference))
                {
                    *binding = None;
                }
            }
        }
    }

    pub(super) fn new_profile_bindings<'a>(
        &'a self,
        previous: &'a Self,
    ) -> impl Iterator<Item = &'a ProfileRef> + 'a {
        self.roles
            .iter()
            .zip(&previous.roles)
            .filter_map(|(next, old)| (next != old).then_some(next.as_ref()).flatten())
            .chain(self.stores.iter().flat_map(|store| {
                let old = previous.stores.iter().find(|old| match store.id() {
                    Some(id) => old.id() == Some(id),
                    None => old.id().is_none() && old.name == store.name,
                });
                [
                    (
                        store.text_profile.as_ref(),
                        old.and_then(|old| old.text_profile.as_ref()),
                    ),
                    (
                        store.coding_profile.as_ref(),
                        old.and_then(|old| old.coding_profile.as_ref()),
                    ),
                ]
                .into_iter()
                .filter_map(|(next, old)| (next != old).then_some(next).flatten())
            }))
    }

    pub(super) fn knowledge_for(
        &self,
        role: Role,
        id: ProfileId,
        root: &Path,
        profiles: &Catalog,
    ) -> Vec<retrieval::Plan> {
        let resolve = |reference: &ProfileRef| profiles.resolve_profile_reference(reference);
        if self.profile(role).and_then(resolve) != Some(id) {
            return Vec::new();
        }
        self.knowledge_plans(root, |store| {
            let bound = match role {
                Role::Text => store.text_profile.as_ref(),
                Role::Coding => store.coding_profile.as_ref(),
                Role::Agent | Role::Journal => None,
            };
            bound.and_then(resolve) == Some(id)
        })
    }

    pub(super) fn autonomous_knowledge(&self, root: &Path) -> Vec<retrieval::Plan> {
        self.knowledge_plans(root, |store| store.autonomous_enabled)
    }

    fn knowledge_plans(
        &self,
        root: &Path,
        eligible: impl Fn(&Store) -> bool,
    ) -> Vec<retrieval::Plan> {
        self.stores
            .iter()
            .filter_map(|store| {
                if !eligible(store) || !store.has_enabled_sources() {
                    return None;
                }
                let scope = self.scope(store, root).ok()?;
                let (endpoint, model, managed) = self.embedding_fields(store);
                let (endpoint, model) =
                    managed.map_or((endpoint, model), |id| (MANAGED_EMBEDDING_DESTINATION, id));
                Some(retrieval::Plan::new(
                    store.name.clone(),
                    endpoint.trim().to_owned(),
                    model.trim().to_owned(),
                    scope,
                    root.to_path_buf(),
                ))
            })
            .collect()
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.stores.len() > knowledge::MAX_STORES
            || self.endpoint.len() > 128
            || self.endpoint.chars().any(char::is_control)
            || self.embedding.len() > local_ai::MAX_MODEL_BYTES
            || self.embedding.chars().any(char::is_control)
            || self
                .managed_embedding
                .as_deref()
                .is_some_and(|id| !valid_embedding_model_id(id))
            || self
                .default_managed_model
                .as_deref()
                .is_some_and(|id| !managed_models::valid_id(id))
        {
            return Err("Invalid local AI settings".into());
        }
        Profile::new(&self.endpoint, "embedding")?;
        for (index, store) in self.stores.iter().enumerate() {
            store.validate()?;
            if self.stores[..index].iter().any(|other| {
                other.name == store.name || store.id().is_some_and(|id| other.id() == Some(id))
            }) {
                return Err("Knowledge store names and identifiers must be unique".into());
            }
        }
        Ok(())
    }

    fn scope(&self, store: &Store, root: &Path) -> Result<Scope, String> {
        let (endpoint, model, managed) = self.embedding_fields(store);
        match managed {
            Some(id) => Scope::managed(store, root, id),
            None => Scope::new(store, endpoint, model),
        }
    }

    fn embedding_fields<'a>(&'a self, store: &'a Store) -> (&'a str, &'a str, Option<&'a str>) {
        store.embedding.as_ref().map_or(
            (
                self.endpoint.as_str(),
                self.embedding.as_str(),
                self.managed_embedding.as_deref(),
            ),
            |embedding| {
                (
                    embedding.endpoint.as_str(),
                    embedding.model.as_str(),
                    embedding.managed_model.as_deref(),
                )
            },
        )
    }

    fn automatic_embedding(&self, store: &Store) -> Embedding {
        let (endpoint, model, managed_model) = self.embedding_fields(store);
        if let Some(id) = managed_model {
            return Embedding {
                endpoint: local_ai::DEFAULT_ENDPOINT.into(),
                model: String::new(),
                managed_model: Some(id.into()),
            };
        }
        Embedding {
            endpoint: endpoint.trim().into(),
            model: model.trim().into(),
            managed_model: None,
        }
    }

    fn automatic_ready(&self, store: &Store) -> bool {
        let uses_policy_model = self
            .embedding_fields(store)
            .2
            .is_some_and(valid_policy_model_id);
        store.has_automatic_sources()
            && store.automatic_embedding.as_ref() == Some(&self.automatic_embedding(store))
            && uses_policy_model == store.automatic_policy_model.is_some()
            && store.qdrant == store.automatic_qdrant
    }

    pub(super) fn merge_automatic_knowledge(&mut self, previous: &Self, updated: &Self) {
        for store in &mut self.stores {
            let same_store = |other: &&Store| {
                store
                    .id()
                    .map_or_else(|| store.name == other.name, |id| other.id() == Some(id))
            };
            let Some(before) = previous.stores.iter().find(same_store) else {
                continue;
            };
            let Some(after) = updated.stores.iter().find(same_store) else {
                continue;
            };
            for folder in &mut store.folders {
                if before.folders.contains(folder)
                    && let Some(refreshed) =
                        after.folders.iter().find(|new| new.root == folder.root)
                {
                    *folder = refreshed.clone();
                }
            }
        }
    }

    fn default_embedding(&self) -> Embedding {
        Embedding {
            endpoint: self.endpoint.clone(),
            model: self.embedding.clone(),
            managed_model: self.managed_embedding.clone(),
        }
    }

    fn index_scopes(&self, root: &Path) -> Vec<Scope> {
        self.stores
            .iter()
            .filter_map(|store| self.scope(store, root).ok())
            .collect()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Choice(Option<ProfileRef>, String);

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.1)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum EmbeddingField {
    Endpoint,
    Model,
}

#[derive(Clone, Copy)]
pub(crate) enum EmbeddingScope {
    Default,
    Store,
}

#[derive(Clone, Copy)]
pub(crate) enum RuntimeFile {
    Index,
    Signature,
    Archive,
}

#[derive(Default)]
pub(super) struct Dialog {
    cache: Cache,
    profile_dir: PathBuf,
    runtime_downloads_allowed: bool,
    model_downloads_allowed: bool,
    runtime_paths: [String; 3],
    online: online::State,
    hf: huggingface::State,
    runtime_removal: Option<runtime_package::RemovalPlan>,
    backend_repair: Option<BackendRepair>,
    managed: managed::Models,
    policy_embeddings: Option<PolicyEmbeddings>,
    policy_embedding_load: Option<ModelInventory>,
    mlx: mlx::State,
    models: Vec<String>,
    model: Option<String>,
    status: Option<String>,
    request: Option<Request>,
    generation: u64,
    selected: Option<usize>,
    store_name: String,
    source: String,
    knowledge_backend: knowledge_backend::Editor,
    folder_options: knowledge_options::Draft,
    file_size: Option<FileSizeEdit>,
    automatic_status: BTreeMap<String, String>,
    runtime_update_status: Option<String>,
    confirm_remove: bool,
    consent: bool,
    consent_policy_model: Option<[u8; 32]>,
    consent_vault_revision: Option<Weak<()>>,
    query: String,
    drafts: Vec<DraftIndex>,
    hits: Vec<String>,
    folder_review: Option<Arc<folder::Review>>,
    folder_review_page: usize,
}

struct DraftIndex {
    index: Arc<Index>,
    vault_revision: Option<Weak<()>>,
}

struct FileSizeEdit {
    path: PathBuf,
    input: String,
    edited: bool,
}

struct BackendRepair {
    model_id: String,
    model_name: String,
    required: PackageBackend,
    paths: [String; 3],
    installed: bool,
}

enum Request {
    Ai {
        _cancel: local_ai::Cancel,
    },
    Folder {
        _cancel: folder::Cancel,
    },
    RuntimeRemovalReview {
        _cancel: runtime_package::Cancel,
    },
    Runtime {
        cancel: runtime_package::Cancel,
        requires_ai: bool,
    },
}

#[derive(Clone)]
pub(crate) enum Message {
    Endpoint(String),
    Discover,
    Model(String),
    TestModel,
    RuntimePath(RuntimeFile, String),
    ImportRuntime,
    CheckRuntimeOnline,
    ChooseRuntimeOnline(runtime_package::online::Choice),
    InstallRuntimeOnline,
    PauseRuntimeDownload,
    ReviewRuntimeDownloadDiscard(bool),
    DiscardRuntimeDownload,
    RuntimeUpdateMode(runtime_policy::Mode),
    RuntimeUpdateBackend(runtime_policy::Backend),
    RuntimeDownloadProgress(u64, runtime_package::online::Progress),
    HuggingFace(huggingface::Action),
    ReviewRuntimeRemoval,
    ConfirmRuntimeRemoval,
    CancelRuntimeRemoval,
    CloseBackendRepair,
    Managed(managed::Message),
    Mlx(mlx::Action),
    Role(Role, Option<ProfileRef>),
    Embedding(String),
    EmbeddingSource(EmbeddingScope, Option<String>),
    PolicyEmbeddingsLoaded(PolicyEmbeddings),
    StoreName(String),
    CreateStore,
    SelectStore(usize),
    ConfigureStore(EmbeddingField, String),
    KnowledgeBackend {
        store: usize,
        edit: knowledge_backend::Edit,
    },
    BindKnowledgeProfile {
        store: usize,
        role: Role,
        expected_role: ProfileRef,
        binding: Option<ProfileRef>,
    },
    EnableAutonomousKnowledge {
        store: usize,
        enabled: bool,
    },
    ConfirmRemove(bool),
    RemoveStore,
    Source(String),
    FolderOptions(knowledge_options::Edit),
    ConfigureFolder(usize),
    AddSource,
    SourceEnabled {
        store: usize,
        kind: SourceKind,
        path: PathBuf,
        enabled: bool,
    },
    FileSync {
        store: usize,
        path: PathBuf,
        mode: SyncMode,
    },
    ConfigureFileSize(usize, PathBuf),
    FileSize(String),
    ApplyFileSize(usize, PathBuf),
    CancelFileSize,
    ApproveAutomaticEmbedding(usize),
    RemoveSource(usize),
    ReviewFolder,
    ReviewFolderUpdate(usize),
    ApproveFolder,
    FolderReviewPage(usize),
    DiscardFolderReview,
    RemoveFolder(usize),
    Consent(bool),
    Index {
        rebuild: bool,
    },
    Query(String),
    Search,
    Cancel,
    Finished(u64, Result<Output, String>),
    ManagedBackendLoaded(u64, Option<PackageBackend>),
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LocalAiMessage([redacted])")
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Output {
    Models(Vec<String>),
    ModelTest,
    RuntimeActivated(String, Option<PackageBackend>, bool),
    RuntimeDownloadPaused {
        received: u64,
        interrupted: bool,
    },
    RuntimeDownloadDiscarded(bool),
    RuntimeCatalog(Arc<runtime_package::online::Catalog>),
    HuggingFace(huggingface::Output),
    RuntimeRemovalReviewed(Option<runtime_package::RemovalPlan>),
    RuntimeRemoved(Option<String>),
    ManagedModels {
        models: Vec<managed_models::Model>,
        default: Result<managed::SavedDefault, String>,
        active_backend: Option<PackageBackend>,
        notice: String,
    },
    ManagedNotice(String),
    Mlx(mlx::ResultValue),
    Index(Arc<Index>, bool, UpdateSummary, bool, Option<Weak<()>>),
    FolderReview(Arc<folder::Review>),
    Hits {
        query: String,
        hits: Vec<String>,
        vault_revision: Option<Weak<()>>,
    },
}

impl Dialog {
    pub(super) fn new(profile: PathBuf) -> Self {
        Self {
            cache: Cache::new(profile.clone()),
            profile_dir: profile,
            runtime_downloads_allowed: true,
            model_downloads_allowed: true,
            ..Self::default()
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.request.is_some()
    }

    pub(super) fn install_model_catalog(
        &mut self,
        catalog: Option<Arc<local_ai::model_catalog::Catalog>>,
    ) {
        self.hf.install_model_catalog(catalog);
    }

    pub(super) fn open_managed_models(&mut self) -> Task<Message> {
        if self.request.is_some() || self.backend_repair.is_some() {
            return Task::none();
        }
        let managed = self.update_managed(managed::Message::Refresh);
        if self.policy_embedding_load.is_some() {
            return managed;
        }
        self.policy_embeddings = None;
        let root = self.profile_dir.clone();
        let (load, policy) = ModelInventory::start(
            move |cancelled| PolicyEmbeddings::load(&root, cancelled),
            || PolicyEmbeddings {
                unavailable: true,
                ..PolicyEmbeddings::default()
            },
            Message::PolicyEmbeddingsLoaded,
        );
        self.policy_embedding_load = Some(load);
        Task::batch([managed, policy])
    }

    pub(super) fn backend_repair_open(&self) -> bool {
        self.backend_repair.is_some()
    }

    pub(super) fn runtime_removal_review_open(&self) -> bool {
        match self.request {
            Some(Request::RuntimeRemovalReview { .. }) => true,
            None => self.runtime_removal.is_some() || self.mlx.runtime_removal_open(),
            Some(_) => false,
        }
    }

    pub(super) fn backend_repair_busy(&self) -> bool {
        self.backend_repair_open() && self.busy()
    }

    #[cfg(test)]
    pub(super) fn open_backend_repair_for_test(&mut self) -> Arc<std::sync::atomic::AtomicBool> {
        self.backend_repair = Some(BackendRepair {
            model_id: "0123456789abcdef0123456789abcdef".into(),
            model_name: "Fixture GPU model".into(),
            required: if cfg!(target_os = "macos") {
                PackageBackend::Metal
            } else {
                PackageBackend::Vulkan
            },
            paths: Default::default(),
            installed: false,
        });
        let (cancel, requested) = runtime_package::Cancel::test_token();
        self.request = Some(Request::Runtime {
            cancel,
            requires_ai: true,
        });
        requested
    }

    pub(super) fn has_managed_model_selection(&self) -> bool {
        self.managed.has_selection() || self.mlx.has_selection()
    }

    pub(super) fn refresh_managed_model_status(&mut self) {
        self.managed.update_status(&self.profile_dir);
        self.mlx.refresh_status(&self.profile_dir);
    }

    pub(super) fn managed_default_change(
        &self,
        message: &Message,
        state: &State,
    ) -> Option<managed::DefaultChange> {
        if self.busy() || self.managed.has_action() {
            return None;
        }
        let mark = match message {
            Message::Managed(managed::Message::SetDefault) => true,
            Message::Managed(managed::Message::ClearDefault) => false,
            _ => return None,
        };
        self.managed
            .default_change(mark, state.default_managed_model.as_deref())
    }

    pub(super) fn set_managed_default(&mut self, id: Option<String>) {
        self.managed.set_default(id);
    }

    #[cfg(test)]
    pub(super) fn save_indexes(&self, next: &State, saved: &State) -> SaveIndexes {
        self.prepare_save_indexes(next, saved, None, true).unwrap()
    }

    pub(super) fn prepare_save_indexes(
        &self,
        next: &State,
        saved: &State,
        vault: Option<&VaultWindow>,
        ai_enabled: bool,
    ) -> Result<SaveIndexes, String> {
        let retained = next.index_scopes(&self.profile_dir);
        let previous = saved.index_scopes(&self.profile_dir);
        let staged = self
            .drafts
            .iter()
            .filter_map(|draft| {
                let scope = retained.iter().find(|scope| scope.same_as(&draft.index))?;
                ((ai_enabled || scope.qdrant.is_none())
                    && !previous.iter().any(|old| old.same_snapshot(scope)))
                .then_some((draft, scope))
            })
            .map(|(draft, scope)| {
                Ok(StagedIndex {
                    index: Arc::clone(&draft.index),
                    scope: scope.clone(),
                    sources: scope.enabled_sources(),
                    vault_key: resolve_reviewed_key(scope, vault, draft.vault_revision.as_ref())?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let retired = previous
            .into_iter()
            .filter(|old| {
                old.qdrant.is_some() && !retained.iter().any(|new| new.same_snapshot(old))
            })
            .map(|scope| {
                let vault_key = scope
                    .qdrant
                    .as_ref()
                    .map_or(Ok(None), |settings| settings.resolve_vault_key(vault, None));
                RetiredIndex { scope, vault_key }
            })
            .collect();
        Ok(SaveIndexes {
            cache: self.cache.clone(),
            retained,
            staged,
            retired,
            automatic: None,
        })
    }

    pub(super) fn mark_saved(&mut self) {
        self.drafts.clear();
    }

    pub(super) fn automatic_sync_idle(&self) -> bool {
        !self.busy()
            && self.folder_review.is_none()
            && self.backend_repair.is_none()
            && self.runtime_removal.is_none()
    }

    pub(super) fn set_automatic_status(&mut self, status: BTreeMap<String, String>) {
        self.automatic_status = status;
    }

    pub(super) fn set_runtime_update_status(&mut self, status: Option<String>) {
        self.runtime_update_status = status;
    }

    pub(super) fn set_runtime_downloads_allowed(&mut self, allowed: bool) {
        self.runtime_downloads_allowed = allowed;
    }

    pub(super) fn merge_automatic_knowledge(&mut self, state: &State) {
        self.discard_stale_drafts(state);
        self.hits.clear();
        self.folder_review = None;
        self.clear_consent();
    }

    #[cfg(test)]
    pub(super) fn update(
        &mut self,
        message: Message,
        state: &mut State,
        saved: &State,
        ai_allowed: bool,
    ) -> Task<Message> {
        self.update_with_vault(message, state, saved, ai_allowed, None)
    }

    pub(super) fn update_with_vault(
        &mut self,
        message: Message,
        state: &mut State,
        saved: &State,
        ai_allowed: bool,
        vault: Option<&VaultWindow>,
    ) -> Task<Message> {
        if self.backend_repair_open()
            && !matches!(
                message,
                Message::RuntimePath(..)
                    | Message::ImportRuntime
                    | Message::CheckRuntimeOnline
                    | Message::ChooseRuntimeOnline(_)
                    | Message::InstallRuntimeOnline
                    | Message::PauseRuntimeDownload
                    | Message::ReviewRuntimeDownloadDiscard(_)
                    | Message::DiscardRuntimeDownload
                    | Message::RuntimeDownloadProgress(..)
                    | Message::CloseBackendRepair
                    | Message::Cancel
                    | Message::Finished(..)
                    | Message::ManagedBackendLoaded(..)
                    | Message::PolicyEmbeddingsLoaded(..)
            )
        {
            return Task::none();
        }
        match message {
            Message::RuntimeUpdateMode(mode)
                if self.runtime_downloads_allowed && self.request.is_none() =>
            {
                state.runtime_update.mode = mode;
            }
            Message::RuntimeUpdateBackend(backend)
                if self.runtime_downloads_allowed && self.request.is_none() =>
            {
                state.runtime_update.backend = backend;
            }
            Message::Endpoint(value) => {
                state.endpoint = limited(&value, 128).into();
                self.models.clear();
                self.model = None;
                self.invalidate_index();
                self.discard_stale_drafts(state);
                self.clear_consent();
            }
            Message::Discover if ai_allowed && self.request.is_none() => {
                return self.run(
                    local_ai::discover_with_credential(&state.endpoint, None),
                    Output::Models,
                );
            }
            Message::Model(model) => {
                if self.models.contains(&model) {
                    self.model = Some(model);
                }
            }
            Message::RuntimePath(file, path) if self.request.is_none() => {
                let paths = match &mut self.backend_repair {
                    Some(repair) if repair.installed => return Task::none(),
                    Some(repair) => &mut repair.paths,
                    None => &mut self.runtime_paths,
                };
                paths[file as usize] = limited(&path, 4096).into();
            }
            Message::ImportRuntime
                if ai_allowed
                    && self.request.is_none()
                    && self.runtime_removal.is_none()
                    && self.runtime_paths_ready() =>
            {
                return self.import_runtime();
            }
            Message::CheckRuntimeOnline if self.runtime_online_allowed(ai_allowed) => {
                self.online = online::State::default();
                return self.run_managed(
                    runtime_package::online::check(
                        self.profile_dir.clone(),
                        self.required_runtime(),
                    ),
                    "Checking the signed runtime channel…",
                    true,
                    Output::RuntimeCatalog,
                );
            }
            Message::ChooseRuntimeOnline(choice) if self.runtime_online_allowed(ai_allowed) => {
                let required = self.required_runtime();
                self.online.select(choice, required);
            }
            Message::InstallRuntimeOnline if self.runtime_online_allowed(ai_allowed) => {
                return self.install_runtime_online();
            }
            Message::PauseRuntimeDownload
                if self
                    .online
                    .progress
                    .is_some_and(|progress| progress.resumable) =>
            {
                if let Some(pause) = self.online.pause.take() {
                    pause.request();
                    self.status = Some("Saving the runtime download checkpoint…".into());
                }
            }
            Message::ReviewRuntimeDownloadDiscard(confirm) if self.request.is_none() => {
                self.online.discard_confirmed = confirm;
            }
            Message::DiscardRuntimeDownload
                if self.request.is_none() && self.online.discard_confirmed =>
            {
                self.online.discard_confirmed = false;
                return self.run_managed(
                    runtime_package::online::discard_download(self.profile_dir.clone()),
                    "Discarding the saved runtime download…",
                    false,
                    Output::RuntimeDownloadDiscarded,
                );
            }
            Message::RuntimeDownloadProgress(generation, progress)
                if generation == self.generation && self.request.is_some() =>
            {
                self.online.progress = Some(progress);
            }
            Message::ReviewRuntimeRemoval
                if self.request.is_none()
                    && self.runtime_removal.is_none()
                    && !self.mlx.runtime_removal_open() =>
            {
                return self.run_removal_review(
                    runtime_package::review_removal(self.profile_dir.clone()),
                    "Checking the installed runtime…",
                    Output::RuntimeRemovalReviewed,
                );
            }
            Message::ConfirmRuntimeRemoval if self.request.is_none() => {
                if let Some(plan) = self.runtime_removal.clone() {
                    return self.run_managed(
                        runtime_package::remove(self.profile_dir.clone(), plan),
                        "Removing installed runtime packages…",
                        false,
                        Output::RuntimeRemoved,
                    );
                }
            }
            Message::CancelRuntimeRemoval if self.runtime_removal_review_open() => {
                if self.request.is_some() {
                    self.cancel();
                } else {
                    self.runtime_removal = None;
                    self.mlx.discard_runtime_removal();
                    self.status = None;
                }
            }
            Message::CloseBackendRepair if self.backend_repair.is_some() => {
                if self.request.is_some() {
                    self.cancel();
                } else if let Some(repair) = self.backend_repair.take() {
                    self.managed
                        .return_to_model(&repair.model_id, &self.profile_dir);
                    if !repair.installed {
                        self.status = None;
                    }
                }
            }
            Message::Managed(message)
                if self.request.is_none() && (ai_allowed || !message.requires_ai()) =>
            {
                return if matches!(message, managed::Message::Refresh) {
                    self.open_managed_models()
                } else {
                    self.update_managed(message)
                };
            }
            Message::Mlx(action)
                if self.request.is_none() && (ai_allowed || !action.requires_ai(&self.mlx)) =>
            {
                return self.update_mlx(action);
            }
            Message::TestModel if ai_allowed && self.request.is_none() => {
                if let Some(model) = &self.model {
                    let pending = Profile::new(&state.endpoint, model).and_then(|profile| {
                        local_ai::start(profile, |client| {
                            client
                                .complete_text_before(
                                    "Reply briefly.",
                                    "This is a connectivity test. Reply OK.",
                                    false,
                                    Instant::now() + Duration::from_secs(30),
                                )
                                .map(|_| ())
                                .map_err(local_ai::Error::message)
                        })
                    });
                    return self.run(pending, |()| Output::ModelTest);
                }
            }
            Message::Role(role, profile) => state.roles[role as usize] = profile,
            Message::Embedding(model) => {
                state.embedding = limited(&model, local_ai::MAX_MODEL_BYTES).into();
                self.invalidate_index();
                self.discard_stale_drafts(state);
                self.clear_consent();
            }
            Message::EmbeddingSource(scope, id)
                if self.request.is_none()
                    && id.as_deref().is_none_or(|id| self.embedding_available(id)) =>
            {
                match scope {
                    EmbeddingScope::Default => state.managed_embedding = id,
                    EmbeddingScope::Store => {
                        let defaults = state.default_embedding();
                        let Some(store) =
                            self.selected.and_then(|index| state.stores.get_mut(index))
                        else {
                            return Task::none();
                        };
                        store.embedding.get_or_insert(defaults).managed_model = id;
                    }
                }
                self.invalidate_index();
                self.discard_stale_drafts(state);
                self.clear_consent();
            }
            Message::PolicyEmbeddingsLoaded(models) => {
                self.policy_embedding_load = None;
                self.policy_embeddings = Some(models);
            }
            Message::StoreName(name) => self.store_name = limited(&name, 128).into(),
            Message::CreateStore => {
                let name = self.store_name.trim();
                if !name.is_empty()
                    && !name.chars().any(char::is_control)
                    && state.stores.len() < knowledge::MAX_STORES
                    && !state.stores.iter().any(|store| store.name == name)
                {
                    match Store::new(name) {
                        Ok(mut store) => {
                            store.embedding = Some(state.default_embedding());
                            state.stores.push(store);
                        }
                        Err(error) => {
                            self.status = Some(format!("Could not create the store ({error})."));
                            return Task::none();
                        }
                    }
                    self.select_store(Some(state.stores.len() - 1));
                    self.store_name.clear();
                } else {
                    self.status =
                        Some("Enter a unique store name. Up to 32 stores are supported.".into());
                }
            }
            Message::SelectStore(index) if index < state.stores.len() => {
                self.select_store(Some(index));
            }
            Message::ConfigureStore(field, value) => {
                let defaults = state.default_embedding();
                if let Some(store) = self.selected.and_then(|index| state.stores.get_mut(index)) {
                    let embedding = store.embedding.get_or_insert(defaults);
                    match field {
                        EmbeddingField::Endpoint => {
                            embedding.endpoint = limited(&value, 128).into()
                        }
                        EmbeddingField::Model => {
                            embedding.model = limited(&value, local_ai::MAX_MODEL_BYTES).into()
                        }
                    }
                    self.invalidate_index();
                    self.discard_stale_drafts(state);
                    self.clear_consent();
                }
            }
            Message::KnowledgeBackend { store, edit }
                if self.selected == Some(store) && !self.busy() =>
            {
                if let knowledge_backend::Edit::VaultKey(Some(id)) = &edit
                    && !vault.is_some_and(|vault| {
                        vault
                            .choices(VaultKind::QdrantApiKey)
                            .iter()
                            .any(|choice| &choice.id == id)
                    })
                {
                    self.status = Some("Unlock the vault and choose a Qdrant API key.".into());
                    return Task::none();
                }
                if let Some(store) = state.stores.get_mut(store)
                    && self.knowledge_backend.apply(store, edit)
                {
                    store.automatic_embedding = None;
                    store.automatic_policy_model = None;
                    store.automatic_qdrant = None;
                    store.automatic_qdrant_expanded = false;
                    let error = knowledge_backend::validate(store).err();
                    self.invalidate_index();
                    self.discard_stale_drafts(state);
                    self.clear_consent();
                    self.status = error;
                }
            }
            Message::BindKnowledgeProfile {
                store,
                role,
                expected_role,
                binding,
            } if self.selected == Some(store) && state.profile(role) == Some(&expected_role) => {
                if let Some(store) = state.stores.get_mut(store) {
                    match role {
                        Role::Text => store.text_profile = binding,
                        Role::Coding => store.coding_profile = binding,
                        Role::Agent | Role::Journal => {}
                    }
                }
            }
            Message::EnableAutonomousKnowledge { store, enabled }
                if self.selected == Some(store) && self.request.is_none() =>
            {
                if let Some(store) = state.stores.get_mut(store) {
                    store.autonomous_enabled = enabled;
                }
            }
            Message::ConfirmRemove(value) => self.confirm_remove = value,
            Message::RemoveStore if self.confirm_remove => {
                if let Some(index) = self.selected.filter(|&index| index < state.stores.len()) {
                    state.stores.remove(index);
                    self.discard_stale_drafts(state);
                    self.select_store(None);
                }
            }
            Message::Source(value) if !self.busy() => {
                self.source = limited(&value, 4096).into();
                self.folder_review = None;
            }
            Message::FolderOptions(edit) if !self.busy() => {
                if matches!(&edit, knowledge_options::Edit::MaxFileBytes(_))
                    && self.folder_review.is_some()
                {
                    self.status = Some(
                        "File size limit changed. Review the folder again before approving it."
                            .into(),
                    );
                }
                self.folder_options.update(edit);
                self.folder_review = None;
            }
            Message::ConfigureFolder(index) if !self.busy() => {
                if let Some(folder) = self
                    .selected
                    .and_then(|selected| state.stores.get(selected))
                    .and_then(|store| store.folders.get(index))
                {
                    self.source = folder.root.display().to_string();
                    self.folder_options.set(&folder.options);
                    self.folder_review = None;
                    self.status = Some(
                        "Edit the folder options, then review and approve the resulting file list."
                            .into(),
                    );
                }
            }
            Message::AddSource => {
                if let Some(store) = self.selected.and_then(|index| state.stores.get_mut(index)) {
                    let result = knowledge_backend::validate(store)
                        .and_then(|()| store.add_source(PathBuf::from(self.source.trim())));
                    match result {
                        Ok(()) => {
                            self.source.clear();
                            self.invalidate_index();
                            self.discard_stale_drafts(state);
                            self.clear_consent();
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::SourceEnabled {
                store,
                kind,
                path,
                enabled,
            } if self.selected == Some(store) && !self.busy() => {
                if let Some(store) = state.stores.get_mut(store)
                    && store.set_source_enabled(kind, path, enabled)
                {
                    self.invalidate_index();
                    self.clear_consent();
                    self.status = Some(if enabled {
                        "Source enabled in the draft. Save to apply the change.".into()
                    } else {
                        "Source disabled in the draft. Save to exclude it from retrieval.".into()
                    });
                }
            }
            Message::RemoveSource(source) => {
                if let Some(store) = self.selected.and_then(|index| state.stores.get_mut(index))
                    && source < store.sources.len()
                {
                    if self
                        .file_size
                        .as_ref()
                        .is_some_and(|edit| edit.path == store.sources[source])
                    {
                        self.file_size = None;
                    }
                    store.remove_file(source);
                    self.invalidate_index();
                    self.discard_stale_drafts(state);
                    self.clear_consent();
                }
            }
            Message::FileSync { store, path, mode }
                if self.selected == Some(store) && !self.busy() =>
            {
                if let Some(store) = state.stores.get_mut(store)
                    && store.set_file_sync(path, mode)
                {
                    self.status =
                        Some("Source sync mode changed in the draft. Save to apply it.".into());
                }
            }
            Message::ConfigureFileSize(store, path)
                if self.selected == Some(store) && !self.busy() =>
            {
                if let Some(store) = state.stores.get(store)
                    && store.sources.contains(&path)
                {
                    let input = knowledge_options::file_size_text(store.file_size_limit(&path));
                    self.file_size = Some(FileSizeEdit {
                        path,
                        input,
                        edited: false,
                    });
                }
            }
            Message::FileSize(value) if !self.busy() => {
                if let Some(edit) = &mut self.file_size {
                    edit.input = limited(&value, 32).into();
                    edit.edited = true;
                }
            }
            Message::ApplyFileSize(store, path) if self.selected == Some(store) && !self.busy() => {
                if let Some(edit) = self.file_size.as_ref().filter(|edit| edit.path == path)
                    && let Some(store) = state.stores.get_mut(store)
                {
                    let automatic = store.file_sync(&path) == SyncMode::Automatic;
                    let result = if edit.edited {
                        knowledge_options::file_size(&edit.input)
                            .and_then(|limit| store.set_file_size_limit(path, limit))
                    } else {
                        Ok(false)
                    };
                    match result {
                        Ok(changed) => {
                            self.file_size = None;
                            if changed {
                                self.invalidate_index();
                                self.discard_stale_drafts(state);
                                self.clear_consent();
                                self.status = Some(if automatic {
                                    "File size limit changed. Select Automatic again to approve background sync at this limit. Update the index and save to apply it."
                                } else {
                                    "File size limit changed in the draft. Update the index and save to apply it."
                                }.into());
                            }
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::CancelFileSize => self.file_size = None,
            Message::ApproveAutomaticEmbedding(store)
                if ai_allowed && self.selected == Some(store) && !self.busy() =>
            {
                if let Some(selected) = state.stores.get(store)
                    && selected.has_automatic_sources()
                {
                    let review = state.scope(selected, &self.profile_dir).and_then(|scope| {
                        self.review_policy_embedding(
                            state.embedding_fields(selected).2,
                            scope.profile.policy_authority_fingerprint(),
                        )
                    });
                    match review {
                        Ok(fingerprint) => {
                            let embedding = state.automatic_embedding(selected);
                            let qdrant = selected.qdrant.clone();
                            self.status = Some(if qdrant.is_some() {
                                "Automatic embedding and Qdrant destinations approved in the draft. Save to enable them."
                            } else {
                                "Automatic embedding destination approved in the draft. Save to enable it."
                            }.into());
                            state.stores[store].automatic_embedding = Some(embedding);
                            state.stores[store].automatic_policy_model = fingerprint;
                            state.stores[store].automatic_qdrant_expanded = qdrant.is_some();
                            state.stores[store].automatic_qdrant = qdrant;
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::ReviewFolder if !self.busy() => {
                if let Some(store) = self.selected.and_then(|index| state.stores.get(index)) {
                    return self.review_folder(store, PathBuf::from(self.source.trim()));
                }
            }
            Message::ReviewFolderUpdate(index) if !self.busy() => {
                if let Some(store) = self
                    .selected
                    .and_then(|selected| state.stores.get(selected))
                    && let Some(folder) = store.folders.get(index)
                {
                    self.folder_options.set(&folder.options);
                    return self.review_folder(store, folder.root.clone());
                }
            }
            Message::ApproveFolder => {
                if let Some(review) = &self.folder_review
                    && let Some(store) = self.selected.and_then(|index| state.stores.get_mut(index))
                {
                    let result = knowledge_backend::validate(store)
                        .and_then(|()| store.approve_folder(review));
                    match result {
                        Ok(()) => {
                            let empty = store.source_count() == 0;
                            self.source.clear();
                            self.invalidate_index();
                            self.discard_stale_drafts(state);
                            self.clear_consent();
                            self.status = Some(if empty {
                                "The approved store has no files. Save to apply the removals."
                                    .into()
                            } else {
                                "Folder review approved. Update the index, then save the store settings.".into()
                            });
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::FolderReviewPage(page) => {
                if self.folder_review.as_ref().is_some_and(|review| {
                    page < review.rows.len().div_ceil(FOLDER_REVIEW_PAGE_SIZE)
                }) {
                    self.folder_review_page = page;
                }
            }
            Message::DiscardFolderReview => {
                self.folder_review = None;
                self.status = Some("Folder review discarded.".into());
            }
            Message::RemoveFolder(folder) => {
                if let Some(store) = self.selected.and_then(|index| state.stores.get_mut(index))
                    && folder < store.folders.len()
                {
                    store.remove_folder(folder);
                    self.invalidate_index();
                    self.discard_stale_drafts(state);
                    self.clear_consent();
                    self.status =
                        Some("Folder removed from the draft. Save to apply the change.".into());
                }
            }
            Message::Consent(consent) => {
                self.clear_consent();
                if !consent {
                    self.invalidate_index();
                } else {
                    let review = self
                        .selected
                        .and_then(|index| state.stores.get(index))
                        .map_or(Ok((None, None)), |store| {
                            let managed = state.embedding_fields(store).2;
                            let fingerprint = if managed.is_some_and(valid_policy_model_id) {
                                let scope = state.scope(store, &self.profile_dir)?;
                                self.review_policy_embedding(
                                    managed,
                                    scope.profile.policy_authority_fingerprint(),
                                )?
                            } else {
                                None
                            };
                            let key = store.qdrant.as_ref().map_or(Ok(None), |settings| {
                                settings.resolve_vault_key(vault, None)
                            })?;
                            Ok((fingerprint, key))
                        });
                    match review {
                        Ok((fingerprint, key)) => {
                            self.consent = true;
                            self.consent_policy_model = fingerprint;
                            self.consent_vault_revision = key.as_deref().map(VaultKey::revision);
                        }
                        Err(error) => {
                            self.invalidate_index();
                            self.status = Some(error);
                        }
                    }
                }
            }
            Message::Index { rebuild }
                if ai_allowed
                    && self.consent
                    && self.request.is_none()
                    && self.folder_review.is_none() =>
            {
                if let Some(store) = self.selected.and_then(|index| state.stores.get(index))
                    && store.has_enabled_sources()
                {
                    self.hits.clear();
                    let cache = self.cache.clone();
                    let profile_dir = self.profile_dir.clone();
                    let baseline = saved.clone();
                    let pending = state.scope(store, &self.profile_dir).and_then(|scope| {
                        self.require_policy_embedding_consent(&scope)?;
                        let vault_key =
                            resolve_reviewed_key(&scope, vault, self.consent_vault_revision.as_ref())?;
                        let scopes = saved.index_scopes(&self.profile_dir);
                        if scopes.iter().any(|old| old.same_snapshot(&scope) && old != &scope) {
                            return Err("Save the source availability changes before updating this index.".into());
                        }
                        let persistent = scopes.contains(&scope);
                        let base = scopes.into_iter().find(|old| scope.can_reuse(old));
                        let draft = self
                            .drafts
                            .iter()
                            .find(|draft| !persistent && scope.same_as(&draft.index))
                            .map(|draft| Arc::clone(&draft.index));
                        local_ai::start(scope.profile.clone(), move |client| {
                            let authorize = || {
                                knowledge::check_active(client)?;
                                scope.profile.require_current_policy_authority()?;
                                if let Some(settings) = &scope.qdrant {
                                    remote_index::require_policy()?;
                                    settings.require_vault_key(vault_key.as_deref())?;
                                }
                                Configuration::require_local_settings(&profile_dir, &baseline)
                            };
                            let previous = if let Some(index) = draft {
                                Some((scope.clone(), index))
                            } else if let Some(base) = base {
                                match cache.load(&base) {
                                    Ok(index) => Some((base, Arc::new(index))),
                                    Err(error) if rebuild || error.kind() == io::ErrorKind::NotFound => None,
                                    Err(error) => return Err(format!(
                                        "Could not load the previous knowledge index ({error}). Choose Rebuild index to replace it."
                                    )),
                                }
                            } else {
                                None
                            };
                            authorize()?;
                            let (index, summary) = Index::update(
                                client,
                                &scope,
                                previous
                                    .as_ref()
                                    .map(|(base, index)| (base, index.as_ref())),
                                rebuild,
                                authorize,
                            )?;
                            let remote_missing = scope.qdrant.is_some()
                                && previous.as_ref().and_then(|(base, index)| {
                                    base.same_snapshot(&scope).then_some(index.qdrant_receipt.as_ref()).flatten()
                                }).is_none_or(|receipt| !receipt.sources.covers(&scope.enabled_sources()));
                            drop(previous);
                            let mut cleanup_warning = false;
                            if persistent && (summary.indexed != 0 || summary.removed != 0 || remote_missing) {
                                cleanup_warning = Configuration::publish_knowledge_index(&profile_dir, |current| {
                                    if !current.index_scopes(&profile_dir).contains(&scope) {
                                        return Err(io::Error::other("The saved knowledge settings changed. Update the index again."));
                                    }
                                    remote_index::publish(&cache, &scope, &index, scope.enabled_sources(), Some(client), vault_key.clone(), authorize)
                                })
                                    .map_err(|error| {
                                        format!("Could not save the knowledge index ({error}).")
                                    })?.cleanup_warning;
                            }
                            let revision = vault_key.as_deref().map(VaultKey::revision);
                            Ok(Output::Index(Arc::new(index), persistent, summary, cleanup_warning, revision))
                        })
                    });
                    return self.run(pending, |output| output);
                }
            }
            Message::Query(query) => {
                self.query = limited(&query, 2048).into();
                self.hits.clear();
            }
            Message::Search
                if ai_allowed
                    && self.consent
                    && self.request.is_none()
                    && self.folder_review.is_none() =>
            {
                if let Some(store) = self.selected.and_then(|index| state.stores.get(index))
                    && store.has_enabled_sources()
                {
                    self.hits.clear();
                    let cache = self.cache.clone();
                    let query = self.query.trim().to_owned();
                    let profile_dir = self.profile_dir.clone();
                    let baseline = saved.clone();
                    let pending = state.scope(store, &self.profile_dir).and_then(|scope| {
                        self.require_policy_embedding_consent(&scope)?;
                        let vault_key =
                            resolve_reviewed_key(&scope, vault, self.consent_vault_revision.as_ref())?;
                        let index = self.drafts.iter().find(|draft| scope.same_as(&draft.index))
                            .map(|draft| Arc::clone(&draft.index));
                        local_ai::start(scope.profile.clone(), move |client| {
                            Configuration::require_local_settings(&profile_dir, &baseline)?;
                            let (index, stamp) = match index {
                                Some(index) => (index, None),
                                None => {
                                    let (index, stamp) = cache.load_stamped(&scope).map_err(|error| {
                                    if error.kind() == io::ErrorKind::NotFound {
                                        "No saved index matches these settings. Choose Update index.".into()
                                    } else {
                                        format!("Could not load the saved index ({error}). Choose Update index.")
                                    }
                                    })?;
                                    (Arc::new(index), Some(stamp))
                                }
                            };
                            let vault_revision = vault_key.as_deref().map(VaultKey::revision);
                            let hits = index.search_with_vault(client, &scope, &query, vault_key, || {
                                scope.profile.require_current_policy_authority()?;
                                Configuration::require_local_settings(&profile_dir, &baseline)?;
                                if let Some(stamp) = &stamp {
                                    cache.require_current(&scope, stamp).map_err(|_| "The knowledge index changed. Search again.")?;
                                }
                                Ok(())
                            })?;
                            scope.profile.require_current_policy_authority()?;
                            Configuration::require_local_settings(&profile_dir, &baseline)?;
                            Ok(Output::Hits { query, hits, vault_revision })
                        })
                    });
                    return self.run(pending, |output| output);
                }
            }
            Message::Cancel => self.cancel(),
            Message::ManagedBackendLoaded(generation, backend) if generation == self.generation => {
                self.managed.set_active_backend(backend);
            }
            Message::Finished(generation, result) if generation == self.generation => {
                let request = self.request.take();
                self.online.progress = None;
                self.online.pause = None;
                self.hf.pause = None;
                self.hf.progress = None;
                let refresh_backend =
                    matches!(&request, Some(Request::Runtime { .. })) && result.is_err();
                let allowed_without_ai = matches!(
                    request,
                    Some(
                        Request::Folder { .. }
                            | Request::RuntimeRemovalReview { .. }
                            | Request::Runtime {
                                requires_ai: false,
                                ..
                            }
                    )
                );
                if !ai_allowed && !allowed_without_ai {
                    self.invalidate_index();
                    self.status = None;
                    return Task::none();
                }
                self.status = Some(match result {
                    Ok(Output::Models(models)) => {
                        self.model = self.model.take().filter(|model| models.contains(model));
                        self.models = models;
                        format!("{} installed models", self.models.len())
                    }
                    Ok(Output::ModelTest) => "The model replied to the connection test.".into(),
                    Ok(Output::HuggingFace(output)) => self.finish_huggingface(output),
                    Ok(Output::RuntimeCatalog(catalog)) => {
                        let count = catalog.choices().len();
                        self.online.catalog = Some(catalog);
                        if count == 0 {
                            "The signed channel has no compatible package for this host and backend.".into()
                        } else {
                            format!(
                                "Found {count} compatible signed runtime packages. Choose a package to review."
                            )
                        }
                    }
                    Ok(Output::RuntimeDownloadPaused {
                        received,
                        interrupted,
                    }) => {
                        let action = if interrupted { "Interrupted" } else { "Paused" };
                        format!(
                            "{action} after {received} bytes. Use Download or resume for the same signed package to continue."
                        )
                    }
                    Ok(Output::RuntimeDownloadDiscarded(true)) => {
                        "Discarded the saved runtime download.".into()
                    }
                    Ok(Output::RuntimeDownloadDiscarded(false)) => {
                        "There is no saved runtime download.".into()
                    }
                    Ok(Output::RuntimeActivated(id, backend, cleanup_failed)) => {
                        self.online.selected = None;
                        self.managed.set_active_backend(backend);
                        match &mut self.backend_repair {
                            Some(repair) if backend != Some(repair.required) => {
                                repair.installed = false;
                                format!(
                                    "Could not confirm the required signed {} runtime as active. Check the package and try again.",
                                    repair.required
                                )
                            }
                            repair => {
                                if let Some(repair) = repair {
                                    repair.installed = true;
                                }
                                let cleanup = if cleanup_failed {
                                    " The saved download could not be removed. Use Discard saved runtime download to remove it."
                                } else {
                                    ""
                                };
                                format!(
                                    "Installed {id}. Test a managed model to confirm activation.{cleanup}"
                                )
                            }
                        }
                    }
                    Ok(Output::RuntimeRemovalReviewed(plan)) => {
                        let found = plan.is_some();
                        self.runtime_removal = plan;
                        if found {
                            "Review removal of the installed runtime.".into()
                        } else {
                            "There are no installed llama.cpp runtime packages.".into()
                        }
                    }
                    Ok(Output::RuntimeRemoved(warning)) => {
                        self.runtime_removal = None;
                        self.managed.set_active_backend(None);
                        self.refresh_managed_model_status();
                        warning.unwrap_or_else(|| "Removed the installed llama.cpp runtime.".into())
                    }
                    Ok(Output::ManagedModels {
                        models,
                        default,
                        active_backend,
                        notice,
                    }) => {
                        self.managed.set_active_backend(active_backend);
                        let (default, status) = match default {
                            Ok(default) => (Some(default), notice),
                            Err(error) => (
                                None,
                                format!("{notice}. Could not read the default model ({error})."),
                            ),
                        };
                        self.managed.set_models(
                            models,
                            default,
                            state.default_managed_model.as_deref(),
                            &self.profile_dir,
                        );
                        status
                    }
                    Ok(Output::ManagedNotice(notice)) => {
                        self.refresh_managed_model_status();
                        notice
                    }
                    Ok(Output::Mlx(output)) => self.mlx.finish(output, &self.profile_dir),
                    Ok(Output::FolderReview(review)) => {
                        self.folder_review = Some(review);
                        self.folder_review_page = 0;
                        "Folder scan complete. Review the selection before approving it.".into()
                    }
                    Ok(Output::Index(index, persistent, summary, cleanup_warning, revision)) => {
                        let mut detail = format!(
                            "{} text passages · files reused {}, indexed {}, removed {}",
                            index.len(),
                            summary.reused,
                            summary.indexed,
                            summary.removed,
                        );
                        if cleanup_warning {
                            detail
                                .push_str(". The obsolete Qdrant generation could not be removed");
                        }
                        self.hits.clear();
                        if persistent && summary.indexed == 0 && summary.removed == 0 {
                            format!("The saved index is current. {detail}.")
                        } else if persistent {
                            format!("Saved {detail}.")
                        } else if let Err(error) =
                            revision.as_ref().map_or(Ok(()), require_vault_revision)
                        {
                            self.clear_consent();
                            error
                        } else {
                            self.retain_draft(state, index, revision, &detail)
                        }
                    }
                    Ok(Output::Hits {
                        query,
                        hits,
                        vault_revision,
                    }) if query == self.query.trim() => {
                        match vault_revision
                            .as_ref()
                            .map_or(Ok(()), require_vault_revision)
                        {
                            Ok(()) => {
                                self.hits = hits;
                                format!("{} matching passages", self.hits.len())
                            }
                            Err(error) => {
                                self.clear_consent();
                                error
                            }
                        }
                    }
                    Ok(Output::Hits { .. }) => {
                        self.status = None;
                        return Task::none();
                    }
                    Err(error) => error,
                });
                if refresh_backend {
                    let profile = self.profile_dir.clone();
                    return Task::perform(
                        async move {
                            runtime_package::active_package_backend(&profile)
                                .ok()
                                .flatten()
                        },
                        move |backend| Message::ManagedBackendLoaded(generation, backend),
                    );
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn run<T: Send + 'static>(
        &mut self,
        pending: Result<Pending<T>, String>,
        output: impl FnOnce(T) -> Output + Send + 'static,
    ) -> Task<Message> {
        let Pending { reply, cancel } = match pending {
            Ok(pending) => pending,
            Err(error) => {
                self.status = Some(error);
                return Task::none();
            }
        };
        self.receive(reply, Request::Ai { _cancel: cancel }, "Working…", output)
    }

    fn review_folder(&mut self, store: &Store, root: PathBuf) -> Task<Message> {
        let pending = knowledge_backend::validate(store)
            .and_then(|()| self.folder_options.options())
            .and_then(|options| store.folder_review_options(root, options));
        let (reply, cancel) = match pending {
            Ok(pending) => pending,
            Err(error) => {
                self.status = Some(error);
                return Task::none();
            }
        };
        self.invalidate_index();
        self.clear_consent();
        self.receive(
            reply,
            Request::Folder { _cancel: cancel },
            "Checking the selected folder…",
            Output::FolderReview,
        )
    }

    fn import_runtime(&mut self) -> Task<Message> {
        let paths = self.runtime_import();
        let profile = self.profile_dir.clone();
        self.run_managed(
            runtime_package::start(profile.clone(), paths),
            "Installing and checking the signed runtime package…",
            true,
            move |id| {
                Output::RuntimeActivated(
                    id,
                    runtime_package::active_package_backend(&profile)
                        .ok()
                        .flatten(),
                    false,
                )
            },
        )
    }

    fn required_runtime(&self) -> Option<PackageBackend> {
        self.backend_repair.as_ref().map(|repair| repair.required)
    }

    fn runtime_online_allowed(&self, ai_allowed: bool) -> bool {
        ai_allowed
            && self.runtime_downloads_allowed
            && self.request.is_none()
            && self.runtime_removal.is_none()
            && self
                .backend_repair
                .as_ref()
                .is_none_or(|repair| !repair.installed)
    }

    fn install_runtime_online(&mut self) -> Task<Message> {
        let Some((catalog, choice)) = self
            .online
            .catalog
            .clone()
            .zip(self.online.selected.clone())
        else {
            return Task::none();
        };
        if self
            .required_runtime()
            .is_some_and(|required| required != choice.backend)
        {
            return Task::none();
        }
        let profile = self.profile_dir.clone();
        let download = match runtime_package::online::install(profile.clone(), catalog, choice) {
            Ok(download) => download,
            Err(error) => {
                self.status = Some(error);
                return Task::none();
            }
        };
        let result = self.run_managed(
            Ok(download.pending),
            "Downloading or verifying the saved signed runtime package…",
            true,
            move |outcome| match outcome {
                runtime_package::online::InstallOutcome::Installed { id, cleanup_failed } => {
                    Output::RuntimeActivated(
                        id,
                        runtime_package::active_package_backend(&profile)
                            .ok()
                            .flatten(),
                        cleanup_failed,
                    )
                }
                runtime_package::online::InstallOutcome::Paused {
                    received,
                    interrupted,
                } => Output::RuntimeDownloadPaused {
                    received,
                    interrupted,
                },
            },
        );
        self.online.pause = Some(download.pause);
        let generation = self.generation;
        Task::batch([
            result,
            Task::run(download.progress, move |progress| {
                Message::RuntimeDownloadProgress(generation, progress)
            }),
        ])
    }

    fn runtime_import(&self) -> runtime_package::Import {
        let [index, signature, archive] = self.runtime_paths().each_ref().map(|path| path.trim());
        runtime_package::Import {
            index: index.into(),
            signature: signature.into(),
            archive: archive.into(),
            expected_backend: self.backend_repair.as_ref().map(|repair| repair.required),
        }
    }

    fn runtime_paths(&self) -> &[String; 3] {
        self.backend_repair
            .as_ref()
            .map_or(&self.runtime_paths, |repair| &repair.paths)
    }

    fn runtime_paths_ready(&self) -> bool {
        self.backend_repair
            .as_ref()
            .is_none_or(|repair| !repair.installed)
            && self
                .runtime_paths()
                .iter()
                .all(|path| !path.trim().is_empty())
    }

    fn run_managed<T: Send + 'static>(
        &mut self,
        pending: Result<runtime_package::Pending<T>, String>,
        status: &'static str,
        requires_ai: bool,
        output: impl FnOnce(T) -> Output + Send + 'static,
    ) -> Task<Message> {
        match pending {
            Ok(runtime_package::Pending { reply, cancel }) => self.receive(
                reply,
                Request::Runtime {
                    cancel,
                    requires_ai,
                },
                status,
                output,
            ),
            Err(error) => {
                self.status = Some(error);
                Task::none()
            }
        }
    }

    fn run_removal_review<T: Send + 'static>(
        &mut self,
        pending: Result<runtime_package::Pending<T>, String>,
        status: &'static str,
        output: impl FnOnce(T) -> Output + Send + 'static,
    ) -> Task<Message> {
        match pending {
            Ok(runtime_package::Pending { reply, cancel }) => self.receive(
                reply,
                Request::RuntimeRemovalReview { _cancel: cancel },
                status,
                output,
            ),
            Err(error) => {
                self.status = Some(error);
                Task::none()
            }
        }
    }

    fn receive<T: Send + 'static>(
        &mut self,
        reply: async_channel::Receiver<Result<T, String>>,
        request: Request,
        status: &'static str,
        output: impl FnOnce(T) -> Output + Send + 'static,
    ) -> Task<Message> {
        self.generation = self.generation.wrapping_add(1);
        self.request = Some(request);
        self.status = Some(status.into());
        let generation = self.generation;
        Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The local worker stopped".into()))
                    .map(output)
            },
            move |result| Message::Finished(generation, result),
        )
    }

    fn cancel(&mut self) {
        if let Some(Request::Runtime { cancel, .. }) = &self.request {
            cancel.request();
            self.status = Some("Managed model operation cancellation requested.".into());
            return;
        }
        if self.request.take().is_some() {
            self.generation = self.generation.wrapping_add(1);
            self.status = Some("Cancelled.".into());
        }
    }

    fn invalidate_index(&mut self) {
        self.cancel();
        self.folder_review = None;
        self.hits.clear();
    }

    fn clear_consent(&mut self) {
        self.consent = false;
        self.consent_policy_model = None;
        self.consent_vault_revision = None;
    }

    fn review_policy_embedding(
        &self,
        id: Option<&str>,
        current: Option<[u8; 32]>,
    ) -> Result<Option<[u8; 32]>, String> {
        let Some(id) = id.filter(|id| valid_policy_model_id(id)) else {
            return Ok(None);
        };
        if current.is_some() && current == self.policy_embedding_fingerprint(id) {
            Ok(current)
        } else {
            Err(POLICY_EMBEDDING_CHANGED.into())
        }
    }

    fn require_policy_embedding_consent(&self, scope: &Scope) -> Result<(), String> {
        if self.consent_policy_model == scope.profile.policy_authority_fingerprint() {
            Ok(())
        } else {
            Err(POLICY_EMBEDDING_CHANGED.into())
        }
    }

    fn policy_embedding_fingerprint(&self, id: &str) -> Option<[u8; 32]> {
        self.policy_embeddings
            .as_ref()?
            .choices
            .iter()
            .find(|model| model.id == id)
            .map(|model| model.fingerprint)
    }

    fn embedding_available(&self, id: &str) -> bool {
        self.policy_embedding_fingerprint(id).is_some()
            || self
                .policy_embeddings
                .as_ref()
                .is_some_and(|models| models.user_allowed)
                && self.managed.embedding_models().any(|model| model.id == id)
    }

    fn embedding_options(&self) -> Vec<EmbeddingChoice> {
        let mut options = vec![EmbeddingChoice {
            id: None,
            label: "External local service (Ollama)".into(),
        }];
        if let Some(models) = &self.policy_embeddings {
            options.extend(models.choices.iter().map(|model| EmbeddingChoice {
                id: Some(model.id.clone()),
                label: format!("{} (Managed by policy)", model.name),
            }));
            if models.user_allowed {
                options.extend(
                    self.managed
                        .embedding_models()
                        .map(|model| EmbeddingChoice {
                            id: Some(model.id.clone()),
                            label: format!("{} · {}", model.name, &model.id[..8]),
                        }),
                );
            }
        }
        options
    }

    fn consent_current(&self) -> bool {
        self.consent
            && self
                .consent_vault_revision
                .as_ref()
                .is_none_or(|revision| require_vault_revision(revision).is_ok())
    }

    fn discard_stale_drafts(&mut self, state: &State) {
        let scopes = state.index_scopes(&self.profile_dir);
        self.drafts
            .retain(|draft| scopes.iter().any(|scope| scope.same_as(&draft.index)));
    }

    fn retain_draft(
        &mut self,
        state: &State,
        index: Arc<Index>,
        vault_revision: Option<Weak<()>>,
        detail: &str,
    ) -> String {
        let Some(scope) = self
            .selected
            .and_then(|selected| state.stores.get(selected))
            .and_then(|store| state.scope(store, &self.profile_dir).ok())
            .filter(|scope| scope.same_as(&index))
        else {
            return "The knowledge settings changed. Update the index again.".into();
        };
        let used = self
            .drafts
            .iter()
            .filter(|draft| !scope.same_as(&draft.index))
            .map(|draft| draft.index.bytes())
            .sum::<usize>();
        let bytes = index.bytes();
        if bytes > MAX_DRAFT_INDEX_BYTES {
            return "Save the knowledge store settings first, then update this large index.".into();
        }
        if used + bytes > MAX_DRAFT_INDEX_BYTES {
            return "Save the existing draft indexes before updating another store.".into();
        }
        self.drafts.retain(|draft| !scope.same_as(&draft.index));
        self.drafts.push(DraftIndex {
            index,
            vault_revision,
        });
        format!("Prepared {detail}. Save the settings to keep the index.")
    }

    pub(super) fn suspend(&mut self) {
        self.invalidate_index();
        self.drafts.clear();
        self.clear_consent();
        self.managed.dismiss_action();
        self.mlx.invalidate_review();
        self.online = online::State::default();
        self.hf = huggingface::State::default();
    }

    fn select_store(&mut self, index: Option<usize>) {
        self.invalidate_index();
        self.selected = index;
        self.confirm_remove = false;
        self.clear_consent();
        self.status = None;
        self.source.clear();
        self.knowledge_backend = knowledge_backend::Editor::default();
        self.folder_options = knowledge_options::Draft::default();
        self.file_size = None;
        self.query.clear();
    }

    pub(super) fn view<'a>(
        &'a self,
        page: Page,
        state: &'a State,
        profiles: &Catalog,
        editable: bool,
        ai_enabled: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let can_edit = editable && self.request.is_none();
        let can_request = can_edit && ai_enabled;
        let progress = row![
            self.status.as_ref().map(|status| {
                container(scrollable(
                    text(status.as_str()).wrapping(text::Wrapping::WordOrGlyph),
                ))
                .width(Length::Fill)
                .max_height(48)
            }),
            self.request
                .as_ref()
                .map(|_| button("Cancel request").on_press(Message::Cancel)),
            (self.online.pause.is_some()
                && self
                    .online
                    .progress
                    .is_some_and(|progress| progress.resumable))
            .then(|| button("Pause download").on_press(Message::PauseRuntimeDownload)),
            self.online.progress.map(|progress| text(format!(
                "{} / {} MiB",
                progress.received / (1024 * 1024),
                progress.total.div_ceil(1024 * 1024)
            ))
            .size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if matches!(page, Page::Models(_))
            && let Some(repair) = &self.backend_repair
        {
            return column![
                self.backend_repair_view(repair, can_edit, can_request),
                progress
            ]
            .spacing(10)
            .into();
        }
        let inventory_status: Option<Element<'_, Message>> = if matches!(page, Page::Models(_)) {
            None
        } else if self.policy_embedding_load.is_some() {
            Some(text("Loading embedding models…").size(13).into())
        } else if self
            .policy_embeddings
            .as_ref()
            .is_some_and(|models| models.unavailable)
        {
            Some(
                row![
                    text("One or more policy-managed embedding models are unavailable.").size(13),
                    button("Refresh embedding models")
                        .style(button::secondary)
                        .on_press_maybe(
                            can_edit.then_some(Message::Managed(managed::Message::Refresh))
                        ),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap()
                .into(),
            )
        } else {
            None
        };
        let page = match page {
            Page::Models(tokens) => self.models_view(state, can_edit, can_request, tokens),
            Page::Knowledge(vault_keys) => {
                self.knowledge_view(state, profiles, can_request, vault_keys)
            }
            Page::Preferences => preferences(
                state,
                profiles,
                reveal,
                self.embedding_options(),
                can_edit,
                self.runtime_downloads_allowed,
                self.runtime_update_status.as_deref(),
            ),
        };
        column![
            scrollable(page)
                .id(reveal.viewport.clone())
                .spacing(6)
                .height(Length::Fill),
            inventory_status,
            progress,
        ]
        .spacing(10)
        .into()
    }

    fn models_view<'a>(
        &'a self,
        state: &'a State,
        can_edit: bool,
        can_request: bool,
        hub_tokens: Vec<CredentialChoice>,
    ) -> Element<'a, Message> {
        let models = pick_list(self.models.as_slice(), self.model.as_ref(), Message::Model)
            .placeholder("Select an installed model")
            .width(Length::Fill)
            .dismiss_on_escape();
        column![
            text("Local Models").size(20),
            text("Discover installed models from an explicitly configured local service."),
            endpoint_field(state),
            row![
                button("Refresh").on_press_maybe(can_request.then_some(Message::Discover)),
                models,
                button("Test model").on_press_maybe(
                    (can_request && self.model.is_some()).then_some(Message::TestModel)
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text("Use a model service already running on this computer.").size(13),
            self.online.view(
                can_request && self.runtime_downloads_allowed && self.runtime_removal.is_none(),
                can_edit,
                None,
            ),
            (!self.runtime_downloads_allowed)
                .then(|| text(crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED).size(13)),
            text("Offline llama.cpp Runtime Package").size(17),
            text("Enter the absolute paths to the stable runtime index, its detached signature and the package ZIP.").size(13),
            self.runtime_fields(can_edit),
            row![
                button("Install signed runtime").on_press_maybe(
                    (can_request && self.runtime_removal.is_none() && self.runtime_paths_ready())
                        .then_some(Message::ImportRuntime),
                ),
                button("Remove installed runtime…")
                    .style(button::secondary)
                    .on_press_maybe(
                        (can_edit && self.runtime_removal.is_none() && !self.mlx.runtime_removal_open())
                            .then_some(Message::ReviewRuntimeRemoval),
                    ),
            ]
            .spacing(8),
            self.runtime_removal.as_ref().map(|_| column![
                text("Remove the installed llama.cpp runtime?").size(17),
                text("This removes the installed runtime packages. Your GGUF models, AI profiles and runtime trust settings stay available.")
                    .wrapping(text::Wrapping::WordOrGlyph),
                row![
                    button("Cancel")
                        .style(button::secondary)
                        .on_press_maybe(can_edit.then_some(Message::CancelRuntimeRemoval)),
                    button("Remove runtime")
                        .style(button::danger)
                        .on_press_maybe(can_edit.then_some(Message::ConfirmRuntimeRemoval)),
                ]
                .spacing(8),
            ]
            .spacing(8)),
            self.managed.view(can_edit, can_request, state.default_managed_model.as_deref()),
            self.mlx.view(
                can_edit,
                can_request,
                self.runtime_removal.is_none(),
                self.runtime_downloads_allowed,
            ),
            self.hf.view(
                can_request && self.runtime_removal.is_none(),
                self.model_downloads_allowed,
                hub_tokens,
            ),
        ]
        .spacing(10)
        .width(Length::Fill)
        .into()
    }

    fn backend_repair_view<'a>(
        &'a self,
        repair: &'a BackendRepair,
        can_edit: bool,
        can_request: bool,
    ) -> Element<'a, Message> {
        let body: Element<'_, Message> = if repair.installed {
            text("The package was installed. Return to the model to Start or Test.").into()
        } else {
            column![
                text("Choose the stable signed runtime index, its detached signature and the matching package ZIP using absolute paths.")
                    .wrapping(text::Wrapping::WordOrGlyph),
                self.runtime_fields(can_request),
                self.online.view(
                    can_request && self.runtime_downloads_allowed,
                    can_edit,
                    Some(repair.required),
                ),
                (!self.runtime_downloads_allowed)
                    .then(|| text(crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED).size(13)),
            ]
            .spacing(10)
            .into()
        };
        column![
            text("Install matching runtime").size(20),
            text(format!("Model: {}", repair.model_name)).wrapping(text::Wrapping::WordOrGlyph),
            text(format!(
                "Required signed package backend · {}",
                repair.required
            )),
            row![
                button(if repair.installed {
                    "Return to model"
                } else {
                    "Cancel repair"
                })
                .style(button::secondary)
                .on_press_maybe(can_edit.then_some(Message::CloseBackendRepair)),
                (!repair.installed).then(|| button("Install matching signed runtime")
                    .on_press_maybe(
                        (can_request && self.runtime_paths_ready())
                            .then_some(Message::ImportRuntime),
                    )),
            ]
            .spacing(8),
            scrollable(body).height(Length::Fill),
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    fn runtime_fields(&self, enabled: bool) -> Element<'_, Message> {
        column![
            field(
                "Signed index",
                self.runtime_path(RuntimeFile::Index, "runtime-index-v1.json", enabled)
            ),
            field(
                "Index signature",
                self.runtime_path(RuntimeFile::Signature, "runtime-index-v1.sig", enabled)
            ),
            field(
                "Runtime package",
                self.runtime_path(RuntimeFile::Archive, "Package ZIP", enabled)
            ),
        ]
        .spacing(8)
        .into()
    }

    fn runtime_path(
        &self,
        file: RuntimeFile,
        placeholder: &str,
        enabled: bool,
    ) -> Element<'_, Message> {
        text_input(placeholder, &self.runtime_paths()[file as usize])
            .on_input_maybe(enabled.then_some(move |path| Message::RuntimePath(file, path)))
            .into()
    }

    fn knowledge_view<'a>(
        &'a self,
        state: &'a State,
        profiles: &Catalog,
        can_request: bool,
        vault_keys: Vec<CredentialChoice>,
    ) -> Element<'a, Message> {
        let selected = self
            .selected
            .and_then(|index| state.stores.get(index).map(|store| (index, store)));
        let stores =
            state
                .stores
                .iter()
                .enumerate()
                .fold(column![].spacing(4), |rows, (index, store)| {
                    rows.push(
                        button(text(store.name.as_str()).wrapping(text::Wrapping::WordOrGlyph))
                            .width(Length::Fill)
                            .style(if self.selected == Some(index) {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .on_press(Message::SelectStore(index)),
                    )
                });
        let sidebar = column![
            text_input("New store name", &self.store_name)
                .on_input(Message::StoreName)
                .on_submit(Message::CreateStore),
            button("Create store").on_press(Message::CreateStore),
            stores,
        ]
        .spacing(8)
        .width(180);
        let mut details = column![].spacing(10).width(Length::Fill);
        if let Some((store_index, store)) = selected {
            let (endpoint, model, managed) = state.embedding_fields(store);
            let backend_valid = knowledge_backend::validate(store).is_ok();
            let qdrant_destination = store
                .qdrant
                .as_ref()
                .filter(|_| backend_valid)
                .map(|settings| settings.destination());
            let choices = profiles.choices();
            let bindings = [
                (Role::Text, "terminal AI chat", store.text_profile.as_ref()),
                (Role::Coding, "code analysis", store.coding_profile.as_ref()),
            ]
            .into_iter()
            .fold(column![].spacing(6), |controls, (role, use_case, bound)| {
                let profile = state.profile(role).and_then(|expected| {
                    let id = profiles.resolve_profile_reference(expected)?;
                    let (_, name) = choices.iter().find(|(candidate, _)| *candidate == id)?;
                    let binding = profiles.profile_reference(id)?;
                    Some((id, name, expected.clone(), binding))
                });
                let control: Element<'_, Message> =
                    if let Some((id, name, expected_role, binding)) = profile {
                        checkbox(
                            bound.and_then(|reference| {
                                profiles.resolve_profile_reference(reference)
                            }) == Some(id),
                        )
                        .label(format!("Offer this store for {use_case} with {name}"))
                        .on_toggle(move |enabled| Message::BindKnowledgeProfile {
                            store: store_index,
                            role,
                            expected_role: expected_role.clone(),
                            binding: enabled.then(|| binding.clone()),
                        })
                        .into()
                    } else if state.profile(role).is_some() {
                        text(format!(
                            "The selected profile for {use_case} is unavailable."
                        ))
                        .size(13)
                        .into()
                    } else {
                        text(format!(
                            "Assign a {} in Local Preferences to offer this store for {use_case}.",
                            role.label()
                        ))
                        .size(13)
                        .into()
                    };
                controls.push(control)
            });
            let can_review = self.request.is_none();
            let can_index = can_request
                && backend_valid
                && self.folder_review.is_none()
                && self.consent_current()
                && store.has_enabled_sources()
                && (managed.is_some() || !model.trim().is_empty());
            let sources = store.sources.iter().enumerate().fold(
                column![].spacing(6),
                |rows, (index, source)| {
                    let enabled = store.source_enabled(SourceKind::File, source);
                    rows.push(
                        column![
                            row![
                                text(source.display().to_string())
                                    .wrapping(text::Wrapping::WordOrGlyph)
                                    .width(Length::Fill),
                                can_review.then(|| {
                                    pick_list(
                                        SyncMode::ALL,
                                        Some(store.file_sync(source)),
                                        move |mode| Message::FileSync {
                                            store: store_index,
                                            path: source.clone(),
                                            mode,
                                        },
                                    )
                                    .dismiss_on_escape()
                                }),
                                button("Size limit…").on_press_maybe(can_review.then(|| {
                                    Message::ConfigureFileSize(store_index, source.clone())
                                })),
                                button(if enabled { "Disable" } else { "Enable" }).on_press_maybe(
                                    can_review.then(|| Message::SourceEnabled {
                                        store: store_index,
                                        kind: SourceKind::File,
                                        path: source.clone(),
                                        enabled: !enabled
                                    })
                                ),
                                button("Remove").on_press(Message::RemoveSource(index))
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .wrap(),
                            store.file_size_limit(source).map(|limit| {
                                let effective = store.effective_file_size_limit(source);
                                text(if limit == effective {
                                    format!("Size limit · {limit} bytes")
                                } else {
                                    format!("Size limit · {effective} bytes ({limit} configured)")
                                })
                                .size(13)
                            }),
                            self.file_size
                                .as_ref()
                                .filter(|edit| &edit.path == source)
                                .map(|edit| column![
                                    text(knowledge_options::file_size_help(source)).size(13),
                                    text_input("Use format default", &edit.input)
                                        .on_input_maybe(can_review.then_some(Message::FileSize)),
                                    (!edit.edited
                                        && store.file_size_limit(source).is_some_and(|limit| {
                                            let default = DEFAULT_TEXT_BYTES as u64;
                                            limit > default
                                                && store.effective_file_size_limit(source)
                                                    == default
                                        }))
                                    .then(|| text(
                                        "Re-enter the saved value to review a higher limit."
                                    )
                                    .size(13)),
                                    row![
                                        button("Apply size limit").on_press_maybe(can_review.then(
                                            || Message::ApplyFileSize(store_index, source.clone())
                                        )),
                                        button("Cancel").on_press(Message::CancelFileSize),
                                    ]
                                    .spacing(6)
                                    .wrap(),
                                ]
                                .spacing(6)),
                        ]
                        .spacing(5),
                    )
                },
            );
            let folders = store.folders.iter().enumerate().fold(
                column![].spacing(6),
                |rows, (index, folder)| {
                    let enabled = store.source_enabled(SourceKind::Folder, &folder.root);
                    rows.push(
                        column![
                            text(format!(
                                "{} · {} approved files · {}",
                                folder.root.display(),
                                folder.len(),
                                folder.options.sync,
                            ))
                            .wrapping(text::Wrapping::WordOrGlyph)
                            .width(Length::Fill),
                            folder.options.max_file_bytes.map(|limit| text(format!(
                                "Configured size limit · {} bytes per file",
                                limit.bytes(),
                            ))
                            .size(13)),
                            row![
                                button("Review update…").on_press_maybe(
                                    (can_review && enabled)
                                        .then_some(Message::ReviewFolderUpdate(index))
                                ),
                                button("Options…").on_press_maybe(
                                    can_review.then_some(Message::ConfigureFolder(index))
                                ),
                                button(if enabled { "Disable" } else { "Enable" }).on_press_maybe(
                                    can_review.then(|| Message::SourceEnabled {
                                        store: store_index,
                                        kind: SourceKind::Folder,
                                        path: folder.root.clone(),
                                        enabled: !enabled
                                    })
                                ),
                                button("Remove folder").on_press(Message::RemoveFolder(index)),
                            ]
                            .spacing(8)
                            .wrap(),
                        ]
                        .spacing(6),
                    )
                },
            );
            details = details.push(text(store.name.as_str()).size(19))
                .push(self.knowledge_backend.view(store_index, store, can_review, vault_keys))
                .push(text_input("Absolute path to a UTF-8 file, PDF, or folder", &self.source).on_input_maybe(can_review.then_some(Message::Source)))
                .push(row![
                    button("Add file").on_press_maybe(backend_valid.then_some(Message::AddSource)),
                    button("Review folder…").on_press_maybe((can_review && backend_valid && !self.source.trim().is_empty()).then_some(Message::ReviewFolder)),
                ].spacing(8).wrap())
                .push(self.folder_options.view(can_review).map(Message::FolderOptions))
                .push(sources)
                .push(folders)
                .push(self.folder_review.as_deref().map(|review| folder_review_view(review, self.folder_review_page)))
                .push(field("Embedding source", embedding_picker(
                    self.embedding_options(), managed, EmbeddingScope::Store, can_review,
                )))
                .push(managed.is_none().then(|| column![
                    field("Local embedding endpoint", text_input("Numeric loopback address and port", endpoint)
                        .on_input(|value| Message::ConfigureStore(EmbeddingField::Endpoint, value))),
                    field("Embedding model", text_input("Installed embedding model", model)
                        .on_input(|value| Message::ConfigureStore(EmbeddingField::Model, value))),
                ].spacing(10)))
                .push(store.embedding.is_none().then(|| text("This store inherits the default embedding settings until you select its own source.").size(13)))
                .push(qdrant_destination.as_ref().map(|destination| {
                    text(format!("Source and query embeddings and opaque identifiers will be sent to {destination}."))
                        .wrapping(text::Wrapping::WordOrGlyph).size(13)
                }))
                .push(checkbox(self.consent_current())
                    .label(match (managed.is_some(), store.qdrant.is_some()) {
                        (true, false) => "Allow this managed model to process these source excerpts and searches",
                        (false, false) => "Allow these source excerpts and searches to be sent to this local service",
                        (true, true) => "Allow this managed model to embed source excerpts and searches and send vectors to Qdrant",
                        (false, true) => "Allow this local service to embed source excerpts and searches and send vectors to Qdrant",
                    })
                    .on_toggle_maybe(backend_valid.then_some(Message::Consent)))
                .push(managed.is_none().then(|| text("Use a local embedding model you trust. The selected service can forward requests.").size(13)))
                .push(store.has_automatic_sources().then(|| {
                    let destination = managed.map_or_else(
                        || format!("{endpoint} · {model}"),
                        |id| format!("{MANAGED_EMBEDDING_DESTINATION} · {id}"),
                    );
                    let approved = state.automatic_ready(store)
                        && self.review_policy_embedding(managed, store.automatic_policy_model).is_ok();
                    let model_available = managed.map_or_else(
                        || Profile::new(endpoint, model).is_ok(),
                        |id| self.embedding_available(id),
                    );
                    column![
                        text(format!("Automatic embedding destination · {destination}"))
                            .wrapping(text::Wrapping::WordOrGlyph),
                        text("Automatic sources can send new or changed text here in the background. Folder discovery stays within the reviewed options. Save changes to apply them.").size(13),
                        qdrant_destination.as_ref().map(|destination| {
                            text(format!("Automatic indexing will send source embeddings and opaque identifiers to {destination}."))
                                .wrapping(text::Wrapping::WordOrGlyph).size(13)
                        }),
                        (!approved || store.qdrant.is_some() && !store.automatic_qdrant_expanded).then(|| button(if approved {
                                "Review automatic Qdrant capacity…"
                            } else if store.qdrant.is_some() {
                                "Approve automatic embedding and Qdrant destinations"
                            } else {
                                "Approve automatic embedding destination"
                            })
                            .on_press_maybe((can_request && model_available)
                                .then_some(Message::ApproveAutomaticEmbedding(store_index)))),
                        approved.then(|| text(self.automatic_status.get(&store.name)
                            .map_or("Automatic sync checks each store in turn and pauses during edits.", String::as_str))
                            .wrapping(text::Wrapping::WordOrGlyph).size(13)),
                    ].spacing(6)
                }))
                .push(bindings)
                .push(checkbox(store.autonomous_enabled)
                    .label("Offer this store for Agent, Planning, Swarm, and scheduled AI requests")
                    .on_toggle_maybe(can_review.then_some(move |enabled| {
                        Message::EnableAutonomousKnowledge { store: store_index, enabled }
                    })))
                .push(text("Terminal AI chat and code analysis ask separately before embedding a query and show retrieved excerpts before sending the request.").size(13))
                .push(text("Saved indexes can be searched again after reopening. Updating changed settings takes effect after Save.").size(13))
                .push(row![
                    button("Update index").on_press_maybe(
                        can_index.then_some(Message::Index { rebuild: false }),
                    ),
                    button("Rebuild index").style(button::secondary).on_press_maybe(
                        can_index.then_some(Message::Index { rebuild: true }),
                    ),
                ].spacing(8).wrap())
                .push(text("Update reuses unchanged enabled files. Rebuild replaces enabled vectors and keeps disabled vectors that fit the embedding dimensions.").size(13))
                .push(row![
                    text_input("Test search", &self.query).on_input(Message::Query),
                    button("Search").on_press_maybe(
                        (can_index && !self.query.trim().is_empty()).then_some(Message::Search),
                    )
                ].spacing(8));
            for hit in &self.hits {
                details = details.push(text(hit.as_str()).wrapping(text::Wrapping::WordOrGlyph));
            }
            if self.confirm_remove {
                details = details.push(
                    row![
                        text("Remove this store from the profile? The source files stay on disk."),
                        button("Remove store")
                            .style(button::danger)
                            .on_press(Message::RemoveStore),
                        button("Cancel").on_press(Message::ConfirmRemove(false)),
                    ]
                    .spacing(8)
                    .wrap(),
                );
            } else {
                details =
                    details.push(button("Remove store…").on_press(Message::ConfirmRemove(true)));
            }
        } else {
            details = details.push(text("Select or create a knowledge store."));
        }
        column![
            text("Knowledge Stores").size(20),
            row![sidebar, details].spacing(16)
        ]
        .spacing(12)
        .width(Length::Fill)
        .into()
    }
}

const FOLDER_REVIEW_PAGE_SIZE: usize = 128;

fn folder_review_view(review: &folder::Review, page: usize) -> Element<'_, Message> {
    let pages = review.rows.len().div_ceil(FOLDER_REVIEW_PAGE_SIZE);
    let rows = review
        .rows
        .iter()
        .skip(page * FOLDER_REVIEW_PAGE_SIZE)
        .take(FOLDER_REVIEW_PAGE_SIZE)
        .fold(column![].spacing(4), |rows, entry| {
            let result = match entry.status {
                folder::Status::Ready { bytes, change } => format!("{change} · {bytes} bytes"),
                folder::Status::Skipped(reason) => format!("Skipped · {reason}"),
                folder::Status::Removed => "Removed from the folder index".into(),
            };
            rows.push(
                row![
                    text(entry.path.as_str())
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::WordOrGlyph),
                    text(result)
                        .width(180)
                        .wrapping(text::Wrapping::WordOrGlyph),
                ]
                .spacing(8),
            )
        });
    let count = review.folder.len();
    let summary = if review.missing {
        "The folder is missing. Approving this review removes its indexed files.".to_owned()
    } else {
        let skipped = review
            .rows
            .iter()
            .filter(|row| matches!(row.status, folder::Status::Skipped(_)))
            .count();
        let removed = review
            .rows
            .iter()
            .filter(|row| matches!(row.status, folder::Status::Removed))
            .count();
        format!("{count} files ready · {skipped} skipped · {removed} removed")
    };
    column![
        text("Review folder sources").size(17),
        text(review.folder.root.display().to_string()).wrapping(text::Wrapping::WordOrGlyph),
        review
            .folder
            .options
            .max_file_bytes
            .map(|limit| text(format!(
                "Configured size limit · {} bytes per file",
                limit.bytes()
            ))
            .size(13)),
        text(summary).wrapping(text::Wrapping::WordOrGlyph),
        (pages > 1).then(|| row![
            button("Previous page").on_press_maybe((page > 0).then(|| Message::FolderReviewPage(page - 1))),
            text(format!("{} of {pages}", page + 1)),
            button("Next page").on_press_maybe((page + 1 < pages).then_some(Message::FolderReviewPage(page + 1))),
        ].spacing(8).align_y(Alignment::Center)),
        (!review.rows.is_empty()).then(|| scrollable(rows).height(150)),
        text(
            "Approval selects the files and contents on all pages. Indexing requires separate consent."
        )
        .size(13),
        row![
            button("Approve folder files")
                .on_press_maybe((review.existing || count > 0).then_some(Message::ApproveFolder)),
            button("Discard review").on_press(Message::DiscardFolderReview),
        ]
        .spacing(8)
        .wrap(),
    ]
    .spacing(6)
    .into()
}

pub(super) struct SaveIndexes {
    cache: Cache,
    retained: Vec<Scope>,
    staged: Vec<StagedIndex>,
    retired: Vec<RetiredIndex>,
    automatic: Option<knowledge_sync::CommitGuard>,
}

struct StagedIndex {
    index: Arc<Index>,
    scope: Scope,
    sources: SourceMask,
    vault_key: Option<Arc<VaultKey>>,
}

struct RetiredIndex {
    scope: Scope,
    vault_key: Result<Option<Arc<VaultKey>>, String>,
}

fn resolve_reviewed_key(
    scope: &Scope,
    vault: Option<&VaultWindow>,
    revision: Option<&Weak<()>>,
) -> Result<Option<Arc<VaultKey>>, String> {
    let Some(settings) = scope
        .qdrant
        .as_ref()
        .filter(|settings| settings.vault_key.is_some())
    else {
        return Ok(None);
    };
    let revision = revision.ok_or("Review and approve the Qdrant request again.")?;
    settings.resolve_vault_key(vault, Some(revision))
}

impl SaveIndexes {
    #[cfg(test)]
    pub(super) fn commit(
        self,
        write_settings: impl FnOnce() -> io::Result<()>,
        sync_settings: impl FnOnce() -> io::Result<()>,
        client: Option<&local_ai::Client>,
        ai_enabled: bool,
    ) -> io::Result<Option<String>> {
        self.commit_checked(|_| write_settings(), sync_settings, client, ai_enabled)
    }

    pub(super) fn commit_checked(
        self,
        write_settings: impl FnOnce(&dyn Fn() -> io::Result<()>) -> io::Result<()>,
        sync_settings: impl FnOnce() -> io::Result<()>,
        client: Option<&local_ai::Client>,
        ai_enabled: bool,
    ) -> io::Result<Option<String>> {
        if let Some(automatic) = &self.automatic {
            automatic.check(&self.cache)?;
        }
        let active = || {
            self.automatic
                .as_ref()
                .map_or(Ok(()), knowledge_sync::CommitGuard::active)
        };
        let authorize = || active().map_err(|error| error.to_string());
        let mut attempted = Vec::new();
        let mut cleanup_warning = false;
        let result = (|| {
            for staged in &self.staged {
                if !ai_enabled && staged.scope.qdrant.is_some() {
                    continue;
                }
                attempted.push(staged);
                let authorize_stage = || {
                    authorize()?;
                    staged.scope.profile.require_current_policy_authority()
                };
                let publication = remote_index::publish(
                    &self.cache,
                    &staged.scope,
                    &staged.index,
                    staged.sources.clone(),
                    client,
                    staged.vault_key.clone(),
                    authorize_stage,
                )?;
                cleanup_warning |= publication.cleanup_warning;
            }
            let require_current = || {
                active()?;
                for staged in &attempted {
                    staged
                        .scope
                        .profile
                        .require_current_policy_authority()
                        .map_err(io::Error::other)?;
                    if let Some(key) = &staged.vault_key {
                        key.require_current().map_err(io::Error::other)?;
                    }
                }
                Ok(())
            };
            require_current()?;
            write_settings(&require_current)
        })();
        if let Err(error) = result {
            for staged in attempted {
                let receipt = self
                    .cache
                    .load(&staged.scope)
                    .ok()
                    .and_then(|index| index.qdrant_receipt);
                if self.cache.remove(&staged.index).is_ok()
                    && let Some(receipt) = receipt
                {
                    let _ = remote_index::retire(
                        &staged.scope,
                        receipt,
                        client,
                        staged.vault_key.clone(),
                        authorize,
                    );
                }
            }
            return Err(error);
        }
        if !self.retired.is_empty()
            && let Err(error) = sync_settings()
        {
            return Ok(Some(format!(
                "Could not synchronize the saved settings ({error}). The previous Qdrant generations were retained."
            )));
        }
        for retired in &self.retired {
            if let Ok(index) = self.cache.load(&retired.scope)
                && let Some(receipt) = index.qdrant_receipt
            {
                cleanup_warning |= retired.vault_key.as_ref().map_or(true, |key| {
                    remote_index::retire(&retired.scope, receipt, client, key.clone(), authorize)
                        .is_err()
                });
            }
        }
        let mut warning = self
            .cache
            .retain(&self.retained)
            .err()
            .map(|error| format!("Obsolete knowledge indexes could not be removed ({error})."))
            .unwrap_or_default();
        if cleanup_warning {
            if !warning.is_empty() {
                warning.push(' ');
            }
            warning.push_str("An obsolete Qdrant generation could not be removed.");
        }
        Ok((!warning.is_empty()).then_some(warning))
    }
}

fn endpoint_field(state: &State) -> Element<'_, Message> {
    field(
        "Local endpoint",
        text_input("Numeric loopback address and port", &state.endpoint)
            .on_input(Message::Endpoint),
    )
}

fn preferences(
    state: &State,
    profiles: &Catalog,
    reveal: &Reveal,
    models: Vec<EmbeddingChoice>,
    editable: bool,
    runtime_downloads_allowed: bool,
    runtime_status: Option<&str>,
) -> Element<'static, Message> {
    let options: Vec<_> = std::iter::once(Choice(None, "Use default".into()))
        .chain(
            profiles
                .role_choices()
                .into_iter()
                .filter_map(|(id, name)| {
                    profiles
                        .profile_reference(id)
                        .map(|reference| Choice(Some(reference), name))
                }),
        )
        .collect();
    let mut content = column![
        text("Local Preferences").size(20),
        text("Profile roles").size(17)
    ]
    .spacing(10)
    .width(Length::Fill);
    for role in Role::ALL {
        let selected = match state.profile(role) {
            None => options.first().cloned(),
            Some(reference) => profiles
                .resolve_profile_reference(reference)
                .and_then(|id| profiles.profile_reference(id))
                .and_then(|reference| {
                    options
                        .iter()
                        .find(|option| option.0.as_ref() == Some(&reference))
                })
                .cloned(),
        };
        content = content.push(
            reveal.wrap(
                Setting::Role(role),
                field(
                    role.label(),
                    pick_list(options.clone(), selected, move |choice| {
                        Message::Role(role, choice.0)
                    })
                    .placeholder("Selected profile unavailable")
                    .width(Length::Fill)
                    .dismiss_on_escape(),
                ),
            ),
        );
    }
    content = content.push(
        reveal.wrap(
            Setting::Embedding,
            field(
                Setting::Embedding.title(),
                column![
                    embedding_picker(
                        models,
                        state.managed_embedding.as_deref(),
                        EmbeddingScope::Default,
                        editable
                    ),
                    state.managed_embedding.is_none().then(|| {
                        reveal.input(
                            Setting::Embedding,
                            editable,
                            text_input("Installed embedding model", &state.embedding)
                                .on_input_maybe(editable.then_some(Message::Embedding)),
                        )
                    }),
                ]
                .spacing(8),
            ),
        ),
    );
    content
        .push(runtime_preferences(
            state.runtime_update,
            editable,
            runtime_downloads_allowed,
            runtime_status,
        ))
        .into()
}

fn runtime_preferences(
    settings: runtime_policy::Settings,
    editable: bool,
    runtime_downloads_allowed: bool,
    status: Option<&str>,
) -> Element<'static, Message> {
    let can_edit = editable && runtime_downloads_allowed;
    let mode: Element<_> = if !runtime_downloads_allowed {
        text(runtime_policy::Mode::Off.to_string()).into()
    } else if can_edit {
        pick_list(
            runtime_policy::Mode::ALL,
            Some(settings.mode),
            Message::RuntimeUpdateMode,
        )
        .width(Length::Fill)
        .dismiss_on_escape()
    } else {
        text(settings.mode.to_string()).into()
    };
    let backend: Element<_> = if can_edit {
        pick_list(
            runtime_policy::Backend::choices(),
            Some(settings.backend),
            Message::RuntimeUpdateBackend,
        )
        .width(Length::Fill)
        .dismiss_on_escape()
    } else {
        text(settings.backend.to_string()).into()
    };
    column![
        text("Managed runtime updates").size(17),
        field("Update policy", mode),
        field("Preferred backend", backend),
        text(if runtime_downloads_allowed {
            "Save to apply. Notify and Automatic Stable check the signed chardonnay/kortty-llama-runtimes channel on GitHub at startup and daily while AI is enabled. Automatic Stable downloads and installs a compatible verified release when managed requests are idle."
        } else {
            crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED
        })
            .size(14)
            .wrapping(text::Wrapping::WordOrGlyph),
        status.map(|status| text(status.to_owned()).size(14).wrapping(text::Wrapping::WordOrGlyph)),
    ]
    .spacing(8)
    .into()
}

#[derive(Clone, Eq, PartialEq)]
struct EmbeddingChoice {
    id: Option<String>,
    label: String,
}

#[derive(Clone, Default)]
pub(crate) struct PolicyEmbeddings {
    user_allowed: bool,
    choices: Vec<PolicyEmbedding>,
    unavailable: bool,
}

#[derive(Clone)]
struct PolicyEmbedding {
    id: String,
    name: String,
    fingerprint: [u8; 32],
}

impl PolicyEmbeddings {
    fn load(root: &Path, cancelled: &AtomicBool) -> Self {
        let policy = ClipboardPolicy::load();
        let mut inventory = Self {
            user_allowed: policy.ai_user_model_files_allowed(),
            ..Self::default()
        };
        for model in policy
            .ai_runtime_models()
            .iter()
            .filter(|model| model.runtime == PolicyModelRuntime::Gguf)
        {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            if policy_models::available(model, root, cancelled)
                && let Ok(authority) = PolicyModelAuthority::resolve(root, &model.id, model.runtime)
                && authority.definition() == model
            {
                inventory.choices.push(PolicyEmbedding {
                    id: model.id.clone(),
                    name: model.name.clone(),
                    fingerprint: authority.fingerprint(),
                });
            } else {
                inventory.unavailable = true;
            }
        }
        inventory
    }
}

impl fmt::Display for EmbeddingChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn embedding_picker(
    options: Vec<EmbeddingChoice>,
    selected_id: Option<&str>,
    scope: EmbeddingScope,
    editable: bool,
) -> Element<'static, Message> {
    let selected = options
        .iter()
        .find(|option| option.id.as_deref() == selected_id)
        .cloned()
        .unwrap_or_else(|| EmbeddingChoice {
            id: selected_id.map(str::to_owned),
            label: if selected_id.is_some_and(valid_policy_model_id) {
                "Saved policy-managed embedding model is unavailable"
            } else {
                "Saved managed embedding model is unavailable"
            }
            .into(),
        });
    if editable {
        pick_list(options, Some(selected), move |choice| {
            Message::EmbeddingSource(scope, choice.id)
        })
        .width(Length::Fill)
        .dismiss_on_escape()
    } else {
        text(selected.label).into()
    }
}

fn field<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).width(175), control.into()]
        .align_y(Alignment::Center)
        .spacing(8)
        .into()
}

fn limited(value: &str, bytes: usize) -> &str {
    &value[..value.floor_char_boundary(bytes.min(value.len()))]
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn profile_catalog(ids: &[u64]) -> Catalog {
        let profiles: Vec<_> = ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "name": format!("Profile {id}"),
                    "endpoint": local_ai::DEFAULT_ENDPOINT,
                    "model": "fixture-model",
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "profiles": profiles,
            "default": ids.first(),
            "last_id": ids.iter().max().copied().unwrap_or(0),
        }))
        .unwrap()
    }

    #[test]
    fn cancelled_runtime_removal_review_ignores_queued_success() {
        use super::super::{Configuration, Dialog as Manager, Message as ManagerMessage};
        use crate::private_files::Directory;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        Directory::open(&root, false)
            .unwrap()
            .private_child("llama-runtime", true)
            .unwrap()
            .publish_copy("activation.json", b"fixture activation")
            .unwrap();
        let plan = runtime_package::inspect_removal(&root).unwrap().unwrap();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root,
            Vec::new(),
            true,
            None,
        );
        manager.page = super::super::Page::LocalModels;

        for route in 0..3 {
            let (cancel, requested) = runtime_package::Cancel::test_token();
            manager.local.request = Some(Request::RuntimeRemovalReview { _cancel: cancel });
            let generation = manager.local.generation;
            assert!(manager.local.runtime_removal_review_open());
            let message = match route {
                0 => ManagerMessage::Local(Message::Cancel),
                1 => manager.escape_message(),
                _ => manager.child_close_message().expect("expected child close"),
            };
            let _ = manager.update(message);
            assert!(requested.load(std::sync::atomic::Ordering::Acquire));
            assert!(!manager.local.busy());
            let _ = manager.update(ManagerMessage::Local(Message::Finished(
                generation,
                Ok(Output::RuntimeRemovalReviewed(Some(plan.clone()))),
            )));
            assert!(!manager.local.runtime_removal_review_open());
            assert!(manager.child_close_message().is_none());
            assert_eq!(manager.local.status.as_deref(), Some("Cancelled."));
        }
    }

    #[test]
    fn ai_runtime_download_policy_locks_online_actions_and_keeps_saved_preferences() {
        let profile = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(profile.0.clone());
        let saved = State {
            runtime_update: runtime_policy::Settings {
                mode: runtime_policy::Mode::AutomaticStable,
                backend: runtime_policy::Backend::Cpu,
            },
            ..Default::default()
        };
        let mut draft = saved.clone();
        dialog.set_runtime_downloads_allowed(false);
        for message in [
            Message::CheckRuntimeOnline,
            Message::InstallRuntimeOnline,
            Message::RuntimeUpdateMode(runtime_policy::Mode::Off),
        ] {
            assert!(
                iced_runtime::task::into_stream(dialog.update(message, &mut draft, &saved, true),)
                    .is_none()
            );
        }
        assert_eq!(draft.runtime_update, saved.runtime_update);
        assert!(!dialog.busy());
        let _ = dialog.update(
            Message::RuntimePath(RuntimeFile::Index, "local-signed-index.json".into()),
            &mut draft,
            &saved,
            true,
        );
        assert_eq!(dialog.runtime_paths()[0], "local-signed-index.json");
        dialog.set_runtime_downloads_allowed(true);
        let _ = dialog.update(
            Message::RuntimeUpdateMode(runtime_policy::Mode::Notify),
            &mut draft,
            &saved,
            true,
        );
        assert_eq!(draft.runtime_update.mode, runtime_policy::Mode::Notify);
    }

    #[test]
    fn managed_embedding_scope_is_distinct_and_persists_the_coding_route() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("guide.txt");
        std::fs::write(&path, "Private guide").unwrap();
        let mut store = Store::new("Guide").unwrap();
        store.add_source(path).unwrap();
        store.coding_profile = Some(ProfileRef::Numeric(7));
        let mut state = State {
            embedding: "external-vector-model".into(),
            stores: vec![store],
            ..State::default()
        };
        state.roles[Role::Coding as usize] = Some(ProfileRef::Numeric(7));
        let external = state.scope(&state.stores[0], &profile.0).unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        state.managed_embedding = Some(id.into());
        let managed = state.scope(&state.stores[0], &profile.0).unwrap();
        assert!(!managed.can_reuse(&external));
        let mut dialog = Dialog::new(profile.0.clone());
        let saved = state.clone();
        let _ = dialog.update(Message::SelectStore(0), &mut state, &saved, true);
        let _ = dialog.update(
            Message::Role(Role::Text, Some(ProfileRef::Numeric(6))),
            &mut state,
            &saved,
            true,
        );
        let bind_text = |profile| Message::BindKnowledgeProfile {
            store: 0,
            role: Role::Text,
            expected_role: ProfileRef::Numeric(profile),
            binding: Some(ProfileRef::Numeric(profile)),
        };
        let _ = dialog.update(bind_text(9), &mut state, &saved, true);
        assert_eq!(state.stores[0].text_profile, None);
        let _ = dialog.update(bind_text(6), &mut state, &saved, true);
        assert_eq!(state.stores[0].text_profile, Some(ProfileRef::Numeric(6)));
        let reloaded: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(reloaded.scope(&reloaded.stores[0], &profile.0).unwrap() == managed);
        let catalog = profile_catalog(&[6, 7, 8]);
        for (role, profile_id) in [(Role::Coding, 7), (Role::Text, 6)] {
            let plan = reloaded
                .knowledge_for(role, profile_id, &profile.0, &catalog)
                .pop()
                .unwrap();
            assert_eq!(plan.model, id);
            assert_eq!(plan.endpoint, MANAGED_EMBEDDING_DESTINATION);
            assert!(
                reloaded
                    .knowledge_for(role, 8, &profile.0, &catalog)
                    .is_empty()
            );
        }
        state.retain_profiles(|reference| *reference == ProfileRef::Numeric(7));
        assert_eq!(state.stores[0].text_profile, None);
        assert_eq!(state.stores[0].coding_profile, Some(ProfileRef::Numeric(7)));
        let no_consent = dialog.update(
            Message::Index { rebuild: false },
            &mut state,
            &reloaded,
            true,
        );
        assert!(iced_runtime::task::into_stream(no_consent).is_none());
    }

    #[test]
    fn named_roles_and_legacy_knowledge_bindings_require_the_same_projected_profile() {
        let root = crate::snippets::tests::Profile::new();
        let source = root.0.join("guide.txt");
        std::fs::write(&source, "Private guide").unwrap();
        let installed = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-knowledge'\nname = 'Policy profile'\nprovider = 'lm-studio'\nendpoint = 'http://127.0.0.1:1234'\nmodel = 'fixture'\n",
        ).unwrap();
        let ordinary = profile_catalog(&[7]);
        let mut projected = ordinary.clone();
        projected.project_policy_profiles(&installed).unwrap();
        let reference = ProfileRef::Policy("policy-knowledge".into());
        let handle = projected.resolve_profile_reference(&reference).unwrap();
        let alias = projected.legacy_policy_alias(handle).unwrap();
        let mut state: State = serde_json::from_value(serde_json::json!({
            "roles": ["policy-knowledge", 7, "policy-knowledge", "policy-knowledge"],
            "embedding": "fixture-embedding",
            "stores": [{"name": "Guide", "sources": [source], "text_profile": alias}],
        }))
        .unwrap();

        let baseline = state.clone();
        assert_eq!(state.profile(Role::Journal), Some(&reference));
        assert!(
            state
                .knowledge_for(Role::Text, handle, &root.0, &ordinary)
                .is_empty()
        );
        assert!(
            state
                .knowledge_for(Role::Text, 7, &root.0, &projected)
                .is_empty()
        );
        assert_eq!(
            state
                .knowledge_for(Role::Text, handle, &root.0, &projected)
                .len(),
            1
        );

        let mut dialog = Dialog::new(root.0.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut state, &baseline, true);
        let _ = dialog.update(
            Message::BindKnowledgeProfile {
                store: 0,
                role: Role::Text,
                expected_role: reference.clone(),
                binding: Some(reference.clone()),
            },
            &mut state,
            &baseline,
            true,
        );
        assert_eq!(
            state.new_profile_bindings(&baseline).collect::<Vec<_>>(),
            vec![&reference]
        );
        let saved = serde_json::to_value(&state).unwrap();
        assert_eq!(
            saved["roles"],
            serde_json::json!([
                "policy-knowledge",
                7,
                "policy-knowledge",
                "policy-knowledge"
            ])
        );
        assert_eq!(saved["stores"][0]["text_profile"], "policy-knowledge");
        let reloaded: State = serde_json::from_value(saved).unwrap();
        assert_eq!(reloaded, state);
        assert!(
            reloaded
                .knowledge_for(Role::Text, handle, &root.0, &ordinary)
                .is_empty()
        );
        assert_eq!(
            reloaded
                .knowledge_for(Role::Text, handle, &root.0, &projected)
                .len(),
            1
        );
    }

    #[test]
    fn knowledge_ignores_hits_for_an_earlier_query() {
        let mut dialog = Dialog::default();
        let mut state = State::default();
        let saved = State::default();
        let _ = dialog.update(Message::Query("new query".into()), &mut state, &saved, true);
        let _ = dialog.update(
            Message::Finished(
                0,
                Ok(Output::Hits {
                    query: "old query".into(),
                    hits: vec!["old result".into()],
                    vault_revision: None,
                }),
            ),
            &mut state,
            &saved,
            true,
        );
        assert!(dialog.hits.is_empty());
    }

    #[test]
    fn knowledge_consent_cannot_survive_an_endpoint_or_embedding_model_change() {
        let mut dialog = Dialog::default();
        let mut state = State::default();
        let saved = State::default();
        let _ = dialog.update(Message::Consent(true), &mut state, &saved, true);
        let _ = dialog.update(
            Message::Endpoint("127.0.0.1:8080".into()),
            &mut state,
            &saved,
            true,
        );
        assert!(!dialog.consent);
        let _ = dialog.update(Message::Consent(true), &mut state, &saved, true);
        let _ = dialog.update(
            Message::Embedding("embed-model".into()),
            &mut state,
            &saved,
            true,
        );
        assert!(!dialog.consent);
    }

    #[test]
    fn policy_managed_embedding_selection_is_read_only_and_review_binds_exact_source() {
        let alias = "policy-office-embedding";
        let before = [1; 32];
        let after = [2; 32];
        let mut dialog = Dialog::default();
        let mut state = State::default();
        let saved = State::default();
        let choose = |dialog: &mut Dialog, state: &mut State| {
            let _ = dialog.update(
                Message::EmbeddingSource(EmbeddingScope::Default, Some(alias.into())),
                state,
                &saved,
                true,
            );
        };
        choose(&mut dialog, &mut state);
        assert!(state.managed_embedding.is_none());
        let load = |fingerprint| PolicyEmbeddings {
            user_allowed: false,
            choices: vec![PolicyEmbedding {
                id: alias.into(),
                name: "Office embeddings".into(),
                fingerprint,
            }],
            unavailable: false,
        };
        let _ = dialog.update(
            Message::PolicyEmbeddingsLoaded(load(before)),
            &mut state,
            &saved,
            true,
        );
        choose(&mut dialog, &mut state);
        assert_eq!(state.managed_embedding.as_deref(), Some(alias));
        assert_eq!(
            dialog.embedding_options()[1].label,
            "Office embeddings (Managed by policy)"
        );
        assert_eq!(
            dialog.review_policy_embedding(Some(alias), Some(before)),
            Ok(Some(before))
        );
        assert_eq!(
            dialog.review_policy_embedding(Some(alias), Some(after)),
            Err(POLICY_EMBEDDING_CHANGED.into())
        );

        let restored: State =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert!(restored.validate().is_ok());
        state.default_managed_model = Some(alias.into());
        assert!(state.validate().is_err());
        let _ = dialog.update(
            Message::PolicyEmbeddingsLoaded(load(after)),
            &mut state,
            &saved,
            true,
        );
        assert!(
            dialog
                .review_policy_embedding(Some(alias), Some(before))
                .is_err()
        );
        assert_eq!(
            dialog.review_policy_embedding(Some(alias), Some(after)),
            Ok(Some(after))
        );
    }

    #[test]
    fn knowledge_stores_keep_their_embedding_scope_when_defaults_or_other_stores_change() {
        let mut state: State = serde_json::from_value(serde_json::json!({
            "embedding": "legacy-model",
            "stores": [{"name": "Legacy", "sources": []}]
        }))
        .unwrap();
        let saved = state.clone();
        let mut dialog = Dialog::default();
        let _ = dialog.update(
            Message::StoreName("Independent".into()),
            &mut state,
            &saved,
            true,
        );
        let _ = dialog.update(Message::CreateStore, &mut state, &saved, true);
        let independent = state.scope(&state.stores[1], &dialog.profile_dir).unwrap();
        assert!(state.scope(&state.stores[0], &dialog.profile_dir).unwrap() != independent);
        assert_eq!(state.embedding_fields(&state.stores[0]).1, "legacy-model");

        let _ = dialog.update(
            Message::Embedding("new-default".into()),
            &mut state,
            &saved,
            true,
        );
        assert_eq!(state.embedding_fields(&state.stores[0]).1, "new-default");
        let _ = dialog.update(Message::SelectStore(0), &mut state, &saved, true);
        let _ = dialog.update(Message::Consent(true), &mut state, &saved, true);
        let _ = dialog.update(
            Message::ConfigureStore(EmbeddingField::Endpoint, "127.0.0.1:12001".into()),
            &mut state,
            &saved,
            true,
        );
        assert!(!dialog.consent);
        assert_eq!(
            state.embedding_fields(&state.stores[0]),
            ("127.0.0.1:12001", "new-default", None)
        );

        let reloaded: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(
            reloaded
                .scope(&reloaded.stores[1], &dialog.profile_dir)
                .unwrap()
                == independent
        );
    }
}
