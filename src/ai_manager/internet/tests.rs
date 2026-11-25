use std::{path::PathBuf, sync::Arc};

use serde_json::json;

use super::*;
use crate::ai_manager::{Configuration, GeneralSection, ProfileId};

fn configuration() -> Configuration {
    serde_json::from_value(json!({
        "general":{"internet":{"brave_plugin":"mcp/global"}},
        "profiles":{"profiles":[{
            "id":7,"name":"Internet fixture","endpoint":"127.0.0.1:12345","model":"fixture",
            "connection_mode":"lm-studio","trusted_local_endpoint":true,
            "credential_env":"KORTTY_GLOBAL_MCP_LOCAL_TOKEN","lm_web":{"mode":"brave"}
        }],"default":7,"last_id":7}
    }))
    .unwrap()
}

#[test]
fn saved_global_mcp_defaults_resolve_profiles_and_revoke_old_web_reviews() {
    let root = crate::snippets::tests::Profile::new();
    let path = root.0.clone();
    let config = configuration();
    config
        .save_against(&path, &Configuration::default())
        .unwrap();
    let config = Configuration::load_from(&root.0).unwrap();
    let id = ProfileId::from(7_u64);
    assert_eq!(
        config
            .web_search_profile(id, false)
            .unwrap()
            .2
            .unwrap()
            .plugin,
        "mcp/global"
    );
    let scope = crate::workspace::terminal_ai::web::Scope::new(Some(&config), id, true).unwrap();
    assert!(scope.require_current().is_ok());
    let mut changed = config.clone();
    changed.general.internet.brave_plugin = "mcp/changed".into();
    changed.save_against(&path, &config).unwrap();
    assert!(scope.require_current().is_err());
    assert_ne!(
        config.agent_profile_revision(id),
        changed.agent_profile_revision(id)
    );
    assert_eq!(
        changed
            .web_search_profile(id, false)
            .unwrap()
            .2
            .unwrap()
            .plugin,
        "mcp/changed"
    );
    assert!(
        changed
            .profiles
            .profile(id)
            .unwrap()
            .lm_web
            .as_ref()
            .unwrap()
            .plugin
            .is_empty()
    );
    changed.general.internet.brave_plugin.clear();
    assert!(changed.web_search_profile(id, false).is_none());
    config.validate().unwrap();
}

#[test]
fn global_internet_search_reveals_drafts_and_locks_both_input_paths() {
    let mut dialog = Dialog::new(
        Arc::new(Configuration::default()),
        PathBuf::new(),
        Vec::new(),
        true,
        None,
    );
    for field in Field::ALL {
        let setting = Setting::Internet(field);
        let _ = dialog.update(Message::Search(setting.title().into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.revealed.setting, Some(setting));
        assert_eq!(dialog.general_section, GeneralSection::Internet);
        let value = match field {
            Field::SearxngUrl => "https://search.example.com/",
            Field::TavilyLabel | Field::BrightDataLabel => "global-search",
            _ => "mcp/global",
        };
        let _ = dialog.update(Message::Internet(field, value.into()));
        assert_eq!(field.value(&dialog.general.internet), value);
    }
    let before = dialog.general.clone();
    let _ = dialog.update(Message::Search("LM Studio Toolpack".into()));
    let _ = dialog.update(Message::SearchChoose);
    assert_eq!(
        dialog.revealed.setting,
        Some(Setting::Internet(Field::ToolpackPlugin))
    );
    dialog = dialog.with_web_search_policy(false);
    let _ = dialog.update(Message::Internet(Field::BravePlugin, "mcp/denied".into()));
    assert!(dialog.general == before);
    let _ = dialog.update(Message::Save);
    assert!(!dialog.saving);
    assert!(dialog.notice.contains("Managed policy"));
    let mut readonly = Dialog::new(
        Arc::new(Configuration::default()),
        PathBuf::new(),
        Vec::new(),
        false,
        None,
    );
    let _ = readonly.update(Message::Internet(Field::TavilyLabel, "denied".into()));
    assert!(!readonly.dirty());
}

#[test]
fn global_defaults_do_not_change_legacy_profile_revisions_until_used() {
    let mut config = configuration();
    config.general.internet = Default::default();
    let id = ProfileId::from(7_u64);
    let legacy = config.agent_profile_revision(id).unwrap();
    let bytes = serde_json::to_vec(&config).unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("\"internet\""));
    config.general.internet.tavily_label = "custom-label".into();
    assert_ne!(config.agent_profile_revision(id).unwrap(), legacy);
    config.general.internet.tavily_label = "bad\nlabel".into();
    assert!(config.validate().is_err());
}

#[test]
fn global_internet_policy_remains_locked_after_successful_save() {
    let root = crate::snippets::tests::Profile::new();
    let saved = Arc::new(Configuration::default());
    let mut dialog = Dialog::new(Arc::clone(&saved), root.0.clone(), Vec::new(), true, None)
        .with_web_search_policy(false);
    let _ = dialog.update(Message::Saved(Ok((saved, None))));
    assert!(!dialog.profiles.web_search_allowed());
    let before = dialog.general.clone();
    let _ = dialog.update(Message::Internet(
        Field::BravePlugin,
        "mcp/forbidden".into(),
    ));
    assert!(dialog.general == before);
    assert!(!dialog.dirty());
}

#[test]
fn saved_searxng_url_is_required_and_revokes_reviews_without_rerouting_pages() {
    let root = crate::snippets::tests::Profile::new();
    let mut config = configuration();
    config.profiles = serde_json::from_value(json!({"profiles":[{
        "id":7,"name":"SearXNG fixture","endpoint":"127.0.0.1:12345","model":"fixture",
        "connection_mode":"lm-studio","trusted_local_endpoint":true,
        "credential_env":"KORTTY_GLOBAL_MCP_LOCAL_TOKEN",
        "lm_web":{"mode":"searxng","plugin":"mcp/explicit", "searxng_page":{
            "endpoint":"https://pages.example.com/mcp"
        }}
    }],"default":7,"last_id":7}))
    .unwrap();
    let id = ProfileId::from(7_u64);
    let original_profile = serde_json::to_value(&config.profiles).unwrap();
    assert!(config.web_search_profile(id, false).is_none());
    config.general.internet.searxng_url = "  https://search.example.com/  ".into();
    config.general.internet = config.general.internet.normalized();
    config
        .save_against(&root.0, &Configuration::default())
        .unwrap();
    let config = Configuration::load_from(&root.0).unwrap();
    assert_eq!(
        config.general.internet.searxng_url,
        "https://search.example.com/"
    );
    let scope = crate::workspace::terminal_ai::web::Scope::new(Some(&config), id, true).unwrap();
    assert!(scope.require_current().is_ok());
    assert_eq!(
        scope.page_review().as_deref(),
        Some("web_url_read · https://pages.example.com/mcp")
    );
    assert!(scope.search_review().contains("mcp/explicit"));
    assert!(!scope.search_review().contains("search.example.com"));
    let mut changed = config.clone();
    changed.general.internet.searxng_url = "https://other-search.example.com/".into();
    changed.save_against(&root.0, &config).unwrap();
    assert!(scope.require_current().is_err());
    assert_eq!(
        serde_json::to_value(&changed.profiles).unwrap(),
        original_profile
    );
    changed.general.internet.searxng_url.clear();
    assert!(changed.web_search_profile(id, false).is_none());
}
