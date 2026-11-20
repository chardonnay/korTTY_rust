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
    Profile(ProfileSetting),
    Role(Role),
    Embedding,
    Knowledge(KnowledgeSetting),
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
            Self::Profile(_) => Page::Profiles,
            Self::Role(_) | Self::Embedding => Page::LocalPreferences,
            Self::Knowledge(_) => Page::Knowledge,
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
            Self::Profile(setting) => setting.title(),
            Self::Role(role) => role.label(),
            Self::Embedding => "Default embedding model",
            Self::Knowledge(setting) => setting.title(),
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
            Self::Control(_) => "",
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
    const ALL: [Self; 40] = [
        Self::LocalSetup,
        Self::LocalEndpoint,
        Self::RefreshModels,
        Self::InstalledModel,
        Self::TestModel,
        Self::SignedIndex,
        Self::IndexSignature,
        Self::RuntimePackage,
        Self::InstallRuntime,
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
}

impl KnowledgeSetting {
    const ALL: [Self; 4] = [Self::Name, Self::Create, Self::Update, Self::Rebuild];

    fn title(self) -> &'static str {
        match self {
            Self::Name => "New store name",
            Self::Create => "Create store",
            Self::Update => "Update index",
            Self::Rebuild => "Rebuild index",
        }
    }

    pub(super) fn needs_store(self) -> bool {
        matches!(self, Self::Update | Self::Rebuild)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfileSetting {
    Name,
    Default,
    Connection,
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
}

impl ProfileSetting {
    const ALL: [Self; 17] = [
        Self::Name,
        Self::Default,
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
    ];

    pub(super) fn section(self) -> profiles::Section {
        match self {
            Self::Name
            | Self::Default
            | Self::Connection
            | Self::Endpoint
            | Self::Trust
            | Self::Credential => profiles::Section::Connection,
            Self::ModelMode
            | Self::Model
            | Self::AvailableModels
            | Self::ManagedModel
            | Self::Reasoning
            | Self::Vision
            | Self::PromptPreset => profiles::Section::Model,
            Self::SelectionLimit | Self::Timeout | Self::OutputLimit | Self::Tokenizer => {
                profiles::Section::Limits
            }
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Name => "Profile name",
            Self::Default => "Use as default profile",
            Self::Connection => "Connection type",
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
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Connection => "provider HTTP llama.cpp CLI executable arguments stdin",
            Self::Endpoint => {
                "URL connection cloud provider preset OpenAI Anthropic Gemini Mistral DeepSeek Groq OpenRouter MiniMax"
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
            Self::Tokenizer => "token count quota estimate CL100K O200K P50K R50K",
            _ => "",
        }
    }

    fn visible(self, profile: &profiles::Profile) -> bool {
        if profile.policy_definition().is_some() {
            return matches!(
                self,
                Self::Name
                    | Self::Connection
                    | Self::Endpoint
                    | Self::Credential
                    | Self::Model
                    | Self::Default
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
            );
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
            _ => true,
        }
    }
}

pub(super) fn results<'a>(
    query: &'a str,
    profile: Option<&'a profiles::Profile>,
) -> impl Iterator<Item = Target> + 'a {
    let query = query.trim().to_lowercase();
    [
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
            .into_iter()
            .filter(move |setting| profile.is_some_and(|profile| setting.visible(profile)))
            .map(Setting::Profile),
    )
    .chain(Role::ALL.into_iter().map(Setting::Role))
    .chain([Setting::Embedding])
    .chain(KnowledgeSetting::ALL.into_iter().map(Setting::Knowledge))
    .chain(Control::ALL.into_iter().map(Setting::Control))
    .map(Target::Setting)
    .chain(Page::ALL.into_iter().map(Target::Page))
    .filter(move |target| {
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
        assert!(results("API ENDPOINT", Some(&profile)).any(|target| target == endpoint));
        profile.connection_mode = profiles::ConnectionMode::Managed;
        assert!(!results("API ENDPOINT", Some(&profile)).any(|target| target == endpoint));
        assert!(results("GGUF model", Some(&profile)).any(|target| target == model));
        assert!(results("Qwen prompt", Some(&profile)).any(|target| target == preset));
        profile.connection_mode = profiles::ConnectionMode::Cli;
        assert!(results("Qwen prompt", Some(&profile)).any(|target| target == preset));
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
        ] {
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
                _ => {
                    assert!(matches!(messages.as_slice(), [Message::Search(value)] if value == "2"))
                }
            }
        }
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
