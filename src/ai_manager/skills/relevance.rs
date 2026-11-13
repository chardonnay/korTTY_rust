//! Local skill relevance weights from the pinned Java selector.

use std::cmp::Reverse;
use std::collections::HashSet;

use super::Skill;

pub(super) const MAX_AUTOMATIC_SKILLS: usize = 2;
const MINIMUM_SCORE: usize = 4;
const MAX_CONTEXT_BYTES: usize = 32 * 1024;
const MAX_HEADINGS: usize = 128;
const STOP_WORDS: &[&str] = &[
    "about", "after", "also", "and", "are", "ask", "but", "can", "code", "das", "der", "die",
    "ein", "eine", "for", "from", "how", "ich", "ist", "mit", "not", "oder", "please", "prompt",
    "soll", "the", "this", "und", "use", "was", "welche", "what", "when", "where", "wie", "with",
    "you", "zur",
];

pub(super) struct ScoredSkill<'a> {
    pub(super) id: u64,
    pub(super) skill: &'a Skill,
    score: usize,
}

pub(super) fn ranked<'a>(
    context: &str,
    candidates: impl Iterator<Item = (u64, &'a Skill)>,
) -> Vec<(u64, &'a Skill)> {
    let ranked = scored(context, candidates);
    strong_matches(&ranked)
        .iter()
        .take(MAX_AUTOMATIC_SKILLS)
        .map(|candidate| (candidate.id, candidate.skill))
        .collect()
}

pub(super) fn scored<'a>(
    context: &str,
    candidates: impl Iterator<Item = (u64, &'a Skill)>,
) -> Vec<ScoredSkill<'a>> {
    let context =
        normalized(&context[..context.floor_char_boundary(MAX_CONTEXT_BYTES.min(context.len()))]);
    let context_terms = terms(&context);
    let mut ranked: Vec<_> = candidates
        .map(|(id, skill)| ScoredSkill {
            id,
            skill,
            score: score(skill, &context, &context_terms),
        })
        .collect();
    ranked.sort_unstable_by_key(|candidate| (Reverse(candidate.score), candidate.id));
    ranked
}

pub(super) fn strong_matches<'a, 'skill>(
    ranked: &'a [ScoredSkill<'skill>],
) -> &'a [ScoredSkill<'skill>] {
    &ranked[..ranked.partition_point(|candidate| candidate.score >= MINIMUM_SCORE)]
}

pub(super) fn should_classify(ranked: &[ScoredSkill<'_>]) -> bool {
    if ranked.is_empty() {
        return false;
    }
    match strong_matches(ranked) {
        [] | [_] => true,
        [first, second] => first.score - second.score <= 2,
        _ => false,
    }
}

fn score(skill: &Skill, context: &str, context_terms: &HashSet<&str>) -> usize {
    let weighted = |value: &str, phrase_weight: usize, term_weight: usize| {
        let value = normalized(value);
        if value.is_empty() {
            return 0;
        }
        let phrase_score = usize::from(contains_topic(context, &value)) * phrase_weight;
        let term_score = terms(&value).intersection(context_terms).count() * term_weight;
        phrase_score + term_score
    };
    let tags = skill
        .tags
        .iter()
        .map(|tag| weighted(tag, 6, 4))
        .sum::<usize>();
    let headings = skill
        .content
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            line.starts_with('#').then(|| line.trim_start_matches('#'))
        })
        .take(MAX_HEADINGS)
        .map(|heading| weighted(heading, 4, 1))
        .sum::<usize>();
    // Keep the explicit bundled topics, including short language names such as Go and C.
    let bundled_topic = skill.builtin_topics.iter().any(|topic| {
        let topic = normalized(topic);
        !topic.is_empty() && contains_topic(context, &topic)
    });
    tags + weighted(&skill.name, 6, 2)
        + weighted(&skill.description, 5, 1)
        + headings
        + usize::from(bundled_topic) * MINIMUM_SCORE
}

fn normalized(text: &str) -> String {
    text.trim().to_lowercase().replace('_', "-")
}

fn terms(text: &str) -> HashSet<&str> {
    text.split(|ch: char| !ch.is_alphanumeric() && !matches!(ch, '#' | '+' | '.' | '_' | '-'))
        .filter(|term| term.chars().take(3).count() == 3 && !STOP_WORDS.contains(term))
        .collect()
}

fn contains_topic(context: &str, phrase: &str) -> bool {
    context.match_indices(phrase).any(|(start, _)| {
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        !context[..start].chars().next_back().is_some_and(word)
            && !context[start + phrase.len()..]
                .chars()
                .next()
                .is_some_and(word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_uses_the_java_strong_match_count_and_score_gap() {
        let skill = Skill::default();
        for (scores, expected) in [
            (&[][..], false),
            (&[3, 1], true),
            (&[7, 3, 2], true),
            (&[6, 4, 3], true),
            (&[7, 4, 3], false),
            (&[5, 5, 4], false),
        ] {
            let ranked: Vec<_> = scores
                .iter()
                .enumerate()
                .map(|(index, &score)| ScoredSkill {
                    id: index as u64 + 1,
                    skill: &skill,
                    score,
                })
                .collect();
            assert_eq!(should_classify(&ranked), expected, "scores {scores:?}");
        }
    }
}
