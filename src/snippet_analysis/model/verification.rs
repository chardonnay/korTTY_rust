//! Deterministic verification deltas for a specific accepted apply run.

use std::collections::{BTreeMap, HashSet};
use std::io;

use serde::{Deserialize, Serialize};

use super::{Analysis, ApplyRun, ApplyStatus, Purpose, Record};
use crate::snippets::SnippetId;

const MAX_ITEMS: usize = 256;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MATCH_THRESHOLD: f64 = 0.34;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Verification {
    pub(crate) previous_record_id: SnippetId,
    pub(crate) run_id: SnippetId,
    pub(crate) matches_applied_revision: bool,
    pub(crate) resolved_previous_ids: Vec<String>,
    pub(crate) persisting_current_to_previous: BTreeMap<String, String>,
    pub(crate) new_ids: Vec<String>,
}

impl Verification {
    pub(crate) fn compare(pre: &Record, run_id: SnippetId, current: &Record) -> io::Result<Self> {
        let run = require_accepted_run(pre, run_id, current)?;
        let before = identifiers(&pre.analysis)?;
        let after = identifiers(&current.analysis)?;
        let mut persisting = match_findings(&pre.analysis, &current.analysis);
        match_dependencies(&pre.analysis, &current.analysis, &mut persisting);
        let matched_previous: HashSet<_> = persisting.values().map(String::as_str).collect();
        let resolved_previous_ids = before
            .into_iter()
            .filter(|id| !matched_previous.contains(id))
            .map(str::to_owned)
            .collect();
        let new_ids = after
            .into_iter()
            .filter(|id| !persisting.contains_key(*id))
            .map(str::to_owned)
            .collect();
        Ok(Self {
            previous_record_id: pre.id,
            run_id,
            matches_applied_revision: current.source.sha256 == run.after_sha256,
            resolved_previous_ids,
            persisting_current_to_previous: persisting,
            new_ids,
        })
    }

    /// Existing records can be deleted independently. Keep stored IDs usable
    /// when the old analysis is gone, while validating the current partition.
    pub(in crate::snippet_analysis) fn validate(
        &self,
        current: &Record,
        pre: Option<&Record>,
    ) -> io::Result<()> {
        if current.purpose != Purpose::Verify
            || current.previous_record_id != Some(self.previous_record_id)
            || current.id == self.previous_record_id
        {
            return Err(invalid(
                "The verification record has an invalid analysis identity",
            ));
        }
        let after = identifiers(&current.analysis)?;
        let resolved = self.resolved_previous_ids.iter().map(String::as_str);
        let pairs = &self.persisting_current_to_previous;
        let before: Vec<_> = resolved.chain(pairs.values().map(String::as_str)).collect();
        if before.len() > MAX_ITEMS
            || !partition(
                &after,
                self.new_ids.iter().chain(pairs.keys()).map(String::as_str),
            )
            || before.iter().copied().collect::<HashSet<_>>().len() != before.len()
            || before.iter().any(|id| id.is_empty())
        {
            return Err(invalid("The verification has invalid finding references"));
        }
        if let Some(pre) = pre {
            let run = require_accepted_run(pre, self.run_id, current)?;
            if self.matches_applied_revision != (current.source.sha256 == run.after_sha256)
                || !partition(&identifiers(&pre.analysis)?, before)
            {
                return Err(invalid(
                    "The verification does not match the earlier findings",
                ));
            }
        }
        Ok(())
    }
}

fn require_accepted_run<'a>(
    pre: &'a Record,
    run_id: SnippetId,
    current: &Record,
) -> io::Result<&'a ApplyRun> {
    let run = pre
        .apply_runs
        .iter()
        .find(|run| run.id == run_id && run.status == ApplyStatus::Accepted)
        .ok_or_else(|| invalid("The accepted apply run no longer exists"))?;
    if pre.id == current.id
        || current.analyzed_at < run.decided_at.unwrap_or(u64::MAX)
        || current.provenance.profile_id != pre.provenance.profile_id
    {
        return Err(invalid(
            "The verification must follow the accepted run and use its analysis profile",
        ));
    }
    Ok(run)
}

fn identifiers(analysis: &Analysis) -> io::Result<Vec<&str>> {
    let count = analysis
        .improvements
        .len()
        .saturating_add(analysis.dependencies.len());
    if count > MAX_ITEMS {
        return Err(invalid("The analysis has too many findings to verify"));
    }
    let bytes: usize = analysis
        .improvements
        .iter()
        .map(|item| {
            item.category.len() + item.title.len() + item.detail.len() + item.recommendation.len()
        })
        .chain(analysis.dependencies.iter().map(|item| item.name.len()))
        .sum();
    if bytes > MAX_TEXT_BYTES {
        return Err(invalid("The analysis has too much finding text to verify"));
    }
    let ids: Vec<_> = analysis
        .improvements
        .iter()
        .map(|item| item.id.as_str())
        .chain(analysis.dependencies.iter().map(|item| item.id.as_str()))
        .collect();
    if ids.iter().any(|id| id.is_empty())
        || ids.iter().copied().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err(invalid("The analysis findings need unique IDs"));
    }
    Ok(ids)
}

fn partition<'a>(ids: &[&str], values: impl IntoIterator<Item = &'a str>) -> bool {
    let mut remaining: HashSet<_> = ids.iter().copied().collect();
    values.into_iter().all(|id| remaining.remove(id)) && remaining.is_empty()
}

struct Terms {
    category: String,
    title: HashSet<String>,
    text: HashSet<String>,
}

fn terms(analysis: &Analysis) -> Vec<Terms> {
    analysis
        .improvements
        .iter()
        .map(|finding| Terms {
            category: finding.category.to_lowercase(),
            title: tokens(&finding.title),
            text: tokens(&format!("{} {}", finding.detail, finding.recommendation)),
        })
        .collect()
}

fn match_findings(pre: &Analysis, post: &Analysis) -> BTreeMap<String, String> {
    let previous = terms(pre);
    let current = terms(post);
    let mut candidates = Vec::new();
    for (after, now) in current.iter().enumerate() {
        for (before, old) in previous.iter().enumerate() {
            if now.category != old.category {
                continue;
            }
            let score =
                0.65 * jaccard(&now.title, &old.title) + 0.35 * jaccard(&now.text, &old.text);
            if score >= MATCH_THRESHOLD {
                let distance = post.improvements[after]
                    .line
                    .zip(pre.improvements[before].line)
                    .map_or(usize::MAX, |(a, b)| a.abs_diff(b));
                candidates.push((score, distance, before, after));
            }
        }
    }
    candidates.sort_unstable_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(post.improvements[a.3].id.cmp(&post.improvements[b.3].id))
            .then(pre.improvements[a.2].id.cmp(&pre.improvements[b.2].id))
    });
    let mut used_before = vec![false; previous.len()];
    let mut used_after = vec![false; current.len()];
    let mut matches = BTreeMap::new();
    for (_, _, before, after) in candidates {
        if !used_before[before] && !used_after[after] {
            used_before[before] = true;
            used_after[after] = true;
            matches.insert(
                post.improvements[after].id.clone(),
                pre.improvements[before].id.clone(),
            );
        }
    }
    matches
}

fn match_dependencies(pre: &Analysis, post: &Analysis, matches: &mut BTreeMap<String, String>) {
    let previous: Vec<_> = pre
        .dependencies
        .iter()
        .map(|item| normalized_name(&item.name))
        .collect();
    let mut used = vec![false; previous.len()];
    for item in &post.dependencies {
        let name = normalized_name(&item.name);
        if !name.is_empty()
            && let Some(index) = previous
                .iter()
                .enumerate()
                .position(|(index, previous)| !used[index] && *previous == name)
        {
            used[index] = true;
            matches.insert(item.id.clone(), pre.dependencies[index].id.clone());
        }
    }
}

fn normalized_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count();
    shared as f64 / (a.len() + b.len() - shared) as f64
}

fn tokens(value: &str) -> HashSet<String> {
    const STOP_WORDS: &str = "the and for with that this from are was were not but use using \
        into when which should could would can will all any its has have been than then there \
        their they you your our out via per der die das den dem des und oder mit für von vom \
        zum zur ein eine einen einem einer eines ist sind wird werden nicht auch auf aus bei \
        als wenn dass sich noch nur durch über unter nach kann soll sollte muss wie";
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter_map(|raw| {
            let length = raw.chars().count();
            if length < 3 || STOP_WORDS.split_whitespace().any(|word| word == raw) {
                return None;
            }
            let word = if length > 3 {
                raw.strip_suffix('s').unwrap_or(raw)
            } else {
                raw
            };
            Some(word.to_owned())
        })
        .collect()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippet_analysis::model::Finding;

    #[test]
    fn similar_findings_match_by_category_then_by_nearest_line() {
        let finding = |id: &str, category: &str, title: &str, line| Finding {
            id: id.into(),
            category: category.into(),
            title: title.into(),
            line: Some(line),
            detail: "Preserve argument boundaries during expansion.".into(),
            ..Finding::default()
        };
        let pre = Analysis {
            improvements: vec![
                finding("F1", "security", "Quote variable expansions", 2),
                finding("F2", "security", "Quote variable expansions", 15),
                finding("F3", "performance", "Quote variable expansions", 16),
            ],
            ..Analysis::default()
        };
        let post = Analysis {
            improvements: vec![finding(
                "F9",
                "Security",
                "Quote the variable expansion",
                16,
            )],
            ..Analysis::default()
        };
        assert_eq!(
            match_findings(&pre, &post),
            BTreeMap::from([("F9".into(), "F2".into())]),
        );
    }
}
