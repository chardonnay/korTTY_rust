//! Verified model recommendations and exact Hugging Face artifact matching.

use std::{cmp::Reverse, fmt, sync::Arc};

use super::{Purpose, Review};
use crate::local_ai::model_catalog::{Catalog, bootstrap};

pub(crate) use crate::local_ai::model_catalog::{Recommendation, Role as RecommendationRole};

pub(crate) const GIB: u64 = 1024 * 1024 * 1024;

impl RecommendationRole {
    pub(crate) const ALL: [Self; 3] = [Self::Text, Self::Coding, Self::Embedding];

    pub(crate) fn purpose(self) -> Purpose {
        match self {
            Self::Text | Self::Coding => Purpose::Chat,
            Self::Embedding => Purpose::Embedding,
        }
    }
}

impl fmt::Display for RecommendationRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Text => "Text and translation",
            Self::Coding => "Coding",
            Self::Embedding => "Knowledge embeddings",
        })
    }
}

impl Recommendation {
    pub(crate) fn matches_review(&self, review: &Review) -> bool {
        self.model_id == review.repository.id()
            && self
                .revision
                .as_deref()
                .is_none_or(|revision| revision == review.revision)
    }

    pub(super) fn retain_files(&self, review: &mut Review) -> Result<(), String> {
        let quantization = self.quantization.to_ascii_lowercase();
        review.files.retain(|selection| {
            let filename = selection.files()[0]
                .path()
                .rsplit('/')
                .next()
                .unwrap_or_default();
            let name = filename.to_ascii_lowercase();
            let stem = super::super::split::components(&name)
                .map(|(stem, _, _)| stem)
                .or_else(|| name.strip_suffix(".gguf"));
            stem.and_then(|stem| stem.strip_suffix(quantization.as_str()))
                .is_some_and(|prefix| prefix.ends_with(['-', '_', '.']))
        });
        // Prefer a single verified GGUF when the same quantization also has shards.
        review
            .files
            .sort_by_key(|selection| selection.files().len() != 1);
        if review.files.is_empty() {
            return Err(format!(
                "The pinned repository has no complete verified {} GGUF.",
                self.quantization
            ));
        }
        Ok(())
    }
}

impl fmt::Display for Recommendation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} · {}", self.model_id, self.quantization)
    }
}

pub(crate) fn recommendations(
    catalog: Option<&Catalog>,
    role: RecommendationRole,
    memory: Option<u64>,
) -> Vec<Arc<Recommendation>> {
    let memory = memory.unwrap_or_default();
    let mut candidates = catalog
        .unwrap_or_else(|| bootstrap())
        .recommendations()
        .iter()
        .filter(|entry| entry.roles.contains(&role) && entry.minimum_system_memory_bytes <= memory)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|entry| Reverse((entry.preference, entry.minimum_system_memory_bytes)));
    candidates.into_iter().cloned().map(Arc::new).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use RecommendationRole::{Coding, Embedding, Text};

    fn ids(role: RecommendationRole, ram: Option<u64>) -> Vec<String> {
        recommendations(None, role, ram)
            .iter()
            .map(|entry| entry.id.clone())
            .collect()
    }

    #[test]
    fn recommendations_follow_the_pinned_roles_memory_and_order() {
        assert_eq!(ids(Text, None), ["qwen3-1.7b-q4"]);
        assert_eq!(ids(Text, Some(16 * GIB - 1)), ["qwen3-1.7b-q4"]);
        assert_eq!(ids(Text, Some(16 * GIB)), ["qwen3-4b-q4", "qwen3-1.7b-q4"]);
        assert_eq!(
            ids(Text, Some(24 * GIB - 1)),
            ["qwen3-4b-q4", "qwen3-1.7b-q4"]
        );
        assert_eq!(
            ids(Text, Some(24 * GIB)),
            ["qwen3-8b-q4", "qwen3-4b-q4", "qwen3-1.7b-q4"]
        );
        assert_eq!(ids(Coding, None), ["qwen3-1.7b-q4"]);
        assert_eq!(
            ids(Coding, Some(16 * GIB)),
            ["qwen2.5-coder-7b-q4", "qwen3-1.7b-q4"]
        );
        assert_eq!(
            ids(Embedding, None),
            [
                "qwen3-embedding-0.6b-q8",
                "bge-m3-q8",
                "nomic-embed-text-1.5-q8"
            ]
        );
        assert_eq!(
            ids(Embedding, Some(24 * GIB)),
            [
                "qwen3-embedding-0.6b-q8",
                "qwen3-embedding-4b-q4",
                "qwen3-embedding-8b-q4",
                "bge-m3-q8",
                "nomic-embed-text-1.5-q8"
            ]
        );
        for entry in bootstrap().recommendations() {
            assert!(super::super::lowercase_hex(
                entry.revision.as_deref().unwrap(),
                40
            ));
            assert!(super::super::Repository::parse(&entry.model_id).is_ok());
        }
    }
}
