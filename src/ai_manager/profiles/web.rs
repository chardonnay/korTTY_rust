//! Per-profile Internet integration settings.

use std::fmt;

use iced::Element;
use iced::widget::{TextInput, button, checkbox, column, text, text_input};

use crate::ai_manager::search::{ProfileSetting, Reveal, Setting, WebSetting};
use crate::local_ai::web_search::{
    MAX_PAGE_URL_BYTES, McpMode, McpSettings, PageProvider, SearxngPageSettings, page_provider,
};

use super::{Editor, Message, Profile, java_backup::JavaWebHint, view::choice};

const MAX_TOOL_NAMES_BYTES: usize = 520;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Choice {
    Disabled,
    Tavily,
    Mcp(McpMode),
}

impl Choice {
    const STANDARD: [Self; 2] = [Self::Disabled, Self::Tavily];
    const LM_STUDIO: [Self; 7] = [
        Self::Disabled,
        Self::Tavily,
        Self::Mcp(McpMode::Tavily),
        Self::Mcp(McpMode::BrightData),
        Self::Mcp(McpMode::Brave),
        Self::Mcp(McpMode::Searxng),
        Self::Mcp(McpMode::Toolpack),
    ];
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Tavily => formatter.write_str("Tavily · direct search"),
            Self::Mcp(mode) => mode.fmt(formatter),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Action {
    Select(Choice),
    UseImported,
    ImportedToolpackNames(String),
    Plugin(String),
    Tools(String),
    SearxngPages(bool),
    SearxngEndpoint(String),
    SearxngBearerEnv(String),
}

impl Profile {
    pub(in crate::ai_manager) fn has_web_search(&self) -> bool {
        self.tavily_search || self.lm_web.is_some()
    }

    pub(super) fn clear_web_search(&mut self) {
        self.tavily_search = false;
        self.lm_web = None;
    }

    fn web_choice(&self) -> Choice {
        if let Some(settings) = &self.lm_web {
            Choice::Mcp(settings.mode)
        } else if self.tavily_search {
            Choice::Tavily
        } else {
            Choice::Disabled
        }
    }

    fn imported_web_hint(&self) -> Option<&JavaWebHint> {
        self.java_web_hint.as_ref().filter(|hint| {
            !self.restore_review && !self.has_web_search() && self.supports_java_web_hint(hint)
        })
    }

    pub(in crate::ai_manager) fn searchable_web_setting(&self, setting: WebSetting) -> bool {
        if self.is_cli() || self.restore_review || self.policy_definition().is_some() {
            return false;
        }
        let mcp = self.lm_web.as_ref();
        let is_mode = |mode| mcp.is_some_and(|settings| settings.mode == mode);
        match setting {
            WebSetting::Provider => true,
            WebSetting::Plugin => mcp.is_some_and(|settings| settings.mode.uses_plugin()),
            WebSetting::ToolpackNames => is_mode(McpMode::Toolpack),
            WebSetting::ImportedToolpackNames => {
                matches!(self.imported_web_hint(), Some(JavaWebHint::Toolpack { .. }))
            }
            WebSetting::UseImported => self.imported_web_hint().is_some(),
            WebSetting::SearxngPages => is_mode(McpMode::Searxng),
            WebSetting::SearxngEndpoint | WebSetting::SearxngBearerEnv => {
                is_mode(McpMode::Searxng)
                    && mcp.is_some_and(|settings| settings.searxng_page.is_some())
            }
        }
    }
}

impl Editor {
    pub(in crate::ai_manager) fn can_reveal_web_setting(&self, setting: WebSetting) -> bool {
        !self.web_search_denied
            && self
                .searchable_profile()
                .is_some_and(|profile| profile.searchable_web_setting(setting))
    }

    pub(super) fn update_web_search(&mut self, action: Action) {
        if let Action::ImportedToolpackNames(names) = action {
            if self.selected().is_some_and(|profile| {
                matches!(
                    profile.imported_web_hint(),
                    Some(JavaWebHint::Toolpack { .. })
                ) && profile.credential().ok().flatten().is_some()
            }) {
                self.imported_toolpack_names = if names.len() <= MAX_TOOL_NAMES_BYTES
                    && !names.chars().any(char::is_control)
                {
                    names
                } else {
                    String::new()
                };
                self.status = None;
            }
            return;
        }
        let names = if matches!(action, Action::UseImported) {
            self.imported_toolpack_names.clone()
        } else {
            String::new()
        };
        let Some(profile) = self.selected_mut().filter(|profile| !profile.is_cli()) else {
            return;
        };
        match action {
            Action::UseImported => {
                let Some(hint) = profile.imported_web_hint() else {
                    return;
                };
                let mut settings = hint.mcp_settings();
                if let Some(toolpack) = settings
                    .as_mut()
                    .filter(|settings| settings.mode == McpMode::Toolpack)
                {
                    toolpack.tool_names = names;
                }
                let mut updated = profile.clone();
                updated.tavily_search = settings.is_none();
                updated.lm_web = settings;
                if let Err(error) = updated.validate_connection() {
                    self.set_status(error);
                    return;
                }
                *profile = updated;
            }
            Action::Select(mode) if mode != profile.web_choice() => match mode {
                Choice::Disabled => profile.clear_web_search(),
                Choice::Tavily => {
                    profile.clear_web_search();
                    profile.tavily_search = true;
                }
                Choice::Mcp(mode) if profile.allows_lm_studio_mcp() => {
                    profile.clear_web_search();
                    profile.lm_web = Some(McpSettings::new(mode));
                }
                _ => return,
            },
            Action::Plugin(plugin)
                if plugin.len() <= 68 && !plugin.chars().any(char::is_control) =>
            {
                if let Some(settings) = &mut profile.lm_web
                    && settings.mode.uses_plugin()
                {
                    settings.plugin = plugin;
                }
            }
            Action::Tools(tools)
                if tools.len() <= MAX_TOOL_NAMES_BYTES && !tools.chars().any(char::is_control) =>
            {
                if let Some(settings) = &mut profile.lm_web
                    && settings.mode == McpMode::Toolpack
                {
                    settings.tool_names = tools;
                }
            }
            Action::SearxngPages(enabled) => {
                if let Some(settings) = profile
                    .lm_web
                    .as_mut()
                    .filter(|settings| settings.mode == McpMode::Searxng)
                {
                    if enabled {
                        settings
                            .searxng_page
                            .get_or_insert_with(|| SearxngPageSettings {
                                endpoint: String::new(),
                                bearer_env: None,
                            });
                    } else {
                        settings.searxng_page = None;
                    }
                }
            }
            Action::SearxngEndpoint(endpoint)
                if endpoint.len() <= MAX_PAGE_URL_BYTES
                    && !endpoint.chars().any(char::is_control) =>
            {
                if let Some(page) = profile
                    .lm_web
                    .as_mut()
                    .and_then(|settings| settings.searxng_page.as_mut())
                {
                    page.endpoint = endpoint;
                }
            }
            Action::SearxngBearerEnv(name)
                if name.len() <= 128 && !name.chars().any(char::is_control) =>
            {
                if let Some(page) = profile
                    .lm_web
                    .as_mut()
                    .and_then(|settings| settings.searxng_page.as_mut())
                {
                    page.bearer_env = (!name.is_empty()).then_some(name);
                }
            }
            _ => return,
        }
        self.imported_toolpack_names.clear();
        self.status = None;
    }

    pub(super) fn web_search_view<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        if self.web_search_denied {
            return text("Managed policy blocks web search.").size(13).into();
        }
        let mode = profile.web_choice();
        let select = |mode| Message::Web(Action::Select(mode));
        let picker = if profile.allows_lm_studio_mcp() {
            choice(Choice::LM_STUDIO, mode, editable, select)
        } else {
            choice(Choice::STANDARD, mode, editable, select)
        };
        let mut fields = column![reveal.wrap(
            target(WebSetting::Provider),
            column![text("Web search"), picker].spacing(6)
        )]
        .spacing(6);
        if let Some(JavaWebHint::Toolpack { plugin }) = profile.imported_web_hint() {
            let has_token_name = profile.credential().ok().flatten().is_some();
            fields = fields
                .push(text(format!("Imported LM Studio Toolpack · {plugin}")).size(13))
                .push(text(if has_token_name {
                    "Enter one to eight exact tool names from this installed plugin."
                } else {
                    "Set the LM Studio API token environment variable in this profile to review the imported plugin."
                }).size(13))
                .push(
                    input(reveal, WebSetting::ImportedToolpackNames, editable && has_token_name, text_input("Allowed Toolpack names, separated by spaces", &self.imported_toolpack_names)
                        .on_input_maybe((editable && has_token_name).then_some(|tools| {
                            Message::Web(Action::ImportedToolpackNames(tools))
                        }))),
                )
                .push(reveal.action(target(WebSetting::UseImported), button("Use imported Toolpack").on_press_maybe(
                    (editable && has_token_name && !self.imported_toolpack_names.trim().is_empty())
                        .then_some(Message::Web(Action::UseImported)),
                )));
        } else if let Some(hint) = profile.imported_web_hint() {
            let label = hint.mcp_settings().map_or_else(
                || "Tavily · direct search".into(),
                |settings| format!("{}\n{}", settings.mode, settings.review()),
            );
            fields = fields.push(text(format!("Imported web integration · {label}")).size(13));
            fields = fields.push(
                reveal.action(
                    target(WebSetting::UseImported),
                    button("Use imported integration")
                        .on_press_maybe(editable.then_some(Message::Web(Action::UseImported))),
                ),
            );
        }
        match mode {
            Choice::Disabled => {}
            Choice::Tavily => {
                fields = fields.push(
                    text(
                        "Direct search and page extraction use api.tavily.com and TAVILY_API_KEY.",
                    )
                    .size(13),
                );
            }
            Choice::Mcp(mode) => {
                let settings = profile.lm_web.as_ref().expect("selected MCP mode");
                fields = fields.push(text(mode.setup()).size(13));
                if mode == McpMode::Searxng {
                    fields = fields.push(searxng_page_fields(settings, editable, reveal));
                } else if let Some(provider) = page_provider(Some(settings)) {
                    let detail = match provider {
                        PageProvider::Tavily => {
                            "Read a webpage submits the reviewed URL to Tavily Extract using TAVILY_API_KEY."
                        }
                        PageProvider::BrightData => {
                            "Read a webpage submits the reviewed URL to the Bright Data MCP server using BRIGHT_DATA_API_TOKEN."
                        }
                        PageProvider::Searxng(_) => {
                            "Read a webpage submits the reviewed URL to the configured SearXNG MCP endpoint."
                        }
                    };
                    fields = fields.push(text(detail).size(13));
                }
                if mode.uses_plugin() {
                    fields = fields.push(input(
                        reveal,
                        WebSetting::Plugin,
                        editable,
                        text_input(
                            "Blank uses the global ID; mcp/server-name overrides it",
                            &settings.plugin,
                        )
                        .on_input_maybe(editable.then_some(|id| Message::Web(Action::Plugin(id)))),
                    ));
                }
                if mode == McpMode::Toolpack {
                    fields = fields.push(input(
                        reveal,
                        WebSetting::ToolpackNames,
                        editable,
                        text_input(
                            "Allowed Toolpack names, separated by spaces",
                            &settings.tool_names,
                        )
                        .on_input_maybe(
                            editable.then_some(|tools| Message::Web(Action::Tools(tools))),
                        ),
                    ));
                }
            }
        }
        fields.into()
    }
}

fn target(setting: WebSetting) -> Setting {
    Setting::Profile(ProfileSetting::Web(setting))
}

fn input<'a>(
    reveal: &Reveal,
    setting: WebSetting,
    editable: bool,
    input: TextInput<'a, Message>,
) -> Element<'a, Message> {
    let setting = target(setting);
    reveal.wrap(setting, reveal.input(setting, editable, input))
}

fn searxng_page_fields<'a>(
    settings: &'a McpSettings,
    editable: bool,
    reveal: &Reveal,
) -> Element<'a, Message> {
    let mut fields = column![
        reveal.wrap(
            target(WebSetting::SearxngPages),
            checkbox(settings.searxng_page.is_some())
                .label("Read webpages through a SearXNG MCP endpoint")
                .on_toggle_maybe(
                    editable.then_some(|enabled| Message::Web(Action::SearxngPages(enabled)))
                )
        )
    ]
    .spacing(6);
    if let Some(page) = &settings.searxng_page {
        fields = fields
            .push(
                text("Page requests call web_url_read at the full MCP endpoint you enter.")
                    .size(13),
            )
            .push(input(
                reveal,
                WebSetting::SearxngEndpoint,
                editable,
                text_input("SearXNG MCP endpoint URL", &page.endpoint).on_input_maybe(
                    editable.then_some(|url| Message::Web(Action::SearxngEndpoint(url))),
                ),
            ))
            .push(input(
                reveal,
                WebSetting::SearxngBearerEnv,
                editable,
                text_input(
                    "Bearer token environment variable (optional)",
                    page.bearer_env.as_deref().unwrap_or_default(),
                )
                .on_input_maybe(
                    editable.then_some(|name| Message::Web(Action::SearxngBearerEnv(name))),
                ),
            ));
        if !page.endpoint.is_empty() {
            let destination = page.review_destination().map_or_else(
                |error| error,
                |url| format!("Page request destination · {url}"),
            );
            fields = fields.push(text(destination).size(13));
        }
    }
    fields.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::profiles::{ConnectionMode, Field, java_backup};

    #[test]
    fn web_setting_search_requires_the_active_integration_and_respects_policy() {
        use std::{path::PathBuf, sync::Arc};

        use iced::{Font, Renderer, Size, keyboard::Key};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::ai_manager::{Configuration, Dialog, Message as ManagerMessage, search::Target};
        use crate::settings_search::test_input;

        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        )
        .with_web_search_policy(true);
        let send = |dialog: &mut Dialog, message| dialog.update(ManagerMessage::Profiles(message));
        let _ = send(&mut dialog, Message::Add);
        let _ = send(
            &mut dialog,
            Message::ConnectionMode(ConnectionMode::LmStudio),
        );
        let _ = send(&mut dialog, Message::TrustLocal(true));
        let _ = send(
            &mut dialog,
            Message::Edit(Field::Credential, "KORTTY_LM_STUDIO_FIXTURE_TOKEN".into()),
        );
        let _ = send(
            &mut dialog,
            Message::Web(Action::Select(Choice::Mcp(McpMode::Searxng))),
        );
        let endpoint = target(WebSetting::SearxngEndpoint);
        let _ = dialog.update(ManagerMessage::Search("searxng endpoint url".into()));
        assert!(
            !dialog
                .search_results()
                .any(|result| result == Target::Setting(endpoint))
        );

        let _ = send(&mut dialog, Message::Web(Action::SearxngPages(true)));
        let draft = dialog.profiles.catalog().clone();
        let reveal = dialog.update(ManagerMessage::SearchChoose);
        assert_eq!(dialog.revealed.setting, Some(endpoint));
        assert!(dialog.profiles.catalog() == &draft);
        assert!(dialog.request.is_none());
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            dialog.view(Vec::new(), Vec::new()),
            Size::new(760.0, 360.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, reveal);
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Character("x".into()),
            Some("x"),
        );
        assert!(
            matches!(messages.as_slice(), [ManagerMessage::Profiles(Message::Web(Action::SearxngEndpoint(value)))] if value == "x")
        );
        drop(interface);

        let _ = send(
            &mut dialog,
            Message::Web(Action::Select(Choice::Mcp(McpMode::Toolpack))),
        );
        let _ = dialog.update(ManagerMessage::Search("toolpack names".into()));
        let names = target(WebSetting::ToolpackNames);
        assert_eq!(dialog.search_results().next(), Some(Target::Setting(names)));
        dialog = dialog.with_web_search_policy(false);
        assert!(
            !dialog
                .search_results()
                .any(|result| result == Target::Setting(names))
        );
        let _ = dialog.update(ManagerMessage::Reveal(Target::Setting(names)));
        assert!(dialog.revealed.setting.is_none());
    }

    #[test]
    fn searxng_page_settings_are_explicit_and_scoped_to_the_selected_web_integration() {
        let (catalog, _) = java_backup::decode(
            br#"<globalSettings><aiSearxngMcpPluginId>mcp/searxng</aiSearxngMcpPluginId>
                <aiSearxngUrl>https://example.com/search</aiSearxngUrl><aiProfiles>
                <profile><id>java-local</id><name>Local MCP</name><model>chat-model</model>
                  <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                  <internetAccessMode>SEARXNG_MCP</internetAccessMode></profile>
                </aiProfiles></globalSettings>"#,
        )
        .unwrap();
        let mut editor = Editor::new(catalog);
        editor.update(Message::TrustLocal(true), true);
        editor.update(
            Message::Edit(Field::Credential, "KORTTY_LM_STUDIO_FIXTURE_TOKEN".into()),
            true,
        );
        editor.update(Message::Web(Action::UseImported), true);
        let settings = editor.selected().unwrap().lm_web.as_ref().unwrap();
        assert_eq!(settings.mode, McpMode::Searxng);
        assert!(settings.searxng_page.is_none());
        editor.update(Message::Web(Action::SearxngPages(true)), false);
        assert!(
            editor
                .selected()
                .unwrap()
                .lm_web
                .as_ref()
                .unwrap()
                .searxng_page
                .is_none()
        );
        editor.update(Message::Web(Action::SearxngPages(true)), true);
        assert!(editor.validated_catalog().is_err());
        editor.update(
            Message::Web(Action::SearxngEndpoint(
                "https://MCP.example.com/custom-mcp".into(),
            )),
            true,
        );
        editor.update(
            Message::Web(Action::SearxngBearerEnv(
                "KORTTY_SEARXNG_FIXTURE_TOKEN".into(),
            )),
            true,
        );
        let catalog = editor.validated_catalog().unwrap();
        let profile = catalog.profile(1).unwrap();
        let page = profile
            .lm_web
            .as_ref()
            .unwrap()
            .searxng_page
            .as_ref()
            .unwrap();
        assert_eq!(
            page.review_destination().unwrap(),
            "https://mcp.example.com/custom-mcp"
        );
        assert_eq!(
            page.bearer_env.as_deref(),
            Some("KORTTY_SEARXNG_FIXTURE_TOKEN")
        );

        editor.update(Message::Web(Action::SearxngPages(false)), true);
        assert!(editor.validated_catalog().is_ok());
        assert!(
            editor
                .selected()
                .unwrap()
                .lm_web
                .as_ref()
                .unwrap()
                .searxng_page
                .is_none()
        );
        editor.update(
            Message::Web(Action::Select(Choice::Mcp(McpMode::Brave))),
            true,
        );
        editor.update(Message::Web(Action::SearxngPages(true)), true);
        assert!(
            editor
                .selected()
                .unwrap()
                .lm_web
                .as_ref()
                .unwrap()
                .searxng_page
                .is_none()
        );
    }

    #[test]
    fn imported_web_integration_requires_endpoint_trust_and_explicit_selection() {
        let (catalog, _) = java_backup::decode(
            br#"<globalSettings><aiProfiles>
                <profile><id>java-local</id><name>Local MCP</name><model>chat-model</model>
                  <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                  <internetAccessMode>BRIGHT_DATA_WEB_MCP</internetAccessMode></profile>
            </aiProfiles></globalSettings>"#,
        )
        .unwrap();
        let mut editor = Editor::new(catalog);
        let use_imported = Message::Web(Action::UseImported);
        editor.update(use_imported.clone(), true);
        assert!(!editor.selected().unwrap().has_web_search());
        editor.update(Message::TrustLocal(true), true);
        editor.update(use_imported.clone(), false);
        assert!(!editor.selected().unwrap().has_web_search());
        editor.update(use_imported.clone(), true);
        let active = editor.selected().unwrap();
        assert_eq!(active.lm_web.as_ref().unwrap().mode, McpMode::BrightData);
        let mut portable = editor.catalog().clone();
        portable.prepare_backup();
        let pending = portable.profile(1).unwrap();
        assert!(pending.restore_review);
        assert!(!pending.has_web_search());
        assert!(pending.java_web_hint.is_some());

        editor.update(Message::Web(Action::Select(Choice::Tavily)), true);
        editor.update(use_imported.clone(), true);
        assert!(editor.selected().unwrap().tavily_search);
        editor.update(Message::TrustLocal(false), true);
        let revoked = editor.selected().unwrap();
        assert!(revoked.restore_review);
        assert!(!revoked.has_web_search());
        assert!(revoked.validate().is_ok());
        editor.update(Message::TrustLocal(true), true);
        editor.update(use_imported.clone(), true);
        assert!(editor.selected().unwrap().has_web_search());
        editor.update(
            Message::Edit(Field::Endpoint, "127.0.0.1:4321".into()),
            true,
        );
        editor.update(Message::TrustLocal(true), true);
        editor.update(use_imported, true);
        let changed = editor.selected().unwrap();
        assert!(!changed.has_web_search());
        assert!(changed.java_web_hint.is_none());
    }

    #[test]
    fn imported_web_integration_is_cleared_on_https_endpoint_or_transport_changes() {
        let (catalog, _) = java_backup::decode(
            br#"<globalSettings><aiProfiles>
                <profile><id>java-remote</id><name>Remote AI</name><model>chat-model</model>
                  <apiUrl>https://api.openai.com/v1/chat/completions</apiUrl>
                  <internetAccessMode>KORTTY_TAVILY_TOOL</internetAccessMode></profile>
            </aiProfiles></globalSettings>"#,
        )
        .unwrap();
        let mut editor = Editor::new(catalog);
        editor.update(Message::TrustRemote(true), true);
        editor.update(Message::Web(Action::UseImported), true);
        assert!(editor.selected().unwrap().tavily_search);
        editor.update(Message::TrustRemote(false), true);
        assert!(!editor.selected().unwrap().has_web_search());
        editor.update(Message::TrustRemote(true), true);
        assert!(!editor.selected().unwrap().has_web_search());
        editor.update(Message::Web(Action::UseImported), true);
        assert!(editor.selected().unwrap().tavily_search);

        let mut changed_transport = Editor::new(editor.catalog().clone());
        changed_transport.update(Message::ConnectionMode(ConnectionMode::External), true);
        let changed = changed_transport.selected().unwrap();
        assert!(!changed.has_web_search());
        assert!(changed.java_web_hint.is_none());

        editor.update(
            Message::Edit(Field::Endpoint, "https://example.com/v1".into()),
            true,
        );
        editor.update(Message::TrustRemote(true), true);
        let changed = editor.selected().unwrap();
        assert!(!changed.has_web_search());
        assert!(changed.java_web_hint.is_none());
    }

    #[test]
    fn imported_lm_studio_https_mcp_requires_trust_and_has_to_be_selected_again_after_revocation() {
        let endpoint = "https://localhost:1234/api/v1/chat";
        let (catalog, _) = java_backup::decode(
            format!(
                r#"<globalSettings><aiProfiles>
                    <profile><id>java-local</id><name>HTTPS MCP</name>
                      <apiUrl>{endpoint}</apiUrl><modelSelectionMode>AUTO</modelSelectionMode>
                      <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode></profile>
                    </aiProfiles></globalSettings>"#
            )
            .as_bytes(),
        )
        .unwrap();
        let mut editor = Editor::new(catalog);
        let use_imported = Message::Web(Action::UseImported);
        editor.update(Message::TrustLocal(true), true);
        editor.update(use_imported.clone(), true);
        assert!(editor.selected().unwrap().restore_review);
        assert!(!editor.selected().unwrap().has_web_search());

        editor.update(Message::TrustRemote(true), true);
        assert!(!editor.selected().unwrap().has_web_search());
        editor.update(use_imported.clone(), true);
        let active = editor.validated_catalog().unwrap();
        assert_eq!(
            active.profile(1).unwrap().lm_web.as_ref().unwrap().mode,
            McpMode::Tavily
        );
        assert_eq!(active.profile(1).unwrap().destination().unwrap(), endpoint);
        assert!(!active.profile(1).unwrap().reviewed_agent_https);

        editor.update(Message::TrustRemote(false), true);
        assert!(editor.selected().unwrap().restore_review);
        assert!(!editor.selected().unwrap().has_web_search());
        editor.update(Message::TrustRemote(true), true);
        assert!(!editor.selected().unwrap().has_web_search());
        editor.update(use_imported, true);
        assert!(editor.selected().unwrap().has_web_search());

        editor.update(
            Message::Edit(Field::Endpoint, "https://127.0.0.1:1234/api/v1/chat".into()),
            true,
        );
        let changed = editor.selected().unwrap();
        assert!(!changed.has_web_search());
        assert!(!changed.trusted_remote_endpoint);
        assert!(changed.java_web_hint.is_none());
    }

    #[test]
    fn imported_toolpack_requires_a_trusted_endpoint_token_and_exact_user_entered_tools() {
        for endpoint in [
            "http://127.0.0.1:1234/api/v1/chat",
            "https://localhost:1234/api/v1/chat",
        ] {
            let (catalog, _) = java_backup::decode(
                format!(
                    r#"<globalSettings>
                      <aiLmStudioToolpackMcpPluginId>mcp/installed-tools</aiLmStudioToolpackMcpPluginId>
                      <aiProfiles><profile><id>toolpack</id><name>Toolpack</name>
                        <apiUrl>{endpoint}</apiUrl><model>fixture</model>
                        <encryptedApiKey>omitted-token</encryptedApiKey>
                        <internetAccessMode>LM_STUDIO_TOOLPACK</internetAccessMode>
                      </profile></aiProfiles></globalSettings>"#
                )
                .as_bytes(),
            )
            .unwrap();
            let mut editor = Editor::new(catalog);
            let source = editor.selected().unwrap().java_source.clone();
            let trust = if endpoint.starts_with("https:") {
                Message::TrustRemote
            } else {
                Message::TrustLocal
            };
            let names = |value: &str| Message::Web(Action::ImportedToolpackNames(value.into()));
            let adopt = Message::Web(Action::UseImported);
            let token = || Message::Edit(Field::Credential, "KORTTY_TOOLPACK_TOKEN".into());
            editor.update(names("before_trust"), true);
            editor.update(adopt.clone(), true);
            assert!(editor.selected().unwrap().restore_review);
            assert!(editor.selected().unwrap().credential_env.is_empty());
            assert!(!editor.selected().unwrap().has_web_search());

            editor.update(trust(true), true);
            editor.update(names("before_token"), true);
            editor.update(adopt.clone(), true);
            assert!(editor.imported_toolpack_names.is_empty());
            assert!(!editor.selected().unwrap().has_web_search());
            editor.update(token(), true);
            editor.update(names("from_read_only_editor"), false);
            assert!(editor.imported_toolpack_names.is_empty());
            for invalid in ["", "*", "search search", "a b c d e f g h i", "bad/tool"] {
                editor.update(names(invalid), true);
                editor.update(adopt.clone(), true);
                assert!(!editor.selected().unwrap().has_web_search());
                assert!(editor.status.is_some());
            }

            editor.update(names("Search page.read"), true);
            editor.update(adopt.clone(), false);
            assert!(!editor.selected().unwrap().has_web_search());
            let draft = editor.validated_catalog().unwrap();
            let serialized = serde_json::to_string(&draft).unwrap();
            assert!(!serialized.contains("Search page.read"));
            assert!(serialized.contains("mcp/installed-tools"));
            editor.update(adopt.clone(), true);
            let active = editor.validated_catalog().unwrap();
            let profile = active.profile(1).unwrap();
            let settings = profile.lm_web.as_ref().unwrap();
            assert_eq!(settings.mode, McpMode::Toolpack);
            assert_eq!(settings.plugin, "mcp/installed-tools");
            assert_eq!(settings.tool_names, "Search page.read");
            assert_eq!(profile.java_source, source);
            assert_eq!(profile.destination().unwrap(), endpoint);

            let mut portable = active;
            portable.prepare_backup();
            let draft = portable.profile(1).unwrap();
            assert!(draft.restore_review && !draft.has_web_search());
            assert!(draft.java_web_hint.is_some());
            assert!(draft.credential_env.is_empty());

            editor.update(trust(false), true);
            editor.update(trust(true), true);
            editor.update(token(), true);
            editor.update(adopt.clone(), true);
            assert!(!editor.selected().unwrap().has_web_search());
            editor.update(names("previous_credential"), true);
            editor.update(
                Message::Edit(Field::Credential, "SECOND_TOOLPACK_TOKEN".into()),
                true,
            );
            editor.update(adopt.clone(), true);
            assert!(!editor.selected().unwrap().has_web_search());
            editor.update(names("previous_policy"), true);
            editor.deny_web_search(true);
            editor.update(names("blocked_by_policy"), true);
            editor.deny_web_search(false);
            editor.update(adopt.clone(), true);
            assert!(!editor.selected().unwrap().has_web_search());
            editor.update(names("previous_endpoint"), true);
            editor.update(
                Message::Edit(Field::Endpoint, endpoint.replace(":1234", ":4321")),
                true,
            );
            editor.update(trust(true), true);
            editor.update(token(), true);
            editor.update(adopt, true);
            let changed = editor.selected().unwrap();
            assert!(!changed.has_web_search());
            assert!(changed.java_web_hint.is_none());
        }
    }
}
