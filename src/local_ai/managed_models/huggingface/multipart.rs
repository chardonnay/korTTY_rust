//! Complete canonical shard sets from one immutable Hub repository revision.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::local_ai::managed_models::{MAX_MODEL_BYTES, split};

use super::Artifact;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Selection(Arc<[Artifact]>);

impl Selection {
    pub(super) fn files(&self) -> &[Artifact] {
        &self.0
    }
}

impl fmt::Display for Selection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let first = &self.0[0];
        if self.0.len() == 1 {
            return fmt::Display::fmt(first, formatter);
        }
        let name = split::components(first.path())
            .expect("complete shard set")
            .0;
        let size = total(&*self.0).expect("bounded shard set");
        write!(
            formatter,
            "{name} · {} shards · {} MiB",
            self.0.len(),
            size.div_ceil(1024 * 1024)
        )
    }
}

pub(super) fn selections(files: Vec<Artifact>) -> Vec<Selection> {
    let mut sets = Vec::new();
    let mut shards: BTreeMap<(String, usize), Vec<(usize, Artifact)>> = BTreeMap::new();
    for file in files {
        if let Some((stem, index, count)) = split::components(file.path()) {
            shards
                .entry((stem.to_owned(), count))
                .or_default()
                .push((index, file));
        } else {
            sets.push(Selection(Arc::from([file])));
        }
    }
    for mut group in shards.into_values() {
        group.sort_unstable_by_key(|(index, _)| *index);
        let group: Vec<_> = group.into_iter().map(|(_, file)| file).collect();
        if complete(&group) && total(&group).is_some() {
            sets.push(Selection(group.into()));
        }
    }
    sets.sort_unstable_by(|left, right| left.0[0].path().cmp(right.0[0].path()));
    sets
}

pub(super) fn complete<'a>(files: impl IntoIterator<Item = &'a Artifact>) -> bool {
    let mut files = files.into_iter();
    let Some(first) = files.next() else {
        return false;
    };
    match split::components(first.path()) {
        None => files.next().is_none(),
        Some((stem, 1, count)) if (2..=split::MAX_SHARDS).contains(&count) => {
            files.enumerate().try_fold(1, |number, (index, file)| {
                (split::components(file.path()) == Some((stem, index + 2, count)))
                    .then_some(number + 1)
            }) == Some(count)
        }
        _ => false,
    }
}

pub(super) fn total<'a>(files: impl IntoIterator<Item = &'a Artifact>) -> Option<u64> {
    files.into_iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.size())
            .filter(|&bytes| bytes <= MAX_MODEL_BYTES)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> Artifact {
        Artifact {
            path: path.into(),
            size,
            digest: "a".repeat(64),
        }
    }

    #[test]
    fn only_complete_ordered_same_directory_shards_are_selectable() {
        let first = file("source/model-00001-of-00002.gguf", 4);
        let second = file("source/model-00002-of-00002.gguf", 5);
        let incomplete = file("other/model-00001-of-00002.gguf", 7);
        let oversized_first = file("large/model-00001-of-00002.gguf", MAX_MODEL_BYTES);
        let oversized_second = file("large/model-00002-of-00002.gguf", 1);
        let catalog = selections(vec![
            second.clone(),
            incomplete,
            oversized_first,
            first.clone(),
            oversized_second,
        ]);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].files(), &[first.clone(), second.clone()]);
        assert_eq!(total(catalog[0].files()), Some(9));
        assert!(!complete([&second, &first]));
        assert!(!complete([&first, &first]));
    }
}
