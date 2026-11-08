use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::io;

use roxmltree::Node;

use super::{Summary, invalid, is_display_character, metadata::java_whitespace, scalar_text};

pub(crate) const MAX_QUERY_BYTES: usize = 512;
const TOP_KEYWORDS: usize = 15;

pub(super) fn read(meta: Node<'_, '_>) -> io::Result<Vec<String>> {
    let mut wrappers = meta
        .children()
        .filter(|node| node.has_tag_name("aiKeywords"));
    let wrapper = wrappers.next();
    if wrappers.next().is_some() {
        return Err(invalid("Duplicate session journal keyword list"));
    }
    wrapper
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| node.has_tag_name("keyword"))
        .map(|node| scalar_text(node).map(Cow::into_owned))
        .collect()
}

pub(crate) fn usable(keyword: &str) -> bool {
    keyword.len() <= MAX_QUERY_BYTES
        && !keyword.trim_matches(java_whitespace).is_empty()
        && keyword
            .chars()
            .all(|character| !character.is_control() && is_display_character(character))
}

/// Ranks occurrences by descending frequency, with first appearance in catalog order breaking ties.
pub(super) fn rank(journals: &[Summary]) -> Vec<String> {
    let mut counts = HashMap::new();
    for (order, keyword) in journals
        .iter()
        .flat_map(|journal| &journal.keywords)
        .filter(|keyword| usable(keyword))
        .enumerate()
    {
        let lowercase = keyword.to_lowercase();
        if usable(&lowercase) {
            counts.entry(lowercase).or_insert((0_usize, order)).0 += 1;
        }
    }
    let key = |(_, (count, order)): &(String, (usize, usize))| (Reverse(*count), *order);
    let mut ranked: Vec<_> = counts.into_iter().collect();
    if ranked.len() > TOP_KEYWORDS {
        ranked.select_nth_unstable_by_key(TOP_KEYWORDS, key);
        ranked.truncate(TOP_KEYWORDS);
    }
    ranked.sort_unstable_by_key(key);
    ranked.into_iter().map(|(keyword, _)| keyword).collect()
}
