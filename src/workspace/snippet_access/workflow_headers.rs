//! Language-specific workflow headers from local snippets and managed policy.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::clipboard_policy::{ClipboardPolicy, ManagedScriptHeader, SnippetPolicy};
use crate::profile;
use crate::snippet_analysis::model::hardening::{HEADER_CATEGORY, HeaderRef, MAX_HEADER_BYTES};
use crate::snippets::{self, Library, Snippet, Variable, resolve};
use crate::terminal_agent::swarm::workflow::{Header, HeaderFacts, Language};
use crate::workspace::{Workspace, tab_titles::WindowSettings};

const LIBRARY_ERROR: &str = "Could not read the saved script headers.";
const SETTINGS_ERROR: &str = "Could not read the saved script header defaults.";
const STALE_DEFAULT: &str = "This language's script header default changed in another instance.";
const STALE_CHOICE: &str =
    "The selected script header changed or is no longer available. Choose it again.";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(in crate::workspace) struct Defaults(BTreeMap<Language, HeaderRef>);

impl Defaults {
    pub(in crate::workspace) fn is_default(&self) -> bool {
        self.0.is_empty()
    }

    fn get(&self, language: Language) -> Option<HeaderRef> {
        self.0.get(&language).copied()
    }

    fn set(&mut self, language: Language, id: Option<HeaderRef>) {
        if let Some(id) = id {
            self.0.insert(language, id);
        } else {
            self.0.remove(&language);
        }
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) enum Choice {
    #[default]
    Auto,
    None,
    Template(Entry),
}

impl Choice {
    pub(in crate::workspace) fn reference(&self) -> Option<HeaderRef> {
        match self {
            Self::Template(entry) => Some(entry.id),
            Self::Auto | Self::None => None,
        }
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Default (or automatic)",
            Self::None => "No header",
            Self::Template(entry) => return entry.display(formatter),
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Entry {
    id: HeaderRef,
    name: String,
    category: String,
    language: String,
    content: Arc<str>,
}

impl Entry {
    fn from_snippet(snippet: &Snippet, language: Language) -> Option<Self> {
        (snippet.category().eq_ignore_ascii_case(HEADER_CATEGORY)
            && applies_to(snippet.language(), language)
            && snippet.content().len() <= MAX_HEADER_BYTES
            && !snippet.content().trim().is_empty())
        .then(|| Self {
            id: HeaderRef::User(snippet.id()),
            name: snippet.name().into(),
            category: snippet.category().into(),
            language: snippet.language().into(),
            content: snippet.shared_content(),
        })
    }

    fn from_managed(header: &ManagedScriptHeader) -> Self {
        Self {
            id: HeaderRef::Managed { managed: header.id },
            name: header.name.clone(),
            category: HEADER_CATEGORY.into(),
            language: "bash".into(),
            content: Arc::from(header.content.as_str()),
        }
    }

    fn find(
        library: &Library,
        policy: &SnippetPolicy,
        language: Language,
        id: HeaderRef,
    ) -> Option<Self> {
        match id {
            HeaderRef::User(id) => Self::from_snippet(library.get(id)?, language),
            HeaderRef::Managed { managed } if language == Language::Bash => policy
                .script_headers
                .iter()
                .find(|header| header.id == managed)
                .map(Self::from_managed),
            HeaderRef::Managed { .. } => None,
        }
    }

    fn display(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.id {
            HeaderRef::User(_) => formatter.write_str(&self.name),
            HeaderRef::Managed { .. } => write!(formatter, "{} (managed)", self.name),
        }
    }
}

pub(super) fn applies_to(metadata: &str, language: Language) -> bool {
    if metadata.eq_ignore_ascii_case(language.snippet_language()) {
        return true;
    }
    let aliases: &[&str] = match language {
        Language::Bash => &["sh", "shell", "shellscript"],
        Language::Python => &["py", "python3"],
        Language::Perl => &["pl"],
        Language::Ruby => &["rb"],
        Language::PowerShell => &["ps", "ps1", "pwsh"],
        Language::WindowsCmd => &["cmd", "batch", "windows-cmd"],
        Language::AppleScript => &["scpt"],
        Language::Ansible => &["yaml", "yml", "ansible-playbook"],
    };
    aliases
        .iter()
        .any(|alias| metadata.eq_ignore_ascii_case(alias))
}

fn choices(library: &Library, policy: &SnippetPolicy, language: Language) -> Vec<Choice> {
    let mut entries: Vec<_> = library
        .entries()
        .iter()
        .filter_map(|snippet| Some((Entry::from_snippet(snippet, language)?, snippet.id())))
        .collect();
    entries.sort_by_cached_key(|(entry, id)| (entry.name.to_lowercase(), String::from(*id)));
    let mut choices = vec![Choice::Auto, Choice::None];
    if language == Language::Bash {
        choices.extend(
            policy
                .script_headers
                .iter()
                .map(Entry::from_managed)
                .map(Choice::Template),
        );
    }
    choices.extend(
        entries
            .into_iter()
            .map(|(entry, _)| Choice::Template(entry)),
    );
    choices
}

struct Template {
    entry: Entry,
    variables: Vec<Variable>,
}

impl Template {
    fn new(entry: Entry, library: &Library) -> Self {
        let variables = resolve::referenced_variables(&entry.content, library.variables())
            .into_iter()
            .cloned()
            .collect();
        Self { entry, variables }
    }

    fn header(&self, language: Language, facts: &HeaderFacts<'_>) -> Result<Header, String> {
        let text = resolve::workflow_header(
            &self.entry.content,
            &self.variables,
            facts.generated_at,
            facts.creator,
            MAX_HEADER_BYTES,
        )?;
        match self.entry.id {
            HeaderRef::User(_) => Header::custom(language, &text),
            HeaderRef::Managed { .. } => Header::managed_bash(&text),
        }
    }

    fn is_current(&self, entry: &Entry, library: &Library) -> bool {
        self.entry == *entry
            && self.variables.iter().eq(resolve::referenced_variables(
                &entry.content,
                library.variables(),
            ))
    }
}

enum Origin {
    None,
    Explicit(Template),
    Default {
        id: Option<HeaderRef>,
        template: Option<Template>,
    },
}

pub(in crate::workspace) struct Resolved {
    language: Language,
    header: Header,
    origin: Origin,
}

impl Resolved {
    pub(in crate::workspace) fn header(&self) -> &Header {
        &self.header
    }

    fn uses_defaults(&self) -> bool {
        matches!(self.origin, Origin::Default { .. })
    }

    fn reference(&self) -> Option<HeaderRef> {
        match &self.origin {
            Origin::None => None,
            Origin::Explicit(template) => Some(template.entry.id),
            Origin::Default { id, .. } => *id,
        }
    }

    fn is_current(
        &self,
        defaults: &Defaults,
        library: Option<&Library>,
        policy: &SnippetPolicy,
    ) -> bool {
        let template = match &self.origin {
            Origin::None => return true,
            Origin::Explicit(template) => Some(template),
            Origin::Default { id, template } if *id == defaults.get(self.language) => {
                template.as_ref()
            }
            Origin::Default { .. } => return false,
        };
        let Some(id) = self.reference() else {
            return true;
        };
        let Some(library) = library else {
            return false;
        };
        let current = Entry::find(library, policy, self.language, id);
        match (template, current) {
            (Some(template), Some(current)) => template.is_current(&current, library),
            (None, None) => true,
            _ => false,
        }
    }
}

fn resolve_choice(
    language: Language,
    choice: &Choice,
    facts: &HeaderFacts<'_>,
    defaults: &Defaults,
    library: Option<&Library>,
    policy: &SnippetPolicy,
) -> Result<Resolved, String> {
    let (header, origin) = match choice {
        Choice::None => (Header::NONE, Origin::None),
        Choice::Template(selected) => {
            let library = library.ok_or(LIBRARY_ERROR)?;
            let current = Entry::find(library, policy, language, selected.id)
                .filter(|current| current == selected)
                .ok_or(STALE_CHOICE)?;
            let template = Template::new(current, library);
            (
                template.header(language, facts)?,
                Origin::Explicit(template),
            )
        }
        Choice::Auto => {
            let id = defaults.get(language);
            let template = if let Some(id) = id {
                let library = library.ok_or(LIBRARY_ERROR)?;
                Entry::find(library, policy, language, id)
                    .map(|entry| Template::new(entry, library))
            } else {
                None
            };
            let header = match &template {
                Some(template) => template.header(language, facts)?,
                None => Header::automatic(language, HeaderFacts { ..*facts }),
            };
            (header, Origin::Default { id, template })
        }
    };
    Ok(Resolved {
        language,
        header,
        origin,
    })
}

fn open_library() -> io::Result<Library> {
    snippets::profile_path().and_then(Library::open)
}

fn read_defaults(path: &Path) -> io::Result<Defaults> {
    match WindowSettings::read(path) {
        Ok(settings) => Ok(settings.workflow_header_defaults),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Defaults::default()),
        Err(error) => Err(error),
    }
}

fn write_default(
    path: &Path,
    baseline: &WindowSettings,
    language: Language,
    id: Option<HeaderRef>,
) -> Result<Defaults, String> {
    let mut persisted = match WindowSettings::read(path) {
        Ok(settings) => settings,
        Err(error) if error.kind() == io::ErrorKind::NotFound => WindowSettings {
            workflow_header_defaults: Defaults::default(),
            ..baseline.clone()
        },
        Err(_) => return Err(SETTINGS_ERROR.into()),
    };
    let saved_id = persisted.workflow_header_defaults.get(language);
    if saved_id != id {
        if saved_id != baseline.workflow_header_defaults.get(language) {
            return Err(STALE_DEFAULT.into());
        }
        persisted.workflow_header_defaults.set(language, id);
        persisted
            .save(path)
            .map_err(|_| "Could not save the script header default.")?;
    }
    Ok(persisted.workflow_header_defaults)
}

impl Workspace {
    pub(in crate::workspace) fn workflow_header_choices(
        &self,
        language: Language,
    ) -> io::Result<Vec<Choice>> {
        open_library().map(|library| choices(&library, &ClipboardPolicy::load().snippets, language))
    }

    pub(in crate::workspace) fn resolve_workflow_headers(
        &self,
        selections: &[(Language, &Choice)],
        facts: &HeaderFacts<'_>,
        policy: &SnippetPolicy,
    ) -> Result<Vec<Resolved>, String> {
        let defaults = if selections
            .iter()
            .any(|(_, choice)| matches!(choice, Choice::Auto))
        {
            profile::window_settings_path()
                .and_then(|path| read_defaults(&path))
                .map_err(|_| SETTINGS_ERROR)?
        } else {
            Defaults::default()
        };
        let library = if selections.iter().any(|&(language, choice)| match choice {
            Choice::Auto => defaults.get(language).is_some(),
            Choice::None => false,
            Choice::Template(_) => true,
        }) {
            Some(open_library().map_err(|_| LIBRARY_ERROR)?)
        } else {
            None
        };
        selections
            .iter()
            .map(|&(language, choice)| {
                resolve_choice(language, choice, facts, &defaults, library.as_ref(), policy)
            })
            .collect()
    }

    pub(in crate::workspace) fn save_workflow_header_default(
        &mut self,
        language: Language,
        choice: &Choice,
    ) -> Result<(), String> {
        let id = match choice {
            Choice::Auto => None,
            Choice::None => {
                return Err("Choose a script header to set the language default.".into());
            }
            Choice::Template(selected) => {
                let library = open_library().map_err(|_| LIBRARY_ERROR)?;
                let current = Entry::find(
                    &library,
                    &ClipboardPolicy::load().snippets,
                    language,
                    selected.id,
                )
                .ok_or(STALE_CHOICE)?;
                if current != *selected {
                    return Err(STALE_CHOICE.into());
                }
                Some(current.id)
            }
        };
        let path = profile::window_settings_path().map_err(|_| SETTINGS_ERROR)?;
        let defaults = write_default(&path, &self.window_settings, language, id)?;
        self.window_settings.workflow_header_defaults = defaults;
        Ok(())
    }
}

pub(in crate::workspace) fn workflow_headers_current(
    headers: &[Resolved],
    policy: &SnippetPolicy,
) -> bool {
    let defaults = if headers.iter().any(Resolved::uses_defaults) {
        let Ok(defaults) = profile::window_settings_path().and_then(|path| read_defaults(&path))
        else {
            return false;
        };
        defaults
    } else {
        Defaults::default()
    };
    let library = if headers.iter().any(|header| header.reference().is_some()) {
        let Ok(library) = open_library() else {
            return false;
        };
        Some(library)
    } else {
        None
    };
    headers
        .iter()
        .all(|header| header.is_current(&defaults, library.as_ref(), policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard_policy::ManagedHeaderId;
    use crate::snippets::{Input, SnippetId, tests::Profile};

    fn input(name: &str, language: &str, category: &str, content: impl Into<Arc<str>>) -> Input {
        Input {
            name: name.into(),
            language: language.into(),
            category: category.into(),
            content: content.into(),
        }
    }

    fn facts() -> HeaderFacts<'static> {
        HeaderFacts {
            goal: "Summarize test runs",
            creator: "review-user",
            ssh_target: Some(("remote-user", "review-host")),
            generated_at: "2026-11-10T12:34:56".parse().unwrap(),
            profile_name: "Review profile",
        }
    }

    #[test]
    fn legacy_window_preferences_have_automatic_headers() {
        let profile = Profile::new();
        let path = profile.0.join("window-settings.json");
        assert!(read_defaults(&path).unwrap().is_default());
        profile::save_atomic(&path, br#"{"manual_scale":135}"#).unwrap();
        let mut settings = WindowSettings::read(&path).unwrap();
        assert!(settings.workflow_header_defaults.is_default());
        assert_eq!(settings.manual_scale, 135);
        assert!(serde_json::to_value(&settings).unwrap()["workflow_header_defaults"].is_null());

        let bash = HeaderRef::User(SnippetId::new().unwrap());
        let python = HeaderRef::User(SnippetId::new().unwrap());
        settings
            .workflow_header_defaults
            .set(Language::Bash, Some(bash));
        settings
            .workflow_header_defaults
            .set(Language::Python, Some(python));
        settings.save(&path).unwrap();
        assert!(
            serde_json::to_value(&settings).unwrap()["workflow_header_defaults"]["bash"]
                .is_string()
        );
        let mut reopened = WindowSettings::read(&path).unwrap();
        assert_eq!(
            reopened.workflow_header_defaults.get(Language::Bash),
            Some(bash)
        );
        assert_eq!(
            reopened.workflow_header_defaults.get(Language::Python),
            Some(python)
        );
        assert_eq!(reopened.manual_scale, 135);
        reopened.workflow_header_defaults.set(Language::Bash, None);
        reopened.save(&path).unwrap();
        let defaults = read_defaults(&path).unwrap();
        assert_eq!(defaults.get(Language::Bash), None);
        assert_eq!(defaults.get(Language::Python), Some(python));
    }

    #[test]
    fn saving_default_preserves_external_preferences_and_detects_language_conflicts() {
        let profile = Profile::new();
        let path = profile.0.join("window-settings.json");
        let mut baseline = WindowSettings {
            manual_scale: 135,
            ..WindowSettings::default()
        };
        let original = HeaderRef::User(SnippetId::new().unwrap());
        let replacement = HeaderRef::User(SnippetId::new().unwrap());
        let other = HeaderRef::User(SnippetId::new().unwrap());
        baseline.workflow_header_defaults =
            write_default(&path, &baseline, Language::Bash, Some(original)).unwrap();
        let mut external = WindowSettings::read(&path).unwrap();
        assert_eq!(external.manual_scale, 135);
        external.manual_scale = 146;
        external
            .workflow_header_defaults
            .set(Language::Python, Some(other));
        external.save(&path).unwrap();

        let defaults = write_default(&path, &baseline, Language::Bash, Some(replacement)).unwrap();
        assert_eq!(defaults.get(Language::Bash), Some(replacement));
        assert_eq!(defaults.get(Language::Python), Some(other));
        let current = WindowSettings::read(&path).unwrap();
        assert_eq!(current.manual_scale, 146);
        assert_eq!(current.workflow_header_defaults, defaults);

        let error = write_default(&path, &baseline, Language::Bash, None).unwrap_err();
        assert_eq!(error, STALE_DEFAULT);
        assert_eq!(WindowSettings::read(&path).unwrap(), current);
    }

    #[test]
    fn chooser_filters_by_category_language_and_size() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let cmd = library
            .save(
                None,
                input("Batch", "Windows-CMD", "script-header", "REM Batch"),
            )
            .unwrap();
        let yaml = library
            .save(None, input("Playbook", "yaml", HEADER_CATEGORY, "# YAML"))
            .unwrap();
        library
            .save(None, input("Other", "yaml", "Ops", "# Other"))
            .unwrap();
        library
            .save(
                None,
                input(
                    "Large",
                    "bash",
                    HEADER_CATEGORY,
                    "#".repeat(MAX_HEADER_BYTES + 1),
                ),
            )
            .unwrap();
        let ids = |language| {
            choices(&library, &SnippetPolicy::default(), language)
                .iter()
                .map(Choice::reference)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(Language::WindowsCmd),
            [None, None, Some(HeaderRef::User(cmd))]
        );
        assert_eq!(
            ids(Language::Ansible),
            [None, None, Some(HeaderRef::User(yaml))]
        );
        assert_eq!(ids(Language::Bash), [None, None]);
    }

    #[test]
    fn defaults_only_use_current_applicable_templates() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let id = library
            .save(
                None,
                input("Original", "bash", HEADER_CATEGORY, "# Original"),
            )
            .unwrap();
        let mut defaults = Defaults::default();
        let policy = SnippetPolicy::default();
        defaults.set(Language::Bash, Some(HeaderRef::User(id)));
        defaults.set(Language::Python, Some(HeaderRef::User(id)));
        let review = |language, library: &Library| {
            resolve_choice(
                language,
                &Choice::Auto,
                &facts(),
                &defaults,
                Some(library),
                &policy,
            )
            .unwrap()
        };
        let custom = review(Language::Bash, &library);
        assert_eq!(custom.header().text(), Some("# Original"));
        let automatic = review(Language::Python, &library);
        assert!(automatic.header().text().unwrap().contains("review-user"));
        assert!(
            !automatic
                .header()
                .instruction(Language::Python)
                .contains("review-user")
        );
        assert!(custom.is_current(&defaults, Some(&library), &policy));

        library.remove(id).unwrap();
        assert!(!custom.is_current(&defaults, Some(&library), &policy));
        let missing = review(Language::Bash, &library);
        assert!(missing.header().text().unwrap().contains("review-user"));
        assert!(missing.is_current(&defaults, Some(&library), &policy));
        defaults.set(Language::Bash, None);
        assert!(!missing.is_current(&defaults, Some(&library), &policy));
    }

    #[test]
    fn managed_workflow_header_is_distinct_and_bound_to_the_current_policy() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let user = library
            .save(
                None,
                input("Operations", "bash", HEADER_CATEGORY, "# Local"),
            )
            .unwrap();
        let managed = ManagedHeaderId::try_from("01".repeat(32)).unwrap();
        let id = HeaderRef::Managed { managed };
        let source = "#!/usr/bin/env bash\nset -euo pipefail\n# Managed by ${team}";
        let mut policy = SnippetPolicy {
            script_headers: vec![ManagedScriptHeader {
                id: managed,
                name: "Operations".into(),
                content: source.into(),
            }],
            allow_custom_script_headers: false,
            ..SnippetPolicy::default()
        };
        library
            .save_variable(
                None,
                Variable {
                    name: "team".into(),
                    value: "Ops".into(),
                },
            )
            .unwrap();
        let choices = choices(&library, &policy, Language::Bash);
        assert_eq!(choices[2].reference(), Some(id));
        assert_eq!(choices[2].to_string(), "Operations (managed)");
        assert_eq!(choices[3].reference(), Some(HeaderRef::User(user)));
        assert_eq!(library.entries().len(), 1);

        let mut defaults = Defaults::default();
        defaults.set(Language::Bash, Some(id));
        let encoded = serde_json::to_value(&defaults).unwrap();
        assert_eq!(encoded["bash"]["managed"], "01".repeat(32));
        assert_eq!(
            serde_json::from_value::<Defaults>(encoded).unwrap(),
            defaults
        );
        let review = |policy: &SnippetPolicy, choice: &Choice| {
            resolve_choice(
                Language::Bash,
                choice,
                &facts(),
                &defaults,
                Some(&library),
                policy,
            )
        };
        let approved = review(&policy, &Choice::Auto).unwrap();
        let template = "#!/usr/bin/env bash\nset -euo pipefail\n# Managed by Ops";
        assert_eq!(approved.header().text(), Some(template));
        assert!(
            !approved
                .header()
                .instruction(Language::Bash)
                .contains("pipefail")
        );
        let draft = crate::terminal_agent::swarm::workflow::finish_draft(
            "#!/bin/bash\nprintf 'ok\\n'",
            Language::Bash,
            facts().goal,
            approved.header(),
        )
        .unwrap();
        assert_eq!(draft.code(), format!("{template}\nprintf 'ok\\n'"));
        assert!(approved.is_current(&defaults, Some(&library), &policy));

        policy.script_headers[0].content.push_str("\n# Changed");
        assert!(!approved.is_current(&defaults, Some(&library), &policy));
        assert_eq!(
            review(&policy, &choices[2]).err().as_deref(),
            Some(STALE_CHOICE)
        );
        policy.script_headers.clear();
        let fallback = review(&policy, &Choice::Auto).unwrap();
        assert!(fallback.header().text().unwrap().contains("review-user"));
        assert!(!approved.is_current(&defaults, Some(&library), &policy));
    }

    #[test]
    fn reviewed_snippet_retains_exact_local_values_and_detects_stale_sources() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let source = "# ${username} ${datetime} ${Team} ${empty} $${clipboard} ${undeclared}";
        let id = library
            .save(None, input("Template", "bash", HEADER_CATEGORY, source))
            .unwrap();
        let variable = |name: &str, value: &str| Variable {
            name: name.into(),
            value: value.into(),
        };
        library
            .save_variable(None, variable("team", "local-only-value"))
            .unwrap();
        library.save_variable(None, variable("empty", "")).unwrap();
        let policy = SnippetPolicy::default();
        let choice = choices(&library, &policy, Language::Bash).pop().unwrap();
        let defaults = Defaults::default();
        let resolve = |library: &Library| {
            resolve_choice(
                Language::Bash,
                &choice,
                &facts(),
                &defaults,
                Some(library),
                &policy,
            )
        };
        let reviewed = resolve(&library).unwrap();
        assert_eq!(
            reviewed.header().text(),
            Some("# review-user 2026-11-10 12:34:56 local-only-value  ${clipboard} ${undeclared}")
        );
        assert!(
            !reviewed
                .header()
                .instruction(Language::Bash)
                .contains("local-only-value")
        );
        assert!(reviewed.is_current(&defaults, Some(&library), &policy));

        library
            .save_variable(None, variable("unrelated", "other"))
            .unwrap();
        assert!(reviewed.is_current(&defaults, Some(&library), &policy));
        library
            .save_variable(Some("team"), variable("team", "changed"))
            .unwrap();
        assert!(!reviewed.is_current(&defaults, Some(&library), &policy));
        library
            .save(
                Some(id),
                input("Template", "bash", HEADER_CATEGORY, "# ${clipboard}"),
            )
            .unwrap();
        assert_eq!(resolve(&library).err().as_deref(), Some(STALE_CHOICE));
        let changed = choices(&library, &policy, Language::Bash).pop().unwrap();
        assert!(
            resolve_choice(
                Language::Bash,
                &changed,
                &facts(),
                &defaults,
                Some(&library),
                &policy
            )
            .is_err()
        );
    }
}
