use std::{fmt, io};

use crate::local_ai::Client;

use super::{Question, changed, check_cancel, clip, index, rank_terms, read_entries};
use crate::session_journal::{
    Catalog, MAX_CATALOG_BYTES, Summary, deletion::DeletionIdentity, tree, viewer::Source,
};

const MAX_SHORTLIST: usize = 24;
const MAX_SELECTED: usize = 6;
const MAX_CARD_BYTES: usize = 1_000;
const MAX_RANK_FIELD_BYTES: usize = 2_048;
const RANK_FUSION_K: f64 = 60.0;

#[derive(Clone)]
pub(crate) struct Candidate {
    pub(crate) source: Source,
    pub(crate) title: String,
    pub(crate) card: String,
    lexical: f64,
    stamp: DeletionIdentity,
    revision: Option<[u8; 32]>,
}

#[derive(Clone)]
pub(crate) struct Ranking {
    pub(crate) question: Question,
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) partial: bool,
    pub(crate) filter: tree::Filter,
}

impl fmt::Debug for Ranking {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalQuestionRanking")
            .field("candidates", &self.candidates.len())
            .field("partial", &self.partial)
            .finish_non_exhaustive()
    }
}

impl Ranking {
    /// The lexical shortlist is deterministic and requires no index or model.
    pub(crate) fn prepare(
        question: Question,
        catalog: &Catalog,
        filter: tree::Filter,
        mask: &impl Fn(&str) -> io::Result<String>,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Self> {
        let terms = rank_terms(question.as_str());
        let mut documents = Vec::new();
        let mut frequency = vec![0_usize; terms.len()];
        let mut remaining = MAX_CATALOG_BYTES;
        let mut partial = catalog.limited || catalog.skipped != 0;
        for summary in catalog
            .journals
            .iter()
            .filter(|journal| filter.matches(journal))
        {
            check_cancel(cancelled)?;
            let (Ok(source), Some(stamp)) = (Source::catalog(summary), &summary.deletion_identity)
            else {
                partial = true;
                continue;
            };
            let Some(evidence) = read_entries(
                &source,
                &mut remaining,
                Some(stamp),
                &terms,
                mask,
                cancelled,
                true,
            )?
            else {
                partial = true;
                continue;
            };
            if !filter.matches(&evidence.summary) {
                continue;
            }
            partial |= evidence.partial;
            let excerpts = evidence.semantic.as_ref().map_or_else(
                || evidence.candidates.as_slice(),
                |card| card.excerpts.as_slice(),
            );
            let metadata: Vec<_> = fields(&evidence.summary)
                .map(mask)
                .collect::<io::Result<_>>()?;
            let mut hits = vec![0_u8; terms.len()];
            let mut score_field = |field: &str, weight| {
                let normalized = clip(field, MAX_RANK_FIELD_BYTES).to_lowercase();
                for (term, count) in terms.iter().zip(&mut hits) {
                    if normalized.contains(term.as_str()) {
                        *count += weight;
                    }
                }
            };
            for (index, field) in metadata.iter().enumerate() {
                score_field(field, if index == 0 { 2 } else { 1 });
            }
            for excerpt in &evidence.candidates {
                score_field(&excerpt.excerpt, 1);
            }

            let title = clip(&metadata[0], 160).to_owned();
            let mut card = String::new();
            for text in std::iter::once(title.as_str())
                .chain(excerpts.iter().map(|entry| entry.excerpt.as_str()))
                .chain(metadata.iter().skip(1).map(String::as_str))
                .filter(|text| !text.trim().is_empty())
            {
                card.push_str(clip(text, 220));
                card.push('\n');
            }
            let card = clip(card.trim(), MAX_CARD_BYTES).to_owned();
            if card.is_empty() {
                continue;
            }

            for (count, hits) in frequency.iter_mut().zip(&hits) {
                *count += usize::from(*hits != 0);
            }
            documents.push((
                Candidate {
                    source,
                    title,
                    card,
                    lexical: 0.0,
                    stamp: evidence.stamp,
                    revision: evidence.semantic.map(|card| card.revision),
                },
                hits,
                summary.started_at,
            ));
        }
        let total = documents.len() as f64;
        let weights: Vec<_> = frequency
            .into_iter()
            .map(|count| {
                let count = count as f64;
                (1.0 + (total - count + 0.5) / (count + 0.5)).ln()
            })
            .collect();
        for (candidate, hits, _) in &mut documents {
            candidate.lexical = hits
                .iter()
                .zip(&weights)
                .map(|(&hits, weight)| {
                    let hits = f64::from(hits);
                    weight * (hits * 2.2) / (hits + 1.2)
                })
                .sum();
        }
        documents.sort_by(|(left, _, left_time), (right, _, right_time)| {
            right
                .lexical
                .total_cmp(&left.lexical)
                .then_with(|| right_time.cmp(left_time))
                .then_with(|| left.source.id().cmp(right.source.id()))
        });
        check_cancel(cancelled)?;
        let candidates = documents
            .into_iter()
            .take(MAX_SHORTLIST)
            .map(|(candidate, _, _)| candidate)
            .collect();
        Ok(Self {
            question,
            candidates,
            partial,
            filter,
        })
    }

    pub(crate) fn lexical(&self) -> Vec<Source> {
        self.candidates
            .iter()
            .take(MAX_SELECTED)
            .map(|candidate| candidate.source.clone())
            .collect()
    }

    /// Call only with the separately reviewed embedding request.
    pub(crate) fn semantic(
        &self,
        client: &Client,
        stopped: &dyn Fn() -> bool,
        authorized: &dyn Fn() -> bool,
    ) -> (Vec<Source>, bool) {
        let stopped = || stopped() || !client.active();
        self.semantic_rank(client, &stopped, authorized)
            .map_or_else(|| (self.lexical(), false), |ranked| (ranked, true))
    }

    fn semantic_rank(
        &self,
        client: &Client,
        stopped: &dyn Fn() -> bool,
        authorized: &dyn Fn() -> bool,
    ) -> Option<Vec<Source>> {
        if self.candidates.is_empty() || !authorized() {
            return None;
        }
        self.require_current(stopped).ok()?;
        let model = client.embedding_revision()?;
        let keys: Vec<_> = self
            .candidates
            .iter()
            .map(|candidate| {
                index::key(
                    &candidate.stamp,
                    candidate.revision?,
                    &candidate.card,
                    model,
                )
                .ok()
            })
            .collect::<Option<_>>()?;
        let mut cards: Vec<_> = self
            .candidates
            .iter()
            .zip(&keys)
            .map(|(candidate, key)| index::load(&candidate.source, key))
            .collect();
        let mut missing: Vec<_> = cards
            .iter()
            .enumerate()
            .filter_map(|(position, vector)| vector.is_none().then_some(position))
            .collect();
        let embed = |missing: &[usize]| {
            let input: Vec<_> = std::iter::once(self.question.as_str())
                .chain(
                    missing
                        .iter()
                        .map(|&position| self.candidates[position].card.as_str()),
                )
                .collect();
            let mut reply = client
                .embed_reviewed(&input, || {
                    self.require_current(stopped)
                        .map_err(|error| error.to_string())?;
                    if !authorized() {
                        return Err("The journal semantic review is no longer allowed.".into());
                    }
                    Ok(())
                })
                .ok()?
                .into_iter();
            Some((reply.next()?, reply))
        };
        let (mut query, mut reply) = embed(&missing)?;
        if cards
            .iter()
            .flatten()
            .any(|vector| vector.len() != query.len())
        {
            // An endpoint can replace a model under the same name. Rebuild incompatible vectors.
            missing = (0..cards.len()).collect();
            (query, reply) = embed(&missing)?;
        }
        for (&position, vector) in missing.iter().zip(reply) {
            cards[position] = Some(vector);
        }
        let cards = cards.into_iter().collect::<Option<Vec<_>>>()?;
        let ranked = fuse(&self.candidates, &query, &cards)?;
        self.require_current(stopped).ok()?;
        if !authorized() {
            return None;
        }
        for position in missing {
            let candidate = &self.candidates[position];
            let _ = index::save(
                &candidate.source,
                &candidate.stamp,
                &keys[position],
                &cards[position],
                stopped,
            );
        }
        check_cancel(stopped).ok()?;
        Some(ranked)
    }

    fn require_current(&self, stopped: &dyn Fn() -> bool) -> io::Result<()> {
        let mut budget = MAX_CATALOG_BYTES;
        for candidate in &self.candidates {
            check_cancel(stopped)?;
            candidate
                .source
                .with_document_stamp_budget(&mut budget, |_, stamp, document| {
                    if *stamp != candidate.stamp
                        || Some(index::document_revision(document)?) != candidate.revision
                    {
                        return Err(changed());
                    }
                    Ok(())
                })?;
        }
        check_cancel(stopped)
    }
}

fn fields(journal: &Summary) -> impl Iterator<Item = &str> {
    [
        journal.title.as_str(),
        journal.description.as_str(),
        journal.connection_name.as_str(),
        journal.host.as_str(),
        journal.username.as_str(),
    ]
    .into_iter()
    .chain(journal.keywords.iter().take(12).map(String::as_str))
}

fn fuse(candidates: &[Candidate], query: &[f32], cards: &[Vec<f32>]) -> Option<Vec<Source>> {
    if cards.len() != candidates.len() || query.is_empty() {
        return None;
    }
    let norm = |vector: &[f32]| -> Option<f64> {
        let squares: f64 = vector.iter().map(|&value| f64::from(value).powi(2)).sum();
        (squares.is_finite() && squares > 0.0).then(|| squares.sqrt())
    };
    let query_length = norm(query)?;
    let mut scores = Vec::with_capacity(candidates.len());
    for (index, card) in cards.iter().enumerate() {
        if card.len() != query.len() {
            return None;
        }
        let length = norm(card)?;
        let dot: f64 = query
            .iter()
            .zip(card)
            .map(|(&left, &right)| f64::from(left) * f64::from(right))
            .sum();
        scores.push((index, (dot / (length * query_length)).clamp(-1.0, 1.0)));
    }
    scores.sort_by(|(a, left), (b, right)| right.total_cmp(left).then_with(|| a.cmp(b)));
    let weight = |rank| 1.0 / (RANK_FUSION_K + rank as f64 + 1.0);
    for (semantic_rank, (index, score)) in scores.iter_mut().enumerate() {
        let lexical = if candidates[*index].lexical > 0.0 {
            weight(*index)
        } else {
            0.0
        };
        *score = lexical + weight(semantic_rank);
    }
    scores.sort_by(|(a, left), (b, right)| right.total_cmp(left).then_with(|| a.cmp(b)));
    Some(
        scores
            .into_iter()
            .take(MAX_SELECTED)
            .map(|(index, _)| candidates[index].source.clone())
            .collect(),
    )
}
