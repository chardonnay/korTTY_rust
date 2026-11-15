//! Projects Java analysis files into private, immutable native histories.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::sync::Arc;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::clipboard_policy::ClipboardPolicy;
use crate::session_journal::export_output::valid_component;
use crate::snippet_analysis::diagram::AnalysisDiagram;
use crate::snippet_analysis::model::{
    Analysis, Dependency, ExportPhase, Finding, ProjectFileNote, Provenance, Source, Usage,
    java_history as legacy,
};
use crate::snippets::folders;

use super::{
    ApplyStatus, BoundedWriter, Document, ExportEntry, History, MAX_CONTENT_BYTES,
    MAX_EXPORT_ENTRIES, MAX_FILE_BYTES, MAX_RECORDS, Purpose, Record, SnippetId, Subject,
    content_hash, file_name, invalid, valid_export, valid_hash,
};

pub(crate) const MAX_BACKUP_BYTES: usize = MAX_FILE_BYTES;
const MEMBER_DIRECTORY: &str = "snippet-analyses/";
const MAX_FINDINGS: usize = 256;

pub(crate) struct Projection {
    pub(crate) filename: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) omissions: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JavaHistory {
    schema_version: u32,
    snippet_id: String,
    revision: u64,
    updated_at: u64,
    #[serde(default)]
    records: Vec<JavaRecord>,
    #[serde(default, deserialize_with = "read_blobs")]
    blobs: BTreeMap<String, Arc<str>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JavaRecord {
    id: String,
    snippet_id: String,
    #[serde(default)]
    purpose: Option<JavaPurpose>,
    previous_record_id: Option<String>,
    analyzed_at: u64,
    updated_at: u64,
    #[serde(default)]
    pinned: bool,
    source: JavaSource,
    #[serde(default)]
    provenance: JavaProvenance,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    improvements: Vec<Finding>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
    project_files: Option<Vec<ProjectFileNote>>,
    diagram: Option<legacy::Diagram>,
    selection: Option<legacy::Selection>,
    #[serde(default)]
    apply_runs: Vec<JavaApply>,
    verification: Option<JavaVerification>,
    #[serde(default)]
    exports: Vec<JavaExport>,
    modularization: Option<legacy::Modularization>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum JavaPurpose {
    Analysis,
    Rerun,
    Verify,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JavaSource {
    sha256: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    report_language_code: String,
    #[serde(default)]
    code_text_language_code: String,
    content: Option<String>,
    #[serde(default, rename = "contentTruncated")]
    _content_truncated: bool,
    line_count: usize,
    #[serde(default)]
    snippet_name: String,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct JavaProvenance {
    profile_id: String,
    profile_name: String,
    model: String,
    skill_ids: Vec<String>,
    skill_names: Vec<String>,
    additional_instructions: String,
    usage: Usage,
    duration_millis: u64,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavaApply {
    id: String,
    outcome: legacy::Outcome,
    partial: bool,
    started_at: u64,
    finished_at: u64,
    decided_at: u64,
    saved_to_snippet_at: u64,
    request: JavaRequest,
    items: Vec<legacy::WorkItem>,
    checkpoint: Option<JavaCheckpoint>,
    result_sha256: String,
    result_content: Option<String>,
    accepted_content_sha256: String,
    accepted_content: Option<String>,
    summary: String,
    changes: Vec<legacy::Change>,
    implemented_requirements: Vec<String>,
    completed_work_item_ids: Vec<String>,
    applied_finding_ids: Vec<String>,
    stats: JavaStats,
    provenance: Option<JavaProvenance>,
    failure_key: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavaRequest {
    base_sha256: String,
    base_content: Option<String>,
    snippet_language: String,
    code_text_language_code: Option<String>,
    improvement_ids: Vec<String>,
    dependency_ids: Vec<String>,
    hardening_options: Vec<String>,
    input_hardening_options: Vec<String>,
    migration_target_language: Option<String>,
    migration_target_host_format: Option<String>,
    migration_label: Option<String>,
    header_snippet_id: Option<String>,
    header_name: Option<String>,
    ai_profile_id: Option<String>,
    additional_instructions: String,
    classic_hardening_instructions: String,
    input_hardening_instructions: String,
    header_text: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavaCheckpoint {
    content: Option<String>,
    completed_stages: u32,
    total_stages: u32,
    summaries: Vec<String>,
    changes: Vec<legacy::Change>,
    completed_requirement_ids: Vec<String>,
    usage: Usage,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavaStats {
    usage: Usage,
    elapsed_seconds: u64,
    retries: u32,
    completed_items: u32,
    total_items: u32,
    status_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JavaVerification {
    previous_record_id: String,
    #[serde(default)]
    resolved_previous_ids: Vec<String>,
    #[serde(default)]
    persisting_current_to_previous: BTreeMap<String, String>,
    #[serde(default)]
    new_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JavaExport {
    exported_at: u64,
    format: super::super::model::ReportFormat,
    phase: ExportPhase,
    run_id: Option<String>,
    file_name: String,
}

/// Accepts only members under Java's analysis directory with a matching subject and exact blobs.
pub(crate) fn project(member: &str, bytes: &[u8]) -> io::Result<Projection> {
    let id = member
        .strip_prefix(MEMBER_DIRECTORY)
        .and_then(|name| name.strip_suffix(".json"))
        .ok_or_else(|| invalid("Invalid Java analysis member path"))?;
    let subject = if let Some(folder) = id.strip_prefix("folder-") {
        Subject::Folder(folders::Id::from_java_uuid(folder)?)
    } else {
        Subject::Snippet(SnippetId::from_java_uuid(id)?)
    };
    let java: JavaHistory = decode_json(bytes)?;
    if java.schema_version != 1
        || java.snippet_id != id
        || java.records.is_empty()
        || java.records.len() > MAX_RECORDS
    {
        return Err(invalid(
            "Invalid Java analysis schema, subject or record count",
        ));
    }
    let mut omissions = 0;
    let mut ids = HashSet::new();
    let mut records = Vec::with_capacity(java.records.len());
    for record in java.records {
        let record_id = SnippetId::from_java_uuid(&record.id)?;
        if record.snippet_id != id || !ids.insert(record_id) {
            return Err(invalid("Invalid Java analysis record identity"));
        }
        records.push(record.project(subject, record_id, &java.blobs, &mut omissions)?);
    }
    let retained = records
        .iter()
        .filter(|record| record.source.content.is_some())
        .count();
    let attachments = attachment_count(&records);
    let history = History {
        subject,
        records,
        revision: java.revision,
        updated_at: java.updated_at,
    };
    let document = Document::capture(history, MAX_CONTENT_BYTES)?;
    omissions += retained
        - document
            .history
            .records
            .iter()
            .filter(|record| record.source.content.is_some())
            .count();
    omissions += attachments - attachment_count(&document.history.records);
    let filename = file_name(subject)?;
    let bytes = encode(&document)?;
    binding(&filename, &bytes)?;
    Ok(Projection {
        filename,
        bytes,
        omissions,
    })
}

fn attachment_count(records: &[Record]) -> usize {
    records
        .iter()
        .filter_map(|record| record.java_history.as_ref())
        .map(|evidence| evidence.attachments.len())
        .sum()
}

/// Validates the exact native filename, record identities and source contents.
pub(crate) fn binding(filename: &str, bytes: &[u8]) -> io::Result<Subject> {
    Ok(decode(filename, bytes)?.subject)
}

/// Retains history and removes machine-local pending recovery and apply authority.
pub(crate) fn portable_backup(filename: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
    portable_backup_with_limit(filename, bytes, MAX_CONTENT_BYTES)
}

pub(crate) fn content_limit(policy: &ClipboardPolicy) -> usize {
    policy
        .snippets
        .analysis_max_stored_content_bytes
        .unwrap_or(MAX_CONTENT_BYTES)
        .min(MAX_CONTENT_BYTES)
}

pub(crate) fn portable_backup_with_limit(
    filename: &str,
    bytes: &[u8],
    limit: usize,
) -> io::Result<Vec<u8>> {
    let mut history = decode(filename, bytes)?;
    for record in &mut history.records {
        record.apply_recovery = None;
        record
            .apply_runs
            .retain(|run| run.status != ApplyStatus::Proposed);
        if record
            .modularization
            .as_ref()
            .is_some_and(|job| !job.published)
        {
            record.modularization = None;
        }
        let mut exports = std::mem::take(&mut record.exports);
        exports.retain(|export| valid_export(export, record));
        record.exports = exports;
    }
    let document = Document::capture(history, limit.min(MAX_CONTENT_BYTES))?;
    let bytes = encode(&document)?;
    binding(filename, &bytes)?;
    Ok(bytes)
}

pub(crate) fn fits_retention(filename: &str, bytes: &[u8], limit: usize) -> io::Result<bool> {
    let document = decode_document(filename, bytes)?;
    Ok(document.fits_retention(limit.min(MAX_CONTENT_BYTES)))
}

fn decode(filename: &str, bytes: &[u8]) -> io::Result<History> {
    let document = decode_document(filename, bytes)?;
    let subject = document.history.subject;
    document.decode(subject)
}

fn decode_document(filename: &str, bytes: &[u8]) -> io::Result<Document> {
    let document: Document = decode_json(bytes)?;
    let subject = document.history.subject;
    if filename != file_name(subject)? {
        return Err(invalid("The analysis filename does not match its subject"));
    }
    Ok(document)
}

fn decode_json<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> io::Result<T> {
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err(invalid("The analysis history exceeds its storage limit"));
    }
    serde_json::from_slice(bytes).map_err(|_| invalid("Invalid or unsupported analysis history"))
}

fn encode(document: &Document) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    serde_json::to_writer(
        BoundedWriter {
            output: &mut bytes,
            remaining: MAX_BACKUP_BYTES,
        },
        document,
    )
    .map_err(io::Error::other)?;
    Ok(bytes)
}

fn read_blobs<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Arc<str>>, D::Error> {
    struct Blobs;
    impl<'de> Visitor<'de> for Blobs {
        type Value = BTreeMap<String, Arc<str>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a SHA-256 source map")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut blobs = BTreeMap::new();
            while let Some((hash, blob)) = map.next_entry::<String, Arc<str>>()? {
                if !valid_hash(&hash)
                    || blob.len() > MAX_CONTENT_BYTES
                    || content_hash(&blob).as_deref().ok() != Some(&hash)
                    || blobs.insert(hash, blob).is_some()
                {
                    return Err(de::Error::custom(
                        "Invalid or duplicate Java analysis source blob",
                    ));
                }
            }
            Ok(blobs)
        }
    }
    deserializer.deserialize_map(Blobs)
}

impl JavaRecord {
    fn project(
        self,
        subject: Subject,
        id: SnippetId,
        blobs: &BTreeMap<String, Arc<str>>,
        omissions: &mut usize,
    ) -> io::Result<Record> {
        let source = self.source.project(blobs)?;
        let analysis = Analysis {
            summary: self.summary,
            improvements: self.improvements,
            dependencies: self.dependencies,
            project_files: self.project_files.unwrap_or_default(),
        };
        validate_analysis(&analysis)?;
        let purpose = match self.purpose {
            None | Some(JavaPurpose::Analysis) => Purpose::Analysis,
            Some(JavaPurpose::Rerun) => Purpose::Rerun,
            Some(JavaPurpose::Verify) => Purpose::Verify,
        };
        let previous_record_id = self
            .previous_record_id
            .as_deref()
            .map(SnippetId::from_java_uuid)
            .transpose()?;
        let (profile_id, provenance) = self.provenance.project()?;
        let diagram = self
            .diagram
            .as_ref()
            .and_then(|diagram| AnalysisDiagram::from_java(diagram, &source).ok());
        let mut attachments = BTreeMap::new();
        let apply_runs = self
            .apply_runs
            .into_iter()
            .map(|run| run.project(blobs, &mut attachments, omissions))
            .collect::<io::Result<_>>()?;
        let mut java_history = legacy::Evidence {
            profile_id,
            selection: self.selection,
            apply_runs,
            verification: self
                .verification
                .map(JavaVerification::project)
                .transpose()?,
            modularization: self.modularization,
            historical_diagram: self.diagram,
            after_apply_exports: Vec::new(),
            attachments,
        };
        let mut exports = Vec::new();
        for entry in self.exports {
            let entry = entry.project()?;
            if entry.phase == ExportPhase::BeforeApply
                && matches!(subject, Subject::Snippet(_))
                && exports.len() < MAX_EXPORT_ENTRIES
            {
                exports.push(entry);
            } else if entry.phase == ExportPhase::AfterApply {
                java_history.after_apply_exports.push(entry);
            } else {
                *omissions += 1;
            }
        }
        java_history.validate()?;
        Ok(Record {
            id,
            purpose,
            previous_record_id,
            analyzed_at: self.analyzed_at,
            updated_at: self.updated_at,
            pinned: self.pinned,
            source,
            provenance,
            analysis,
            diagram,
            apply_runs: Vec::new(),
            apply_recovery: None,
            modularization: None,
            verification: None,
            exports,
            java_history: Some(java_history),
        })
    }
}

impl JavaSource {
    fn project(self, blobs: &BTreeMap<String, Arc<str>>) -> io::Result<Source> {
        let content = self
            .content
            .as_deref()
            .map(|reference| {
                if reference != self.sha256 {
                    return Err(invalid(
                        "The Java analysis source points to a different hash",
                    ));
                }
                let content = blobs
                    .get(reference)
                    .ok_or_else(|| invalid("The Java analysis source blob is missing"))?;
                let lines = content.bytes().filter(|&value| value == b'\n').count()
                    + usize::from(!content.is_empty() && !content.ends_with('\n'));
                if lines != self.line_count {
                    return Err(invalid("The Java analysis source line count differs"));
                }
                Ok(Arc::clone(content))
            })
            .transpose()?;
        if !valid_hash(&self.sha256)
            || !legacy::text(&self.language, 256)
            || !legacy::text(&self.report_language_code, 256)
            || !legacy::text(&self.code_text_language_code, 256)
            || !legacy::text(&self.snippet_name, 1024)
        {
            return Err(invalid("Invalid Java analysis source metadata"));
        }
        Ok(Source {
            sha256: self.sha256,
            language: self.language,
            report_language_code: self.report_language_code,
            code_text_language_code: self.code_text_language_code,
            content_truncated: content.is_none(),
            content,
            line_count: self.line_count,
            snippet_name: self.snippet_name,
        })
    }
}

impl JavaProvenance {
    fn project(self) -> io::Result<(String, Provenance)> {
        if !legacy::text(&self.profile_name, 4096)
            || !legacy::text(&self.model, 4096)
            || !legacy::text(&self.additional_instructions, 64 * 1024)
            || [&self.skill_ids, &self.skill_names]
                .into_iter()
                .any(|items| {
                    items.len() > MAX_FINDINGS
                        || items.iter().any(|value| !legacy::text(value, 4096))
                })
        {
            return Err(invalid("Invalid Java analysis provenance"));
        }
        Ok((
            self.profile_id,
            Provenance {
                profile_name: self.profile_name,
                model: self.model,
                skill_ids: self.skill_ids,
                skill_names: self.skill_names,
                additional_instructions: self.additional_instructions,
                usage: self.usage,
                duration_millis: self.duration_millis,
                ..Provenance::default()
            },
        ))
    }
}

impl JavaApply {
    fn project(
        self,
        blobs: &BTreeMap<String, Arc<str>>,
        attachments: &mut BTreeMap<String, Arc<str>>,
        omissions: &mut usize,
    ) -> io::Result<legacy::ApplyRun> {
        let checkpoint_sha256 = self
            .checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.content.as_deref())
            .unwrap_or_default()
            .to_owned();
        for (key, expected) in [
            (
                self.request.base_content.as_deref(),
                Some(self.request.base_sha256.as_str()),
            ),
            (
                self.result_content.as_deref(),
                Some(self.result_sha256.as_str()),
            ),
            (
                self.accepted_content.as_deref(),
                Some(self.accepted_content_sha256.as_str()),
            ),
            (
                (!checkpoint_sha256.is_empty()).then_some(checkpoint_sha256.as_str()),
                None,
            ),
        ] {
            if let Some(key) = key {
                if expected.is_some_and(|expected| key != expected) {
                    return Err(invalid(
                        "A Java apply snapshot references a missing or different blob",
                    ));
                }
                let content = blobs
                    .get(key)
                    .ok_or_else(|| invalid("A Java apply snapshot references a missing blob"))?;
                attachments
                    .entry(key.to_owned())
                    .or_insert_with(|| Arc::clone(content));
            }
        }
        if [
            &self.request.additional_instructions,
            &self.request.classic_hardening_instructions,
            &self.request.input_hardening_instructions,
            &self.request.header_text,
        ]
        .into_iter()
        .any(|value| !value.is_empty())
            || self
                .provenance
                .as_ref()
                .is_some_and(|provenance| !provenance.additional_instructions.is_empty())
        {
            *omissions += 1;
        }
        let options = legacy::ApplyOptions {
            snippet_language: self.request.snippet_language,
            code_text_language_code: self.request.code_text_language_code,
            improvement_ids: self.request.improvement_ids,
            dependency_ids: self.request.dependency_ids,
            hardening_options: self.request.hardening_options,
            input_hardening_options: self.request.input_hardening_options,
            migration_target_language: self.request.migration_target_language,
            migration_target_host_format: self.request.migration_target_host_format,
            migration_label: self.request.migration_label,
            header_snippet_id: self.request.header_snippet_id,
            header_name: self.request.header_name,
            ai_profile_id: self.request.ai_profile_id,
        };
        let progress = legacy::RunProgress {
            retries: self.stats.retries,
            completed_items: self.stats.completed_items,
            total_items: self.stats.total_items,
            status_key: self.stats.status_key,
        };
        let details = legacy::ApplyDetails {
            options: (options != legacy::ApplyOptions::default()).then_some(options),
            work_items: self.items,
            completed_work_item_ids: self.completed_work_item_ids,
            checkpoint_progress: self
                .checkpoint
                .map(|checkpoint| legacy::CheckpointProgress {
                    completed_stages: checkpoint.completed_stages,
                    total_stages: checkpoint.total_stages,
                    summaries: checkpoint.summaries,
                    changes: checkpoint.changes,
                    completed_requirement_ids: checkpoint.completed_requirement_ids,
                    usage: checkpoint.usage,
                }),
            progress: (progress != legacy::RunProgress::default()).then_some(progress),
            provenance: self
                .provenance
                .map(|provenance| legacy::RunProvenance {
                    profile_id: provenance.profile_id,
                    profile_name: provenance.profile_name,
                    model: provenance.model,
                    skill_ids: provenance.skill_ids,
                    skill_names: provenance.skill_names,
                    usage: provenance.usage,
                    duration_millis: provenance.duration_millis,
                })
                .filter(|provenance| *provenance != legacy::RunProvenance::default()),
            failure_key: self.failure_key,
        };
        let details = if !details.valid() {
            *omissions += 1;
            None
        } else {
            (details != legacy::ApplyDetails::default()).then_some(details)
        };
        Ok(legacy::ApplyRun {
            id: SnippetId::from_java_uuid(&self.id)?,
            outcome: self.outcome,
            partial: self.partial,
            started_at: self.started_at,
            finished_at: self.finished_at,
            decided_at: self.decided_at,
            saved_to_snippet_at: self.saved_to_snippet_at,
            base_sha256: self.request.base_sha256,
            result_sha256: self.result_sha256,
            accepted_content_sha256: self.accepted_content_sha256,
            checkpoint_sha256,
            summary: self.summary,
            changes: self.changes,
            implemented_requirements: self.implemented_requirements,
            applied_finding_ids: self.applied_finding_ids,
            usage: self.stats.usage,
            elapsed_seconds: self.stats.elapsed_seconds,
            details,
        })
    }
}

impl JavaVerification {
    fn project(self) -> io::Result<legacy::Verification> {
        Ok(legacy::Verification {
            previous_record_id: SnippetId::from_java_uuid(&self.previous_record_id)?,
            resolved_previous_ids: self.resolved_previous_ids,
            persisting_current_to_previous: self.persisting_current_to_previous,
            new_ids: self.new_ids,
        })
    }
}

impl JavaExport {
    fn project(self) -> io::Result<ExportEntry> {
        let run_id = self
            .run_id
            .as_deref()
            .map(SnippetId::from_java_uuid)
            .transpose()?;
        if self.exported_at == 0
            || !valid_component(&self.file_name)
            || (self.phase == ExportPhase::AfterApply) != run_id.is_some()
        {
            return Err(invalid("Invalid Java analysis export receipt"));
        }
        Ok(ExportEntry {
            exported_at: self.exported_at,
            format: self.format,
            phase: self.phase,
            run_id,
            file_name: self.file_name,
        })
    }
}

fn validate_analysis(analysis: &Analysis) -> io::Result<()> {
    let mut ids = HashSet::new();
    let valid = legacy::text(&analysis.summary, 256 * 1024)
        && analysis.improvements.len() + analysis.dependencies.len() <= MAX_FINDINGS
        && analysis.project_files.len() <= MAX_FINDINGS
        && analysis.improvements.iter().all(|finding| {
            !finding.id.is_empty()
                && legacy::text(&finding.id, 256)
                && ids.insert(&finding.id)
                && legacy::text(&finding.category, 256)
                && legacy::text(&finding.severity, 256)
                && legacy::text(&finding.title, 4096)
                && legacy::text(&finding.detail, 64 * 1024)
                && legacy::text(&finding.recommendation, 64 * 1024)
        })
        && analysis.dependencies.iter().all(|dependency| {
            !dependency.id.is_empty()
                && legacy::text(&dependency.id, 256)
                && ids.insert(&dependency.id)
                && [
                    &dependency.name,
                    &dependency.kind,
                    &dependency.purpose,
                    &dependency.suggestion,
                ]
                .into_iter()
                .all(|value| legacy::text(value, 64 * 1024))
        })
        && analysis
            .project_files
            .iter()
            .all(|file| legacy::text(&file.path, 1024) && legacy::text(&file.purpose, 4096));
    if !valid {
        return Err(invalid("Invalid Java analysis report"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const SUBJECT: &str = "8eb2f96f-f2a0-4fe5-94e3-6239e7d7bd82";
    const RECORD: &str = "ec212de4-b613-4e82-82b8-f2a33ac8b875";
    const RUN: &str = "60d05355-79b4-43be-8386-f07d6a2af9dd";

    fn fixture(subject: &str) -> Value {
        let source = "printf 'Wert α'\r\n";
        let result = "printf 'Wert β'\r\n";
        let sha = content_hash(source).unwrap();
        let result_sha = content_hash(result).unwrap();
        json!({
            "schemaVersion": 1, "snippetId": subject, "revision": 3, "updatedAt": 154,
            "blobs": { (&sha): source, (&result_sha): result },
            "records": [{
                "id": RECORD, "snippetId": subject, "purpose": "ANALYSIS",
                "analyzedAt": 150, "updatedAt": 154, "pinned": true,
                "source": { "sha256": sha, "content": sha, "language": "bash", "lineCount": 1 },
                "provenance": {"profileId": "91", "profileName": "Fixture", "model": "local"},
                "summary": "Preserve the diagnosis.",
                "improvements": [{"id": "S1", "category": "security", "detail": "Validate input."}],
                "selection": {"improvementIds":["S1"]},
                "applyRuns": [{"id": RUN, "outcome": "ACCEPTED", "summary": "Prior accepted edit",
                    "resultSha256": result_sha, "resultContent": result_sha,
                    "acceptedContentSha256": result_sha, "acceptedContent": result_sha,
                    "request":{"baseSha256": sha, "baseContent": sha}, "savedToSnippetAt": 154}],
                "diagram": {"typeId":"logical-structure", "mermaid":"flowchart TD\n A[Start] --> B[End]",
                    "sourceSha256": sha, "codeReferences":[{"nodeId":"A", "startLine":1,"endLine":1}]},
                "exports":[{"exportedAt":153, "format":"JSON", "phase":"AFTER_APPLY", "runId":RUN, "fileName":"previous.json"}]
            }]
        })
    }

    #[test]
    fn java_analysis_project_keeps_exact_reports_and_read_only_apply_evidence() {
        let path = format!("snippet-analyses/{SUBJECT}.json");
        let projection = project(&path, &serde_json::to_vec(&fixture(SUBJECT)).unwrap()).unwrap();
        assert_eq!(projection.omissions, 0);
        assert_eq!(
            projection.filename,
            format!("{}.json", SUBJECT.replace('-', ""))
        );
        let history = decode(&projection.filename, &projection.bytes).unwrap();
        assert_eq!(history.revision, 3);
        let record = history.current().unwrap();
        assert!(record.pinned);
        assert_eq!(
            record.source.content.as_deref(),
            Some("printf 'Wert α'\r\n")
        );
        assert_eq!(
            record.provenance.profile_id,
            crate::ai_manager::ProfileRef::Numeric(0)
        );
        assert!(record.apply_runs.is_empty() && record.apply_recovery.is_none());
        let evidence = record.java_history.as_ref().unwrap();
        assert_eq!(evidence.profile_id, "91");
        assert_eq!(evidence.after_apply_exports.len(), 1);
        let run = &evidence.apply_runs[0];
        assert!(run.details.is_none());
        assert_eq!(
            evidence.attachments[&run.result_sha256].as_ref(),
            "printf 'Wert β'\r\n"
        );
        assert!(record.diagram.as_ref().unwrap().validate(1).is_ok());
        assert_eq!(
            portable_backup(&projection.filename, &projection.bytes).unwrap(),
            projection.bytes
        );
    }

    #[test]
    fn java_analysis_preserves_historical_run_parameters_and_progress_without_private_prompts() {
        let path = format!("snippet-analyses/{SUBJECT}.json");
        let mut java = fixture(SUBJECT);
        let run = &mut java["records"][0]["applyRuns"][0];
        let checkpoint_sha = run["resultSha256"].clone();
        run["outcome"] = json!("FAILED");
        run["request"]["snippetLanguage"] = json!("Bash");
        run["request"]["codeTextLanguageCode"] = json!("de");
        run["request"]["improvementIds"] = json!(["S1"]);
        run["request"]["dependencyIds"] = json!(["D1"]);
        run["request"]["hardeningOptions"] = json!(["STRICT_MODE"]);
        run["request"]["inputHardeningOptions"] = json!(["FILE_CHECK"]);
        run["request"]["migrationTargetLanguage"] = json!("Python");
        run["request"]["migrationTargetHostFormat"] = json!("Linux");
        run["request"]["migrationLabel"] = json!("Python on Linux");
        run["request"]["headerSnippetId"] = json!(SUBJECT);
        run["request"]["headerName"] = json!("License header");
        run["request"]["aiProfileId"] = json!("91");
        run["request"]["additionalInstructions"] = json!("private-request-fixture");
        run["request"]["headerText"] = json!("private-header-fixture");
        run["items"] = json!([{"stage": 1, "phase": "CORE", "id": "S1", "label": "Guard input",
            "category": "security", "severity": "high", "state": "DONE"}]);
        run["completedWorkItemIds"] = json!(["S1"]);
        run["checkpoint"] = json!({"completedStages": 1, "totalStages": 2, "content": checkpoint_sha,
            "summaries": ["Inputs checked"], "changes": [{"finding": "S1", "reason": "Validation"}],
            "completedRequirementIds": ["R1"], "usage": {"promptTokens": 20, "totalTokens": 24}});
        run["stats"] = json!({"elapsedSeconds": 8, "retries": 2, "completedItems": 1, "totalItems": 3,
            "statusKey": "stage.failed", "usage": {"totalTokens": 30}});
        run["provenance"] = json!({"profileId": "92", "profileName": "Apply fixture", "model": "apply-model",
            "skillIds": ["apply-skill"], "skillNames": ["Apply skill"], "durationMillis": 8000,
            "additionalInstructions": "private-provenance-fixture"});
        run["failureKey"] = json!("workflow.timeout");

        let projection = project(&path, &serde_json::to_vec(&java).unwrap()).unwrap();
        assert_eq!(projection.omissions, 1);
        let serialized = std::str::from_utf8(&projection.bytes).unwrap();
        assert!(!serialized.contains("private-"));
        let history = decode(&projection.filename, &projection.bytes).unwrap();
        let record = history.current().unwrap();
        assert!(record.apply_runs.is_empty() && record.apply_recovery.is_none());
        let evidence = record.java_history.as_ref().unwrap();
        let run = &evidence.apply_runs[0];
        assert_eq!(run.outcome, legacy::Outcome::Failed);
        assert_eq!(run.elapsed_seconds, 8);
        assert_eq!(run.usage.total_tokens, 30);
        assert_eq!(
            evidence.attachments[&run.checkpoint_sha256].as_ref(),
            "printf 'Wert β'\r\n"
        );
        let details = run.details.as_ref().unwrap();
        assert_eq!(details.failure_key.as_deref(), Some("workflow.timeout"));
        assert_eq!(details.completed_work_item_ids, ["S1"]);
        assert_eq!(details.work_items[0].state, "DONE");
        let options = details.options.as_ref().unwrap();
        assert_eq!(options.ai_profile_id.as_deref(), Some("91"));
        assert_eq!(options.header_snippet_id.as_deref(), Some(SUBJECT));
        assert_eq!(options.migration_label.as_deref(), Some("Python on Linux"));
        assert_eq!(options.input_hardening_options, ["FILE_CHECK"]);
        let checkpoint = details.checkpoint_progress.as_ref().unwrap();
        assert_eq!(
            (checkpoint.completed_stages, checkpoint.total_stages),
            (1, 2)
        );
        assert_eq!(checkpoint.completed_requirement_ids, ["R1"]);
        assert_eq!(checkpoint.usage.total_tokens, 24);
        let progress = details.progress.as_ref().unwrap();
        assert_eq!(
            (
                progress.completed_items,
                progress.total_items,
                progress.retries
            ),
            (1, 3, 2)
        );
        assert_eq!(progress.status_key.as_deref(), Some("stage.failed"));
        let provenance = details.provenance.as_ref().unwrap();
        assert_eq!(provenance.profile_id, "92");
        assert_eq!(provenance.skill_names, ["Apply skill"]);
        assert_eq!(
            portable_backup(&projection.filename, &projection.bytes).unwrap(),
            projection.bytes
        );
    }

    #[test]
    fn invalid_additional_run_metadata_is_omitted_without_losing_the_historical_result() {
        let path = format!("snippet-analyses/{SUBJECT}.json");
        let mut java = fixture(SUBJECT);
        java["records"][0]["applyRuns"][0]["items"] =
            json!([{"id":"S1", "label":"\u{202e}hidden"}]);
        let projection = project(&path, &serde_json::to_vec(&java).unwrap()).unwrap();
        assert_eq!(projection.omissions, 1);
        let history = decode(&projection.filename, &projection.bytes).unwrap();
        let record = history.current().unwrap();
        let evidence = record.java_history.as_ref().unwrap();
        let run = &evidence.apply_runs[0];
        assert!(run.details.is_none());
        assert_eq!(run.outcome, legacy::Outcome::Accepted);
        assert!(evidence.attachments.contains_key(&run.result_sha256));
    }

    #[test]
    fn java_analysis_project_rejects_cross_subject_and_changed_blobs() {
        let subject = format!("folder-{SUBJECT}");
        let path = format!("snippet-analyses/{subject}.json");
        let mut java = fixture(&subject);
        let projection = project(&path, &serde_json::to_vec(&java).unwrap()).unwrap();
        assert!(matches!(
            binding(&projection.filename, &projection.bytes),
            Ok(Subject::Folder(_))
        ));
        assert!(
            project(
                &format!("snippet-analyses/{SUBJECT}.json"),
                &serde_json::to_vec(&java).unwrap()
            )
            .is_err()
        );
        java["records"][0]["snippetId"] = json!(SUBJECT);
        assert!(project(&path, &serde_json::to_vec(&java).unwrap()).is_err());
        java["records"][0]["snippetId"] = json!(subject);
        let sha = java["records"][0]["source"]["sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        java["blobs"][sha] = json!("changed after the analysis");
        assert!(project(&path, &serde_json::to_vec(&java).unwrap()).is_err());
    }
}
