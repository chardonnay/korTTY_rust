//! Decodes analysis replies and constrains locations to the submitted sources.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

use super::super::model::{Analysis, Dependency, Finding, ProjectFileNote};
use super::{Snapshot, relative_path, safe_metadata, source_lines, source_text};
use crate::local_ai::reply;

pub(super) struct Sources {
    project: bool,
    files: Vec<FileLines>,
}

struct FileLines {
    path: String,
    folded: String,
    count: usize,
}

impl Sources {
    pub(super) fn new(snapshot: &Snapshot) -> Self {
        Self {
            project: snapshot.project,
            files: snapshot
                .files
                .iter()
                .map(|file| FileLines {
                    path: file.path.clone(),
                    folded: file.path.to_lowercase(),
                    count: source_lines(&source_text(&file.content)).count(),
                })
                .collect(),
        }
    }

    fn known_file(&self, raw: &str) -> Option<&FileLines> {
        if !self.project {
            return None;
        }
        let mut normalized = raw.trim().trim_matches(['`', '\'', '"']).replace('\\', "/");
        while let Some((path, suffix)) = normalized.rsplit_once(':') {
            if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                break;
            }
            normalized.truncate(path.len());
        }
        let mut path = normalized.trim_start_matches('/');
        while let Some(rest) = path.strip_prefix("./") {
            path = rest.trim_start_matches('/');
        }
        if !relative_path(path) {
            return None;
        }
        if let Some(exact) = self.files.iter().find(|file| file.path == path) {
            return Some(exact);
        }
        let lower = path.to_lowercase();
        let mut best: Option<&FileLines> = None;
        let mut tied = false;
        for file in &self.files {
            if lower == file.folded || path_suffix(&lower, &file.folded) {
                match best {
                    Some(best) if best.folded.len() > file.folded.len() => {}
                    Some(best) if best.folded.len() == file.folded.len() => tied = true,
                    _ => {
                        best = Some(file);
                        tied = false;
                    }
                }
            }
        }
        if best.is_some() {
            return best.filter(|_| !tied);
        }
        let mut matches = self
            .files
            .iter()
            .filter(|file| path_suffix(&file.folded, &lower));
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }
}

fn path_suffix(path: &str, suffix: &str) -> bool {
    path.strip_suffix(suffix)
        .is_some_and(|prefix| prefix.ends_with('/'))
}

type Object = Map<String, Value>;

pub(super) fn parse(answer: &str, sources: &Sources) -> Result<Analysis, String> {
    let object = parse_object(answer).ok_or("AI analysis did not return a JSON object.")?;
    let summary = string(
        &object,
        &["summary", "explanation", "description", "overview"],
    )
    .to_owned();
    let mut ids = HashSet::new();
    let dependencies: Vec<_> = objects(&object, &["dependencies", "deps", "requirements"])
        .filter_map(|(index, entry)| dependency(index, entry, &mut ids))
        .collect();
    let improvements: Vec<_> = objects(
        &object,
        &["improvements", "findings", "suggestions", "tips"],
    )
    .filter_map(|(index, entry)| finding(index, entry, sources, &mut ids))
    .collect();
    if summary.is_empty() && dependencies.is_empty() && improvements.is_empty() {
        return Err("AI returned no usable snippet analysis.".into());
    }
    Ok(Analysis {
        summary,
        dependencies,
        improvements,
        project_files: project_files(&object, sources),
    })
}

fn parse_object(answer: &str) -> Option<Object> {
    let answer = answer.trim();
    if let Ok(object) = serde_json::from_str(reply::strip_fence(answer)) {
        return Some(object);
    }
    let sanitized = reply::sanitize_reply(answer);
    let start = sanitized.find('{')?;
    let candidate = &sanitized[start..];
    object_prefix(candidate).or_else(|| object_prefix(&repair_raw_quotes(candidate)?))
}

fn object_prefix(candidate: &str) -> Option<Object> {
    serde_json::Deserializer::from_str(candidate)
        .into_iter::<Object>()
        .next()?
        .ok()
}

// Analysis fields are prose. Like the Java parser, preserve a raw quote in a string only
// when the next non-space character cannot delimit that JSON key or value.
fn repair_raw_quotes(candidate: &str) -> Option<String> {
    let mut repaired = String::with_capacity(candidate.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut changed = false;
    for (offset, character) in candidate.char_indices() {
        if escaped {
            escaped = false;
        } else if in_string && character == '\\' {
            escaped = true;
        } else if character == '"' {
            let closes_string = candidate[offset + 1..]
                .trim_start()
                .chars()
                .next()
                .is_none_or(|next| matches!(next, ',' | '}' | ']' | ':'));
            if !in_string || closes_string {
                in_string = !in_string;
            } else {
                changed = true;
                repaired.push('\\');
            }
        }
        repaired.push(character);
    }
    changed.then_some(repaired)
}

fn objects<'a>(object: &'a Object, names: &[&str]) -> impl Iterator<Item = (usize, &'a Object)> {
    names
        .iter()
        .find_map(|name| object.get(*name)?.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, value)| Some((index + 1, value.as_object()?)))
}

fn string<'a>(object: &'a Object, names: &[&str]) -> &'a str {
    names
        .iter()
        .filter_map(|name| Some(object.get(*name)?.as_str()?.trim()))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn dependency(index: usize, entry: &Object, ids: &mut HashSet<String>) -> Option<Dependency> {
    let name = string(entry, &["name", "dependency", "tool", "service"]);
    if name.is_empty() {
        return None;
    }
    Some(Dependency {
        id: unique_id(string(entry, &["id"]), "D", index, ids),
        name: name.to_owned(),
        kind: string(entry, &["kind", "type", "category"]).to_owned(),
        purpose: string(entry, &["purpose", "reason", "detail", "description"]).to_owned(),
        suggestion: string(
            entry,
            &[
                "suggestion",
                "recommendation",
                "replacement",
                "reduce",
                "alternative",
            ],
        )
        .to_owned(),
    })
}

fn finding(
    index: usize,
    entry: &Object,
    sources: &Sources,
    ids: &mut HashSet<String>,
) -> Option<Finding> {
    let title = string(entry, &["title", "name"]);
    let detail = string(entry, &["detail", "impact", "description"]);
    let recommendation = string(entry, &["recommendation", "fix", "suggestion"]);
    if title.is_empty() && detail.is_empty() && recommendation.is_empty() {
        return None;
    }
    let file = raw_path(entry, 0).and_then(|raw| sources.known_file(raw));
    let line_file = if sources.project {
        file
    } else {
        sources.files.first()
    };
    let line = line_file.and_then(|file| {
        ["line", "lineNumber"].iter().find_map(|key| {
            let value = entry.get(*key)?;
            let line = value
                .as_u64()
                .and_then(|line| usize::try_from(line).ok())
                .or_else(|| value.as_str()?.trim().parse::<usize>().ok())?;
            (line > 0 && line <= file.count).then_some(line)
        })
    });
    let severity = string(entry, &["severity", "priority"]);
    Some(Finding {
        id: unique_id(string(entry, &["id"]), "I", index, ids),
        category: category(string(entry, &["category", "kind", "type"])).into(),
        severity: if severity.is_empty() {
            "info"
        } else {
            severity
        }
        .into(),
        title: title.to_owned(),
        detail: detail.to_owned(),
        recommendation: recommendation.to_owned(),
        line,
        file: file.map(|file| file.path.clone()),
    })
}

fn unique_id(raw: &str, prefix: &str, index: usize, ids: &mut HashSet<String>) -> String {
    if !raw.is_empty() && raw.len() <= 64 && safe_metadata(raw) && ids.insert(raw.to_owned()) {
        return raw.to_owned();
    }
    let base = format!("{prefix}{index}");
    if ids.insert(base.clone()) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let id = format!("{base}-{suffix}");
        if ids.insert(id.clone()) {
            return id;
        }
        suffix += 1;
    }
}

fn category(value: &str) -> &'static str {
    match value.to_lowercase().as_str() {
        "security" | "sicherheit" | "sec" | "vulnerability" => "security",
        "optimization" | "optimisation" | "performance" | "optimierung" | "perf" | "efficiency" => {
            "optimization"
        }
        "dependency" | "dependencies" | "abhängigkeit" | "abhaengigkeit" | "deps" | "dep" => {
            "dependency"
        }
        _ => "design",
    }
}

fn raw_path(entry: &Object, depth: usize) -> Option<&str> {
    if depth == 4 {
        return None;
    }
    for key in [
        "file",
        "path",
        "filePath",
        "file_path",
        "filename",
        "fileName",
        "source",
        "location",
        "files",
    ] {
        let Some(value) = entry.get(key) else {
            continue;
        };
        let path = match value {
            Value::Object(nested) => raw_path(nested, depth + 1),
            Value::Array(values) if values.len() == 1 => values[0].as_str(),
            Value::Array(_) => return None,
            value => value.as_str(),
        };
        if path.is_some_and(|path| !path.trim().is_empty()) {
            return path;
        }
    }
    None
}

fn project_files(object: &Object, sources: &Sources) -> Vec<ProjectFileNote> {
    if !sources.project {
        return Vec::new();
    }
    let mut purposes = BTreeMap::new();
    let files = ["files", "fileSummaries", "file_summaries"]
        .iter()
        .find_map(|key| object.get(*key).filter(|value| !value.is_null()));
    let mut note = |path: &str, purpose: &str| {
        if let Some(file) = sources.known_file(path)
            && !purpose.trim().is_empty()
        {
            purposes
                .entry(file.path.as_str())
                .or_insert_with(|| purpose.trim().to_owned());
        }
    };
    match files {
        Some(Value::Array(entries)) => {
            for entry in entries.iter().filter_map(Value::as_object) {
                if let Some(path) = raw_path(entry, 0) {
                    note(path, purpose(entry));
                }
            }
        }
        Some(Value::Object(entries)) => {
            for (path, entry) in entries {
                note(
                    path,
                    entry
                        .as_str()
                        .or_else(|| entry.as_object().map(purpose))
                        .unwrap_or_default(),
                );
            }
        }
        _ => {}
    }
    sources
        .files
        .iter()
        .map(|file| ProjectFileNote {
            path: file.path.clone(),
            purpose: purposes.remove(file.path.as_str()).unwrap_or_default(),
        })
        .collect()
}

fn purpose(entry: &Object) -> &str {
    string(entry, &["purpose", "summary", "description", "role"])
}
