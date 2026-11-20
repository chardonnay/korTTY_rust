//! Search destinations for the AI Manager's existing pages and settings.

use iced::widget::{Id, container};
use iced::{Element, Length, Task};

use super::{Page, local::Role, profiles};

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

    pub(super) fn label(self) -> String {
        match self {
            Self::Page(page) => page.title().into(),
            Self::Setting(setting) => format!("{} · {}", setting.page().title(), setting.title()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Setting {
    Features,
    GlobalTimeout,
    CodeTextLanguage,
    ChatTerminalActions,
    AgentShortcut,
    AgentExecution,
    AgentConfirmation,
    ShowAgentSetup,
    JournalSemanticSearch,
    WizardScan,
    WizardTest,
    Profile(ProfileSetting),
    Role(Role),
    Embedding,
    Knowledge(KnowledgeSetting),
    Hub(HubSetting),
    Control(Control),
}

impl Setting {
    fn page(self) -> Page {
        match self {
            Self::Features
            | Self::GlobalTimeout
            | Self::CodeTextLanguage
            | Self::ChatTerminalActions
            | Self::AgentShortcut
            | Self::AgentExecution
            | Self::AgentConfirmation
            | Self::ShowAgentSetup
            | Self::JournalSemanticSearch => Page::General,
            Self::Profile(_) | Self::WizardScan | Self::WizardTest => Page::Profiles,
            Self::Role(_) | Self::Embedding => Page::LocalPreferences,
            Self::Knowledge(_) => Page::Knowledge,
            Self::Hub(_) => Page::LocalModels,
            Self::Control(control) => control.page(),
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Features => "Enable AI features",
            Self::GlobalTimeout => "Global request timeout (minutes)",
            Self::CodeTextLanguage => "Default code text language",
            Self::ChatTerminalActions => "AI chat code blocks in the terminal",
            Self::AgentShortcut => "Enable terminal agent shortcuts",
            Self::AgentExecution => "Allow Agent command execution",
            Self::AgentConfirmation => "Require approval for mutating Agent command sets",
            Self::ShowAgentSetup => "Show terminal agent setup for submitted aliases",
            Self::JournalSemanticSearch => "Use local semantic ranking for journal questions",
            Self::WizardScan => "Find local model servers",
            Self::WizardTest => "Test profile connection",
            Self::Profile(setting) => setting.title(),
            Self::Role(role) => role.label(),
            Self::Embedding => "Default embedding model",
            Self::Knowledge(setting) => setting.title(),
            Self::Hub(setting) => setting.title(),
            Self::Control(control) => control.title(),
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Features => "safety enable disable",
            Self::GlobalTimeout => "limits timeouts",
            Self::CodeTextLanguage => "snippet script comments output messages translation",
            Self::ChatTerminalActions => "actions insert run copy save",
            Self::AgentShortcut => "agent ask plan alias input command name case OSC 133",
            Self::AgentExecution => "agent execute run swarm scheduled diagnostics permission",
            Self::AgentConfirmation => {
                "agent confirmation mutating command sets approve remaining always"
            }
            Self::ShowAgentSetup => "run dialog ask skip bypass startup prompt confirmation",
            Self::JournalSemanticSearch => "journal semantic search embedding lexical ranking BM25",
            Self::WizardScan => "scan discover loopback LM Studio Ollama",
            Self::WizardTest => "run fixed CLI test verify provider",
            Self::Profile(setting) => setting.keywords(),
            Self::Role(Role::Text) => "translation profile roles",
            Self::Role(_) => "profile roles",
            Self::Embedding => "rag knowledge vector",
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
            Self::Knowledge(_) => "",
            Self::Hub(HubSetting::Query) => "Hugging Face HuggingFace Hub GGUF search query",
            Self::Hub(_) => "Hugging Face HuggingFace Hub GGUF",
            Self::Control(_) => "",
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
    SignedIndex,
    IndexSignature,
    RuntimePackage,
    InstallRuntime,
    CheckRuntimeOnline,
    RuntimeOnlineChoice,
    DownloadRuntimeOnline,
    DiscardRuntimeDownload,
    CloseRuntimeRepair,
    RemoveRuntime,
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
}

impl Control {
    const ALL: [Self; 45] = [
        Self::LocalSetup,
        Self::LocalEndpoint,
        Self::RefreshModels,
        Self::InstalledModel,
        Self::TestModel,
        Self::SignedIndex,
        Self::IndexSignature,
        Self::RuntimePackage,
        Self::InstallRuntime,
        Self::CheckRuntimeOnline,
        Self::RuntimeOnlineChoice,
        Self::DownloadRuntimeOnline,
        Self::DiscardRuntimeDownload,
        Self::CloseRuntimeRepair,
        Self::RemoveRuntime,
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
    ];

    pub(super) fn page(self) -> Page {
        match self {
            Self::LocalSetup
            | Self::LocalEndpoint
            | Self::RefreshModels
            | Self::InstalledModel
            | Self::TestModel
            | Self::SignedIndex
            | Self::IndexSignature
            | Self::RuntimePackage
            | Self::InstallRuntime
            | Self::CheckRuntimeOnline
            | Self::RuntimeOnlineChoice
            | Self::DownloadRuntimeOnline
            | Self::DiscardRuntimeDownload
            | Self::CloseRuntimeRepair
            | Self::RemoveRuntime => Page::LocalModels,
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
            | Self::ExternalMemory => Page::Memory,
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
            | Self::ShortenSkill => Page::Skills,
        }
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
        )
    }

    fn title(self) -> &'static str {
        match self {
            Self::LocalSetup => "Set up local AI",
            Self::LocalEndpoint => "Local endpoint",
            Self::RefreshModels => "Refresh installed models",
            Self::InstalledModel => "Select an installed model",
            Self::TestModel => "Test model",
            Self::SignedIndex => "Signed index",
            Self::IndexSignature => "Index signature",
            Self::RuntimePackage => "Runtime package",
            Self::InstallRuntime => "Install signed runtime",
            Self::CheckRuntimeOnline => "Check signed runtime updates",
            Self::RuntimeOnlineChoice => "Choose a verified runtime package",
            Self::DownloadRuntimeOnline => "Download or resume selected runtime",
            Self::DiscardRuntimeDownload => "Discard saved runtime download",
            Self::CloseRuntimeRepair => "Cancel repair or return to model",
            Self::RemoveRuntime => "Remove installed runtime",
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
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnowledgeSetting {
    Name,
    Create,
    Update,
    Rebuild,
    VectorStorage,
    QdrantEndpoint,
    QdrantCollection,
    QdrantDimensions,
    QdrantAuthentication,
    QdrantCredentialVariable,
    SourcePath,
    AddFile,
    ReviewFolder,
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
    TestQuery,
    TestSearch,
}

impl KnowledgeSetting {
    const ALL: [Self; 25] = [
        Self::Name,
        Self::Create,
        Self::Update,
        Self::Rebuild,
        Self::VectorStorage,
        Self::QdrantEndpoint,
        Self::QdrantCollection,
        Self::QdrantDimensions,
        Self::QdrantAuthentication,
        Self::QdrantCredentialVariable,
        Self::SourcePath,
        Self::AddFile,
        Self::ReviewFolder,
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
        Self::TestQuery,
        Self::TestSearch,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Name => "New store name",
            Self::Create => "Create store",
            Self::Update => "Update index",
            Self::Rebuild => "Rebuild index",
            Self::VectorStorage => "Vector storage backend",
            Self::QdrantEndpoint => "Qdrant endpoint",
            Self::QdrantCollection => "Qdrant collection",
            Self::QdrantDimensions => "Qdrant embedding dimensions",
            Self::QdrantAuthentication => "Qdrant authentication",
            Self::QdrantCredentialVariable => "Qdrant API key variable",
            Self::SourcePath => "Source file or folder path",
            Self::AddFile => "Add source file",
            Self::ReviewFolder => "Review source folder",
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
            Self::TestQuery => "Test search query",
            Self::TestSearch => "Run knowledge search",
        }
    }

    pub(super) fn needs_store(self) -> bool {
        !matches!(self, Self::Name | Self::Create)
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
            | Self::MiniMaxRegion
            | Self::Web(_) => profiles::Section::Connection,
            Self::ModelMode
            | Self::Model
            | Self::AvailableModels
            | Self::ManagedModel
            | Self::Reasoning
            | Self::Vision
            | Self::PromptPreset => profiles::Section::Model,
            Self::SelectionLimit
            | Self::Timeout
            | Self::OutputLimit
            | Self::Tokenizer
            | Self::ReviewedAgentHttps
            | Self::ReviewedPlanningHttps
            | Self::Usage(_)
            | Self::RecordedUsage => profiles::Section::Limits,
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
            Self::AvailableModels => "discovery load suggested examples",
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

pub(super) fn results<'a>(
    query: &'a str,
    profile: Option<&'a profiles::Profile>,
) -> impl Iterator<Item = Target> + 'a {
    let targets = [
        Setting::Features,
        Setting::JournalSemanticSearch,
        Setting::GlobalTimeout,
        Setting::CodeTextLanguage,
        Setting::ChatTerminalActions,
        Setting::AgentShortcut,
        Setting::AgentExecution,
        Setting::AgentConfirmation,
        Setting::ShowAgentSetup,
    ]
    .into_iter()
    .chain(
        ProfileSetting::ALL
            .iter()
            .copied()
            .filter(move |setting| profile.is_some_and(|profile| setting.visible(profile)))
            .map(Setting::Profile),
    )
    .chain(Role::ALL.into_iter().map(Setting::Role))
    .chain([Setting::Embedding])
    .chain(KnowledgeSetting::ALL.into_iter().map(Setting::Knowledge))
    .chain(HubSetting::ALL.into_iter().map(Setting::Hub))
    .chain(Control::ALL.into_iter().map(Setting::Control))
    .map(Target::Setting)
    .chain(Page::ALL.into_iter().map(Target::Page));
    matching(query, targets)
}

pub(super) fn matching(
    query: &str,
    targets: impl Iterator<Item = Target>,
) -> impl Iterator<Item = Target> {
    let query = query.trim().to_lowercase();
    targets.filter(move |target| {
        let terms = match target {
            Target::Page(page) => page.keywords(),
            Target::Setting(setting) => setting.keywords(),
        };
        let haystack = format!("{} {terms}", target.label()).to_lowercase();
        !query.is_empty() && query.split_whitespace().all(|word| haystack.contains(word))
    })
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
    use crate::ai_manager::{Configuration, Dialog, Message};

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
            expected.insert(
                0,
                Target::Setting(Setting::Hub(HubSetting::DiscardPausedMlx)),
            );
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
