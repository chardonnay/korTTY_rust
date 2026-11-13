//! Immutable proposals and their explicit review decisions.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::hardening::{HardeningOption, InputOption, Requirement};
use super::{Analysis, Provenance, content_hash, source_fits};
use crate::ai_manager::CodeTextLanguage;
use crate::snippets::SnippetId;

pub(crate) const MAX_APPLY_CONTENT_BYTES: usize = 256 * 1024;
const MAX_ANALYSIS_ITEMS: usize = 256;
const MAX_SELECTION: usize =
    MAX_ANALYSIS_ITEMS + HardeningOption::ALL.len() + InputOption::ALL.len() + 1;
const MAX_REASON_BYTES: usize = 4096;
const MAX_ANCHOR_BYTES: usize = 512;
pub(in crate::snippet_analysis) const APPLY_PLAN_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ApplyStatus {
    Proposed,
    Accepted,
    Rejected,
}

impl ApplyStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Proposed => "Waiting for review",
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Selection {
    pub(crate) findings: Vec<String>,
    pub(crate) dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) requirements: Vec<Requirement>,
}

impl Selection {
    pub(in crate::snippet_analysis) fn has_required_source(&self) -> bool {
        self.requirements.iter().all(Requirement::has_source)
    }

    fn fits_retention(&self, limit: usize) -> bool {
        self.requirements
            .iter()
            .all(|item| item.fits_retention(limit))
    }

    fn apply_retention(&mut self, limit: usize) -> io::Result<()> {
        self.requirements
            .iter_mut()
            .try_for_each(|item| item.apply_retention(limit))
    }

    fn matches_live(&self, live: &Self) -> bool {
        self.findings == live.findings
            && self.dependencies == live.dependencies
            && self.requirements.len() == live.requirements.len()
            && self
                .requirements
                .iter()
                .zip(&live.requirements)
                .all(|(archived, current)| archived.matches_live(current))
    }

    pub(crate) fn len(&self) -> usize {
        self.findings.len() + self.dependencies.len() + self.requirements.len()
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        self.findings
            .iter()
            .chain(&self.dependencies)
            .any(|selected| selected == id)
            || self
                .requirements
                .iter()
                .any(|requirement| requirement.id() == id)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(in crate::snippet_analysis) fn valid(&self, analysis: &Analysis) -> bool {
        let mut seen = HashSet::new();
        (1..=MAX_SELECTION).contains(&self.len())
            && self.findings.len() + self.dependencies.len() <= MAX_ANALYSIS_ITEMS
            && self.findings.iter().all(|id| {
                seen.insert(id.as_str()) && analysis.improvements.iter().any(|item| &item.id == id)
            })
            && self.dependencies.iter().all(|id| {
                seen.insert(id.as_str()) && analysis.dependencies.iter().any(|item| &item.id == id)
            })
            && self.requirements.iter().all(|requirement| {
                let id = requirement.id();
                requirement.valid()
                    && seen.insert(id)
                    && !analysis.improvements.iter().any(|item| item.id == id)
                    && !analysis.dependencies.iter().any(|item| item.id == id)
            })
    }

    pub(in crate::snippet_analysis) fn requirements_through(&self, count: usize) -> &[Requirement] {
        let end = count.saturating_sub(self.findings.len() + self.dependencies.len());
        &self.requirements[..end.min(self.requirements.len())]
    }

    pub(in crate::snippet_analysis) fn through(&self, count: usize) -> Self {
        Self {
            findings: self.findings.iter().take(count).cloned().collect(),
            dependencies: self
                .dependencies
                .iter()
                .take(count.saturating_sub(self.findings.len()))
                .cloned()
                .collect(),
            requirements: self.requirements_through(count).to_vec(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChangeReason {
    pub(crate) id: String,
    pub(crate) reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) anchor: Option<String>,
}

impl ChangeReason {
    pub(crate) fn valid_anchor(anchor: &str) -> bool {
        anchor.len() <= MAX_ANCHOR_BYTES
            && !anchor.trim().is_empty()
            && !anchor
                .chars()
                .any(|character| character.is_control() && character != '\t')
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Checkpoint {
    #[serde(skip)]
    pub(in crate::snippet_analysis) code: Arc<str>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(in crate::snippet_analysis) code_omitted: bool,
    pub(in crate::snippet_analysis) code_sha256: String,
    pub(in crate::snippet_analysis) reasons: Vec<ChangeReason>,
    pub(in crate::snippet_analysis) completed: usize,
}

impl Checkpoint {
    pub(crate) fn completed(&self) -> usize {
        self.completed
    }

    pub(crate) fn has_source(&self) -> bool {
        !self.code_omitted
    }

    pub(in crate::snippet_analysis) fn fits_retention(&self, limit: usize) -> bool {
        self.code_omitted || source_fits(&self.code, limit)
    }

    pub(in crate::snippet_analysis) fn omit_source(&mut self) {
        self.code = Arc::from("");
        self.code_omitted = true;
        omit_anchors(&mut self.reasons);
    }

    fn valid_source(&self) -> io::Result<bool> {
        if !valid_hash(&self.code_sha256) {
            return Ok(false);
        }
        if self.code_omitted {
            return Ok(
                self.code.is_empty() && self.reasons.iter().all(|reason| reason.anchor.is_none())
            );
        }
        Ok(valid_body(&self.code, false) && content_hash(&self.code)? == self.code_sha256)
    }

    fn same_progress(&self, other: &Self) -> bool {
        self.completed == other.completed
            && self.code_sha256 == other.code_sha256
            && self.reasons.len() == other.reasons.len()
            && self.matches_reason_prefix(other)
    }

    fn matches_reason_prefix(&self, live: &Self) -> bool {
        self.reasons.len() <= live.reasons.len()
            && self
                .reasons
                .iter()
                .zip(&live.reasons)
                .all(|(saved, current)| {
                    saved.id == current.id
                        && saved.reason == current.reason
                        && (self.code_omitted || saved.anchor == current.anchor)
                })
    }
}

impl fmt::Debug for Checkpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Checkpoint")
            .field("completed", &self.completed)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplyRequestSnapshot {
    pub(in crate::snippet_analysis) version: u8,
    pub(in crate::snippet_analysis) source_sha256: String,
    pub(in crate::snippet_analysis) rendered_sha256: String,
    pub(in crate::snippet_analysis) plan_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::snippet_analysis) system_prompt: Option<String>,
    pub(in crate::snippet_analysis) selection: Selection,
    pub(in crate::snippet_analysis) instructions: String,
    pub(in crate::snippet_analysis) guidance: String,
    pub(in crate::snippet_analysis) report_language_code: String,
    pub(in crate::snippet_analysis) code_text_language: CodeTextLanguage,
    pub(in crate::snippet_analysis) stage_width: usize,
    pub(in crate::snippet_analysis) total_stages: usize,
}

impl ApplyRequestSnapshot {
    pub(crate) fn instructions(&self) -> &str {
        &self.instructions
    }

    fn selected_through(&self, completed: usize) -> Selection {
        self.selection
            .through(completed.saturating_mul(self.stage_width))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplyRecovery {
    pub(crate) revision: SnippetId,
    pub(crate) started_at: u64,
    pub(crate) elapsed_millis: u64,
    pub(crate) profile_name: String,
    pub(crate) model: String,
    pub(crate) request: ApplyRequestSnapshot,
    pub(crate) checkpoint: Option<Arc<Checkpoint>>,
}

impl fmt::Debug for ApplyRecovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplyRecovery")
            .field("revision", &self.revision)
            .field(
                "completed",
                &self.checkpoint.as_deref().map(Checkpoint::completed),
            )
            .finish_non_exhaustive()
    }
}

impl ApplyRecovery {
    pub(in crate::snippet_analysis) fn matches_live(&self, live: &Self) -> bool {
        if !self.request.selection.matches_live(&live.request.selection) {
            return false;
        }
        let mut retained = live.clone();
        retained.request.selection = self.request.selection.clone();
        if self
            .checkpoint
            .as_ref()
            .is_some_and(|point| !point.has_source())
            && let Some(point) = retained.checkpoint.as_mut()
        {
            Arc::make_mut(point).omit_source();
        }
        retained == *self
    }

    pub(in crate::snippet_analysis) fn fits_retention(&self, limit: usize) -> bool {
        self.request.selection.fits_retention(limit)
            && self
                .checkpoint
                .as_ref()
                .is_none_or(|point| point.fits_retention(limit))
    }

    pub(in crate::snippet_analysis) fn apply_retention(&mut self, limit: usize) -> io::Result<()> {
        self.request.selection.apply_retention(limit)?;
        if let Some(point) = self.checkpoint.as_mut()
            && !point.fits_retention(limit)
        {
            Arc::make_mut(point).omit_source();
        }
        Ok(())
    }

    pub(in crate::snippet_analysis) fn continues(&self, previous: &Self) -> bool {
        let mut request = self.request.clone();
        request.selection = previous.request.selection.clone();
        if request != previous.request
            || !previous
                .request
                .selection
                .matches_live(&self.request.selection)
            || self.started_at != previous.started_at
            || self.elapsed_millis < previous.elapsed_millis
        {
            return false;
        }
        if match (&self.checkpoint, &previous.checkpoint) {
            (Some(current), Some(previous)) => previous.same_progress(current),
            (None, None) => true,
            _ => false,
        } {
            return self.profile_name == previous.profile_name && self.model == previous.model;
        }
        match (&self.checkpoint, &previous.checkpoint) {
            (Some(current), Some(previous)) => {
                current.has_source()
                    && current.completed == previous.completed + 1
                    && previous.matches_reason_prefix(current)
            }
            (Some(current), None) => current.has_source() && current.completed == 1,
            (None, _) => false,
        }
    }

    pub(in crate::snippet_analysis) fn validate(&self, analysis: &Analysis) -> io::Result<()> {
        let request = &self.request;
        let short_text =
            |text: &str, limit| text.len() <= limit && !text.chars().any(char::is_control);
        if self.started_at == 0
            || !short_text(&self.profile_name, 160)
            || !short_text(&self.model, crate::local_ai::MAX_MODEL_BYTES)
            || request.version != APPLY_PLAN_VERSION
            || !valid_hash(&request.source_sha256)
            || !valid_hash(&request.rendered_sha256)
            || !valid_hash(&request.plan_sha256)
            || request.system_prompt.as_ref().is_some_and(|system| {
                system.is_empty()
                    || system.len() > 16 * 1024
                    || system.chars().any(|ch| ch.is_control() && ch != '\n')
            })
            || request.instructions.len() > 4096
            || request.guidance.len() > 16 * 1024
            || !short_text(&request.report_language_code, 64)
            || !request.selection.valid(analysis)
            || !matches!(request.stage_width, 3 | 6)
            || request.total_stages != request.selection.len().div_ceil(request.stage_width)
        {
            return Err(invalid("The stored apply request has an invalid plan"));
        }
        if let Some(checkpoint) = self.checkpoint.as_deref() {
            if !(1..=request.total_stages).contains(&checkpoint.completed)
                || !checkpoint.valid_source()?
                || (checkpoint.completed == request.total_stages
                    && checkpoint.code_sha256 == request.source_sha256)
            {
                return Err(invalid(
                    "The stored apply checkpoint has invalid source or progress",
                ));
            }
            let selected = request.selected_through(checkpoint.completed);
            if checkpoint.reasons.len() != selected.len()
                || !valid_reasons(&checkpoint.reasons, &selected)
                || (checkpoint.has_source()
                    && selected.requirements.iter().any(|requirement| {
                        requirement.has_source() && !requirement.present(&checkpoint.code)
                    }))
                || checkpoint
                    .reasons
                    .iter()
                    .any(|reason| reason.reason.chars().any(char::is_control))
            {
                return Err(invalid(
                    "The stored apply checkpoint has invalid change reasons",
                ));
            }
        }
        Ok(())
    }

    pub(in crate::snippet_analysis) fn matches_proposal(&self, run: &ApplyRun) -> bool {
        let Some(checkpoint) = self.checkpoint.as_deref() else {
            return false;
        };
        let total = self.request.total_stages;
        let completed = checkpoint.completed;
        let expected_partial = (completed < total).then_some(PartialApply {
            completed_stages: completed,
            total_stages: total,
            total_items: self.request.selection.len(),
        });
        run.started_at == self.started_at
            && run.id == self.revision
            && run.before_sha256 == self.request.source_sha256
            && run.after_sha256 == checkpoint.code_sha256
            && self
                .request
                .selected_through(completed)
                .matches_live(&run.selected)
            && run.partial == expected_partial
            && run.code_text_language == Some(self.request.code_text_language)
            && run.provenance.profile_name == self.profile_name
            && run.provenance.model == self.model
            && run.provenance.additional_instructions == self.request.instructions
            && run.provenance.duration_millis == self.elapsed_millis
            && run.reasons.len() == checkpoint.reasons.len()
            && run
                .reasons
                .iter()
                .zip(&checkpoint.reasons)
                .all(|(reason, saved)| {
                    reason.id == saved.id
                        && reason.reason == saved.reason
                        && (checkpoint.code_omitted
                            || reason
                                .anchor
                                .as_ref()
                                .is_none_or(|anchor| saved.anchor.as_ref() == Some(anchor)))
                })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialApply {
    pub(crate) completed_stages: usize,
    pub(crate) total_stages: usize,
    pub(crate) total_items: usize,
}

impl PartialApply {
    pub(crate) fn progress(self) -> String {
        format!(
            "{} of {} stages completed",
            self.completed_stages, self.total_stages
        )
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplyRun {
    pub(crate) id: SnippetId,
    pub(crate) status: ApplyStatus,
    pub(crate) started_at: u64,
    pub(crate) decided_at: Option<u64>,
    pub(crate) before: String,
    pub(crate) after: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    before_omitted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    after_omitted: bool,
    pub(crate) before_sha256: String,
    pub(crate) after_sha256: String,
    pub(crate) selected: Selection,
    pub(crate) reasons: Vec<ChangeReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) partial: Option<PartialApply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) code_text_language: Option<CodeTextLanguage>,
    #[serde(default)]
    pub(crate) provenance: Provenance,
}

impl fmt::Debug for ApplyRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplyRun")
            .field("id", &self.id)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl ApplyRun {
    pub(crate) fn new(
        before: String,
        after: String,
        selected: Selection,
        reasons: Vec<ChangeReason>,
        started_at: u64,
    ) -> io::Result<Self> {
        let run = Self {
            id: SnippetId::new()?,
            status: ApplyStatus::Proposed,
            started_at,
            decided_at: None,
            before_sha256: content_hash(&before)?,
            after_sha256: content_hash(&after)?,
            before,
            after,
            before_omitted: false,
            after_omitted: false,
            selected,
            reasons,
            partial: None,
            code_text_language: None,
            provenance: Provenance::default(),
        };
        run.validate_content()?;
        Ok(run)
    }

    pub(crate) fn code_text_language_code(&self) -> Option<&str> {
        self.code_text_language
            .map(|language| language.explicit_code().unwrap_or("auto"))
    }

    pub(crate) fn has_review_source(&self) -> bool {
        !self.before_omitted && !self.after_omitted && self.selected.has_required_source()
    }

    pub(crate) fn matches_live(&self, live: &Self) -> bool {
        if !live.has_review_source() || !self.selected.matches_live(&live.selected) {
            return false;
        }
        let mut retained = live.clone();
        retained.selected = self.selected.clone();
        if self.before_omitted {
            retained.before = String::new();
            retained.before_omitted = true;
        }
        if self.after_omitted {
            retained.after = String::new();
            retained.after_omitted = true;
            omit_anchors(&mut retained.reasons);
        }
        retained == *self
    }

    pub(in crate::snippet_analysis) fn fits_retention(&self, limit: usize) -> bool {
        (self.before_omitted || source_fits(&self.before, limit))
            && (self.after_omitted || source_fits(&self.after, limit))
            && self.selected.fits_retention(limit)
    }

    pub(in crate::snippet_analysis) fn apply_retention(&mut self, limit: usize) -> io::Result<()> {
        if !self.before_omitted && !source_fits(&self.before, limit) {
            self.before = String::new();
            self.before_omitted = true;
        }
        if !self.after_omitted && !source_fits(&self.after, limit) {
            self.after = String::new();
            self.after_omitted = true;
            omit_anchors(&mut self.reasons);
        }
        self.selected.apply_retention(limit)
    }

    pub(crate) fn validate(&self, analysis: &Analysis) -> io::Result<()> {
        self.validate_content()?;
        if !self.selected.valid(analysis) {
            return Err(invalid(
                "The apply selection no longer matches the analysis",
            ));
        }
        if !valid_hash(&self.before_sha256)
            || !valid_hash(&self.after_sha256)
            || (!self.before_omitted && content_hash(&self.before)? != self.before_sha256)
            || (!self.after_omitted && content_hash(&self.after)? != self.after_sha256)
        {
            return Err(invalid("The apply source does not match its hash"));
        }
        Ok(())
    }

    fn validate_content(&self) -> io::Result<()> {
        if !valid_body(&self.before, self.before_omitted)
            || !valid_body(&self.after, self.after_omitted)
            || self.before_sha256 == self.after_sha256
            || self.selected.is_empty()
            || self.started_at == 0
            || (self.status == ApplyStatus::Proposed) != self.decided_at.is_none()
            || self.decided_at.is_some_and(|date| date < self.started_at)
        {
            return Err(invalid(
                "The apply run has invalid source, selection or timestamps",
            ));
        }
        if !valid_reasons(&self.reasons, &self.selected)
            || (self.after_omitted && self.reasons.iter().any(|reason| reason.anchor.is_some()))
            || self.selected.requirements.iter().any(|requirement| {
                !requirement.valid()
                    || (!self.after_omitted
                        && requirement.has_source()
                        && !requirement.present(&self.after))
                    || !self
                        .reasons
                        .iter()
                        .any(|reason| reason.id == requirement.id())
            })
        {
            return Err(invalid("The apply run has invalid change reasons"));
        }
        if let Some(partial) = self.partial
            && (partial.completed_stages == 0
                || partial.completed_stages >= partial.total_stages
                || partial.total_stages > partial.total_items
                || partial.total_items > MAX_SELECTION
                || partial.total_items <= self.selected.len()
                || self.reasons.len() != self.selected.len()
                || partial.completed_stages > self.selected.len()
                || partial.total_stages - partial.completed_stages
                    > partial.total_items - self.selected.len())
        {
            return Err(invalid("The partial apply run has invalid progress"));
        }
        Ok(())
    }
}

fn valid_body(content: &str, omitted: bool) -> bool {
    if omitted {
        content.is_empty()
    } else {
        !content.trim().is_empty() && content.len() <= MAX_APPLY_CONTENT_BYTES
    }
}

fn omit_anchors(reasons: &mut [ChangeReason]) {
    for reason in reasons {
        reason.anchor = None;
    }
}

fn valid_reasons(reasons: &[ChangeReason], selected: &Selection) -> bool {
    let mut seen = HashSet::new();
    reasons.len() <= MAX_SELECTION
        && reasons.iter().all(|reason| {
            selected.contains(&reason.id)
                && seen.insert(&reason.id)
                && !reason.reason.trim().is_empty()
                && reason.reason.len() <= MAX_REASON_BYTES
                && reason
                    .anchor
                    .as_deref()
                    .is_none_or(ChangeReason::valid_anchor)
        })
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
