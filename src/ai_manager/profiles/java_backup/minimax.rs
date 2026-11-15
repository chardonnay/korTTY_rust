//! Recognize the Java preset that can use the reviewed native MiniMax adapter.

use super::cli_profile::Settings;
use crate::local_ai::cli::catalog::Provider;

const JAVA_TEXT_CHAT: [&str; 9] = [
    "text",
    "chat",
    "--message",
    "{prompt}",
    "--output",
    "text",
    "--no-color",
    "--quiet",
    "--non-interactive",
];

pub(super) fn draft(provider: Option<Provider>, arguments: &[String]) -> Option<Settings> {
    (provider == Some(Provider::MiniMax) && arguments.iter().map(String::as_str).eq(JAVA_TEXT_CHAT))
        .then(Settings::java_minimax_draft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::profiles::{ConnectionMode, Reasoning};

    fn java_profile(
        provider: &str,
        arguments: &str,
    ) -> (super::super::Catalog, super::super::Report) {
        let xml = format!(
            r#"<globalSettings>
            <sessionJournalAiProfileId>java-mmx</sessionJournalAiProfileId>
            <automationJournalAiProfileId>java-mmx</automationJournalAiProfileId>
            <aiProfiles><profile><id>java-mmx</id><name>MiniMax fixture</name>
            <connectionMode>LOCAL_CLI</connectionMode><cliProviderId>{provider}</cliProviderId>
            <cliExecutablePath>C:\a path\untrusted-mmx.cmd</cliExecutablePath>
            <model>unused-java-model</model><reasoningEffort>XHIGH</reasoningEffort>
            <cliArgumentsTemplate>{arguments}</cliArgumentsTemplate></profile></aiProfiles>
            </globalSettings>"#
        );
        super::super::decode(xml.as_bytes()).unwrap()
    }

    #[test]
    fn java_minimax_preset_import_keeps_source_but_requires_a_local_executable_and_region_review() {
        let (catalog, report) = java_profile("minimax", &JAVA_TEXT_CHAT.join("\n"));
        assert_eq!(report.cli_imported, 1);
        let profile = &catalog.profiles[0];
        assert!(profile.connection_mode == ConnectionMode::Cli);
        assert!(profile.model.is_empty());
        assert_eq!(profile.reasoning, Reasoning::Default);
        assert!(profile.restore_review);
        assert!(profile.validate_connection().is_err());
        let cli = profile.cli.as_ref().unwrap();
        assert!(cli.is_java_draft());
        let saved = serde_json::to_value(cli).unwrap();
        assert_eq!(saved["provider"], "minimax");
        assert_eq!(saved["path"], "");
        assert_eq!(saved["arguments"], serde_json::json!([]));
        assert!(saved["fingerprint"].is_null());
        assert_eq!(saved["minimax"]["module"], "");
        assert_eq!(saved["minimax"]["config"], "");
        assert_eq!(saved["minimax"]["require_region_selection"], true);
        let source = profile.java_source.as_ref().unwrap();
        assert_eq!(source.id(), "java-mmx");
        assert_eq!(
            report.journal_profiles.recording.as_ref().unwrap().source(),
            Some(source)
        );
        assert_eq!(
            report
                .journal_profiles
                .automation
                .as_ref()
                .unwrap()
                .source(),
            Some(source)
        );
        assert!(catalog.validate_java_profiles().is_ok());
    }

    #[test]
    fn java_inline_prompts_remain_unavailable_for_other_providers_and_modified_minimax_commands() {
        let arguments = JAVA_TEXT_CHAT.join(" ");
        let (catalog, report) = java_profile("gemini-cli", &arguments);
        assert!(catalog.profiles.is_empty());
        assert_eq!(report.omitted, 1);
        let (catalog, report) = java_profile("minimax", &format!("{arguments} --config alternate"));
        assert!(catalog.profiles.is_empty());
        assert_eq!(report.omitted, 1);
    }
}
