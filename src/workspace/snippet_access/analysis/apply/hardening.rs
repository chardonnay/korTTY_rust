//! Bind hardening choices to the snippet snapshot before showing the AI preview.

use crate::clipboard_policy::{ClipboardPolicy, ManagedHeaderId, ManagedScriptHeader};
use crate::snippet_analysis::model::hardening::{
    HEADER_CATEGORY, HeaderRef, MAX_HEADER_BYTES, Requirement, is_declarative,
};
use crate::snippet_window::analysis::hardening::{Controls, Edit, HeaderChoice};
use crate::snippets::{Library, Snippet, Variable, resolve};
use crate::terminal_agent::swarm::workflow::Language;
use crate::workspace::snippet_access::workflow_headers::applies_to;

use super::*;

pub(super) const MANAGED_HEADER_CHANGED: &str =
    "The managed script header changed. Choose the header and review a new apply request.";

pub(super) fn controls<'a>(page: &'a Page, window: &SnippetWindow) -> Option<Controls<'a>> {
    let Subject::Snippet(subject) = page.subject else {
        return None;
    };
    let record = page.record()?;
    if !matches!(page.state, State::Ready) || record.apply_recovery.is_some() {
        return None;
    }
    let mut headers = vec![HeaderChoice {
        id: None,
        name: "No header".into(),
    }];
    let language = window
        .analysis_language(subject)
        .unwrap_or(&record.source.language);
    if applies_to(language, Language::Bash) {
        headers.extend(
            window
                .managed_script_headers()
                .iter()
                .map(|entry| HeaderChoice {
                    id: Some(HeaderRef::Managed { managed: entry.id }),
                    name: format!("{} (managed)", entry.name),
                }),
        );
    }
    let managed_end = headers.len();
    headers.extend(
        window
            .latest_library()
            .entries()
            .iter()
            .filter(|entry| {
                entry.id() != subject && entry.category().eq_ignore_ascii_case(HEADER_CATEGORY)
            })
            .map(|entry| HeaderChoice {
                id: Some(HeaderRef::User(entry.id())),
                name: entry.name().into(),
            }),
    );
    headers[managed_end..].sort_by(|left, right| left.name.cmp(&right.name));
    Some(Controls {
        draft: &page.hardening,
        input_supported: !is_declarative(language),
        headers,
    })
}

pub(super) fn requirements(
    window: &SnippetWindow,
    page: &Page,
    language: &str,
) -> Result<Vec<Requirement>, &'static str> {
    let mut requirements = page
        .hardening
        .preferences(!is_declarative(language))?
        .requirements(language);
    if let Some(id) = page.hardening.header {
        let library = window.latest_library();
        let variables = library.variables();
        let policy;
        let (source, managed_sha256) = match id {
            HeaderRef::User(id) => (
                header(library, page.subject, id)
                    .ok_or("Choose an available script header from the library.")?
                    .content(),
                None,
            ),
            HeaderRef::Managed { managed } => {
                policy = ClipboardPolicy::load();
                let header = managed_header(&policy.snippets.script_headers, managed)
                    .filter(|_| applies_to(language, Language::Bash))
                    .ok_or("Choose an available managed Bash header.")?;
                (
                    header.content.as_str(),
                    Some(managed_hash(header, variables)?),
                )
            }
        };
        let values = resolve::copy_requirements(source, variables);
        if values.clipboard {
            return Err(
                "Remove the clipboard placeholder from the saved script header before using it with AI.",
            );
        }
        let resolved = resolve::interactive_limited(
            source,
            variables,
            &values.missing,
            None,
            MAX_HEADER_BYTES,
        )?;
        requirements.push(Requirement::resolved_header(id, &resolved, managed_sha256)?);
    }
    Ok(requirements)
}

fn managed_header(
    headers: &[ManagedScriptHeader],
    id: ManagedHeaderId,
) -> Option<&ManagedScriptHeader> {
    headers.iter().find(|header| header.id == id)
}

fn managed_hash(
    header: &ManagedScriptHeader,
    variables: &[Variable],
) -> Result<String, &'static str> {
    const ERROR: &str = "Could not bind the managed script header to its current source.";
    let referenced = resolve::referenced_variables(&header.content, variables);
    let snapshot =
        serde_json::to_string(&(&header.name, &header.content, referenced)).map_err(|_| ERROR)?;
    content_hash(&snapshot).map_err(|_| ERROR)
}

pub(super) fn managed_current(selection: &Selection, library_path: &std::path::Path) -> bool {
    let Some((managed, expected)) =
        selection
            .requirements
            .iter()
            .find_map(|requirement| match requirement {
                Requirement::Header {
                    snippet: HeaderRef::Managed { managed },
                    managed_sha256,
                    ..
                } => Some((*managed, managed_sha256.as_deref())),
                _ => None,
            })
    else {
        return true;
    };
    let policy = ClipboardPolicy::load();
    if !policy.ai_allowed {
        return false;
    }
    let Some(header) = managed_header(&policy.snippets.script_headers, managed) else {
        return false;
    };
    Library::open(library_path.to_owned())
        .ok()
        .and_then(|library| managed_hash(header, library.variables()).ok())
        .as_deref()
        .zip(expected)
        .is_some_and(|(actual, expected)| actual == expected)
}

fn header(library: &Library, subject: Subject, id: SnippetId) -> Option<&Snippet> {
    if subject == Subject::Snippet(id) {
        return None;
    }
    library
        .get(id)
        .filter(|snippet| snippet.category().eq_ignore_ascii_case(HEADER_CATEGORY))
}

impl Workspace {
    pub(super) fn update_snippet_hardening(
        &mut self,
        id: window::Id,
        pre: SnippetId,
        edit: Edit,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let Subject::Snippet(subject) = page.subject else {
            return Task::none();
        };
        if !matches!(page.state, State::Ready) {
            return Task::none();
        }
        let Some(record) = page
            .record()
            .filter(|record| record.id == pre && record.apply_recovery.is_none())
        else {
            return Task::none();
        };
        let language = surface
            .window
            .analysis_language(subject)
            .unwrap_or(&record.source.language);
        let input_supported = !is_declarative(language);
        match edit {
            Edit::SaveClassic | Edit::SaveInput => {
                let mut next = self.window_settings.clone();
                if matches!(edit, Edit::SaveClassic) {
                    next.snippet_hardening.classic = page.hardening.classic().to_vec();
                } else if input_supported {
                    let preferences = match page.hardening.preferences(true) {
                        Ok(preferences) => preferences,
                        Err(error) => {
                            page.status = Some(error.into());
                            return Task::none();
                        }
                    };
                    next.snippet_hardening.input_enabled = preferences.input_enabled;
                    next.snippet_hardening.input = preferences.input;
                    next.snippet_hardening.max_file_size_mib = preferences.max_file_size_mib;
                } else {
                    return Task::none();
                }
                page.status = Some(
                    match crate::profile::window_settings_path().and_then(|path| next.save(&path)) {
                        Ok(()) => {
                            self.window_settings = next;
                            "Saved hardening defaults for new analysis tabs.".into()
                        }
                        Err(error) => format!("Could not save hardening defaults ({error})."),
                    },
                );
            }
            Edit::Header(Some(HeaderRef::User(header_id)))
                if header(surface.window.latest_library(), page.subject, header_id).is_none() =>
            {
                page.status = Some("That script header is no longer available.".into());
            }
            Edit::Header(Some(HeaderRef::Managed { managed }))
                if !applies_to(language, Language::Bash)
                    || managed_header(
                        &ClipboardPolicy::load().snippets.script_headers,
                        managed,
                    )
                    .is_none() =>
            {
                page.status = Some("That managed Bash header is no longer available.".into());
            }
            edit => {
                page.hardening.update(edit, input_supported);
                page.status = None;
            }
        }
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_apply_header_keeps_the_script_lead_and_binds_only_used_values() {
        let id = ManagedHeaderId::try_from("02".repeat(32)).unwrap();
        let mut header = ManagedScriptHeader {
            id,
            name: "Operations".into(),
            content: "#!/usr/bin/env bash\nset -euo pipefail\n# ${team}".into(),
        };
        let mut variables = vec![
            Variable {
                name: "team".into(),
                value: "Operations".into(),
            },
            Variable {
                name: "unrelated".into(),
                value: "Original".into(),
            },
        ];
        let reviewed = managed_hash(&header, &variables).unwrap();
        variables[1].value = "Changed".into();
        assert_eq!(managed_hash(&header, &variables).unwrap(), reviewed);
        variables[0].value = "Changed".into();
        assert_ne!(managed_hash(&header, &variables).unwrap(), reviewed);
        variables[0].value = "Operations".into();
        header.content.push_str("\n# More");
        assert_ne!(managed_hash(&header, &variables).unwrap(), reviewed);

        let requirement = Requirement::resolved_header(
            HeaderRef::Managed { managed: id },
            "#!/usr/bin/env bash\nset -euo pipefail\n# Operations",
            Some(reviewed),
        )
        .unwrap();
        assert!(requirement.present("#!/bin/bash\nset -euo pipefail\n# Operations\nprintf ok"));
        assert!(!requirement.present("#!/bin/bash\n# Operations\nprintf ok"));
        let encoded = serde_json::to_value(&requirement).unwrap();
        assert_eq!(encoded["snippet"]["managed"], "02".repeat(32));
        assert_eq!(
            serde_json::from_value::<Requirement>(encoded).unwrap(),
            requirement
        );
    }
}
