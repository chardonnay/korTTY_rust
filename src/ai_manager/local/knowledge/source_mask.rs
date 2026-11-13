use serde::{Serialize, Serializer};

const WORD_BITS: usize = u32::BITS as usize;

/// Source positions covered by an approved index, with no trailing zero words.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::ai_manager::local) struct SourceMask(Vec<u32>);

impl SourceMask {
    pub(super) fn insert(&mut self, source: usize) {
        let word = source / WORD_BITS;
        if word >= self.0.len() {
            self.0.resize(word + 1, 0);
        }
        self.0[word] |= 1 << (source % WORD_BITS);
    }

    pub(super) fn contains(&self, source: usize) -> bool {
        self.word(source / WORD_BITS) & (1 << (source % WORD_BITS)) != 0
    }

    pub(in crate::ai_manager::local) fn covers(&self, required: &Self) -> bool {
        required
            .0
            .iter()
            .enumerate()
            .all(|(index, word)| self.word(index) & word == *word)
    }

    pub(super) fn valid_for(&self, count: usize) -> bool {
        let complete = count / WORD_BITS;
        let remainder = count % WORD_BITS;
        self.0.len() <= Self::word_count(count)
            && (remainder == 0 || self.word(complete) >> remainder == 0)
    }

    pub(super) fn word(&self, index: usize) -> u32 {
        self.0.get(index).copied().unwrap_or_default()
    }

    pub(super) fn word_count(count: usize) -> usize {
        count.div_ceil(WORD_BITS)
    }

    pub(super) fn from_words(mut words: Vec<u32>) -> Self {
        words.truncate(
            words
                .iter()
                .rposition(|&word| word != 0)
                .map_or(0, |i| i + 1),
        );
        Self(words)
    }
}

impl From<u32> for SourceMask {
    fn from(word: u32) -> Self {
        Self::from_words(vec![word])
    }
}

impl Serialize for SourceMask {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.len() <= 1 {
            // Keep the existing digest for previously approved standing routes.
            serializer.serialize_u32(self.word(0))
        } else {
            self.0.serialize(serializer)
        }
    }
}
