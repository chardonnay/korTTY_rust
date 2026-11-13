//! Transient progress and exact reviewed keywords from a bounded structured reply.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use serde::de::{DeserializeSeed, Deserializer as _, IgnoredAny, MapAccess, SeqAccess, Visitor};
use zeroize::{Zeroize, Zeroizing};

use crate::local_ai::StreamDelta;

use super::{MAX_KEYWORDS, MAX_RESPONSE_BYTES, verified_keyword};

const UPDATE_INTERVAL: Duration = Duration::from_millis(75);

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Stage {
    Checking,
    Window,
    Closing,
    Screenshot,
}

#[derive(Clone)]
pub(crate) struct Preview {
    pub(crate) stage: Stage,
    pub(crate) answer_bytes: usize,
    pub(crate) keywords: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Reporter<'a> {
    pub(crate) show_keywords: bool,
    pub(crate) update: Option<&'a mut dyn FnMut(Option<Preview>)>,
}

impl Reporter<'_> {
    pub(crate) fn begin(&mut self, stage: Stage) {
        self.publish(Some(Preview {
            stage,
            answer_bytes: 0,
            keywords: Vec::new(),
        }));
    }

    pub(crate) fn clear(&mut self) {
        self.publish(None);
    }

    fn publish(&mut self, preview: Option<Preview>) {
        if let Some(update) = &mut self.update {
            update(preview);
        }
    }
}

#[derive(Default)]
pub(super) struct Buffer {
    answer_bytes: usize,
    answer: Zeroizing<String>,
    keywords: Vec<String>,
    last: Option<Instant>,
}

impl Buffer {
    pub(super) fn push(
        &mut self,
        stage: Stage,
        delta: StreamDelta,
        evidence: &[&str],
        mask: &impl Fn(&str) -> io::Result<String>,
        reporter: &mut Reporter<'_>,
    ) {
        let mut text = match delta {
            StreamDelta::Text(text) => text,
            StreamDelta::Reasoning(mut reasoning) => {
                reasoning.zeroize();
                return;
            }
        };
        self.answer_bytes = self.answer_bytes.saturating_add(text.len());
        let mut changed = false;
        if reporter.show_keywords {
            if self.answer_bytes > MAX_RESPONSE_BYTES {
                self.answer.zeroize();
                changed = !self.keywords.is_empty();
                self.keywords.clear();
            } else {
                self.answer.push_str(&text);
                if text.contains('"') && self.answer.contains("\"keywords\"") {
                    let keywords = verified_keywords(&self.answer, evidence, mask);
                    changed = self.keywords != keywords;
                    self.keywords = keywords;
                }
            }
        }
        text.zeroize();
        let now = Instant::now();
        if changed
            || self
                .last
                .is_none_or(|last| now.duration_since(last) >= UPDATE_INTERVAL)
        {
            self.last = Some(now);
            reporter.publish(Some(Preview {
                stage,
                answer_bytes: self.answer_bytes,
                keywords: self.keywords.clone(),
            }));
        }
    }
}

fn verified_keywords<Mask: Fn(&str) -> io::Result<String>>(
    input: &str,
    evidence: &[&str],
    mask: &Mask,
) -> Vec<String> {
    struct Keywords<'a, Mask> {
        evidence: &'a [&'a str],
        mask: &'a Mask,
        words: &'a mut Vec<String>,
    }

    impl<'de, Mask: Fn(&str) -> io::Result<String>> DeserializeSeed<'de> for Keywords<'_, Mask> {
        type Value = ();

        fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
            decoder.deserialize_seq(self)
        }
    }

    impl<'de, Mask: Fn(&str) -> io::Result<String>> Visitor<'de> for Keywords<'_, Mask> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an array of exact journal keywords")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
            for _ in 0..MAX_KEYWORDS {
                let Some(word) = sequence.next_element::<String>()? else {
                    break;
                };
                let word = Zeroizing::new(word);
                if let Ok(Some(safe)) =
                    verified_keyword(&word, self.evidence, self.words, self.mask)
                {
                    self.words.push(safe);
                }
            }
            Ok(())
        }
    }

    struct Object<'a, Mask>(Keywords<'a, Mask>);

    impl<'de, Mask: Fn(&str) -> io::Result<String>> Visitor<'de> for Object<'_, Mask> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a journal summary object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut fields: A) -> Result<(), A::Error> {
            while let Some(key) = fields.next_key::<String>()? {
                if key == "keywords" {
                    fields.next_value_seed(self.0)?;
                    break;
                }
                fields.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }

    let mut words = Vec::new();
    let _ = serde_json::Deserializer::from_str(input).deserialize_map(Object(Keywords {
        evidence,
        mask,
        words: &mut words,
    }));
    words
}
