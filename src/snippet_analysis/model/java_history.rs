//! Read-only Java evidence preserved alongside the native analysis report.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::session_journal::export_output::valid_component;
use crate::snippets::SnippetId;

use super::{ExportEntry, ExportPhase, MAX_CONTENT_BYTES, Usage, content_hash};

const MAX_ENTRIES: usize = 128;
const MAX_ITEMS: usize = 256;
const MAX_TEXT: usize = 64 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Evidence {
    pub(crate) profile_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) selection: Option<Selection>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) apply_runs: Vec<ApplyRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verification: Option<Verification>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) after_apply_exports: Vec<ExportEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) modularization: Option<Modularization>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) historical_diagram: Option<Diagram>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) attachments: BTreeMap<String, Arc<str>>,
}

impl Evidence {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let mut run_ids = HashSet::new();
        let good = text(&self.profile_id, 256)
            && self.selection.as_ref().is_none_or(Selection::valid)
            && self.apply_runs.len() <= MAX_ENTRIES
            && self
                .apply_runs
                .iter()
                .all(|run| run_ids.insert(run.id) && run.valid())
            && self.verification.as_ref().is_none_or(Verification::valid)
            && self.after_apply_exports.len() <= MAX_ENTRIES
            && self.after_apply_exports.iter().all(|entry| {
                entry.exported_at != 0
                    && entry.phase == ExportPhase::AfterApply
                    && entry.run_id.is_some()
                    && valid_component(&entry.file_name)
            })
            && self
                .modularization
                .as_ref()
                .is_none_or(Modularization::valid)
            && self.historical_diagram.as_ref().is_none_or(Diagram::valid);
        if !good {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java analysis evidence",
            ));
        }
        let referenced: HashSet<_> = self
            .apply_runs
            .iter()
            .flat_map(ApplyRun::snapshots)
            .collect();
        for (key, value) in &self.attachments {
            if !referenced.contains(key.as_str())
                || value.len() > MAX_CONTENT_BYTES
                || content_hash(value)? != *key
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid Java apply attachment",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Selection {
    pub(crate) improvement_ids: Vec<String>,
    pub(crate) dependency_ids: Vec<String>,
    pub(crate) hardening: Vec<String>,
    pub(crate) input_hardening_enabled: bool,
    pub(crate) input_hardening_options: Vec<String>,
    pub(crate) input_hardening_max_file_size_bytes: u64,
    pub(crate) header_snippet_id: Option<String>,
    pub(crate) migration_target_language: Option<String>,
    pub(crate) migration_target_host_format: Option<String>,
    pub(crate) code_text_language_code: Option<String>,
    pub(crate) updated_at: u64,
}

impl Selection {
    fn valid(&self) -> bool {
        [
            &self.improvement_ids,
            &self.dependency_ids,
            &self.hardening,
            &self.input_hardening_options,
        ]
        .into_iter()
        .all(|items| identifiers(items))
            && [
                &self.header_snippet_id,
                &self.migration_target_language,
                &self.migration_target_host_format,
                &self.code_text_language_code,
            ]
            .into_iter()
            .flatten()
            .all(|value| text(value, 256))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum Outcome {
    Running,
    PendingReview,
    Accepted,
    Rejected,
    Failed,
    Cancelled,
    #[default]
    Interrupted,
}

impl Outcome {
    pub(crate) fn label(self, saved_to_snippet_at: u64) -> &'static str {
        match self {
            Self::Running | Self::Interrupted => "Interrupted",
            Self::PendingReview => "Pending review",
            Self::Accepted if saved_to_snippet_at != 0 => "Accepted and saved",
            Self::Accepted => "Accepted in editor",
            Self::Rejected => "Rejected",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ApplyRun {
    pub(crate) id: SnippetId,
    pub(crate) outcome: Outcome,
    pub(crate) partial: bool,
    pub(crate) started_at: u64,
    pub(crate) finished_at: u64,
    pub(crate) decided_at: u64,
    pub(crate) saved_to_snippet_at: u64,
    pub(crate) base_sha256: String,
    pub(crate) result_sha256: String,
    pub(crate) accepted_content_sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) checkpoint_sha256: String,
    pub(crate) summary: String,
    pub(crate) changes: Vec<Change>,
    pub(crate) implemented_requirements: Vec<String>,
    pub(crate) applied_finding_ids: Vec<String>,
    pub(crate) usage: Usage,
    pub(crate) elapsed_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<ApplyDetails>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ApplyDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) options: Option<ApplyOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) work_items: Vec<WorkItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) completed_work_item_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) checkpoint_progress: Option<CheckpointProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) progress: Option<RunProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provenance: Option<RunProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failure_key: Option<String>,
}

impl ApplyRun {
    pub(crate) fn snapshots(&self) -> [&str; 4] {
        [
            &self.base_sha256,
            &self.result_sha256,
            &self.accepted_content_sha256,
            &self.checkpoint_sha256,
        ]
    }

    fn valid(&self) -> bool {
        self.snapshots()
            .into_iter()
            .all(|value| value.is_empty() || hash(value))
            && text(&self.summary, MAX_TEXT)
            && self.changes.len() <= MAX_ITEMS
            && self.changes.iter().all(Change::valid)
            && identifiers(&self.implemented_requirements)
            && identifiers(&self.applied_finding_ids)
            && self.details.as_ref().is_none_or(ApplyDetails::valid)
    }
}

impl ApplyDetails {
    pub(in crate::snippet_analysis) fn valid(&self) -> bool {
        self.options.as_ref().is_none_or(ApplyOptions::valid)
            && self.work_items.len() <= MAX_ITEMS
            && self.work_items.iter().all(WorkItem::valid)
            && identifiers(&self.completed_work_item_ids)
            && self
                .checkpoint_progress
                .as_ref()
                .is_none_or(CheckpointProgress::valid)
            && self.progress.as_ref().is_none_or(|progress| {
                progress
                    .status_key
                    .as_ref()
                    .is_none_or(|key| text(key, 256))
            })
            && self.provenance.as_ref().is_none_or(RunProvenance::valid)
            && self.failure_key.as_ref().is_none_or(|key| text(key, 256))
    }
}

/// Historical request metadata. No prompt text, header content or live profile reference.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ApplyOptions {
    pub(crate) snippet_language: String,
    pub(crate) code_text_language_code: Option<String>,
    pub(crate) improvement_ids: Vec<String>,
    pub(crate) dependency_ids: Vec<String>,
    pub(crate) hardening_options: Vec<String>,
    pub(crate) input_hardening_options: Vec<String>,
    pub(crate) migration_target_language: Option<String>,
    pub(crate) migration_target_host_format: Option<String>,
    pub(crate) migration_label: Option<String>,
    pub(crate) header_snippet_id: Option<String>,
    pub(crate) header_name: Option<String>,
    pub(crate) ai_profile_id: Option<String>,
}

impl ApplyOptions {
    fn valid(&self) -> bool {
        text(&self.snippet_language, 256)
            && [
                &self.improvement_ids,
                &self.dependency_ids,
                &self.hardening_options,
                &self.input_hardening_options,
            ]
            .into_iter()
            .all(|items| identifiers(items))
            && [
                &self.code_text_language_code,
                &self.migration_target_language,
                &self.migration_target_host_format,
                &self.header_snippet_id,
                &self.ai_profile_id,
            ]
            .into_iter()
            .flatten()
            .all(|value| text(value, 256))
            && [&self.migration_label, &self.header_name]
                .into_iter()
                .flatten()
                .all(|value| text(value, 4096))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct WorkItem {
    // Java uses the stage index from the historical checklist.
    pub(crate) stage: i32,
    pub(crate) phase: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) category: String,
    pub(crate) severity: String,
    pub(crate) state: String,
}

impl WorkItem {
    fn valid(&self) -> bool {
        text(&self.label, 4096)
            && [
                &self.phase,
                &self.id,
                &self.category,
                &self.severity,
                &self.state,
            ]
            .into_iter()
            .all(|value| text(value, 256))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CheckpointProgress {
    pub(crate) completed_stages: u32,
    pub(crate) total_stages: u32,
    pub(crate) summaries: Vec<String>,
    pub(crate) changes: Vec<Change>,
    pub(crate) completed_requirement_ids: Vec<String>,
    pub(crate) usage: Usage,
}

impl CheckpointProgress {
    fn valid(&self) -> bool {
        self.summaries.len() <= MAX_ITEMS
            && self.summaries.iter().all(|summary| text(summary, MAX_TEXT))
            && self.changes.len() <= MAX_ITEMS
            && self.changes.iter().all(Change::valid)
            && identifiers(&self.completed_requirement_ids)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RunProgress {
    pub(crate) retries: u32,
    pub(crate) completed_items: u32,
    pub(crate) total_items: u32,
    pub(crate) status_key: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RunProvenance {
    pub(crate) profile_id: String,
    pub(crate) profile_name: String,
    pub(crate) model: String,
    pub(crate) skill_ids: Vec<String>,
    pub(crate) skill_names: Vec<String>,
    pub(crate) usage: Usage,
    pub(crate) duration_millis: u64,
}

impl RunProvenance {
    fn valid(&self) -> bool {
        text(&self.profile_id, 256)
            && text(&self.profile_name, 4096)
            && text(&self.model, 4096)
            && [&self.skill_ids, &self.skill_names]
                .into_iter()
                .all(|items| {
                    items.len() <= MAX_ITEMS && items.iter().all(|value| text(value, 4096))
                })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Change {
    pub(crate) finding: String,
    pub(crate) anchor: String,
    pub(crate) reason: String,
}

impl Change {
    fn valid(&self) -> bool {
        text(&self.finding, 256) && text(&self.anchor, 4096) && text(&self.reason, MAX_TEXT)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Verification {
    pub(crate) previous_record_id: SnippetId,
    pub(crate) resolved_previous_ids: Vec<String>,
    pub(crate) persisting_current_to_previous: BTreeMap<String, String>,
    pub(crate) new_ids: Vec<String>,
}

impl Verification {
    fn valid(&self) -> bool {
        identifiers(&self.resolved_previous_ids)
            && identifiers(&self.new_ids)
            && self.persisting_current_to_previous.len() <= MAX_ITEMS
            && self
                .persisting_current_to_previous
                .iter()
                .all(|(id, prior)| text(id, 256) && text(prior, 256))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Modularization {
    pub(crate) recommended: bool,
    pub(crate) rationale: String,
    pub(crate) files: Vec<Module>,
}

impl Modularization {
    fn valid(&self) -> bool {
        text(&self.rationale, MAX_TEXT)
            && self.files.len() <= MAX_ITEMS
            && self.files.iter().all(|file| {
                text(&file.path, 1024) && text(&file.purpose, 4096) && identifiers(&file.symbols)
            })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Module {
    pub(crate) path: String,
    pub(crate) purpose: String,
    pub(crate) symbols: Vec<String>,
    pub(crate) executable: bool,
    pub(crate) entry_point: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Diagram {
    pub(crate) type_id: String,
    pub(crate) mermaid: String,
    pub(crate) code_references: Vec<CodeReference>,
    pub(crate) notice: String,
    pub(crate) fallback: bool,
    pub(crate) source_sha256: String,
    pub(crate) profile_id: String,
    pub(crate) generated_at: u64,
}

impl Diagram {
    fn valid(&self) -> bool {
        text(&self.type_id, 128)
            && text(&self.mermaid, 512 * 1024)
            && text(&self.notice, 4096)
            && text(&self.profile_id, 256)
            && (self.source_sha256.is_empty() || hash(&self.source_sha256))
            && self.code_references.len() <= MAX_ITEMS
            && self
                .code_references
                .iter()
                .all(|reference| text(&reference.node_id, 256) && text(&reference.label, 4096))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CodeReference {
    pub(crate) node_id: String,
    pub(crate) label: String,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
}

pub(in crate::snippet_analysis) fn text(value: &str, limit: usize) -> bool {
    value.len() <= limit
        && value.char_indices().all(|(index, character)| {
            crate::session_journal::is_display_character(character)
                || (character == '\r' && value.as_bytes().get(index + 1) == Some(&b'\n'))
        })
}

fn identifiers(items: &[String]) -> bool {
    items.len() <= MAX_ITEMS && items.iter().all(|id| text(id, 256))
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
