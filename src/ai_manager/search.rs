//! Search destinations for the AI Manager's existing pages and settings.

use std::borrow::Cow;

use iced::widget::{Id, container};
use iced::{Element, Length, Task};

use super::{Page, java_models, local::Role, profiles};
use crate::settings_search::{self, Candidate};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Page(Page),
    Setting(Setting),
}

impl Target {
    pub(super) fn page(self) -> Page {
        match self {
            Self::Page(page) => page,
            Self::Setting(setting) => setting.page(),
        }
    }

    pub(super) fn label(self, hints: &java_models::Hints) -> Cow<'static, str> {
        match self {
            Self::Page(page) => page.title().into(),
            Self::Setting(setting @ Setting::ImportedJavaModel(key)) => hints
                .search_hint(key)
                .map_or_else(|| setting.title().into(), |hint| hint.label().into()),
            Self::Setting(setting) => setting.title().into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Setting {
    Features,
    ConfirmBeforeSend,
    AgentDebugMessages,
    AgentRuntimeMessages,
    Internet(super::internet::Field),
    GlobalTimeout,
    SecurityProfile,
    ClearSecurityProfile,
    AgentHistoryLimit,
    SnippetInstructions,
    SnippetAlternatives,
    CodeTextLanguage,
    ChatTerminalActions,
    AgentShortcut,
    AgentShortcutName,
    AgentShortcutCase,
    AgentTriage,
    AgentRunPresentation,
    AgentExecution,
    AgentConfirmation,
    ShowAgentSetup,
    JournalSemanticSearch,
    ImportedGeneral(ImportedGeneral),
    WizardScan,
    WizardTest,
    Profile(ProfileSetting),
    ProfileAction(ProfileAction),
    Role(Role),
    Embedding,
    LocalPreference(LocalPreference),
    Knowledge(KnowledgeSetting),
    Hub(HubSetting),
    Managed(ManagedSetting),
    Mlx(MlxSetting),
    ImportedJavaModel(java_models::SearchKey),
    SkillProvider(SkillProviderSetting),
    Control(Control),
}

impl Setting {
    fn page(self) -> Page {
        match self {
            Self::Features
            | Self::ConfirmBeforeSend
            | Self::AgentDebugMessages
            | Self::AgentRuntimeMessages
            | Self::Internet(_)
            | Self::GlobalTimeout
            | Self::SecurityProfile
            | Self::ClearSecurityProfile
            | Self::AgentHistoryLimit
            | Self::SnippetInstructions
            | Self::SnippetAlternatives
            | Self::CodeTextLanguage
            | Self::ChatTerminalActions
            | Self::AgentShortcut
            | Self::AgentShortcutName
            | Self::AgentShortcutCase
            | Self::AgentTriage
            | Self::AgentRunPresentation
            | Self::AgentExecution
            | Self::AgentConfirmation
            | Self::ShowAgentSetup
            | Self::JournalSemanticSearch
            | Self::ImportedGeneral(_) => Page::General,
            Self::Profile(_) | Self::ProfileAction(_) | Self::WizardScan | Self::WizardTest => {
                Page::Profiles
            }
            Self::Role(_) | Self::Embedding | Self::LocalPreference(_) => Page::LocalPreferences,
            Self::Knowledge(_) => Page::Knowledge,
            Self::Hub(_) | Self::Managed(_) | Self::Mlx(_) | Self::ImportedJavaModel(_) => {
                Page::LocalModels
            }
            Self::SkillProvider(_) => Page::Skills,
            Self::Control(control) => control.page(),
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Features => "Enable AI features",
            Self::AgentDebugMessages => "Show Agent debug messages",
            Self::AgentRuntimeMessages => "Show Agent runtime messages",
            Self::Internet(field) => field.title(),
            Self::ConfirmBeforeSend => "Show confirmation before sending terminal AI requests",
            Self::GlobalTimeout => "Global request timeout (minutes)",
            Self::SecurityProfile => "Security-check profile",
            Self::ClearSecurityProfile => "Clear security-check profile",
            Self::AgentHistoryLimit => "Recent agent prompt limit",
            Self::SnippetInstructions => "Allow additional instructions in snippets",
            Self::SnippetAlternatives => "Alternative solutions per request",
            Self::CodeTextLanguage => "Default code text language",
            Self::ChatTerminalActions => "AI chat code blocks in the terminal",
            Self::AgentShortcut => "Enable terminal agent shortcuts",
            Self::AgentShortcutName => "Base command name",
            Self::AgentShortcutCase => "Ignore command name case",
            Self::AgentTriage => "Agent complexity check",
            Self::AgentRunPresentation => "Default Agent presentation",
            Self::AgentExecution => "Allow Agent command execution",
            Self::AgentConfirmation => "Require approval for mutating Agent command sets",
            Self::ShowAgentSetup => "Show terminal agent setup for submitted aliases",
            Self::JournalSemanticSearch => "Use local semantic ranking for journal questions",
            Self::ImportedGeneral(setting) => setting.title(),
            Self::WizardScan => "Find local model servers",
            Self::WizardTest => "Test profile connection",
            Self::Profile(setting) => setting.title(),
            Self::ProfileAction(action) => action.title(),
            Self::Role(role) => role.label(),
            Self::Embedding => "Default embedding model",
            Self::LocalPreference(setting) => setting.title(),
            Self::Knowledge(setting) => setting.title(),
            Self::Hub(setting) => setting.title(),
            Self::Managed(setting) => setting.title(),
            Self::Mlx(setting) => setting.title(),
            Self::ImportedJavaModel(_) => "Imported Java Model Settings",
            Self::SkillProvider(setting) => setting.title(),
            Self::Control(control) => control.title(),
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Features => "safety enable disable",
            Self::AgentDebugMessages => "agent diagnostic command output transcript messages",
            Self::AgentRuntimeMessages => "agent runtime messages run defaults",
            Self::Internet(field) => field.keywords(),
            Self::ConfirmBeforeSend => {
                "confirm before send summarize recent output solve problem request review"
            }
            Self::GlobalTimeout => "limits timeouts",
            Self::SecurityProfile => "snippet security review dedicated AI profile",
            Self::ClearSecurityProfile => "snippet security review use default profile",
            Self::AgentHistoryLimit => "agent input history size recent prompts 5 100",
            Self::SnippetInstructions => "snippet AI additional instructions prompt",
            Self::SnippetAlternatives => "snippet AI alternative solution count",
            Self::CodeTextLanguage => "snippet script comments output messages translation",
            Self::ChatTerminalActions => "actions insert run copy save",
            Self::AgentShortcut => "agent ask plan alias input OSC 133",
            Self::AgentShortcutName => "agent alias terminal prompt",
            Self::AgentShortcutCase => "agent alias case insensitive uppercase lowercase",
            Self::AgentExecution => "agent execute run swarm scheduled diagnostics permission",
            Self::AgentConfirmation => {
                "agent confirmation mutating command sets approve remaining always"
            }
            Self::ShowAgentSetup => "run dialog ask skip bypass startup prompt confirmation",
            Self::JournalSemanticSearch => "journal semantic search embedding lexical ranking BM25",
            Self::AgentTriage => "triage solution options approaches planning",
            Self::AgentRunPresentation => "agent run execution target chat tab terminal window",
            Self::ImportedGeneral(setting) => setting.keywords(),
            Self::WizardScan => "scan discover loopback LM Studio Ollama",
            Self::WizardTest => "run fixed CLI test verify provider",
            Self::Profile(setting) => setting.keywords(),
            Self::ProfileAction(action) => action.keywords(),
            Self::Role(Role::Text) => "translation profile roles",
            Self::Role(_) => "profile roles",
            Self::Embedding => "rag knowledge vector",
            Self::LocalPreference(setting) => setting.keywords(),
            Self::Knowledge(KnowledgeSetting::Name | KnowledgeSetting::Create) => {
                "new add collection rag"
            }
            Self::Knowledge(KnowledgeSetting::Update | KnowledgeSetting::Rebuild) => {
                "index refresh embedding vectors rag"
            }
            Self::Knowledge(KnowledgeSetting::QdrantAuthentication) => "vault API key credential",
            Self::Knowledge(KnowledgeSetting::QdrantCredentialVariable) => "environment credential",
            Self::Knowledge(KnowledgeSetting::TestQuery | KnowledgeSetting::TestSearch) => {
                "retrieval"
            }
            Self::Knowledge(KnowledgeSetting::Inventory(action)) => action.keywords(),
            Self::Knowledge(_) => "",
            Self::Hub(HubSetting::Access) => {
                "Hugging Face HuggingFace token vault credential anonymous"
            }
            Self::Hub(HubSetting::Query) => "Hugging Face HuggingFace Hub GGUF search query",
            Self::Hub(_) => "Hugging Face HuggingFace Hub GGUF",
            Self::Managed(ManagedSetting::IdleMinutes | ManagedSetting::NeverUnload) => {
                "idle unload llama.cpp"
            }
            Self::Managed(ManagedSetting::OpenImport | ManagedSetting::ChooseFiles) => {
                "local model file picker register"
            }
            Self::Managed(_) => "llama.cpp",
            Self::Mlx(MlxSetting::Idle) => "idle timeout unload",
            Self::Mlx(MlxSetting::NeverUnload) => "keep running",
            Self::Mlx(_) => "Apple Silicon",
            Self::ImportedJavaModel(_) => "imported Java model settings",
            Self::Control(Control::DiscardRuntimeDownload) => "ZIP",
            Self::Control(Control::KeepRuntimeDownload) => "runtime download",
            Self::Control(Control::UpdateStableRuntime) => "check install runtime update llama.cpp",
            Self::Control(Control::InstallStableRuntime) => "stable signed llama.cpp",
            Self::Control(Control::RuntimeVersionsEntry) => {
                "other version check signed runtime updates"
            }
            Self::Control(Control::OpenSignedPackage) => "import local package offline ZIP",
            Self::SkillProvider(_) | Self::Control(_) => "",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImportedGeneral {
    JournalSearch,
    AgentConfirmation,
    AgentExecution,
    AgentShortcut,
}

impl ImportedGeneral {
    const ALL: [Self; 4] = [
        Self::JournalSearch,
        Self::AgentConfirmation,
        Self::AgentExecution,
        Self::AgentShortcut,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::JournalSearch => "Use imported journal search preference",
            Self::AgentConfirmation => "Use imported confirmation preference",
            Self::AgentExecution => "Use imported execution preference",
            Self::AgentShortcut => "Use imported command preferences",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::JournalSearch => "Java restore semantic ranking journal",
            Self::AgentConfirmation => "Java restore agent mutating command approval",
            Self::AgentExecution => "Java restore agent terminal execution",
            Self::AgentShortcut => "Java restore agent alias name case",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalPreference {
    RuntimeUpdatePolicy,
    RuntimeBackend,
    EmbeddingSource,
    EmbeddingSetup,
    RefreshEmbeddingModels,
    HubAccess,
    ImportedRole(super::java_roles::Role),
}

impl LocalPreference {
    const CONTROLS: [Self; 6] = [
        Self::RuntimeUpdatePolicy,
        Self::RuntimeBackend,
        Self::EmbeddingSource,
        Self::EmbeddingSetup,
        Self::RefreshEmbeddingModels,
        Self::HubAccess,
    ];

    pub(super) fn title(self) -> &'static str {
        use super::java_roles::Role as JavaRole;
        match self {
            Self::RuntimeUpdatePolicy => "Managed runtime update policy",
            Self::RuntimeBackend => "Preferred runtime backend",
            Self::EmbeddingSource => "Default embedding source",
            Self::EmbeddingSetup => "Set up an embedding model",
            Self::RefreshEmbeddingModels => "Refresh embedding models",
            Self::HubAccess => "Hugging Face access preference",
            Self::ImportedRole(role) => match role {
                JavaRole::Default => "Imported Java default profile",
                JavaRole::Text => "Imported Java text profile",
                JavaRole::Coding => "Imported Java coding profile",
                JavaRole::Agent => "Imported Java agent profile",
                JavaRole::Security => "Imported Java security profile",
                JavaRole::Journal => "Imported Java journal profile",
            },
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::RuntimeUpdatePolicy => "llama.cpp updates notify automatic stable signed runtime",
            Self::RuntimeBackend => "llama.cpp runtime CPU GPU Metal Vulkan",
            Self::EmbeddingSource => "default embedding model vector managed local endpoint",
            Self::EmbeddingSetup => "guided embedding model vector",
            Self::RefreshEmbeddingModels => "reload managed policy embedding inventory",
            Self::HubAccess => "hub credential vault token gated anonymous",
            Self::ImportedRole(_) => "adopt use imported profile role backup restore",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfileAction {
    TestConnection,
    TestCli,
    DiscoverModels,
    LoadModels,
    ChooseImportedModel,
    RefreshManagedModels,
    ConfirmImportedModel,
    Delete,
    CancelDelete,
    ConfirmDelete,
}

impl ProfileAction {
    const ALL: [Self; 10] = [
        Self::TestConnection,
        Self::TestCli,
        Self::DiscoverModels,
        Self::LoadModels,
        Self::ChooseImportedModel,
        Self::RefreshManagedModels,
        Self::ConfirmImportedModel,
        Self::Delete,
        Self::CancelDelete,
        Self::ConfirmDelete,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::TestConnection => "Test connection",
            Self::TestCli => "Run fixed CLI test",
            Self::DiscoverModels => "Discover models",
            Self::LoadModels => "Load models",
            Self::ChooseImportedModel => "Choose local model",
            Self::RefreshManagedModels => "Refresh managed models",
            Self::ConfirmImportedModel => "Confirm model and review profile",
            Self::Delete => "Delete profile",
            Self::CancelDelete => "Cancel profile removal",
            Self::ConfirmDelete => "Remove profile",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::TestConnection => "check provider endpoint",
            Self::TestCli => "check executable command",
            Self::DiscoverModels | Self::LoadModels => "provider model discovery",
            Self::ChooseImportedModel | Self::ConfirmImportedModel => {
                "Java restore mapping GGUF MLX"
            }
            Self::RefreshManagedModels => "local models refresh GGUF MLX",
            Self::Delete | Self::CancelDelete | Self::ConfirmDelete => "profile deletion",
        }
    }

    pub(super) fn section(self) -> Option<profiles::Section> {
        match self {
            Self::ChooseImportedModel => Some(profiles::Section::Connection),
            Self::LoadModels | Self::RefreshManagedModels | Self::ConfirmImportedModel => {
                Some(profiles::Section::Model)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSetting {
    OpenImport,
    CloseImport,
    ChooseFiles,
    Source,
    Projector,
    ImportPurpose,
    Import,
    Reference,
    Refresh,
    Select,
    SetDefault,
    ClearDefault,
    RepairBackend,
    Reattach,
    Start,
    Stop,
    Test,
    Configure,
    ReviewRemoval,
    Name,
    Backend,
    Purpose,
    Context,
    CpuThreads,
    GpuLayers,
    IdleMinutes,
    NeverUnload,
    Save,
    Cancel,
    ConfirmReattachment,
    ConfirmRemoval,
}

impl ManagedSetting {
    const ALL: [Self; 31] = [
        Self::OpenImport,
        Self::CloseImport,
        Self::ChooseFiles,
        Self::Source,
        Self::Projector,
        Self::ImportPurpose,
        Self::Import,
        Self::Reference,
        Self::Refresh,
        Self::Select,
        Self::SetDefault,
        Self::ClearDefault,
        Self::RepairBackend,
        Self::Reattach,
        Self::Start,
        Self::Stop,
        Self::Test,
        Self::Configure,
        Self::ReviewRemoval,
        Self::Name,
        Self::Backend,
        Self::Purpose,
        Self::Context,
        Self::CpuThreads,
        Self::GpuLayers,
        Self::IdleMinutes,
        Self::NeverUnload,
        Self::Save,
        Self::Cancel,
        Self::ConfirmReattachment,
        Self::ConfirmRemoval,
    ];

    pub(super) fn configuration_field(self) -> bool {
        matches!(
            self,
            Self::Name
                | Self::Backend
                | Self::Purpose
                | Self::Context
                | Self::CpuThreads
                | Self::GpuLayers
                | Self::IdleMinutes
                | Self::NeverUnload
        )
    }

    fn title(self) -> &'static str {
        match self {
            Self::OpenImport => "Import GGUF…",
            Self::CloseImport => "Close GGUF import",
            Self::ChooseFiles => "Choose GGUF files…",
            Self::Source => "GGUF model path",
            Self::Projector => "Vision projector path",
            Self::ImportPurpose => "GGUF import purpose",
            Self::Import => "Import private GGUF copy",
            Self::Reference => "Register external GGUF",
            Self::Refresh => "Refresh managed GGUF models",
            Self::Select => "Select a managed GGUF model",
            Self::SetDefault => "Set installed local model as default",
            Self::ClearDefault => "Clear default managed GGUF model",
            Self::RepairBackend => "Install matching GGUF runtime",
            Self::Reattach => "Reattach local GGUF",
            Self::Start => "Start managed GGUF model",
            Self::Stop => "Stop managed GGUF model",
            Self::Test => "Test managed GGUF model",
            Self::Configure => "Configure managed GGUF model",
            Self::ReviewRemoval => "Remove managed GGUF model",
            Self::Name => "GGUF model name",
            Self::Backend => "GGUF model backend",
            Self::Purpose => "GGUF model purpose",
            Self::Context => "GGUF context size (tokens)",
            Self::CpuThreads => "GGUF CPU threads",
            Self::GpuLayers => "GGUF GPU layers",
            Self::IdleMinutes => "GGUF idle timeout (minutes)",
            Self::NeverUnload => "Never unload managed GGUF model while idle",
            Self::Save => "Save GGUF model settings",
            Self::Cancel => "Cancel GGUF model review",
            Self::ConfirmReattachment => "Confirm GGUF model reattachment",
            Self::ConfirmRemoval => "Confirm GGUF model removal",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MlxSetting {
    Unpin,
    CheckOnline,
    OnlineChoice,
    ReviewOnline,
    RuntimeIndex,
    RuntimeSignature,
    RuntimeArchive,
    Refresh,
    ReviewRuntime,
    ReviewRuntimeRemoval,
    Source,
    ReviewImport,
    Select,
    ReviewReattachment,
    Start,
    Test,
    Stop,
    ReviewRemoval,
    Name,
    Idle,
    NeverUnload,
    Save,
    CancelReview,
    ConfirmOnline,
    ConfirmRuntime,
    ConfirmCatalog,
    ConfirmImport,
    ConfirmReattachment,
    ConfirmRemoval,
    ConfirmRuntimeRemoval,
}

impl MlxSetting {
    const ALL: [Self; 30] = [
        Self::Unpin,
        Self::CheckOnline,
        Self::OnlineChoice,
        Self::ReviewOnline,
        Self::RuntimeIndex,
        Self::RuntimeSignature,
        Self::RuntimeArchive,
        Self::Refresh,
        Self::ReviewRuntime,
        Self::ReviewRuntimeRemoval,
        Self::Source,
        Self::ReviewImport,
        Self::Select,
        Self::ReviewReattachment,
        Self::Start,
        Self::Test,
        Self::Stop,
        Self::ReviewRemoval,
        Self::Name,
        Self::Idle,
        Self::NeverUnload,
        Self::Save,
        Self::CancelReview,
        Self::ConfirmOnline,
        Self::ConfirmRuntime,
        Self::ConfirmCatalog,
        Self::ConfirmImport,
        Self::ConfirmReattachment,
        Self::ConfirmRemoval,
        Self::ConfirmRuntimeRemoval,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Unpin => "Follow stable MLX releases",
            Self::CheckOnline => "Check signed MLX releases",
            Self::OnlineChoice => "Select a signed MLX release",
            Self::ReviewOnline => "Review MLX runtime download",
            Self::RuntimeIndex => "MLX runtime index path",
            Self::RuntimeSignature => "MLX runtime detached signature path",
            Self::RuntimeArchive => "MLX runtime package archive path",
            Self::Refresh => "Refresh managed MLX models and runtime",
            Self::ReviewRuntime => "Review signed offline MLX runtime",
            Self::ReviewRuntimeRemoval => "Remove installed MLX runtime",
            Self::Source => "Local MLX model directory",
            Self::ReviewImport => "Review MLX model import",
            Self::Select => "Choose a registered MLX model",
            Self::ReviewReattachment => "Review local MLX reattachment",
            Self::Start => "Start managed MLX model",
            Self::Test => "Test managed MLX model",
            Self::Stop => "Stop managed MLX model",
            Self::ReviewRemoval => "Remove managed MLX model",
            Self::Name => "MLX model display name",
            Self::Idle => "MLX model idle minutes",
            Self::NeverUnload => "Never unload MLX model while idle",
            Self::Save => "Save MLX model settings",
            Self::CancelReview => "Cancel MLX review",
            Self::ConfirmOnline => "Download and install signed MLX runtime",
            Self::ConfirmRuntime => "Install reviewed offline MLX runtime",
            Self::ConfirmCatalog => "Accept reviewed signed MLX catalog",
            Self::ConfirmImport => "Import reviewed MLX model",
            Self::ConfirmReattachment => "Reattach reviewed MLX model",
            Self::ConfirmRemoval => "Confirm managed MLX model removal",
            Self::ConfirmRuntimeRemoval => "Confirm installed MLX runtime removal",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SkillProviderSetting {
    Select,
    Add,
    Remove,
    Name,
    Type,
    Enabled,
    Base,
    CredentialEnv,
    VaultCredential,
    ClearVaultCredential,
    Username,
    Sample,
    Apply,
    Revert,
    Test,
    Close,
}

impl SkillProviderSetting {
    const ALL: [Self; 16] = [
        Self::Select,
        Self::Add,
        Self::Remove,
        Self::Name,
        Self::Type,
        Self::Enabled,
        Self::Base,
        Self::CredentialEnv,
        Self::VaultCredential,
        Self::ClearVaultCredential,
        Self::Username,
        Self::Apply,
        Self::Revert,
        Self::Test,
        Self::Sample,
        Self::Close,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Select => "Choose external skill provider",
            Self::Add => "Add external skill provider",
            Self::Remove => "Remove selected external skill provider",
            Self::Name => "External skill provider name",
            Self::Type => "External skill provider type",
            Self::Enabled => "Enable external skill provider",
            Self::Base => "External skill provider base URL",
            Self::CredentialEnv => "External skill provider credential environment variable",
            Self::VaultCredential => "Choose external skill provider vault credential",
            Self::ClearVaultCredential => "Clear external skill provider vault credential",
            Self::Username => "External skill provider HTTP Basic username",
            Self::Sample => "External skill provider test document",
            Self::Apply => "Apply external skill provider edits",
            Self::Revert => "Revert external skill provider edits",
            Self::Test => "Test external skill provider",
            Self::Close => "Close external skill provider editor",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HubSetting {
    Access,
    RecommendationRole,
    RecommendedModel,
    ReviewRecommendation,
    Query,
    Search,
    LoadMore,
    ShowAllModels,
    Format,
    Repository,
    ReviewRepository,
    ReviewMlx,
    MlxFiles,
    DownloadMlx,
    FindPausedMlx,
    ResumeMlx,
    DiscardPausedMlx,
    Artifact,
    Purpose,
    Download,
    FindPaused,
    DiscardPaused,
    KeepPaused,
    Resume,
    Pause,
}

impl HubSetting {
    const ALL: [Self; 25] = [
        Self::Access,
        Self::RecommendationRole,
        Self::RecommendedModel,
        Self::ReviewRecommendation,
        Self::Query,
        Self::Search,
        Self::LoadMore,
        Self::ShowAllModels,
        Self::Format,
        Self::Repository,
        Self::ReviewRepository,
        Self::ReviewMlx,
        Self::MlxFiles,
        Self::DownloadMlx,
        Self::FindPausedMlx,
        Self::ResumeMlx,
        Self::DiscardPausedMlx,
        Self::Artifact,
        Self::Purpose,
        Self::Download,
        Self::FindPaused,
        Self::DiscardPaused,
        Self::KeepPaused,
        Self::Resume,
        Self::Pause,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Access => "Hub access",
            Self::RecommendationRole => "Recommended model role",
            Self::RecommendedModel => "Choose a recommended model",
            Self::ReviewRecommendation => "Review recommended model",
            Self::Query => "Search Hugging Face models",
            Self::Search => "Search Hugging Face",
            Self::LoadMore => "Load more search results",
            Self::ShowAllModels => "Show all models or filter by RAM fit",
            Self::Format => "Model format: GGUF or MLX",
            Self::Repository => "Choose a GGUF repository",
            Self::ReviewRepository => "Review repository",
            Self::ReviewMlx => "Review MLX package",
            Self::MlxFiles => "Reviewed MLX package files and checksums",
            Self::DownloadMlx => "Download and register MLX model",
            Self::FindPausedMlx => "Find paused MLX download",
            Self::ResumeMlx => "Resume reviewed MLX download",
            Self::DiscardPausedMlx => "Discard paused MLX download",
            Self::Artifact => "Choose a reviewed GGUF file",
            Self::Purpose => "Model purpose",
            Self::Download => "Download and register reviewed model",
            Self::FindPaused => "Find paused download",
            Self::DiscardPaused => "Discard paused download",
            Self::KeepPaused => "Keep download",
            Self::Resume => "Resume reviewed download",
            Self::Pause => "Pause download",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Control {
    LocalSetup,
    LocalEndpoint,
    RefreshModels,
    InstalledModel,
    TestModel,
    InstallStableRuntime,
    UpdateStableRuntime,
    RuntimeVersionsEntry,
    OpenSignedPackage,
    HideSignedPackage,
    SignedIndex,
    IndexSignature,
    RuntimePackage,
    InstallRuntime,
    CheckRuntimeOnline,
    RuntimeOnlineChoice,
    DownloadRuntimeOnline,
    PauseRuntimeDownload,
    CancelRuntimeRequest,
    DiscardRuntimeDownload,
    KeepRuntimeDownload,
    CloseRuntimeRepair,
    RemoveRuntime,
    ConfirmRuntimeRemoval,
    CancelRuntimeRemoval,
    MemoryEnabled,
    Learning,
    Budget,
    Scope,
    ReloadMemory,
    Entries,
    MemoryMarkdown,
    Archive,
    History,
    CleanMemory,
    ClearMemory,
    ExternalMemory,
    UseImportedMemoryActivation,
    UseImportedMemoryLearning,
    UseImportedMemoryBudget,
    FindMemoryMarkdown,
    SaveMemoryMarkdown,
    SaveMemoryEntry,
    CancelMemoryEntry,
    RestoreMemoryVersion,
    MemoryReferencePath,
    ReviewMemoryReference,
    ApproveMemoryReference,
    BackFromMemoryView,
    AddSkill,
    ImportSkill,
    ExportSkill,
    ImportExternal,
    ExternalUpdates,
    SkillsEnabled,
    SkillAutoSelect,
    LearnInSkills,
    ShowHiddenSkills,
    SkillUsage,
    SkillStatistics,
    SkillName,
    SkillDescription,
    SkillTags,
    FindSkill,
    ApplySkill,
    RevertSkill,
    ShortenSkill,
    SkillLibraryQuery,
    SkillLibrarySort,
    DeleteSkill,
    SkillProviders,
    SkillActive,
    SkillTarget,
    SkillMarkdownContent,
    SkillLearnedRules,
    RestoreBundledSkill,
    ToggleBundledSkill,
    SkillImported(super::skills::Preference),
}

impl Control {
    const ALL: [Self; 81] = [
        Self::LocalSetup,
        Self::LocalEndpoint,
        Self::RefreshModels,
        Self::InstalledModel,
        Self::TestModel,
        Self::InstallStableRuntime,
        Self::UpdateStableRuntime,
        Self::RuntimeVersionsEntry,
        Self::OpenSignedPackage,
        Self::HideSignedPackage,
        Self::SignedIndex,
        Self::IndexSignature,
        Self::RuntimePackage,
        Self::InstallRuntime,
        Self::CheckRuntimeOnline,
        Self::RuntimeOnlineChoice,
        Self::DownloadRuntimeOnline,
        Self::PauseRuntimeDownload,
        Self::CancelRuntimeRequest,
        Self::DiscardRuntimeDownload,
        Self::KeepRuntimeDownload,
        Self::CloseRuntimeRepair,
        Self::RemoveRuntime,
        Self::ConfirmRuntimeRemoval,
        Self::CancelRuntimeRemoval,
        Self::MemoryEnabled,
        Self::Learning,
        Self::Budget,
        Self::Scope,
        Self::ReloadMemory,
        Self::Entries,
        Self::MemoryMarkdown,
        Self::Archive,
        Self::History,
        Self::CleanMemory,
        Self::ClearMemory,
        Self::ExternalMemory,
        Self::UseImportedMemoryActivation,
        Self::UseImportedMemoryLearning,
        Self::UseImportedMemoryBudget,
        Self::FindMemoryMarkdown,
        Self::SaveMemoryMarkdown,
        Self::SaveMemoryEntry,
        Self::CancelMemoryEntry,
        Self::RestoreMemoryVersion,
        Self::MemoryReferencePath,
        Self::ReviewMemoryReference,
        Self::ApproveMemoryReference,
        Self::BackFromMemoryView,
        Self::AddSkill,
        Self::ImportSkill,
        Self::ExportSkill,
        Self::ImportExternal,
        Self::ExternalUpdates,
        Self::SkillName,
        Self::SkillDescription,
        Self::SkillTags,
        Self::SkillsEnabled,
        Self::SkillAutoSelect,
        Self::LearnInSkills,
        Self::ShowHiddenSkills,
        Self::SkillUsage,
        Self::SkillStatistics,
        Self::FindSkill,
        Self::ApplySkill,
        Self::RevertSkill,
        Self::ShortenSkill,
        Self::SkillLibraryQuery,
        Self::SkillLibrarySort,
        Self::DeleteSkill,
        Self::SkillProviders,
        Self::SkillActive,
        Self::SkillTarget,
        Self::SkillMarkdownContent,
        Self::SkillLearnedRules,
        Self::RestoreBundledSkill,
        Self::ToggleBundledSkill,
        Self::SkillImported(super::skills::Preference::Enabled),
        Self::SkillImported(super::skills::Preference::AutoDetect),
        Self::SkillImported(super::skills::Preference::LearnedRules),
        Self::SkillImported(super::skills::Preference::Statistics),
    ];

    pub(super) fn page(self) -> Page {
        match self {
            Self::LocalSetup
            | Self::LocalEndpoint
            | Self::RefreshModels
            | Self::InstalledModel
            | Self::TestModel
            | Self::InstallStableRuntime
            | Self::UpdateStableRuntime
            | Self::RuntimeVersionsEntry
            | Self::OpenSignedPackage
            | Self::HideSignedPackage
            | Self::SignedIndex
            | Self::IndexSignature
            | Self::RuntimePackage
            | Self::InstallRuntime
            | Self::CheckRuntimeOnline
            | Self::RuntimeOnlineChoice
            | Self::DownloadRuntimeOnline
            | Self::PauseRuntimeDownload
            | Self::CancelRuntimeRequest
            | Self::DiscardRuntimeDownload
            | Self::KeepRuntimeDownload
            | Self::CloseRuntimeRepair
            | Self::RemoveRuntime
            | Self::ConfirmRuntimeRemoval
            | Self::CancelRuntimeRemoval => Page::LocalModels,
            Self::MemoryEnabled
            | Self::Learning
            | Self::Budget
            | Self::Scope
            | Self::ReloadMemory
            | Self::Entries
            | Self::MemoryMarkdown
            | Self::Archive
            | Self::History
            | Self::CleanMemory
            | Self::ClearMemory
            | Self::ExternalMemory
            | Self::UseImportedMemoryActivation
            | Self::UseImportedMemoryLearning
            | Self::UseImportedMemoryBudget
            | Self::FindMemoryMarkdown
            | Self::SaveMemoryMarkdown
            | Self::SaveMemoryEntry
            | Self::CancelMemoryEntry
            | Self::RestoreMemoryVersion
            | Self::MemoryReferencePath
            | Self::ReviewMemoryReference
            | Self::ApproveMemoryReference
            | Self::BackFromMemoryView => Page::Memory,
            Self::AddSkill
            | Self::ImportSkill
            | Self::ExportSkill
            | Self::ImportExternal
            | Self::ExternalUpdates
            | Self::SkillsEnabled
            | Self::SkillAutoSelect
            | Self::LearnInSkills
            | Self::ShowHiddenSkills
            | Self::SkillUsage
            | Self::SkillStatistics
            | Self::SkillName
            | Self::SkillDescription
            | Self::SkillTags
            | Self::FindSkill
            | Self::ApplySkill
            | Self::RevertSkill
            | Self::ShortenSkill
            | Self::SkillLibraryQuery
            | Self::SkillLibrarySort
            | Self::DeleteSkill
            | Self::SkillProviders
            | Self::SkillActive
            | Self::SkillTarget
            | Self::SkillMarkdownContent
            | Self::SkillLearnedRules
            | Self::RestoreBundledSkill
            | Self::ToggleBundledSkill
            | Self::SkillImported(_) => Page::Skills,
        }
    }

    pub(super) fn runtime_progress(self) -> bool {
        matches!(
            self,
            Self::PauseRuntimeDownload | Self::CancelRuntimeRequest
        )
    }

    pub(super) fn contextual_runtime(self) -> bool {
        self.runtime_progress()
            || self.runtime_toolbar()
            || matches!(
                self,
                Self::DiscardRuntimeDownload
                    | Self::KeepRuntimeDownload
                    | Self::RemoveRuntime
                    | Self::ConfirmRuntimeRemoval
                    | Self::CancelRuntimeRemoval
            )
    }

    pub(super) fn runtime_toolbar(self) -> bool {
        matches!(
            self,
            Self::InstallStableRuntime
                | Self::UpdateStableRuntime
                | Self::RuntimeVersionsEntry
                | Self::OpenSignedPackage
                | Self::HideSignedPackage
        )
    }

    pub(super) fn contextual_memory(self) -> bool {
        matches!(
            self,
            Self::UseImportedMemoryActivation
                | Self::UseImportedMemoryLearning
                | Self::UseImportedMemoryBudget
                | Self::FindMemoryMarkdown
                | Self::SaveMemoryMarkdown
                | Self::SaveMemoryEntry
                | Self::CancelMemoryEntry
                | Self::RestoreMemoryVersion
                | Self::MemoryReferencePath
                | Self::ReviewMemoryReference
                | Self::ApproveMemoryReference
                | Self::BackFromMemoryView
        )
    }

    pub(super) fn contextual_skill(self) -> bool {
        matches!(
            self,
            Self::SkillLibraryQuery
                | Self::SkillLibrarySort
                | Self::DeleteSkill
                | Self::SkillProviders
                | Self::SkillActive
                | Self::SkillTarget
                | Self::SkillMarkdownContent
                | Self::SkillLearnedRules
                | Self::RestoreBundledSkill
                | Self::ToggleBundledSkill
                | Self::SkillImported(_)
        )
    }

    pub(super) fn skill_form(self) -> bool {
        matches!(
            self,
            Self::SkillName
                | Self::SkillDescription
                | Self::SkillTags
                | Self::FindSkill
                | Self::ApplySkill
                | Self::RevertSkill
                | Self::ShortenSkill
                | Self::SkillActive
                | Self::SkillTarget
                | Self::SkillMarkdownContent
                | Self::SkillLearnedRules
                | Self::RestoreBundledSkill
                | Self::ToggleBundledSkill
        )
    }

    fn title(self) -> &'static str {
        match self {
            Self::LocalSetup => "Set up local AI",
            Self::LocalEndpoint => "Local endpoint",
            Self::RefreshModels => "Refresh installed models",
            Self::InstalledModel => "Select an installed model",
            Self::TestModel => "Test model",
            Self::InstallStableRuntime => "Install runtime…",
            Self::UpdateStableRuntime => "Update runtime…",
            Self::RuntimeVersionsEntry => "Other runtime versions…",
            Self::OpenSignedPackage => "Import signed package…",
            Self::HideSignedPackage => "Hide package import",
            Self::SignedIndex => "Signed index",
            Self::IndexSignature => "Index signature",
            Self::RuntimePackage => "Runtime package",
            Self::InstallRuntime => "Install signed runtime",
            Self::CheckRuntimeOnline => "Check signed runtime updates",
            Self::RuntimeOnlineChoice => "Choose a verified runtime package",
            Self::DownloadRuntimeOnline => "Download or resume selected runtime",
            Self::PauseRuntimeDownload => "Pause runtime download",
            Self::CancelRuntimeRequest => "Cancel runtime request",
            Self::DiscardRuntimeDownload => "Discard saved runtime download",
            Self::KeepRuntimeDownload => "Keep saved ZIP",
            Self::CloseRuntimeRepair => "Cancel repair or return to model",
            Self::RemoveRuntime => "Remove installed runtime",
            Self::ConfirmRuntimeRemoval => "Remove runtime after review",
            Self::CancelRuntimeRemoval => "Cancel runtime removal",
            Self::MemoryEnabled => "Enable AI memory",
            Self::Learning => "Learning",
            Self::Budget => "Token budget",
            Self::Scope => "Scope",
            Self::ReloadMemory => "Reload",
            Self::Entries => "Entries",
            Self::MemoryMarkdown => "Markdown",
            Self::Archive => "Archive",
            Self::History => "History",
            Self::CleanMemory => "Clean up",
            Self::ClearMemory => "Clear",
            Self::ExternalMemory => "External memory files",
            Self::UseImportedMemoryActivation => "Use imported memory activation preference",
            Self::UseImportedMemoryLearning => "Use imported memory learning preference",
            Self::UseImportedMemoryBudget => "Use imported memory token budget",
            Self::FindMemoryMarkdown => "Find in memory Markdown",
            Self::SaveMemoryMarkdown => "Save memory Markdown",
            Self::SaveMemoryEntry => "Save edited memory entry",
            Self::CancelMemoryEntry => "Cancel memory entry edit",
            Self::RestoreMemoryVersion => "Restore selected memory history version",
            Self::MemoryReferencePath => "External memory file path",
            Self::ReviewMemoryReference => "Review external memory file",
            Self::ApproveMemoryReference => "Approve reviewed external memory file version",
            Self::BackFromMemoryView => {
                "Back from memory archive, history, cleanup or external files"
            }
            Self::AddSkill => "Add skill",
            Self::ImportSkill => "Import Markdown",
            Self::ExportSkill => "Export Markdown",
            Self::ImportExternal => "Import external skills",
            Self::ExternalUpdates => "Check external updates",
            Self::SkillsEnabled => "Enable AI skills",
            Self::SkillAutoSelect => "Automatically select relevant skills",
            Self::LearnInSkills => "Allow saving learned rules in skills",
            Self::ShowHiddenSkills => "Show hidden",
            Self::SkillUsage => "Record local skill statistics",
            Self::SkillStatistics => "Skill statistics",
            Self::SkillName => "Skill name",
            Self::SkillDescription => "Description",
            Self::SkillTags => "Tags",
            Self::FindSkill => "Find in skill",
            Self::ApplySkill => "Apply skill edits",
            Self::RevertSkill => "Revert edits",
            Self::ShortenSkill => "Shorten with local AI",
            Self::SkillLibraryQuery => "Search skills in the library",
            Self::SkillLibrarySort => "Sort the skill library",
            Self::DeleteSkill => "Delete selected skill",
            Self::SkillProviders => "External skill providers",
            Self::SkillActive => "Enable selected skill",
            Self::SkillTarget => "Skill target (Chat, Agent, Both, Connection)",
            Self::SkillMarkdownContent => "Edit skill Markdown",
            Self::SkillLearnedRules => "Edit learned rules for the skill",
            Self::RestoreBundledSkill => "Restore bundled skill version",
            Self::ToggleBundledSkill => "Hide or unhide bundled skill",
            Self::SkillImported(preference) => match preference {
                super::skills::Preference::Enabled => "Use imported skill activation preference",
                super::skills::Preference::AutoDetect => {
                    "Use imported automatic skill selection preference"
                }
                super::skills::Preference::LearnedRules => {
                    "Use imported learned skill rules preference"
                }
                super::skills::Preference::Statistics => "Use imported skill statistics preference",
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnowledgeSetting {
    Inventory(InventoryAction),
    Name,
    Create,
    Update,
    Rebuild,
    VectorStorage,
    LocalDimensions,
    QdrantEndpoint,
    QdrantCollection,
    QdrantDimensions,
    QdrantAuthentication,
    QdrantCredentialVariable,
    SourcePath,
    AddFile,
    FileSize,
    ApplyFileSize,
    CancelFileSize,
    ReviewFolder,
    FolderReviewSummary,
    FolderReviewPrevious,
    FolderReviewNext,
    FolderReviewApprove,
    FolderReviewDiscard,
    FolderSync,
    FolderRecursive,
    FolderGitIgnore,
    FolderPdf,
    FolderFileLimit,
    FolderIncludes,
    FolderExcludes,
    EmbeddingSource,
    EmbeddingEndpoint,
    EmbeddingModel,
    EmbeddingConsent,
    AutomaticEmbeddingApproval,
    AutomaticQdrantCapacity,
    TerminalChatProfile,
    CodeAnalysisProfile,
    AgentKnowledge,
    TestQuery,
    TestSearch,
    ReviewStoreRemoval,
    ConfirmStoreRemoval,
    CancelStoreRemoval,
}

impl KnowledgeSetting {
    const ALL: [Self; 43] = [
        Self::Name,
        Self::Create,
        Self::Update,
        Self::Rebuild,
        Self::VectorStorage,
        Self::LocalDimensions,
        Self::QdrantEndpoint,
        Self::QdrantCollection,
        Self::QdrantDimensions,
        Self::QdrantAuthentication,
        Self::QdrantCredentialVariable,
        Self::SourcePath,
        Self::AddFile,
        Self::FileSize,
        Self::ApplyFileSize,
        Self::CancelFileSize,
        Self::ReviewFolder,
        Self::FolderReviewSummary,
        Self::FolderReviewPrevious,
        Self::FolderReviewNext,
        Self::FolderReviewApprove,
        Self::FolderReviewDiscard,
        Self::FolderSync,
        Self::FolderRecursive,
        Self::FolderGitIgnore,
        Self::FolderPdf,
        Self::FolderFileLimit,
        Self::FolderIncludes,
        Self::FolderExcludes,
        Self::EmbeddingSource,
        Self::EmbeddingEndpoint,
        Self::EmbeddingModel,
        Self::EmbeddingConsent,
        Self::AutomaticEmbeddingApproval,
        Self::AutomaticQdrantCapacity,
        Self::TerminalChatProfile,
        Self::CodeAnalysisProfile,
        Self::AgentKnowledge,
        Self::TestQuery,
        Self::TestSearch,
        Self::ReviewStoreRemoval,
        Self::ConfirmStoreRemoval,
        Self::CancelStoreRemoval,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Inventory(action) => action.title(),
            Self::Name => "New store name",
            Self::Create => "Create store",
            Self::Update => "Update index",
            Self::Rebuild => "Rebuild index",
            Self::VectorStorage => "Vector storage backend",
            Self::LocalDimensions => "Local embedding dimensions",
            Self::QdrantEndpoint => "Qdrant endpoint",
            Self::QdrantCollection => "Qdrant collection",
            Self::QdrantDimensions => "Qdrant embedding dimensions",
            Self::QdrantAuthentication => "Qdrant authentication",
            Self::QdrantCredentialVariable => "Qdrant API key variable",
            Self::SourcePath => "Source file or folder path",
            Self::AddFile => "Add source file",
            Self::FileSize => "Source file size limit",
            Self::ApplyFileSize => "Apply source file size limit",
            Self::CancelFileSize => "Cancel source file size limit edit",
            Self::ReviewFolder => "Review source folder",
            Self::FolderReviewSummary => "Folder review summary",
            Self::FolderReviewPrevious => "Previous folder review page",
            Self::FolderReviewNext => "Next folder review page",
            Self::FolderReviewApprove => "Approve folder files",
            Self::FolderReviewDiscard => "Discard folder review",
            Self::FolderSync => "Folder sync mode",
            Self::FolderRecursive => "Include subfolders",
            Self::FolderGitIgnore => "Respect .gitignore",
            Self::FolderPdf => "Include PDFs with a text layer",
            Self::FolderFileLimit => "Folder per-file size limit",
            Self::FolderIncludes => "Folder include globs",
            Self::FolderExcludes => "Folder exclude globs",
            Self::EmbeddingSource => "Embedding source",
            Self::EmbeddingEndpoint => "Local embedding endpoint",
            Self::EmbeddingModel => "Embedding model",
            Self::EmbeddingConsent => "Allow embedding source excerpts and searches",
            Self::AutomaticEmbeddingApproval => "Approve automatic embedding destination",
            Self::AutomaticQdrantCapacity => "Review automatic Qdrant capacity",
            Self::TerminalChatProfile => "Offer this store for terminal AI chat",
            Self::CodeAnalysisProfile => "Offer this store for code analysis",
            Self::AgentKnowledge => {
                "Offer this store for Agent, Planning, Swarm, and scheduled AI requests"
            }
            Self::TestQuery => "Test search query",
            Self::TestSearch => "Run knowledge search",
            Self::ReviewStoreRemoval => "Review removal of the knowledge store",
            Self::ConfirmStoreRemoval => "Confirm removal of the knowledge store",
            Self::CancelStoreRemoval => "Cancel removal of the knowledge store",
        }
    }

    pub(super) fn needs_store(self) -> bool {
        !matches!(self, Self::Name | Self::Create | Self::Inventory(_))
    }

    pub(super) fn store_settings(self) -> bool {
        self.advanced_store_settings()
            || matches!(
                self,
                Self::Name
                    | Self::EmbeddingSource
                    | Self::EmbeddingEndpoint
                    | Self::EmbeddingModel
                    | Self::TerminalChatProfile
                    | Self::CodeAnalysisProfile
                    | Self::AgentKnowledge
            )
    }

    pub(super) fn advanced_store_settings(self) -> bool {
        matches!(
            self,
            Self::VectorStorage
                | Self::LocalDimensions
                | Self::QdrantEndpoint
                | Self::QdrantCollection
                | Self::QdrantDimensions
                | Self::QdrantAuthentication
                | Self::QdrantCredentialVariable
                | Self::EmbeddingEndpoint
                | Self::EmbeddingModel
        )
    }

    pub(super) fn needs_active_review(self) -> bool {
        matches!(
            self,
            Self::FolderReviewSummary
                | Self::FolderReviewPrevious
                | Self::FolderReviewNext
                | Self::FolderReviewApprove
                | Self::FolderReviewDiscard
                | Self::FileSize
                | Self::ApplyFileSize
                | Self::CancelFileSize
                | Self::ConfirmStoreRemoval
                | Self::CancelStoreRemoval
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InventoryAction {
    ConfigureStore,
    OpenTestSearch,
    Refresh,
    AddFiles,
    AddFolder,
    CancelIndexing,
    SyncMode,
    AdvancedSource,
    ToggleSource,
    RemoveSources,
}

impl InventoryAction {
    const ALL: [Self; 10] = [
        Self::ConfigureStore,
        Self::OpenTestSearch,
        Self::Refresh,
        Self::AddFiles,
        Self::AddFolder,
        Self::CancelIndexing,
        Self::SyncMode,
        Self::AdvancedSource,
        Self::ToggleSource,
        Self::RemoveSources,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::ConfigureStore => "Configure knowledge store",
            Self::OpenTestSearch => "Test search",
            Self::Refresh => "Refresh knowledge inventory",
            Self::AddFiles => "Add files…",
            Self::AddFolder => "Add folder…",
            Self::CancelIndexing => "Cancel indexing",
            Self::SyncMode => "Sync mode…",
            Self::AdvancedSource => "Advanced source settings",
            Self::ToggleSource => "Enable / disable sources",
            Self::RemoveSources => "Remove sources…",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::ConfigureStore => "settings edit",
            Self::OpenTestSearch => "retrieval",
            Self::Refresh => "reload source statuses",
            Self::AddFiles => "add source files picker",
            Self::AddFolder => "add source directory picker",
            Self::CancelIndexing => "cancel source update scanning",
            Self::SyncMode => "change source synchronization manual automatic",
            Self::AdvancedSource => "advanced folder file options",
            Self::ToggleSource => "toggle active enabled disabled",
            Self::RemoveSources => "delete selected source",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfileSetting {
    Name,
    Default,
    Connection,
    CloudProvider,
    CliProvider,
    CliExecutable,
    CliModel,
    CliPresets,
    CliArguments,
    CliAddArgument,
    CliReasoning,
    CliReview,
    MiniMaxModule,
    MiniMaxConfig,
    MiniMaxRegion,
    Endpoint,
    Trust,
    Credential,
    ModelMode,
    Model,
    AvailableModels,
    ManagedModel,
    Reasoning,
    Vision,
    PromptPreset,
    SelectionLimit,
    Timeout,
    OutputLimit,
    Tokenizer,
    ReviewedAgentHttps,
    ReviewedPlanningHttps,
    Usage(profiles::UsageField),
    RecordedUsage,
    Web(WebSetting),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WebSetting {
    Provider,
    Plugin,
    ToolpackNames,
    ImportedToolpackNames,
    UseImported,
    SearxngPages,
    SearxngEndpoint,
    SearxngBearerEnv,
}

impl WebSetting {
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Provider => "Web search provider",
            Self::Plugin => "Installed MCP plugin ID",
            Self::ToolpackNames => "Allowed Toolpack names",
            Self::ImportedToolpackNames => "Imported Toolpack names",
            Self::UseImported => "Use imported web integration",
            Self::SearxngPages => "Read webpages through SearXNG MCP",
            Self::SearxngEndpoint => "SearXNG MCP endpoint URL",
            Self::SearxngBearerEnv => "SearXNG bearer token environment variable",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Provider => "internet Tavily direct",
            Self::Plugin => "LM Studio MCP server name",
            Self::ToolpackNames | Self::ImportedToolpackNames => "allowed tools plugin",
            Self::UseImported => "Java backup review Toolpack",
            Self::SearxngPages => "page extraction webpage reader",
            Self::SearxngEndpoint => "page URL web_url_read",
            Self::SearxngBearerEnv => "optional credential variable",
        }
    }
}

impl ProfileSetting {
    pub(super) const ALL: &[Self] = &[
        Self::Name,
        Self::Default,
        Self::CloudProvider,
        Self::CliProvider,
        Self::CliExecutable,
        Self::CliModel,
        Self::CliPresets,
        Self::CliArguments,
        Self::CliAddArgument,
        Self::CliReasoning,
        Self::CliReview,
        Self::MiniMaxModule,
        Self::MiniMaxConfig,
        Self::MiniMaxRegion,
        Self::Connection,
        Self::Endpoint,
        Self::Trust,
        Self::Credential,
        Self::ModelMode,
        Self::Model,
        Self::AvailableModels,
        Self::ManagedModel,
        Self::Reasoning,
        Self::Vision,
        Self::PromptPreset,
        Self::SelectionLimit,
        Self::Timeout,
        Self::OutputLimit,
        Self::Tokenizer,
        Self::ReviewedAgentHttps,
        Self::ReviewedPlanningHttps,
        Self::Usage(profiles::UsageField::Limit),
        Self::Usage(profiles::UsageField::Days),
        Self::Usage(profiles::UsageField::Yellow),
        Self::Usage(profiles::UsageField::Red),
        Self::Usage(profiles::UsageField::InputPrice),
        Self::Usage(profiles::UsageField::OutputPrice),
        Self::Usage(profiles::UsageField::Currency),
        Self::RecordedUsage,
        Self::Web(WebSetting::Provider),
        Self::Web(WebSetting::Plugin),
        Self::Web(WebSetting::ToolpackNames),
        Self::Web(WebSetting::ImportedToolpackNames),
        Self::Web(WebSetting::UseImported),
        Self::Web(WebSetting::SearxngPages),
        Self::Web(WebSetting::SearxngEndpoint),
        Self::Web(WebSetting::SearxngBearerEnv),
    ];

    pub(super) fn section(self) -> profiles::Section {
        match self {
            Self::Name
            | Self::Default
            | Self::Connection
            | Self::Endpoint
            | Self::Trust
            | Self::Credential
            | Self::CloudProvider
            | Self::CliProvider
            | Self::CliExecutable
            | Self::CliModel
            | Self::CliPresets
            | Self::CliArguments
            | Self::CliAddArgument
            | Self::CliReasoning
            | Self::CliReview
            | Self::MiniMaxModule
            | Self::MiniMaxConfig
            | Self::MiniMaxRegion => profiles::Section::Connection,
            Self::ModelMode
            | Self::Model
            | Self::AvailableModels
            | Self::ManagedModel
            | Self::Reasoning
            | Self::Vision
            | Self::PromptPreset
            | Self::Web(_) => profiles::Section::Model,
            Self::SelectionLimit
            | Self::Timeout
            | Self::OutputLimit
            | Self::ReviewedAgentHttps
            | Self::ReviewedPlanningHttps => profiles::Section::Limits,
            Self::Tokenizer | Self::Usage(_) | Self::RecordedUsage => profiles::Section::Budget,
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Name => "Profile name",
            Self::Default => "Use as default profile",
            Self::Connection => "Connection type",
            Self::CloudProvider => "Cloud provider preset",
            Self::CliProvider => "CLI provider",
            Self::CliExecutable => "CLI executable path",
            Self::CliModel => "CLI model identifier",
            Self::CliPresets => "CLI command presets",
            Self::CliArguments => "CLI arguments",
            Self::CliAddArgument => "Add a CLI argument",
            Self::CliReasoning => "CLI reasoning effort",
            Self::CliReview => "Review CLI executable and arguments",
            Self::MiniMaxModule => "MiniMax CLI module path",
            Self::MiniMaxConfig => "MiniMax CLI config path",
            Self::MiniMaxRegion => "MiniMax CLI region",
            Self::Endpoint => "API endpoint",
            Self::Trust => "Endpoint trust",
            Self::Credential => "API key variable",
            Self::ModelMode => "Model selection",
            Self::Model => "Model identifier",
            Self::AvailableModels => "Available models",
            Self::ManagedModel => "Managed model",
            Self::Reasoning => "Reasoning",
            Self::Vision => "Vision",
            Self::PromptPreset => "Model prompt preset",
            Self::SelectionLimit => "Max. selection characters",
            Self::Timeout => "Profile timeout (min)",
            Self::OutputLimit => "Max. output tokens",
            Self::Tokenizer => "Forecast tokenizer",
            Self::ReviewedAgentHttps => "Allow reviewed Agent requests over HTTPS",
            Self::ReviewedPlanningHttps => "Allow reviewed AI Planning requests over HTTPS",
            Self::Usage(field) => field.label(),
            Self::RecordedUsage => "Refresh recorded usage",
            Self::Web(setting) => setting.title(),
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Connection => "provider HTTP llama.cpp CLI transport",
            Self::CloudProvider => {
                "OpenAI Anthropic Gemini Mistral DeepSeek Groq OpenRouter MiniMax"
            }
            Self::CliExecutable => "Node.js command binary program",
            Self::CliPresets => "LM Studio Codex MiniMax stdin loaded model",
            Self::MiniMaxModule => "dist/mmx.mjs package",
            Self::MiniMaxConfig => "config.json OAuth login",
            Self::CliReview => "approve command",
            Self::Endpoint => {
                "URL connection OpenAI Anthropic Gemini Mistral DeepSeek Groq OpenRouter MiniMax"
            }
            Self::Trust => "trusted local endpoint",
            Self::Credential => "credential environment connection",
            Self::AvailableModels => "discovery list suggested examples",
            Self::ManagedModel => "GGUF llama.cpp",
            Self::Vision => "image input",
            Self::PromptPreset => {
                "model family Llama Qwen Mistral Mixtral Gemma DeepSeek Phi GPT-OSS"
            }
            Self::SelectionLimit | Self::Timeout | Self::OutputLimit => "limits timeouts",
            Self::Tokenizer => "token count estimate CL100K O200K P50K R50K",
            Self::ReviewedAgentHttps => "Terminal Agent Ask Run Swarm Scheduler permission",
            Self::ReviewedPlanningHttps => "plan request permission",
            Self::Usage(_) | Self::RecordedUsage => "token budget cost quota",
            Self::Web(setting) => setting.keywords(),
            _ => "",
        }
    }

    pub(super) fn visible(self, profile: &profiles::Profile) -> bool {
        if profile.policy_definition().is_some() {
            return matches!(
                self,
                Self::Name
                    | Self::Connection
                    | Self::Endpoint
                    | Self::Credential
                    | Self::Model
                    | Self::Default
                    | Self::RecordedUsage
            ) || (self == Self::AvailableModels
                && !profile.is_managed()
                && profile.policy_auto_candidate().is_none()
                && profile.model.is_empty());
        }
        if profile.is_cli() {
            return matches!(
                self,
                Self::Name
                    | Self::Connection
                    | Self::SelectionLimit
                    | Self::Timeout
                    | Self::PromptPreset
            ) || profile.searchable_cli_setting(self);
        }
        match self {
            Self::Endpoint
            | Self::Trust
            | Self::Credential
            | Self::ModelMode
            | Self::AvailableModels
            | Self::Vision => !profile.is_managed(),
            Self::Model => {
                !profile.is_managed() && profile.model_mode == profiles::ModelMode::Manual
            }
            Self::ManagedModel => profile.is_managed(),
            Self::CloudProvider => profile.has_cloud_provider_presets(),
            Self::ReviewedAgentHttps => profile.remote_protocol().is_some(),
            Self::ReviewedPlanningHttps => profile.supports_reviewed_planning_https(),
            Self::Web(setting) => profile.searchable_web_setting(setting),
            Self::CliProvider
            | Self::CliExecutable
            | Self::CliModel
            | Self::CliPresets
            | Self::CliArguments
            | Self::CliAddArgument
            | Self::CliReasoning
            | Self::CliReview
            | Self::MiniMaxModule
            | Self::MiniMaxConfig
            | Self::MiniMaxRegion => false,
            _ => true,
        }
    }
}

pub(super) fn targets(profile: Option<&profiles::Profile>) -> impl Iterator<Item = Target> {
    let targets = [
        Setting::Features,
        Setting::ConfirmBeforeSend,
        Setting::AgentDebugMessages,
        Setting::AgentRuntimeMessages,
        Setting::JournalSemanticSearch,
        Setting::GlobalTimeout,
        Setting::SecurityProfile,
        Setting::ClearSecurityProfile,
        Setting::AgentHistoryLimit,
        Setting::SnippetInstructions,
        Setting::SnippetAlternatives,
        Setting::CodeTextLanguage,
        Setting::ChatTerminalActions,
        Setting::AgentShortcut,
        Setting::AgentShortcutName,
        Setting::AgentShortcutCase,
        Setting::AgentTriage,
        Setting::AgentRunPresentation,
        Setting::AgentExecution,
        Setting::AgentConfirmation,
        Setting::ShowAgentSetup,
    ]
    .into_iter()
    .chain(
        super::internet::Field::ALL
            .into_iter()
            .map(Setting::Internet),
    )
    .chain(
        ImportedGeneral::ALL
            .into_iter()
            .map(Setting::ImportedGeneral),
    )
    .chain(
        ProfileSetting::ALL
            .iter()
            .copied()
            .filter(move |setting| profile.is_some_and(|profile| setting.visible(profile)))
            .map(Setting::Profile),
    )
    .chain(ProfileAction::ALL.into_iter().map(Setting::ProfileAction))
    .chain(Role::ALL.into_iter().map(Setting::Role))
    .chain([Setting::Embedding])
    .chain(
        LocalPreference::CONTROLS
            .into_iter()
            .chain(
                super::java_roles::Role::ALL
                    .into_iter()
                    .map(LocalPreference::ImportedRole),
            )
            .map(Setting::LocalPreference),
    )
    .chain(
        KnowledgeSetting::ALL
            .into_iter()
            .chain(
                InventoryAction::ALL
                    .into_iter()
                    .map(KnowledgeSetting::Inventory),
            )
            .map(Setting::Knowledge),
    )
    .chain(HubSetting::ALL.into_iter().map(Setting::Hub))
    .chain(ManagedSetting::ALL.into_iter().map(Setting::Managed))
    .chain(MlxSetting::ALL.into_iter().map(Setting::Mlx))
    .chain(
        SkillProviderSetting::ALL
            .into_iter()
            .map(Setting::SkillProvider),
    )
    .chain(Control::ALL.into_iter().map(Setting::Control))
    .map(Target::Setting);
    Page::ALL.into_iter().map(Target::Page).chain(targets)
}

pub(super) fn matching(
    query: &str,
    targets: impl Iterator<Item = Target>,
    hints: &java_models::Hints,
) -> std::vec::IntoIter<Target> {
    if !settings_search::has_query(query) {
        return Vec::new().into_iter();
    }
    let mut targets: Vec<_> = targets.collect();
    targets.sort_by_key(|target| Page::ALL.iter().position(|page| *page == target.page()));
    settings_search::rank(
        query,
        targets.into_iter().map(|target| {
            let detail = match target {
                Target::Page(page) => page.keywords(),
                Target::Setting(setting) => setting.keywords(),
            };
            let identity = match target {
                Target::Setting(Setting::ImportedJavaModel(key)) => {
                    hints.search_hint(key).map(|hint| hint.id.as_str())
                }
                _ => None,
            };
            Candidate {
                label: target.label(hints),
                detail,
                page: target.page().title(),
                is_page: matches!(target, Target::Page(_)),
                identity,
                target,
            }
        }),
    )
}

pub(super) fn imported_models(hints: &java_models::Hints) -> impl Iterator<Item = Target> {
    hints
        .iter()
        .enumerate()
        .map(|(index, hint)| Target::Setting(Setting::ImportedJavaModel(hint.search_key(index))))
}

#[cfg(test)]
pub(super) fn results(
    query: &str,
    profile: Option<&profiles::Profile>,
) -> impl Iterator<Item = Target> {
    matching(query, targets(profile), &java_models::Hints::default())
}

pub(super) struct Reveal {
    pub(super) setting: Option<Setting>,
    pub(super) viewport: Id,
    row: Id,
    input: Id,
}

impl Default for Reveal {
    fn default() -> Self {
        Self {
            setting: None,
            viewport: Id::unique(),
            row: Id::unique(),
            input: Id::unique(),
        }
    }
}

impl Reveal {
    pub(super) fn inventory_action<'a, Message: 'a>(
        &self,
        action: InventoryAction,
        content: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        self.action(
            Setting::Knowledge(KnowledgeSetting::Inventory(action)),
            content,
        )
    }

    pub(super) fn knowledge_input<'a, Message: Clone + 'a>(
        &self,
        setting: KnowledgeSetting,
        editable: bool,
        input: iced::widget::TextInput<'a, Message>,
    ) -> Element<'a, Message> {
        let setting = Setting::Knowledge(setting);
        self.wrap(setting, self.input(setting, editable, input))
    }

    pub(super) fn wrap<'a, Message: 'a>(
        &self,
        setting: Setting,
        content: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        self.wrap_width(setting, content, Length::Fill)
    }

    pub(super) fn action<'a, Message: 'a>(
        &self,
        setting: Setting,
        content: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        self.wrap_width(setting, content, Length::Shrink)
    }

    fn wrap_width<'a, Message: 'a>(
        &self,
        setting: Setting,
        content: impl Into<Element<'a, Message>>,
        width: Length,
    ) -> Element<'a, Message> {
        let row = container(content).width(width);
        if self.setting == Some(setting) {
            row.id(self.row.clone())
                .padding(if width == Length::Shrink { 3 } else { 0 })
                .style(container::secondary)
                .into()
        } else {
            row.into()
        }
    }

    pub(super) fn input<'a, Message: Clone>(
        &self,
        setting: Setting,
        editable: bool,
        input: iced::widget::TextInput<'a, Message>,
    ) -> iced::widget::TextInput<'a, Message> {
        let target = (editable && self.setting == Some(setting)).then_some(&self.input);
        crate::settings_search::mark_input(input, target)
    }

    pub(super) fn control_id(&self, setting: Setting) -> Id {
        if self.setting == Some(setting) {
            self.input.clone()
        } else {
            Id::from(format!("ai-manager-{setting:?}"))
        }
    }

    pub(super) fn keyboard_action<'a, Message: Clone + 'a>(
        &self,
        setting: Setting,
        content: impl Into<Element<'a, Message>>,
        message: Option<Message>,
        accept_enter: bool,
    ) -> Element<'a, Message> {
        crate::workspace::dialog_design::keyboard_table::action_with_id(
            content.into(),
            message,
            accept_enter,
            Some(self.control_id(setting)),
        )
    }

    pub(super) fn apply<Message: 'static>(&self) -> Task<Message> {
        crate::settings_search::scroll_to_row(self.viewport.clone(), self.row.clone())
            .chain(crate::settings_search::focus_if_present(self.input.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use iced::advanced::widget::{Operation, Tree, operation};
    use iced::advanced::{Layout, layout};
    use iced::futures::{StreamExt, executor::block_on};
    use iced::{Font, Rectangle, Renderer, Size, Vector};

    use super::*;
    use crate::ai_manager::{Configuration, Dialog, LocalSection, Message};

    struct Position {
        viewport_id: Id,
        row_id: Id,
        viewport: Rectangle,
        row_y: Option<f32>,
        offset: f32,
    }

    impl Operation for Position {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
            if id == Some(&self.row_id) {
                self.row_y = Some(bounds.y);
            }
        }

        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: Rectangle,
            _: Rectangle,
            translation: Vector,
            _: &mut dyn operation::Scrollable,
        ) {
            if id == Some(&self.viewport_id) {
                self.viewport = bounds;
                self.offset = translation.y;
            }
        }
    }

    fn applied_position(dialog: &Dialog, reveal: Task<Message>) -> Position {
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut view = dialog.view(Vec::new(), Vec::new());
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(760.0, 300.0)),
        );
        let mut apply = |operation: &mut dyn Operation| {
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, operation);
        };
        let mut actions = iced_runtime::task::into_stream(reveal).unwrap();
        while let Some(action) = block_on(actions.next()) {
            let iced_runtime::Action::Widget(mut operation) = action else {
                panic!("revealing a control should produce only widget actions");
            };
            loop {
                apply(operation.as_mut());
                let operation::Outcome::Chain(next) = operation.finish() else {
                    break;
                };
                operation = next;
            }
        }
        let mut position = Position {
            viewport_id: dialog.revealed.viewport.clone(),
            row_id: dialog.revealed.row.clone(),
            viewport: Rectangle::default(),
            row_y: None,
            offset: 0.0,
        };
        apply(&mut position);
        position
    }

    #[test]
    fn general_search_reveals_the_locked_agent_triage_setting() {
        use crate::clipboard_policy::parse;
        use crate::terminal_agent::planning::TriageMode;

        let settings = Configuration::decode(br#"{"general":{"agent_triage":"never"}}"#).unwrap();
        let policy =
            parse("[meta]\nschema-version=1\n[[rule]]\n[rule.ai-memory]\ntriage='always'\n")
                .unwrap();
        let mut dialog = Dialog::new(Arc::new(settings), PathBuf::new(), Vec::new(), true, None)
            .with_memory_policy(&policy.ai_memory, true);
        let _ = dialog.update(Message::Search("agent complexity check".into()));
        let reveal = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::General);
        assert_eq!(dialog.revealed.setting, Some(Setting::AgentTriage));
        let position = applied_position(&dialog, reveal);
        let visible_y = position.row_y.unwrap() - position.offset;
        assert!(visible_y >= position.viewport.y - 1.0);
        assert!(visible_y < position.viewport.y + position.viewport.height);
        assert_eq!(dialog.managed_triage, Some(TriageMode::Always));
        let _ = dialog.update(Message::AgentTriage(TriageMode::Auto));
        assert_eq!(dialog.general.agent_triage, TriageMode::Never);
    }

    #[test]
    fn general_search_reveals_imported_preferences_without_adopting_them() {
        let mut dialog = Dialog::new(
            Arc::new(Configuration {
                java_journal_semantic_search: Some(true),
                java_agent_confirm_mutating: Some(false),
                java_agent_execution_enabled: Some(false),
                java_agent_shortcut: Some(super::super::JavaAgentShortcut {
                    name: Some("fixture".into()),
                    case_insensitive: Some(false),
                }),
                ..Configuration::default()
            }),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let draft = dialog.general.clone();

        for imported in ImportedGeneral::ALL {
            let setting = Setting::ImportedGeneral(imported);
            let _ = dialog.update(Message::Search(setting.title().into()));
            let reveal = dialog.update(Message::SearchChoose);
            assert_eq!(dialog.page, Page::General);
            assert_eq!(dialog.revealed.setting, Some(setting));
            let position = applied_position(&dialog, reveal);
            let visible_y =
                position.row_y.expect("imported preference is highlighted") - position.offset;
            assert!(visible_y >= position.viewport.y - 1.0);
            assert!(visible_y < position.viewport.y + position.viewport.height);
            assert!(dialog.general == draft);
            assert!(dialog.request.is_none());
        }
    }

    #[test]
    fn local_models_search_reveals_the_exact_imported_row_without_adopting_it() {
        use java_models::Kind;

        let mut hints = java_models::import(
            Kind::Gguf,
            br#"<llamaModels><model><id>archive-chat</id><displayName>Archive Model</displayName></model></llamaModels>"#,
        )
        .unwrap()
        .hints;
        let mlx = java_models::import(
            Kind::Mlx,
            br#"{"schemaVersion":1,"models":[{"id":"archive-chat","displayName":"Archive Model"}]}"#,
        )
        .unwrap()
        .hints;
        hints.merge(mlx);
        let mut dialog = Dialog::new(
            Arc::new(Configuration {
                java_model_hints: hints,
                ..Configuration::default()
            }),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::Search("ARCHIVE Model".into()));
        let targets: Vec<_> = dialog.search_results().collect();
        assert!(matches!(
            targets.as_slice(),
            [
                Target::Setting(Setting::ImportedJavaModel(_)),
                Target::Setting(Setting::ImportedJavaModel(_)),
                ..
            ]
        ));
        assert!(
            !targets[2..]
                .iter()
                .any(|target| matches!(target, Target::Setting(Setting::ImportedJavaModel(_))))
        );

        for kind in [Kind::Gguf, Kind::Mlx] {
            let _ = dialog.update(Message::Search(format!("{} archive-chat", kind.label())));
            let targets = dialog.search_results().collect::<Vec<_>>();
            let [Target::Setting(Setting::ImportedJavaModel(key)), ..] = targets.as_slice() else {
                panic!("expected one imported model result, got {targets:?}");
            };
            let hint = dialog.saved.java_model_hints.search_hint(*key).unwrap();
            assert_eq!(hint.kind(), kind);
            assert_eq!(hint.id, "archive-chat");
            assert!(
                !targets[1..]
                    .iter()
                    .any(|target| matches!(target, Target::Setting(Setting::ImportedJavaModel(_))))
            );

            let reveal = dialog.update(Message::SearchChoose);
            assert_eq!(dialog.page, Page::LocalModels);
            assert_eq!(
                dialog.revealed.setting,
                Some(Setting::ImportedJavaModel(*key))
            );
            let position = applied_position(&dialog, reveal);
            let visible_y = position
                .row_y
                .expect("the matched model row is highlighted")
                - position.offset;
            assert!(visible_y >= position.viewport.y - 1.0);
            assert!(visible_y < position.viewport.y + position.viewport.height);
            assert!(dialog.request.is_none());
        }

        let _ = dialog.update(Message::Search("gguf archive-chat".into()));
        let stale = dialog.search_results().next().unwrap();
        let replacement = java_models::import(
            Kind::Mlx,
            br#"{"schemaVersion":1,"models":[{"id":"archive-chat","displayName":"Archive Model"}]}"#,
        )
        .unwrap()
        .hints;
        dialog.saved = Arc::new(Configuration {
            java_model_hints: replacement,
            ..Configuration::default()
        });
        dialog.page = Page::General;
        let _ = dialog.update(Message::Reveal(stale));
        assert_eq!(dialog.page, Page::General);
        assert_eq!(dialog.search, "gguf archive-chat");
    }

    #[test]
    fn local_preferences_search_scrolls_runtime_and_imported_roles_in_one_viewport() {
        use crate::ai_manager::{backup, java_roles};
        use crate::clipboard_policy::ClipboardPolicy;

        let (portable, _) = backup::portable_java_profiles(
            br#"<globalSettings>
            <defaultAiProfileId>java-review</defaultAiProfileId>
            <textAiProfileId>missing-text</textAiProfileId>
            <terminalAgentAiProfileId>java-review</terminalAgentAiProfileId>
            <securityCheckAiProfileId>java-review</securityCheckAiProfileId>
            <aiProfiles><profile><id>java-review</id><name>Imported review</name>
                <apiUrl>https://review.example.test/v1</apiUrl><model>fixture</model>
            </profile></aiProfiles>
        </globalSettings>"#,
        )
        .unwrap();
        let merged = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        let mut dialog = Dialog::new(
            Arc::new(Configuration::decode(&merged).unwrap()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        dialog.local.set_runtime_downloads_allowed(false);
        let local = dialog.local_state.clone();
        let profiles = dialog.profiles.catalog().clone();
        assert!(
            !dialog.search_target_available(Target::Setting(Setting::LocalPreference(
                LocalPreference::ImportedRole(java_roles::Role::Coding),
            )))
        );
        for (query, setting, section) in [
            (
                "preferred runtime backend",
                LocalPreference::RuntimeBackend,
                LocalSection::Runtime,
            ),
            (
                "imported Java agent profile",
                LocalPreference::ImportedRole(java_roles::Role::Agent),
                LocalSection::Roles,
            ),
            (
                "managed runtime update policy",
                LocalPreference::RuntimeUpdatePolicy,
                LocalSection::Runtime,
            ),
            (
                "imported Java text profile",
                LocalPreference::ImportedRole(java_roles::Role::Text),
                LocalSection::Roles,
            ),
            (
                "Hugging Face access preference",
                LocalPreference::HubAccess,
                LocalSection::HuggingFace,
            ),
        ] {
            let _ = dialog.update(Message::Search(query.into()));
            let reveal = dialog.update(Message::SearchChoose);
            assert_eq!(dialog.page, Page::LocalPreferences);
            assert_eq!(dialog.local_section, section);
            assert_eq!(
                dialog.revealed.setting,
                Some(Setting::LocalPreference(setting))
            );
            let position = applied_position(&dialog, reveal);
            let visible_y =
                position.row_y.expect("the chosen setting is highlighted") - position.offset;
            assert!(
                visible_y >= position.viewport.y - 1.0
                    && visible_y < position.viewport.y + position.viewport.height
            );
            assert_eq!(dialog.local_state, local);
            assert_eq!(dialog.profiles.catalog(), &profiles);
        }
    }

    #[test]
    fn local_preferences_search_distinguishes_embedding_source_and_manual_identifier() {
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let source = Setting::LocalPreference(LocalPreference::EmbeddingSource);
        let guide = Setting::LocalPreference(LocalPreference::EmbeddingSetup);
        let _ = dialog.update(Message::Search("default embedding model".into()));
        assert_eq!(
            dialog.search_results().next(),
            Some(Target::Setting(Setting::Embedding))
        );
        assert!(dialog.search_target_available(Target::Setting(guide)));

        dialog
            .local_state
            .set_guided_embedding("fixture-managed-embedding".into());
        assert!(!dialog.search_target_available(Target::Setting(guide)));
        assert!(!dialog.search_target_available(Target::Setting(Setting::Embedding)));
        assert_eq!(
            dialog.search_results().next(),
            Some(Target::Setting(source))
        );
        assert!(
            results("Hugging Face token", None)
                .any(|target| target == Target::Setting(Setting::Hub(HubSetting::Access)))
        );
    }

    #[test]
    fn ranked_pages_and_punctuation_colliding_model_ids_remain_navigable() {
        let hints = java_models::import(
            java_models::Kind::Gguf,
            br#"<llamaModels><model><id>archive-chat</id><displayName>Archive Model</displayName></model><model><id>archive_chat</id><displayName>Archive Model</displayName></model></llamaModels>"#,
        )
        .unwrap()
        .hints;
        let mut dialog = Dialog::new(
            Arc::new(Configuration {
                java_model_hints: hints,
                ..Configuration::default()
            }),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::Search("P! ".into()));
        assert!(dialog.search_results().next().is_none());
        let _ = dialog.update(Message::Search("PRÓFÎLES!".into()));
        assert_eq!(
            dialog.search_results().next(),
            Some(Target::Page(Page::Profiles))
        );
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::Profiles);
        assert!(dialog.search.is_empty());

        let _ = dialog.update(Message::Search("Archive Model".into()));
        let targets = dialog.search_results().collect::<Vec<_>>();
        let [
            Target::Setting(Setting::ImportedJavaModel(first)),
            Target::Setting(Setting::ImportedJavaModel(second)),
            ..,
        ] = targets.as_slice()
        else {
            panic!("expected the two distinct model registrations, got {targets:?}");
        };
        assert_ne!(first, second);
        let _ = dialog.update(Message::SearchStep(1));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::ImportedJavaModel(*second))
        );
        assert_eq!(
            dialog
                .saved
                .java_model_hints
                .search_hint(*second)
                .unwrap()
                .id,
            "archive_chat"
        );
    }

    #[test]
    fn ai_manager_search_respects_connection_mode_and_matches_unopened_pages() {
        let mut profile = profiles::Profile::default();
        let endpoint = Target::Setting(Setting::Profile(ProfileSetting::Endpoint));
        let model = Target::Setting(Setting::Profile(ProfileSetting::ManagedModel));
        let preset = Target::Setting(Setting::Profile(ProfileSetting::PromptPreset));
        let provider = Target::Setting(Setting::Profile(ProfileSetting::CloudProvider));
        let executable = Target::Setting(Setting::Profile(ProfileSetting::CliExecutable));
        let connection_type = Target::Setting(Setting::Profile(ProfileSetting::Connection));
        let quota = Target::Setting(Setting::Profile(ProfileSetting::Usage(
            profiles::UsageField::Limit,
        )));
        assert!(results("API ENDPOINT", Some(&profile)).any(|target| target == endpoint));
        assert_eq!(
            results("CLI connection type", Some(&profile)).next(),
            Some(connection_type)
        );
        assert_eq!(results("token quota", Some(&profile)).next(), Some(quota));
        assert!(!results("cloud provider", Some(&profile)).any(|target| target == provider));
        profile.connection_mode = profiles::ConnectionMode::RemoteOpenAi;
        assert_eq!(
            results("cloud provider", Some(&profile)).next(),
            Some(provider)
        );
        profile.connection_mode = profiles::ConnectionMode::RemoteOpenAiExact;
        assert!(!results("cloud provider", Some(&profile)).any(|target| target == provider));
        profile.connection_mode = profiles::ConnectionMode::Managed;
        assert!(!results("API ENDPOINT", Some(&profile)).any(|target| target == endpoint));
        assert!(results("GGUF model", Some(&profile)).any(|target| target == model));
        assert!(results("Qwen prompt", Some(&profile)).any(|target| target == preset));
        profile.connection_mode = profiles::ConnectionMode::Cli;
        assert!(results("Qwen prompt", Some(&profile)).any(|target| target == preset));
        assert!(!results("token quota", Some(&profile)).any(|target| target == quota));
        assert!(!results("cli executable path", Some(&profile)).any(|target| target == executable));
        assert!(
            results("Hugging Face", None).any(|target| target == Target::Page(Page::LocalModels))
        );
        assert!(
            results("translation profile", None)
                .any(|target| target == Target::Setting(Setting::Role(Role::Text)))
        );
    }

    #[test]
    fn knowledge_search_reveals_actions_without_editing_or_indexing_the_store() {
        use crate::ai_manager::local;

        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let update = Target::Setting(Setting::Knowledge(KnowledgeSetting::Update));
        let create = Target::Setting(Setting::Knowledge(KnowledgeSetting::Create));
        let _ = dialog.update(Message::Search("update index".into()));
        assert!(!dialog.search_results().any(|target| target == update));
        let _ = dialog.update(Message::Search("create store".into()));
        assert!(dialog.search_results().any(|target| target == create));
        let _ = dialog.update(Message::SearchChoose);
        assert!(dialog.page == Page::Knowledge);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Knowledge(KnowledgeSetting::Create))
        );
        assert!(!dialog.local_state.has_knowledge_stores());

        let _ = dialog.update(Message::Local(local::Message::StoreName("Fixture".into())));
        let _ = dialog.update(Message::Local(local::Message::CreateStore));
        let _ = dialog.update(Message::Search("update index".into()));
        assert!(dialog.search_results().any(|target| target == update));
        let before = serde_json::to_value(&dialog.local_state).unwrap();
        let task = dialog.update(Message::SearchChoose);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Knowledge(KnowledgeSetting::Update))
        );
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = iced_runtime::UserInterface::build(
            dialog.view(Vec::new(), Vec::new()),
            Size::new(760.0, 360.0),
            iced_runtime::user_interface::Cache::default(),
            &mut renderer,
        );
        let mut position = Position {
            viewport_id: dialog.revealed.viewport.clone(),
            row_id: dialog.revealed.row.clone(),
            viewport: Rectangle::default(),
            row_y: None,
            offset: 0.0,
        };
        interface.operate(&renderer, &mut position);
        let target = position
            .row_y
            .expect("target button has a native reveal row");
        assert!(target >= position.viewport.y + position.viewport.height);
        crate::settings_search::test_input::apply(&mut interface, &renderer, task);
        interface.operate(&renderer, &mut position);
        let y = target - position.offset;
        assert!(y >= position.viewport.y && y < position.viewport.y + position.viewport.height);
        assert_eq!(serde_json::to_value(&dialog.local_state).unwrap(), before);
    }

    #[test]
    fn knowledge_inventory_search_reveals_each_toolbar_control_on_first_visit() {
        use iced_runtime::{UserInterface, user_interface::Cache};

        for action in InventoryAction::ALL {
            let mut dialog = Dialog::new(
                Arc::new(Configuration::default()),
                PathBuf::new(),
                Vec::new(),
                false,
                None,
            );
            assert_eq!(dialog.page, Page::General);
            let setting = Setting::Knowledge(KnowledgeSetting::Inventory(action));
            let _ = dialog.update(Message::Search(action.title().into()));
            assert_eq!(
                dialog.search_results().next(),
                Some(Target::Setting(setting))
            );
            let task = dialog.update(Message::SearchChoose);
            assert_eq!(dialog.page, Page::Knowledge);
            assert_eq!(dialog.revealed.setting, Some(setting));
            assert!(!dialog.local_state.has_knowledge_stores());

            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                dialog.view(Vec::new(), Vec::new()),
                Size::new(640.0, 300.0),
                Cache::default(),
                &mut renderer,
            );
            let mut position = Position {
                viewport_id: dialog.revealed.viewport.clone(),
                row_id: dialog.revealed.row.clone(),
                viewport: Rectangle::default(),
                row_y: None,
                offset: 0.0,
            };
            crate::settings_search::test_input::apply(&mut interface, &renderer, task);
            interface.operate(&renderer, &mut position);
            let y = position
                .row_y
                .unwrap_or_else(|| panic!("{} has no reveal row", action.title()))
                - position.offset;
            assert!(
                y >= position.viewport.y && y < position.viewport.y + position.viewport.height,
                "{} is outside the Knowledge Stores viewport",
                action.title()
            );
        }
    }

    #[test]
    fn local_models_search_reveals_runtime_and_gguf_buttons_on_first_visit() {
        for (query, setting) in [
            (
                "install runtime",
                Setting::Control(Control::InstallStableRuntime),
            ),
            (
                "other version",
                Setting::Control(Control::RuntimeVersionsEntry),
            ),
            (
                "import local package",
                Setting::Control(Control::OpenSignedPackage),
            ),
            ("import gguf", Setting::Managed(ManagedSetting::OpenImport)),
            (
                "choose gguf files",
                Setting::Managed(ManagedSetting::ChooseFiles),
            ),
        ] {
            let mut dialog = Dialog::new(
                Arc::new(Configuration::default()),
                PathBuf::new(),
                Vec::new(),
                true,
                None,
            );
            assert_eq!(dialog.page, Page::General);
            let _ = dialog.update(Message::Search(query.into()));
            assert_eq!(
                dialog.search_results().next(),
                Some(Target::Setting(setting))
            );
            let reveal = dialog.update(Message::SearchChoose);
            assert_eq!(dialog.page, Page::LocalModels);
            assert_eq!(dialog.revealed.setting, Some(setting));
            let position = applied_position(&dialog, reveal);
            let y = position
                .row_y
                .unwrap_or_else(|| panic!("no reveal row for {query}"))
                - position.offset;
            assert!(
                y >= position.viewport.y && y < position.viewport.y + position.viewport.height,
                "{query} is outside the Local Models viewport",
            );
        }
    }

    #[test]
    fn control_search_focuses_local_model_memory_and_skill_fields() {
        use crate::ai_manager::{local, memory, skills};
        use crate::settings_search::test_input;
        use iced::keyboard::Key;
        use iced_runtime::{UserInterface, user_interface::Cache};

        struct Focus<'a> {
            input: &'a Id,
            focused: Option<bool>,
        }
        impl Operation for Focus<'_> {
            fn traverse(&mut self, operation: &mut dyn FnMut(&mut dyn Operation)) {
                operation(self);
            }

            fn focusable(
                &mut self,
                id: Option<&Id>,
                _: Rectangle,
                target: &mut dyn operation::Focusable,
            ) {
                if id == Some(self.input) {
                    self.focused = Some(target.is_focused());
                }
            }
        }

        let scratch = crate::snippets::tests::Profile::new();
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            scratch.0.clone(),
            Vec::new(),
            true,
            None,
        );
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        for (query, control) in [
            ("signed index", Control::SignedIndex),
            ("token budget", Control::Budget),
            ("skill name", Control::SkillName),
        ] {
            if control == Control::SkillName {
                let _ = dialog.open_skill_find_for_test();
                assert!(dialog.skills.find_open());
            }
            let _ = dialog.update(Message::Search(query.into()));
            let task = dialog.update(Message::SearchChoose);
            assert_eq!(dialog.page, control.page());
            assert_eq!(dialog.revealed.setting, Some(Setting::Control(control)));
            let mut interface = UserInterface::build(
                dialog.view(Vec::new(), Vec::new()),
                Size::new(900.0, 480.0),
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(&mut interface, &renderer, task);
            let mut focus = Focus {
                input: &dialog.revealed.input,
                focused: None,
            };
            interface.operate(&renderer, &mut focus);
            assert_eq!(focus.focused, Some(true), "focus for {control:?}");
            let typed = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Character("7".into()),
                Some("7"),
            );
            match control {
                Control::SignedIndex => assert!(matches!(typed.as_slice(),
                    [Message::Local(local::Message::RuntimePath(_, text))] if text.ends_with('7'))),
                Control::Budget => assert!(matches!(typed.as_slice(),
                    [Message::Memory(memory::Message::Tokens(text))] if text.ends_with('7'))),
                _ => {
                    assert!(matches!(typed.as_slice(),
                        [Message::Skills(skills::Message::EditName(text))] if text.ends_with('7')));
                    assert!(dialog.skills.find_open());
                }
            }
        }
    }

    #[test]
    fn ai_manager_search_enter_focuses_an_editable_result_and_keeps_search_for_disabled_fields() {
        use iced::advanced::widget::operation::focusable;
        use iced::keyboard::{Key, key::Named};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::settings_search::test_input;

        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::Profiles(profiles::Message::Add));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let bounds = Size::new(760.0, 360.0);
        for (query, setting) in [
            ("global request timeout", Setting::GlobalTimeout),
            ("base command name", Setting::AgentShortcutName),
            (
                "max selection characters",
                Setting::Profile(ProfileSetting::SelectionLimit),
            ),
            (
                "api key variable",
                Setting::Profile(ProfileSetting::Credential),
            ),
            (
                "reset period in days",
                Setting::Profile(ProfileSetting::Usage(profiles::UsageField::Days)),
            ),
            (
                "cli executable path",
                Setting::Profile(ProfileSetting::CliExecutable),
            ),
            (
                "minimax cli config path",
                Setting::Profile(ProfileSetting::MiniMaxConfig),
            ),
        ] {
            if setting == Setting::Profile(ProfileSetting::CliExecutable) {
                let _ = dialog.update(Message::Profiles(profiles::Message::ConnectionMode(
                    profiles::ConnectionMode::Cli,
                )));
            }
            if setting == Setting::Profile(ProfileSetting::MiniMaxConfig) {
                let _ = dialog.update(Message::Profiles(profiles::Message::Cli(
                    profiles::CliAction::MiniMaxPreset,
                )));
            }
            let _ = dialog.update(Message::Search(query.into()));
            let mut interface = UserInterface::build(
                dialog.view(Vec::new(), Vec::new()),
                bounds,
                Cache::default(),
                &mut renderer,
            );
            interface.operate(&renderer, &mut focusable::focus(dialog.search_id.clone()));
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Enter),
                None,
            );
            assert!(matches!(messages.as_slice(), [Message::SearchChoose]));
            let cache = interface.into_cache();
            let reveal = dialog.update(messages.into_iter().next().unwrap());
            assert_eq!(dialog.revealed.setting, Some(setting));
            let mut interface = UserInterface::build(
                dialog.view(Vec::new(), Vec::new()),
                bounds,
                cache,
                &mut renderer,
            );
            test_input::apply(&mut interface, &renderer, reveal);
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Character("2".into()),
                Some("2"),
            );
            match setting {
                Setting::GlobalTimeout => assert!(
                    matches!(messages.as_slice(), [Message::Timeout(value)] if value.ends_with('2'))
                ),
                Setting::AgentShortcutName => assert!(
                    matches!(messages.as_slice(), [Message::AgentShortcutName(value)] if value.ends_with('2'))
                ),
                Setting::Profile(ProfileSetting::SelectionLimit) => assert!(
                    matches!(messages.as_slice(), [Message::Profiles(profiles::Message::Number(profiles::Number::Selection, value))] if value.ends_with('2'))
                ),
                Setting::Profile(ProfileSetting::Usage(profiles::UsageField::Days)) => assert!(
                    matches!(messages.as_slice(), [Message::Profiles(profiles::Message::EditUsage(profiles::UsageField::Days, value))] if value.ends_with('2'))
                ),
                Setting::Profile(ProfileSetting::CliExecutable) => assert!(
                    matches!(messages.as_slice(), [Message::Profiles(profiles::Message::Cli(profiles::CliAction::Path(value)))] if value.ends_with('2'))
                ),
                Setting::Profile(ProfileSetting::MiniMaxConfig) => assert!(
                    matches!(messages.as_slice(), [Message::Profiles(profiles::Message::Cli(profiles::CliAction::MiniMaxConfig(value)))] if value.ends_with('2'))
                ),
                _ => {
                    assert!(matches!(messages.as_slice(), [Message::Search(value)] if value == "2"))
                }
            }
        }
    }

    #[test]
    fn hub_search_reveals_the_query_without_a_request_and_respects_download_policy() {
        use iced::keyboard::Key;
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::ai_manager::local::{self, huggingface::Action};
        use crate::settings_search::test_input;

        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let query = Target::Setting(Setting::Hub(HubSetting::Query));
        let _ = dialog.update(Message::Search("huggingface search query".into()));
        assert_eq!(dialog.search_results().collect::<Vec<_>>(), [query]);
        let reveal = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::LocalModels);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Hub(HubSetting::Query))
        );
        assert!(!dialog.local.busy());

        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let bounds = Size::new(760.0, 360.0);
        {
            let mut ui = UserInterface::build(
                dialog.view(Vec::new(), Vec::new()),
                bounds,
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(&mut ui, &renderer, reveal);
            let messages = test_input::press(
                &mut ui,
                &mut renderer,
                Key::Character("q".into()),
                Some("q"),
            );
            assert!(
                matches!(messages.as_slice(), [Message::Local(local::Message::HuggingFace(Action::Query(value)))] if value == "q")
            );
        }

        let _ = dialog.update(Message::Search("huggingface review repository".into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Hub(HubSetting::ReviewRepository))
        );
        assert!(!dialog.local.busy());
        let _ = dialog.update(Message::Search("huggingface reviewed gguf file".into()));
        assert_eq!(dialog.search_results().count(), 0);

        dialog.set_model_downloads_allowed(false);
        let _ = dialog.update(Message::Search("huggingface search query".into()));
        assert_eq!(dialog.search_results().count(), 0);
        assert!(iced_runtime::task::into_stream(dialog.update(Message::Reveal(query))).is_none());
        let discard = Target::Setting(Setting::Hub(HubSetting::DiscardPaused));
        let _ = dialog.update(Message::Search(
            "huggingface discard paused download".into(),
        ));
        let mut expected = vec![discard];
        if crate::local_ai::mlx_runtime::supported() {
            expected.push(Target::Setting(Setting::Hub(HubSetting::DiscardPausedMlx)));
        }
        assert_eq!(dialog.search_results().collect::<Vec<_>>(), expected);
        let _ = dialog.update(Message::Reveal(discard));
        let _ = dialog.update(Message::Search("huggingface keep download".into()));
        assert_eq!(dialog.search_results().count(), 0);
        let _ = dialog.update(Message::SearchEscape);
        let _ = dialog.update(Message::Local(local::Message::HuggingFace(
            Action::DiscardPaused,
        )));
        let keep = Target::Setting(Setting::Hub(HubSetting::KeepPaused));
        let _ = dialog.update(Message::Search("huggingface keep download".into()));
        assert_eq!(dialog.search_results().collect::<Vec<_>>(), [keep]);
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Hub(HubSetting::KeepPaused))
        );
        assert!(!dialog.local.busy());
    }

    #[test]
    fn profile_setup_search_validates_steps_preserves_the_draft_and_focuses_the_name() {
        use iced::advanced::widget::operation::focusable;
        use iced::keyboard::{Key, key::Named};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::settings_search::test_input;

        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        dialog.open_profile_wizard_for_test();
        let candidate = dialog.profiles.selected().unwrap().id;
        let edit =
            |field, value: &str| Message::Profiles(profiles::Message::Edit(field, value.into()));
        let name = Target::Setting(Setting::Profile(ProfileSetting::Name));
        let _ = dialog.update(edit(profiles::Field::Endpoint, "invalid-endpoint"));
        let _ = dialog.update(Message::Search("profile name".into()));
        assert!(dialog.search_results().next().is_none());
        assert!(iced_runtime::task::into_stream(dialog.update(Message::Reveal(name))).is_none());

        let _ = dialog.update(Message::Search("find local model servers".into()));
        assert_eq!(
            dialog.search_results().next(),
            Some(Target::Setting(Setting::WizardScan))
        );
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.revealed.setting, Some(Setting::WizardScan));
        assert!(dialog.request.is_none());

        let _ = dialog.update(edit(profiles::Field::Endpoint, "127.0.0.1:11434"));
        let _ = dialog.update(Message::Search("model identifier".into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Profile(ProfileSetting::Model))
        );
        let _ = dialog.update(edit(profiles::Field::Model, "wizard-test-model"));
        let _ = dialog.update(Message::Search("profile name".into()));

        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let bounds = Size::new(760.0, 360.0);
        let mut interface = UserInterface::build(
            dialog.view(Vec::new(), Vec::new()),
            bounds,
            Cache::default(),
            &mut renderer,
        );
        interface.operate(&renderer, &mut focusable::focus(dialog.search_id.clone()));
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Named(Named::Enter),
            None,
        );
        assert!(matches!(messages.as_slice(), [Message::SearchChoose]));
        let cache = interface.into_cache();
        let reveal = dialog.update(messages.into_iter().next().unwrap());
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Profile(ProfileSetting::Name))
        );
        let mut interface = UserInterface::build(
            dialog.view(Vec::new(), Vec::new()),
            bounds,
            cache,
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, reveal);
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Character("X".into()),
            Some("X"),
        );
        assert!(
            matches!(messages.as_slice(), [Message::Profiles(profiles::Message::Edit(profiles::Field::Name, value))] if value.ends_with('X'))
        );
        drop(interface);
        let _ = dialog.update(messages.into_iter().next().unwrap());
        let draft = dialog.profiles.selected().unwrap().clone();
        let _ = dialog.update(Message::Search("test profile connection".into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.revealed.setting, Some(Setting::WizardTest));
        assert!(dialog.request.is_none());
        assert!(dialog.profiles.selected() == Some(&draft));
        assert!(dialog.profiles.catalog().profile(candidate).is_none());
        assert!(dialog.profile_wizard_open());
    }

    #[test]
    fn profile_action_search_reveals_discovery_and_removal_without_triggering_either() {
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let action = |action| Target::Setting(Setting::ProfileAction(action));
        assert!(!dialog.search_target_available(action(ProfileAction::TestConnection)));
        let _ = dialog.update(Message::Profiles(profiles::Message::Add));
        let draft = dialog.profiles.catalog().clone();
        assert!(dialog.search_target_available(action(ProfileAction::TestConnection)));
        assert!(!dialog.search_target_available(action(ProfileAction::TestCli)));
        assert!(!dialog.search_target_available(action(ProfileAction::ConfirmDelete)));

        let _ = dialog.update(Message::Search("load models".into()));
        let reveal = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::Profiles);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::ProfileAction(ProfileAction::LoadModels))
        );
        assert!(dialog.request.is_none());
        assert_eq!(dialog.profiles.catalog(), &draft);
        let position = applied_position(&dialog, reveal);
        let row = position.row_y.expect("the discovery button is highlighted");
        assert!(row - position.offset >= position.viewport.y - 1.0);
        assert!(row - position.offset < position.viewport.y + position.viewport.height);

        let _ = dialog.update(Message::Profiles(profiles::Message::Delete));
        let _ = dialog.update(Message::Search("remove profile".into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::ProfileAction(ProfileAction::ConfirmDelete))
        );
        assert!(dialog.search_target_available(action(ProfileAction::CancelDelete)));
        assert!(!dialog.search_target_available(action(ProfileAction::TestConnection)));
        assert_eq!(dialog.profiles.catalog(), &draft);
        let _ = dialog.update(Message::Profiles(profiles::Message::CancelDelete));
        assert!(!dialog.search_target_available(action(ProfileAction::ConfirmDelete)));
        assert_eq!(dialog.profiles.catalog(), &draft);
    }

    #[test]
    fn profile_action_search_distinguishes_imported_model_mapping_from_connection_test() {
        use crate::ai_manager::backup;
        use crate::clipboard_policy::ClipboardPolicy;

        let (portable, _) = backup::portable_java_profiles(
            br#"<globalSettings>
            <aiProfiles><profile><id>java-local</id><name>Imported work</name>
                <connectionMode>EMBEDDED_LLAMA_CPP</connectionMode>
                <embeddedModelId>java-model</embeddedModelId>
            </profile></aiProfiles>
        </globalSettings>"#,
        )
        .unwrap();
        let merged = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        let mut dialog = Dialog::new(
            Arc::new(Configuration::decode(&merged).unwrap()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let action = |action| Target::Setting(Setting::ProfileAction(action));
        let draft = dialog.profiles.catalog().clone();
        assert!(dialog.search_target_available(action(ProfileAction::ChooseImportedModel)));
        assert!(!dialog.search_target_available(action(ProfileAction::TestConnection)));
        assert!(!dialog.search_target_available(action(ProfileAction::ConfirmImportedModel)));
        let _ = dialog.update(Message::Search("refresh managed models".into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::Profiles);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::ProfileAction(ProfileAction::RefreshManagedModels))
        );
        assert!(dialog.search_target_available(action(ProfileAction::RefreshManagedModels)));
        assert_eq!(dialog.profiles.catalog(), &draft);
        assert!(dialog.request.is_none());
    }

    #[test]
    fn ai_manager_search_reveals_profile_option_inside_the_scrolled_section() {
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::Profiles(profiles::Message::Add));
        let _ = dialog.update(Message::Profiles(profiles::Message::Section(
            profiles::Section::Limits,
        )));
        let draft = dialog.profiles.catalog().clone();
        let _ = dialog.update(Message::Search("api key variable".into()));
        let _ = dialog.update(Message::SearchEscape);
        assert_eq!(dialog.page, Page::General);
        assert!(dialog.search.is_empty());
        let _ = dialog.update(Message::Search("api key variable".into()));
        let reveal = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::Profiles);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Profile(ProfileSetting::Credential))
        );
        assert_eq!(dialog.profiles.catalog(), &draft);

        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut view = dialog.view(Vec::new(), Vec::new());
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(760.0, 300.0)),
        );
        let mut apply = |op: &mut dyn Operation| {
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, op);
        };
        let mut position = Position {
            viewport_id: dialog.revealed.viewport.clone(),
            row_id: dialog.revealed.row.clone(),
            viewport: Rectangle::default(),
            row_y: None,
            offset: 0.0,
        };
        apply(&mut position);
        let target_y = position
            .row_y
            .expect("the profile connection section exposes the selected field");
        assert!(
            position.viewport.height > 0.0
                && target_y >= position.viewport.y + position.viewport.height,
            "the selected row at {target_y} starts below the viewport {:?}",
            position.viewport,
        );
        let mut actions = iced_runtime::task::into_stream(reveal).unwrap();
        while let Some(action) = block_on(actions.next()) {
            if let iced_runtime::Action::Widget(mut operation) = action {
                loop {
                    apply(operation.as_mut());
                    let operation::Outcome::Chain(next) = operation.finish() else {
                        break;
                    };
                    operation = next;
                }
            }
        }
        apply(&mut position);
        assert!(position.offset > 0.0);
        let y = target_y - position.offset;
        assert!(
            y >= position.viewport.y && y < position.viewport.y + position.viewport.height,
            "the selected row at {y} should be visible within {:?}",
            position.viewport,
        );
    }
}
