//! Local model discovery, profile roles and knowledge-store configuration.

pub(super) mod huggingface;
mod knowledge;
mod knowledge_backend;
mod knowledge_inventory;
mod knowledge_options;
pub(super) mod knowledge_settings;
mod managed;
pub(super) use managed::ModelId;
mod model_configuration;
pub(crate) use model_configuration::Kind as ModelConfigurationKind;
mod mlx;
mod online;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use iced::widget::{
    button, checkbox, column, container, pick_list, responsive, row, scrollable, text, text_input,
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
use crate::workspace::dialog_design;

use super::profiles::{Catalog, ProfileRef, picker::DismissOnEscape as _};
use super::{
    Configuration, LocalSection, ProfileId, file_chooser, java_knowledge,
    java_models::{self, Hint, Hints, Metadata},
    model_inventory::ModelInventory,
    search::{Reveal, Setting},
};

pub(super) use knowledge::folder::supported as knowledge_file_supported;
pub(super) use knowledge::outcomes;
use knowledge::qdrant::{VaultKey, require_vault_revision};
pub(super) use knowledge::retrieval;
use knowledge::source::SyncMode;
pub(crate) use knowledge::sync as knowledge_sync;
use knowledge::{
    Cache, Embedding, Index, Scope, SourceIssue, SourceKind, SourceMask, SourceUpdates, Store,
    UpdateSummary, remote_index,
};
use knowledge::{file_review, folder};
use knowledge_inventory::{SourcePhase, SourceWork};
use knowledge_sync::SourceStatus;

pub(super) fn pick_native_knowledge_sources(
    owner: &dyn iced::window::Window,
    kind: SourceKind,
) -> iced::futures::future::BoxFuture<'static, Option<Vec<PathBuf>>> {
    let picker = rfd::AsyncFileDialog::new()
        .set_parent(owner)
        .set_title(kind.to_string());
    match kind {
        SourceKind::File => {
            // AppKit merges filters into a single extension list. Keep extensionless
            // supported sources selectable there and validate every result in korTTY.
            let picker = if cfg!(target_os = "macos") {
                picker
            } else {
                picker
                    .add_filter("Supported files", folder::SUPPORTED_EXTENSIONS)
                    .add_filter("All files", &["*"])
            };
            let files = picker.pick_files();
            Box::pin(async move {
                files
                    .await
                    .map(|files| files.iter().map(|file| file.path().to_owned()).collect())
            })
        }
        SourceKind::Folder => {
            let folder = picker.pick_folder();
            Box::pin(async move { folder.await.map(|folder| vec![folder.path().to_owned()]) })
        }
    }
}

pub(super) fn pick_native_managed_files(
    owner: &dyn iced::window::Window,
) -> iced::futures::future::BoxFuture<'static, Option<Vec<PathBuf>>> {
    let files = rfd::AsyncFileDialog::new()
        .set_parent(owner)
        .set_title("Import GGUF")
        .add_filter("GGUF models", &["gguf", "GGUF"])
        .pick_files();
    Box::pin(async move {
        files.await.map(|files| {
            files
                .iter()
                .take(managed_models::MAX_MODELS + 1)
                .map(|file| file.path().to_owned())
                .collect()
        })
    })
}

const MAX_DRAFT_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MANAGED_EMBEDDING_DESTINATION: &str = "Managed llama.cpp on this computer";
const POLICY_EMBEDDING_CHANGED: &str = "The policy-managed embedding model changed. Refresh the managed models and review the destination again.";

fn valid_embedding_model_id(id: &str) -> bool {
    managed_models::valid_id(id) || valid_policy_model_id(id)
}

pub(super) enum Page<'a> {
    Models {
        tokens: Vec<CredentialChoice>,
        hints: &'a Hints,
        setup_available: bool,
    },
    Knowledge(&'a java_knowledge::Hints, &'a Hints),
    Preferences(LocalSection, Vec<CredentialChoice>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Text,
    Coding,
    Agent,
    Journal,
    Security,
}

impl Role {
    pub(super) const ALL: [Self; 5] = [
        Self::Text,
        Self::Coding,
        Self::Agent,
        Self::Journal,
        Self::Security,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Text => "Text profile",
            Self::Coding => "Coding profile",
            Self::Agent => "Terminal agent profile",
            Self::Journal => "Session journal profile",
            Self::Security => "Snippet security profile",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct State {
    roles: [Option<ProfileRef>; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    security_profile: Option<ProfileRef>,
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
            security_profile: None,
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
    pub(super) fn has_knowledge_stores(&self) -> bool {
        !self.stores.is_empty()
    }
    pub(super) fn set_guided_profile(&mut self, role: Role, reference: ProfileRef) {
        self.set_profile(role, Some(reference));
    }

    pub(super) fn set_profile(&mut self, role: Role, reference: Option<ProfileRef>) {
        if role == Role::Security {
            self.security_profile = reference;
        } else {
            self.roles[role as usize] = reference;
        }
    }

    pub(super) fn set_guided_embedding(&mut self, model: String) {
        self.managed_embedding = Some(model);
    }

    pub(super) fn needs_guided_embedding(&self) -> bool {
        self.uses_manual_embedding() && self.embedding.trim().is_empty()
    }

    pub(super) fn uses_manual_embedding(&self) -> bool {
        self.managed_embedding.is_none()
    }

    pub(super) fn prepare_backup(&mut self) {
        self.runtime_update.mode = runtime_policy::Mode::Off;
        for store in &mut self.stores {
            store.prepare_backup();
        }
    }

    pub(super) fn profile(&self, role: Role) -> Option<&ProfileRef> {
        if role == Role::Security {
            self.security_profile.as_ref()
        } else {
            self.roles[role as usize].as_ref()
        }
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
        for role in self
            .roles
            .iter_mut()
            .chain(std::iter::once(&mut self.security_profile))
        {
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
        Role::ALL
            .into_iter()
            .filter_map(|role| {
                let next = self.profile(role)?;
                (Some(next) != previous.profile(role)).then_some(next)
            })
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
                Role::Agent | Role::Journal | Role::Security => None,
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

    fn new_store_index(&self, store: &Store) -> Result<usize, String> {
        if self.stores.len() >= knowledge::MAX_STORES {
            return Err("Up to 32 knowledge stores are supported.".into());
        }
        if self
            .stores
            .iter()
            .any(|existing| existing.name == store.name)
        {
            return Err("Enter a unique knowledge store name.".into());
        }
        Ok(self.stores.len())
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
                other.name == store.name
                    || store.id().is_some_and(|id| other.id() == Some(id))
                    || store.java_origin.as_ref().is_some_and(|origin| {
                        other
                            .java_origin
                            .as_ref()
                            .is_some_and(|other| other.id == origin.id)
                    })
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

    pub(super) fn new_java_knowledge_origins<'a>(
        &'a self,
        current: &'a Self,
    ) -> impl Iterator<Item = &'a java_knowledge::Origin> + 'a {
        self.stores.iter().filter_map(move |store| {
            let origin = store.java_origin.as_ref()?;
            (!current.stores.iter().any(|existing| {
                existing.id() == store.id() && existing.java_origin.as_ref() == Some(origin)
            }))
            .then_some(origin)
        })
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
    offline_runtime_open: bool,
    backend_repair: Option<BackendRepair>,
    managed: managed::Models,
    installed_selection: managed::InventorySelection,
    policy_embeddings: Option<PolicyEmbeddings>,
    policy_embedding_load: Option<ModelInventory>,
    mlx: mlx::State,
    models: Vec<String>,
    model: Option<String>,
    status: Option<String>,
    request: Option<Request>,
    generation: u64,
    selected: Option<usize>,
    selected_sources: BTreeSet<knowledge::SourceKey>,
    source_sync_editor: Option<knowledge_inventory::SyncEdit>,
    source_results: knowledge_inventory::Results,
    source_work: Option<SourceWork>,
    source_anchor: Option<knowledge::SourceKey>,
    selection_modifiers: iced::keyboard::Modifiers,
    source_removal: Option<SourceRemoval>,
    source_picker: Option<KnowledgeSourcePicker>,
    new_store: Option<Store>,
    knowledge_settings: Option<knowledge_settings::Draft>,
    manual_source: bool,
    store_name: String,
    java_store_origin: Option<java_knowledge::Origin>,
    source: String,
    knowledge_backend: knowledge_backend::Editor,
    folder_options: knowledge_options::Draft,
    file_size: Option<FileSizeEdit>,
    folder_advanced: Option<FolderAdvanced>,
    automatic_status: BTreeMap<String, knowledge_sync::Status>,
    active_automatic_sources: Option<(String, Arc<knowledge_sync::SourceReport>)>,
    runtime_update_status: Option<String>,
    confirm_remove: bool,
    consent: bool,
    consent_policy_model: Option<[u8; 32]>,
    consent_vault_revision: Option<Weak<()>>,
    query: String,
    drafts: Vec<DraftIndex>,
    hits: Vec<String>,
    folder_review: Option<Arc<folder::Review>>,
    source_updates: Option<Arc<SourceUpdates>>,
    approved_update: Option<ApprovedSourceUpdate>,
    folder_review_page: usize,
}

enum KnowledgeSourcePicker {
    ChooseInitial(SourceKind),
    Native {
        ticket: u64,
        kind: SourceKind,
    },
    Retry(SourceKind),
    Browse(file_chooser::Chooser),
    ScanningFiles,
    ReviewFiles {
        review: Arc<file_review::Review>,
        page: usize,
        size_limit: String,
    },
    InitialFolder,
}

struct DraftIndex {
    index: Arc<Index>,
    vault_revision: Option<Weak<()>>,
    review: Option<Arc<IndexReview>>,
}

pub(crate) struct IndexReview {
    store: Scope,
    sources: Scope,
    coverage: SourceMask,
    outcome_warning: bool,
    outcomes: outcomes::Rows,
}

impl fmt::Debug for IndexReview {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IndexReview([redacted])")
    }
}

struct SourceRemoval {
    store: usize,
    id: Option<String>,
    name: String,
    sources: BTreeSet<knowledge::SourceKey>,
}

struct ApprovedSourceUpdate {
    definition: Store,
    review: Arc<SourceUpdates>,
}

struct FileSizeEdit {
    path: PathBuf,
    input: String,
    edited: bool,
}

struct FolderAdvanced {
    store: usize,
    path: PathBuf,
    original: knowledge::source::Options,
    draft: knowledge_options::Draft,
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
    SourceUpdates {
        cancel: Option<folder::Cancel>,
    },
    SourceIndex {
        cancel: Option<local_ai::Cancel>,
    },
    Files {
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
    OpenModelSetup,
    Endpoint(String),
    Discover,
    Model(String),
    TestModel,
    RuntimePath(RuntimeFile, String),
    ImportRuntime,
    CheckRuntimeOnline,
    OpenStableRuntimeReview,
    ConfirmStableRuntime,
    OpenRuntimeVersions,
    CloseRuntimeReview,
    OpenOfflineRuntimeFromReview,
    RevealRuntimeImport,
    UnpinRuntime,
    ChooseRuntimeOnline(runtime_package::online::Choice),
    InstallRuntimeOnline,
    PauseRuntimeDownload,
    ReviewRuntimeDownloadDiscard(bool),
    DiscardRuntimeDownload,
    RuntimeUpdateMode(runtime_policy::Mode),
    RuntimeUpdateBackend(runtime_policy::Backend),
    RuntimeDownloadProgress(u64, runtime_package::online::Progress),
    HuggingFace(huggingface::Action),
    HubSizeResolved(u64, managed_models::huggingface::Repository, Option<u64>),
    ReviewRuntimeRemoval,
    ToggleOfflineRuntime,
    ConfirmRuntimeRemoval,
    CancelRuntimeRemoval,
    CloseBackendRepair,
    Managed(managed::Message),
    RevealManagedSetting(super::search::ManagedSetting),
    RevealMlxConfiguration,
    ContinueReviewedModelStart(Vec<managed::ReviewedModel>),
    OpenManagedFilePicker(Arc<()>),
    ManagedFilesPicked(Arc<()>, Option<Vec<PathBuf>>),
    Mlx(mlx::Action),
    ReviewJavaModel(java_models::Kind, String),
    ReviewJavaKnowledgeStore(String),
    ReviewJavaKnowledgeSource(String, String),
    CancelJavaKnowledgeStore,
    Role(Role, Option<ProfileRef>),
    Embedding(String),
    EmbeddingSource(EmbeddingScope, Option<String>),
    PolicyEmbeddingsLoaded(PolicyEmbeddings),
    StoreName(String),
    CreateStore,
    KnowledgeSettings(knowledge_settings::Action),
    LocalDimensionsDetected {
        store: String,
        model: String,
        value: Option<usize>,
    },
    SelectStore(usize),
    RefreshKnowledgeInventory,
    SelectKnowledgeSource(usize, SourceKind, PathBuf),
    RevealKnowledgeSetting(super::search::KnowledgeSetting),
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
    ToggleManualSource,
    PickNativeKnowledgeSources(SourceKind),
    OpenNativeKnowledgeSourcePicker(u64),
    NativeKnowledgeSourcesPicked(u64, Option<Vec<PathBuf>>),
    BrowseKnowledgeSources(SourceKind),
    KnowledgeSourceBrowse(file_chooser::Message),
    UseBrowsedKnowledgeSources,
    ApproveBrowsedKnowledgeFiles,
    FileReviewPage(usize),
    FileReviewLimit(String),
    ReviewWithFileLimit,
    CloseKnowledgeSourcePicker,
    FolderOptions(knowledge_options::Edit),
    ConfigureFolder(usize),
    EditFolderAdvanced(knowledge_options::Edit),
    ReviewFolderAdvanced,
    CancelFolderAdvanced,
    AddSource,
    SourceEnabled {
        store: usize,
        kind: SourceKind,
        path: PathBuf,
        enabled: bool,
    },
    OpenSourceSync(usize),
    SourceSyncMode(SyncMode),
    ApplySourceSync,
    CancelSourceSync,
    ConfigureFileSize(usize, PathBuf),
    FileSize(String),
    ApplyFileSize(usize, PathBuf),
    CancelFileSize,
    ApproveAutomaticEmbedding(usize),
    ReviewSourceRemoval(usize),
    CancelSourceRemoval,
    RemoveSelectedSources,
    ReviewFolder,
    ReviewSourceUpdates(usize),
    ApproveSourceUpdates,
    CancelSourceUpdates,
    IndexReviewed,
    ApproveFolder,
    FolderReviewPage(usize),
    DiscardFolderReview,
    Consent(bool),
    Index {
        rebuild: bool,
    },
    Query(String),
    Search,
    Cancel,
    Finished(u64, Result<Output, String>),
    ManagedBackendLoaded(u64, Option<(String, PackageBackend)>),
    SourceResults(
        u64,
        Arc<BTreeMap<knowledge::SourceKey, knowledge::SourceResult>>,
    ),
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
    RuntimeUnpinned,
    HuggingFace(huggingface::Output),
    RuntimeRemovalReviewed(Option<runtime_package::RemovalPlan>),
    RuntimeRemoved(Option<String>),
    ManagedModels {
        models: Vec<managed_models::Model>,
        default: Result<managed::SavedDefault, String>,
        active_package: Option<(String, PackageBackend)>,
        mlx: Option<Result<mlx::ResultValue, String>>,
        notice: String,
    },
    ManagedNotice(String),
    Mlx(mlx::ResultValue),
    Index(
        Arc<Index>,
        bool,
        UpdateSummary,
        bool,
        Option<Weak<()>>,
        Arc<IndexReview>,
    ),
    FolderReview(Arc<folder::Review>),
    SourceUpdates(Arc<SourceUpdates>),
    SourceIssue(SourceIssue),
    FileReview(Arc<file_review::Review>),
    ConfirmedFileReview(file_review::Confirmed),
    Hits {
        query: String,
        hits: Vec<String>,
        vault_revision: Option<Weak<()>>,
    },
}

impl Dialog {
    pub(super) fn can_reveal_knowledge_setting(
        &self,
        state: &State,
        profiles: &Catalog,
        setting: super::search::KnowledgeSetting,
    ) -> bool {
        use super::search::KnowledgeSetting::*;

        match setting {
            Name | Create => return true,
            Inventory(_) => {
                return self.knowledge_settings_title().is_none()
                    && !self.knowledge_source_picker_open()
                    && !self.knowledge_file_review_open()
                    && !self.knowledge_source_updates_open()
                    && !self.knowledge_source_advanced_open()
                    && !self.knowledge_source_sync_open()
                    && self.source_removal.is_none();
            }
            _ => {}
        }

        if setting.needs_active_review() {
            let Some(store) = self.selected.and_then(|index| state.stores.get(index)) else {
                return false;
            };
            return match setting {
                FileSize | ApplyFileSize | CancelFileSize => {
                    self.file_size.as_ref().is_some_and(|edit| {
                        store.sources.contains(&edit.path)
                            && (self.request.is_none() || setting == CancelFileSize)
                    })
                }
                ConfirmStoreRemoval | CancelStoreRemoval => self.confirm_remove,
                _ => self
                    .folder_review
                    .as_deref()
                    .is_some_and(|review| match setting {
                        FolderReviewPrevious => self.folder_review_page > 0,
                        FolderReviewNext => {
                            self.folder_review_page + 1 < folder_review_pages(review)
                        }
                        FolderReviewApprove => can_approve_folder_review(review),
                        _ => true,
                    }),
            };
        }
        let Some(store) = self
            .selected
            .and_then(|index| state.stores.get(index))
            .or_else(|| state.stores.first())
        else {
            return false;
        };
        match setting {
            LocalDimensions => store.qdrant.is_none(),
            QdrantEndpoint | QdrantCollection | QdrantDimensions | QdrantAuthentication => {
                store.qdrant.is_some()
            }
            QdrantCredentialVariable => store
                .qdrant
                .as_ref()
                .is_some_and(|settings| settings.vault_key.is_none()),
            EmbeddingEndpoint | EmbeddingModel => state.embedding_fields(store).2.is_none(),
            FolderSync => self.request.is_none(),
            TerminalChatProfile | CodeAnalysisProfile => {
                let role = if setting == TerminalChatProfile {
                    Role::Text
                } else {
                    Role::Coding
                };
                state
                    .profile(role)
                    .and_then(|reference| profiles.resolve_profile_reference(reference))
                    .is_some_and(|id| {
                        profiles
                            .choices()
                            .iter()
                            .any(|(candidate, _)| *candidate == id)
                    })
            }
            AgentKnowledge => self.request.is_none(),
            EmbeddingConsent => knowledge_backend::validate(store).is_ok(),
            AutomaticEmbeddingApproval | AutomaticQdrantCapacity => {
                store.has_automatic_sources()
                    && if self.automatic_destination_approved(state, store) {
                        setting == AutomaticQdrantCapacity
                            && store.qdrant.is_some()
                            && !store.automatic_qdrant_expanded
                    } else {
                        setting == AutomaticEmbeddingApproval
                    }
            }
            ReviewStoreRemoval => !self.confirm_remove,
            _ => true,
        }
    }

    pub(super) fn reveal_knowledge_store(&mut self, state: &State) {
        if self
            .selected
            .is_none_or(|index| index >= state.stores.len())
            && state.has_knowledge_stores()
        {
            self.select_store(Some(0));
        }
    }

    pub(super) fn reveal_knowledge_path(&mut self) {
        self.manual_source = true;
    }
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
        self.close_default_menu();
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

    pub(super) fn models_escape_message(&self) -> Option<Message> {
        if self.default_menu_open() {
            return Some(Message::Managed(managed::Message::CloseDefaultMenu));
        }
        if self.online.review.is_some() {
            return Some(Message::CloseRuntimeReview);
        }
        if self.request.is_some() {
            return None;
        }
        self.managed
            .escape_message()
            .map(Message::Managed)
            .or_else(|| {
                self.mlx
                    .configuration_open()
                    .then_some(Message::Mlx(mlx::Action::CancelConfiguration))
            })
            .or_else(|| {
                self.offline_runtime_open
                    .then_some(Message::ToggleOfflineRuntime)
            })
    }

    pub(super) fn runtime_review_title(&self) -> Option<&'static str> {
        self.online.review.as_ref().map(online::Review::title)
    }

    pub(super) fn runtime_review_height(&self) -> Option<f32> {
        self.online.review.as_ref().map(online::Review::height)
    }

    pub(super) fn runtime_versions_open(&self) -> bool {
        matches!(self.online.review, Some(online::Review::Versions))
    }

    pub(super) fn runtime_review_view(&self, can_request: bool) -> Option<Element<'_, Message>> {
        self.online.review_view(
            self.status.as_deref(),
            can_request,
            self.runtime_downloads_allowed,
            self.request.is_some(),
            self.managed.runtime_summary().0,
        )
    }

    pub(super) fn close_runtime_review(&mut self) {
        if self.online.review.take().is_some() {
            if self.request.is_some() {
                self.cancel();
            }
            self.online.reset_catalog();
        }
    }

    pub(super) fn pending_managed_file_picker(&self, ticket: &Arc<()>) -> bool {
        self.managed.pending_file_picker(ticket)
    }

    pub(super) fn finish_managed_file_picker(
        &mut self,
        ticket: &Arc<()>,
        paths: Option<Vec<PathBuf>>,
        can_import: bool,
    ) {
        if !self.managed.pending_file_picker(ticket) {
            return;
        }
        self.managed.finish_file_picker();
        if self.request.is_some() || !can_import || self.managed.has_action() {
            return;
        }
        if let Some(paths) = paths
            && let Err(problem) = self.managed.stage_files(paths)
        {
            self.status = Some(problem.into());
        }
    }

    pub(super) fn can_reveal_managed_setting(
        &self,
        setting: super::search::ManagedSetting,
        editable: bool,
        ai_enabled: bool,
    ) -> bool {
        use super::search::ManagedSetting::{ChooseFiles, CloseImport, OpenImport};
        if matches!(setting, OpenImport | CloseImport | ChooseFiles)
            && !self.model_toolbar_visible()
        {
            return false;
        }
        let editable = editable && self.request.is_none();
        self.managed
            .has_search_setting(setting, editable, editable && ai_enabled, &self.mlx)
    }

    pub(super) fn reveal_managed_setting(
        &mut self,
        setting: super::search::ManagedSetting,
        editable: bool,
    ) -> Task<Message> {
        let previous = self.model_configuration_kind();
        self.managed.reveal_search_setting(setting);
        if previous.is_none() && self.model_configuration_kind().is_some() {
            self.status = None;
        }
        if setting == super::search::ManagedSetting::Select
            && editable
            && self.request.is_none()
            && self.managed.picker_needs_models()
        {
            self.update_managed(managed::Message::Refresh)
        } else {
            Task::none()
        }
    }

    pub(super) fn can_reveal_mlx_setting(
        &self,
        setting: super::search::MlxSetting,
        editable: bool,
        ai_enabled: bool,
    ) -> bool {
        self.mlx.has_search_setting(
            setting,
            editable && self.request.is_none(),
            ai_enabled,
            self.runtime_removal.is_none(),
            self.runtime_downloads_allowed,
        )
    }

    pub(super) fn reveal_mlx_setting(
        &mut self,
        setting: super::search::MlxSetting,
    ) -> Task<Message> {
        use super::search::MlxSetting;
        if matches!(
            setting,
            MlxSetting::Name | MlxSetting::Idle | MlxSetting::NeverUnload
        ) && !self.managed.has_action()
            && self.mlx.begin_configuration()
        {
            self.status = None;
        }
        if setting == super::search::MlxSetting::Select && self.mlx.picker_needs_models() {
            self.update_mlx(mlx::Action::Refresh)
        } else {
            Task::none()
        }
    }

    pub(super) fn can_reveal_runtime_control(
        &self,
        control: super::search::Control,
        editable: bool,
        ai_enabled: bool,
    ) -> bool {
        use super::search::Control::*;

        let online_enabled = editable
            && ai_enabled
            && self.request.is_none()
            && self.runtime_downloads_allowed
            && self.runtime_removal.is_none();
        let repair = self.backend_repair.as_ref();
        match control {
            _ if control.runtime_toolbar() => {
                self.model_toolbar_visible()
                    && match control {
                        InstallStableRuntime => self.managed.runtime_summary().0.is_none(),
                        UpdateStableRuntime => self.managed.runtime_summary().0.is_some(),
                        OpenSignedPackage => !self.offline_runtime_open,
                        HideSignedPackage => self.offline_runtime_open,
                        _ => true,
                    }
            }
            CancelRuntimeRequest => self.runtime_request_active(),
            PauseRuntimeDownload => self.runtime_request_active() && self.online.can_pause(),
            CloseRuntimeRepair => repair.is_some(),
            _ if repair.is_some_and(|repair| repair.installed) => false,
            SignedIndex | IndexSignature | RuntimePackage | InstallRuntime => true,
            RemoveRuntime => repair.is_none() && self.runtime_removal.is_none(),
            ConfirmRuntimeRemoval | CancelRuntimeRemoval => {
                repair.is_none() && self.runtime_removal.is_some()
            }
            CheckRuntimeOnline => repair.is_some(),
            RuntimeOnlineChoice
            | DownloadRuntimeOnline
            | DiscardRuntimeDownload
            | KeepRuntimeDownload => self.online.has_search_control(
                control,
                online_enabled,
                repair.map(|repair| repair.required),
            ),
            _ => repair.is_none(),
        }
    }

    fn model_toolbar_visible(&self) -> bool {
        self.backend_repair.is_none()
            && self.online.review.is_none()
            && !self.runtime_removal_review_open()
            && self.model_configuration_kind().is_none()
            && self.model_review_title().is_none()
    }

    fn runtime_request_active(&self) -> bool {
        matches!(
            self.request,
            Some(Request::Runtime { .. } | Request::RuntimeRemovalReview { .. })
        )
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

    pub(super) fn has_managed_models_to_refresh(&self) -> bool {
        self.managed.has_models() || self.mlx.has_selection()
    }

    pub(super) fn reveal_offline_runtime(&mut self) {
        self.offline_runtime_open = true;
    }

    pub(super) fn stage_java_model(&mut self, hint: &Hint) {
        if self.busy() || self.runtime_removal_review_open() || self.backend_repair_open() {
            return;
        }
        if let Err(error) = java_models::require_review_policy() {
            self.status = Some(error.to_string());
            return;
        }
        let staged = match hint.kind() {
            java_models::Kind::Gguf => self.managed.stage_java_settings(hint),
            java_models::Kind::Mlx => self.mlx.stage_java_settings(hint),
        };
        if staged {
            self.status = Some("Review the proposed values in the selected model’s settings, then save to apply them.".into());
        }
    }

    pub(super) fn stage_java_knowledge_store(
        &mut self,
        state: &State,
        store: &java_knowledge::Store,
    ) {
        if self.busy() || !super::backup::java_ai_hints_allowed(&ClipboardPolicy::load()) {
            return;
        }
        let origin = match store.origin() {
            Ok(origin) => origin,
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        if let Some((index, existing)) =
            state.stores.iter().enumerate().find_map(|(index, native)| {
                native
                    .java_origin
                    .as_ref()
                    .filter(|existing| existing.id == origin.id)
                    .map(|existing| (index, existing))
            })
        {
            if *existing == origin {
                self.select_store(Some(index));
                self.status = Some(
                    "Select and review local sources and an embedding model for this store.".into(),
                );
            } else {
                self.status = Some(
                    "A native store already uses a different definition of this Java store.".into(),
                );
            }
            return;
        }
        self.select_store(None);
        self.store_name = limited(&store.name, 128).trim().into();
        self.java_store_origin = Some(origin);
        self.status = Some("Review the name, then choose Create store. Select the embedding model, backend and local sources before indexing.".into());
    }

    pub(super) fn stage_java_knowledge_source(
        &mut self,
        state: &State,
        store: &java_knowledge::Store,
        source_id: &str,
    ) {
        if self.busy() || !super::backup::java_ai_hints_allowed(&ClipboardPolicy::load()) {
            return;
        }
        let active = self
            .selected
            .and_then(|index| state.stores.get(index))
            .and_then(|native| native.java_origin.as_ref());
        if store.origin().ok().as_ref() != active || active.is_none() {
            return;
        }
        let Some(source) = store.sources.iter().find(|source| source.id == source_id) else {
            return;
        };
        let mut options = knowledge_options::Draft::default();
        let notice = match source.kind {
            java_knowledge::Kind::File => {
                "Choose the local file and add it. Review its file-size limit separately."
            }
            java_knowledge::Kind::Directory => {
                let includes = source.includes.join("; ");
                let excludes = source.excludes.join("; ");
                if includes.len() > 4096
                    || excludes.len() > 4096
                    || source
                        .includes
                        .iter()
                        .chain(&source.excludes)
                        .any(|pattern| pattern.contains(';'))
                {
                    self.status =
                        Some("Some Java globs need manual entry in the folder options.".into());
                    return;
                }
                use knowledge_options::Edit;
                options.update(Edit::Recursive(source.recursive));
                options.update(Edit::GitIgnore(source.respect_gitignore));
                options.update(Edit::MaxFileBytes(source.max_file_bytes.to_string()));
                options.update(Edit::Includes(includes));
                options.update(Edit::Excludes(excludes));
                if let Err(error) = options.options() {
                    self.status = Some(format!(
                        "Review the Java globs in the folder options ({error})."
                    ));
                    return;
                }
                "Choose the local folder, then review and approve the resulting file list."
            }
        };
        self.source.clear();
        self.folder_review = None;
        self.folder_options = options;
        self.status = Some(notice.into());
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
        if self.busy() || !self.can_select_installed() {
            return None;
        }
        let id = match message {
            Message::Managed(managed::Message::SetDefault) => Some(self.installed_current()?),
            Message::Managed(managed::Message::ClearDefault) => None,
            _ => return None,
        };
        self.managed
            .default_change(id, state.default_managed_model.as_deref())
    }

    pub(super) fn set_managed_default(&mut self, id: Option<managed::ModelId>) {
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
                    && draft
                        .review
                        .as_ref()
                        .is_none_or(|review| &review.store == scope)
                    && !previous.iter().any(|old| old.same_snapshot(scope)))
                .then_some((draft, scope))
            })
            .map(|(draft, scope)| {
                Ok(StagedIndex {
                    index: Arc::clone(&draft.index),
                    scope: draft
                        .review
                        .as_ref()
                        .map_or_else(|| scope.clone(), |review| review.sources.clone()),
                    sources: draft
                        .review
                        .as_ref()
                        .map_or_else(|| scope.enabled_sources(), |review| review.coverage.clone()),
                    vault_key: resolve_reviewed_key(scope, vault, draft.vault_revision.as_ref())?,
                    completed: draft.review.as_ref().and_then(|review| {
                        next.stores
                            .iter()
                            .find(|store| {
                                next.scope(store, &self.profile_dir).as_ref() == Ok(scope)
                            })
                            .map(|store| (store.clone(), review.outcomes.clone()))
                    }),
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
            && self.knowledge_settings_title().is_none()
            && self.folder_review.is_none()
            && self.source_updates.is_none()
            && self.backend_repair.is_none()
            && self.runtime_removal.is_none()
    }

    pub(super) fn set_automatic_status(
        &mut self,
        status: BTreeMap<String, knowledge_sync::Status>,
        active: Option<(String, Arc<knowledge_sync::SourceReport>)>,
    ) {
        self.automatic_status = status;
        self.active_automatic_sources = active;
    }

    pub(super) fn set_runtime_update_status(&mut self, status: Option<String>) {
        self.runtime_update_status = status;
    }

    pub(super) fn set_runtime_downloads_allowed(&mut self, allowed: bool) {
        self.runtime_downloads_allowed = allowed;
    }

    pub(super) fn can_refresh_embedding_inventory(&self, editable: bool) -> bool {
        editable
            && self.request.is_none()
            && self.policy_embedding_load.is_none()
            && self
                .policy_embeddings
                .as_ref()
                .is_some_and(|models| models.unavailable)
    }

    pub(super) fn merge_automatic_knowledge(&mut self, state: &State) {
        self.discard_stale_drafts(state);
        self.hits.clear();
        self.folder_review = None;
        self.source_updates = None;
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
                    | Message::HubSizeResolved(..)
            )
        {
            return Task::none();
        }
        if self.edit_knowledge_settings(&message, state, vault) {
            if matches!(message, Message::EmbeddingSource(EmbeddingScope::Store, _)) {
                return self.suggest_local_dimensions();
            }
            return Task::none();
        }
        match message {
            Message::KnowledgeSettings(action) => {
                let create = matches!(action, knowledge_settings::Action::Open { create: true });
                self.update_knowledge_settings(action, state, vault);
                if create {
                    return self.suggest_local_dimensions();
                }
            }
            Message::LocalDimensionsDetected {
                store,
                model,
                value,
            } => self.accept_local_dimensions(&store, &model, value),
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
                return self.check_runtime_online();
            }
            Message::OpenStableRuntimeReview
                if ai_allowed
                    && self.request.is_none()
                    && !self.managed.has_action()
                    && self.runtime_removal.is_none() =>
            {
                let preferred = state
                    .runtime_update
                    .backend
                    .required()
                    .or(self.managed.runtime_summary().1);
                self.open_runtime_install(preferred, Vec::new());
            }
            Message::ConfirmStableRuntime if self.runtime_online_allowed(ai_allowed) => {
                if let Some(online::Review::Stable { approved, .. }) = &mut self.online.review {
                    *approved = true;
                    return self.check_runtime_online();
                }
            }
            Message::OpenRuntimeVersions
                if self.runtime_online_allowed(ai_allowed) && !self.managed.has_action() =>
            {
                self.online.review = Some(online::Review::Versions);
                return self.check_runtime_online();
            }
            Message::CloseRuntimeReview => self.close_runtime_review(),
            Message::OpenOfflineRuntimeFromReview
                if ai_allowed && self.request.is_none() && self.online.review.is_some() =>
            {
                self.close_runtime_review();
                self.reveal_offline_runtime();
                return Task::done(Message::RevealRuntimeImport);
            }
            Message::UnpinRuntime
                if self.runtime_online_allowed(ai_allowed) && self.online.has_pin() =>
            {
                if let Some(catalog) = self.online.catalog.clone() {
                    return self.run_managed(
                        runtime_package::online::unpin(self.profile_dir.clone(), catalog),
                        "Clearing the runtime pin…",
                        true,
                        |()| Output::RuntimeUnpinned,
                    );
                }
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
            Message::ToggleOfflineRuntime => self.offline_runtime_open = !self.offline_runtime_open,
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
            Message::ContinueReviewedModelStart(models) if ai_allowed && self.request.is_none() => {
                return self.start_reviewed_models(models);
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
            Message::Role(role, profile) => state.set_profile(role, profile),
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
            Message::CancelJavaKnowledgeStore => {
                if self.java_store_origin.take().is_some() {
                    self.store_name.clear();
                    self.status = None;
                }
            }
            Message::CreateStore => {
                if self.java_store_origin.is_some()
                    && !super::backup::java_ai_hints_allowed(&ClipboardPolicy::load())
                {
                    self.status =
                        Some("Managed policy prevents adopting Java knowledge stores.".into());
                    return Task::none();
                }
                let name = self.store_name.trim();
                if !name.is_empty()
                    && !name.chars().any(char::is_control)
                    && state.stores.len() < knowledge::MAX_STORES
                    && !state.stores.iter().any(|store| store.name == name)
                {
                    match Store::new(name) {
                        Ok(mut store) => {
                            store.java_origin = self.java_store_origin.take();
                            store.embedding = Some(if store.java_origin.is_some() {
                                Embedding {
                                    endpoint: local_ai::DEFAULT_ENDPOINT.into(),
                                    model: String::new(),
                                    managed_model: None,
                                }
                            } else {
                                state.default_embedding()
                            });
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
                return self.load_source_results(state);
            }
            Message::SourceResults(ticket, results) => self.source_results.loaded(ticket, results),
            Message::RefreshKnowledgeInventory if self.selected.is_some() && !self.busy() => {
                return self.load_source_results(state);
            }
            Message::SelectKnowledgeSource(index, kind, path) if self.selected == Some(index) => {
                let row = (kind, path);
                if let Some(store) = state.stores.get(index) {
                    let visible: Vec<_> = store.source_keys().collect();
                    if visible.contains(&row) {
                        self.source_removal = None;
                        self.source_sync_editor = None;
                        crate::input::select_row(
                            &mut self.selected_sources,
                            &mut self.source_anchor,
                            row,
                            &visible,
                            self.selection_modifiers,
                        );
                    }
                }
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
                        Role::Agent | Role::Journal | Role::Security => {}
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
            Message::ToggleManualSource => self.manual_source = !self.manual_source,
            Message::PickNativeKnowledgeSources(kind) if !self.busy() => {
                if matches!(
                    self.source_picker,
                    None | Some(
                        KnowledgeSourcePicker::ChooseInitial(_)
                            | KnowledgeSourcePicker::Retry(_)
                            | KnowledgeSourcePicker::ReviewFiles { .. }
                    )
                ) && let Some(store) = self.source_store(state)
                    && knowledge_backend::validate(store).is_ok()
                {
                    self.generation = self.generation.wrapping_add(1);
                    let ticket = self.generation;
                    self.source_picker = Some(KnowledgeSourcePicker::Native { ticket, kind });
                    self.status = None;
                    return Task::done(Message::OpenNativeKnowledgeSourcePicker(ticket));
                }
            }
            Message::NativeKnowledgeSourcesPicked(ticket, paths) => {
                if let Some(kind) = self.pending_native_knowledge_kind(ticket) {
                    if let Some(paths) = paths.filter(|paths| !paths.is_empty()) {
                        self.source_picker = Some(KnowledgeSourcePicker::Retry(kind));
                        match kind {
                            SourceKind::File => {
                                return self.review_picked_knowledge_files(state, paths, None);
                            }
                            SourceKind::Folder => {
                                if let [folder] = paths.as_slice() {
                                    return self
                                        .review_picked_knowledge_folder(state, folder.clone());
                                }
                                self.status = Some("Choose one folder to review.".into());
                            }
                        }
                    } else {
                        self.close_knowledge_source_picker();
                    }
                }
            }
            Message::BrowseKnowledgeSources(kind) if !self.busy() => {
                if let Some(store) = self.source_store(state)
                    && knowledge_backend::validate(store).is_ok()
                {
                    let mode = match kind {
                        SourceKind::File => file_chooser::Mode::KnowledgeFiles(
                            knowledge::MAX_SOURCES.saturating_sub(store.source_count()),
                        ),
                        SourceKind::Folder => file_chooser::Mode::KnowledgeFolder,
                    };
                    let (chooser, task) =
                        file_chooser::Chooser::open(mode, None, &mut self.generation);
                    self.source_picker = Some(KnowledgeSourcePicker::Browse(chooser));
                    self.status = None;
                    return task.map(Message::KnowledgeSourceBrowse);
                }
            }
            Message::KnowledgeSourceBrowse(message) if !self.busy() => {
                if let Some(KnowledgeSourcePicker::Browse(chooser)) = &mut self.source_picker {
                    return chooser
                        .update(message, &mut self.generation)
                        .map(Message::KnowledgeSourceBrowse);
                }
            }
            Message::UseBrowsedKnowledgeSources if !self.busy() => {
                if let Some(KnowledgeSourcePicker::Browse(chooser)) = &self.source_picker {
                    if let Some(folder) = chooser.chosen_folder() {
                        return self.review_picked_knowledge_folder(state, folder.to_owned());
                    }
                    if !chooser.selection().is_empty() {
                        return self.review_picked_knowledge_files(
                            state,
                            chooser.selection().to_vec(),
                            None,
                        );
                    }
                }
            }
            Message::ApproveBrowsedKnowledgeFiles if !self.busy() => {
                if let Some(KnowledgeSourcePicker::ReviewFiles {
                    review, size_limit, ..
                }) = &self.source_picker
                    && review.has_ready_files()
                    && knowledge_options::file_size(size_limit) == Ok(review.file_limit)
                {
                    match file_review::confirm(Arc::clone(review)) {
                        Ok((reply, cancel)) => {
                            return self.receive(
                                reply,
                                Request::Files { _cancel: cancel },
                                "Verifying the reviewed files…",
                                Output::ConfirmedFileReview,
                            );
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::FileReviewPage(next) => {
                if let Some(KnowledgeSourcePicker::ReviewFiles { review, page, .. }) =
                    &mut self.source_picker
                    && next < review.rows.len().div_ceil(SOURCE_REVIEW_PAGE_SIZE)
                {
                    *page = next;
                }
            }
            Message::FileReviewLimit(input) if !self.busy() => {
                if let Some(KnowledgeSourcePicker::ReviewFiles { size_limit, .. }) =
                    &mut self.source_picker
                {
                    *size_limit = limited(&input, 24).into();
                }
            }
            Message::ReviewWithFileLimit if !self.busy() => {
                if let Some(KnowledgeSourcePicker::ReviewFiles {
                    review, size_limit, ..
                }) = &self.source_picker
                {
                    match knowledge_options::file_size(size_limit) {
                        Ok(limit) => {
                            return self.review_picked_knowledge_files(
                                state,
                                review.rows.iter().map(|row| row.path.clone()).collect(),
                                limit,
                            );
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::CloseKnowledgeSourcePicker => self.close_knowledge_source_picker(),
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
            Message::ConfigureFolder(index)
                if !self.busy() && self.knowledge_settings_title().is_none() =>
            {
                if let Some(store) = self.selected
                    && let Some(folder) = state
                        .stores
                        .get(store)
                        .and_then(|store| store.folders.get(index))
                {
                    self.folder_advanced = Some(FolderAdvanced {
                        store,
                        path: folder.root.clone(),
                        original: folder.options.clone(),
                        draft: knowledge_options::Draft::advanced(&folder.options),
                    });
                    self.status = None;
                }
            }
            Message::EditFolderAdvanced(edit) if !self.busy() => {
                if let Some(advanced) = &mut self.folder_advanced {
                    advanced.draft.update(edit);
                    self.status = None;
                }
            }
            Message::CancelFolderAdvanced => self.folder_advanced = None,
            Message::ReviewFolderAdvanced if !self.busy() => {
                if let Some(advanced) = &self.folder_advanced {
                    let pending = state.stores.get(advanced.store)
                        .filter(|store| self.selected == Some(advanced.store) && store.folders.iter().any(|folder| {
                            folder.root == advanced.path && folder.options == advanced.original
                        }))
                        .ok_or_else(|| "The selected folder changed. Reopen Advanced to review the current options.".to_owned())
                        .and_then(|store| {
                            knowledge_backend::validate(store)?;
                            let options = advanced.draft.options()?;
                            store.folder_review_options(advanced.path.clone(), options)
                        });
                    if pending.is_ok() {
                        self.folder_advanced = None;
                        self.folder_review = None;
                    }
                    return self.start_folder_review(pending);
                }
            }
            Message::AddSource if !self.busy() && !self.knowledge_source_picker_open() => {
                return self.review_picked_knowledge_files(
                    state,
                    vec![PathBuf::from(self.source.trim())],
                    None,
                );
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
            Message::ReviewSourceRemoval(index)
                if self.selected == Some(index)
                    && !self.busy()
                    && !self.knowledge_source_picker_open()
                    && self.knowledge_settings_title().is_none() =>
            {
                if let Some(store) = state.stores.get(index)
                    && !self.selected_sources.is_empty()
                {
                    self.source_removal = Some(SourceRemoval {
                        store: index,
                        id: store.id().map(str::to_owned),
                        name: store.name.clone(),
                        sources: self.selected_sources.clone(),
                    });
                }
            }
            Message::CancelSourceRemoval => self.source_removal = None,
            Message::RemoveSelectedSources if !self.busy() => self.remove_selected_sources(state),
            Message::OpenSourceSync(index)
                if self.selected == Some(index)
                    && !self.busy()
                    && self.knowledge_settings_title().is_none() =>
            {
                self.source_sync_editor = self.source_sync_edit(state, index);
            }
            Message::SourceSyncMode(mode) => {
                if let Some(edit) = &mut self.source_sync_editor {
                    edit.mode = mode;
                }
            }
            Message::CancelSourceSync => self.source_sync_editor = None,
            Message::ApplySourceSync if !self.busy() => {
                if let Some(edit) = self.source_sync_editor.take()
                    && self.selected == Some(edit.store)
                    && self
                        .source_sync_edit(state, edit.store)
                        .is_some_and(|current| {
                            current.source == edit.source && current.mode != edit.mode
                        })
                {
                    let (kind, path) = edit.source;
                    let store = &mut state.stores[edit.store];
                    match kind {
                        SourceKind::File => {
                            if store.set_file_sync(path, edit.mode) {
                                self.status = Some(
                                    "Source sync mode changed in the draft. Save to apply it."
                                        .into(),
                                );
                            }
                        }
                        SourceKind::Folder => {
                            if let Some(folder) =
                                store.folders.iter().find(|folder| folder.root == path)
                            {
                                self.folder_options.set(&folder.options);
                                self.folder_options
                                    .update(knowledge_options::Edit::Sync(edit.mode));
                                self.source = path.display().to_string();
                                self.manual_source = true;
                                self.folder_review = None;
                                return self.review_folder(store, path);
                            }
                        }
                    }
                }
            }
            Message::ConfigureFileSize(store, path)
                if self.selected == Some(store)
                    && !self.busy()
                    && (self.file_size.is_some() || self.knowledge_settings_title().is_none()) =>
            {
                if let Some(store) = state.stores.get(store)
                    && store.sources.contains(&path)
                {
                    let input = knowledge_options::format_advanced_limit(
                        store
                            .file_size_limit(&path)
                            .unwrap_or(knowledge::source::NEW_SOURCE_DEFAULT),
                    );
                    self.file_size = Some(FileSizeEdit {
                        path,
                        input,
                        edited: false,
                    });
                    self.status = None;
                }
            }
            Message::FileSize(value) if !self.busy() => {
                if let Some(edit) = &mut self.file_size {
                    edit.input = limited(&value, 32).into();
                    edit.edited = true;
                    self.status = None;
                }
            }
            Message::ApplyFileSize(store, path) if self.selected == Some(store) && !self.busy() => {
                if let Some(edit) = self.file_size.as_ref().filter(|edit| edit.path == path)
                    && let Some(store) = state.stores.get_mut(store)
                {
                    let automatic = store.file_sync(&path) == SyncMode::Automatic;
                    let result = if edit.edited {
                        knowledge_options::advanced_file_size(&edit.input)
                            .and_then(|limit| store.set_file_size_limit(path, Some(limit)))
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
            Message::ReviewSourceUpdates(index)
                if !self.busy()
                    && self.selected == Some(index)
                    && self.folder_review.is_none()
                    && !self.knowledge_source_picker_open()
                    && self.knowledge_settings_title().is_none() =>
            {
                if let Some(store) = state.stores.get(index) {
                    match knowledge_backend::validate(store)
                        .and_then(|()| store.review_source_updates(index, &self.selected_sources))
                    {
                        Ok((reply, cancel)) => {
                            self.source_work = Some(SourceWork::new(
                                store,
                                store.source_keys().filter(|(kind, path)| {
                                    store.source_enabled(*kind, path)
                                        && (self.selected_sources.is_empty()
                                            || self
                                                .selected_sources
                                                .contains(&(*kind, path.clone())))
                                }),
                                SourcePhase::Scanning,
                            ));
                            return self.receive(
                                reply,
                                Request::SourceUpdates {
                                    cancel: Some(cancel),
                                },
                                "Checking the enabled sources…",
                                Output::SourceUpdates,
                            );
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::ApproveSourceUpdates if !self.busy() => {
                if let Some(review) = self
                    .source_updates
                    .take()
                    .filter(|review| review.can_approve())
                {
                    let result = state
                        .stores
                        .get_mut(review.store)
                        .filter(|_| {
                            self.selected == Some(review.store)
                                && self.selected_sources == review.selection
                        })
                        .ok_or_else(|| {
                            "The store or selection changed. Review the selected sources again."
                                .to_owned()
                        })
                        .and_then(|store| {
                            store.approve_source_updates(&review)?;
                            Ok(store.clone())
                        });
                    self.status = Some(match result {
                        Ok(definition) => {
                            self.invalidate_index();
                            self.discard_stale_drafts(state);
                            self.clear_consent();
                            let indexable = review.has_indexable_files();
                            self.approved_update =
                                Some(ApprovedSourceUpdate { definition, review });
                            if indexable {
                                "Review approved. Enable embedding consent, then choose Index reviewed sources.".into()
                            } else {
                                "Review approved. Save the settings to apply the folder changes."
                                    .into()
                            }
                        }
                        Err(error) => error,
                    });
                }
            }
            Message::CancelSourceUpdates => {
                self.source_updates = None;
                self.status = None;
            }
            Message::ApproveFolder
                if matches!(
                    self.source_picker,
                    Some(KnowledgeSourcePicker::InitialFolder)
                ) =>
            {
                if let Some(review) = &self.folder_review
                    && can_approve_folder_review(review)
                    && let Some(mut store) = self.new_store.clone()
                {
                    match knowledge_backend::validate(&store)
                        .and_then(|()| store.approve_folder(review))
                    {
                        Ok(()) => self.publish_new_store(state, store),
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::ApproveFolder if !self.knowledge_source_picker_open() => {
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
                if self
                    .folder_review
                    .as_deref()
                    .is_some_and(|review| page < folder_review_pages(review))
                    || page < self.source_update_pages()
                {
                    self.folder_review_page = page;
                }
            }
            Message::DiscardFolderReview => {
                self.folder_review = None;
                if matches!(
                    self.source_picker,
                    Some(KnowledgeSourcePicker::InitialFolder)
                ) {
                    self.cancel();
                    self.source_picker =
                        Some(KnowledgeSourcePicker::ChooseInitial(SourceKind::Folder));
                    self.status = None;
                } else {
                    self.status = Some("Folder review discarded.".into());
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
            Message::Index { .. } | Message::IndexReviewed
                if ai_allowed
                    && self.consent
                    && self.request.is_none()
                    && self.folder_review.is_none()
                    && self.source_updates.is_none() =>
            {
                if let Some(store) = self.selected.and_then(|index| state.stores.get(index))
                    && store.has_enabled_sources()
                {
                    let rebuild = matches!(message, Message::Index { rebuild: true });
                    let approval = if matches!(message, Message::IndexReviewed) {
                        let Some(review) = self.approved_source_update(store) else {
                            self.status =
                                Some("Review the current selection before indexing.".into());
                            return Task::none();
                        };
                        Some(Arc::clone(review))
                    } else {
                        None
                    };
                    self.hits.clear();
                    let cache = self.cache.clone();
                    let profile_dir = self.profile_dir.clone();
                    let baseline = saved.clone();
                    let pending = state.scope(store, &self.profile_dir).and_then(|scope| {
                        let mut reviewed = if !rebuild && !self.selected_sources.is_empty() {
                            scope.for_source_rows(store, &self.selected_sources)
                        } else {
                            scope.clone()
                        };
                        if let Some(approval) = &approval {
                            reviewed.apply_source_reviews(approval);
                        }
                        if reviewed.enabled_sources() == SourceMask::default() {
                            return Err("Select an enabled source to update.".into());
                        }
                        let partial = reviewed.enabled_sources() != scope.enabled_sources();
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
                            .find(|draft| !persistent && scope.same_as(&draft.index)
                                && draft.review.as_ref().is_none_or(|review| review.store == scope))
                            .map(|draft| (Arc::clone(&draft.index), draft.review.clone()));
                        let rows: BTreeMap<_, _> = reviewed.source_rows(store).collect();
                        let work = SourceWork::new(store, rows.values().cloned(), SourcePhase::Indexing);
                        let attempted_store = store.clone();
                        let pending = local_ai::start(scope.profile.clone(), move |client| {
                            let authorize = || {
                                knowledge::check_active(client)?;
                                scope.profile.require_current_policy_authority()?;
                                if let Some(settings) = &scope.qdrant {
                                    remote_index::require_policy()?;
                                    settings.require_vault_key(vault_key.as_deref())?;
                                }
                                Configuration::require_local_settings(&profile_dir, &baseline)
                            };
                            let prior_review = draft.as_ref().and_then(|(_, review)| review.as_ref()).cloned();
                            let (previous, previous_stamp) = if let Some((index, _)) = draft {
                                (Some((scope.clone(), index)), None)
                            } else if let Some(base) = base {
                                match cache.load_file_backed(&base, || knowledge::check_active(client)) {
                                    Ok((index, stamp)) => (Some((base, Arc::new(index))), Some(stamp)),
                                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error.to_string()),
                                    Err(error) if rebuild || error.kind() == io::ErrorKind::NotFound => (None, None),
                                    Err(error) => return Err(format!(
                                        "Could not load the previous knowledge index ({error}). Choose Rebuild index to replace it."
                                    )),
                                }
                            } else {
                                (None, None)
                            };
                            authorize()?;
                            let mut failed_source = None;
                            let result = Index::update_observed(
                                client,
                                &reviewed,
                                previous
                                    .as_ref()
                                    .map(|(base, index)| (base, index.as_ref())),
                                rebuild,
                                authorize,
                                &mut failed_source,
                            );
                            let (index, summary) = match result {
                                Ok(result) => result,
                                Err(message) => match failed_source.and_then(|position| rows.get(&position))
                                    .or_else(|| rows.values().next().filter(|first| rows.values().all(|row| row == *first))) {
                                    Some(source) => {
                                        authorize()?;
                                        let mut message = message;
                                        if persistent && outcomes::save(
                                            &profile_dir, &baseline, &attempted_store,
                                            &BTreeMap::from([(source.clone(), SourceStatus::Error)]),
                                        ).is_err() {
                                            message.push(' ');
                                            message.push_str(outcomes::SAVE_WARNING);
                                        }
                                        return Ok(Output::SourceIssue(SourceIssue {
                                            source: source.clone(),
                                            message,
                                        }));
                                    }
                                    None => return Err(message),
                                },
                            };
                            let mut coverage = previous.as_ref().map_or_else(
                                || reviewed.enabled_sources(),
                                |(base, index)| remote_index::retained_coverage(&scope, &reviewed, base, Some(index)),
                            );
                            let mut completed: outcomes::Rows = rows.values().map(|source| {
                                let problems = attempted_store.folders.iter()
                                    .find(|folder| source.0 == SourceKind::Folder && folder.root == source.1)
                                    .and_then(|folder| folder.review_problems)
                                    .unwrap_or_default();
                                (source.clone(), SourceStatus::Checked { problems })
                            }).collect();
                            let mut reviewed = reviewed;
                            if let Some(prior) = prior_review {
                                reviewed.include_previous_reviews(&prior.sources);
                                coverage = coverage.union(&prior.coverage);
                                for (source, status) in &prior.outcomes {
                                    completed.entry(source.clone()).or_insert(*status);
                                }
                            }
                            let remote_missing = scope.qdrant.is_some()
                                && previous.as_ref().and_then(|(base, index)| {
                                    base.same_snapshot(&scope).then_some(index.qdrant_receipt.as_ref()).flatten()
                                }).is_none_or(|receipt| !receipt.sources.covers(&coverage));
                            let index_was_missing = previous.is_none();
                            let previous_scope = previous.map(|(base, _)| base);
                            let require_previous = || {
                                if (scope.qdrant.is_none() || partial)
                                    && let Some((base, stamp)) = previous_scope.as_ref().zip(previous_stamp.as_ref())
                                {
                                    cache.require_current(base, stamp)?;
                                } else if partial && persistent && index_was_missing {
                                    cache.require_absent(&scope)?;
                                }
                                Ok::<(), io::Error>(())
                            };
                            require_previous().map_err(|error| format!("The saved knowledge index changed ({error}). Update the index again."))?;
                            let mut cleanup_warning = false;
                            if persistent && (summary.indexed != 0 || summary.removed != 0 || remote_missing || index_was_missing) {
                                cleanup_warning = Configuration::publish_knowledge_index(&profile_dir, |current| {
                                    if !current.index_scopes(&profile_dir).contains(&scope) {
                                        return Err(io::Error::other("The saved knowledge settings changed. Update the index again."));
                                    }
                                    remote_index::publish(&cache, &reviewed, &index, coverage.clone(), Some(client), vault_key.clone(), || {
                                        authorize()?;
                                        require_previous().map_err(|error| format!("The saved knowledge index changed ({error}). Update the index again."))
                                    })
                                })
                                    .map_err(|error| {
                                        format!("Could not save the knowledge index ({error}).")
                                    })?.cleanup_warning;
                            }
                            let outcome_warning = persistent
                                && outcomes::save(&profile_dir, &baseline, &attempted_store, &completed).is_err();
                            let revision = vault_key.as_deref().map(VaultKey::revision);
                            Ok(Output::Index(Arc::new(index), persistent, summary, cleanup_warning, revision,
                                Arc::new(IndexReview { store: scope, sources: reviewed, coverage, outcome_warning, outcomes: completed })))
                        })?;
                        Ok((work, pending))
                    });
                    match pending {
                        Ok((work, Pending { reply, cancel })) => {
                            self.source_work = Some(work);
                            return self.receive(
                                reply,
                                Request::SourceIndex {
                                    cancel: Some(cancel),
                                },
                                "Indexing the reviewed sources…",
                                |output| output,
                            );
                        }
                        Err(error) => self.status = Some(error),
                    }
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
                    && self.folder_review.is_none()
                    && self.source_updates.is_none() =>
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
                                    let (index, stamp) = cache
                                        .load_file_backed(&scope, || knowledge::check_active(client))
                                        .map_err(|error| match error.kind() {
                                            io::ErrorKind::NotFound => "No saved index matches these settings. Choose Update index.".into(),
                                            io::ErrorKind::Interrupted => error.to_string(),
                                            _ => format!("Could not load the saved index ({error}). Choose Update index."),
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
                            if let Some(stamp) = &stamp {
                                cache.require_current(&scope, stamp).map_err(|_| "The knowledge index changed. Search again.")?;
                            }
                            scope.profile.require_current_policy_authority()?;
                            Configuration::require_local_settings(&profile_dir, &baseline)?;
                            Ok(Output::Hits { query, hits, vault_revision })
                        })
                    });
                    return self.run(pending, |output| output);
                }
            }
            Message::Cancel => self.cancel_request(),
            Message::HubSizeResolved(revision, repository, bytes) if ai_allowed => {
                return self.resolve_hub_size(revision, repository, bytes, vault);
            }
            Message::ManagedBackendLoaded(generation, package) if generation == self.generation => {
                self.managed.set_active_package(package);
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
                            | Request::SourceUpdates { .. }
                            | Request::Files { .. }
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
                if matches!(request, Some(Request::SourceUpdates { .. }))
                    && let Some(work) = &mut self.source_work
                    && work.phase == SourcePhase::Cancelling
                {
                    work.phase = SourcePhase::Cancelled;
                    self.status = Some("Source update cancelled.".into());
                    return Task::none();
                }
                if matches!(
                    request,
                    Some(Request::SourceUpdates { .. } | Request::SourceIndex { .. })
                ) {
                    match &result {
                        Ok(Output::SourceIssue(issue)) => {
                            if let Some(work) = &mut self.source_work {
                                if work.phase == SourcePhase::Cancelling {
                                    work.phase = SourcePhase::Cancelled;
                                } else {
                                    work.phase = SourcePhase::Interrupted;
                                    work.failed = Some(issue.source.clone());
                                }
                            }
                        }
                        Err(_) => {
                            if let Some(work) = &mut self.source_work {
                                work.phase = if work.phase == SourcePhase::Cancelling {
                                    SourcePhase::Cancelled
                                } else {
                                    SourcePhase::Interrupted
                                };
                            }
                        }
                        Ok(_) => self.source_work = None,
                    }
                    if self
                        .source_work
                        .as_ref()
                        .is_some_and(|work| work.phase == SourcePhase::Cancelled)
                    {
                        self.status = Some("Source update cancelled.".into());
                        return Task::none();
                    }
                }
                self.status = Some(match result {
                    Ok(Output::Models(models)) => {
                        self.model = self.model.take().filter(|model| models.contains(model));
                        self.models = models;
                        format!("{} installed models", self.models.len())
                    }
                    Ok(Output::ModelTest) => "The model replied to the connection test.".into(),
                    Ok(Output::HuggingFace(output))
                        if matches!(output, huggingface::Output::Search(..)) =>
                    {
                        self.status = Some(self.finish_huggingface(output));
                        return self.fill_hub_sizes(vault);
                    }
                    Ok(Output::HuggingFace(output)) => self.finish_huggingface(output),
                    Ok(Output::RuntimeCatalog(catalog)) => {
                        let count = catalog.choices().len();
                        if let Some(online::Review::Stable {
                            approved: true,
                            required,
                            ..
                        }) = &self.online.review
                            && let Some(choice) = catalog.preferred_choice(*required).cloned()
                        {
                            self.online.catalog = Some(catalog);
                            self.online.selected = Some(choice);
                            return self.install_runtime_online();
                        }
                        if matches!(self.online.review, Some(online::Review::Versions))
                            && let Some(active) = self.managed.runtime_summary().0
                        {
                            self.online.selected = catalog
                                .choices()
                                .iter()
                                .find(|choice| choice.id == active)
                                .cloned();
                        }
                        self.online.catalog = Some(catalog);
                        if count == 0 {
                            "The signed channel has no compatible package for this host and backend.".into()
                        } else {
                            format!(
                                "Found {count} compatible signed runtime packages. Choose a package to review."
                            )
                        }
                    }
                    Ok(Output::RuntimeUnpinned) => {
                        self.online.pin_cleared = true;
                        "The installed runtime now follows stable updates.".into()
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
                        let reviewed = self.online.selected.as_ref().is_some_and(|choice| {
                            choice.id == id && Some(choice.backend) == backend
                        });
                        self.online.selected = None;
                        self.managed
                            .set_active_package(backend.map(|backend| (id.clone(), backend)));
                        if reviewed
                            && let Some(online::Review::Stable {
                                approved: true,
                                start,
                                ..
                            }) = self.online.review.take()
                            && !start.is_empty()
                        {
                            self.status = Some(format!("Installed {id}."));
                            return Task::done(Message::ContinueReviewedModelStart(start));
                        }
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
                        self.managed.set_active_package(None);
                        self.refresh_managed_model_status();
                        warning.unwrap_or_else(|| "Removed the installed llama.cpp runtime.".into())
                    }
                    Ok(Output::ManagedModels {
                        models,
                        default,
                        active_package,
                        mlx,
                        notice,
                    }) => {
                        let offer_backend = self.managed.configuring();
                        self.managed.set_active_package(active_package);
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
                        let status = match mlx {
                            Some(Ok(output)) => {
                                self.mlx.finish(output, &self.profile_dir);
                                status
                            }
                            Some(Err(error)) => {
                                format!("{status}. Could not read the MLX inventory ({error}).")
                            }
                            None => status,
                        };
                        self.preselect_installed_default(state.default_managed_model.as_deref());
                        if offer_backend {
                            self.managed.offer_matching_backend();
                        }
                        status
                    }
                    Ok(Output::ManagedNotice(notice)) => {
                        self.refresh_managed_model_status();
                        notice
                    }
                    Ok(Output::Mlx(output)) => {
                        let status = self.mlx.finish(output, &self.profile_dir);
                        self.preselect_installed_default(state.default_managed_model.as_deref());
                        status
                    }
                    Ok(Output::FileReview(review)) => {
                        self.source_picker = Some(KnowledgeSourcePicker::ReviewFiles {
                            size_limit: knowledge_options::file_size_text(review.file_limit),
                            review,
                            page: 0,
                        });
                        self.status = None;
                        return Task::none();
                    }
                    Ok(Output::ConfirmedFileReview(confirmed)) => {
                        self.commit_reviewed_files(state, confirmed);
                        return Task::none();
                    }
                    Ok(Output::SourceUpdates(updates)) => {
                        if self.selected != Some(updates.store)
                            || self.selected_sources != updates.selection
                        {
                            "The store or selection changed. Review the selected sources again."
                                .into()
                        } else {
                            let outcomes = updates
                                .files
                                .iter()
                                .filter(|row| matches!(row.status, file_review::Status::Skipped(_)))
                                .map(|row| {
                                    (
                                        (SourceKind::File, row.path.clone()),
                                        SourceStatus::Unavailable,
                                    )
                                })
                                .chain(
                                    updates
                                        .issues
                                        .iter()
                                        .map(|issue| (issue.source.clone(), SourceStatus::Error)),
                                )
                                .collect();
                            let outcome_warning =
                                state.stores.get(updates.store).is_some_and(|store| {
                                    outcomes::save(&self.profile_dir, saved, store, &outcomes)
                                        .is_err()
                                });
                            self.refresh_source_outcomes(state);
                            self.source_updates = Some(updates);
                            self.folder_review_page = 0;
                            if outcome_warning {
                                format!(
                                    "Scan complete. Review the proposed source updates. {}",
                                    outcomes::SAVE_WARNING
                                )
                            } else {
                                "Scan complete. Review the proposed source updates.".into()
                            }
                        }
                    }
                    Ok(Output::SourceIssue(issue)) => issue.message,
                    Ok(Output::FolderReview(review)) => {
                        self.folder_review = Some(review);
                        self.folder_review_page = 0;
                        self.status = Some(
                            "Folder scan complete. Review the selection before approving it."
                                .into(),
                        );
                        if matches!(
                            self.source_picker,
                            Some(KnowledgeSourcePicker::InitialFolder)
                        ) {
                            return Task::none();
                        }
                        return Task::done(Message::RevealKnowledgeSetting(
                            super::search::KnowledgeSetting::FolderReviewSummary,
                        ));
                    }
                    Ok(Output::Index(
                        index,
                        persistent,
                        summary,
                        cleanup_warning,
                        revision,
                        review,
                    )) => {
                        if persistent {
                            self.source_results.replace(
                                &self.profile_dir,
                                state,
                                self.selected,
                                &index,
                                &review,
                            );
                        }
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
                        if review.outcome_warning {
                            detail.push_str(". ");
                            detail.push_str(outcomes::SAVE_WARNING.trim_end_matches('.'));
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
                            self.retain_draft(state, index, revision, review, &detail)
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
                    Err(error) => {
                        if matches!(
                            self.source_picker,
                            Some(KnowledgeSourcePicker::ScanningFiles)
                        ) {
                            self.source_picker =
                                Some(KnowledgeSourcePicker::Retry(SourceKind::File));
                        }
                        error
                    }
                });
                if refresh_backend {
                    let profile = self.profile_dir.clone();
                    return Task::perform(
                        async move {
                            runtime_package::active_package_summary(&profile)
                                .ok()
                                .flatten()
                        },
                        move |package| Message::ManagedBackendLoaded(generation, package),
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
        self.start_folder_review(pending)
    }

    fn start_folder_review(
        &mut self,
        pending: Result<folder::ReviewTask<Arc<folder::Review>>, String>,
    ) -> Task<Message> {
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
        self.backend_repair
            .as_ref()
            .map(|repair| repair.required)
            .or_else(|| {
                self.online
                    .review
                    .as_ref()
                    .and_then(online::Review::required)
            })
    }

    fn open_runtime_install(
        &mut self,
        required: Option<PackageBackend>,
        start: Vec<managed::ReviewedModel>,
    ) {
        self.online.reset_catalog();
        self.online.review = Some(online::Review::Stable {
            upgrade: self.managed.runtime_summary().0.is_some(),
            required,
            start,
            approved: false,
        });
        self.status = None;
    }

    fn check_runtime_online(&mut self) -> Task<Message> {
        self.online.reset_catalog();
        self.run_managed(
            runtime_package::online::check(self.profile_dir.clone(), self.required_runtime()),
            "Checking the signed runtime channel…",
            true,
            Output::RuntimeCatalog,
        )
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

    fn cancel_request(&mut self) {
        match self.request.as_mut() {
            Some(Request::SourceUpdates { cancel }) => drop(cancel.take()),
            Some(Request::SourceIndex { cancel }) => drop(cancel.take()),
            _ => {
                self.cancel();
                return;
            }
        }
        if let Some(work) = &mut self.source_work {
            work.phase = SourcePhase::Cancelling;
        }
        self.status = Some("Cancelling the source update…".into());
    }

    fn source_request(&self) -> bool {
        matches!(
            self.request,
            Some(Request::SourceUpdates { .. } | Request::SourceIndex { .. })
        )
    }

    fn invalidate_index(&mut self) {
        self.cancel();
        self.source_work = None;
        self.folder_review = None;
        self.source_updates = None;
        self.approved_update = None;
        self.hits.clear();
    }

    fn approved_source_update(&self, store: &Store) -> Option<&Arc<SourceUpdates>> {
        self.approved_update
            .as_ref()
            .filter(|approved| {
                approved.definition == *store
                    && self.selected == Some(approved.review.store)
                    && self.selected_sources == approved.review.selection
                    && approved.review.has_indexable_files()
            })
            .map(|approved| &approved.review)
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

    fn automatic_destination_approved(&self, state: &State, store: &Store) -> bool {
        state.automatic_ready(store)
            && self
                .review_policy_embedding(
                    state.embedding_fields(store).2,
                    store.automatic_policy_model,
                )
                .is_ok()
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
        review: Arc<IndexReview>,
        detail: &str,
    ) -> String {
        let Some(scope) = self
            .selected
            .and_then(|selected| state.stores.get(selected))
            .and_then(|store| state.scope(store, &self.profile_dir).ok())
            .filter(|scope| scope.same_as(&index) && *scope == review.store)
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
        self.source_results
            .replace(&self.profile_dir, state, self.selected, &index, &review);
        self.drafts.push(DraftIndex {
            index,
            vault_revision,
            review: Some(review),
        });
        format!("Prepared {detail}. Save the settings to keep the index.")
    }

    pub(super) fn suspend(&mut self) {
        self.close_knowledge_source_picker();
        self.close_knowledge_settings();
        self.invalidate_index();
        self.drafts.clear();
        self.clear_consent();
        self.source_results.reset();
        self.managed.dismiss_action();
        self.mlx.invalidate_review();
        self.online = online::State::default();
        self.hf.suspend();
    }

    fn select_store(&mut self, index: Option<usize>) {
        self.invalidate_index();
        self.selected = index;
        self.source_results.reset();
        self.selected_sources.clear();
        self.source_sync_editor = None;
        self.source_anchor = None;
        self.source_removal = None;
        self.close_knowledge_source_picker();
        self.close_knowledge_settings();
        self.manual_source = false;
        if self.java_store_origin.take().is_some() {
            self.store_name.clear();
        }
        self.confirm_remove = false;
        self.clear_consent();
        self.status = None;
        self.source.clear();
        self.knowledge_backend = knowledge_backend::Editor::default();
        self.folder_options = knowledge_options::Draft::default();
        self.file_size = None;
        self.folder_advanced = None;
        self.query.clear();
    }

    pub(super) fn set_selection_modifiers(&mut self, modifiers: iced::keyboard::Modifiers) {
        self.selection_modifiers = modifiers;
    }

    fn remove_selected_sources(&mut self, state: &mut State) {
        let Some(review) = self.source_removal.take() else {
            return;
        };
        let Some(store) = state.stores.get_mut(review.store).filter(|store| {
            self.selected == Some(review.store)
                && store.id() == review.id.as_deref()
                && store.name == review.name
                && self.selected_sources == review.sources
                && review.sources.is_subset(&store.source_keys().collect())
        }) else {
            self.status = Some("The store or selection changed. Review the removal again.".into());
            return;
        };
        let contains = |kind, path: &PathBuf| review.sources.contains(&(kind, path.clone()));
        for position in (0..store.sources.len()).rev() {
            if contains(SourceKind::File, &store.sources[position]) {
                store.remove_file(position);
            }
        }
        for position in (0..store.folders.len()).rev() {
            if contains(SourceKind::Folder, &store.folders[position].root) {
                store.remove_folder(position);
            }
        }
        self.selected_sources.clear();
        self.source_anchor = None;
        self.source_sync_editor = None;
        self.file_size = None;
        self.folder_advanced = None;
        self.folder_review = None;
        self.folder_options = knowledge_options::Draft::default();
        self.invalidate_index();
        self.discard_stale_drafts(state);
        self.clear_consent();
        self.status = Some(format!(
            "Removed {} source(s) from the draft. Save to apply the change.",
            review.sources.len()
        ));
    }

    pub(super) fn knowledge_source_picker_open(&self) -> bool {
        self.source_picker.is_some()
    }

    fn source_store<'a>(&'a self, state: &'a State) -> Option<&'a Store> {
        self.new_store
            .as_ref()
            .or_else(|| self.selected.and_then(|index| state.stores.get(index)))
    }

    pub(super) fn pending_native_knowledge_kind(&self, ticket: u64) -> Option<SourceKind> {
        match self.source_picker {
            Some(KnowledgeSourcePicker::Native {
                ticket: current,
                kind,
            }) if current == ticket => Some(kind),
            _ => None,
        }
    }

    fn review_picked_knowledge_files(
        &mut self,
        state: &State,
        paths: Vec<PathBuf>,
        file_limit: Option<u64>,
    ) -> Task<Message> {
        let Some(store) = self.source_store(state) else {
            return Task::none();
        };
        let pending = knowledge_backend::validate(store)
            .and_then(|()| store.check_source_file_overlaps(&paths))
            .and_then(|()| file_review::start(paths, file_limit));
        match pending {
            Ok((reply, cancel)) => {
                self.source_picker = Some(KnowledgeSourcePicker::ScanningFiles);
                self.receive(
                    reply,
                    Request::Files { _cancel: cancel },
                    "Checking the selected files…",
                    Output::FileReview,
                )
            }
            Err(error) => {
                self.status = Some(error);
                Task::none()
            }
        }
    }

    fn commit_reviewed_files(&mut self, state: &mut State, confirmed: file_review::Confirmed) {
        let Some(store) = self.source_store(state) else {
            return;
        };
        match knowledge_backend::validate(store)
            .and_then(|()| store.review_source_files(&confirmed.paths, confirmed.file_limit))
        {
            Ok(updated) if self.new_store.is_some() => self.publish_new_store(state, updated),
            Ok(updated) => {
                if let Some(index) = self.selected {
                    state.stores[index] = updated;
                    self.source.clear();
                    self.close_knowledge_source_picker();
                    self.invalidate_index();
                    self.discard_stale_drafts(state);
                    self.clear_consent();
                    self.status = Some(format!(
                        "Added {} source files to the draft.",
                        confirmed.paths.len()
                    ));
                }
            }
            Err(error) => self.status = Some(error),
        }
    }

    fn review_picked_knowledge_folder(&mut self, state: &State, folder: PathBuf) -> Task<Message> {
        if let Some(store) = &self.new_store {
            let pending = knowledge_backend::validate(store)
                .and_then(|()| store.folder_review_options(folder, Default::default()));
            self.source_picker = Some(KnowledgeSourcePicker::InitialFolder);
            self.folder_review = None;
            return self.start_folder_review(pending);
        }
        let Some(store) = self.selected.and_then(|index| state.stores.get(index)) else {
            return Task::none();
        };
        self.source_picker = None;
        self.source = file_chooser::display_path(&folder).into_owned();
        self.manual_source = true;
        self.folder_options = knowledge_options::Draft::default();
        self.review_folder(store, folder)
    }

    fn publish_new_store(&mut self, state: &mut State, store: Store) {
        match state.new_store_index(&store).and_then(|index| {
            store.validate()?;
            Ok(index)
        }) {
            Ok(index) => {
                state.stores.push(store);
                self.select_store(Some(index));
                self.status = Some(
                    "Knowledge store and sources added to the draft. Save AI Manager to apply them."
                        .into(),
                );
            }
            Err(error) => self.status = Some(error),
        }
    }

    pub(super) fn close_knowledge_source_picker(&mut self) {
        if let Some(picker) = self.source_picker.take() {
            if matches!(
                picker,
                KnowledgeSourcePicker::ScanningFiles | KnowledgeSourcePicker::ReviewFiles { .. }
            ) {
                self.cancel();
            }
            if matches!(picker, KnowledgeSourcePicker::InitialFolder) {
                self.cancel();
                self.folder_review = None;
            }
            self.status = None;
        }
        self.new_store = None;
    }

    pub(super) fn view<'a>(
        &'a self,
        page: Page<'a>,
        state: &'a State,
        profiles: &Catalog,
        editable: bool,
        ai_enabled: bool,
        reveal: &'a Reveal,
    ) -> Element<'a, Message> {
        use super::search::{Control, LocalPreference, Setting};

        let can_edit = editable && self.request.is_none();
        let can_request = can_edit && ai_enabled;
        if matches!(&page, Page::Knowledge(..))
            && let Some(picker) = &self.source_picker
        {
            return self.knowledge_source_picker_view(picker, can_edit, reveal);
        }
        let models_page = matches!(page, Page::Models { .. });
        let preferences_page = matches!(page, Page::Preferences(..));
        let mark_progress = |control, content: Element<'a, Message>| {
            if models_page && self.runtime_request_active() {
                reveal.action(Setting::Control(control), content)
            } else {
                content
            }
        };
        let progress = row![
            self.status
                .as_ref()
                .filter(|_| !preferences_page || self.request.is_some())
                .map(|status| {
                    container(scrollable(
                        text(status.as_str()).wrapping(text::Wrapping::WordOrGlyph),
                    ))
                    .width(Length::Fill)
                    .max_height(48)
                }),
            (self.request.is_some()
                && !(matches!(page, Page::Knowledge(..)) && self.source_request()))
            .then(|| mark_progress(
                Control::CancelRuntimeRequest,
                button("Cancel request").on_press(Message::Cancel).into()
            )),
            self.online.can_pause().then(|| mark_progress(
                Control::PauseRuntimeDownload,
                button("Pause download")
                    .on_press(Message::PauseRuntimeDownload)
                    .into(),
            )),
            self.online.progress.map(|progress| text(format!(
                "{} / {} MiB",
                progress.received / (1024 * 1024),
                progress.total.div_ceil(1024 * 1024)
            ))
            .size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if models_page && let Some(repair) = &self.backend_repair {
            return column![
                self.backend_repair_view(repair, can_edit, can_request, reveal),
                progress
            ]
            .spacing(10)
            .into();
        }
        let shows_embedding = matches!(
            page,
            Page::Knowledge(..) | Page::Preferences(LocalSection::Runtime, _)
        );
        let inventory_status: Option<Element<'_, Message>> = if !shows_embedding {
            None
        } else if self.policy_embedding_load.is_some() {
            Some(text("Loading embedding models…").size(13).into())
        } else if self
            .policy_embeddings
            .as_ref()
            .is_some_and(|models| models.unavailable)
        {
            let refresh = button("Refresh embedding models")
                .style(button::secondary)
                .on_press_maybe(
                    self.can_refresh_embedding_inventory(editable)
                        .then_some(Message::Managed(managed::Message::Refresh)),
                );
            let refresh = if preferences_page {
                reveal.action(
                    Setting::LocalPreference(LocalPreference::RefreshEmbeddingModels),
                    refresh,
                )
            } else {
                refresh.into()
            };
            Some(
                row![
                    text("One or more policy-managed embedding models are unavailable.").size(13),
                    refresh,
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
            Page::Models {
                tokens,
                hints,
                setup_available,
            } => scrollable(self.models_view(
                state,
                can_edit,
                can_request,
                tokens,
                (hints, setup_available),
                reveal,
            ))
            .id(reveal.viewport.clone())
            .spacing(6)
            .height(Length::Fill)
            .into(),
            Page::Knowledge(hints, models) => responsive(move |size| {
                scrollable(self.knowledge_view(
                    state,
                    (can_edit, can_request),
                    (hints, models),
                    reveal,
                    size.height,
                ))
                .id(reveal.viewport.clone())
                .spacing(6)
                .height(Length::Fill)
                .into()
            })
            .into(),
            Page::Preferences(section, tokens) => match section {
                LocalSection::Roles => profile_roles(state, profiles, can_edit, reveal),
                LocalSection::Runtime => column![
                    runtime_preferences(
                        state.runtime_update,
                        can_edit,
                        self.runtime_downloads_allowed,
                        self.runtime_update_status.as_deref(),
                        reveal,
                    ),
                    embedding_preference(state, reveal, self.embedding_options(), can_edit),
                ]
                .spacing(14)
                .into(),
                LocalSection::HuggingFace if self.model_downloads_allowed => {
                    let (access, _) = self
                        .hf
                        .access_picker(can_request && self.runtime_removal.is_none(), tokens);
                    column![
                        reveal.action(
                            Setting::LocalPreference(LocalPreference::HubAccess),
                            preference_field("Hugging Face access", access),
                        ),
                        preference_hint("Choose a Vault token for gated models, or use anonymous access. The selection applies to this AI Manager session."),
                    ]
                    .spacing(8)
                    .into()
                }
                LocalSection::HuggingFace => {
                    text(crate::clipboard_policy::AI_MODEL_DOWNLOADS_DISABLED)
                        .size(13)
                        .into()
                }
            },
        };
        column![page, inventory_status, progress].spacing(10).into()
    }

    fn models_view<'a>(
        &'a self,
        state: &'a State,
        can_edit: bool,
        can_request: bool,
        hub_tokens: Vec<CredentialChoice>,
        (hints, setup_available): (&'a Hints, bool),
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        use super::search::{Control::*, Setting};
        let action = |control, button| reveal.action(Setting::Control(control), button);
        let endpoint = Setting::Control(LocalEndpoint);
        let models = pick_list(self.models.as_slice(), self.model.as_ref(), Message::Model)
            .placeholder("Select an installed model")
            .width(Length::Fill)
            .dismiss_on_escape();
        let local_service = column![
            text("Local model service").size(17),
            text("Discover models from an explicitly configured service already running on this computer.").size(13),
            reveal.wrap(endpoint, field("Local endpoint", reveal.input(endpoint, can_edit,
                text_input("Numeric loopback address and port", &state.endpoint).on_input(Message::Endpoint)))),
            row![
                action(RefreshModels, button("Refresh").on_press_maybe(can_request.then_some(Message::Discover))),
                reveal.wrap(Setting::Control(InstalledModel), models),
                action(TestModel, button("Test model").on_press_maybe(
                    (can_request && self.model.is_some()).then_some(Message::TestModel)
                )),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);
        let show_offline = self.offline_runtime_open;
        let installed = self.managed.runtime_summary().0.is_some();
        let (runtime_action, runtime_label) = if installed {
            (UpdateStableRuntime, "Update runtime…")
        } else {
            (InstallStableRuntime, "Install runtime…")
        };
        let (package_action, package_label) = if show_offline {
            (HideSignedPackage, "Hide package import")
        } else {
            (OpenSignedPackage, "Import signed package…")
        };
        column![
            text("Install a runtime and register local models to use AI on this computer.").size(13),
            text("Local runtimes").size(17),
            self.runtime_table(),
            row![
                action(runtime_action, dialog_design::secondary_action(runtime_label)
                    .on_press_maybe((can_request && self.runtime_removal.is_none()).then_some(Message::OpenStableRuntimeReview)),
                ),
                action(RuntimeVersionsEntry, dialog_design::secondary_action("Other versions…")
                    .on_press_maybe((can_request && self.runtime_downloads_allowed && self.runtime_removal.is_none())
                        .then_some(Message::OpenRuntimeVersions)),
                ),
                action(package_action, dialog_design::secondary_action(package_label)
                    .on_press_maybe(can_edit.then_some(Message::ToggleOfflineRuntime)),
                ),
                action(RemoveRuntime, dialog_design::secondary_action("Remove installed runtime…")
                    .on_press_maybe(
                        (can_edit && installed && self.runtime_removal.is_none() && !self.mlx.runtime_removal_open())
                            .then_some(Message::ReviewRuntimeRemoval),
                    )),
            ]
            .spacing(8)
            .wrap(),
            show_offline.then(|| column![
                text("Offline llama.cpp runtime package").size(17),
                text("Enter the absolute paths to the stable runtime index, its detached signature and the package ZIP.").size(13),
                self.runtime_fields(can_edit, reveal),
                action(InstallRuntime, button("Install signed runtime").on_press_maybe(
                    (can_request && self.runtime_removal.is_none() && self.runtime_paths_ready())
                        .then_some(Message::ImportRuntime),
                )),
            ].spacing(8)),
            (!self.runtime_downloads_allowed)
                .then(|| text(crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED).size(13)),
            self.runtime_removal.as_ref().map(|_| column![
                text("Remove the installed llama.cpp runtime?").size(17),
                text("This removes the installed runtime packages. Your GGUF models, AI profiles and runtime trust settings stay available.")
                    .wrapping(text::Wrapping::WordOrGlyph),
                row![
                    action(CancelRuntimeRemoval, button("Cancel")
                        .style(button::secondary)
                        .on_press_maybe(can_edit.then_some(Message::CancelRuntimeRemoval))),
                    action(ConfirmRuntimeRemoval, button("Remove runtime")
                        .style(button::danger)
                        .on_press_maybe(can_edit.then_some(Message::ConfirmRuntimeRemoval))),
                ]
                .spacing(8),
            ]
            .spacing(8)),
            self.managed_models_view(
                can_edit, can_request, state.default_managed_model.as_deref(),
                setup_available, reveal,
            ),
            self.mlx.view(
                can_edit,
                can_request,
                self.runtime_removal.is_none(),
                self.runtime_downloads_allowed,
                reveal,
            ),
            self.java_models_view(hints, can_request, reveal),
            self.online.view(
                can_request && self.runtime_downloads_allowed && self.runtime_removal.is_none(),
                can_edit,
                None,
                reveal,
            ),
            self.hf.view(
                can_request && self.runtime_removal.is_none(),
                self.model_downloads_allowed,
                hub_tokens,
                reveal,
            ),
            local_service,
        ]
        .spacing(10)
        .width(Length::Fill)
        .into()
    }

    fn runtime_table(&self) -> Element<'_, Message> {
        const WIDTHS: [u16; 4] = [2, 2, 2, 3];
        use knowledge_inventory::cells;
        let (version, backend) = self.managed.runtime_summary();
        let backend_name = backend.map_or_else(|| "—".to_owned(), |backend| backend.to_string());
        let state = if version.is_some() {
            "Installed"
        } else {
            "Not installed"
        };
        column![
            container(cells(
                ["Runtime", "Version", "Backend", "State"].map(str::to_owned),
                WIDTHS,
                true,
            ))
            .padding([5, 8])
            .style(crate::workspace::dialog_design::header),
            container(cells(
                [
                    "llama.cpp".into(),
                    version.unwrap_or("—").into(),
                    backend_name,
                    state.into()
                ],
                WIDTHS,
                false
            ))
            .padding([5, 8])
            .style(dialog_design::sidebar),
            self.mlx
                .runtime_summary()
                .map(|(version, state)| container(cells(
                    [
                        "Apple MLX".into(),
                        version.into(),
                        "Metal".into(),
                        state.into(),
                    ],
                    WIDTHS,
                    false
                ))
                .padding([5, 8])),
            iced::widget::rule::horizontal(1),
        ]
        .into()
    }

    fn java_models_view<'a>(
        &'a self,
        hints: &'a Hints,
        can_request: bool,
        reveal: &Reveal,
    ) -> Option<Element<'a, Message>> {
        if hints.is_empty() {
            return None;
        }
        let mut view = column![
            text("Imported Java Model Settings").size(17),
            text("Register and select a local model above to review these settings before saving.")
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph),
        ]
        .spacing(8);
        let can_request = can_request && !self.runtime_removal_review_open();
        for (index, hint) in hints.iter().enumerate() {
            let target = match hint.metadata {
                Metadata::Gguf { purpose, .. } => self
                    .managed
                    .java_target(purpose)
                    .map(|model| model.name.as_str()),
                Metadata::Mlx { .. } => self.mlx.java_target(),
            };
            view = view.push(
                reveal.wrap(
                    super::search::Setting::ImportedJavaModel(hint.search_key(index)),
                    column![
                        text(hint.label()).wrapping(text::Wrapping::WordOrGlyph),
                        text(hint.details())
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                        target.map(|target| button(
                            text(format!("Review for {target}…"))
                                .wrapping(text::Wrapping::WordOrGlyph)
                        )
                        .on_press_maybe(
                            can_request
                                .then(|| Message::ReviewJavaModel(hint.kind(), hint.id.clone()))
                        )),
                    ]
                    .spacing(4),
                ),
            );
        }
        Some(view.into())
    }

    fn backend_repair_view<'a>(
        &'a self,
        repair: &'a BackendRepair,
        can_edit: bool,
        can_request: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        use super::search::{Control, Setting};

        let body: Element<'_, Message> = if repair.installed {
            text("The package was installed. Return to the model to Start or Test.").into()
        } else {
            column![
                text("Choose the stable signed runtime index, its detached signature and the matching package ZIP using absolute paths.")
                    .wrapping(text::Wrapping::WordOrGlyph),
                self.runtime_fields(can_request, reveal),
                self.online.view(
                    can_request && self.runtime_downloads_allowed,
                    can_edit,
                    Some(repair.required),
                    reveal,
                ),
                (!self.runtime_downloads_allowed)
                    .then(|| text(crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED).size(13)),
            ]
            .spacing(10)
            .into()
        };
        let actions = row![
            reveal.action(
                Setting::Control(Control::CloseRuntimeRepair),
                button(if repair.installed {
                    "Return to model"
                } else {
                    "Cancel repair"
                })
                .style(button::secondary)
                .on_press_maybe(can_edit.then_some(Message::CloseBackendRepair))
            ),
            (!repair.installed).then(|| reveal.action(
                Setting::Control(Control::InstallRuntime),
                button("Install matching signed runtime").on_press_maybe(
                    (can_request && self.runtime_paths_ready()).then_some(Message::ImportRuntime),
                )
            )),
        ]
        .spacing(8);
        column![
            text("Install matching runtime").size(20),
            text(format!("Model: {}", repair.model_name)).wrapping(text::Wrapping::WordOrGlyph),
            text(format!(
                "Required signed package backend · {}",
                repair.required
            )),
            actions,
            scrollable(body)
                .id(reveal.viewport.clone())
                .height(Length::Fill),
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    fn runtime_fields(&self, enabled: bool, reveal: &Reveal) -> Element<'_, Message> {
        use super::search::{Control, Setting};
        column(
            [
                (
                    Control::SignedIndex,
                    "Signed index",
                    RuntimeFile::Index,
                    "runtime-index-v1.json",
                ),
                (
                    Control::IndexSignature,
                    "Index signature",
                    RuntimeFile::Signature,
                    "runtime-index-v1.sig",
                ),
                (
                    Control::RuntimePackage,
                    "Runtime package",
                    RuntimeFile::Archive,
                    "Package ZIP",
                ),
            ]
            .into_iter()
            .map(|(control, label, file, placeholder)| {
                let setting = Setting::Control(control);
                let input = text_input(placeholder, &self.runtime_paths()[file as usize])
                    .on_input_maybe(
                        enabled.then_some(move |path| Message::RuntimePath(file, path)),
                    );
                reveal.wrap(setting, field(label, reveal.input(setting, enabled, input)))
            }),
        )
        .spacing(8)
        .into()
    }

    fn knowledge_view<'a>(
        &'a self,
        state: &'a State,
        (can_review, can_request): (bool, bool),
        (hints, models): (&'a java_knowledge::Hints, &'a Hints),
        reveal: &Reveal,
        viewport_height: f32,
    ) -> Element<'a, Message> {
        use super::search::{InventoryAction, KnowledgeSetting, Setting};
        use KnowledgeSetting::*;
        let store_name = Setting::Knowledge(Name);
        let importing_store = self.java_store_origin.is_some();
        let selected = self
            .selected
            .and_then(|index| state.stores.get(index).map(|store| (index, store)));
        let can_configure = can_review && selected.is_some();
        let backend_valid =
            selected.is_some_and(|(_, store)| knowledge_backend::validate(store).is_ok());
        let can_index = selected.is_some_and(|(_, store)| {
            let (_, model, managed) = state.embedding_fields(store);
            can_request
                && backend_valid
                && self.folder_review.is_none()
                && self.consent_current()
                && store.has_enabled_sources()
                && (managed.is_some() || !model.trim().is_empty())
        });
        let create = row![
            importing_store.then(|| container(
                reveal.wrap(
                    store_name,
                    reveal.input(
                        store_name,
                        can_review,
                        text_input("New store name", &self.store_name)
                            .size(13)
                            .on_input_maybe(can_review.then_some(Message::StoreName))
                            .on_submit_maybe(can_review.then_some(Message::CreateStore))
                    )
                )
            )
            .width(300)),
            reveal.action(
                Setting::Knowledge(Create),
                dialog_design::secondary_action("Create knowledge store").on_press_maybe(
                    can_review.then_some(if importing_store {
                        Message::CreateStore
                    } else {
                        Message::KnowledgeSettings(knowledge_settings::Action::Open {
                            create: true,
                        })
                    })
                )
            ),
            self.java_store_origin.as_ref().map(|_| {
                dialog_design::secondary_action("Cancel Java store review")
                    .on_press(Message::CancelJavaKnowledgeStore)
            }),
            reveal.inventory_action(
                InventoryAction::ConfigureStore,
                dialog_design::secondary_action("Configure").on_press_maybe(
                    can_configure.then_some(Message::KnowledgeSettings(
                        knowledge_settings::Action::Open { create: false }
                    ))
                ),
            ),
            reveal.action(
                Setting::Knowledge(ReviewStoreRemoval),
                dialog_design::secondary_action("Remove store…").on_press_maybe(
                    (can_configure && !self.confirm_remove).then_some(Message::ConfirmRemove(true))
                )
            ),
            reveal.inventory_action(
                InventoryAction::OpenTestSearch,
                dialog_design::secondary_action("Test search").on_press_maybe(
                    can_configure.then_some(Message::RevealKnowledgeSetting(TestQuery))
                )
            ),
            reveal.inventory_action(
                InventoryAction::Refresh,
                dialog_design::secondary_action("Refresh")
                    .on_press_maybe(can_configure.then_some(Message::RefreshKnowledgeInventory)),
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let removal = (selected.is_some() && self.confirm_remove).then(|| {
            row![
                text("Remove this store from the profile? The source files stay on disk.").size(13),
                reveal.action(
                    Setting::Knowledge(ConfirmStoreRemoval),
                    button("Remove store")
                        .style(button::danger)
                        .on_press(Message::RemoveStore),
                ),
                reveal.action(
                    Setting::Knowledge(CancelStoreRemoval),
                    dialog_design::secondary_action("Cancel")
                        .on_press(Message::ConfirmRemove(false)),
                ),
            ]
            .spacing(8)
            .wrap()
        });
        let source_path = row![
            container(
                reveal.knowledge_input(
                    SourcePath,
                    can_configure,
                    text_input(
                        "Absolute path to a UTF-8 file, PDF, or folder",
                        &self.source
                    )
                    .size(13)
                    .on_input_maybe(can_configure.then_some(Message::Source))
                )
            )
            .width(445),
            reveal.action(
                Setting::Knowledge(AddFile),
                dialog_design::secondary_action("Add file").on_press_maybe(
                    (can_configure && backend_valid && !self.source.trim().is_empty())
                        .then_some(Message::AddSource)
                )
            ),
            reveal.action(
                Setting::Knowledge(ReviewFolder),
                dialog_design::secondary_action("Review folder…").on_press_maybe(
                    (can_configure && backend_valid && !self.source.trim().is_empty())
                        .then_some(Message::ReviewFolder)
                )
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let enter_path = self.manual_source;
        let mut source_actions = row![
            reveal.inventory_action(
                InventoryAction::AddFiles,
                dialog_design::secondary_action("Add files…").on_press_maybe(
                    (can_configure && backend_valid)
                        .then_some(Message::PickNativeKnowledgeSources(SourceKind::File)),
                )
            ),
            reveal.inventory_action(
                InventoryAction::AddFolder,
                dialog_design::secondary_action("Add folder…").on_press_maybe(
                    (can_configure && backend_valid)
                        .then_some(Message::PickNativeKnowledgeSources(SourceKind::Folder)),
                )
            ),
            reveal.action(
                Setting::Knowledge(Update),
                dialog_design::secondary_action("Update now…").on_press_maybe(selected.and_then(
                    |(index, store)| {
                        (can_review
                            && backend_valid
                            && self.folder_review.is_none()
                            && store.has_sources_for_update(&self.selected_sources))
                        .then_some(Message::ReviewSourceUpdates(index))
                    }
                ))
            ),
            reveal.inventory_action(
                InventoryAction::CancelIndexing,
                dialog_design::secondary_action("Cancel").on_press_maybe(
                    (self.source_request()
                        && self
                            .source_work
                            .as_ref()
                            .is_some_and(|work| work.phase != SourcePhase::Cancelling))
                    .then_some(Message::Cancel),
                )
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        source_actions = source_actions.extend(self.source_actions(selected, can_review, reveal));
        let mut maintenance = row![
            button(if enter_path {
                "Hide path field"
            } else {
                "Enter path…"
            })
            .style(button::text)
            .on_press_maybe(can_configure.then_some(Message::ToggleManualSource)),
            reveal.action(
                Setting::Knowledge(Rebuild),
                dialog_design::secondary_action("Rebuild index")
                    .on_press_maybe(can_index.then_some(Message::Index { rebuild: true }))
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut details = column![].spacing(10).width(Length::Fill);
        if let Some((store_index, store)) = selected {
            if self.approved_source_update(store).is_some() {
                maintenance = maintenance.push(
                    dialog_design::secondary_action("Index reviewed sources")
                        .on_press_maybe(can_index.then_some(Message::IndexReviewed)),
                );
            }
            let (endpoint, model, managed) = state.embedding_fields(store);
            let qdrant_destination = store
                .qdrant
                .as_ref()
                .filter(|_| backend_valid)
                .map(|settings| settings.destination());
            details = details
                .push(text("Update uses the selected sources, or all enabled sources when the selection is empty. Rebuild replaces all enabled vectors.").size(13))
                .push(self.folder_options.view(can_review, reveal).map(Message::FolderOptions))
                .push(self.folder_review.as_deref().map(|review| folder_review_view(review, self.folder_review_page, reveal)))
                .push(qdrant_destination.as_ref().map(|destination| {
                    text(format!("Source and query embeddings and opaque identifiers will be sent to {destination}."))
                        .wrapping(text::Wrapping::WordOrGlyph).size(13)
                }))
                .push(reveal.action(Setting::Knowledge(EmbeddingConsent), checkbox(self.consent_current())
                    .label(match (managed.is_some(), store.qdrant.is_some()) {
                        (true, false) => "Allow this managed model to process these source excerpts and searches",
                        (false, false) => "Allow these source excerpts and searches to be sent to this local service",
                        (true, true) => "Allow this managed model to embed source excerpts and searches and send vectors to Qdrant",
                        (false, true) => "Allow this local service to embed source excerpts and searches and send vectors to Qdrant",
                    })
                    .on_toggle_maybe(backend_valid.then_some(Message::Consent))))
                .push(managed.is_none().then(|| text("Use a local embedding model you trust. The selected service can forward requests.").size(13)))
                .push(store.has_automatic_sources().then(|| {
                    let destination = managed.map_or_else(
                        || format!("{endpoint} · {model}"),
                        |id| format!("{MANAGED_EMBEDDING_DESTINATION} · {id}"),
                    );
                    let approved = self.automatic_destination_approved(state, store);
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
                        (!approved || store.qdrant.is_some() && !store.automatic_qdrant_expanded).then(|| reveal.action(
                            Setting::Knowledge(if approved { AutomaticQdrantCapacity } else { AutomaticEmbeddingApproval }),
                            button(if approved {
                                "Review automatic Qdrant capacity…"
                            } else if store.qdrant.is_some() {
                                "Approve automatic embedding and Qdrant destinations"
                            } else {
                                "Approve automatic embedding destination"
                            })
                            .on_press_maybe((can_request && model_available)
                                .then_some(Message::ApproveAutomaticEmbedding(store_index))))),
                        approved.then(|| text(self.automatic_status.get(&store.name)
                            .map_or("Automatic sync checks each store in turn and pauses during edits.", |status| status.summary.as_str()))
                            .wrapping(text::Wrapping::WordOrGlyph).size(13)),
                    ].spacing(6)
                }))
                .push(text("Terminal AI chat and code analysis ask separately before embedding a query and show retrieved excerpts before sending the request.").size(13))
                .push(text("Saved indexes can be searched again after reopening. Updating changed settings takes effect after Save.").size(13))
                .push(row![
                    reveal.knowledge_input(TestQuery, can_review, text_input("Test search", &self.query).on_input(Message::Query)),
                    reveal.action(Setting::Knowledge(TestSearch), button("Search").on_press_maybe(
                        (can_index && !self.query.trim().is_empty()).then_some(Message::Search),
                    ))
                ].spacing(8));
            for hit in &self.hits {
                details = details.push(text(hit.as_str()).wrapping(text::Wrapping::WordOrGlyph));
            }
        }
        let (stores_height, sources_height) = knowledge_inventory::body_heights(viewport_height);
        column![
            text("Build and search knowledge stores from reviewed files and folders.").size(13),
            text("Knowledge stores").size(15),
            self.store_inventory(state, stores_height),
            create,
            removal,
            text("Sources").size(15),
            self.source_inventory(state, selected, sources_height),
            source_actions.wrap(),
            maintenance.wrap(),
            enter_path.then_some(source_path),
            details,
            self.java_knowledge_view(hints, models, selected.map(|(_, store)| store), can_request),
        ]
        .spacing(8)
        .width(Length::Fill)
        .into()
    }

    fn java_knowledge_view<'a>(
        &'a self,
        hints: &'a java_knowledge::Hints,
        models: &Hints,
        selected: Option<&Store>,
        can_review: bool,
    ) -> Option<Element<'a, Message>> {
        if hints.is_empty() {
            return None;
        }
        let native_origin = selected.and_then(|store| store.java_origin.as_ref());
        let review_origin = self.java_store_origin.as_ref().or(native_origin);
        let mut rows = column![text("Imported Java Knowledge Stores").size(17)].spacing(10);
        for store in hints.iter() {
            let backend = match store.backend {
                java_knowledge::Backend::LocalHnsw => "Local HNSW",
                java_knowledge::Backend::Qdrant => "Qdrant",
            };
            let selected = review_origin
                .filter(|origin| origin.id == store.id)
                .is_some_and(|origin| store.origin().as_ref().is_ok_and(|value| value == origin));
            let mut details = column![
                text(format!(
                    "{} · {backend} · {} sources",
                    store.name,
                    store.sources.len()
                ))
                .wrapping(text::Wrapping::WordOrGlyph),
                button("Review store…").on_press_maybe(
                    can_review.then(|| Message::ReviewJavaKnowledgeStore(store.id.clone()))
                ),
            ]
            .spacing(5);
            if let Some(model_id) = &store.embedding_model_id {
                let label = models
                    .get(java_models::Kind::Gguf, model_id)
                    .filter(|hint| {
                        matches!(
                            hint.metadata,
                            Metadata::Gguf {
                                purpose: managed_models::Purpose::Embedding,
                                ..
                            }
                        )
                    })
                    .map_or_else(
                        || model_id.clone(),
                        |hint| format!("{} ({model_id})", hint.name),
                    );
                details = details.push(
                    text(format!(
                        "Java embedding model · {label} · {} dimensions",
                        store.embedding_dimensions
                    ))
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph),
                );
            }
            if store.backend == java_knowledge::Backend::Qdrant {
                let destination = [store.qdrant_origin.as_deref(), store.collection.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                if !destination.is_empty() {
                    details = details.push(
                        text(format!("Java Qdrant destination · {destination}"))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    );
                }
                details = details.push(
                    text(
                        "Review a Qdrant destination and use a new collection for the Rust index.",
                    )
                    .size(13),
                );
            }
            if selected {
                let roles = [
                    (store.text_enabled, "Text"),
                    (store.coding_enabled, "Coding"),
                    (store.autonomous_enabled, "Autonomous"),
                ]
                .into_iter()
                .filter_map(|(enabled, label)| enabled.then_some(label))
                .collect::<Vec<_>>();
                if !roles.is_empty() {
                    details =
                        details.push(text(format!("Java uses · {}", roles.join(", "))).size(13));
                }
                for source in &store.sources {
                    let kind = match source.kind {
                        java_knowledge::Kind::File => "File",
                        java_knowledge::Kind::Directory => "Folder",
                    };
                    let sync = match source.sync {
                        java_knowledge::Sync::Manual => "Manual",
                        java_knowledge::Sync::Automatic => "Automatic",
                    };
                    let label = source
                        .basename
                        .as_deref()
                        .filter(|name| *name != source.name)
                        .map_or_else(
                            || source.name.clone(),
                            |name| format!("{} ({name})", source.name),
                        );
                    details = details.push(
                        column![
                            text(format!(
                                "{label} · {kind} · {sync} · {} · {} bytes per file",
                                if source.enabled {
                                    "Enabled"
                                } else {
                                    "Disabled"
                                },
                                source.max_file_bytes
                            ))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                            (!source.includes.is_empty()).then(|| text(format!(
                                "Include · {}",
                                source.includes.join(", ")
                            ))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph)),
                            (!source.excludes.is_empty()).then(|| text(format!(
                                "Exclude · {}",
                                source.excludes.join(", ")
                            ))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph)),
                            (review_origin == native_origin && native_origin.is_some()).then(
                                || button("Review source…").on_press_maybe(can_review.then(|| {
                                    Message::ReviewJavaKnowledgeSource(
                                        store.id.clone(),
                                        source.id.clone(),
                                    )
                                }))
                            ),
                        ]
                        .spacing(4),
                    );
                }
            }
            rows = rows.push(details);
        }
        Some(rows.into())
    }
}

const SOURCE_REVIEW_PAGE_SIZE: usize = 128;

fn folder_review_pages(review: &folder::Review) -> usize {
    review.rows.len().div_ceil(SOURCE_REVIEW_PAGE_SIZE)
}

fn can_approve_folder_review(review: &folder::Review) -> bool {
    review.existing || review.folder.len() > 0
}

fn folder_review_view<'a>(
    review: &'a folder::Review,
    page: usize,
    reveal: &Reveal,
) -> Element<'a, Message> {
    use super::search::{KnowledgeSetting::*, Setting};
    let action = |setting, button| reveal.action(Setting::Knowledge(setting), button);
    let pages = folder_review_pages(review);
    let rows = review
        .rows
        .iter()
        .skip(page * SOURCE_REVIEW_PAGE_SIZE)
        .take(SOURCE_REVIEW_PAGE_SIZE)
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
        text(file_chooser::display_path(&review.folder.root).into_owned())
            .wrapping(text::Wrapping::WordOrGlyph),
        review
            .folder
            .options
            .max_file_bytes
            .map(|limit| text(format!(
                "Configured size limit · {} bytes per file",
                limit.bytes()
            ))
            .size(13)),
        reveal.wrap(Setting::Knowledge(FolderReviewSummary), text(summary).wrapping(text::Wrapping::WordOrGlyph)),
        (pages > 1).then(|| row![
            action(FolderReviewPrevious, button("Previous page").on_press_maybe((page > 0).then(|| Message::FolderReviewPage(page - 1)))),
            text(format!("{} of {pages}", page + 1)),
            action(FolderReviewNext, button("Next page").on_press_maybe((page + 1 < pages).then_some(Message::FolderReviewPage(page + 1)))),
        ].spacing(8).align_y(Alignment::Center)),
        (!review.rows.is_empty()).then(|| scrollable(rows).height(150)),
        text(
            "Approval selects the files and contents on all pages. Indexing requires separate consent."
        )
        .size(13),
        row![
            action(FolderReviewApprove, button("Approve folder files")
                .on_press_maybe(can_approve_folder_review(review).then_some(Message::ApproveFolder))),
            action(FolderReviewDiscard, button("Discard review").on_press(Message::DiscardFolderReview)),
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
    completed: Option<(Store, outcomes::Rows)>,
}

struct RetiredIndex {
    scope: Scope,
    vault_key: Result<Option<Arc<VaultKey>>, String>,
}

pub(super) struct SourceCompletions(Vec<(Store, outcomes::Rows)>);

impl SourceCompletions {
    pub(super) fn save(self, root: &Path, state: &State) -> io::Result<()> {
        for (store, rows) in self.0 {
            outcomes::merge(root, state, &store, &rows)?;
        }
        Ok(())
    }
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
    pub(super) fn completed_outcomes(&self, ai_enabled: bool) -> SourceCompletions {
        SourceCompletions(
            self.staged
                .iter()
                .filter(|staged| ai_enabled || staged.scope.qdrant.is_none())
                .filter_map(|staged| staged.completed.clone())
                .collect(),
        )
    }

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

pub(super) fn profile_role_picker(
    state: &State,
    profiles: &Catalog,
    role: Role,
    editable: bool,
    reveal: &Reveal,
    setting: Setting,
) -> Element<'static, Message> {
    let default_label = if setting == Setting::SecurityProfile {
        "Use default profile"
    } else {
        "Use default"
    };
    let options: Vec<_> = std::iter::once(Choice(None, default_label.into()))
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
        .chain(
            profiles
                .role_specific_choices(role)
                .filter_map(|(id, name)| Some(Choice(Some(profiles.profile_reference(id)?), name))),
        )
        .collect();
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
    if editable {
        pick_list(options, selected, move |choice| {
            Message::Role(role, choice.0)
        })
        .placeholder("Selected profile unavailable")
        .width(Length::Fill)
        .text_size(13)
        .keyboard_navigation(reveal.control_id(setting))
        .dismiss_on_escape()
    } else {
        text(selected.map_or_else(|| "Selected profile unavailable".into(), |choice| choice.1))
            .size(13)
            .into()
    }
}

fn profile_roles(
    state: &State,
    profiles: &Catalog,
    editable: bool,
    reveal: &Reveal,
) -> Element<'static, Message> {
    let mut content = column![text("Choose which profile handles each AI role.").size(13)]
        .spacing(8)
        .width(Length::Fill);
    for role in Role::ALL {
        let setting = Setting::Role(role);
        let picker = profile_role_picker(state, profiles, role, editable, reveal, setting);
        content = content.push(reveal.wrap(setting, preference_field(role.label(), picker)));
    }
    content.into()
}

fn embedding_preference(
    state: &State,
    reveal: &Reveal,
    models: Vec<EmbeddingChoice>,
    editable: bool,
) -> Element<'static, Message> {
    use super::search::LocalPreference;
    preference_field(
        Setting::Embedding.title(),
        column![
            reveal.wrap(
                Setting::LocalPreference(LocalPreference::EmbeddingSource),
                embedding_picker(
                    models,
                    state.managed_embedding.as_deref(),
                    EmbeddingScope::Default,
                    editable
                ),
            ),
            state.uses_manual_embedding().then(|| {
                let setting = Setting::Embedding;
                reveal.wrap(
                    setting,
                    reveal.input(
                        setting,
                        editable,
                        text_input("Installed embedding model", &state.embedding)
                            .on_input_maybe(editable.then_some(Message::Embedding)),
                    ),
                )
            }),
        ]
        .spacing(8),
    )
}

fn runtime_preferences(
    settings: runtime_policy::Settings,
    editable: bool,
    runtime_downloads_allowed: bool,
    status: Option<&str>,
    reveal: &Reveal,
) -> Element<'static, Message> {
    use super::search::LocalPreference;

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
        reveal.wrap(
            Setting::LocalPreference(LocalPreference::RuntimeUpdatePolicy),
            preference_field("Runtime updates", mode),
        ),
        reveal.wrap(
            Setting::LocalPreference(LocalPreference::RuntimeBackend),
            preference_field("Preferred backend", backend),
        ),
        preference_hint(if runtime_downloads_allowed {
            "Notify and Automatic Stable check for signed releases on startup and daily. Automatic Stable installs compatible packages when managed models are idle."
        } else {
            crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED
        }),
        status.map(|status| preference_hint(status.to_owned())),
    ]
    .spacing(8)
    .into()
}

fn preference_field<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![
        text(label).size(13).width(166),
        container(control).width(320)
    ]
    .align_y(Alignment::Center)
    .into()
}

fn preference_hint<'a>(hint: impl Into<String>) -> Element<'a, Message> {
    row![
        iced::widget::space::horizontal().width(166),
        text(hint.into())
            .size(12)
            .wrapping(text::Wrapping::WordOrGlyph)
    ]
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

const FORM_LABEL_WIDTH: f32 = 175.0;
const FORM_COLUMN_GAP: f32 = 8.0;

fn field<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).width(FORM_LABEL_WIDTH), control.into()]
        .align_y(Alignment::Center)
        .spacing(FORM_COLUMN_GAP)
        .into()
}

fn limited(value: &str, bytes: usize) -> &str {
    &value[..value.floor_char_boundary(bytes.min(value.len()))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_search_routes_to_current_profile_and_destination_controls() {
        use crate::ai_manager::search::{KnowledgeSetting, Setting, Target};
        use crate::ai_manager::{Dialog as Manager, Message as ManagerMessage};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("notes.txt");
        std::fs::write(&source, "Profile and destination fixture.").unwrap();
        let mut store = Store::new("Profile search").unwrap();
        store.add_source(source.clone()).unwrap();
        assert!(store.set_file_sync(source, SyncMode::Automatic));
        store.qdrant = Some(Default::default());
        let mut config = Configuration {
            profiles: profile_catalog(&[7]),
            ..Configuration::default()
        };
        config.local.stores.push(store);
        config
            .local
            .set_profile(Role::Text, Some(ProfileRef::Numeric(7)));
        config
            .local
            .set_profile(Role::Coding, Some(ProfileRef::Numeric(99)));
        let mut manager = Manager::new(Arc::new(config), root, Vec::new(), true, None);
        let reveal = |manager: &mut Manager, query: &str, setting| {
            let _ = manager.update(ManagerMessage::Search(query.into()));
            assert_eq!(
                manager.search_results().next(),
                Some(Target::Setting(Setting::Knowledge(setting)))
            );
            let _ = manager.update(ManagerMessage::SearchChoose);
            assert_eq!(manager.revealed.setting, Some(Setting::Knowledge(setting)));
        };
        let baseline = manager.local_state.clone();
        reveal(
            &mut manager,
            "offer this store for terminal AI chat",
            KnowledgeSetting::TerminalChatProfile,
        );
        let _ = manager.update(ManagerMessage::Search(
            "offer this store for code analysis".into(),
        ));
        assert_eq!(manager.search_results().next(), None);
        reveal(
            &mut manager,
            "scheduled AI requests",
            KnowledgeSetting::AgentKnowledge,
        );
        reveal(
            &mut manager,
            "allow embedding source excerpts",
            KnowledgeSetting::EmbeddingConsent,
        );
        reveal(
            &mut manager,
            "approve automatic embedding destination",
            KnowledgeSetting::AutomaticEmbeddingApproval,
        );
        assert_eq!(manager.local_state, baseline);
        assert!(!manager.local.consent_current());
        assert!(!manager.local.busy());

        let embedding = manager
            .local_state
            .automatic_embedding(&manager.local_state.stores[0]);
        let store = &mut manager.local_state.stores[0];
        store.automatic_embedding = Some(embedding);
        store.automatic_qdrant = store.qdrant.clone();
        let approved = manager.local_state.clone();
        reveal(
            &mut manager,
            "review automatic Qdrant capacity",
            KnowledgeSetting::AutomaticQdrantCapacity,
        );
        assert_eq!(manager.local_state, approved);
        manager.local_state.stores[0].automatic_qdrant_expanded = true;
        let _ = manager.update(ManagerMessage::Search(
            "review automatic Qdrant capacity".into(),
        ));
        assert_eq!(manager.search_results().next(), None);
    }

    #[test]
    fn knowledge_inventory_search_keeps_the_source_selection_and_leaves_actions_to_the_user() {
        use crate::ai_manager::search::{InventoryAction, KnowledgeSetting, Setting, Target};
        use crate::ai_manager::{
            Configuration, Dialog as Manager, Message as ManagerMessage, Page as ManagerPage,
        };

        let profile = crate::snippets::tests::Profile::new();
        let file = profile.0.canonicalize().unwrap().join("notes.txt");
        std::fs::write(&file, "Search fixture").unwrap();
        let mut store = Store::new("Fixture").unwrap();
        store.add_source(file.clone()).unwrap();
        let saved = Configuration {
            local: State {
                stores: vec![store],
                ..State::default()
            },
            ..Configuration::default()
        };
        let mut dialog = Manager::new(Arc::new(saved), profile.0.clone(), Vec::new(), true, None);
        let refresh = Target::Setting(Setting::Knowledge(KnowledgeSetting::Inventory(
            InventoryAction::Refresh,
        )));
        let _ = dialog.open_page(refresh);
        assert_eq!(dialog.page, ManagerPage::Knowledge);
        assert!(!dialog.local.busy());
        let _ = dialog.update(ManagerMessage::Local(Message::SelectStore(0)));
        let _ = dialog.update(ManagerMessage::Local(Message::SelectKnowledgeSource(
            0,
            SourceKind::File,
            file,
        )));
        let selection = dialog.local.selected_sources.clone();
        assert_eq!(selection.len(), 1);
        let before = serde_json::to_value(&dialog.local_state).unwrap();
        for (query, action) in [
            ("refresh knowledge inventory", InventoryAction::Refresh),
            ("sync mode", InventoryAction::SyncMode),
            ("remove sources", InventoryAction::RemoveSources),
        ] {
            let target = Setting::Knowledge(KnowledgeSetting::Inventory(action));
            let _ = dialog.update(ManagerMessage::Search(query.into()));
            let _ = dialog.update(ManagerMessage::SearchChoose);
            assert_eq!(dialog.revealed.setting, Some(target));
            assert_eq!(dialog.local.selected_sources, selection);
            assert!(!dialog.knowledge_source_sync_open());
            assert!(!dialog.knowledge_source_removal_open());
            assert!(dialog.knowledge_settings_title().is_none());
            assert!(!dialog.local.knowledge_source_picker_open());
            assert_eq!(serde_json::to_value(&dialog.local_state).unwrap(), before);
        }

        let _ = dialog.update(ManagerMessage::Local(Message::OpenSourceSync(0)));
        assert!(dialog.knowledge_source_sync_open());
        assert!(!dialog.search_target_available(refresh));
        let _ = dialog.open_page(refresh);
        assert!(dialog.knowledge_source_sync_open());
    }

    #[test]
    fn knowledge_search_reveals_the_active_file_limit_and_store_removal() {
        use crate::ai_manager::search::{KnowledgeSetting, Setting, Target};
        use crate::ai_manager::{Dialog as Manager, Message as ManagerMessage};
        use crate::settings_search::test_input;
        use iced::keyboard::Key;
        use iced::{Font, Renderer, Size};
        use iced_runtime::{UserInterface, user_interface::Cache};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("notes.txt");
        std::fs::write(&source, "A reviewed source file.").unwrap();
        let mut store = Store::new("Searchable store").unwrap();
        store.add_source(source.clone()).unwrap();
        let baseline = store.clone();
        let mut config = Configuration::default();
        config.local.stores.push(store);
        let mut manager = Manager::new(Arc::new(config), root, Vec::new(), true, None);
        let _ = manager.update(ManagerMessage::Reveal(Target::Setting(Setting::Knowledge(
            KnowledgeSetting::ReviewStoreRemoval,
        ))));
        assert_eq!(manager.local.selected, Some(0));
        assert!(!manager.local.confirm_remove);
        let _ = manager.update(ManagerMessage::Local(Message::ConfigureFileSize(
            0,
            source.clone(),
        )));
        let _ = manager.update(ManagerMessage::Search("source file size limit".into()));
        let setting = Setting::Knowledge(KnowledgeSetting::FileSize);
        assert_eq!(
            manager.search_results().next(),
            Some(Target::Setting(setting))
        );
        let task = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(manager.revealed.setting, Some(setting));
        let typed = {
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                manager.view(Vec::new(), Vec::new()),
                Size::new(900.0, 480.0),
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(&mut interface, &renderer, task);
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Character("7".into()),
                Some("7"),
            )
        };
        assert!(
            matches!(typed.as_slice(), [ManagerMessage::Local(Message::FileSize(value))] if value.ends_with('7'))
        );
        for event in typed {
            let _ = manager.update(event);
        }
        let _ = manager.update(ManagerMessage::Search(
            "apply source file size limit".into(),
        ));
        let _ = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(
            manager.revealed.setting,
            Some(Setting::Knowledge(KnowledgeSetting::ApplyFileSize))
        );
        let edit = manager.local.file_size.as_ref().unwrap();
        assert_eq!(edit.path, source);
        assert!(edit.input.ends_with('7'));
        assert_eq!(
            manager.local_state.stores.as_slice(),
            std::slice::from_ref(&baseline)
        );
        let _ = manager.update(ManagerMessage::Local(Message::CancelFileSize));
        let _ = manager.update(ManagerMessage::Local(Message::ConfirmRemove(true)));
        let _ = manager.update(ManagerMessage::Search(
            "confirm removal of the knowledge store".into(),
        ));
        let _ = manager.update(ManagerMessage::SearchChoose);
        let removal = Setting::Knowledge(KnowledgeSetting::ConfirmStoreRemoval);
        assert_eq!(manager.revealed.setting, Some(removal));
        assert!(manager.local.confirm_remove);
        assert_eq!(manager.local_state.stores, [baseline]);
        let _ = manager.update(ManagerMessage::Local(Message::ConfirmRemove(false)));
        let stale = manager.update(ManagerMessage::Reveal(Target::Setting(removal)));
        assert!(iced_runtime::task::into_stream(stale).is_none());
    }

    #[test]
    fn knowledge_scan_cancellation_stays_bound_to_the_running_source_request() {
        use iced::futures::{StreamExt as _, executor::block_on};

        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("review.txt");
        std::fs::write(&path, "A fixture for the source scan.").unwrap();
        let mut store = Store::new("Cancellation fixture").unwrap();
        store.add_source(path.clone()).unwrap();
        let mut draft = State {
            stores: vec![store],
            ..State::default()
        };
        let saved = draft.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, true);
        let task = dialog.update(Message::ReviewSourceUpdates(0), &mut draft, &saved, true);
        let key = (SourceKind::File, path);
        let status = |dialog: &Dialog| dialog.source_work.as_ref().unwrap().status(&key);
        assert_eq!(status(&dialog), Some("Scanning…"));
        let _ = dialog.update(Message::Cancel, &mut draft, &saved, true);
        assert_eq!(status(&dialog), Some("Cancelling…"));
        assert!(dialog.source_request());
        let overlapping = dialog.update(Message::ReviewSourceUpdates(0), &mut draft, &saved, true);
        assert!(iced_runtime::task::into_stream(overlapping).is_none());
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(reply)) = block_on(stream.next()) else {
            panic!("source scan did not reply");
        };
        let _ = dialog.update(reply, &mut draft, &saved, true);
        assert_eq!(status(&dialog), Some("Cancelled"));
        assert!(!dialog.source_request());
        assert!(dialog.source_updates.is_none());
        assert_eq!(saved, draft);
    }

    #[test]
    fn knowledge_page_selects_its_initial_store_before_loading_managed_models() {
        use crate::ai_manager::{Dialog as Manager, Message as ManagerMessage, Page};

        let profile = crate::snippets::tests::Profile::new();
        let config = Configuration {
            local: State {
                stores: vec![Store::new("Source outcome fixture").unwrap()],
                ..State::default()
            },
            ..Configuration::default()
        };
        let mut manager = Manager::new(Arc::new(config), profile.0.clone(), Vec::new(), true, None);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let _task = manager.update(ManagerMessage::Page(Page::Knowledge));
            assert_eq!(manager.local.selected, Some(0));
            if manager.local.busy() {
                break;
            }
            assert_eq!(
                manager.local.status.as_deref(),
                Some("A managed local model operation is already running")
            );
            assert!(
                std::time::Instant::now() < deadline,
                "the global model operation did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            manager.local.status.as_deref(),
            Some("Reading managed models…")
        );
    }

    #[test]
    fn knowledge_source_outcome_reopens_after_a_failed_then_repaired_manual_update() {
        use iced::futures::{StreamExt as _, executor::block_on};
        use std::io::Write as _;

        let _exclusive = local_ai::test_support::exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let failing = root.join("first.txt");
        let unchanged = root.join("second.txt");
        std::fs::write(&failing, "First indexed source").unwrap();
        std::fs::write(&unchanged, "Second indexed source").unwrap();
        let (server, _) = local_ai::test_support::listener();
        let endpoint = server.local_addr().unwrap().to_string();
        let reply = std::thread::spawn(move || {
            for (inputs, embeddings) in [
                (
                    &["First indexed source", "Second indexed source"][..],
                    &[[1.0, 0.0], [0.0, 1.0]][..],
                ),
                (&["First repaired source"][..], &[[1.0, 1.0]][..]),
            ] {
                let mut socket = local_ai::test_support::accept(&server);
                let request: serde_json::Value = serde_json::from_slice(
                    &local_ai::test_support::wire_request(&mut socket, "POST /api/embed HTTP/1.1"),
                )
                .unwrap();
                assert_eq!(request["input"], serde_json::json!(inputs));
                let response = serde_json::json!({ "embeddings": embeddings });
                socket
                    .get_mut()
                    .write_all(
                        local_ai::test_support::fixture_http_response(
                            "200 OK",
                            "",
                            &response.to_string(),
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let mut store = Store::new("Source outcome fixture").unwrap();
        store.add_source(failing.clone()).unwrap();
        store.add_source(unchanged.clone()).unwrap();
        store.embedding = Some(Embedding {
            endpoint,
            model: "embedding-fixture".into(),
            managed_model: None,
        });
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
        let saved = Configuration::load_fixture_from(&root).unwrap().local;
        let mut state = saved.clone();
        let finish = |dialog: &mut Dialog, state: &mut State, task| {
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(event)) = block_on(stream.next()) else {
                panic!("expected a source inventory or index result");
            };
            let _ = dialog.update(event, state, &saved, true);
        };
        let mut dialog = Dialog::new(root.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut state, &saved, true);
        let _ = dialog.update(Message::Consent(true), &mut state, &saved, true);
        let task = dialog.update(Message::Index { rebuild: false }, &mut state, &saved, true);
        finish(&mut dialog, &mut state, task);
        let first = (SourceKind::File, failing.clone());
        let second = (SourceKind::File, unchanged);
        let first_index = dialog.source_results.restored(&first).1.unwrap();
        let second_index = dialog.source_results.restored(&second).1.unwrap();
        assert!(first_index.indexed_at > 0 && second_index.indexed_at > 0);
        std::fs::write(&failing, b"Invalid\0text").unwrap();
        let _ = dialog.update(
            Message::SelectKnowledgeSource(0, SourceKind::File, failing),
            &mut state,
            &saved,
            true,
        );
        let task = dialog.update(Message::Index { rebuild: false }, &mut state, &saved, true);
        finish(&mut dialog, &mut state, task);
        assert_eq!(
            dialog.source_work.as_ref().unwrap().status(&first),
            Some("Error")
        );
        drop(dialog);

        let mut reopened = Dialog::new(root.clone());
        let _ = reopened.update(Message::SelectStore(0), &mut state, &saved, true);
        let task = reopened.load_source_results(&state);
        finish(&mut reopened, &mut state, task);
        assert_eq!(
            reopened.source_results.restored(&first),
            (Some(SourceStatus::Error), Some(first_index))
        );
        assert_eq!(SourceStatus::Error.problems(), Some(1));
        assert_eq!(
            reopened.source_results.restored(&second),
            (
                Some(SourceStatus::Checked { problems: 0 }),
                Some(second_index)
            )
        );

        std::fs::write(&first.1, "First repaired source").unwrap();
        let _ = reopened.update(
            Message::SelectKnowledgeSource(0, first.0, first.1.clone()),
            &mut state,
            &saved,
            true,
        );
        let _ = reopened.update(Message::Consent(true), &mut state, &saved, true);
        let task = reopened.update(Message::Index { rebuild: false }, &mut state, &saved, true);
        finish(&mut reopened, &mut state, task);
        reply.join().unwrap();
        drop(reopened);

        let mut recovered = Dialog::new(root);
        let _ = recovered.update(Message::SelectStore(0), &mut state, &saved, true);
        let task = recovered.load_source_results(&state);
        finish(&mut recovered, &mut state, task);
        let (status, result) = recovered.source_results.restored(&first);
        assert_eq!(status, Some(SourceStatus::Checked { problems: 0 }));
        let result = result.unwrap();
        assert_eq!((result.files, result.chunks), (1, 1));
        assert!(result.indexed_at >= first_index.indexed_at);
        assert_eq!(
            recovered.source_results.restored(&second),
            (
                Some(SourceStatus::Checked { problems: 0 }),
                Some(second_index)
            )
        );
    }

    #[test]
    fn knowledge_source_selection_survives_row_removal_and_resets_on_store_change() {
        let saved = State::default();
        let mut draft = saved.clone();
        let first = PathBuf::from("earlier.txt");
        let chosen = PathBuf::from("chosen.txt");
        let mut store = Store::new("Fixtures").unwrap();
        store.sources = vec![first, chosen.clone()];
        draft.stores.push(store);
        draft.stores.push(Store::new("Other").unwrap());
        let mut dialog = Dialog::new(PathBuf::new());

        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, true);
        let _ = dialog.update(
            Message::SelectKnowledgeSource(0, SourceKind::File, chosen.clone()),
            &mut draft,
            &saved,
            true,
        );
        let _ = dialog.update(Message::OpenSourceSync(0), &mut draft, &saved, true);
        assert_eq!(
            dialog
                .source_sync_editor
                .as_ref()
                .map(|edit| &edit.source.1),
            Some(&chosen)
        );
        let _ = dialog.update(
            Message::SourceSyncMode(SyncMode::Automatic),
            &mut draft,
            &saved,
            true,
        );
        assert_eq!(draft.stores[0].file_sync(&chosen), SyncMode::Manual);
        draft.stores[0].remove_file(0);
        assert!(
            dialog
                .selected_sources
                .contains(&(SourceKind::File, chosen.clone()))
        );
        let _ = dialog.update(Message::SelectStore(1), &mut draft, &saved, true);
        assert!(dialog.source_sync_editor.is_none());
        let _ = dialog.update(
            Message::SelectKnowledgeSource(0, SourceKind::File, chosen),
            &mut draft,
            &saved,
            true,
        );
        assert!(dialog.selected_sources.is_empty());
        assert_eq!(draft.stores[0].sources.len(), 1);
    }

    #[test]
    fn knowledge_source_modifiers_and_removal_follow_the_reviewed_rows() {
        use iced::keyboard::Modifiers;

        let saved = State::default();
        let mut draft = saved.clone();
        let mut store = Store::new("Fixtures").unwrap();
        let files = ["a.txt", "b.txt", "c.txt", "d.txt"].map(PathBuf::from);
        store.sources = files.to_vec();
        draft.stores.push(store);
        let mut dialog = Dialog::new(PathBuf::new());
        let press =
            |path: &PathBuf| Message::SelectKnowledgeSource(0, SourceKind::File, path.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, true);
        let _ = dialog.update(press(&files[0]), &mut draft, &saved, true);
        #[cfg(target_os = "macos")]
        dialog.set_selection_modifiers(Modifiers::LOGO);
        #[cfg(not(target_os = "macos"))]
        dialog.set_selection_modifiers(Modifiers::CTRL);
        let _ = dialog.update(press(&files[2]), &mut draft, &saved, true);
        assert_eq!(
            dialog.selected_sources,
            BTreeSet::from([
                (SourceKind::File, files[0].clone()),
                (SourceKind::File, files[2].clone())
            ])
        );
        dialog.set_selection_modifiers(Modifiers::SHIFT);
        let _ = dialog.update(press(&files[3]), &mut draft, &saved, true);
        assert_eq!(
            dialog.selected_sources,
            BTreeSet::from([
                (SourceKind::File, files[2].clone()),
                (SourceKind::File, files[3].clone())
            ])
        );
        let _ = dialog.update(Message::ReviewSourceRemoval(0), &mut draft, &saved, true);
        assert_eq!(
            dialog.knowledge_settings_title(),
            Some("Remove knowledge sources")
        );
        let _ = dialog.update(Message::CancelSourceRemoval, &mut draft, &saved, true);
        assert!(dialog.knowledge_settings_height().is_none());
        let _ = dialog.update(Message::ReviewSourceRemoval(0), &mut draft, &saved, true);
        dialog.close_knowledge_settings();
        let _ = dialog.update(Message::RemoveSelectedSources, &mut draft, &saved, true);
        assert_eq!(draft.stores[0].sources, files);
        let _ = dialog.update(Message::ReviewSourceRemoval(0), &mut draft, &saved, true);
        let _ = dialog.update(Message::RemoveSelectedSources, &mut draft, &saved, true);
        assert_eq!(draft.stores[0].sources, files[..2]);
        assert!(dialog.selected_sources.is_empty());
        assert!(dialog.knowledge_settings_height().is_none());
    }

    #[test]
    fn knowledge_chooser_reviews_the_exact_files_and_commits_the_batch_atomically() {
        use iced::futures::{StreamExt as _, executor::block_on};

        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        std::fs::create_dir(root.join("Child")).unwrap();
        for name in ["alpha.txt", "beta.MD", "ignored.bin"] {
            std::fs::write(root.join(name), "Reviewed source fixture.").unwrap();
        }
        let saved = State::default();
        let mut draft = saved.clone();
        draft.stores.push(Store::new("Fixture store").unwrap());
        draft.stores.push(Store::new("Other store").unwrap());
        let mut dialog = Dialog::new(root.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, true);

        let complete = |dialog: &mut Dialog, draft: &mut State, message| {
            let mut actions =
                iced_runtime::task::into_stream(dialog.update(message, draft, &saved, true))
                    .unwrap();
            let Some(iced_runtime::Action::Output(result @ Message::Finished(..))) =
                block_on(actions.next())
            else {
                panic!("Missing file review result");
            };
            let _ = dialog.update(result, draft, &saved, true);
        };
        let review = |dialog: &mut Dialog, draft: &mut State| {
            let _ = dialog.update(
                Message::BrowseKnowledgeSources(SourceKind::File),
                draft,
                &saved,
                true,
            );
            let _ = dialog.update(
                Message::KnowledgeSourceBrowse(file_chooser::Message::Listed(
                    dialog.generation,
                    Err("Fixture starting folder".into()),
                )),
                draft,
                &saved,
                true,
            );
            let _ = dialog.update(
                Message::KnowledgeSourceBrowse(file_chooser::Message::Folder(
                    root.display().to_string(),
                )),
                draft,
                &saved,
                true,
            );
            let task = dialog.update(
                Message::KnowledgeSourceBrowse(file_chooser::Message::Go),
                draft,
                &saved,
                true,
            );
            let mut actions = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(
                listing @ Message::KnowledgeSourceBrowse(file_chooser::Message::Listed(
                    ticket,
                    Ok(_),
                )),
            )) = block_on(actions.next())
            else {
                panic!("Missing knowledge source listing");
            };
            let Message::KnowledgeSourceBrowse(file_chooser::Message::Listed(_, Ok(directory))) =
                &listing
            else {
                unreachable!()
            };
            assert_eq!(
                directory
                    .entries
                    .iter()
                    .map(|entry| entry.display.as_str())
                    .collect::<Vec<_>>(),
                ["Child", "alpha.txt", "beta.MD"]
            );
            let _ = dialog.update(listing, draft, &saved, true);
            for row in [1, 2] {
                let _ = dialog.update(
                    Message::KnowledgeSourceBrowse(file_chooser::Message::Entry(ticket, row)),
                    draft,
                    &saved,
                    true,
                );
            }
            complete(dialog, draft, Message::UseBrowsedKnowledgeSources);
            assert!(
                matches!(&dialog.source_picker, Some(KnowledgeSourcePicker::ReviewFiles { review, .. }) if review.rows.len() == 2 && review.rows[0].path == root.join("alpha.txt") && review.rows[1].path == root.join("beta.MD"))
            );
        };
        review(&mut dialog, &mut draft);
        assert!(draft.stores[0].sources.is_empty());
        let _ = dialog.update(Message::SelectStore(1), &mut draft, &saved, true);
        let _ = dialog.update(
            Message::ApproveBrowsedKnowledgeFiles,
            &mut draft,
            &saved,
            true,
        );
        assert!(draft.stores[1].sources.is_empty());

        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, true);
        review(&mut dialog, &mut draft);
        let approve = Message::ApproveBrowsedKnowledgeFiles;
        std::fs::write(root.join("alpha.txt"), "Changed source").unwrap();
        complete(&mut dialog, &mut draft, approve.clone());
        assert!(draft.stores[0].sources.is_empty());
        assert!(
            dialog
                .status
                .as_deref()
                .is_some_and(|status| status.contains("changed after the scan"))
        );
        std::fs::write(root.join("alpha.txt"), "Reviewed source fixture.").unwrap();
        draft.stores[0].add_source(root.join("beta.MD")).unwrap();
        complete(&mut dialog, &mut draft, approve.clone());
        assert_eq!(draft.stores[0].sources, [root.join("beta.MD")]);
        assert!(
            dialog
                .status
                .as_deref()
                .is_some_and(|status| status.contains("overlaps"))
        );
        draft.stores[0].remove_file(0);
        complete(&mut dialog, &mut draft, approve);
        assert_eq!(
            draft.stores[0].sources,
            [root.join("alpha.txt"), root.join("beta.MD")]
        );
        assert!(!dialog.knowledge_source_picker_open());
        assert!(saved.stores.is_empty());

        let previous = draft.stores[0].clone();
        for action in [
            knowledge_settings::Action::Open { create: true },
            knowledge_settings::Action::Name("New store".into()),
            knowledge_settings::Action::Apply,
        ] {
            let _ = dialog.update(Message::KnowledgeSettings(action), &mut draft, &saved, true);
        }
        let choose_native = Message::PickNativeKnowledgeSources(SourceKind::File);
        let _ = dialog.update(choose_native.clone(), &mut draft, &saved, true);
        let discarded_ticket = dialog.generation;
        assert!(
            dialog
                .pending_native_knowledge_kind(discarded_ticket)
                .is_some()
        );
        complete(
            &mut dialog,
            &mut draft,
            Message::NativeKnowledgeSourcesPicked(
                discarded_ticket,
                Some(vec![root.join("ignored.bin")]),
            ),
        );
        assert!(matches!(
            &dialog.source_picker,
            Some(KnowledgeSourcePicker::ReviewFiles { review, .. }) if !review.has_ready_files()
        ));
        let _ = dialog.update(choose_native, &mut draft, &saved, true);
        let ticket = dialog.generation;
        let invalid = root.join("binary.txt");
        std::fs::write(&invalid, [0xff, 0x00]).unwrap();
        let selected = vec![root.join("alpha.txt"), invalid, root.join("beta.MD")];
        let _ = dialog.update(
            Message::NativeKnowledgeSourcesPicked(discarded_ticket, Some(selected.clone())),
            &mut draft,
            &saved,
            true,
        );
        assert!(dialog.pending_native_knowledge_kind(ticket).is_some());
        complete(
            &mut dialog,
            &mut draft,
            Message::NativeKnowledgeSourcesPicked(ticket, Some(selected)),
        );
        assert!(
            matches!(&dialog.source_picker, Some(KnowledgeSourcePicker::ReviewFiles { review, .. }) if matches!(review.rows[1].status, file_review::Status::Skipped(_)))
        );
        assert_eq!(draft.stores.len(), 2);
        assert_eq!(draft.stores[0], previous);
        draft.stores[1].name = "New store".into();
        complete(
            &mut dialog,
            &mut draft,
            Message::ApproveBrowsedKnowledgeFiles,
        );
        assert_eq!(draft.stores.len(), 2);
        assert!(dialog.knowledge_source_picker_open());
        draft.stores[1].name = "Other store".into();
        complete(
            &mut dialog,
            &mut draft,
            Message::ApproveBrowsedKnowledgeFiles,
        );
        assert_eq!(draft.stores[0], previous);
        assert_eq!(draft.stores[2].name, "New store");
        assert_eq!(
            draft.stores[2].sources,
            [root.join("alpha.txt"), root.join("beta.MD")]
        );
        assert_eq!(dialog.selected, Some(2));
        assert!(!dialog.knowledge_source_picker_open());
    }

    #[test]
    fn initial_folder_scan_is_cancelled_or_committed_with_the_new_store() {
        use iced::futures::{StreamExt as _, executor::block_on};

        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "Reviewed source fixture.").unwrap();
        let saved = State::default();
        let mut draft = saved.clone();
        let mut dialog = Dialog::new(root.clone());

        for cancel in [true, false] {
            for action in [
                knowledge_settings::Action::Open { create: true },
                knowledge_settings::Action::Name("Folder store".into()),
                knowledge_settings::Action::Apply,
            ] {
                let _ = dialog.update(Message::KnowledgeSettings(action), &mut draft, &saved, true);
            }
            let (chooser, listing) = file_chooser::Chooser::open(
                file_chooser::Mode::KnowledgeFolder,
                Some(root.clone()),
                &mut dialog.generation,
            );
            dialog.source_picker = Some(KnowledgeSourcePicker::Browse(chooser));
            let mut actions = iced_runtime::task::into_stream(listing).unwrap();
            let Some(iced_runtime::Action::Output(
                listing @ file_chooser::Message::Listed(_, Ok(_)),
            )) = block_on(actions.next())
            else {
                panic!("Missing folder listing");
            };
            let _ = dialog.update(
                Message::KnowledgeSourceBrowse(listing),
                &mut draft,
                &saved,
                true,
            );
            let scan = dialog.update(
                Message::UseBrowsedKnowledgeSources,
                &mut draft,
                &saved,
                true,
            );
            let mut actions = iced_runtime::task::into_stream(scan).expect("Folder scan started");
            let Some(iced_runtime::Action::Output(result @ Message::Finished(_, Ok(_)))) =
                block_on(actions.next())
            else {
                panic!("Missing successful folder review");
            };
            assert!(draft.stores.is_empty());
            if cancel {
                let _ = dialog.update(
                    Message::CloseKnowledgeSourcePicker,
                    &mut draft,
                    &saved,
                    true,
                );
            }
            let _ = dialog.update(result, &mut draft, &saved, true);
            assert_eq!(dialog.folder_review.is_some(), !cancel);
            assert!(draft.stores.is_empty());
            let _ = dialog.update(Message::ApproveFolder, &mut draft, &saved, true);
            assert_eq!(draft.stores.len(), usize::from(!cancel));
            assert!(!dialog.knowledge_source_picker_open());
        }
        assert_eq!(draft.stores[0].name, "Folder store");
        assert_eq!(draft.stores[0].folders[0].root, root);
        assert_eq!(draft.stores[0].source_count(), 1);
    }

    #[test]
    fn folder_review_search_reveals_actions_without_changing_pages_or_approving() {
        use crate::ai_manager::search::{KnowledgeSetting, Setting, Target};
        use crate::ai_manager::{Dialog as Manager, Message as ManagerMessage};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("sources");
        std::fs::create_dir(&source).unwrap();
        for index in 0..=SOURCE_REVIEW_PAGE_SIZE {
            std::fs::write(
                source.join(format!("{index:03}.txt")),
                "Folder review fixture.",
            )
            .unwrap();
        }
        let (reply, _cancel) = folder::start(
            source,
            None,
            SOURCE_REVIEW_PAGE_SIZE + 1,
            Vec::new(),
            knowledge::source::Options::default(),
        )
        .unwrap();
        let review = reply.recv_blocking().unwrap().unwrap();
        assert_eq!(folder_review_pages(&review), 2);

        let mut config = Configuration::default();
        config
            .local
            .stores
            .push(Store::new("Folder review").unwrap());
        let mut manager = Manager::new(Arc::new(config), root, Vec::new(), true, None);
        manager.local.select_store(Some(0));
        manager.local.folder_review = Some(Arc::clone(&review));
        let reveal = |manager: &mut Manager, query: &str, target| {
            let _ = manager.update(ManagerMessage::Search(query.into()));
            assert_eq!(
                manager.search_results().next(),
                Some(Target::Setting(Setting::Knowledge(target)))
            );
            let _ = manager.update(ManagerMessage::SearchChoose);
            assert_eq!(manager.revealed.setting, Some(Setting::Knowledge(target)));
        };
        reveal(
            &mut manager,
            "next folder review page",
            KnowledgeSetting::FolderReviewNext,
        );
        assert_eq!(manager.local.folder_review_page, 0);
        let _ = manager.update(ManagerMessage::Local(Message::FolderReviewPage(1)));
        reveal(
            &mut manager,
            "previous folder review page",
            KnowledgeSetting::FolderReviewPrevious,
        );
        reveal(
            &mut manager,
            "approve folder files",
            KnowledgeSetting::FolderReviewApprove,
        );
        assert_eq!(manager.local.folder_review_page, 1);
        assert!(Arc::ptr_eq(
            manager.local.folder_review.as_ref().unwrap(),
            &review
        ));
        assert!(manager.local_state.stores[0].folders.is_empty());
        assert!(!manager.local.busy());
        let _ = manager.update(ManagerMessage::Local(Message::DiscardFolderReview));
        let stale = Target::Setting(Setting::Knowledge(KnowledgeSetting::FolderReviewApprove));
        let _ = manager.update(ManagerMessage::Search("approve folder files".into()));
        assert_eq!(manager.search_results().next(), None);
        assert!(
            iced_runtime::task::into_stream(manager.update(ManagerMessage::Reveal(stale)))
                .is_none()
        );
    }

    #[test]
    fn java_knowledge_adoption_requires_explicit_store_and_exact_source_review() {
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let imported = java_knowledge::import(java_knowledge::FIXTURE).unwrap();
        let mut saved = Configuration {
            java_knowledge_hints: imported.hints,
            ..Configuration::default()
        };
        saved
            .local
            .stores
            .push(Store::new("Existing native store").unwrap());
        saved
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();

        let mut state = saved.local.clone();
        let mut dialog = Dialog::new(root.clone());
        let hint = saved.java_knowledge_hints.get("local-store").unwrap();
        dialog.stage_java_knowledge_store(&state, hint);
        assert_eq!(state.stores, saved.local.stores);
        assert_eq!(dialog.store_name, "Project notes");
        assert!(dialog.selected.is_none());

        let _ = dialog.update(Message::CreateStore, &mut state, &saved.local, true);
        assert_eq!(dialog.selected, Some(1));
        assert_eq!(state.stores[0], saved.local.stores[0]);
        let native = &state.stores[1];
        assert_eq!(native.java_origin, Some(hint.origin().unwrap()));
        assert_ne!(native.id(), Some(hint.id.as_str()));
        assert!(native.sources.is_empty() && native.folders.is_empty());
        assert!(native.qdrant.is_none() && native.embedding.as_ref().unwrap().model.is_empty());
        assert!(native.text_profile.is_none() && native.coding_profile.is_none());
        assert!(!native.autonomous_enabled && !native.has_automatic_sources());

        dialog.source = "Manual path draft".into();
        let other = saved.java_knowledge_hints.get("remote-store").unwrap();
        dialog.stage_java_knowledge_source(&state, other, "tree");
        assert_eq!(dialog.source, "Manual path draft");
        dialog.stage_java_knowledge_source(&state, hint, "tree");
        assert!(dialog.source.is_empty());
        let options = dialog.folder_options.options().unwrap();
        assert_eq!(options.sync, SyncMode::Manual);
        assert_eq!(options.includes, ["**/*.md"]);
        assert_eq!(options.excludes, ["draft/**"]);
        assert_eq!(options.max_file_bytes.unwrap().bytes(), 1_048_576);
        assert!(!options.recursive && options.respect_gitignore && !options.include_pdf);
        assert!(state.stores[1].folders.is_empty());

        let next = Configuration {
            local: state,
            ..saved.clone()
        };
        let checks = std::cell::Cell::new(0);
        let denied = next
            .save_with_policy(&root, &saved, None, None, || {
                let check = checks.get();
                checks.set(check + 1);
                ClipboardPolicy {
                    ai_allowed: check == 0,
                    ..ClipboardPolicy::default()
                }
            })
            .unwrap_err();
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            Configuration::load_fixture_from(&root)
                .unwrap()
                .local
                .stores
                .len(),
            1
        );

        next.save_fixture_at(&root, &saved).unwrap();
        let reloaded = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(
            reloaded.local.stores[1].java_origin,
            Some(hint.origin().unwrap())
        );
    }

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
    fn runtime_search_reveals_progress_without_stopping_the_request() {
        use crate::ai_manager::search::{Control, Setting, Target};
        use crate::ai_manager::{
            Dialog as Manager, Message as ManagerMessage, Page as ManagerPage,
        };
        use std::sync::atomic::Ordering;

        let profile = crate::snippets::tests::Profile::new();
        for repair in [false, true] {
            let mut manager = Manager::new(
                Arc::new(Configuration::default()),
                profile.0.clone(),
                Vec::new(),
                true,
                None,
            );
            let cancelled = if repair {
                manager.page = ManagerPage::LocalModels;
                manager.local.open_backend_repair_for_test()
            } else {
                let (cancel, requested) = runtime_package::Cancel::test_token();
                manager.local.request = Some(Request::Runtime {
                    cancel,
                    requires_ai: true,
                });
                requested
            };
            let (pause, paused) = runtime_package::online::Pause::test_token();
            manager.local.online.pause = Some(pause);
            manager.local.online.progress = Some(runtime_package::online::Progress {
                received: 4096,
                total: 8192,
                resumable: false,
            });
            let _ = manager.update(ManagerMessage::Search("pause runtime download".into()));
            assert!(manager.search_results().next().is_none());

            manager.local.online.progress.as_mut().unwrap().resumable = true;
            let generation = manager.local.generation;
            for (query, control) in [
                ("pause runtime download", Control::PauseRuntimeDownload),
                ("cancel runtime request", Control::CancelRuntimeRequest),
            ] {
                let setting = Setting::Control(control);
                let _ = manager.update(ManagerMessage::Search(query.into()));
                assert_eq!(
                    manager.search_results().collect::<Vec<_>>(),
                    [Target::Setting(setting)]
                );
                let reveal = manager.update(ManagerMessage::SearchChoose);
                assert!(iced_runtime::task::into_stream(reveal).is_none());
                assert_eq!(manager.page, ManagerPage::LocalModels);
                assert_eq!(manager.revealed.setting, Some(setting));
                assert_eq!(manager.local.backend_repair_open(), repair);
                assert_eq!(manager.local.generation, generation);
                assert_eq!(manager.local.online.progress.unwrap().received, 4096);
                assert!(manager.local.busy());
                assert!(!paused.load(Ordering::Acquire));
                assert!(!cancelled.load(Ordering::Acquire));
            }
        }
    }

    #[test]
    fn runtime_toolbar_search_preserves_review_state_and_offline_paths() {
        use crate::ai_manager::search::{Control, Setting, Target};
        use crate::ai_manager::{Dialog as Manager, Message as ManagerMessage};

        let profile = crate::snippets::tests::Profile::new();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            profile.0.clone(),
            Vec::new(),
            true,
            None,
        );
        manager.local.set_runtime_downloads_allowed(false);
        let import = Target::Setting(Setting::Control(Control::OpenSignedPackage));
        let install = Target::Setting(Setting::Control(Control::InstallStableRuntime));
        let _ = manager.update(ManagerMessage::Search("import signed package".into()));
        assert_eq!(manager.search_results().next(), Some(import));
        let _ = manager.update(ManagerMessage::SearchChoose);
        assert!(!manager.local.offline_runtime_open);
        assert!(!manager.local.busy());

        manager
            .local
            .managed
            .set_active_package(Some(("fixture runtime".into(), PackageBackend::Cpu)));
        let update = Setting::Control(Control::UpdateStableRuntime);
        let _ = manager.update(ManagerMessage::Search(
            "check install runtime update".into(),
        ));
        assert_eq!(
            manager.search_results().next(),
            Some(Target::Setting(update))
        );
        let _ = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(manager.revealed.setting, Some(update));
        assert!(manager.local.runtime_review_title().is_none());
        assert!(!manager.local.busy());
        assert!(!manager.search_target_available(install));
        let _ = manager.open_page(install);
        assert_eq!(manager.revealed.setting, Some(update));

        let _ = manager.update(ManagerMessage::Local(Message::RuntimePath(
            RuntimeFile::Index,
            "fixture-index.json".into(),
        )));
        let index = Setting::Control(Control::SignedIndex);
        let _ = manager.open_page(Target::Setting(index));
        assert_eq!(manager.revealed.setting, Some(index));
        assert!(manager.local.offline_runtime_open);
        assert!(!manager.search_target_available(import));
        let hide = Setting::Control(Control::HideSignedPackage);
        let _ = manager.update(ManagerMessage::Search("hide package import".into()));
        assert_eq!(manager.search_results().next(), Some(Target::Setting(hide)));
        let _ = manager.update(ManagerMessage::SearchChoose);
        assert_eq!(manager.revealed.setting, Some(hide));
        assert!(manager.local.offline_runtime_open);
        assert_eq!(manager.local.runtime_paths()[0], "fixture-index.json");
        assert!(!manager.local.busy());

        manager.local.online.review = Some(online::Review::Versions);
        assert!(!manager.search_target_available(Target::Setting(hide)));
        let _ = manager.open_page(Target::Setting(hide));
        assert!(manager.local.runtime_versions_open());
        assert_eq!(manager.local.runtime_paths()[0], "fixture-index.json");
    }

    #[test]
    fn runtime_search_reveals_only_the_current_confirmation() {
        use crate::ai_manager::search::{Control, Setting, Target};
        use crate::ai_manager::{Dialog as Manager, Message as ManagerMessage};
        use crate::private_files::Directory;

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        Directory::open(&root, false)
            .unwrap()
            .private_child("llama-runtime", true)
            .unwrap()
            .publish_copy("activation.json", b"fixture activation")
            .unwrap();
        let mut manager = Manager::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        for (query, control) in [
            ("keep saved zip", Control::KeepRuntimeDownload),
            ("cancel runtime removal", Control::CancelRuntimeRemoval),
        ] {
            let _ = manager.update(ManagerMessage::Search(query.into()));
            assert!(
                !manager
                    .search_results()
                    .any(|target| { target == Target::Setting(Setting::Control(control)) })
            );
        }

        manager.local.online.discard_confirmed = true;
        for (query, control) in [
            ("keep saved zip", Control::KeepRuntimeDownload),
            ("discard saved zip", Control::DiscardRuntimeDownload),
        ] {
            let setting = Setting::Control(control);
            let _ = manager.update(ManagerMessage::Search(query.into()));
            let _ = manager.update(ManagerMessage::SearchChoose);
            assert_eq!(manager.revealed.setting, Some(setting));
            assert!(manager.local.online.discard_confirmed);
            assert!(!manager.local.busy());
        }

        manager.local.runtime_removal = runtime_package::inspect_removal(&root).unwrap();
        for (query, control) in [
            ("remove runtime", Control::ConfirmRuntimeRemoval),
            ("cancel runtime removal", Control::CancelRuntimeRemoval),
        ] {
            let setting = Setting::Control(control);
            let _ = manager.update(ManagerMessage::Search(query.into()));
            assert_eq!(
                manager.search_results().next(),
                Some(Target::Setting(setting))
            );
            let _ = manager.update(ManagerMessage::SearchChoose);
            assert_eq!(manager.revealed.setting, Some(setting));
            assert!(manager.local.runtime_removal_review_open());
            assert!(!manager.local.busy());
            assert!(root.join("llama-runtime").join("activation.json").is_file());
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
