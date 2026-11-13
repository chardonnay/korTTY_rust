//! Preview references into reviewed evidence while the structured reply is still open.

use std::fmt;

use serde::de::{DeserializeSeed, Deserializer as _, MapAccess, SeqAccess, Visitor};
use zeroize::{Zeroize, Zeroizing};

use super::{Citation, Finding, MAX_FINDINGS, MAX_REPLY_BYTES, Quotation, StreamDelta};

pub(super) fn reporter<'a>(
    citations: &'a [Citation],
    mut emit: impl FnMut(Vec<Quotation>) + 'a,
) -> impl FnMut(StreamDelta) + 'a {
    let mut answer = Zeroizing::new(String::new());
    let mut shown = 0;
    let mut exceeded = false;
    move |delta| {
        let mut text = match delta {
            StreamDelta::Text(text) => text,
            StreamDelta::Reasoning(mut text) => {
                text.zeroize();
                return;
            }
        };
        if !exceeded {
            if answer.len() + text.len() > MAX_REPLY_BYTES {
                exceeded = true;
                answer.zeroize();
                if shown != 0 {
                    emit(Vec::new());
                }
            } else {
                answer.push_str(&text);
                // A complete finding always ends at an object delimiter.
                if text.contains('}') && shown < MAX_FINDINGS {
                    let findings = quotations(&answer, citations);
                    if findings.len() > shown {
                        shown = findings.len();
                        emit(findings);
                    }
                }
            }
        }
        text.zeroize();
    }
}

fn quotations(input: &str, citations: &[Citation]) -> Vec<Quotation> {
    struct Findings<'a> {
        citations: &'a [Citation],
        preview: &'a mut Vec<Quotation>,
    }

    impl<'de> DeserializeSeed<'de> for Findings<'_> {
        type Value = ();

        fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
            decoder.deserialize_seq(self)
        }
    }

    impl<'de> Visitor<'de> for Findings<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an array of exact journal quotations")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<(), A::Error> {
            for _ in 0..MAX_FINDINGS {
                let Some(mut finding) = array.next_element::<Finding>()? else {
                    break;
                };
                if let Some(quote) = finding.quotation(self.citations)
                    && !self.preview.contains(&quote)
                {
                    self.preview.push(quote);
                }
                finding.quote.zeroize();
            }
            Ok(())
        }
    }

    struct Envelope<'a>(Findings<'a>);

    impl<'de> Visitor<'de> for Envelope<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object with journal findings")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut fields: A) -> Result<(), A::Error> {
            if fields.next_key::<String>()?.as_deref() == Some("findings") {
                fields.next_value_seed(self.0)?;
            }
            Ok(())
        }
    }

    let mut preview = Vec::new();
    // Complete array items may be available before the enclosing JSON is finished.
    let _ = serde_json::Deserializer::from_str(input).deserialize_map(Envelope(Findings {
        citations,
        preview: &mut preview,
    }));
    preview
}
