//! Resolves and freezes the exact script shown for each non-AI swarm approval.

use std::path::Path;

use std::fmt::Write as _;
use zeroize::Zeroizing;

use crate::scheduler_window::script::{Language, Script, arguments_from_lines};
use crate::snippet_analysis::model::content_hash;
use crate::snippets::{Library, SnippetId, resolve};

use super::super::decision::{MAX_COMMAND_BYTES, valid_command};
use super::super::{Shell, Target};

mod windows;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Family {
    Posix(Language),
    PowerShell,
    CommandPrompt,
}

impl Family {
    fn from_language(language: &str) -> Option<Self> {
        match language.to_ascii_lowercase().as_str() {
            "powershell" | "ps1" | "pwsh" => Some(Self::PowerShell),
            "cmd" | "batch" | "bat" => Some(Self::CommandPrompt),
            _ => Language::from_snippet(language).map(Self::Posix),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
struct PowerShellReview {
    source: String,
    arguments: Vec<String>,
    preview: String,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Prepared {
    pub(crate) id: SnippetId,
    pub(crate) name: String,
    pub(crate) command: String,
    revision: String,
    family: Family,
    powershell: Option<PowerShellReview>,
}

impl Prepared {
    pub(crate) fn supports_language(language: &str) -> bool {
        Family::from_language(language).is_some()
    }

    pub(crate) fn capture(
        library: &Library,
        id: SnippetId,
        arguments: &str,
    ) -> Result<Self, &'static str> {
        let snippet = library
            .get(id)
            .ok_or("The selected snippet no longer exists.")?;
        let family = Family::from_language(snippet.language())
            .ok_or("The selected snippet language has no supported script runner.")?;
        let source = resolve::headless(snippet.content(), library.variables(), MAX_COMMAND_BYTES)?
            .replace("\r\n", "\n");
        let revision = revision(library, id)?;
        let (command, powershell) = match family {
            Family::Posix(language) => (
                Script::from_lines(language, source, arguments).command()?,
                None,
            ),
            Family::PowerShell => {
                let arguments = arguments_from_lines(arguments);
                let command = windows::powershell(&source, &arguments)?;
                (
                    command.wire,
                    Some(PowerShellReview {
                        source,
                        arguments,
                        preview: command.preview,
                    }),
                )
            }
            Family::CommandPrompt => (
                windows::cmd(&source, &arguments_from_lines(arguments))?.wire,
                None,
            ),
        };
        if !valid_command(&command) {
            return Err(
                "The resolved script and its arguments exceed the reviewed command limits.",
            );
        }
        Ok(Self {
            id,
            name: snippet.name().into(),
            command,
            revision,
            family,
            powershell,
        })
    }

    pub(crate) fn supports_target(&self, target: &Target) -> bool {
        match target {
            Target::Ssh { .. } => self.supports_ssh(),
            Target::Local { shell, .. } => match shell {
                #[cfg(unix)]
                Shell::Posix(_) => matches!(self.family, Family::Posix(_)),
                #[cfg(windows)]
                Shell::PowerShell(_) => self.family == Family::PowerShell,
                #[cfg(windows)]
                Shell::CommandPrompt(_) => self.family == Family::CommandPrompt,
            },
        }
    }

    pub(crate) fn supports_ssh(&self) -> bool {
        matches!(self.family, Family::Posix(_))
    }

    pub(crate) fn preview(&self) -> &str {
        self.powershell
            .as_ref()
            .map_or(self.command.as_str(), |review| &review.preview)
    }

    pub(crate) fn raw_inputs(&self) -> impl Iterator<Item = &str> {
        self.powershell.iter().flat_map(|review| {
            std::iter::once(review.source.as_str())
                .chain(review.arguments.iter().map(String::as_str))
        })
    }

    pub(crate) fn verify(&self, path: &Path) -> Result<(), &'static str> {
        let library = Library::open(path.to_path_buf())
            .map_err(|_| "Could not recheck the reviewed snippet library.")?;
        if self.still_current(&library) {
            Ok(())
        } else {
            Err("The reviewed snippet or one of its variables changed. Start a new review.")
        }
    }

    fn still_current(&self, library: &Library) -> bool {
        revision(library, self.id).is_ok_and(|current| current == self.revision)
    }
}

fn revision(library: &Library, id: SnippetId) -> Result<String, &'static str> {
    let snippet = library
        .get(id)
        .ok_or("The selected snippet no longer exists.")?;
    let variables = library.variables();
    if snippet.content().len() > MAX_COMMAND_BYTES {
        return Err("The reviewed script exceeds the command limit.");
    }
    let declarations = resolve::placeholder_declarations(variables);
    let report = resolve::inspect_placeholders(snippet.content(), &declarations);
    if report.limited {
        return Err("The snippet exceeds the supported variable count.");
    }
    let mut canonical = Zeroizing::new(String::new());
    let mut add = |value: &str| {
        let _ = write!(*canonical, "{}:{value}", value.len());
    };
    add(snippet.name());
    add(snippet.language());
    add(snippet.content());
    for item in report.entries {
        if matches!(
            item.status,
            resolve::PlaceholderStatus::Stored | resolve::PlaceholderStatus::Prompt
        ) && let Some(variable) = variables
            .iter()
            .find(|variable| variable.matches(&item.name))
        {
            add(&variable.name);
            add(&variable.value);
        }
    }
    content_hash(&canonical).map_err(|_| "Could not identify the reviewed script revision.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::{Input, Variable, tests::Profile};

    #[test]
    fn swarm_script_review_preserves_literal_arguments_and_invalidates_changed_variables() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let save = |library: &mut Library, name: &str, content: &str| {
            library
                .save(
                    None,
                    Input {
                        name: name.into(),
                        language: "bash".into(),
                        category: String::new(),
                        content: content.into(),
                    },
                )
                .unwrap()
        };
        let id = save(
            &mut library,
            "Probe",
            "printf '%s\n' '${server}' \"$1\"\r\n",
        );
        assert!(Prepared::capture(&library, id, "--flag").is_err());
        library
            .save_variable(
                None,
                Variable {
                    name: "server".into(),
                    value: "lab".into(),
                },
            )
            .unwrap();
        let script = Prepared::capture(&library, id, "'$(exit 8)\n--flag").unwrap();
        assert!(script.command.contains("lab"));
        assert!(!script.command.contains('\r'));
        assert!(script.command.contains("''\\''$(exit 8)'"));
        assert!(script.still_current(&library));
        assert!(script.verify(&profile.path()).is_ok());
        library
            .save_variable(
                None,
                Variable {
                    name: "unrelated".into(),
                    value: "new".into(),
                },
            )
            .unwrap();
        assert!(script.still_current(&library));
        library
            .save_variable(
                Some("server"),
                Variable {
                    name: "server".into(),
                    value: "other".into(),
                },
            )
            .unwrap();
        assert!(!script.still_current(&library));
    }
}
