//! Structured apply requirements, independent of model-provided analysis identifiers.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::clipboard_policy::ManagedHeaderId;
use crate::snippets::SnippetId;

pub(crate) const HEADER_CATEGORY: &str = "Script-Header";
pub(crate) const MAX_HEADER_BYTES: usize = 8 * 1024;
pub(crate) const MAX_FILE_SIZE_MIB: u16 = 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum HeaderRef {
    User(SnippetId),
    Managed { managed: ManagedHeaderId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Preferences {
    pub(crate) classic: Vec<HardeningOption>,
    pub(crate) input_enabled: bool,
    pub(crate) input: Vec<InputOption>,
    pub(crate) max_file_size_mib: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            classic: HardeningOption::ALL.to_vec(),
            input_enabled: false,
            input: InputOption::ALL.to_vec(),
            max_file_size_mib: 10,
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn requirements(&self, language: &str) -> Vec<Requirement> {
        let declarative = is_declarative(language);
        let mut requirements: Vec<_> = HardeningOption::ALL
            .into_iter()
            .filter(|option| self.classic.contains(option))
            .map(|option| Requirement::Hardening {
                option,
                declarative,
            })
            .collect();
        if self.input_enabled && !declarative {
            requirements.extend(
                InputOption::ALL
                    .into_iter()
                    .filter(|option| self.input.contains(option))
                    .map(|option| Requirement::Input {
                        option,
                        max_file_size_mib: if option == InputOption::FileSizeLimit {
                            self.max_file_size_mib.min(MAX_FILE_SIZE_MIB)
                        } else {
                            0
                        },
                    }),
            );
        }
        requirements
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum HardeningOption {
    StrictMode,
    ErrorTrapCleanup,
    MeaningfulExitCodes,
    LoggingVerbose,
    ConfigBlock,
    EndSummary,
    StyleGuideClean,
    PreconditionChecks,
    Idempotency,
    SafeMode,
    HelpUsage,
}

impl HardeningOption {
    pub(crate) const ALL: [Self; 11] = [
        Self::StrictMode,
        Self::ErrorTrapCleanup,
        Self::MeaningfulExitCodes,
        Self::LoggingVerbose,
        Self::ConfigBlock,
        Self::EndSummary,
        Self::StyleGuideClean,
        Self::PreconditionChecks,
        Self::Idempotency,
        Self::SafeMode,
        Self::HelpUsage,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::StrictMode => "Strict mode",
            Self::ErrorTrapCleanup => "Error handling and cleanup",
            Self::MeaningfulExitCodes => "Meaningful exit codes",
            Self::LoggingVerbose => "Logging and verbose mode",
            Self::ConfigBlock => "Configuration block",
            Self::EndSummary => "Final summary",
            Self::StyleGuideClean => "Style guide and lint cleanup",
            Self::PreconditionChecks => "Precondition checks",
            Self::Idempotency => "Safe to run again",
            Self::SafeMode => "Dry run and confirmation",
            Self::HelpUsage => "Help and usage",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::StrictMode => "HARDENING-STRICT-MODE",
            Self::ErrorTrapCleanup => "HARDENING-ERROR-CLEANUP",
            Self::MeaningfulExitCodes => "HARDENING-EXIT-CODES",
            Self::LoggingVerbose => "HARDENING-VERBOSE",
            Self::ConfigBlock => "HARDENING-CONFIGURATION",
            Self::EndSummary => "HARDENING-SUMMARY",
            Self::StyleGuideClean => "HARDENING-STYLE",
            Self::PreconditionChecks => "HARDENING-PRECONDITIONS",
            Self::Idempotency => "HARDENING-IDEMPOTENCY",
            Self::SafeMode => "HARDENING-SAFE-MODE",
            Self::HelpUsage => "HARDENING-HELP",
        }
    }

    pub(crate) fn instruction(self, declarative: bool) -> &'static str {
        match (self, declarative) {
            (Self::StrictMode, false) => "Enable the language's strict or abort-on-error mode.",
            (Self::StrictMode, true) => "Use assert and failed_when to stop on invalid state.",
            (Self::ErrorTrapCleanup, false) => {
                "Report failures and clean up temporary state with the language's error trap, finally or ensure facilities."
            }
            (Self::ErrorTrapCleanup, true) => {
                "Handle failures and run cleanup using block, rescue and always."
            }
            (Self::MeaningfulExitCodes, false) => {
                "Use distinct, documented nonzero exit codes for different kinds of failures."
            }
            (Self::MeaningfulExitCodes, true) => {
                "Stop the play with a clear diagnostic for failing tasks. Use any_errors_fatal where appropriate."
            }
            (Self::LoggingVerbose, false) => {
                "Send timestamped diagnostics to stderr and accept both --verbose and -v."
            }
            (Self::LoggingVerbose, true) => {
                "Use the debug module for progress visible with ansible-playbook -v."
            }
            (Self::ConfigBlock, false) => {
                "Collect configurable paths, hosts, packages and other literals in a documented configuration block near the top."
            }
            (Self::ConfigBlock, true) => {
                "Collect configurable paths, hosts, packages and other literals in a vars block near the top."
            }
            (Self::EndSummary, false) => {
                "Print a final summary of work performed with success and failure counts."
            }
            (Self::EndSummary, true) => "End with a debug summary of what changed.",
            (Self::StyleGuideClean, false) => {
                "Follow the source language's style guide and common lint conventions."
            }
            (Self::StyleGuideClean, true) => {
                "Follow ansible-lint conventions and use fully qualified module names."
            }
            (Self::PreconditionChecks, false) => {
                "Verify the commands, privileges and connectivity required by the existing script before performing any work."
            }
            (Self::PreconditionChecks, true) => {
                "Use pre_tasks and assert to check required privileges, packages and connectivity before making changes."
            }
            (Self::Idempotency, false) => {
                "Detect completed steps and skip them so the script is safe to run again."
            }
            (Self::Idempotency, true) => {
                "Use idempotent modules or creates and removes guards to make the playbook safe to run again."
            }
            (Self::SafeMode, false) => {
                "Accept --dry-run to describe intended changes without executing them. Confirm destructive operations unless --yes is set."
            }
            (Self::SafeMode, true) => {
                "Support check mode (--check) and guard destructive tasks so dry runs make no changes."
            }
            (Self::HelpUsage, false) => {
                "Accept --help to show usage and parse command-line arguments for configurable values."
            }
            (Self::HelpUsage, true) => {
                "Document variables and overrides through --extra-vars at the top of the playbook."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum InputOption {
    ParameterValidation,
    FileChecks,
    FileSizeLimit,
    SecurityLogging,
    ForceOverride,
}

impl InputOption {
    pub(crate) const ALL: [Self; 5] = [
        Self::ParameterValidation,
        Self::FileChecks,
        Self::FileSizeLimit,
        Self::SecurityLogging,
        Self::ForceOverride,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ParameterValidation => "Validate parameters",
            Self::FileChecks => "Check input file access and format",
            Self::FileSizeLimit => "Limit input file size",
            Self::SecurityLogging => "Log security violations",
            Self::ForceOverride => "Allow force override",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::ParameterValidation => "INPUT-PARAMETERS",
            Self::FileChecks => "INPUT-FILE-CHECKS",
            Self::FileSizeLimit => "INPUT-FILE-SIZE",
            Self::SecurityLogging => "INPUT-SECURITY-LOGGING",
            Self::ForceOverride => "INPUT-FORCE-OVERRIDE",
        }
    }

    pub(crate) fn instruction(self, mib: u16) -> String {
        match self {
            Self::ParameterValidation => "Validate the expected parameter count with a usage hint. Use each parameter's actual purpose to choose a character allowlist and a length limit of at most 4096 characters. Reject control characters, NUL bytes and shell metacharacters where inappropriate. Report violations to stderr and exit with status 64 before work starts.".into(),
            Self::FileChecks => "Verify every input file exists and is readable before size or content checks (status 66). Only after all enabled metadata size checks pass, verify the expected format (status 65). For text files inspect at most the first 512 bytes for NUL and use file --mime-type or the platform equivalent only if available. Keep the bounded NUL check as fallback.".into(),
            Self::FileSizeLimit => format!("Define MAX_FILE_SIZE = {} in bytes as an adjustable guard configuration value. Zero means unlimited and skips the size check. For a positive limit use a metadata-only size query on every input file before reading even one byte, including any format probe. Reject oversized files or failed metadata queries with status 65. Never read or stream a file to calculate its size.", file_size_bytes(mib)),
            Self::SecurityLogging => "Write each violation and any forced bypass as a timestamped SECURITY: warning to stderr. Also write the same line to the script's existing log when it has one.".into(),
            Self::ForceOverride => "With FORCE set to 1 or --force anywhere on the command line, report each violation and a separate bypass warning, then continue. Exclude --force from every other argument check including the count. Keep enforcement enabled by default.".into(),
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub(crate) enum Requirement {
    Hardening {
        option: HardeningOption,
        declarative: bool,
    },
    Input {
        option: InputOption,
        #[serde(rename = "maxFileSizeMib")]
        max_file_size_mib: u16,
    },
    Header {
        snippet: HeaderRef,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        managed_sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        omitted_sha256: Option<String>,
    },
}

impl fmt::Debug for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Requirement")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl Requirement {
    pub(crate) fn resolved_header(
        snippet: HeaderRef,
        resolved: &str,
        managed_sha256: Option<String>,
    ) -> Result<Self, &'static str> {
        let mut lines = source_lines(resolved).peekable();
        if matches!(snippet, HeaderRef::Managed { .. })
            && lines.peek().is_some_and(|line| line.starts_with("#!"))
        {
            lines.next();
        }
        let text = lines.collect::<Vec<_>>().join("\n");
        let requirement = Self::Header {
            snippet,
            text,
            managed_sha256,
            omitted_sha256: None,
        };
        if requirement.valid() {
            Ok(requirement)
        } else {
            Err(
                "Choose a nonempty script header without a shebang or control characters (up to 8 KiB).",
            )
        }
    }

    pub(crate) fn id(&self) -> &'static str {
        match self {
            Self::Hardening { option, .. } => option.id(),
            Self::Input { option, .. } => option.id(),
            Self::Header { .. } => "SCRIPT-HEADER",
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Hardening { option, .. } => option.label(),
            Self::Input { option, .. } => option.label(),
            Self::Header { .. } => "Add the selected script header",
        }
    }

    pub(crate) fn instruction(&self) -> String {
        match self {
            Self::Hardening { option, declarative } => option.instruction(*declarative).into(),
            Self::Input { option, max_file_size_mib } => format!(
                "Add to the script's input hardening guard, after the shebang and language prologue but before actual work. Use built-in or standard-library facilities where possible and check for any external helper before using it. Report violations to stderr. {}",
                option.instruction(*max_file_size_mib),
            ),
            Self::Header { .. } => "Copy the approved header lines exactly once near the top, following any required shebang or language prologue. Treat the header as source text and preserve its bytes, including prose and indentation, apart from using the source line endings.".into(),
        }
    }

    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Hardening { .. } => true,
            Self::Input {
                option,
                max_file_size_mib,
            } => {
                *max_file_size_mib <= MAX_FILE_SIZE_MIB
                    && (*option == InputOption::FileSizeLimit || *max_file_size_mib == 0)
            }
            Self::Header {
                snippet,
                text,
                managed_sha256,
                omitted_sha256,
            } => {
                let source_bound = match (snippet, managed_sha256) {
                    (HeaderRef::User(_), None) => true,
                    (HeaderRef::Managed { .. }, Some(hash)) => valid_sha256(hash),
                    _ => false,
                };
                if let Some(hash) = omitted_sha256 {
                    return source_bound && text.is_empty() && valid_sha256(hash);
                }
                source_bound
                    && !text.trim().is_empty()
                    && text.len() <= MAX_HEADER_BYTES
                    && !text
                        .chars()
                        .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\n'))
                    && !text.contains('\u{feff}')
                    && !text.starts_with("#!")
            }
        }
    }

    pub(in crate::snippet_analysis) fn has_source(&self) -> bool {
        !matches!(
            self,
            Self::Header {
                omitted_sha256: Some(_),
                ..
            }
        )
    }

    pub(in crate::snippet_analysis) fn fits_retention(&self, limit: usize) -> bool {
        match self {
            Self::Header {
                text,
                omitted_sha256: None,
                ..
            } => super::source_fits(text, limit),
            _ => true,
        }
    }

    pub(in crate::snippet_analysis) fn apply_retention(
        &mut self,
        limit: usize,
    ) -> std::io::Result<()> {
        if let Self::Header {
            text,
            omitted_sha256,
            ..
        } = self
            && omitted_sha256.is_none()
            && !super::source_fits(text, limit)
        {
            *omitted_sha256 = Some(super::content_hash(text)?);
            *text = String::new();
        }
        Ok(())
    }

    pub(in crate::snippet_analysis) fn matches_live(&self, live: &Self) -> bool {
        if let (
            Self::Header {
                snippet,
                managed_sha256,
                omitted_sha256: Some(hash),
                ..
            },
            Self::Header {
                snippet: current,
                text,
                managed_sha256: current_managed,
                omitted_sha256: None,
            },
        ) = (self, live)
        {
            return snippet == current
                && managed_sha256 == current_managed
                && super::content_hash(text).is_ok_and(|actual| actual == *hash);
        }
        self == live
    }

    pub(crate) fn supports(&self, language: &str) -> bool {
        let declarative = is_declarative(language);
        match self {
            Self::Hardening {
                declarative: expected,
                ..
            } => declarative == *expected,
            Self::Input { .. } => !declarative,
            Self::Header { .. } => true,
        }
    }

    pub(crate) fn present(&self, source: &str) -> bool {
        let required: &[&str] = match self {
            Self::Hardening {
                option: HardeningOption::LoggingVerbose,
                declarative: false,
            } => &["--verbose", "-v"],
            Self::Hardening {
                option: HardeningOption::SafeMode,
                declarative: false,
            } => &["--dry-run", "--yes"],
            Self::Hardening {
                option: HardeningOption::HelpUsage,
                declarative: false,
            } => &["--help"],
            Self::Hardening {
                option: HardeningOption::SafeMode,
                declarative: true,
            } => &["--check"],
            Self::Hardening {
                option: HardeningOption::HelpUsage,
                declarative: true,
            } => &["--extra-vars"],
            Self::Input {
                option: InputOption::SecurityLogging,
                ..
            } => &["SECURITY:"],
            Self::Input {
                option: InputOption::ForceOverride,
                ..
            } => &["FORCE", "--force"],
            Self::Input {
                option: InputOption::FileSizeLimit,
                max_file_size_mib,
            } => {
                let bytes = file_size_bytes(*max_file_size_mib).to_string();
                return source_lines(source).any(|line| {
                    let Some((name, value)) = line.split_once('=') else {
                        return false;
                    };
                    !name.trim_start().starts_with(['#', ';'])
                        && has_token(name, "MAX_FILE_SIZE")
                        && value
                            .trim_start()
                            .trim_start_matches(['\'', '"'])
                            .strip_prefix(&bytes)
                            .is_some_and(|suffix| {
                                suffix.is_empty()
                                    || suffix.starts_with([' ', '\t', '\'', '"', ';', ',', ')'])
                            })
                });
            }
            Self::Header {
                text,
                omitted_sha256: None,
                ..
            } => {
                let header: Vec<_> = source_lines(text).collect();
                let lines: Vec<_> = source_lines(source).collect();
                if header.is_empty() {
                    return false;
                }
                let mut copies = lines
                    .windows(header.len())
                    .enumerate()
                    .filter(|(_, lines)| *lines == header);
                return copies.next().is_some_and(|(start, _)| start < 32)
                    && copies.next().is_none();
            }
            Self::Header { .. } => return false,
            _ => &[],
        };
        required.iter().all(|token| has_token(source, token))
    }
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(crate) fn is_declarative(language: &str) -> bool {
    let language = language.trim().to_ascii_lowercase();
    matches!(language.as_str(), "yaml" | "yml") || language.contains("ansible")
}

fn file_size_bytes(mib: u16) -> u64 {
    u64::from(mib) * 1_048_576
}

pub(crate) fn source_lines(source: &str) -> impl Iterator<Item = &str> {
    let mut remaining = source.strip_prefix('\u{feff}').unwrap_or(source);
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        let end = remaining.find(['\r', '\n']).unwrap_or(remaining.len());
        let (line, rest) = remaining.split_at(end);
        remaining = rest
            .strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix("\n\r"))
            .or_else(|| rest.strip_prefix(['\r', '\n']))
            .unwrap_or_default();
        Some(line)
    })
}

fn has_token(source: &str, token: &str) -> bool {
    let identifier_byte = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-');
    source.match_indices(token).any(|(start, _)| {
        !start
            .checked_sub(1)
            .is_some_and(|index| identifier_byte(source.as_bytes()[index]))
            && (token.ends_with(':')
                || !source
                    .as_bytes()
                    .get(start + token.len())
                    .is_some_and(|byte| identifier_byte(*byte)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_requires_both_flags_and_does_not_match_prefixes() {
        let requirement = Requirement::Hardening {
            option: HardeningOption::LoggingVerbose,
            declarative: false,
        };
        assert!(requirement.present("case \"$1\" in --verbose|-v)"));
        assert!(!requirement.present("--verbose --validate"));
        assert!(!requirement.present("--verbose-output -v"));
    }

    #[test]
    fn file_size_checks_the_exact_selected_byte_value() {
        let requirement = Requirement::Input {
            option: InputOption::FileSizeLimit,
            max_file_size_mib: 7,
        };
        assert!(requirement.present("$MAX_FILE_SIZE = 7340032"));
        assert!(!requirement.present("MAX_FILE_SIZE = 107340032"));
        assert!(!requirement.present("MAX_FILE_SIZE = 0\nOTHER = 7340032"));
        assert!(!requirement.present("MAX_FILE_SIZE = 0 # was 7340032"));
        assert!(!requirement.supports("Ansible playbook"));
    }

    #[test]
    fn reviewed_header_is_an_exact_unique_line_block_with_native_endings() {
        let requirement = Requirement::Header {
            snippet: HeaderRef::User(SnippetId::new().unwrap()),
            text: "# Creator: Daniel\n#\n  # Generated".into(),
            managed_sha256: None,
            omitted_sha256: None,
        };
        assert!(requirement.present(
            "\u{feff}#!/bin/sh\r\n# Creator: Daniel\r\n#\r\n  # Generated\r\nprintf ok\r\n"
        ));
        assert!(!requirement.present("# Creator: Daniel\n#\n# Generated\nprintf ok"));
        assert!(
            !requirement.present(
                "# Creator: Daniel\n#\n  # Generated\n# Creator: Daniel\n#\n  # Generated"
            )
        );
    }
}
