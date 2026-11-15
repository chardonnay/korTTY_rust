//! Transient progress and exact reviewed keywords from a bounded structured reply.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use serde::de::{DeserializeSeed, Deserializer as _, IgnoredAny, MapAccess, SeqAccess, Visitor};
use zeroize::{Zeroize, Zeroizing};

use crate::local_ai::StreamDelta;

use super::{MAX_KEYWORDS, MAX_RESPONSE_BYTES, Reply, verified_keyword};

const UPDATE_INTERVAL: Duration = Duration::from_millis(75);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    Checking,
    Window,
    Closing,
    Title,
    Screenshot,
}

impl Stage {
    pub(crate) fn weight(self, prompt_chars: usize) -> f64 {
        match self {
            Self::Checking => 0.0,
            Self::Window => 1.0 + prompt_chars as f64 / 12_000.0,
            Self::Closing => 1.3,
            Self::Title => 1.0,
            Self::Screenshot => 1.5,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Work {
    Plan {
        stage: Stage,
        weight: f64,
    },
    Started {
        stage: Stage,
        weight: f64,
        at: Instant,
    },
    Finished {
        stage: Stage,
        weight: f64,
        at: Instant,
        elapsed: Duration,
    },
}

#[derive(Clone)]
pub(crate) struct Preview {
    pub(crate) stage: Stage,
    pub(crate) answer_bytes: usize,
    pub(crate) keywords: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Reporter<'a> {
    show_keywords: bool,
    update: Option<&'a mut dyn FnMut(Option<Preview>)>,
    work: Option<&'a mut dyn FnMut(Work)>,
    window: Option<WindowWork>,
}

struct WindowWork {
    weight: f64,
    started: bool,
    elapsed: Duration,
}

impl<'a> Reporter<'a> {
    pub(crate) fn new(
        show_keywords: bool,
        update: Option<&'a mut dyn FnMut(Option<Preview>)>,
        work: Option<&'a mut dyn FnMut(Work)>,
    ) -> Self {
        Self {
            show_keywords,
            update,
            work,
            window: None,
        }
    }

    pub(crate) fn tracks_work(&self) -> bool {
        self.work.is_some()
    }

    pub(crate) fn plan(&mut self, stage: Stage, weight: f64) {
        self.publish_work(Work::Plan { stage, weight });
    }

    pub(crate) fn start_work(&mut self, stage: Stage, weight: f64) -> Instant {
        let at = Instant::now();
        self.publish_work(Work::Started { stage, weight, at });
        at
    }

    pub(in crate::session_journal) fn finish_elapsed(
        &mut self,
        stage: Stage,
        weight: f64,
        elapsed: Duration,
    ) {
        self.publish_work(Work::Finished {
            stage,
            weight,
            at: Instant::now(),
            elapsed,
        });
    }

    /// A window may require several consecutive model requests before its card is ready.
    pub(crate) fn with_window<T>(
        &mut self,
        weight: f64,
        execute: impl FnOnce(&mut Self) -> Reply<T>,
    ) -> Reply<T> {
        if !self.tracks_work() {
            return execute(self);
        }
        self.window = Some(WindowWork {
            weight,
            started: false,
            elapsed: Duration::ZERO,
        });
        let reply = execute(self);
        if let Some(window) = self.window.take().filter(|window| window.started) {
            self.finish_elapsed(Stage::Window, window.weight, window.elapsed);
        }
        reply
    }

    pub(super) fn start_window(&mut self) -> Option<Instant> {
        let window = self.window.as_mut()?;
        Some(if window.started {
            Instant::now()
        } else {
            window.started = true;
            let weight = window.weight;
            self.start_work(Stage::Window, weight)
        })
    }

    pub(super) fn add_window_elapsed(&mut self, elapsed: Duration) {
        if let Some(window) = &mut self.window {
            window.elapsed = window.elapsed.saturating_add(elapsed);
        }
    }

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

    fn publish_work(&mut self, work: Work) {
        if let Some(update) = &mut self.work {
            update(work);
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
