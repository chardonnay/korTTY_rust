use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use regex::{Captures, Regex, RegexBuilder};
use toml_edit::{Item, Table};
use zeroize::Zeroizing;

use crate::session_journal::LogFormat;

use super::{Config, DirectoryMode};

const MAX_RULES: usize = 64;
pub(super) const MAX_REDACTED: usize = 32 * 1024;

#[derive(Clone, Default)]
pub(crate) struct Policy {
    include_username: Option<bool>,
    directory_mode: Option<DirectoryMode>,
    journal_directory: Option<PathBuf>,
    journal_log_format: Option<LogFormat>,
    journal_clickable_links_denied: bool,
    journal_rename_denied: bool,
    journal_delete_denied: bool,
    journal_ai_screenshot_analysis: Option<bool>,
    pub(super) rules: Vec<Arc<Rule>>,
}

pub(super) struct Rule {
    matcher: Regex,
    replacement: String,
    regex: bool,
}

impl Policy {
    pub(crate) fn has_redactions(&self) -> bool {
        !self.rules.is_empty()
    }

    pub(crate) fn journal_directory(&self) -> Option<&Path> {
        self.journal_directory.as_deref()
    }

    pub(crate) fn journal_log_format(&self) -> Option<LogFormat> {
        self.journal_log_format
    }

    pub(crate) fn journal_clickable_links_allowed(&self) -> bool {
        !self.journal_clickable_links_denied
    }

    pub(crate) fn journal_rename_allowed(&self) -> bool {
        !self.journal_rename_denied
    }

    pub(crate) fn journal_delete_allowed(&self) -> bool {
        !self.journal_delete_denied
    }

    pub(crate) fn journal_ai_screenshot_analysis(&self) -> Option<bool> {
        self.journal_ai_screenshot_analysis
    }

    pub(crate) fn apply(&self, config: &mut Config) {
        if let Some(include) = self.include_username {
            config.include_username = include;
        }
        if let Some(mode) = self.directory_mode {
            config.directory_mode = mode;
        }
    }

    pub(crate) fn read_rule(&mut self, rule: &Table) -> io::Result<()> {
        if let Some(logging) = table(rule, "logging")? {
            allowed(
                logging,
                &[
                    "terminal-include-username",
                    "terminal-directory-mode",
                    "directory",
                    "retention-days",
                    "compress",
                    "format",
                    "rotation-max-files",
                    "rotation-total-size-mb",
                ],
            )?;
            if let Some(include) = boolean(logging, "terminal-include-username")? {
                self.include_username = Some(self.include_username.unwrap_or(true) && include);
            }
            if let Some(mode) = string(logging, "terminal-directory-mode")? {
                let mode = match mode {
                    "none" => DirectoryMode::None,
                    "full" => DirectoryMode::Full,
                    "relative" => DirectoryMode::Relative,
                    _ => return Err(invalid("Invalid managed terminal log directory mode")),
                };
                self.directory_mode = Some(
                    self.directory_mode
                        .map_or(mode, |current| current.max(mode)),
                );
            }
        }
        if let Some(journal) = table(rule, "session-journal")? {
            allowed(
                journal,
                &[
                    "replace",
                    "storage-path",
                    "log-format",
                    "clickable-links",
                    "allow-rename",
                    "allow-delete",
                    "ai-screenshot-analysis",
                ],
            )?;
            self.journal_clickable_links_denied |=
                boolean(journal, "clickable-links")? == Some(false);
            self.journal_rename_denied |= boolean(journal, "allow-rename")? == Some(false);
            self.journal_delete_denied |= boolean(journal, "allow-delete")? == Some(false);
            if let Some(value) = string(journal, "log-format")? {
                let format = LogFormat::parse_policy(value).ok_or_else(|| {
                    invalid("Managed session journal log format must be XML, JSON or YAML")
                })?;
                if self
                    .journal_log_format
                    .is_none_or(|current| format.extension() < current.extension())
                {
                    self.journal_log_format = Some(format);
                }
            }
            if let Some(enabled) = boolean(journal, "ai-screenshot-analysis")? {
                self.journal_ai_screenshot_analysis =
                    Some(self.journal_ai_screenshot_analysis.unwrap_or(true) && enabled);
            }
            if let Some(value) = string(journal, "storage-path")? {
                let path = Path::new(value);
                if !path.is_absolute()
                    || value.chars().any(char::is_control)
                    || path.components().any(|part| part == Component::ParentDir)
                {
                    return Err(invalid(
                        "Managed session journal storage requires an absolute path",
                    ));
                }
                if self.journal_directory.as_ref().is_none_or(|current| {
                    value
                        .encode_utf16()
                        .lt(current.to_string_lossy().encode_utf16())
                }) {
                    self.journal_directory = Some(path.to_owned());
                }
            }
            if let Some(entries) = journal.get("replace") {
                let entries = entries.as_array_of_tables().ok_or_else(|| {
                    invalid("Session journal replacements must be an array of tables")
                })?;
                for entry in entries {
                    if self.rules.len() == MAX_RULES {
                        return Err(invalid("Too many managed terminal redaction rules"));
                    }
                    self.rules.push(Arc::new(Rule::compile(entry)?));
                }
            }
        }
        Ok(())
    }
}

impl Rule {
    fn compile(table: &Table) -> io::Result<Self> {
        allowed(
            table,
            &["pattern", "replacement", "regex", "ignore-case", "label"],
        )?;
        let pattern = string(table, "pattern")?
            .ok_or_else(|| invalid("A managed terminal redaction rule lacks a pattern"))?;
        let replacement = string(table, "replacement")?.unwrap_or("***");
        let regex = boolean(table, "regex")?.unwrap_or(false);
        let ignore_case = boolean(table, "ignore-case")?.unwrap_or(false);
        let _ = string(table, "label")?;
        if pattern.is_empty()
            || pattern.len() > 1024
            || replacement.len() > 1024
            || replacement.chars().any(char::is_control)
        {
            return Err(invalid(
                "Managed terminal redaction pattern or replacement exceeds safe limits",
            ));
        }
        let literal;
        let pattern = if regex {
            pattern
        } else {
            literal = regex::escape(pattern);
            &literal
        };
        let matcher = RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .size_limit(256 * 1024)
            .build()
            .map_err(|_| {
                invalid("Unsupported or malformed managed terminal redaction expression")
            })?;
        Ok(Self {
            matcher,
            replacement: replacement.into(),
            regex,
        })
    }

    pub(super) fn replace(&self, text: &str, deadline: Instant) -> Option<Zeroizing<String>> {
        if Instant::now() >= deadline {
            return None;
        }
        let mut out = Zeroizing::new(String::with_capacity(text.len()));
        let mut end = 0;
        for captures in self.matcher.captures_iter(text) {
            if Instant::now() >= deadline {
                return None;
            }
            let found = captures.get(0)?;
            append(&mut out, &text[end..found.start()])?;
            if self.regex {
                let mut replacement = Zeroizing::new(String::new());
                if expand(&self.replacement, &captures, &mut replacement).is_some() {
                    append(&mut out, &replacement)?;
                } else {
                    append(&mut out, &self.replacement)?;
                }
            } else {
                append(&mut out, &self.replacement)?;
            }
            end = found.end();
        }
        if Instant::now() >= deadline {
            return None;
        }
        append(&mut out, &text[end..])?;
        Some(out)
    }
}

// Java Matcher uses the longest numeric group that exists, or a braced named group.
fn expand(mut template: &str, captures: &Captures<'_>, result: &mut String) -> Option<()> {
    while let Some(index) = template.find(['$', '\\']) {
        append(result, &template[..index])?;
        let (prefix, tail) = template[index..].split_at(1);
        if prefix == "\\" {
            let char = tail.chars().next()?;
            append(result, &tail[..char.len_utf8()])?;
            template = &tail[char.len_utf8()..];
        } else if let Some(name) = tail.strip_prefix('{') {
            let (name, rest) = name.split_once('}')?;
            // A nonparticipating group has no text.
            if let Some(value) = captures.name(name) {
                append(result, value.as_str())?;
            }
            template = rest;
        } else {
            let first = tail.as_bytes().first()?.checked_sub(b'0')? as usize;
            if first > 9 || first >= captures.len() {
                return None;
            }
            let mut group = first;
            let mut len = 1;
            for byte in tail.bytes().skip(1).take_while(u8::is_ascii_digit) {
                let extended = group * 10 + usize::from(byte - b'0');
                if extended >= captures.len() {
                    break;
                }
                group = extended;
                len += 1;
            }
            if let Some(value) = captures.get(group) {
                append(result, value.as_str())?;
            }
            template = &tail[len..];
        }
    }
    append(result, template)
}

pub(super) fn append(target: &mut String, part: &str) -> Option<()> {
    if target.len().saturating_add(part.len()) > MAX_REDACTED {
        return None;
    }
    target.push_str(part);
    Some(())
}

fn allowed(table: &Table, keys: &[&str]) -> io::Result<()> {
    if table.iter().any(|(key, _)| !keys.contains(&key)) {
        return Err(invalid("Unsupported managed logging policy key"));
    }
    Ok(())
}
fn table<'a>(owner: &'a Table, name: &str) -> io::Result<Option<&'a Table>> {
    owner
        .get(name)
        .map(|item| {
            item.as_table()
                .ok_or_else(|| invalid("Invalid managed logging policy table"))
        })
        .transpose()
}
fn boolean(owner: &Table, name: &str) -> io::Result<Option<bool>> {
    owner
        .get(name)
        .map(|item| {
            item.as_bool()
                .ok_or_else(|| invalid("Expected a managed logging policy boolean"))
        })
        .transpose()
}
fn string<'a>(owner: &'a Table, name: &str) -> io::Result<Option<&'a str>> {
    owner
        .get(name)
        .map(|item: &Item| {
            item.as_str()
                .ok_or_else(|| invalid("Expected managed logging policy text"))
        })
        .transpose()
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_delete_policy_defaults_to_allowed_and_denial_wins_independently() -> io::Result<()> {
        let rule = |value: &str| {
            format!("[session-journal]\nallow-delete = {value}")
                .parse::<toml_edit::DocumentMut>()
                .unwrap()
        };
        let allow = rule("true");
        let deny = rule("false");
        assert!(Policy::default().journal_delete_allowed());
        for pair in [[&allow, &deny], [&deny, &allow]] {
            let mut policy = Policy::default();
            for rule in pair {
                policy.read_rule(rule.as_table())?;
            }
            assert!(!policy.journal_delete_allowed());
            assert!(policy.journal_rename_allowed());
        }
        assert!(
            Policy::default()
                .read_rule(rule("'false'").as_table())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn journal_rename_policy_defaults_to_allowed_and_denial_wins_independently() -> io::Result<()> {
        let rule = |value: &str| {
            format!("[session-journal]\nallow-rename = {value}")
                .parse::<toml_edit::DocumentMut>()
                .unwrap()
        };
        let allow = rule("true");
        let deny = rule("false");
        let mut policy = Policy::default();
        assert!(policy.journal_rename_allowed());
        policy.read_rule(allow.as_table())?;
        assert!(policy.journal_rename_allowed());
        for pair in [[&allow, &deny], [&deny, &allow]] {
            let mut policy = Policy::default();
            for rule in pair {
                policy.read_rule(rule.as_table())?;
            }
            assert!(!policy.journal_rename_allowed());
            assert!(policy.journal_clickable_links_allowed());
        }
        let links = "[session-journal]\nclickable-links = false"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        policy.read_rule(links.as_table())?;
        assert!(!policy.journal_clickable_links_allowed());
        assert!(policy.journal_rename_allowed());
        for value in ["'false'", "0", "[]", "{}"] {
            assert!(Policy::default().read_rule(rule(value).as_table()).is_err());
        }
        Ok(())
    }

    #[test]
    fn journal_log_format_policy_accepts_java_values_and_resolves_same_tier_conflicts()
    -> io::Result<()> {
        let rule = |value: &str| {
            format!("[session-journal]\nlog-format = {value}")
                .parse::<toml_edit::DocumentMut>()
                .unwrap()
        };
        let formats = [
            ("' XmL '", LogFormat::Xml),
            ("'JSON'", LogFormat::Json),
            ("'yaml'", LogFormat::Yaml),
        ];
        assert_eq!(Policy::default().journal_log_format(), None);
        for (value, format) in formats {
            let mut policy = Policy::default();
            policy.read_rule(rule(value).as_table())?;
            assert_eq!(policy.journal_log_format(), Some(format));
        }
        let mut policy = Policy::default();
        for value in ["'yaml'", "'xml'", "'json'", "'yaml'"] {
            policy.read_rule(rule(value).as_table())?;
        }
        assert_eq!(policy.journal_log_format(), Some(LogFormat::Json));
        for value in ["'jsonl'", "'gz'", "0", "true"] {
            assert!(Policy::default().read_rule(rule(value).as_table()).is_err());
        }
        Ok(())
    }

    #[test]
    fn journal_clickable_links_policy_is_strict_and_denial_wins() -> io::Result<()> {
        let rule = |value: &str| {
            format!("[session-journal]\nclickable-links = {value}")
                .parse::<toml_edit::DocumentMut>()
                .unwrap()
        };
        let mut policy = Policy::default();
        assert!(policy.journal_clickable_links_allowed());
        let allow = rule("true");
        policy.read_rule(allow.as_table())?;
        assert!(policy.journal_clickable_links_allowed());

        let deny = rule("false");
        for pair in [[&allow, &deny], [&deny, &allow]] {
            let mut policy = Policy::default();
            for rule in pair {
                policy.read_rule(rule.as_table())?;
            }
            assert!(!policy.journal_clickable_links_allowed());
        }
        assert!(policy.read_rule(rule("'false'").as_table()).is_err());
        Ok(())
    }

    #[test]
    fn expired_managed_masking_budget_produces_no_output() {
        let mut configured = Table::new();
        configured["pattern"] = toml_edit::value("(.*Z)|(x)");
        configured["regex"] = toml_edit::value(true);
        let rule = Rule::compile(&configured).unwrap();
        assert!(
            rule.replace(&"x".repeat(16 * 1024), Instant::now())
                .is_none()
        );
    }
}
