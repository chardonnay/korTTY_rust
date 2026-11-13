//! Native snippet and folder analysis snapshots.

use std::fmt::{self, Write as _};
use std::io;
use std::sync::Arc;
use std::time::SystemTime;

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::ai_manager::{KnowledgeCitation, ProfileRef};
use crate::snippets::{SnippetId, folders};

use super::diagram::AnalysisDiagram;

mod apply;
pub(crate) mod hardening;
pub(crate) mod modularization;
mod verification;

pub(in crate::snippet_analysis) use apply::APPLY_PLAN_VERSION;
pub(crate) use apply::{
    ApplyRecovery, ApplyRequestSnapshot, ApplyRun, ApplyStatus, ChangeReason, Checkpoint,
    MAX_APPLY_CONTENT_BYTES, PartialApply, Selection,
};
pub(crate) use verification::Verification;

pub(super) const DEFAULT_CONTENT_BYTES: usize = 1024 * 1024;
pub(super) const MAX_CONTENT_BYTES: usize = 5 * DEFAULT_CONTENT_BYTES;

pub(super) fn source_fits(content: &str, limit: usize) -> bool {
    limit != 0 && content.len() <= limit
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum ReportFormat {
    Html,
    Pdf,
    Markdown,
    Json,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum ExportPhase {
    BeforeApply,
    AfterApply,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportEntry {
    pub(crate) exported_at: u64,
    pub(crate) format: ReportFormat,
    pub(crate) phase: ExportPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) run_id: Option<SnippetId>,
    pub(crate) file_name: String,
}

impl ExportEntry {
    pub(crate) fn new(format: ReportFormat, run_id: Option<SnippetId>, file_name: &str) -> Self {
        Self {
            exported_at: now_millis(),
            format,
            phase: if run_id.is_some() {
                ExportPhase::AfterApply
            } else {
                ExportPhase::BeforeApply
            },
            run_id,
            file_name: file_name.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "id")]
pub(crate) enum Subject {
    Snippet(SnippetId),
    Folder(folders::Id),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Analysis {
    pub(crate) summary: String,
    pub(crate) improvements: Vec<Finding>,
    pub(crate) dependencies: Vec<Dependency>,
    pub(crate) project_files: Vec<ProjectFileNote>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Finding {
    pub(crate) id: String,
    pub(crate) category: String,
    pub(crate) severity: String,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) recommendation: String,
    pub(crate) line: Option<usize>,
    pub(crate) file: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Dependency {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) purpose: String,
    pub(crate) suggestion: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProjectFileNote {
    pub(crate) path: String,
    pub(crate) purpose: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Source {
    pub(crate) sha256: String,
    pub(crate) language: String,
    pub(crate) report_language_code: String,
    pub(crate) code_text_language_code: String,
    // The store attaches this from the blob identified by sha256.
    #[serde(skip)]
    pub(crate) content: Option<Arc<str>>,
    pub(crate) content_truncated: bool,
    pub(crate) line_count: usize,
    pub(crate) snippet_name: String,
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("sha256", &self.sha256)
            .field("line_count", &self.line_count)
            .field("content_truncated", &self.content_truncated)
            .finish_non_exhaustive()
    }
}

impl Source {
    /// Hashes the exact analysed UTF-8 bytes even when the body exceeds retention limits.
    pub(crate) fn new(
        content: &str,
        language: &str,
        report_language_code: &str,
        code_text_language_code: &str,
        snippet_name: &str,
    ) -> io::Result<Self> {
        let content_truncated = content.len() > DEFAULT_CONTENT_BYTES;
        let line_count = content.bytes().filter(|&byte| byte == b'\n').count()
            + usize::from(!content.is_empty() && !content.ends_with('\n'));
        Ok(Self {
            sha256: content_hash(content)?,
            language: language.into(),
            report_language_code: report_language_code.into(),
            code_text_language_code: code_text_language_code.into(),
            content: (!content_truncated).then(|| Arc::from(content)),
            content_truncated,
            line_count,
            snippet_name: snippet_name.into(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Provenance {
    pub(crate) profile_id: ProfileRef,
    pub(crate) profile_name: String,
    pub(crate) model: String,
    pub(crate) skill_ids: Vec<String>,
    pub(crate) skill_names: Vec<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_knowledge_citations"
    )]
    pub(crate) knowledge_citations: Vec<KnowledgeCitation>,
    pub(crate) additional_instructions: String,
    pub(crate) usage: Usage,
    pub(crate) duration_millis: u64,
}

impl Default for Provenance {
    fn default() -> Self {
        Self {
            profile_id: ProfileRef::Numeric(0),
            profile_name: String::new(),
            model: String::new(),
            skill_ids: Vec::new(),
            skill_names: Vec::new(),
            knowledge_citations: Vec::new(),
            additional_instructions: String::new(),
            usage: Usage::default(),
            duration_millis: 0,
        }
    }
}

fn deserialize_knowledge_citations<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<KnowledgeCitation>, D::Error> {
    let citations = Vec::deserialize(deserializer)?;
    if !KnowledgeCitation::valid_list(&citations) {
        return Err(serde::de::Error::custom("Invalid knowledge citations"));
    }
    Ok(citations)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Usage {
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: u64,
    pub(crate) cached_prompt_tokens: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Purpose {
    #[default]
    Analysis,
    Rerun,
    Verify,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    pub(crate) id: SnippetId,
    pub(crate) purpose: Purpose,
    pub(crate) previous_record_id: Option<SnippetId>,
    pub(crate) analyzed_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) pinned: bool,
    pub(crate) source: Source,
    pub(crate) provenance: Provenance,
    pub(crate) analysis: Analysis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) diagram: Option<AnalysisDiagram>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) apply_runs: Vec<ApplyRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) apply_recovery: Option<ApplyRecovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modularization: Option<Box<modularization::Job>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verification: Option<Verification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) exports: Vec<ExportEntry>,
}

impl Record {
    pub(crate) fn new(
        purpose: Purpose,
        previous_record_id: Option<SnippetId>,
        source: Source,
        provenance: Provenance,
        analysis: Analysis,
    ) -> io::Result<Self> {
        if !KnowledgeCitation::valid_list(&provenance.knowledge_citations) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid knowledge citations",
            ));
        }
        let now = now_millis();
        Ok(Self {
            id: SnippetId::new()?,
            purpose,
            previous_record_id,
            analyzed_at: now,
            updated_at: now,
            pinned: false,
            source,
            provenance,
            analysis,
            diagram: None,
            apply_runs: Vec::new(),
            apply_recovery: None,
            modularization: None,
            verification: None,
            exports: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct History {
    pub(crate) subject: Subject,
    pub(crate) records: Vec<Record>,
    pub(crate) revision: u64,
    pub(crate) updated_at: u64,
}

impl History {
    pub(super) fn empty(subject: Subject) -> Self {
        Self {
            subject,
            records: Vec::new(),
            revision: 0,
            updated_at: 0,
        }
    }

    pub(crate) fn current(&self) -> Option<&Record> {
        self.records.first()
    }
}

pub(crate) fn content_hash(text: &str) -> io::Result<String> {
    let digest = HashAlgorithm::Sha256
        .digest(text.as_bytes())
        .map_err(|_| io::Error::other("Could not hash the snippet analysis source"))?;
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::UNIX_EPOCH
        .elapsed()
        .map(|elapsed| elapsed.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}
