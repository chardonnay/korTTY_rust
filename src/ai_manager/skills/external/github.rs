//! GitHub repository, directory, and code-search routes for SKILL.md.

use std::collections::HashSet;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::{
    Candidate, Location, MAX_MARKDOWN_BYTES, MAX_QUERY_BYTES, Provider, ProviderProfile, Route,
    http::Operation, normalized_base, parse_url, valid_revision, within,
};

pub(super) const API: &str = "https://api.github.com";
pub(super) const ACCEPT: &str = "application/vnd.github+json";
const WEB: &str = "https://github.com";
const SKILL: &str = "SKILL.md";
const MAX_SKILLS: usize = 300;
const INVALID: &str = "Enter a GitHub repository, skill directory, or SKILL.md reference.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct File {
    owner: String,
    repository: String,
    branch: Option<String>,
    directory: String,
}

impl File {
    pub(super) fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.owner)
            || !valid_name(&self.repository)
            || self
                .branch
                .as_deref()
                .is_some_and(|branch| !valid_branch(branch))
            || !valid_directory(&self.directory)
        {
            return Err(INVALID.into());
        }
        Ok(())
    }

    pub(super) fn bytes(&self) -> usize {
        self.owner.len()
            + self.repository.len()
            + self.branch.as_ref().map_or(0, String::len)
            + self.directory.len()
    }

    fn path(&self) -> String {
        if self.directory.is_empty() {
            SKILL.into()
        } else {
            format!("{}/{SKILL}", self.directory)
        }
    }

    fn name(&self) -> &str {
        self.directory
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.repository)
    }
}

struct Reference {
    owner: String,
    repository: String,
    branch: Option<String>,
    directory: Option<String>,
    named: Option<String>,
}

impl Reference {
    fn parse(query: &str, base: &Url) -> Result<Self, String> {
        let (reference, named) = command_reference(query)?;
        let mut parsed = if reference.contains("://") {
            Self::web(reference, base)?
        } else {
            Self::short(reference)?
        };
        if let Some(named) = named {
            if parsed.directory.is_some() || parsed.named.is_some() {
                return Err(INVALID.into());
            }
            parsed.named = Some(named.into());
        }
        if parsed
            .named
            .as_deref()
            .is_some_and(|name| !valid_named_skill(name))
        {
            return Err(INVALID.into());
        }
        parsed
            .file(parsed.directory.as_deref().unwrap_or_default())
            .validate()?;
        Ok(parsed)
    }

    fn web(reference: &str, base: &Url) -> Result<Self, String> {
        let url = parse_url(reference)?;
        let web = web_base(base)?;
        if !within(&url, &web) {
            return Err("The GitHub reference is outside the selected provider.".into());
        }
        let path = url
            .path()
            .strip_prefix(web.path().trim_end_matches('/'))
            .ok_or(INVALID)?;
        let segments: Vec<_> = path
            .trim_matches('/')
            .split('/')
            .map(|segment| percent_encoding::percent_decode_str(segment).decode_utf8())
            .collect::<Result<_, _>>()
            .map_err(|_| INVALID)?;
        let [owner, repo, rest @ ..] = segments.as_slice() else {
            return Err(INVALID.into());
        };
        let (branch, directory) = match rest {
            [] => (None, None),
            [kind, branch, tail @ ..] if matches!(kind.as_ref(), "tree" | "blob") => {
                let path = (!tail.is_empty()).then(|| tail.join("/"));
                (
                    Some(branch.to_string()),
                    path.map(|path| strip_skill(&path).to_owned()),
                )
            }
            _ => return Err(INVALID.into()),
        };
        Ok(Self::new(owner, repo, branch, directory, None))
    }

    fn short(reference: &str) -> Result<Self, String> {
        let (reference, named) = reference
            .rsplit_once('@')
            .map_or((reference, None), |(repo, name)| {
                (repo, Some(name.to_owned()))
            });
        let mut parts = reference.trim_matches('/').splitn(3, '/');
        let owner = parts.next().ok_or(INVALID)?;
        let repo = parts.next().ok_or(INVALID)?;
        let directory = parts.next().map(|path| strip_skill(path).to_owned());
        if directory.is_some() && named.is_some() {
            return Err(INVALID.into());
        }
        Ok(Self::new(owner, repo, None, directory, named))
    }

    fn new(
        owner: &str,
        repo: &str,
        branch: Option<String>,
        directory: Option<String>,
        named: Option<String>,
    ) -> Self {
        let repo = repo.to_ascii_lowercase();
        Self {
            owner: owner.to_ascii_lowercase(),
            repository: repo.strip_suffix(".git").unwrap_or(&repo).into(),
            branch: branch.filter(|branch| branch != "HEAD"),
            directory,
            named,
        }
    }

    fn file(&self, directory: &str) -> File {
        File {
            owner: self.owner.clone(),
            repository: self.repository.clone(),
            branch: self.branch.clone(),
            directory: directory.into(),
        }
    }
}

fn command_reference(query: &str) -> Result<(&str, Option<&str>), String> {
    let query = query.trim();
    let prefix = ["npx ", "bunx ", "pnpm dlx "]
        .into_iter()
        .find_map(|prefix| query.strip_prefix(prefix));
    let Some(command) = prefix else {
        return Ok((query, None));
    };
    let mut tokens = command.split_ascii_whitespace().peekable();
    if tokens.peek() == Some(&"-y") {
        tokens.next();
    }
    let tool = tokens.next().ok_or(INVALID)?;
    if tool.split('@').next() != Some("skills") || tokens.next() != Some("add") {
        return Err(INVALID.into());
    }
    let reference = tokens.next().ok_or(INVALID)?;
    let named = match tokens.next() {
        None => None,
        Some("--skill") => Some(tokens.next().ok_or(INVALID)?),
        _ => return Err(INVALID.into()),
    };
    if tokens.next().is_some() {
        return Err(INVALID.into());
    }
    Ok((reference, named))
}

pub(super) fn search(
    operation: &Operation<'_>,
    profile: &ProviderProfile,
    query: &str,
) -> Result<Vec<Candidate>, String> {
    let base = profile.base()?.ok_or("Missing GitHub API URL.")?;
    let query = query.trim();
    if query.is_empty() || query.len() > super::MAX_URL_BYTES || query.chars().any(char::is_control)
    {
        return Err(INVALID.into());
    }
    if !query.contains(['/', '@'])
        && !["npx ", "bunx ", "pnpm dlx "]
            .iter()
            .any(|prefix| query.starts_with(prefix))
    {
        return keyword_search(operation, profile, &base, query);
    }
    let mut reference = Reference::parse(query, &base)?;
    if reference.branch.is_none() {
        reference.branch = Some(default_branch(
            operation,
            &base,
            Some(profile),
            &reference.file(""),
        )?);
    }
    if let Some(directory) = &reference.directory {
        return candidate(profile, Some(profile), &base, reference.file(directory))
            .map(|item| vec![item]);
    }
    let body = read_tree(operation, &base, Some(profile), &reference.file(""))?;
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for directory in skill_directories(&body)? {
        if reference.named.as_deref().is_some_and(|name| {
            !directory
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .eq_ignore_ascii_case(name)
        }) || !seen.insert(directory)
        {
            continue;
        }
        candidates.push(candidate(
            profile,
            Some(profile),
            &base,
            reference.file(directory),
        )?);
        if candidates.len() == MAX_SKILLS {
            break;
        }
    }
    if candidates.is_empty() && body["truncated"].as_bool() == Some(true) {
        return Err("The GitHub tree was truncated. Enter the exact skill directory.".into());
    }
    Ok(candidates)
}

fn read_tree(
    operation: &Operation<'_>,
    base: &Url,
    profile: Option<&ProviderProfile>,
    file: &File,
) -> Result<Value, String> {
    let branch = file.branch.as_deref().ok_or("Missing GitHub branch.")?;
    let mut url = api_url(
        base,
        &[
            "repos",
            &file.owner,
            &file.repository,
            "git",
            "trees",
            branch,
        ],
    )?;
    url.query_pairs_mut().append_pair("recursive", "1");
    operation.json(&url, profile, ACCEPT)
}

fn default_branch(
    operation: &Operation<'_>,
    base: &Url,
    profile: Option<&ProviderProfile>,
    file: &File,
) -> Result<String, String> {
    let repo = operation.json(
        &api_url(base, &["repos", &file.owner, &file.repository])?,
        profile,
        ACCEPT,
    )?;
    let branch = string(&repo, "default_branch");
    if !valid_branch(branch) {
        return Err("GitHub returned an invalid default branch.".into());
    }
    Ok(branch.into())
}

pub(super) fn pin_branch(
    operation: &Operation<'_>,
    base: &Url,
    profile: Option<&ProviderProfile>,
    file: &mut File,
) -> Result<(), String> {
    if file.branch.is_none() {
        file.branch = Some(default_branch(operation, base, profile, file)?);
    }
    Ok(())
}

fn skill_directories(body: &Value) -> Result<impl Iterator<Item = &str>, String> {
    let entries = body["tree"]
        .as_array()
        .ok_or("GitHub returned no repository tree.")?;
    Ok(entries.iter().filter_map(|entry| {
        let path = string(entry, "path");
        if string(entry, "type") == "blob" && path.rsplit('/').next() == Some(SKILL) {
            let directory = strip_skill(path);
            return valid_directory(directory).then_some(directory);
        }
        None
    }))
}

pub(super) fn resolve_repository(
    operation: &Operation<'_>,
    base: &Url,
    profile: Option<&ProviderProfile>,
    file: &File,
) -> Result<File, String> {
    let mut file = file.clone();
    pin_branch(operation, base, profile, &mut file)?;
    let body = read_tree(operation, base, profile, &file)?;
    let mut directory = None;
    let mut multiple = false;
    for candidate in skill_directories(&body)? {
        if candidate.is_empty() {
            return Ok(file);
        }
        multiple |= directory.is_some_and(|current| current != candidate);
        directory = Some(candidate);
    }
    if multiple || body["truncated"].as_bool() == Some(true) {
        return Err("The GitHub repository needs an exact skill directory.".into());
    }
    file.directory = directory
        .ok_or("The GitHub repository contains no SKILL.md.")?
        .into();
    Ok(file)
}

fn keyword_search(
    operation: &Operation<'_>,
    profile: &ProviderProfile,
    base: &Url,
    query: &str,
) -> Result<Vec<Candidate>, String> {
    if query.len() > MAX_QUERY_BYTES {
        return Err("The GitHub skill search is too long.".into());
    }
    if profile.credential_name().is_none() {
        return Err(
            "GitHub keyword search requires a token. Repository references also work anonymously."
                .into(),
        );
    }
    let mut url = api_url(base, &["search", "code"])?;
    url.query_pairs_mut()
        .append_pair("q", &format!("{query} filename:{SKILL}"))
        .append_pair("per_page", "50");
    let body = operation.json(&url, Some(profile), ACCEPT)?;
    let items = body["items"]
        .as_array()
        .ok_or("GitHub returned no code search results.")?;
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for item in items.iter().take(50) {
        let path = string(item, "path");
        let repo = string(&item["repository"], "full_name");
        if path.rsplit('/').next() != Some(SKILL)
            || string(item, "name") != SKILL
            || repo.split('/').count() != 2
        {
            continue;
        }
        let Some((owner, repository)) = repo.split_once('/') else {
            continue;
        };
        let reference = Reference::new(owner, repository, None, None, None);
        let file = reference.file(strip_skill(path));
        if file.validate().is_ok() && seen.insert((repo.to_ascii_lowercase(), path)) {
            candidates.push(candidate(profile, Some(profile), base, file)?);
        }
    }
    Ok(candidates)
}

pub(super) fn marketplace_candidate(
    catalogue: &ProviderProfile,
    profiles: &[ProviderProfile],
    reference: &str,
) -> Result<Candidate, String> {
    let base = parse_url(API)?;
    let url = parse_url(reference)?;
    if url.scheme() != "https" || url.origin() != parse_url(WEB)?.origin() {
        return Err("The marketplace result is not a public GitHub skill.".into());
    }
    let reference = Reference::web(url.as_str(), &base)?;
    let download = profiles.iter().find(|profile| {
        profile.enabled
            && profile.provider == Provider::GitHub
            && profile
                .base()
                .ok()
                .flatten()
                .as_ref()
                .map(normalized_base)
                .as_deref()
                == Some(API)
    });
    let mut result = candidate(
        catalogue,
        download,
        &base,
        reference.file(reference.directory.as_deref().unwrap_or_default()),
    )?;
    if reference.directory.is_none() {
        result.resolve_repository = true;
        result.reference = url.to_string();
        result.source_url = result.reference.clone();
    }
    Ok(result)
}

fn candidate(
    catalogue: &ProviderProfile,
    download: Option<&ProviderProfile>,
    base: &Url,
    file: File,
) -> Result<Candidate, String> {
    let name = file.name().to_owned();
    let author = format!("{}/{}", file.owner, file.repository);
    Candidate::new(
        Route {
            provider_id: download.map(|profile| profile.id),
            base: Some(normalized_base(base)),
            location: Location::GitHub(file),
        },
        catalogue,
        &name,
        &author,
    )
}

pub(super) fn fetch(
    operation: &Operation<'_>,
    base: &Url,
    profile: Option<&ProviderProfile>,
    file: &File,
) -> Result<(String, Vec<u8>, String), String> {
    let path = file.path();
    let mut parts = vec!["repos", &file.owner, &file.repository, "contents"];
    parts.extend(path.split('/'));
    let mut url = api_url(base, &parts)?;
    if let Some(branch) = &file.branch {
        url.query_pairs_mut().append_pair("ref", branch);
    }
    let body = operation.json(&url, profile, ACCEPT)?;
    if string(&body, "type") != "file"
        || string(&body, "encoding") != "base64"
        || string(&body, "path") != path
    {
        return Err("GitHub did not return the requested Markdown file.".into());
    }
    let revision = string(&body, "sha");
    if !valid_revision(revision) {
        return Err("GitHub returned an invalid skill revision.".into());
    }
    let size = body["size"]
        .as_u64()
        .filter(|size| *size <= MAX_MARKDOWN_BYTES as u64)
        .ok_or("The GitHub skill file exceeds its size limit or did not report its size.")?;
    let encoded = string(&body, "content");
    if encoded.len() > MAX_MARKDOWN_BYTES * 2 {
        return Err("The GitHub skill file exceeds its size limit.".into());
    }
    let packed: Vec<_> = encoded
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let bytes = STANDARD
        .decode(&packed)
        .map_err(|_| "GitHub returned invalid skill content.")?;
    if bytes.len() as u64 != size {
        return Err("The GitHub skill file size did not match its metadata.".into());
    }
    Ok((file.name().into(), bytes, revision.to_ascii_lowercase()))
}

pub(super) fn test(operation: &Operation<'_>, profile: &ProviderProfile) -> Result<(), String> {
    let base = profile.base()?.ok_or("Missing GitHub API URL.")?;
    operation
        .json(&api_url(&base, &["rate_limit"])?, Some(profile), ACCEPT)
        .map(drop)
}

pub(super) fn file_url(base: &Url, file: &File) -> Result<String, String> {
    let path = file.path();
    let mut parts = vec![
        file.owner.as_str(),
        &file.repository,
        "blob",
        file.branch.as_deref().unwrap_or("HEAD"),
    ];
    parts.extend(path.split('/'));
    api_url(&web_base(base)?, &parts).map(|url| url.to_string())
}

fn web_base(base: &Url) -> Result<Url, String> {
    if normalized_base(base) == API {
        return parse_url(WEB);
    }
    let mut web = base.clone();
    if let Some(path) = base.path().trim_end_matches('/').strip_suffix("/api/v3") {
        web.set_path(path);
    }
    Ok(web)
}

pub(super) fn api_url(base: &Url, parts: &[&str]) -> Result<Url, String> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| "Invalid external skill API URL.")?
        .pop_if_empty()
        .extend(parts.iter().copied());
    Ok(url)
}

pub(super) fn string<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name].as_str().unwrap_or_default()
}

fn strip_skill(path: &str) -> &str {
    if path == SKILL {
        return "";
    }
    path.strip_suffix("/SKILL.md").unwrap_or(path)
}

fn valid_directory(path: &str) -> bool {
    path.len() <= 1_024
        && (path.is_empty() || path.split('/').all(|part| !matches!(part, "" | "." | "..")))
        && !path.chars().any(|ch| ch.is_control() || ch == '\\')
}

fn valid_branch(branch: &str) -> bool {
    !branch.trim().is_empty()
        && branch.len() <= 240
        && !branch.contains("..")
        && !branch
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
}

fn valid_name(name: &str) -> bool {
    !matches!(name, "" | "." | "..")
        && name.len() <= 100
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_named_skill(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && valid_directory(name)
}
