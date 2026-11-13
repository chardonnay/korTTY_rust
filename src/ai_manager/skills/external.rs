//! External skill discovery and explicitly reviewed library changes.

use std::{collections::HashSet, fmt};

use serde::{Deserialize, Serialize};
use url::Url;

use super::{Skill, Store};

mod github;
mod http;
mod skillsmp;
#[cfg(test)]
mod tests;

const MAX_URL_BYTES: usize = 2_048;
const MAX_MARKDOWN_BYTES: usize = 96 * 1024;
const MAX_QUERY_BYTES: usize = 200;
pub(super) const INITIAL_PROVIDER_ID: u64 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Provider {
    #[default]
    GitHub,
    SkillsMp,
    Http,
}

impl Provider {
    pub(super) const ALL: [Self; 3] = [Self::GitHub, Self::SkillsMp, Self::Http];

    fn default_base(self) -> Option<&'static str> {
        match self {
            Self::GitHub => Some(github::API),
            Self::SkillsMp => Some(skillsmp::API),
            Self::Http => None,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GitHub => "GitHub",
            Self::SkillsMp => "SkillsMP",
            Self::Http => "Markdown URL",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProviderProfile {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) provider: Provider,
    pub(crate) base_url: String,
    pub(crate) credential_env: String,
    pub(crate) username: String,
}

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            enabled: true,
            provider: Provider::default(),
            base_url: String::new(),
            credential_env: String::new(),
            username: String::new(),
        }
    }
}

impl ProviderProfile {
    pub(crate) const MAX_PROFILES: usize = 16;

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.id == 0
            || self.name.trim().is_empty()
            || self.name.len() > 120
            || self.name.chars().any(char::is_control)
            || self.username.len() > 128
            || self.username.chars().any(char::is_control)
            || self.username.contains(':')
        {
            return Err("Enter a valid external skill provider name and username.".into());
        }
        let base = self.base()?;
        let credential = self.credential_name();
        crate::local_ai::validate_credential(credential)?;
        if credential.is_some() && base.is_none()
            || !self.username.is_empty()
                && (self.provider != Provider::Http || credential.is_none())
        {
            return Err(
                "Set a provider URL and credential variable before enabling authentication.".into(),
            );
        }
        Ok(())
    }

    fn credential_name(&self) -> Option<&str> {
        (!self.credential_env.is_empty()).then_some(self.credential_env.as_str())
    }

    fn base(&self) -> Result<Option<Url>, String> {
        if self.base_url.trim().is_empty() {
            self.provider.default_base().map(parse_url).transpose()
        } else {
            parse_url(self.base_url.trim()).map(Some)
        }
    }
}

pub(super) fn default_profiles() -> Vec<ProviderProfile> {
    Provider::ALL
        .into_iter()
        .enumerate()
        .map(|(index, provider)| ProviderProfile {
            id: index as u64 + 1,
            name: provider.to_string(),
            provider,
            ..ProviderProfile::default()
        })
        .collect()
}

pub(super) fn is_default_profiles(profiles: &[ProviderProfile]) -> bool {
    profiles == default_profiles()
}

pub(super) fn is_initial_provider_id(value: &u64) -> bool {
    *value == INITIAL_PROVIDER_ID
}

pub(super) fn validate_profiles(profiles: &[ProviderProfile]) -> Result<(), String> {
    if profiles.len() > ProviderProfile::MAX_PROFILES {
        return Err("The external skill provider limit was reached.".into());
    }
    let mut ids = HashSet::new();
    for profile in profiles {
        profile.validate()?;
        if !ids.insert(profile.id) {
            return Err("An external skill provider ID is duplicated.".into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    provider_id: Option<u64>,
    base: Option<String>,
    location: Location,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Location {
    Http(String),
    GitHub(github::File),
}

impl Route {
    fn validate(&self) -> Result<(), String> {
        if self.provider_id == Some(0) {
            return Err("The external skill source has an invalid provider.".into());
        }
        let base = self.base.as_deref().map(parse_url).transpose()?;
        match &self.location {
            Location::Http(url) => {
                let url = parse_url(url)?;
                if self.provider_id.is_none()
                    || base.as_ref().is_some_and(|base| !within(&url, base))
                {
                    return Err("The external skill URL is outside its provider scope.".into());
                }
            }
            Location::GitHub(file) => {
                file.validate()?;
                if base.is_none() || self.provider_id.is_none() && !self.is_public_github() {
                    return Err("The GitHub skill source has an invalid API URL.".into());
                }
            }
        }
        Ok(())
    }

    fn is_public_github(&self) -> bool {
        self.base.as_deref() == Some(github::API)
    }

    fn source_url(&self) -> Result<String, String> {
        match &self.location {
            Location::Http(url) => Ok(url.clone()),
            Location::GitHub(file) => {
                let base = self.base.as_deref().ok_or("Missing GitHub API URL.")?;
                github::file_url(&parse_url(base)?, file)
            }
        }
    }

    fn current<'a>(
        &self,
        profiles: &'a [ProviderProfile],
    ) -> Result<Option<&'a ProviderProfile>, String> {
        let Some(id) = self.provider_id else {
            self.validate()?;
            return Ok(None);
        };
        let profile = selected_profile(profiles, id)?;
        let expected = match self.location {
            Location::Http(_) => Provider::Http,
            Location::GitHub(_) => Provider::GitHub,
        };
        if profile.provider != expected
            || profile.base()?.as_ref().map(normalized_base) != self.base
        {
            return Err("The saved provider URL or type changed. Review the source again.".into());
        }
        self.validate()?;
        Ok(Some(profile))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    route: Route,
    pub(crate) source_url: String,
    pub(crate) revision: String,
    content_sha256: String,
    description_sha256: String,
    skipped_revision: Option<String>,
    imported_at: i64,
}

impl Source {
    pub(super) fn bytes(&self) -> usize {
        self.source_url.len()
            + self.route.base.as_ref().map_or(0, String::len)
            + match &self.route.location {
                Location::Http(url) => url.len(),
                Location::GitHub(file) => file.bytes(),
            }
            + self.revision.len()
            + self.content_sha256.len()
            + self.description_sha256.len()
            + self.skipped_revision.as_ref().map_or(0, String::len)
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        self.route.validate()?;
        if self.source_url != self.route.source_url()?
            || !valid_revision(&self.revision)
            || self
                .skipped_revision
                .as_deref()
                .is_some_and(|value| !valid_revision(value))
            || !hex_value(&self.content_sha256, 64)
            || !hex_value(&self.description_sha256, 64)
            || self.imported_at <= 0
        {
            return Err("The external skill revision or source metadata is invalid.".into());
        }
        Ok(())
    }

    pub(crate) fn provider_id(&self) -> Option<u64> {
        self.route.provider_id
    }

    pub(crate) fn locally_modified(&self, skill: &Skill) -> bool {
        sha256(skill.content.as_bytes()).as_ref() != Ok(&self.content_sha256)
            || sha256(skill.description.as_bytes()).as_ref() != Ok(&self.description_sha256)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub(crate) reference: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) author: String,
    pub(crate) source_url: String,
    pub(crate) stars: Option<u64>,
    route: Route,
    catalogue: ProviderProfile,
    resolve_repository: bool,
}

impl Candidate {
    fn new(
        route: Route,
        catalogue: &ProviderProfile,
        name: &str,
        author: &str,
    ) -> Result<Self, String> {
        route.validate()?;
        let source_url = route.source_url()?;
        Ok(Self {
            reference: source_url.clone(),
            source_url,
            name: label(name, 256),
            description: String::new(),
            author: label(author, 160),
            stars: None,
            route,
            catalogue: catalogue.clone(),
            resolve_repository: false,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Document {
    pub(crate) skill: Skill,
    pub(crate) source_url: String,
    pub(crate) revision: String,
    provider: Option<ProviderProfile>,
    catalogue: Option<ProviderProfile>,
}

impl Document {
    fn new(
        route: Route,
        provider: Option<&ProviderProfile>,
        catalogue: Option<&ProviderProfile>,
        name: &str,
        bytes: Vec<u8>,
        revision: String,
    ) -> Result<Self, String> {
        if bytes.len() > MAX_MARKDOWN_BYTES {
            return Err("The external Markdown file exceeds its size limit.".into());
        }
        let markdown = String::from_utf8(bytes)
            .map_err(|_| "The external Markdown file is not valid UTF-8.")?;
        if let Some(profile) = provider {
            http::require_no_credential_echo(profile, &markdown)?;
        }
        let mut skill = Skill::parse(&markdown, name, false)?;
        if skill.content.trim().is_empty() {
            return Err("The external Markdown file does not contain skill instructions.".into());
        }
        skill.enabled = false;
        skill.target = super::Target::Both;
        skill.origin = None;
        skill.created_at = None;
        skill.rules.clear();
        let source_url = route.source_url()?;
        skill.external = Some(Source {
            route,
            source_url: source_url.clone(),
            revision: revision.clone(),
            content_sha256: sha256(skill.content.as_bytes())?,
            description_sha256: sha256(skill.description.as_bytes())?,
            skipped_revision: None,
            imported_at: jiff::Timestamp::now().as_second(),
        });
        skill.check()?;
        Ok(Self {
            skill,
            source_url,
            revision,
            provider: provider.cloned(),
            catalogue: catalogue.cloned(),
        })
    }

    fn source(&self) -> Result<&Source, String> {
        self.skill
            .external
            .as_ref()
            .ok_or("The reviewed external skill source is missing.".into())
    }

    fn require_current(&self, store: &Store) -> Result<(), String> {
        http::require_policy()?;
        if self.source()?.route.current(&store.external_providers)? != self.provider.as_ref()
            || self.catalogue.as_ref().is_some_and(|old| {
                selected_profile(&store.external_providers, old.id).ok() != Some(old)
            })
        {
            return Err(
                "The external skill provider changed. Download and review it again.".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Update {
    pub(crate) id: u64,
    pub(crate) original: Skill,
    pub(crate) latest: Document,
    pub(crate) can_replace: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Decision {
    Replace,
    Copy,
    Skip,
}

pub(crate) struct Client {
    profiles: Vec<ProviderProfile>,
    catalogue: ProviderProfile,
    network: http::Network,
}

pub(crate) use http::Cancellation;

impl Client {
    pub(crate) fn new(profiles: &[ProviderProfile], provider_id: u64) -> Result<Self, String> {
        let catalogue = selected_profile(profiles, provider_id)?.clone();
        Ok(Self {
            profiles: profiles.to_vec(),
            catalogue,
            network: http::Network::new()?,
        })
    }

    pub(crate) fn cancel_handle(&self) -> Cancellation {
        self.network.cancel_handle()
    }

    pub(crate) fn search(&self, query: &str) -> Result<Vec<Candidate>, String> {
        http::require_policy()?;
        let operation = self.network.operation();
        match self.catalogue.provider {
            Provider::Http => Ok(vec![self.http_candidate(query)?]),
            Provider::GitHub => github::search(&operation, &self.catalogue, query),
            Provider::SkillsMp => {
                skillsmp::search(&operation, &self.catalogue, &self.profiles, query)
            }
        }
    }

    pub(crate) fn fetch(&self, candidate: &Candidate) -> Result<Document, String> {
        if candidate.catalogue != self.catalogue {
            return Err("The selected skill provider changed. Search again.".into());
        }
        let operation = self.network.operation();
        let mut route = candidate.route.clone();
        if candidate.resolve_repository
            && let Location::GitHub(file) = &route.location
        {
            let base = parse_url(route.base.as_deref().ok_or("Missing GitHub API URL.")?)?;
            let download = route.current(&self.profiles)?;
            route.location = Location::GitHub(github::resolve_repository(
                &operation, &base, download, file,
            )?);
        }
        fetch_route(&operation, &route, &self.profiles, Some(&self.catalogue))
    }

    pub(crate) fn test(&self, query: &str) -> Result<(), String> {
        let operation = self.network.operation();
        match self.catalogue.provider {
            Provider::Http => self.fetch(&self.http_candidate(query)?).map(drop),
            Provider::GitHub => github::test(&operation, &self.catalogue),
            Provider::SkillsMp => skillsmp::test(&operation, &self.catalogue),
        }
    }

    fn http_candidate(&self, query: &str) -> Result<Candidate, String> {
        let base = self.catalogue.base()?;
        let value = query.trim();
        let url = if value.is_empty() {
            base.clone().ok_or("Enter a Markdown URL.")?
        } else if value.contains("://") {
            parse_url(value)?
        } else {
            let base = base.as_ref().ok_or("Enter a full Markdown URL.")?;
            let mut directory = base.clone();
            directory
                .path_segments_mut()
                .map_err(|_| "Invalid provider URL.")?
                .pop_if_empty()
                .push("");
            parse_url(
                directory
                    .join(value)
                    .map_err(|_| "Invalid skill URL.")?
                    .as_str(),
            )?
        };
        let name = http_name(&url);
        Candidate::new(
            Route {
                provider_id: Some(self.catalogue.id),
                base: base.as_ref().map(normalized_base),
                location: Location::Http(url.to_string()),
            },
            &self.catalogue,
            &name,
            "",
        )
    }
}

fn fetch_route(
    operation: &http::Operation<'_>,
    route: &Route,
    profiles: &[ProviderProfile],
    catalogue: Option<&ProviderProfile>,
) -> Result<Document, String> {
    let mut route = route.clone();
    let profile = route.current(profiles)?;
    let (name, bytes, revision) = match &mut route.location {
        Location::Http(url) => {
            let url = parse_url(url)?;
            let bytes = operation.get(
                &url,
                profile,
                "text/markdown, text/plain",
                MAX_MARKDOWN_BYTES,
            )?;
            let revision = sha256(&bytes)?;
            (http_name(&url), bytes, revision)
        }
        Location::GitHub(file) => {
            let base = parse_url(route.base.as_deref().ok_or("Missing GitHub API URL.")?)?;
            github::pin_branch(operation, &base, profile, file)?;
            github::fetch(operation, &base, profile, file)?
        }
    };
    Document::new(route, profile, catalogue, &name, bytes, revision)
}

pub(crate) struct UpdateClient {
    profiles: Vec<ProviderProfile>,
    network: http::Network,
}

impl UpdateClient {
    pub(crate) fn new(profiles: &[ProviderProfile]) -> Result<Self, String> {
        Ok(Self {
            profiles: profiles.to_vec(),
            network: http::Network::new()?,
        })
    }

    pub(crate) fn cancel_handle(&self) -> Cancellation {
        self.network.cancel_handle()
    }

    pub(crate) fn check(&self, id: u64, skill: &Skill) -> Result<Option<Update>, String> {
        let source = skill
            .external
            .as_ref()
            .ok_or("This skill has no external source.")?;
        let latest = fetch_route(
            &self.network.operation(),
            &source.route,
            &self.profiles,
            None,
        )?;
        if latest.revision == source.revision
            || source.skipped_revision.as_ref() == Some(&latest.revision)
        {
            return Ok(None);
        }
        Ok(Some(Update {
            id,
            original: skill.clone(),
            latest,
            can_replace: !source.locally_modified(skill),
        }))
    }
}

impl Store {
    pub(crate) fn next_external_provider_id(&mut self) -> Result<u64, String> {
        self.migrate_ids();
        let id = self
            .external_provider_last_id
            .checked_add(1)
            .ok_or("The external skill providers have no free IDs.")?;
        self.external_provider_last_id = id;
        Ok(id)
    }

    pub(crate) fn import_external(&mut self, document: &Document) -> Result<u64, String> {
        document.require_current(self)?;
        let route = &document.source()?.route;
        if self.skills.values().any(|skill| {
            skill.external.as_ref().is_some_and(|source| {
                source.route.base == route.base && source.route.location == route.location
            })
        }) {
            return Err(
                "This external skill is already imported. Check for updates instead.".into(),
            );
        }
        self.insert(document.skill.clone())
    }

    pub(crate) fn apply_external_update(
        &mut self,
        update: &Update,
        decision: Decision,
    ) -> Result<u64, String> {
        update.latest.require_current(self)?;
        let mut current = self
            .skills
            .get(&update.id)
            .filter(|current| **current == update.original)
            .cloned()
            .ok_or("The skill changed after review. Check for updates again.")?;
        let source = current
            .external
            .as_ref()
            .ok_or("This skill has no external source.")?;
        let latest = update.latest.source()?;
        if source.route != latest.route
            || source.revision == latest.revision
            || source.skipped_revision.as_ref() == Some(&latest.revision)
        {
            return Err("The reviewed external skill revision changed.".into());
        }
        match decision {
            Decision::Skip => {
                current
                    .external
                    .as_mut()
                    .expect("verified source")
                    .skipped_revision = Some(latest.revision.clone());
            }
            Decision::Copy => {
                let mut copy = update.latest.skill.clone();
                let name = &current.name[..current
                    .name
                    .floor_char_boundary(current.name.len().min(246))];
                copy.name = format!("{name} ({})", &latest.revision[..7]);
                copy.external = None;
                return self.insert(copy);
            }
            Decision::Replace => {
                if source.locally_modified(&current) {
                    return Err("The imported text or description was edited locally. Save an inactive copy or skip this revision.".into());
                }
                current.content.clone_from(&update.latest.skill.content);
                if !update.latest.skill.description.is_empty() {
                    current
                        .description
                        .clone_from(&update.latest.skill.description);
                }
                let mut source = latest.clone();
                source.description_sha256 = sha256(current.description.as_bytes())?;
                current.external = Some(source);
            }
        }
        self.replace(update.id, current)?;
        Ok(update.id)
    }
}

fn selected_profile(profiles: &[ProviderProfile], id: u64) -> Result<&ProviderProfile, String> {
    let profile = profiles
        .iter()
        .find(|profile| profile.id == id && profile.enabled)
        .ok_or("The external skill provider is unavailable or disabled.")?;
    profile.validate()?;
    Ok(profile)
}

fn parse_url(value: &str) -> Result<Url, String> {
    let invalid = || {
        "Enter an HTTPS URL without credentials, a query or fragment. HTTP requires a numeric loopback address.".to_owned()
    };
    if value.len() > MAX_URL_BYTES
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\')
        || value.split_once("://").is_none_or(|(_, rest)| {
            rest.split('/')
                .next()
                .is_none_or(|authority| authority.contains('@'))
        })
    {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    let allowed_scheme = match url.scheme() {
        "https" => true,
        "http" => loopback,
        _ => false,
    };
    if url.host().is_none()
        || !allowed_scheme
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || !unambiguous_path(url.path())
    {
        return Err(invalid());
    }
    Ok(url)
}

fn normalized_base(url: &Url) -> String {
    url.as_str().trim_end_matches('/').into()
}

fn within(url: &Url, base: &Url) -> bool {
    let path = base.path().trim_end_matches('/');
    url.origin() == base.origin()
        && unambiguous_path(url.path())
        && (url.path() == path
            || url
                .path()
                .strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('/')))
}

fn unambiguous_path(path: &str) -> bool {
    let Ok(decoded) = percent_encoding::percent_decode_str(path).decode_utf8() else {
        return false;
    };
    !decoded.chars().any(|ch| ch.is_control() || ch == '\\')
        && decoded
            .split('/')
            .all(|segment| !matches!(segment, "." | ".."))
        && percent_encoding::percent_decode_str(&decoded)
            .decode_utf8()
            .is_ok_and(|again| again == decoded)
}

fn http_name(url: &Url) -> String {
    let mut parts = url.path().rsplit('/').filter(|part| !part.is_empty());
    let name = parts.next().unwrap_or("External skill");
    let decoded = percent_encoding::percent_decode_str(name).decode_utf8_lossy();
    if decoded.eq_ignore_ascii_case("SKILL.md")
        && let Some(parent) = parts.next()
    {
        return label(
            &percent_encoding::percent_decode_str(parent).decode_utf8_lossy(),
            256,
        );
    }
    let name = decoded
        .get(decoded.len().saturating_sub(3)..)
        .filter(|suffix| suffix.eq_ignore_ascii_case(".md"))
        .map_or(decoded.as_ref(), |_| &decoded[..decoded.len() - 3]);
    label(name, 256)
}

fn label(text: &str, max: usize) -> String {
    let mut result = String::new();
    for ch in text.trim().chars().filter(|ch| !ch.is_control()) {
        if result.len() + ch.len_utf8() > max {
            break;
        }
        result.push(ch);
    }
    result
}

fn valid_revision(value: &str) -> bool {
    hex_value(value, 40) || hex_value(value, 64)
}

fn hex_value(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256(bytes: &[u8]) -> Result<String, String> {
    pgp::crypto::hash::HashAlgorithm::Sha256
        .digest(bytes)
        .map(|digest| digest.iter().map(|byte| format!("{byte:02x}")).collect())
        .map_err(|_| "Could not verify the external skill revision.".into())
}
