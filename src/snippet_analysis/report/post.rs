//! After-apply report context taken from a saved history.

use crate::snippets::SnippetId;

use super::super::model::{Analysis, ApplyStatus, Record};

#[derive(Clone)]
pub(crate) struct Post {
    pub(crate) run_id: SnippetId,
    pub(super) verification: Option<ReportVerification>,
}

#[derive(Clone)]
pub(super) struct ReportVerification {
    pub(super) verified_at: u64,
    pub(super) resolved: Vec<Delta>,
    pub(super) new: Vec<Delta>,
    pub(super) persisting: Vec<Delta>,
}

#[derive(Clone)]
pub(super) struct Delta {
    pub(super) id: String,
    pub(super) previous_id: Option<String>,
    pub(super) title: String,
    pub(super) severity: String,
    pub(super) category: String,
}

impl Post {
    pub(crate) fn capture(record: &Record, run_id: SnippetId, history: &[Record]) -> Option<Self> {
        let run = record.apply_runs.iter().find(|run| run.id == run_id)?;
        let verification = (run.status == ApplyStatus::Accepted)
            .then(|| {
                let decided_at = run.decided_at?;
                history.iter().find_map(|candidate| {
                    let stored = candidate.verification.as_ref()?;
                    if stored.previous_record_id != record.id
                        || stored.run_id != run_id
                        || !stored.matches_applied_revision
                        || candidate.source.sha256 != run.after_sha256
                        || candidate.analyzed_at < decided_at
                    {
                        return None;
                    }
                    let current = &candidate.analysis;
                    Some(ReportVerification {
                        verified_at: candidate.analyzed_at,
                        resolved: stored
                            .resolved_previous_ids
                            .iter()
                            .map(|id| Delta::new(&record.analysis, id, None))
                            .collect(),
                        new: stored
                            .new_ids
                            .iter()
                            .map(|id| Delta::new(current, id, None))
                            .collect(),
                        persisting: stored
                            .persisting_current_to_previous
                            .iter()
                            .map(|(id, previous)| Delta::new(current, id, Some(previous)))
                            .collect(),
                    })
                })
            })
            .flatten();
        Some(Self {
            run_id,
            verification,
        })
    }
}

impl Delta {
    fn new(analysis: &Analysis, id: &str, previous: Option<&str>) -> Self {
        let mut delta = Self {
            id: id.into(),
            previous_id: previous.map(str::to_owned),
            title: String::new(),
            severity: String::new(),
            category: String::new(),
        };
        if let Some(finding) = analysis
            .improvements
            .iter()
            .find(|finding| finding.id == id)
        {
            delta.title = finding.title.clone();
            delta.severity = finding.severity.clone();
            delta.category = finding.category.clone();
        } else if let Some(dependency) = analysis
            .dependencies
            .iter()
            .find(|dependency| dependency.id == id)
        {
            delta.title = dependency.name.clone();
            delta.category = "dependency".into();
        }
        delta
    }
}
