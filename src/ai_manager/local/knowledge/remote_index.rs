//! Publishes Qdrant vectors through an immutable generation and a local receipt.

use std::collections::HashSet;
use std::io;
use std::sync::Arc;

use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::Client;

use super::{Cache, Index, QdrantReceipt, Scope, SourceMask, check_active, folder, qdrant, score};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(in crate::ai_manager::local) struct Publication {
    pub(in crate::ai_manager::local) cleanup_warning: bool,
}

pub(in crate::ai_manager::local) fn require_policy() -> Result<(), String> {
    require_policy_for(&ClipboardPolicy::load())
}

pub(super) fn require_policy_for(policy: &ClipboardPolicy) -> Result<(), String> {
    if policy.problem.is_some() || !policy.ai_allowed {
        return Err("Managed policy disallows Qdrant knowledge requests.".into());
    }
    Ok(())
}

pub(in crate::ai_manager::local) fn publish(
    cache: &Cache,
    scope: &Scope,
    index: &Index,
    sources: SourceMask,
    client: Option<&Client>,
    vault_key: Option<Arc<qdrant::VaultKey>>,
    authorize: impl Fn() -> Result<(), String>,
) -> io::Result<Publication> {
    let Some(settings) = &scope.qdrant else {
        cache.save(index, || {
            authorize().map_err(io::Error::other)?;
            index
                .check_sources(scope, || client.map_or(Ok(()), check_active))
                .map_err(io::Error::other)?;
            authorize().map_err(io::Error::other)
        })?;
        return Ok(Publication::default());
    };
    let check = || {
        authorize()?;
        require_policy()?;
        client.map_or(Ok(()), check_active)
    };
    let connection = qdrant::Connection::new_with_vault(settings, client, vault_key)
        .map_err(io::Error::other)?;
    let check_sources = || {
        check().map_err(io::Error::other)?;
        // Source validation is local. The full saved configuration is checked
        // before and after it and again at each outbound request.
        index
            .check_sources(scope, || client.map_or(Ok(()), check_active))
            .map_err(io::Error::other)?;
        check().map_err(io::Error::other)?;
        connection
            .require_credential_current()
            .map_err(io::Error::other)
    };
    check_sources()?;
    let previous = cache
        .load_stamped(scope)
        .ok()
        .map(|(index, stamp)| (index.qdrant_receipt, stamp));
    let generation = qdrant::new_generation().map_err(io::Error::other)?;
    let receipt = QdrantReceipt {
        generation,
        sources,
    };
    let points: Vec<_> = index
        .entries
        .iter()
        .enumerate()
        .filter(|(_, (passage, _))| receipt.sources.contains(passage.source))
        .map(|(position, (passage, vector))| qdrant::Point {
            position,
            source: passage.source,
            vector,
        })
        .collect();
    let result = connection
        .stage(generation, &points, check)
        .map_err(io::Error::other)
        .and_then(|()| {
            cache.save_with_receipt(index, &receipt, || {
                if let Some((_, stamp)) = &previous {
                    cache.require_current(scope, stamp)?;
                }
                check_sources()
            })
        });
    if let Err(error) = result {
        // A failure reported after local replacement can leave the receipt active.
        let inactive = match cache.load(scope) {
            Ok(current) => current.qdrant_receipt.as_ref() != Some(&receipt),
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
        if inactive && connection.staging_attempted() {
            let _ = connection.delete(generation, check);
        }
        return Err(error);
    }
    let cleanup_warning = previous
        .and_then(|(old, _)| old)
        .is_some_and(|old| connection.delete(old.generation, check).is_err());
    Ok(Publication { cleanup_warning })
}

pub(in crate::ai_manager::local) fn retire(
    scope: &Scope,
    receipt: QdrantReceipt,
    client: Option<&Client>,
    vault_key: Option<Arc<qdrant::VaultKey>>,
    authorize: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    let settings = scope
        .qdrant
        .as_ref()
        .ok_or("The Qdrant destination is unavailable.")?;
    let check = || {
        authorize()?;
        require_policy()
    };
    check()?;
    qdrant::Connection::new_with_vault(settings, client, vault_key)?
        .delete(receipt.generation, check)
}

pub(super) fn automatic_coverage(
    saved: &Scope,
    reviewed: &Scope,
    previous: &Scope,
    index: Option<&Index>,
) -> SourceMask {
    let mut coverage = reviewed.enabled_sources();
    let Some(receipt) = index.and_then(|index| index.qdrant_receipt.as_ref()) else {
        return coverage;
    };
    // Retained vectors have already been sent to this exact backend. Carry only
    // their prior coverage and exclude sources disabled in the saved store.
    let previously_covered: HashSet<_> = previous
        .sources
        .iter()
        .enumerate()
        .filter(|(old, _)| receipt.sources.contains(*old))
        .map(|(_, path)| folder::path_key(path))
        .collect();
    for (source, path) in saved.sources.iter().enumerate() {
        if saved.enabled[source] && previously_covered.contains(&folder::path_key(path)) {
            coverage.insert(source);
        }
    }
    coverage
}

pub(super) fn require_coverage<'a>(
    index: &'a Index,
    scope: &Scope,
) -> Result<&'a QdrantReceipt, String> {
    require_receipt_coverage(index.qdrant_receipt.as_ref(), scope)
}

pub(super) fn require_receipt_coverage<'a>(
    receipt: Option<&'a QdrantReceipt>,
    scope: &Scope,
) -> Result<&'a QdrantReceipt, String> {
    let receipt = receipt.ok_or("Save and update the knowledge index before searching Qdrant.")?;
    if !receipt.sources.covers(&scope.enabled_sources()) {
        return Err("Update the Qdrant index to include the enabled sources.".into());
    }
    Ok(receipt)
}

pub(super) fn query(
    client: &Client,
    scope: &Scope,
    index: &Index,
    vector: &[f32],
    limit: usize,
    vault_key: Option<Arc<qdrant::VaultKey>>,
    authorize: impl Fn() -> Result<(), String>,
) -> Result<Vec<(f32, usize)>, String> {
    let settings = scope
        .qdrant
        .as_ref()
        .ok_or("The Qdrant destination is unavailable.")?;
    let receipt = require_coverage(index, scope)?;
    let check = || {
        check_active(client)?;
        authorize()?;
        require_policy()
    };
    check()?;
    let limit = limit.min(index.enabled_entries(scope));
    let connection = qdrant::Connection::new_with_vault(settings, Some(client), vault_key)?;
    let qdrant::QueryHits { limit, hits } =
        connection.query(receipt.generation, vector, &scope.enabled, limit, check)?;
    if hits.len() != limit {
        return Err("Qdrant returned an incomplete generation. Update the knowledge index.".into());
    }
    let mut ranked = Vec::with_capacity(hits.len());
    for (_, position, source) in hits {
        let (passage, embedding) = index
            .entries
            .get(position)
            .filter(|(passage, _)| passage.source == source && scope.enabled[passage.source])
            .ok_or("Qdrant returned a point outside the verified knowledge index.")?;
        debug_assert_eq!(passage.source, source);
        ranked.push((score(vector, embedding), position));
    }
    ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    check()?;
    connection.require_credential_current()?;
    Ok(ranked)
}
