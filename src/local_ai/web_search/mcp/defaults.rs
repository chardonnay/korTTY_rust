//! Shared MCP names; profiles can retain an explicit plugin override.

use serde::{Deserialize, Serialize};

use super::{Mode, Settings};

pub(crate) const MAX_DEFAULT_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Defaults {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) searxng_url: String,
    pub(crate) tavily_label: String,
    pub(crate) bright_data_label: String,
    pub(crate) brave_plugin: String,
    pub(crate) searxng_plugin: String,
    pub(crate) toolpack_plugin: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            searxng_url: String::new(),
            tavily_label: "tavily".into(),
            bright_data_label: "bright-data".into(),
            brave_plugin: String::new(),
            searxng_plugin: String::new(),
            toolpack_plugin: String::new(),
        }
    }
}

impl Defaults {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if [
            &self.searxng_url,
            &self.tavily_label,
            &self.bright_data_label,
            &self.brave_plugin,
            &self.searxng_plugin,
            &self.toolpack_plugin,
        ]
        .into_iter()
        .any(|value| value.len() > MAX_DEFAULT_BYTES)
        {
            return Err("Keep each Internet default within 4096 UTF-8 bytes.".into());
        }
        let normalized = self.normalized();
        if normalized.searxng_url.chars().any(char::is_control) {
            return Err("Use a SearXNG URL without control characters.".into());
        }
        if [&normalized.tavily_label, &normalized.bright_data_label]
            .into_iter()
            .any(|name| name.chars().any(char::is_control))
        {
            return Err("Use a text MCP server label without control characters. Leave it blank to use the built-in label.".into());
        }
        if [
            &normalized.brave_plugin,
            &normalized.searxng_plugin,
            &normalized.toolpack_plugin,
        ]
        .into_iter()
        .any(|id| !id.is_empty() && !Settings::valid_plugin_id(id))
        {
            return Err("Use a global MCP plugin ID in the form mcp/server-name, or leave it blank until configured.".into());
        }
        Ok(())
    }

    pub(crate) fn normalized(&self) -> Self {
        Self {
            searxng_url: java_text(&self.searxng_url, ""),
            tavily_label: java_text(&self.tavily_label, "tavily"),
            bright_data_label: java_text(&self.bright_data_label, "bright-data"),
            brave_plugin: java_text(&self.brave_plugin, ""),
            searxng_plugin: java_text(&self.searxng_plugin, ""),
            toolpack_plugin: java_text(&self.toolpack_plugin, ""),
        }
    }

    pub(crate) fn resolve(&self, settings: &Settings) -> Result<Settings, String> {
        self.validate()?;
        let defaults = self.normalized();
        settings.validate_inheritable()?;
        if settings.mode == Mode::Searxng && defaults.searxng_url.is_empty() {
            return Err("Set the global SearXNG URL in General → Internet access before using SearXNG. Configure the matching MCP server in LM Studio.".into());
        }
        let mut resolved = settings.clone();
        match settings.mode {
            Mode::Tavily => resolved.server_label.clone_from(&defaults.tavily_label),
            Mode::BrightData => resolved
                .server_label
                .clone_from(&defaults.bright_data_label),
            mode if settings.plugin.is_empty() => resolved.plugin.clone_from(match mode {
                Mode::Brave => &defaults.brave_plugin,
                Mode::Searxng => &defaults.searxng_plugin,
                Mode::Toolpack => &defaults.toolpack_plugin,
                _ => unreachable!("per-request integrations are handled above"),
            }),
            _ => {}
        }
        resolved.validate()?;
        Ok(resolved)
    }
}

fn java_text(value: &str, fallback: &str) -> String {
    // Java String.isBlank and String.trim use different whitespace sets.
    let blank = value.chars().all(|character| {
        matches!(character, '\t'..='\r' | '\u{1c}'..=' ' | '\u{1680}' | '\u{2000}'..='\u{2006}' |
            '\u{2008}'..='\u{200a}' | '\u{2028}'..='\u{2029}' | '\u{205f}' | '\u{3000}')
    });
    if blank {
        fallback.into()
    } else {
        value.trim_matches(|character| character <= '\u{20}').into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_plugin_defaults_are_live_without_rewriting_explicit_profiles() {
        let mut defaults = Defaults::default();
        let mut profile = Settings::new(Mode::Brave);
        assert!(profile.validate_inheritable().is_ok());
        assert!(profile.validate().is_err());
        assert!(defaults.resolve(&profile).is_err());
        defaults.brave_plugin = "mcp/first".into();
        assert_eq!(defaults.resolve(&profile).unwrap().plugin, "mcp/first");
        defaults.brave_plugin = "mcp/second".into();
        assert_eq!(defaults.resolve(&profile).unwrap().plugin, "mcp/second");
        assert!(profile.plugin.is_empty());
        profile.plugin = "mcp/explicit".into();
        assert_eq!(defaults.resolve(&profile).unwrap().plugin, "mcp/explicit");
        profile.plugin = "invalid value".into();
        assert!(profile.validate_inheritable().is_err());
    }

    #[test]
    fn names_and_tool_scopes_are_validated_after_default_resolution() {
        let mut defaults = Defaults::default();
        defaults.tavily_label = "reviewed-search".into();
        let settings = defaults.resolve(&Settings::new(Mode::Tavily)).unwrap();
        assert!(settings.review().contains("reviewed-search"));
        // Runtime-only labels must never turn into a profile-local override.
        assert_eq!(
            serde_json::to_value(&settings).unwrap(),
            serde_json::json!({"mode":"tavily"})
        );
        defaults.toolpack_plugin = "mcp/tools".into();
        let mut toolpack = Settings::new(Mode::Toolpack);
        assert!(defaults.resolve(&toolpack).is_err());
        toolpack.tool_names = "search_web".into();
        assert!(defaults.resolve(&toolpack).is_ok());
        defaults.tavily_label = "bad\nlabel".into();
        assert!(defaults.validate().is_err());
    }

    #[test]
    fn searxng_base_url_is_a_configuration_gate_not_a_page_endpoint() {
        let mut defaults = Defaults::default();
        defaults.searxng_plugin = "mcp/global-search".into();
        let mut profile = Settings::new(Mode::Searxng);
        profile.searxng_page = Some(super::super::SearxngPageSettings {
            endpoint: "https://pages.example.com/mcp".into(),
            bearer_env: None,
        });
        assert!(
            defaults
                .resolve(&profile)
                .err()
                .unwrap()
                .contains("global SearXNG URL")
        );
        defaults.searxng_url = "\u{2003}".into();
        assert!(defaults.resolve(&profile).is_err());
        defaults.searxng_url = "  https://search.example.com/  ".into();
        let resolved = defaults.resolve(&profile).unwrap();
        assert_eq!(resolved.plugin, "mcp/global-search");
        assert!(resolved.searxng_page == profile.searxng_page);
        assert!(
            !serde_json::to_string(&resolved)
                .unwrap()
                .contains("search.example.com")
        );
        defaults.searxng_url = "bad\nurl".into();
        assert!(defaults.validate().is_err());
    }

    #[test]
    fn global_labels_preserve_unicode_and_java_whitespace_normalization() {
        let defaults = Defaults {
            tavily_label: "  Search Zürich\u{a0}  ".into(),
            bright_data_label: "\u{2003}".into(),
            brave_plugin: " mcp/global ".into(),
            ..Default::default()
        };
        assert!(defaults.validate().is_ok());
        let normalized = defaults.normalized();
        assert_eq!(normalized.tavily_label, "Search Zürich\u{a0}");
        assert_eq!(normalized.bright_data_label, "bright-data");
        assert_eq!(normalized.brave_plugin, "mcp/global");
        assert!(
            defaults
                .resolve(&Settings::new(Mode::Tavily))
                .unwrap()
                .review()
                .contains("Search Zürich\u{a0}")
        );
    }

    #[test]
    #[ignore = "requires the private native loopback MCP fixture and synthetic environment credentials"]
    fn global_mcp_names_reach_the_wire_and_reject_old_provider_names() {
        use crate::local_ai::{Client, Profile, ProfileOptions};

        let endpoint = std::env::var("KORTTY_GLOBAL_MCP_ENDPOINT").unwrap();
        let profile = Profile::lm_studio(
            &endpoint,
            "fixture-model",
            ProfileOptions {
                credential_env: Some("KORTTY_GLOBAL_MCP_LOCAL_TOKEN".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let defaults = Defaults {
            searxng_url: "https://search.example.com/".into(),
            tavily_label: "global tavily Zürich".into(),
            bright_data_label: "global bright data".into(),
            brave_plugin: "mcp/global-brave".into(),
            searxng_plugin: "mcp/global-searxng".into(),
            toolpack_plugin: "mcp/global-toolpack".into(),
        };
        let client = Client::new(profile);
        for mode in [
            Mode::Tavily,
            Mode::BrightData,
            Mode::Brave,
            Mode::Searxng,
            Mode::Toolpack,
        ] {
            let mut settings = Settings::new(mode);
            if mode == Mode::Toolpack {
                settings.tool_names = "fixture_search".into();
            }
            let resolved = defaults.resolve(&settings).unwrap();
            let output = super::super::search(
                &client,
                &resolved,
                "reviewed public fixture",
                false,
                None,
                || Ok(()),
            )
            .unwrap();
            assert_eq!(output.len(), 1);
            assert_eq!(output[0].content, "Reviewed fixture excerpt");
            if matches!(mode, Mode::Tavily | Mode::Brave) {
                let error = super::super::search(
                    &client,
                    &resolved,
                    "stale provider fixture",
                    false,
                    None,
                    || Ok(()),
                )
                .err()
                .unwrap();
                assert!(error.contains("outside the reviewed scope"), "{error}");
            }
        }
    }
}
