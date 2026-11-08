//! Text shared by the timeline scanner and native widgets.

use std::borrow::Cow;

use iced::widget::{rich_text, span, text};
use iced::{Color, Element, Font, Length};

use crate::workspace::journal_appearance::Content;

use super::super::{LinkField, LinkTarget, MAX_DETAIL, Message, TimelineEntry, label, timestamp};
use super::Hit;

const ROW_FIELDS: usize = 3;
const MAX_DETAIL_HIGHLIGHTS: usize = 1024;
type DetailLink = (LinkField, LinkTarget);

pub(in crate::workspace::journal_access) struct Projection<'a> {
    entry: &'a TimelineEntry,
    pub(in crate::workspace::journal_access) row: [String; ROW_FIELDS],
    details: Vec<Detail<'a>>,
    first_chat: usize,
}

struct Detail<'a> {
    value: &'a str,
    link: Option<(LinkField, usize)>,
    terminal: Option<TerminalText>,
    clipped: bool,
}

#[derive(Clone, Copy)]
enum TerminalText {
    Input,
    Output,
}

#[derive(Clone, Copy)]
struct RenderText {
    content: Content,
    terminal: Option<TerminalText>,
}

impl<'a> Projection<'a> {
    pub(in crate::workspace::journal_access) fn new(entry: &'a TimelineEntry) -> Self {
        let row = Self::row_fields(entry);
        let mut details: Vec<_> = entry
            .extra_details
            .iter()
            .map(|value| Detail::plain(value))
            .collect();
        let parts = [
            (Some(LinkField::Text), &entry.text, None),
            (Some(LinkField::Note), &entry.note, None),
        ]
        .into_iter()
        .chain(
            entry
                .input
                .iter()
                .map(|value| (None, value, Some(TerminalText::Input))),
        )
        .chain(
            entry
                .output
                .iter()
                .map(|value| (None, value, Some(TerminalText::Output))),
        )
        .filter(|(_, value, _)| !value.is_empty());
        let mut remaining = MAX_DETAIL;
        let mut previous = false;
        for (field, value, terminal) in parts {
            if previous {
                if remaining == 0 {
                    details.push(Detail::plain("…"));
                    break;
                }
                remaining -= 1;
            }
            let end = value
                .char_indices()
                .nth(remaining)
                .map_or(value.len(), |(offset, _)| offset);
            let clipped = end < value.len();
            details.push(Detail {
                value: &value[..end],
                link: field.map(|field| (field, remaining)),
                terminal,
                clipped,
            });
            if clipped {
                break;
            }
            remaining -= value.chars().count();
            previous = true;
        }
        let first_chat = details.len() + ROW_FIELDS;
        details.extend(entry.chat_details.iter().map(|value| Detail::plain(value)));
        Self {
            entry,
            row,
            details,
            first_chat,
        }
    }

    pub(in crate::workspace::journal_access) fn row_fields(
        entry: &TimelineEntry,
    ) -> [String; ROW_FIELDS] {
        let marker = if entry.marker_name.is_empty() {
            &entry.kind
        } else {
            &entry.marker_name
        };
        let title = if entry.title.is_empty() {
            &entry.text
        } else {
            &entry.title
        };
        [
            timestamp(entry.created_at),
            label(marker, 25),
            label(title, 160),
        ]
    }

    pub(super) fn into_fields(self) -> Vec<Cow<'a, str>> {
        self.row
            .into_iter()
            .map(Cow::Owned)
            .chain(
                self.details
                    .into_iter()
                    .map(|field| Cow::Borrowed(field.value)),
            )
            .collect()
    }

    pub(in crate::workspace::journal_access) fn first_chat_field(&self) -> usize {
        self.first_chat
    }

    pub(in crate::workspace::journal_access) fn has_link(
        &self,
        field: LinkField,
        target: &LinkTarget,
    ) -> bool {
        self.details.iter().any(|detail| {
            detail.link.is_some_and(|(candidate, limit)| {
                candidate == field
                    && self
                        .entry
                        .link_spans(field, limit)
                        .iter()
                        .any(|segment| segment.target == Some(target))
            })
        })
    }

    pub(in crate::workspace::journal_access) fn detail_spans(
        &self,
        hits: &[Hit],
        active: Option<&Hit>,
        links_allowed: bool,
        chat_open: bool,
        content: Content,
    ) -> (Vec<text::Span<'a, DetailLink, Font>>, bool) {
        let start = hits.partition_point(|hit| hit.field < ROW_FIELDS);
        let end = if chat_open {
            hits.len()
        } else {
            hits.partition_point(|hit| hit.field < self.first_chat)
        };
        let hits = &hits[start..end];
        let limited = hits.len() > MAX_DETAIL_HIGHLIGHTS;
        let hits = if limited {
            let position = active
                .filter(|hit| hit.entry == hits[0].entry)
                .and_then(|hit| {
                    hits.binary_search_by_key(&(hit.field, hit.range.start), |candidate| {
                        (candidate.field, candidate.range.start)
                    })
                    .ok()
                })
                .unwrap_or(0);
            let start = position
                .saturating_sub(MAX_DETAIL_HIGHLIGHTS / 2)
                .min(hits.len() - MAX_DETAIL_HIGHLIGHTS);
            &hits[start..start + MAX_DETAIL_HIGHLIGHTS]
        } else {
            hits
        };
        let mut spans = Vec::new();
        for (index, detail) in self.details.iter().enumerate() {
            let field_index = ROW_FIELDS + index;
            if field_index >= self.first_chat && !chat_open {
                break;
            }
            if !spans.is_empty() {
                spans.push(span("\n"));
            }
            let hits = field_hits(hits, field_index);
            let style = RenderText {
                content,
                terminal: detail.terminal,
            };
            if links_allowed && let Some((field, limit)) = detail.link {
                let mut offset = 0;
                for segment in self.entry.link_spans(field, limit) {
                    let link = segment.target.map(|target| (field, target.clone()));
                    append_marked(&mut spans, segment.text, offset, hits, active, link, style);
                    offset += segment.text.len();
                }
            } else {
                append_marked(&mut spans, detail.value, 0, hits, active, None, style);
                if detail.clipped {
                    spans.push(span("…"));
                }
            }
        }
        (spans, limited)
    }

    pub(in crate::workspace::journal_access) fn context(
        &self,
        hit: &Hit,
        content: Content,
    ) -> Vec<text::Span<'static, (), Font>> {
        let Some(value) = self.field_value(hit.field) else {
            return Vec::new();
        };
        let start = value[..hit.range.start]
            .char_indices()
            .rev()
            .nth(42)
            .map_or(0, |(index, _)| index);
        let end = value[hit.range.end..]
            .char_indices()
            .nth(42)
            .map_or(value.len(), |(index, _)| hit.range.end + index);
        let mut spans = Vec::new();
        append_marked(
            &mut spans,
            &value[start..end],
            start,
            std::slice::from_ref(hit),
            Some(hit),
            None,
            RenderText {
                content,
                terminal: None,
            },
        );
        let mut spans: Vec<_> = spans
            .into_iter()
            .map(|part| text::Span {
                text: Cow::Owned(part.text.replace(['\n', '\r', '\t'], " ")),
                ..part
            })
            .collect();
        if start > 0 {
            spans.insert(0, span("…"));
        }
        if end < value.len() {
            spans.push(span("…"));
        }
        spans
    }

    pub(super) fn field_value(&self, field: usize) -> Option<&str> {
        self.row.get(field).map(String::as_str).or_else(|| {
            self.details
                .get(field.checked_sub(ROW_FIELDS)?)
                .map(|detail| detail.value)
        })
    }
}

impl<'a> Detail<'a> {
    fn plain(value: &'a str) -> Self {
        Self {
            value,
            link: None,
            terminal: None,
            clipped: false,
        }
    }
}

pub(in crate::workspace::journal_access) fn row_field(
    value: String,
    width: Length,
    field: usize,
    hits: &[Hit],
    active: Option<&Hit>,
    content: Content,
) -> Element<'static, Message> {
    use text::Wrapping;

    let hits = field_hits(hits, field);
    if hits.is_empty() {
        return text(value)
            .font(content.ui_font)
            .size(content.size(13.0))
            .width(width)
            .wrapping(Wrapping::None)
            .into();
    }
    let mut spans = Vec::new();
    append_marked(
        &mut spans,
        &value,
        0,
        hits,
        active,
        None,
        RenderText {
            content,
            terminal: None,
        },
    );
    let spans: Vec<text::Span<'static, (), Font>> = spans
        .into_iter()
        .map(|part| text::Span {
            text: Cow::Owned(part.text.into_owned()),
            ..part
        })
        .collect();
    rich_text(spans)
        .font(content.ui_font)
        .size(content.size(13.0))
        .width(width)
        .wrapping(Wrapping::None)
        .into()
}

fn field_hits(hits: &[Hit], field: usize) -> &[Hit] {
    let start = hits.partition_point(|hit| hit.field < field);
    let end = hits.partition_point(|hit| hit.field <= field);
    &hits[start..end]
}

fn append_marked<'a, L: Clone>(
    spans: &mut Vec<text::Span<'a, L, Font>>,
    value: &'a str,
    offset: usize,
    hits: &[Hit],
    active: Option<&Hit>,
    link: Option<L>,
    style: RenderText,
) {
    let mut push = |part: &'a str, hit: Option<&Hit>| {
        if part.is_empty() {
            return;
        }
        let mut styled = span(part);
        if let Some(kind) = style.terminal {
            let colors = style.content.palette.colors;
            let color = match kind {
                TerminalText::Input => colors.input,
                TerminalText::Output => colors.output,
            };
            styled = styled.font(style.content.mono_font).color(color.color());
        }
        if let Some(link) = &link {
            styled = styled
                .underline(true)
                .link(link.clone())
                .color(style.content.palette.colors.accent.color());
        }
        if let Some(hit) = hit {
            styled = styled
                .background(style.content.highlight(active == Some(hit)))
                .color(Color::BLACK);
        }
        spans.push(styled);
    };
    let first = hits.partition_point(|hit| hit.range.end <= offset);
    let mut cursor = 0;
    for hit in hits[first..]
        .iter()
        .take_while(|hit| hit.range.start < offset + value.len())
    {
        let start = hit.range.start.saturating_sub(offset);
        let end = (hit.range.end - offset).min(value.len());
        push(&value[cursor..start], None);
        push(&value[start..end], Some(hit));
        cursor = end;
    }
    push(&value[cursor..], None);
}
