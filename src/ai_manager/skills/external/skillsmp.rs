//! SkillsMP keyword search with a separate GitHub download route.

use std::collections::HashSet;

use serde_json::Value;

use super::{Candidate, MAX_QUERY_BYTES, ProviderProfile, github, http::Operation, label};

pub(super) const API: &str = "https://skillsmp.com";
const MAX_RESULTS: usize = 30;

pub(super) fn search(
    operation: &Operation<'_>,
    profile: &ProviderProfile,
    profiles: &[ProviderProfile],
    query: &str,
) -> Result<Vec<Candidate>, String> {
    let query = query.trim();
    if query.len() > MAX_QUERY_BYTES
        || !query.chars().any(char::is_alphanumeric)
        || query.chars().any(char::is_control)
    {
        return Err("Enter up to 200 bytes of SkillsMP search keywords.".into());
    }
    let body = search_response(operation, profile, query, MAX_RESULTS)?;
    let skills = body["data"]["skills"]
        .as_array()
        .ok_or("SkillsMP returned no skill list.")?;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for item in skills.iter().take(MAX_RESULTS) {
        let reference = github::string(item, "githubUrl");
        let Ok(mut candidate) = github::marketplace_candidate(profile, profiles, reference) else {
            continue;
        };
        if !seen.insert(candidate.reference.clone()) {
            continue;
        }
        let name = label(github::string(item, "name"), 256);
        if !name.is_empty() {
            candidate.name = name;
        }
        let author = label(github::string(item, "author"), 160);
        if !author.is_empty() {
            candidate.author = author;
        }
        candidate.description = label(github::string(item, "description"), 1_024);
        candidate.stars = item["stars"].as_u64();
        results.push(candidate);
    }
    Ok(results)
}

pub(super) fn test(operation: &Operation<'_>, profile: &ProviderProfile) -> Result<(), String> {
    let body = search_response(operation, profile, "git", 1)?;
    body["data"]["skills"]
        .as_array()
        .map(drop)
        .ok_or("SkillsMP returned no skill list.".into())
}

fn search_response(
    operation: &Operation<'_>,
    profile: &ProviderProfile,
    query: &str,
    limit: usize,
) -> Result<Value, String> {
    let base = profile.base()?.ok_or("Missing SkillsMP API URL.")?;
    let mut url = github::api_url(&base, &["api", "v1", "skills", "search"])?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("limit", &limit.to_string());
    let body = operation.json(&url, Some(profile), "application/json")?;
    if body["success"].as_bool() == Some(false) {
        return Err("SkillsMP rejected the search.".into());
    }
    Ok(body)
}
