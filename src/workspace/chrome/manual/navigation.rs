//! Resolves only locations in the compiled manual, never files or URLs.

use std::ops::Range;

use super::{Chapter, Display};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Location {
    pub(super) chapter: usize,
    pub(super) block: Option<usize>,
    pub(super) excerpt: Option<Range<usize>>,
}

impl Location {
    pub(super) fn chapter(chapter: usize) -> Self {
        Self {
            chapter,
            block: None,
            excerpt: None,
        }
    }

    pub(super) fn valid(&self, chapters: &[Chapter]) -> bool {
        let Some(chapter) = chapters.get(self.chapter) else {
            return false;
        };
        let Some(index) = self.block else {
            return self.excerpt.is_none();
        };
        let Some(block) = chapter.blocks.get(index) else {
            return false;
        };
        self.excerpt
            .as_ref()
            .is_none_or(|range| range.start < range.end && block.text.get(range.clone()).is_some())
    }

    pub(super) fn title<'a>(&self, chapters: &'a [Chapter]) -> Option<&'a str> {
        let chapter = chapters.get(self.chapter)?;
        let section = self
            .block
            .and_then(|index| chapter.blocks.get(..=index))
            .and_then(|blocks| {
                blocks
                    .iter()
                    .rev()
                    .find(|block| matches!(block.display, Display::Heading { .. }))
            });
        Some(section.map_or(&chapter.title, |block| &block.text))
    }
}

pub(super) fn resolve(chapters: &[Chapter], target: &str) -> Option<Location> {
    let anchor = target
        .strip_prefix('#')
        .filter(|anchor| valid_anchor(anchor))?;
    let mut found = None;
    for (chapter, page) in chapters.iter().enumerate() {
        if page.anchor == anchor {
            if found.is_some() {
                return None;
            }
            found = Some(Location::chapter(chapter));
        }
        for (block, item) in page.blocks.iter().enumerate() {
            if matches!(&item.display, Display::Heading { anchor: value } if value == anchor) {
                if found.is_some() {
                    return None;
                }
                found = Some(Location {
                    chapter,
                    block: Some(block),
                    excerpt: None,
                });
            }
        }
    }
    found
}

pub(super) fn heading(value: &str, parent: Option<&str>) -> (String, String) {
    let value = value.trim();
    if let Some((title, anchor)) = value.rsplit_once(" {#")
        && let Some(anchor) = anchor
            .strip_suffix('}')
            .filter(|anchor| valid_anchor(anchor))
        && !title.trim().is_empty()
    {
        return (title.trim().into(), anchor.into());
    }
    let slug = value
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let anchor = parent.map_or_else(|| slug.clone(), |parent| format!("{parent}-{slug}"));
    (value.into(), anchor)
}

fn valid_anchor(anchor: &str) -> bool {
    !anchor.is_empty()
        && anchor.len() <= 160
        && anchor
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_locations_are_exact_unambiguous_and_do_not_open_resources() {
        let chapters = super::super::chapters(
            "# Guide {#guide}\n\n### Open {#open}\n\nBody.\n\n```\n# Not a chapter\n```\n\n## Other {#other}\n\n### Open\n\nMore.\n",
        );
        assert_eq!(chapters.len(), 2);
        assert_eq!(resolve(&chapters, "#open").unwrap().block, Some(0));
        assert_eq!(resolve(&chapters, "#other-open").unwrap().chapter, 1);
        for target in [
            "#unknown",
            "https://example.invalid/#open",
            "file:///guide#open",
            "../guide#open",
            "guide.html#open",
            "#%6fpen",
            "#../open",
            "#",
            "#Open",
        ] {
            assert!(resolve(&chapters, target).is_none(), "{target}");
        }
        let duplicate = super::super::chapters("# First {#same}\n\n## Second {#same}\n");
        assert!(resolve(&duplicate, "#same").is_none());
        let unicode = super::super::chapters("# Guide\n\nä sample\n");
        assert!(
            !Location {
                chapter: 0,
                block: Some(0),
                excerpt: Some(1..2)
            }
            .valid(&unicode)
        );
        assert!(
            Location {
                chapter: 0,
                block: Some(0),
                excerpt: Some(0..2)
            }
            .valid(&unicode)
        );
    }
}
