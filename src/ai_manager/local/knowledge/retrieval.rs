//! Previewable, bounded knowledge retrieval for explicitly routed AI requests.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use serde::{Deserialize, Serialize};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{self, Client, Pending};
use crate::private_files::FileStamp;
use crate::vault_window::VaultWindow;

use super::super::Role;
use super::{Cache, Passage, Scope, check_active, qdrant};

pub(crate) const MAX_QUERY_BYTES: usize = 2048;
const MAX_CONTEXT_BYTES: usize = 4000;
const MAX_EXCERPT_BYTES: usize = 1024;
const MAX_EXCERPTS: usize = 6;
const PER_SOURCE: usize = 2;
const GRAPH_CANDIDATES: usize = 32;
const HEADER: &str = "\n\n<retrieved_context>\n";
const FOOTER: &str = "</retrieved_context>\n";

#[derive(Clone)]
pub(crate) struct Plan {
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    qdrant_destination: Option<String>,
    scope: Scope,
    cache: Cache,
    profile_root: PathBuf,
    saved_route: Option<SavedRoute>,
    standing_review: bool,
}

#[derive(Clone)]
enum SavedRoute {
    Assigned(Role, ProfileRef),
    Autonomous(ProfileRef),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KnowledgeAuthority {
    route: [u8; 32],
    snapshot: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault_envelope: Option<[u8; 32]>,
}

#[cfg(test)]
impl KnowledgeAuthority {
    pub(crate) fn fixture(snapshot: [u8; 32]) -> Self {
        Self {
            route: [0; 32],
            snapshot,
            vault_envelope: None,
        }
    }
}

impl Plan {
    pub(in crate::ai_manager::local) fn new(
        name: String,
        endpoint: String,
        model: String,
        scope: Scope,
        profile_root: PathBuf,
    ) -> Self {
        let qdrant_destination = scope
            .qdrant
            .as_ref()
            .map(super::qdrant::Settings::destination);
        Self {
            name,
            endpoint,
            model,
            qdrant_destination,
            scope,
            cache: Cache::new(profile_root.clone()),
            profile_root,
            saved_route: None,
            standing_review: false,
        }
    }

    pub(crate) fn qdrant_destination(&self) -> Option<&str> {
        self.qdrant_destination.as_deref()
    }

    pub(crate) fn supports_standing_review(&self) -> bool {
        matches!(self.saved_route, Some(SavedRoute::Autonomous(_)))
            && self
                .scope
                .qdrant
                .as_ref()
                .is_none_or(|settings| settings.credential_env.is_empty())
    }

    pub(crate) fn for_standing_review(mut self) -> Self {
        self.standing_review = true;
        self
    }

    fn standing_route(&self) -> Result<[u8; 32], String> {
        let Some(SavedRoute::Autonomous(reference)) = &self.saved_route else {
            return Err(standing_unsupported());
        };
        if !self.supports_standing_review() {
            return Err(standing_unsupported());
        }
        let encoded = serde_json::to_vec(&(
            "kortty autonomous knowledge route v1",
            reference,
            self.scope.digest,
            self.scope.enabled_sources(),
            &self.scope.approved,
        ))
        .map_err(|error| error.to_string())?;
        super::sha256(&encoded).map_err(|error| error.to_string())
    }

    pub(crate) fn require_authority(
        &self,
        authority: &KnowledgeAuthority,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.require_authority_with_policy(authority, check, ClipboardPolicy::load)
    }

    pub(super) fn require_authority_with_policy(
        &self,
        authority: &KnowledgeAuthority,
        check: impl Fn() -> Result<(), String>,
        load_policy: impl Fn() -> ClipboardPolicy,
    ) -> Result<(), String> {
        if self.standing_route()? != authority.route {
            return Err("The approved knowledge store or its settings changed.".into());
        }
        check()?;
        self.require_saved_route()?;
        self.require_standing_backend(authority.vault_envelope, &load_policy)?;
        let evidence = self.cache.authority_evidence(&self.scope, None, &check)?;
        if self.scope.qdrant.is_some() {
            super::remote_index::require_receipt_coverage(evidence.receipt.as_ref(), &self.scope)?;
        }
        if evidence.digest != authority.snapshot {
            return Err(changed_index());
        }
        check()?;
        super::require_source_digests(&self.scope, &evidence.sources, &check)?;
        self.cache
            .require_current(&self.scope, &evidence.stamp)
            .map_err(|_| changed_index())?;
        self.require_saved_route()?;
        self.require_standing_backend(authority.vault_envelope, load_policy)?;
        check()
    }

    fn require_standing_backend(
        &self,
        vault_envelope: Option<[u8; 32]>,
        load_policy: impl FnOnce() -> ClipboardPolicy,
    ) -> Result<(), String> {
        if let Some(settings) = &self.scope.qdrant {
            super::remote_index::require_policy_for(&load_policy())?;
            settings.require_standing_envelope(&self.profile_root, vault_envelope)
        } else if vault_envelope.is_some() {
            Err(standing_unsupported())
        } else {
            Ok(())
        }
    }

    pub(in crate::ai_manager) fn with_saved_route(
        mut self,
        role: Role,
        profile: ProfileRef,
    ) -> Self {
        self.saved_route = Some(SavedRoute::Assigned(role, profile));
        self
    }

    pub(in crate::ai_manager) fn with_autonomous_route(mut self, profile: ProfileRef) -> Self {
        self.saved_route = Some(SavedRoute::Autonomous(profile));
        self
    }

    fn require_saved_route(&self) -> Result<(), String> {
        self.scope.profile.require_current_policy_authority()?;
        let Some(route) = &self.saved_route else {
            return Ok(());
        };
        let changed = || {
            "The saved knowledge settings changed. Retrieve and review the excerpts again."
                .to_owned()
        };
        let current = Configuration::load_from(&self.profile_root).map_err(|_| changed())?;
        let reference = match route {
            SavedRoute::Assigned(_, reference) | SavedRoute::Autonomous(reference) => reference,
        };
        let profile = current
            .resolve_profile_reference(reference)
            .ok_or_else(changed)?;
        let plans = match route {
            SavedRoute::Assigned(role, _) => {
                current.knowledge_for(*role, profile, &self.profile_root)
            }
            SavedRoute::Autonomous(_) => current.autonomous_knowledge(profile, &self.profile_root),
        };
        if current.enabled() && plans.into_iter().any(|plan| plan.scope == self.scope) {
            Ok(())
        } else {
            Err(changed())
        }
    }

    pub(crate) fn start(
        self,
        query: String,
        limit: usize,
        vault: Option<&VaultWindow>,
    ) -> Result<Pending<Retrieved>, String> {
        self.start_masked(query, limit, vault, |field| Ok(field.to_owned()))
    }

    pub(crate) fn start_masked(
        self,
        query: String,
        limit: usize,
        vault: Option<&VaultWindow>,
        mask: impl Fn(&str) -> Result<String, String> + Send + 'static,
    ) -> Result<Pending<Retrieved>, String> {
        self.start_masked_guarded(query, limit, vault, mask, || Ok(()))
    }

    pub(crate) fn start_masked_guarded(
        self,
        query: String,
        limit: usize,
        vault: Option<&VaultWindow>,
        mask: impl Fn(&str) -> Result<String, String> + Send + 'static,
        authorize: impl Fn() -> Result<(), String> + Send + 'static,
    ) -> Result<Pending<Retrieved>, String> {
        if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES || query.contains('\0') {
            return Err("Enter a valid knowledge retrieval query.".into());
        }
        if self.standing_review && !self.supports_standing_review() {
            return Err(standing_unsupported());
        }
        authorize()?;
        self.require_saved_route()?;
        let vault_key = match &self.scope.qdrant {
            Some(settings) if self.standing_review => settings.resolve_standing_vault_key(vault)?,
            Some(settings) => settings.resolve_vault_key(vault, None)?,
            None => None,
        };
        local_ai::start(self.scope.profile.clone(), move |client| {
            self.retrieve(client, &query, limit, vault_key, mask, authorize)
        })
    }

    fn retrieve(
        self,
        client: &Client,
        query: &str,
        limit: usize,
        vault_key: Option<Arc<qdrant::VaultKey>>,
        mask: impl Fn(&str) -> Result<String, String>,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<Retrieved, String> {
        check_active(client)?;
        authorize()?;
        self.require_saved_route()?;
        let budget = (limit / 4).min(MAX_CONTEXT_BYTES);
        if budget < 256 {
            return Err("The profile selection limit is too small for retrieved context.".into());
        }
        let (index, stamp) = self.cache.load_stamped(&self.scope).map_err(|error| {
            format!("Could not open the knowledge index ({error}). Update it in AI Manager.")
        })?;
        let store = mask(&self.name)?;
        let vault_envelope = vault_key
            .as_deref()
            .and_then(qdrant::VaultKey::standing_envelope);
        let before_dispatch = || {
            self.require_saved_route()?;
            if self.standing_review {
                self.require_standing_backend(vault_envelope, ClipboardPolicy::load)?;
            }
            self.cache
                .require_current(&self.scope, &stamp)
                .map_err(|_| changed_index())?;
            authorize()
        };
        let vector = index.query_vector(
            client,
            &self.scope,
            query,
            vault_key.as_deref(),
            before_dispatch,
        )?;
        let vault_revision = vault_key.as_ref().map(|key| key.revision());
        let enabled = index.enabled_entries(&self.scope);
        let search_graph = self.scope.qdrant.is_none() && index.search_graph(enabled).is_some();
        let candidate_limit = if search_graph {
            GRAPH_CANDIDATES
        } else {
            enabled
        };
        let check = || {
            check_active(client)?;
            vault_revision
                .as_ref()
                .map_or(Ok(()), qdrant::require_vault_revision)
        };
        let candidates = index.rank_reviewed(
            client,
            &self.scope,
            &vector,
            candidate_limit,
            vault_key,
            before_dispatch,
        )?;
        let mut excerpts = Excerpts {
            scope: &self.scope,
            store,
            budget,
            context: String::from(HEADER),
            citations: Vec::new(),
            source_counts: vec![0; self.scope.sources.len()],
        };
        for &(_, position) in &candidates {
            excerpts.add(&index.entries[position].0, &mask)?;
            if excerpts.full() {
                break;
            }
        }
        // Size and per-source limits can skip the best graph hits. Refill from
        // the remaining enabled passages if they can still provide context.
        if search_graph && candidates.len() < enabled && !excerpts.full() {
            let mut visited = vec![false; index.len()];
            for &(_, position) in &candidates {
                visited[position] = true;
            }
            for (_, position) in index.rank_exact(&self.scope, &vector, check)? {
                if !visited[position] {
                    excerpts.add(&index.entries[position].0, &mask)?;
                    if excerpts.full() {
                        break;
                    }
                }
            }
        }
        let Excerpts {
            mut context,
            citations,
            ..
        } = excerpts;
        if citations.is_empty() {
            return Err("No retrieved excerpts fit the profile selection limit.".into());
        }
        context.push_str(FOOTER);
        let authority = self
            .standing_review
            .then(|| {
                let route = self.standing_route()?;
                let snapshot = self
                    .cache
                    .digest_current(&self.scope, &stamp, || check_active(client))?;
                Ok::<_, String>(KnowledgeAuthority {
                    route,
                    snapshot,
                    vault_envelope,
                })
            })
            .transpose()?;
        let result = Retrieved {
            plan: self,
            context,
            citations,
            sources: index.sources,
            stamp,
            vault_revision,
            authority,
        };
        result.require_current_guarded(|| {
            check_active(client)?;
            authorize()
        })?;
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) fn index_for_test(&self) -> Result<(), String> {
        let client = Client::new(self.scope.profile.clone());
        let index = super::Index::build(&client, &self.scope)?;
        self.cache
            .save(&index, || Ok(()))
            .map_err(|error| error.to_string())
    }
}

pub(super) fn changed_index() -> String {
    "The knowledge index changed. Retrieve and review the excerpts again.".into()
}

fn standing_unsupported() -> String {
    "Scheduled knowledge reviews require an opted-in local or Qdrant index without an environment API key.".into()
}

struct Excerpts<'a> {
    scope: &'a Scope,
    store: String,
    budget: usize,
    context: String,
    citations: Vec<Citation>,
    source_counts: Vec<usize>,
}

impl Excerpts<'_> {
    fn full(&self) -> bool {
        self.citations.len() == MAX_EXCERPTS
            || self
                .scope
                .enabled
                .iter()
                .zip(&self.source_counts)
                .all(|(&enabled, &count)| !enabled || count == PER_SOURCE)
    }

    fn add(
        &mut self,
        passage: &Passage,
        mask: &impl Fn(&str) -> Result<String, String>,
    ) -> Result<(), String> {
        if self.source_counts[passage.source] == PER_SOURCE {
            return Ok(());
        }
        let file = self.scope.sources[passage.source]
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Source");
        let file = mask(file)?;
        let file = &file[..file.floor_char_boundary(512.min(file.len()))];
        let excerpt = mask(&passage.text)?;
        let excerpt = &excerpt[..excerpt.floor_char_boundary(MAX_EXCERPT_BYTES.min(excerpt.len()))];
        let (line, page) = match passage.location {
            super::Location::Line(line) => (Some(line), None),
            super::Location::Page(page) => (None, Some(page)),
        };
        let row = encode(&Excerpt {
            citation: format!("[R{}]", self.citations.len() + 1),
            store: &self.store,
            file,
            line,
            page,
            excerpt,
        })?;
        if self.context.len() + row.len() + 1 + FOOTER.len() <= self.budget {
            self.context.push_str(&row);
            self.context.push('\n');
            self.source_counts[passage.source] += 1;
            self.citations.push(Citation {
                store: self.store.clone(),
                file: file.into(),
                location: passage.location,
            });
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Excerpt<'a> {
    citation: String,
    store: &'a str,
    file: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    excerpt: &'a str,
}

fn encode(excerpt: &Excerpt<'_>) -> Result<String, String> {
    let json = serde_json::to_string(excerpt).map_err(|error| error.to_string())?;
    // Keep markup from knowledge files inside their JSON string fields.
    Ok(json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

pub(crate) struct Retrieved {
    plan: Plan,
    context: String,
    citations: Vec<Citation>,
    sources: Vec<Option<[u8; 32]>>,
    stamp: FileStamp,
    vault_revision: Option<Weak<()>>,
    authority: Option<KnowledgeAuthority>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "CitationWire", into = "CitationWire")]
pub(crate) struct Citation {
    pub(crate) store: String,
    pub(crate) file: String,
    location: super::Location,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CitationWire {
    store: String,
    file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
}

impl TryFrom<CitationWire> for Citation {
    type Error = &'static str;

    fn try_from(wire: CitationWire) -> Result<Self, Self::Error> {
        let location = match (wire.line, wire.page) {
            (Some(line), None) if (1..=super::source::MAX_TEXT_BYTES).contains(&line) => {
                super::Location::Line(line)
            }
            (None, Some(page)) if (1..=super::pdf::MAX_PAGES).contains(&page) => {
                super::Location::Page(page)
            }
            _ => return Err("Invalid knowledge citation location"),
        };
        Ok(Self {
            store: wire.store,
            file: wire.file,
            location,
        })
    }
}

impl From<Citation> for CitationWire {
    fn from(citation: Citation) -> Self {
        let (line, page) = match citation.location {
            super::Location::Line(line) => (Some(line), None),
            super::Location::Page(page) => (None, Some(page)),
        };
        Self {
            store: citation.store,
            file: citation.file,
            line,
            page,
        }
    }
}

impl Citation {
    pub(crate) fn location(&self) -> impl fmt::Display + '_ {
        self.location
    }

    pub(crate) fn valid_list(citations: &[Self]) -> bool {
        citations.len() <= MAX_EXCERPTS
            && citations.iter().all(|citation| {
                (1..=128).contains(&citation.store.len())
                    && (1..=512).contains(&citation.file.len())
                    && !citation
                        .store
                        .chars()
                        .chain(citation.file.chars())
                        .any(char::is_control)
            })
    }
}

impl fmt::Debug for Retrieved {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RetrievedKnowledge([redacted])")
    }
}

impl Retrieved {
    pub(crate) fn context(&self) -> &str {
        &self.context
    }

    pub(crate) fn citations(&self) -> &[Citation] {
        &self.citations
    }

    pub(crate) fn require_vault_current(&self) -> Result<(), String> {
        self.vault_revision
            .as_ref()
            .map_or(Ok(()), qdrant::require_vault_revision)
    }

    pub(crate) fn validate_references<'a>(
        &self,
        fields: impl Iterator<Item = &'a str>,
    ) -> Result<(), String> {
        for field in fields {
            for tail in field.split("[R").skip(1) {
                let length = tail.bytes().take_while(u8::is_ascii_digit).count();
                if length > 0
                    && tail.as_bytes().get(length) == Some(&b']')
                    && (length != 1
                        || !(b'1'..=b'0' + self.citations.len() as u8)
                            .contains(&tail.as_bytes()[0]))
                {
                    return Err(
                        "The AI cited a knowledge excerpt outside the reviewed context.".into(),
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn require_current(&self, client: &Client) -> Result<(), String> {
        self.require_current_guarded(|| check_active(client))
    }

    pub(crate) fn authority(&self) -> Result<KnowledgeAuthority, String> {
        self.authority.clone().ok_or_else(standing_unsupported)
    }

    pub(crate) fn require_current_guarded(
        &self,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let check = || {
            authorize()?;
            self.require_vault_current()?;
            if self.plan.scope.qdrant.is_some() {
                super::remote_index::require_policy()?;
            }
            Ok(())
        };
        check()?;
        self.plan.require_saved_route()?;
        self.plan
            .cache
            .require_current(&self.plan.scope, &self.stamp)
            .map_err(|_| changed_index())?;
        super::require_source_digests(&self.plan.scope, &self.sources, check)?;
        self.plan
            .cache
            .require_current(&self.plan.scope, &self.stamp)
            .map_err(|_| changed_index())?;
        self.plan.require_saved_route()?;
        if let Some(authority) = &self.authority {
            self.plan
                .require_standing_backend(authority.vault_envelope, ClipboardPolicy::load)?;
        }
        check()
    }
}
