use std::io;
use std::ops::Range;

use roxmltree::Node;
use zeroize::Zeroizing;

use crate::session_journal::metadata::{escape, xml_character};
use crate::session_journal::{MAX_DOCUMENT_BYTES, invalid, parse_document, scalar_text};

use super::rule::Rule;

#[derive(Clone, Copy)]
enum Container {
    Document,
    Entries,
    Entry,
    Excerpt,
    Tags,
    Chat,
    Messages,
    Message,
    Steps,
    Step,
}

/// Rewrites only the JAXB text fields exposed by the pinned journal replacement service.
pub(super) fn rewrite(xml: &str, rule: &Rule) -> io::Result<(usize, Option<Zeroizing<String>>)> {
    if xml.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(too_large());
    }
    let document = parse_document(xml)?;
    let root = document.root_element();
    if root.tag_name().namespace().is_some() {
        return Err(invalid(
            "Unexpected namespace in the session journal document",
        ));
    }
    let mut rewrite = Rewrite {
        xml,
        rule,
        cursor: 0,
        output: None,
        hits: 0,
    };
    rewrite.visit(root, Container::Document)?;
    rewrite.finish()
}

struct Rewrite<'a> {
    xml: &'a str,
    rule: &'a Rule,
    cursor: usize,
    output: Option<Zeroizing<String>>,
    hits: usize,
}

impl Rewrite<'_> {
    fn visit(&mut self, parent: Node<'_, '_>, scope: Container) -> io::Result<()> {
        use Container::*;

        for node in parent
            .children()
            .filter(|node| node.is_element() && node.tag_name().namespace().is_none())
        {
            let nested = match (scope, node.tag_name().name()) {
                (Document, "entries") => Entries,
                (Entries, "entry") => Entry,
                (Entry, "inputExcerpt" | "outputExcerpt") => Excerpt,
                (Entry, "aiTags") => Tags,
                (Entry, "chatSnapshot") => Chat,
                (Chat, "messages") => Messages,
                (Messages, "message") => Message,
                (Message, "steps") => Steps,
                (Steps, "step") => Step,
                (Entry, "title" | "text" | "userNote" | "aiDescription")
                | (Excerpt, "line")
                | (Tags, "tag")
                | (Chat, "title")
                | (Message, "content" | "aiProfileName")
                | (Step, "title" | "summary" | "command" | "output") => {
                    self.scalar(node)?;
                    continue;
                }
                _ => continue,
            };
            self.visit(node, nested)?;
        }
        Ok(())
    }

    fn scalar(&mut self, node: Node<'_, '_>) -> io::Result<()> {
        let value = scalar_text(node)?;
        let Some(updated) = self.rule.rewrite(&value)? else {
            return Ok(());
        };
        if !updated.chars().all(xml_character) {
            return Err(invalid("A journal replacement is invalid in XML text"));
        }
        let escaped = Zeroizing::new(escape(&updated));
        let range = content_range(self.xml, node)?;
        let mut start = range.start;
        let mut replacement = escaped.as_str();
        // Adjacent CDATA and text can share one parser node with only the first source range.
        // Patch the gaps around comments and processing instructions instead of their node ranges.
        for markup in node
            .children()
            .filter(|node| node.is_comment() || node.is_pi())
        {
            let markup = markup.range();
            if start < markup.start {
                self.patch(start..markup.start, replacement)?;
                replacement = "";
            }
            start = markup.end;
        }
        self.patch(start..range.end, replacement)?;
        self.hits += 1;
        Ok(())
    }

    fn patch(&mut self, range: Range<usize>, replacement: &str) -> io::Result<()> {
        let unchanged = self
            .xml
            .get(self.cursor..range.start)
            .ok_or_else(bad_range)?;
        if self.xml.get(range.clone()).is_none() {
            return Err(bad_range());
        }
        let output = self
            .output
            .get_or_insert_with(|| Zeroizing::new(String::with_capacity(self.xml.len())));
        append(output, unchanged)?;
        append(output, replacement)?;
        self.cursor = range.end;
        Ok(())
    }

    fn finish(self) -> io::Result<(usize, Option<Zeroizing<String>>)> {
        let Some(mut output) = self.output else {
            return Ok((0, None));
        };
        append(&mut output, &self.xml[self.cursor..])?;
        parse_document(&output)?;
        Ok((self.hits, Some(output)))
    }
}

fn content_range(xml: &str, node: Node<'_, '_>) -> io::Result<Range<usize>> {
    let range = node.range();
    let source = xml.get(range.clone()).ok_or_else(bad_range)?;
    let mut quote = None;
    let start = source.bytes().position(|byte| match quote {
        Some(active) => {
            if byte == active {
                quote = None;
            }
            false
        }
        None => {
            if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            }
            byte == b'>'
        }
    });
    let start = start.ok_or_else(bad_range)? + 1;
    let end = source
        .rfind("</")
        .filter(|&end| end >= start)
        .ok_or_else(bad_range)?;
    Ok(range.start + start..range.start + end)
}

fn append(output: &mut String, text: &str) -> io::Result<()> {
    if output.len().saturating_add(text.len()) as u64 > MAX_DOCUMENT_BYTES {
        return Err(too_large());
    }
    output.push_str(text);
    Ok(())
}

fn too_large() -> io::Error {
    invalid("Session journal replacement exceeds the XML size limit")
}

fn bad_range() -> io::Error {
    invalid("Invalid session journal replacement XML range")
}

#[cfg(test)]
mod tests;
