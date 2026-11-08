use std::fmt;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::snippets::{Library, SnippetId, resolve};

pub(super) const MAX_COMMAND_BYTES: usize = 16 * 1024;
pub(super) const MAX_ARGUMENT_TEXT_BYTES: usize = 8 * 1024;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Bash,
    Python,
    Perl,
    Ruby,
}

impl Language {
    pub(super) const ALL: [Self; 4] = [Self::Bash, Self::Python, Self::Perl, Self::Ruby];

    pub(super) fn from_snippet(language: &str) -> Option<Self> {
        match language.to_ascii_lowercase().as_str() {
            "bash" | "shell" => Some(Self::Bash),
            "python" => Some(Self::Python),
            "perl" => Some(Self::Perl),
            "ruby" => Some(Self::Ruby),
            _ => None,
        }
    }

    const fn interpreter(self) -> &'static str {
        match self {
            Self::Bash => "bash -s --",
            Self::Python => "python3 -",
            Self::Perl => "perl -",
            Self::Ruby => "ruby -",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bash => "Bash / shell",
            Self::Python => "Python 3",
            Self::Perl => "Perl",
            Self::Ruby => "Ruby",
        })
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Script {
    pub(super) language: Language,
    pub(super) body: String,
    pub(super) arguments: Vec<String>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnippetLink {
    pub(crate) id: SnippetId,
    pub(crate) arguments: Vec<String>,
}

impl SnippetLink {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        validate_arguments(&self.arguments)
    }

    pub(super) fn command(&self, path: &Path) -> Result<String, &'static str> {
        let library =
            Library::open(path.to_path_buf()).map_err(|_| "Could not read the snippet library")?;
        let entry = library
            .get(self.id)
            .ok_or("The linked snippet is missing")?;
        let language = Language::from_snippet(entry.language())
            .ok_or("The linked snippet language cannot be run by the scheduler")?;
        let body = entry.content();
        if body.len() > MAX_COMMAND_BYTES {
            return Err("The linked script exceeds the 16 KiB SSH command limit");
        }
        Script {
            language,
            body: resolve::headless(body, library.variables(), MAX_COMMAND_BYTES)?,
            arguments: self.arguments.clone(),
        }
        .command()
    }
}

pub(super) fn arguments_from_lines(arguments: &str) -> Vec<String> {
    arguments
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn validate_arguments(arguments: &[String]) -> Result<(), &'static str> {
    if arguments.len() > MAX_ARGUMENTS
        || arguments.iter().any(|value| {
            value.trim().is_empty()
                || value.len() > MAX_ARGUMENT_BYTES
                || value.contains(['\0', '\r', '\n'])
        })
        || arguments
            .iter()
            .map(|value| value.len() + 1)
            .sum::<usize>()
            .saturating_sub(1)
            > MAX_ARGUMENT_TEXT_BYTES
    {
        Err("Enter at most 64 arguments, one per line, with 4 KiB per argument and 8 KiB total")
    } else {
        Ok(())
    }
}

impl Script {
    pub(super) fn from_lines(language: Language, body: String, arguments: &str) -> Self {
        Self {
            language,
            body,
            arguments: arguments_from_lines(arguments),
        }
    }

    pub(super) fn command(&self) -> Result<String, &'static str> {
        if self.body.trim().is_empty()
            || self.body.len() > MAX_COMMAND_BYTES
            || self.body.contains('\0')
        {
            return Err("Enter a nonempty script of at most 16 KiB");
        }
        validate_arguments(&self.arguments)?;

        let mut delimiter = String::from("KORTTY_SCRIPT");
        while self.body.lines().any(|line| line == delimiter) {
            delimiter.push('_');
        }
        let mut command = format!("exec {}", self.language.interpreter());
        for argument in &self.arguments {
            command.push_str(" '");
            command.push_str(&argument.replace('\'', "'\\''"));
            command.push('\'');
        }
        // A quoted heredoc feeds the interpreter verbatim. Only the fixed
        // interpreter selector and shell-quoted argv are parsed by the login shell.
        let _ = writeln!(command, " <<'{delimiter}'");
        command.push_str(&self.body);
        if !self.body.ends_with('\n') {
            command.push('\n');
        }
        command.push_str(&delimiter);
        command.push('\n');
        if command.len() > MAX_COMMAND_BYTES {
            return Err("The script and its quoted arguments exceed the 16 KiB SSH command limit");
        }
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_argv_and_picks_an_unused_literal_boundary() {
        let source = "cat <<'inner'\nKORTTY_SCRIPT\nKORTTY_SCRIPT_\n$HOME ' \\\ninner";
        let script = Script::from_lines(
            Language::Bash,
            source.into(),
            "  words  \n\n'$(exit 8)\n--flag",
        );
        assert_eq!(script.arguments, ["  words  ", "'$(exit 8)", "--flag"]);
        let command = script.command().unwrap();
        assert!(command.starts_with(
            "exec bash -s -- '  words  ' ''\\''$(exit 8)' '--flag' <<'KORTTY_SCRIPT__'\n"
        ));
        assert!(command.ends_with(&format!("{source}\nKORTTY_SCRIPT__\n")));
    }

    #[test]
    fn command_rejects_invalid_argv_and_checks_the_encoded_size() {
        let body = "true".to_owned();
        let mut script = Script {
            language: Language::Python,
            body,
            arguments: vec!["x".into(); MAX_ARGUMENTS + 1],
        };
        assert!(script.command().is_err());
        script.arguments = vec!["has\na newline".into()];
        assert!(script.command().is_err());
        script.arguments = vec!["'".repeat(MAX_ARGUMENT_BYTES)];
        assert_eq!(
            script.command().unwrap_err(),
            "The script and its quoted arguments exceed the 16 KiB SSH command limit"
        );
        script.arguments = Vec::new();
        script.body = "a".repeat(MAX_COMMAND_BYTES);
        assert!(script.command().is_err());
    }
}
