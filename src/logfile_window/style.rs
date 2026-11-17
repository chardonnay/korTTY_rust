//! Display-only logfile styling with bounded per-page caching.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use iced::widget::{span, text::Span};
use iced::{Color, Font};
use kortty_terminal::{Color as TerminalColor, Palette, Style};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::terminal_highlight::CompiledSet;
use crate::terminal_links::{address_candidates, email_candidates, printed_link};
use crate::terminal_view::quick_select::printed_path_ranges;

use super::document::Page;

pub(crate) const DEFAULT_SET: &str = "builtin.errors";
const MAX_MATCHES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) links: bool,
    pub(crate) keywords: bool,
    pub(crate) keyword_set: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            links: true,
            keywords: true,
            keyword_set: DEFAULT_SET.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    pub(crate) id: String,
    pub(crate) name: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    pub(super) revision: u64,
    pub(super) line: usize,
    pub(super) range: Range<usize>,
}

pub(crate) struct Context<'a> {
    pub(crate) preferences: &'a Preferences,
    pub(crate) options: &'a [(String, String)],
    pub(crate) rules: Option<&'a Arc<CompiledSet>>,
    pub(crate) palette: Palette,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Kind {
    Keyword(Style),
    Link(Range<usize>),
    Find,
}

struct Styled {
    range: Range<usize>,
    kind: Kind,
}

#[derive(Default)]
pub(super) struct Cache {
    key: Option<(u64, bool, Option<Arc<CompiledSet>>, String)>,
    rows: Vec<Vec<Styled>>,
}

fn marks(
    line: &str,
    links: bool,
    rules: Option<&CompiledSet>,
    search: Option<&Regex>,
) -> Vec<Styled> {
    if !links && rules.is_none() && search.is_none() {
        return Vec::new();
    }
    // Fill winning owners in priority order. Byte ranges originate at UTF-8 boundaries.
    let mut owners = vec![None; line.len()];
    let mut styles = Vec::new();
    let mut paint = |range: Range<usize>, kind: Kind| {
        if owners[range.clone()].iter().any(Option::is_none) {
            let index = styles.len();
            for owner in &mut owners[range] {
                owner.get_or_insert(index);
            }
            styles.push(kind);
        }
    };
    if let Some(search) = search {
        for hit in search.find_iter(line).take(MAX_MATCHES) {
            paint(hit.range(), Kind::Find);
        }
    }
    if links {
        let addresses = address_candidates(line).map(|(value, start)| (value, start, false));
        let emails = email_candidates(line).map(|(value, start)| (value, start, true));
        let mut links: Vec<_> = addresses
            .chain(emails)
            .filter_map(|(value, start, email)| {
                printed_link(value, email).map(|_| start..start + value.len())
            })
            .take(MAX_MATCHES)
            .collect();
        for range in printed_path_ranges(line).take(MAX_MATCHES) {
            if !links
                .iter()
                .any(|link| range.start < link.end && link.start < range.end)
            {
                links.push(range);
            }
        }
        links.sort_unstable_by_key(|range| (range.start, std::cmp::Reverse(range.end)));
        for range in links {
            paint(range.clone(), Kind::Link(range));
        }
    }
    if let Some(rules) = rules {
        for (range, style) in rules.presentation_ranges(line).take(MAX_MATCHES) {
            paint(range, Kind::Keyword(style));
        }
    }
    let mut start = 0;
    let mut spans = Vec::new();
    while start < owners.len() {
        let index = owners[start];
        let end = start
            + owners[start..]
                .iter()
                .take_while(|owner| **owner == index)
                .count();
        if let Some(index) = index {
            spans.push(Styled {
                range: start..end,
                kind: styles[index].clone(),
            });
        }
        start = end;
    }
    spans
}

impl Cache {
    pub(super) fn prepare(
        &mut self,
        page: &Page,
        revision: u64,
        context: &Context<'_>,
        query: &str,
    ) {
        let rules = context.rules.filter(|_| context.preferences.keywords);
        if self
            .key
            .as_ref()
            .is_some_and(|(cached_revision, links, cached_rules, cached_query)| {
                *cached_revision == revision
                    && *links == context.preferences.links
                    && cached_query == query
                    && match (cached_rules.as_ref(), rules) {
                        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
                        (None, None) => true,
                        _ => false,
                    }
            })
        {
            return;
        }
        let search = (!query.is_empty())
            .then(|| {
                RegexBuilder::new(&regex::escape(query))
                    .case_insensitive(true)
                    .build()
                    .ok()
            })
            .flatten();
        self.rows = page
            .lines
            .iter()
            .map(|line| {
                marks(
                    line,
                    context.preferences.links,
                    rules.map(Arc::as_ref),
                    search.as_ref(),
                )
            })
            .collect();
        self.key = Some((
            revision,
            context.preferences.links,
            rules.cloned(),
            query.to_owned(),
        ));
    }

    pub(super) fn line<'a>(
        &self,
        text: &'a str,
        row: usize,
        revision: u64,
        palette: Palette,
        current: bool,
    ) -> Vec<Span<'a, Link, Font>> {
        let mut spans = Vec::new();
        let mut start = 0;
        for styled in &self.rows[row] {
            if start < styled.range.start {
                spans.push(span(&text[start..styled.range.start]));
            }
            let fragment = span(&text[styled.range.clone()]);
            spans.push(match &styled.kind {
                Kind::Keyword(style) => keyword(fragment, *style, palette),
                Kind::Link(range) => fragment
                    .underline(true)
                    .color(Color::from_rgb8(54, 138, 245))
                    .link(Link {
                        revision,
                        line: row,
                        range: range.clone(),
                    }),
                Kind::Find => fragment.color(Color::BLACK).background(if current {
                    Color::from_rgb8(255, 157, 64)
                } else {
                    Color::from_rgb8(246, 217, 108)
                }),
            });
            start = styled.range.end;
        }
        if start < text.len() || spans.is_empty() {
            spans.push(span(&text[start..]));
        }
        spans
    }

    pub(super) fn permits_link(&self, link: &Link) -> bool {
        self.key
            .as_ref()
            .is_some_and(|(revision, links, _, _)| *links && *revision == link.revision)
            && self.rows.get(link.line).is_some_and(|row| {
                row.iter()
                    .any(|styled| matches!(&styled.kind, Kind::Link(range) if *range == link.range))
            })
    }
}

fn keyword<'a>(span: Span<'a, Link, Font>, style: Style, palette: Palette) -> Span<'a, Link, Font> {
    let color = |value| {
        let [r, g, b] = match value {
            TerminalColor::Default => return None,
            TerminalColor::Indexed(index) => palette.rgb(index),
            TerminalColor::Rgb(r, g, b) => [r, g, b],
        };
        Some(Color::from_rgb8(r, g, b))
    };
    let mut font = Font::MONOSPACE;
    if style.bold {
        font.weight = iced::font::Weight::Bold;
    }
    if style.italic {
        font.style = iced::font::Style::Italic;
    }
    span.color_maybe(color(style.foreground))
        .background_maybe(color(style.background))
        .underline(style.underline)
        .font(font)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logfile_style_layers_keep_the_whole_link_under_find_matches() {
        let rules = crate::terminal_highlight::builtins()
            .into_iter()
            .find(|set| set.id == DEFAULT_SET)
            .unwrap()
            .compile()
            .unwrap();
        let line = "é Warning https://example.test/error /var/log/output.log";
        let search = Regex::new("error").unwrap();
        let styled = marks(line, true, Some(&rules), Some(&search));
        assert!(
            styled
                .iter()
                .any(|span| matches!(span.kind, Kind::Keyword(_))
                    && &line[span.range.clone()] == "Warning")
        );
        assert!(styled.iter().any(|span| matches!(span.kind, Kind::Find) && &line[span.range.clone()] == "error"));
        assert!(styled.iter().any(|span| matches!(&span.kind, Kind::Link(full) if &line[full.clone()] == "https://example.test/error")
            && &line[span.range.clone()] == "https://example.test/"));
        assert!(styled.iter().any(|span| matches!(&span.kind, Kind::Link(full) if &line[full.clone()] == "/var/log/output.log")));
        assert!(
            !marks(line, false, Some(&rules), None)
                .iter()
                .any(|span| matches!(span.kind, Kind::Link(_)))
        );
    }

    #[test]
    fn logfile_style_link_copy_requires_a_current_page_and_rendered_token() {
        let prefs = Preferences::default();
        let context = Context {
            preferences: &prefs,
            options: &[],
            rules: None,
            palette: Palette::default(),
        };
        let page = Page {
            first: 0,
            lines: vec!["https://example.test".into()],
        };
        let mut cache = Cache::default();
        cache.prepare(&page, 1, &context, "");
        let link = Link {
            revision: 1,
            line: 0,
            range: 0..page.lines[0].len(),
        };
        assert!(cache.permits_link(&link));
        assert!(!cache.permits_link(&Link {
            range: 1..page.lines[0].len(),
            ..link.clone()
        }));
        cache.prepare(&page, 2, &context, "");
        assert!(!cache.permits_link(&link));
    }
}
