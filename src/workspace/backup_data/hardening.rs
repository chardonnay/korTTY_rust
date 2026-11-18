//! Additive migration of Java Snippet Analysis and Terminal Workflow defaults.

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::snippet_analysis::model::hardening::{HardeningOption, InputOption, MAX_FILE_SIZE_MIB};

use super::{java_flag, preference, preference_value};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    classic: Option<Vec<HardeningOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Vec<InputOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_file_size_mib: Option<u16>,
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        use HardeningOption as Classic;
        use InputOption as Input;

        let settings = Self {
            classic: selection(
                root,
                "snippetHardeningOptions",
                omitted,
                &[
                    ("STRICT_MODE", Classic::StrictMode),
                    ("ERROR_TRAP_CLEANUP", Classic::ErrorTrapCleanup),
                    ("MEANINGFUL_EXIT_CODES", Classic::MeaningfulExitCodes),
                    ("LOGGING_VERBOSE", Classic::LoggingVerbose),
                    ("CONFIG_BLOCK", Classic::ConfigBlock),
                    ("END_SUMMARY", Classic::EndSummary),
                    ("STYLE_GUIDE_CLEAN", Classic::StyleGuideClean),
                    ("PRECONDITION_CHECKS", Classic::PreconditionChecks),
                    ("IDEMPOTENCY", Classic::Idempotency),
                    ("SAFE_MODE", Classic::SafeMode),
                    ("HELP_USAGE", Classic::HelpUsage),
                ],
            ),
            input_enabled: preference(root, "snippetInputHardeningEnabled", omitted, java_flag),
            input: selection(
                root,
                "snippetInputHardeningOptions",
                omitted,
                &[
                    ("PARAM_VALIDATION", Input::ParameterValidation),
                    ("FILE_CHECKS", Input::FileChecks),
                    ("FILE_SIZE_LIMIT", Input::FileSizeLimit),
                    ("SECURITY_LOGGING", Input::SecurityLogging),
                    ("FORCE_OVERRIDE", Input::ForceOverride),
                ],
            ),
            max_file_size_mib: preference(
                root,
                "snippetInputHardeningMaxFileSizeMb",
                omitted,
                |text| {
                    let value = text.parse::<i32>().ok()?;
                    // The Java getter treats a negative persisted value as unset.
                    if value < 0 {
                        return None;
                    }
                    u16::try_from(value.min(i32::from(MAX_FILE_SIZE_MIB))).ok()
                },
            ),
        };
        (settings.count() != 0).then_some(settings)
    }

    pub(super) fn count(&self) -> usize {
        usize::from(self.classic.is_some())
            + usize::from(self.input_enabled.is_some())
            + usize::from(self.input.is_some())
            + usize::from(self.max_file_size_mib.is_some())
    }

    pub(super) fn valid(&self) -> bool {
        self.classic.as_deref().is_none_or(unique)
            && self.input.as_deref().is_none_or(unique)
            && self
                .max_file_size_mib
                .is_none_or(|size| size <= MAX_FILE_SIZE_MIB)
    }
}

fn unique<T: PartialEq>(options: &[T]) -> bool {
    options
        .iter()
        .enumerate()
        .all(|(index, option)| !options[..index].contains(option))
}

fn selection<T: Copy>(
    root: Node<'_, '_>,
    key: &str,
    omitted: &mut usize,
    options: &[(&str, T)],
) -> Option<Vec<T>> {
    let csv = match preference_value(root, key, true) {
        Ok(value) => value?,
        Err(()) => {
            *omitted += 1;
            return None;
        }
    };
    let mut selected = vec![false; options.len()];
    let mut unknown = false;
    for name in csv
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if let Some(index) = options.iter().position(|(candidate, _)| *candidate == name) {
            selected[index] = true;
        } else {
            unknown = true;
        }
    }
    *omitted += usize::from(unknown);
    Some(
        options
            .iter()
            .zip(selected)
            .filter_map(|((_, option), selected)| selected.then_some(*option))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use roxmltree::Document;
    use serde_json::json;

    use super::*;

    fn parse(xml: &str) -> (Option<Preferences>, usize) {
        let document = Document::parse(xml).unwrap();
        let mut omitted = 0;
        let settings = Preferences::read(document.root_element(), &mut omitted);
        (settings, omitted)
    }

    #[test]
    fn missing_and_cleared_selections_have_different_meanings() {
        let (missing, omitted) = parse("<globalSettings/>");
        assert!(missing.is_none());
        assert_eq!(omitted, 0);
        let (cleared, omitted) = parse(
            "<globalSettings>
                <snippetHardeningOptions/>
                <snippetInputHardeningOptions> </snippetInputHardeningOptions>
                <snippetInputHardeningMaxFileSizeMb>0</snippetInputHardeningMaxFileSizeMb>
            </globalSettings>",
        );
        assert_eq!(omitted, 0);
        assert_eq!(
            serde_json::to_value(cleared.unwrap()).unwrap(),
            json!({"classic": [], "input": [], "maxFileSizeMib": 0})
        );
    }

    #[test]
    fn lists_follow_java_order_and_size_uses_java_bounds() {
        let (settings, omitted) = parse(
            "<globalSettings>
                <snippetHardeningOptions>HELP_USAGE, STRICT_MODE, HELP_USAGE, RETIRED</snippetHardeningOptions>
                <snippetInputHardeningEnabled>1</snippetInputHardeningEnabled>
                <snippetInputHardeningOptions>FILE_SIZE_LIMIT,PARAM_VALIDATION</snippetInputHardeningOptions>
                <snippetInputHardeningMaxFileSizeMb>4096</snippetInputHardeningMaxFileSizeMb>
            </globalSettings>",
        );
        assert_eq!(omitted, 1);
        let settings = settings.unwrap();
        assert!(settings.valid());
        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({
                "classic": ["strictMode", "helpUsage"],
                "inputEnabled": true,
                "input": ["parameterValidation", "fileSizeLimit"],
                "maxFileSizeMib": 1024
            })
        );
        let (negative, omitted) = parse(
            "<globalSettings>
                <snippetInputHardeningMaxFileSizeMb>-1</snippetInputHardeningMaxFileSizeMb>
            </globalSettings>",
        );
        assert!(negative.is_none());
        assert_eq!(omitted, 1);
    }
}
