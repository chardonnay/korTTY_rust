//! Field-scoped publication of an analysis for the exact reviewed screenshot.

use std::fmt::Write as _;
use std::io;
use std::ops::Range;

use roxmltree::Node;

use crate::local_ai::MAX_MODEL_BYTES;

use super::entry_edit::{
    append_children, apply_patches, find_entry, lock_source, patch_scalars, require_java_fields,
};
use super::metadata::{escape, xml_character};
use super::screenshot_ai::{Analysis, MAX_DESCRIPTION_BYTES, MAX_TAG_BYTES, MAX_TAGS};
use super::viewer::Screenshot;
use super::{document_id, field, invalid, parse_document, scalar_text};

const DESCRIPTION: &str = "aiDescription";
const TAGS: &str = "aiTags";
const MODEL: &str = "aiAnalysisModel";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) changed: bool,
    pub(crate) durability_warning: bool,
}

/// Stores only analysis fields in the latest stopped journal document.
pub(crate) fn apply(asset: &Screenshot, analysis: &Analysis) -> io::Result<Outcome> {
    validate(analysis)?;
    let source = asset.source();
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The selected session journal identity changed"));
    }
    let entry = find_entry(&document, asset.id())?;
    asset.require_entry(entry)?;
    let updated = patch(&xml, entry, analysis)?;
    lock.publish_document(&stamp, updated.as_deref(), |journal| {
        asset.require_image_current_in(journal)
    })?;
    let changed = updated.is_some();
    Ok(Outcome {
        changed,
        durability_warning: changed && lock.directory().sync().is_err(),
    })
}

pub(super) fn validate(analysis: &Analysis) -> io::Result<()> {
    let valid = |text: &str, limit| text.len() <= limit && text.chars().all(xml_character);
    if (analysis.description.trim().is_empty() && analysis.tags.is_empty())
        || !valid(&analysis.description, MAX_DESCRIPTION_BYTES)
        || !valid(&analysis.model, MAX_MODEL_BYTES)
        || analysis.tags.len() > MAX_TAGS
        || analysis
            .tags
            .iter()
            .any(|tag| tag.trim().is_empty() || !valid(tag, MAX_TAG_BYTES))
    {
        return Err(invalid("Invalid or oversized screenshot analysis"));
    }
    Ok(())
}

pub(super) fn text_bytes(analysis: &Analysis) -> usize {
    analysis.description.len()
        + analysis.model.len()
        + analysis.tags.iter().map(String::len).sum::<usize>()
}

pub(super) fn write_xml(xml: &mut String, analysis: &Analysis) {
    if !analysis.description.is_empty() {
        element(xml, DESCRIPTION, &analysis.description);
    }
    if !analysis.tags.is_empty() {
        xml.push_str("<aiTags>");
        append_tags(xml, &analysis.tags);
        xml.push_str("</aiTags>");
    }
    if !analysis.model.is_empty() {
        element(xml, MODEL, &analysis.model);
    }
}

fn patch(xml: &str, entry: Node<'_, '_>, analysis: &Analysis) -> io::Result<Option<String>> {
    require_java_fields(entry, &[DESCRIPTION, TAGS, MODEL])?;
    let mut patches = Vec::new();
    let mut values = Vec::new();
    for (name, value) in [
        (DESCRIPTION, analysis.description.as_str()),
        (MODEL, analysis.model.as_str()),
    ] {
        if field(entry, name)?.as_deref().unwrap_or_default() != value {
            values.push((name, (!value.is_empty()).then_some(value)));
        }
    }
    patch_scalars(xml, entry, values, &mut patches)?;
    patch_tags(xml, entry, &analysis.tags, &mut patches)?;
    if patches.is_empty() {
        return Ok(None);
    }
    let updated = apply_patches(xml, patches)?;
    parse_document(&updated)?;
    Ok(Some(updated))
}

fn patch_tags(
    xml: &str,
    entry: Node<'_, '_>,
    tags: &[String],
    patches: &mut Vec<(Range<usize>, String)>,
) -> io::Result<()> {
    let mut wrappers = entry.children().filter(|node| node.has_tag_name(TAGS));
    let Some(wrapper) = wrappers.next() else {
        if !tags.is_empty() {
            let mut markup = String::from("<aiTags>");
            append_tags(&mut markup, tags);
            markup.push_str("</aiTags>");
            append_children(xml, entry, markup, patches)?;
        }
        return Ok(());
    };
    if wrappers.next().is_some() {
        return Err(invalid("Duplicate screenshot analysis tag wrapper"));
    }
    let mut next = tags.iter();
    for node in wrapper
        .children()
        .filter(|node| node.has_tag_name("tag") && node.tag_name().namespace().is_none())
    {
        let value = scalar_text(node)?;
        match next.next() {
            Some(tag) if value == tag.as_str() => {}
            Some(tag) => {
                let mut markup = String::new();
                element(&mut markup, "tag", tag);
                patches.push((node.range(), markup));
            }
            None => patches.push((node.range(), String::new())),
        }
    }
    let mut additions = String::new();
    append_tags(&mut additions, next.as_slice());
    append_children(xml, wrapper, additions, patches)
}

fn append_tags(xml: &mut String, tags: &[String]) {
    for tag in tags {
        element(xml, "tag", tag);
    }
}

fn element(xml: &mut String, name: &str, value: &str) {
    let _ = write!(xml, "<{name}>{}</{name}>", escape(value));
}

#[cfg(test)]
mod tests;
