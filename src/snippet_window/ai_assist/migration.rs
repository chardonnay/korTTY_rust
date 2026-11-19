//! Migration is tied to the selected target and reviewed snippet.

use std::fmt;

use crate::snippet_window::languages::mix::{HostFormat, Inspection, scaffold};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Language {
    Bash,
    Python,
    Perl,
    Ruby,
    PowerShell,
    WindowsCmd,
    AppleScript,
    JavaScript,
    Groovy,
}

impl Language {
    pub(crate) const ALL: [Self; 9] = [
        Self::Bash,
        Self::Python,
        Self::Perl,
        Self::Ruby,
        Self::PowerShell,
        Self::WindowsCmd,
        Self::AppleScript,
        Self::JavaScript,
        Self::Groovy,
    ];

    fn properties(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::Bash => ("Bash", "bash", "sh", "#!/usr/bin/env bash"),
            Self::Python => ("Python", "python", "py", "#!/usr/bin/env python3"),
            Self::Perl => ("Perl", "perl", "pl", "#!/usr/bin/env perl"),
            Self::Ruby => ("Ruby", "ruby", "rb", "#!/usr/bin/env ruby"),
            Self::PowerShell => ("PowerShell", "powershell", "ps1", "#!/usr/bin/env pwsh"),
            Self::WindowsCmd => ("Windows CMD", "bat", "cmd", "@echo off"),
            Self::AppleScript => (
                "AppleScript",
                "applescript",
                "applescript",
                "#!/usr/bin/osascript",
            ),
            Self::JavaScript => ("JavaScript", "javascript", "js", "#!/usr/bin/env node"),
            Self::Groovy => ("Groovy", "groovy", "groovy", "#!/usr/bin/env groovy"),
        }
    }

    pub(crate) fn token(self) -> &'static str {
        self.properties().1
    }

    pub(crate) fn available(mix: &Inspection) -> Vec<Self> {
        if !mix.mixed() {
            return Vec::new();
        }
        Self::ALL
            .into_iter()
            .filter(|language| {
                mix.host
                    .is_none_or(|host| scaffold::targets(host).contains(&language.token()))
            })
            .collect()
    }

    pub(crate) fn platform_targets(host: HostFormat) -> Vec<Self> {
        let targets = scaffold::targets(host);
        if targets.is_empty() {
            return vec![Self::Bash];
        }
        Self::ALL
            .into_iter()
            .filter(|language| targets.contains(&language.token()))
            .collect()
    }

    pub(crate) fn file_name(self, original: &str) -> Result<String, String> {
        std::path::Path::new(original)
            .with_extension(self.properties().2)
            .into_os_string()
            .into_string()
            .map_err(|_| "The snippet filename cannot be migrated.".into())
    }

    pub(super) fn lead(self) -> &'static str {
        self.properties().3
    }

    pub(super) fn validate_result(self, original: &str, result: &str) -> Result<(), String> {
        let mut lines = result.lines();
        if lines.next() != Some(self.lead()) || !lines.any(|line| !line.trim().is_empty()) {
            return Err("The AI result did not contain a complete script with the requested interpreter header.".into());
        }
        let mix = crate::snippet_window::languages::mix::inspect(self.token(), result);
        if mix.host.is_some() || mix.mixed() {
            return Err(
                "The proposed script still contains a detected language mix or host document."
                    .into(),
            );
        }
        let incomplete = result.lines().any(|line| {
            let body = line
                .trim()
                .trim_start_matches(['#', '/', '-', ';', '*'])
                .trim();
            let lower = body.to_ascii_lowercase();
            matches!(
                lower.as_str(),
                "..."
                    | "…"
                    | "rest unchanged"
                    | "remaining code omitted"
                    | "implementation omitted"
            )
        });
        let code_lines = |source: &str| {
            source
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
        };
        if incomplete || (code_lines(original) >= 24 && code_lines(result) < 4) {
            return Err("The proposed migration appears to be incomplete.".into());
        }
        Ok(())
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.properties().0)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub(crate) text: String,
    pub(crate) notes: Vec<String>,
}

impl Response {
    pub(crate) fn notes_valid(notes: &[String]) -> bool {
        notes.len() <= 24
            && notes
                .iter()
                .all(|note| note.len() <= 1024 && !note.chars().any(char::is_control))
    }
}

#[derive(Clone)]
pub(crate) struct Proposal {
    pub(crate) notes: Vec<String>,
    pub(crate) plan: Plan,
    pub(crate) security_fixes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum Plan {
    WholeScript(Language),
    Steps {
        host: HostFormat,
        language: Language,
    },
    Platform {
        from: HostFormat,
        to: HostFormat,
        language: Option<Language>,
    },
}

impl Plan {
    pub(crate) fn new(
        mix: &Inspection,
        language: Option<Language>,
        platform: Option<HostFormat>,
    ) -> Result<Self, String> {
        if let Some(to) = platform {
            let from = mix.host.filter(|host| HostFormat::CONVERTIBLE.contains(host) && *host != to)
                .ok_or("Choose a different platform for a supported pipeline or configuration document.")?;
            if !HostFormat::CONVERTIBLE.contains(&to)
                || language
                    .is_some_and(|language| !Language::platform_targets(to).contains(&language))
            {
                return Err("The requested platform or script language is unavailable.".into());
            }
            return Ok(Self::Platform { from, to, language });
        }
        let language = language.ok_or("Choose a target programming language or platform.")?;
        if !Language::available(mix).contains(&language) {
            return Err("The requested language is unavailable for this document.".into());
        }
        Ok(match mix.host {
            Some(host) => Self::Steps { host, language },
            None => Self::WholeScript(language),
        })
    }

    pub(crate) fn language(self) -> Option<Language> {
        match self {
            Self::WholeScript(language) | Self::Steps { language, .. } => Some(language),
            Self::Platform { language, .. } => language,
        }
    }

    pub(crate) fn destination(self) -> Option<HostFormat> {
        match self {
            Self::Platform { to, .. } => Some(to),
            _ => None,
        }
    }

    pub(super) fn scope(self) -> &'static str {
        match self {
            Self::WholeScript(_) => "whole_script",
            Self::Steps { .. } => "script_steps",
            Self::Platform { .. } => "platform_conversion",
        }
    }

    pub(crate) fn instruction(self) -> String {
        match self {
            Self::Steps { host, language } => format!(
                "Rewrite the detected individual script steps of this {} document in {}. Preserve the document byte for byte outside the step bodies and interpreter selectors. Keep every step in its original order, keep its inline or block style, and preserve comments and whitespace outside the bodies. For GitHub Actions, make the shell explicit for every rewritten step. Add any missing shell key immediately after the corresponding run body. Return the complete document.",
                host.label(),
                language,
            ),
            Self::WholeScript(language) => format!(
                "Rewrite the complete snippet in {}. Preserve its observable behavior, inputs, outputs, error handling and placeholders. Remove embedded foreign-language programs. The first line must be exactly {}.",
                language,
                language.lead(),
            ),
            Self::Platform { from, to, language } => {
                let step_language = language.map_or_else(
                    || "Keep existing step languages where the target supports them. Describe necessary interpreter changes in notes.".to_owned(),
                    |language| format!("Write every converted script step in {language} and select its interpreter explicitly wherever the platform supports it."),
                );
                format!(
                    "Convert this complete {from} document to {to}. Preserve tasks, triggers, conditions, variables, secret references, dependencies and artifacts wherever the target has equivalent features. Put every missing or lossy mapping in the notes, including any manual setup required. Do not invent secret values. {step_language}"
                )
            }
        }
    }

    pub(crate) fn result(self, original: &str, text: String) -> Result<String, String> {
        match self {
            Self::Steps { host, language } => {
                let candidate = if original.contains("\r\n")
                    && !original.replace("\r\n", "").contains(['\r', '\n'])
                {
                    super::normalize(&text).replace('\n', "\r\n")
                } else {
                    text
                };
                if !scaffold::preserved(host, original, &candidate, language.token()) {
                    return Err("The proposal changes the pipeline outside its steps or leaves a different step language.".into());
                }
                Ok(candidate)
            }
            Self::WholeScript(language) => {
                let result = super::normalize(&text);
                language.validate_result(original, &result)?;
                Ok(result)
            }
            Self::Platform { to, language, .. } => {
                use crate::snippet_window::languages::mix::inspect;
                let result = super::normalize(&text);
                let detected = inspect(to.document_type().0, &result);
                if detected.host != Some(to) {
                    return Err("The response did not reach the requested platform format.".into());
                }
                if let Some(language) = language
                    && (detected.embedded.is_empty()
                        || !scaffold::explicit_selectors_match(to, &result, language.token())
                        || detected
                            .embedded
                            .iter()
                            .any(|step| step.language != language.token()))
                {
                    return Err(
                        "The converted script steps do not use the selected programming language."
                            .into(),
                    );
                }
                Ok(result)
            }
        }
    }

    pub(crate) fn result_language(self) -> Option<&'static str> {
        match self {
            Self::WholeScript(language) => Some(language.token()),
            Self::Platform { to, .. } => Some(to.document_type().0),
            Self::Steps { .. } => None,
        }
    }

    pub(crate) fn result_metadata(
        self,
        original: &crate::snippets::Metadata,
    ) -> Result<Option<(&'static str, crate::snippets::Metadata)>, String> {
        let Some(language) = self.result_language() else {
            return Ok(None);
        };
        let mut metadata = original.clone();
        metadata.file_name = self.result_file_name(metadata.file_name.as_deref())?;
        metadata.validate().map_err(|error| error.to_string())?;
        Ok(Some((language, metadata)))
    }

    pub(crate) fn result_file_name(self, original: Option<&str>) -> Result<Option<String>, String> {
        match self {
            Self::WholeScript(target) => original.map(|name| target.file_name(name)).transpose(),
            Self::Platform { to, .. } => Ok(Some(to.document_type().1.into())),
            Self::Steps { .. } => Ok(original.map(str::to_owned)),
        }
    }

    pub(crate) fn retains_destination(self, original: &str, result: &str) -> Result<(), String> {
        use crate::snippet_window::languages::mix::inspect;
        let (host, selected) = match self {
            Self::WholeScript(language) => return language.validate_result(original, result),
            Self::Steps { host, language } => (host, Some(language)),
            Self::Platform { to, language, .. } => (to, language),
        };
        let detected = inspect(host.document_type().0, result);
        if detected.host != Some(host)
            || selected.is_some_and(|language| {
                detected.embedded.is_empty()
                    || detected
                        .embedded
                        .iter()
                        .any(|step| step.language != language.token())
            })
        {
            return Err("The proposal changes the migrated platform or step language.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{HostFormat, Language, Plan};

    #[test]
    fn accepts_a_javascript_class_as_a_plain_target_script() {
        assert!(
            Language::JavaScript
                .validate_result("perl -e 'print 1'", "#!/usr/bin/env node\nclass Result {}")
                .is_ok()
        );
    }

    #[test]
    fn selected_github_step_language_requires_an_explicit_matching_shell() {
        let plan = Plan::Platform {
            from: HostFormat::AzurePipelines,
            to: HostFormat::GitHubActions,
            language: Some(Language::Bash),
        };
        let original = "steps:\n  - bash: echo hello";
        let converted =
            "jobs:\n  verify:\n    runs-on: windows-latest\n    steps:\n      - run: echo hello";
        assert!(plan.result(original, converted.into()).is_err());
        assert!(
            plan.result(original, format!("{converted}\n        shell: custom"))
                .is_err()
        );
        assert!(
            plan.result(original, format!("{converted}\n        shell: bash"))
                .is_ok()
        );
    }

    #[test]
    fn platform_only_conversion_can_replace_a_shell_command_with_a_native_task() {
        let plan = Plan::Platform {
            from: HostFormat::AzurePipelines,
            to: HostFormat::Ansible,
            language: None,
        };
        let result = "- hosts: build\n  tasks:\n    - ansible.builtin.file:\n        path: /tmp/build\n        state: directory";
        assert_eq!(
            plan.result("steps:\n  - bash: mkdir -p /tmp/build", result.into())
                .unwrap(),
            result,
        );
    }
}
