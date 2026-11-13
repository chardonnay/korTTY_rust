//! Persisted source boundaries used by manual and automatic knowledge indexing.

use std::io::{self, Read as _};
use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};

use crate::knowledge_pdf as pdf;

const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_BYTES: usize = 512;
const MAX_IGNORE_BYTES: u64 = 64 * 1024;
pub(in crate::ai_manager::local) const DEFAULT_TEXT_BYTES: usize = 128 * 1024;
const PREVIOUS_REVIEWED_TEXT_BYTES: usize = 2 * 1024 * 1024;
const PREVIOUS_EXTENDED_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub(super) const PREVIOUS_LARGE_TEXT_BYTES: usize = 32 * 1024 * 1024;
pub(in crate::ai_manager::local) const MAX_TEXT_BYTES: usize = 50 * 1024 * 1024;
pub(in crate::ai_manager::local) const MAX_FILE_BYTES: u64 = pdf::MAX_FILE_BYTES as u64;

pub(in crate::ai_manager::local) fn validate_file_size(limit: Option<u64>) -> Result<(), String> {
    if limit.is_some_and(|bytes| !(1..=MAX_FILE_BYTES).contains(&bytes)) {
        return Err(format!(
            "Enter a maximum file size from 1 to {MAX_FILE_BYTES} bytes, or leave it blank."
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub(in crate::ai_manager::local) enum FileSizeLimit {
    // Preserve the interpretation and serialization of existing size approvals.
    Legacy(u64),
    Reviewed { bytes: u64 },
    Extended { extended_bytes: u64 },
    LargeText { large_text_bytes: u64 },
    CorpusText { corpus_text_bytes: u64 },
}

impl FileSizeLimit {
    pub(in crate::ai_manager::local) fn reviewed(bytes: u64) -> Self {
        Self::reviewed_for_text(bytes, true)
    }

    pub(in crate::ai_manager::local) fn reviewed_for_text(bytes: u64, text: bool) -> Self {
        if text && bytes > PREVIOUS_LARGE_TEXT_BYTES as u64 {
            Self::CorpusText {
                corpus_text_bytes: bytes,
            }
        } else if text && bytes > PREVIOUS_EXTENDED_TEXT_BYTES as u64 {
            Self::LargeText {
                large_text_bytes: bytes,
            }
        } else if bytes > PREVIOUS_REVIEWED_TEXT_BYTES as u64 {
            Self::Extended {
                extended_bytes: bytes,
            }
        } else if bytes > DEFAULT_TEXT_BYTES as u64 {
            Self::Reviewed { bytes }
        } else {
            Self::Legacy(bytes)
        }
    }

    pub(in crate::ai_manager::local) fn bytes(self) -> u64 {
        match self {
            Self::Legacy(bytes)
            | Self::Reviewed { bytes }
            | Self::Extended {
                extended_bytes: bytes,
            }
            | Self::LargeText {
                large_text_bytes: bytes,
            }
            | Self::CorpusText {
                corpus_text_bytes: bytes,
            } => bytes,
        }
    }

    pub(in crate::ai_manager::local) fn is_reviewed(self) -> bool {
        !matches!(self, Self::Legacy(_))
    }

    pub(in crate::ai_manager::local) fn uses_previous_boundary(self) -> bool {
        match self {
            Self::Legacy(bytes) => bytes > DEFAULT_TEXT_BYTES as u64,
            Self::Reviewed { bytes } => bytes > PREVIOUS_REVIEWED_TEXT_BYTES as u64,
            Self::Extended { extended_bytes } => {
                extended_bytes > PREVIOUS_EXTENDED_TEXT_BYTES as u64
            }
            Self::LargeText { large_text_bytes } => {
                large_text_bytes > PREVIOUS_LARGE_TEXT_BYTES as u64
            }
            Self::CorpusText { .. } => false,
        }
    }

    pub(super) fn validate(self) -> Result<(), String> {
        let valid = match self {
            Self::Legacy(bytes) => (1..=pdf::DEFAULT_FILE_BYTES as u64).contains(&bytes),
            Self::Reviewed { bytes } => (DEFAULT_TEXT_BYTES as u64 + 1
                ..=pdf::PREVIOUS_REVIEWED_FILE_BYTES as u64)
                .contains(&bytes),
            Self::Extended { extended_bytes } => {
                (PREVIOUS_REVIEWED_TEXT_BYTES as u64 + 1..=MAX_FILE_BYTES).contains(&extended_bytes)
            }
            Self::LargeText { large_text_bytes } => (PREVIOUS_EXTENDED_TEXT_BYTES as u64 + 1
                ..=MAX_FILE_BYTES)
                .contains(&large_text_bytes),
            Self::CorpusText { corpus_text_bytes } => {
                (PREVIOUS_LARGE_TEXT_BYTES as u64 + 1..=MAX_FILE_BYTES).contains(&corpus_text_bytes)
            }
        };
        if !valid {
            return Err("Invalid saved knowledge source size limit.".into());
        }
        Ok(())
    }
}

pub(super) fn format_file_size(path: &Path, limit: Option<FileSizeLimit>) -> usize {
    match (pdf::is_pdf(path), limit) {
        (
            true,
            Some(
                FileSizeLimit::Extended { .. }
                | FileSizeLimit::LargeText { .. }
                | FileSizeLimit::CorpusText { .. },
            ),
        ) => pdf::MAX_FILE_BYTES,
        (true, Some(FileSizeLimit::Reviewed { .. })) => pdf::PREVIOUS_REVIEWED_FILE_BYTES,
        (true, _) => pdf::DEFAULT_FILE_BYTES,
        (false, Some(FileSizeLimit::CorpusText { .. })) => MAX_TEXT_BYTES,
        (false, Some(FileSizeLimit::LargeText { .. })) => PREVIOUS_LARGE_TEXT_BYTES,
        (false, Some(FileSizeLimit::Extended { .. })) => PREVIOUS_EXTENDED_TEXT_BYTES,
        (false, Some(FileSizeLimit::Reviewed { .. })) => PREVIOUS_REVIEWED_TEXT_BYTES,
        (false, _) => DEFAULT_TEXT_BYTES,
    }
}

pub(super) fn effective_file_size(path: &Path, limit: Option<FileSizeLimit>) -> u64 {
    let format = format_file_size(path, limit) as u64;
    limit.map_or(format, |limit| limit.bytes().min(format))
}

#[cfg(test)]
#[test]
fn older_reviewed_limit_preserves_its_format_ceilings_until_reapproved() {
    let size = pdf::PREVIOUS_REVIEWED_FILE_BYTES as u64;
    let old: FileSizeLimit = serde_json::from_value(serde_json::json!({"bytes": size})).unwrap();
    let current = FileSizeLimit::reviewed(size);
    let text = Path::new("source.txt");
    let document = Path::new("source.pdf");

    assert!(old.uses_previous_boundary());
    assert_eq!(
        effective_file_size(text, Some(old)),
        PREVIOUS_REVIEWED_TEXT_BYTES as u64
    );
    assert_eq!(effective_file_size(document, Some(old)), size);
    assert_eq!(effective_file_size(text, Some(current)), size);
    assert!(!current.uses_previous_boundary());
}

#[cfg(test)]
#[test]
fn explicitly_large_text_keeps_earlier_folder_and_pdf_limits() {
    let bytes = PREVIOUS_LARGE_TEXT_BYTES as u64;
    let old: FileSizeLimit =
        serde_json::from_value(serde_json::json!({"extended_bytes": bytes})).unwrap();
    let renewed = FileSizeLimit::reviewed(bytes);
    let text = Path::new("source.txt");
    let document = Path::new("source.pdf");
    assert!(old.uses_previous_boundary());
    assert_eq!(
        effective_file_size(text, Some(old)),
        PREVIOUS_EXTENDED_TEXT_BYTES as u64
    );
    assert_eq!(effective_file_size(text, Some(renewed)), bytes);
    assert_eq!(effective_file_size(document, Some(old)), bytes);
    assert_eq!(effective_file_size(document, Some(renewed)), bytes);
    assert_eq!(FileSizeLimit::reviewed_for_text(bytes, false), old);
    assert_eq!(
        serde_json::to_value(renewed).unwrap(),
        serde_json::json!({"large_text_bytes": bytes})
    );
}

#[cfg(test)]
#[test]
fn explicitly_reviewed_corpus_text_limit_preserves_the_old_32_mib_boundary() {
    let bytes = MAX_TEXT_BYTES as u64;
    let older: FileSizeLimit =
        serde_json::from_value(serde_json::json!({"large_text_bytes": bytes})).unwrap();
    let approved = FileSizeLimit::reviewed(bytes);
    let text = Path::new("source.txt");
    assert!(older.uses_previous_boundary());
    assert!(!approved.uses_previous_boundary());
    assert_eq!(
        effective_file_size(text, Some(older)),
        PREVIOUS_LARGE_TEXT_BYTES as u64
    );
    assert_eq!(effective_file_size(text, Some(approved)), bytes);
    assert_eq!(
        serde_json::to_value(approved).unwrap(),
        serde_json::json!({"corpus_text_bytes": bytes})
    );
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SyncMode {
    #[default]
    Manual,
    Automatic,
}

impl SyncMode {
    pub(in crate::ai_manager::local) const ALL: [Self; 2] = [Self::Manual, Self::Automatic];
}

impl std::fmt::Display for SyncMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Manual => "Manual",
            Self::Automatic => "Automatic",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(in crate::ai_manager::local) struct Options {
    pub(in crate::ai_manager::local) sync: SyncMode,
    pub(in crate::ai_manager::local) recursive: bool,
    pub(in crate::ai_manager::local) respect_gitignore: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub(in crate::ai_manager::local) include_pdf: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::ai_manager::local) max_file_bytes: Option<FileSizeLimit>,
    pub(in crate::ai_manager::local) includes: Vec<String>,
    pub(in crate::ai_manager::local) excludes: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            sync: SyncMode::Manual,
            recursive: true,
            respect_gitignore: true,
            include_pdf: false,
            max_file_bytes: None,
            includes: Vec::new(),
            excludes: Vec::new(),
        }
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Options {
    pub(in crate::ai_manager::local) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(in crate::ai_manager::local) fn validate(&self) -> Result<(), String> {
        Filter::new(self).map(|_| ())
    }
}

pub(in crate::ai_manager::local) struct Filter {
    includes: Vec<GlobMatcher>,
    excludes: Vec<GlobMatcher>,
}

impl Filter {
    pub(in crate::ai_manager::local) fn new(options: &Options) -> Result<Self, String> {
        if let Some(limit) = options.max_file_bytes {
            limit.validate()?;
        }
        let mut excludes = compile(&options.excludes)?;
        let subtrees = options
            .excludes
            .iter()
            .filter_map(|pattern| pattern.strip_suffix("/**"))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        excludes.extend(compile(&subtrees)?);
        Ok(Self {
            includes: compile(&options.includes)?,
            excludes,
        })
    }

    pub(in crate::ai_manager::local) fn allows(&self, path: &Path, directory: bool) -> bool {
        !matches_any(&self.excludes, path, directory)
            && (directory
                || self.includes.is_empty()
                || self.includes.iter().any(|pattern| pattern.is_match(path)))
    }
}

#[derive(Default)]
pub(in crate::ai_manager::local) struct Ignore(Vec<IgnoreRule>);

struct IgnoreRule {
    path: GlobMatcher,
    descendants: GlobMatcher,
    directory_only: bool,
    excluded: bool,
}

impl Ignore {
    pub(in crate::ai_manager::local) fn load(
        root: &crate::private_files::Directory,
    ) -> Result<(Self, Option<[u8; 32]>), String> {
        let file = match root.open_file(".gitignore") {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((Self::default(), None));
            }
            Err(error) => return Err(format!("Could not read the source .gitignore ({error}).")),
        };
        let stamp = crate::private_files::FileStamp::read(&file)
            .map_err(|error| format!("Could not verify the source .gitignore ({error})."))?;
        let mut text = String::new();
        (&file)
            .take(MAX_IGNORE_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|error| format!("Could not read the source .gitignore ({error})."))?;
        if text.len() as u64 > MAX_IGNORE_BYTES {
            return Err("The source .gitignore exceeds 64 KiB.".into());
        }
        stamp
            .require_unchanged(&file)
            .and_then(|()| stamp.require_unchanged(&root.open_file(".gitignore")?))
            .map_err(|error| format!("The source .gitignore changed ({error})."))?;
        let digest = super::sha256(text.as_bytes()).map_err(|error| error.to_string())?;
        Ok((Self::parse(&text)?, Some(digest)))
    }

    fn parse(text: &str) -> Result<Self, String> {
        let mut rules = Vec::new();
        for line in text.lines().map(ignore_line) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if rules.len() == MAX_PATTERNS {
                return Err(format!(
                    "The source .gitignore exceeds {MAX_PATTERNS} rules."
                ));
            }
            if line.len() > MAX_PATTERN_BYTES {
                return Err("A source .gitignore rule exceeds 512 bytes.".into());
            }
            let (line, excluded) = line
                .strip_prefix('!')
                .map_or((line, true), |line| (line, false));
            let directory_only = line.ends_with('/');
            let rooted = line.starts_with('/');
            let line = line.trim_matches('/');
            let pattern = if !rooted && !line.contains('/') {
                format!("**/{line}")
            } else {
                line.to_owned()
            };
            let path = ignore_matcher(&pattern)?;
            let descendants = ignore_matcher(&format!("{pattern}/**"))?;
            rules.push(IgnoreRule {
                path,
                descendants,
                directory_only,
                excluded,
            });
        }
        Ok(Self(rules))
    }

    pub(in crate::ai_manager::local) fn allows(&self, path: &Path, directory: bool) -> bool {
        let mut excluded = false;
        for rule in &self.0 {
            if (rule.path.is_match(path) && (directory || !rule.directory_only))
                || rule.descendants.is_match(path)
            {
                excluded = rule.excluded;
            }
        }
        !excluded
    }
}

fn ignore_line(mut line: &str) -> &str {
    while line.ends_with(' ') {
        let preceding = line[..line.len() - 1]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count();
        if preceding % 2 == 1 {
            break;
        }
        line = &line[..line.len() - 1];
    }
    line
}

fn ignore_matcher(pattern: &str) -> Result<GlobMatcher, String> {
    if pattern.is_empty() || pattern.chars().any(char::is_control) {
        return Err("A source .gitignore rule contains an invalid path.".into());
    }
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .case_insensitive(cfg!(windows))
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| format!("Invalid source .gitignore rule ({error})."))
}

fn compile(patterns: &[String]) -> Result<Vec<GlobMatcher>, String> {
    if patterns.len() > MAX_PATTERNS {
        return Err(format!("Use at most {MAX_PATTERNS} source patterns."));
    }
    patterns
        .iter()
        .map(|pattern| {
            if pattern.is_empty()
                || pattern.len() > MAX_PATTERN_BYTES
                || pattern.starts_with('/')
                || pattern.contains('\\')
                || pattern.contains(';')
                || pattern.trim() != pattern
                || pattern.split('/').any(|part| part == "..")
                || pattern.chars().any(char::is_control)
            {
                return Err("Use bounded relative glob patterns with forward slashes.".into());
            }
            let pattern = pattern.trim_end_matches('/');
            if pattern.is_empty() {
                return Err("A source pattern needs a file or directory name.".into());
            }
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .case_insensitive(cfg!(windows))
                .build()
                .map(|glob| glob.compile_matcher())
                .map_err(|error| format!("Invalid source pattern ({error})."))
        })
        .collect()
}

fn matches_any(patterns: &[GlobMatcher], path: &Path, directory: bool) -> bool {
    patterns.iter().any(|pattern| {
        pattern.is_match(path)
            || (!directory
                && path
                    .ancestors()
                    .skip(1)
                    .any(|parent| !parent.as_os_str().is_empty() && pattern.is_match(parent)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_options_preserve_manual_migration_and_validate_scope() {
        assert_eq!(
            serde_json::from_str::<Options>("{}").unwrap(),
            Options::default()
        );
        assert_eq!(Options::default().sync, SyncMode::Manual);
        for pattern in ["../private/*", "/private/*", "C:\\private\\*", "\n", "/"] {
            let options = Options {
                includes: vec![pattern.into()],
                ..Options::default()
            };
            assert!(options.validate().is_err(), "{pattern:?}");
        }
    }

    #[test]
    fn source_globs_filter_files_without_hiding_included_descendants() {
        let filter = Filter::new(&Options {
            includes: vec!["**/*.rs".into()],
            excludes: vec!["generated/**".into(), "**/private*".into()],
            ..Options::default()
        })
        .unwrap();
        assert!(filter.allows(Path::new("src"), true));
        assert!(filter.allows(Path::new("src/main.rs"), false));
        assert!(filter.allows(Path::new("main.rs"), false));
        assert!(!filter.allows(Path::new("src/readme.txt"), false));
        assert!(!filter.allows(Path::new("generated/output.rs"), false));
        assert!(!filter.allows(Path::new("generated"), true));
        assert!(!filter.allows(Path::new("src/private.rs"), false));
    }

    #[test]
    fn source_ignore_preserves_rule_order_rooted_paths_and_directory_rules() {
        let ignore = Ignore::parse("# comment\n*.tmp\n!keep.tmp\n/build/\ncache/\n").unwrap();
        assert!(!ignore.allows(Path::new("src/cache"), true));
        assert!(!ignore.allows(Path::new("src/cache/file.rs"), false));
        assert!(!ignore.allows(Path::new("build/file.rs"), false));
        assert!(ignore.allows(Path::new("src/build/file.rs"), false));
        assert!(!ignore.allows(Path::new("src/old.tmp"), false));
        assert!(ignore.allows(Path::new("src/keep.tmp"), false));
        assert!(ignore.allows(Path::new("cache"), false));
        let escaped = Ignore::parse("\\#notes\n\\!notes\ntrailing\\ \n").unwrap();
        assert!(!escaped.allows(Path::new("#notes"), false));
        assert!(!escaped.allows(Path::new("!notes"), false));
        assert!(!escaped.allows(Path::new("trailing "), false));
        assert!(escaped.allows(Path::new("trailing"), false));
    }
}
