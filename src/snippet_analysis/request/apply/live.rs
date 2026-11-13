//! Bounded display-only progress for a proposed rewrite.

use std::fmt;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::{Deserializer as _, IgnoredAny, MapAccess, SeqAccess, Visitor};
use zeroize::{Zeroize, Zeroizing};

use crate::local_ai::{StreamDelta, reply};

use super::WorkItem;

const UPDATE_INTERVAL: Duration = Duration::from_millis(50);
const MAX_RESPONSE: usize = 256 * 1024;
const MAX_REASON: usize = 1024;

pub(crate) type MaskReason = Box<dyn Fn(&str) -> Option<String> + Send>;

#[derive(Clone)]
pub(crate) struct Progress {
    pub(crate) file: Option<(usize, usize)>,
    pub(crate) stage: usize,
    pub(crate) stages: usize,
    pub(crate) repair: bool,
    pub(crate) answer_bytes: usize,
    pub(crate) reason: String,
}

pub(crate) struct Observer {
    updates: async_channel::Sender<Progress>,
    mask: Option<MaskReason>,
    file: Option<(usize, usize)>,
}

impl Observer {
    pub(crate) fn new(updates: async_channel::Sender<Progress>, mask: Option<MaskReason>) -> Self {
        Self {
            updates,
            mask,
            file: None,
        }
    }

    pub(crate) fn file(&mut self, index: usize, total: usize) {
        self.file = Some((index, total));
    }

    pub(super) fn attempt<'a>(
        &'a self,
        stage: usize,
        stages: usize,
        items: &'a [WorkItem],
        repair: bool,
    ) -> Attempt<'a> {
        let progress = Progress {
            file: self.file,
            stage,
            stages,
            repair,
            answer_bytes: 0,
            reason: String::new(),
        };
        Attempt {
            observer: self,
            progress,
            items,
            answer: Zeroizing::new(String::new()),
            last: None,
        }
    }
}

pub(super) struct Attempt<'a> {
    observer: &'a Observer,
    progress: Progress,
    items: &'a [WorkItem],
    answer: Zeroizing<String>,
    last: Option<Instant>,
}

impl Attempt<'_> {
    pub(super) fn reset(&mut self) {
        self.progress.answer_bytes = 0;
        self.progress.reason.clear();
        self.answer.zeroize();
        self.last = None;
        self.publish();
    }

    pub(super) fn update(&mut self, delta: StreamDelta) {
        let mut chunk = match delta {
            StreamDelta::Text(chunk) => chunk,
            StreamDelta::Reasoning(mut chunk) => {
                chunk.zeroize();
                return;
            }
        };
        self.progress.answer_bytes = self.progress.answer_bytes.saturating_add(chunk.len());
        if self.observer.mask.is_some()
            && self.progress.reason.is_empty()
            && self.answer.len().saturating_add(chunk.len()) <= MAX_RESPONSE
        {
            self.answer.push_str(&chunk);
        }
        chunk.zeroize();
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < UPDATE_INTERVAL)
        {
            return;
        }
        if self.progress.reason.is_empty()
            && let Some(mask) = &self.observer.mask
            && let Some(reason) = complete_reason(&self.answer, self.items)
            && let Some(mut reason) = mask(&reason)
        {
            reason.truncate(reason.floor_char_boundary(MAX_REASON));
            self.progress.reason = reason;
            self.answer.zeroize();
        }
        self.last = Some(now);
        self.publish();
    }

    fn publish(&self) {
        // Only the newest snapshot matters. A repair or fallback replaces the
        // old attempt even when the user interface has not drained the channel.
        let _ = self.observer.updates.force_send(self.progress.clone());
    }
}

fn complete_reason(input: &str, items: &[WorkItem]) -> Option<Zeroizing<String>> {
    struct Reasons<'a> {
        items: &'a [WorkItem],
        result: &'a mut Option<Zeroizing<String>>,
    }

    impl<'de> Visitor<'de> for Reasons<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("reviewed changes in an apply response")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            while let Some(key) = map.next_key::<String>()? {
                if key == "changes" {
                    map.next_value_seed(ChangeList(self))?;
                    break;
                }
                map.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }

    struct ChangeList<'a>(Reasons<'a>);

    impl<'de> serde::de::DeserializeSeed<'de> for ChangeList<'_> {
        type Value = ();

        fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
            decoder.deserialize_seq(self)
        }
    }

    impl Reasons<'_> {
        fn find<'de, A: SeqAccess<'de>>(self, mut changes: A) -> Result<(), A::Error> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Change {
                id: String,
                reason: String,
                #[serde(default, rename = "anchor")]
                _anchor: Option<IgnoredAny>,
            }

            for _ in self.items {
                let Some(change) = changes.next_element::<Change>()? else {
                    break;
                };
                let reason = Zeroizing::new(change.reason);
                if self.items.iter().any(|item| item.id == change.id)
                    && !reason.trim().is_empty()
                    && reason.len() <= 4096
                    && reason.chars().all(|character| !character.is_control())
                {
                    *self.result = Some(reason);
                    break;
                }
            }
            Ok(())
        }
    }

    impl<'de> Visitor<'de> for ChangeList<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("complete selected change reasons")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, changes: A) -> Result<(), A::Error> {
            self.0.find(changes)
        }
    }

    let mut result = None;
    let mut decoder = serde_json::Deserializer::from_str(reply::strip_fence(input.trim_start()));
    let _ = decoder.deserialize_map(Reasons {
        items,
        result: &mut result,
    });
    result
}

#[cfg(test)]
mod tests;
