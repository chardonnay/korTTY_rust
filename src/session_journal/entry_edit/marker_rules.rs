use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io;
use std::ops::Range;

use roxmltree::{Document, Node};

use crate::private_files::Directory;
use crate::session_journal::markers::{Compiled, Definition, match_text};
use crate::session_journal::scalar_text;

use super::{
    LegacyMarker, MarkerOutcome, MarkerSource, Source, append_children, apply_patches,
    bounded_field, child, document_id, entries, escape, field, invalid, lock_source, marker_values,
    normalize_marker, parse_document, patch_scalars, read_snapshot, require_java_fields,
    stale_marker,
};

/// Applies the reviewed rule registry to a stopped journal, under the shared writer lock.
pub(crate) fn apply_rules(
    source: &Source,
    rules: &Compiled,
    overwrite_manual: bool,
) -> io::Result<MarkerOutcome> {
    apply_rules_with_sync(source, rules, overwrite_manual, Directory::sync)
}

pub(super) fn apply_rules_with_sync(
    source: &Source,
    rules: &Compiled,
    overwrite_manual: bool,
    sync: impl FnOnce(&Directory) -> io::Result<()>,
) -> io::Result<MarkerOutcome> {
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The selected session journal identity changed"));
    }
    let (changed, updated) = patch_rules(&xml, &document, rules, overwrite_manual)?;
    let Some(updated) = updated else {
        return Ok(MarkerOutcome::default());
    };
    lock.publish_document(&stamp, Some(&updated), |opened| {
        source.require_location(source.directory(), opened)
    })?;
    Ok(MarkerOutcome {
        changed,
        durability_warning: sync(lock.directory()).is_err(),
    })
}

fn patch_rules(
    xml: &str,
    document: &Document<'_>,
    rules: &Compiled,
    overwrite_manual: bool,
) -> io::Result<(usize, Option<String>)> {
    let root = document.root_element();
    require_java_fields(root, &["entries", "meta"])?;
    let mut patches = Vec::new();
    let mut definitions = Vec::new();
    let mut snapshotted = HashSet::new();
    let mut count = 0;
    let mut errors = 0;
    for entry in entries(document).filter(|entry| entry.tag_name().namespace().is_none()) {
        require_java_fields(entry, &["marker", "markerId", "markerSource"])?;
        let legacy = field(entry, "marker")?;
        let id = field(entry, "markerId")?;
        let origin = field(entry, "markerSource")?;
        let manual = origin.as_deref() == Some(MarkerSource::User.xml());
        let applied = if manual && !overwrite_manual {
            None
        } else {
            let parts = match_parts(entry)?;
            let text = match_text(parts.iter().map(Cow::as_ref));
            rules.first_match(&text).filter(|definition| {
                let applied_id = (!definition.is_builtin()).then_some(definition.id.as_str());
                id.as_deref() != applied_id
                    || LegacyMarker::parse(legacy.as_deref()).unwrap_or(LegacyMarker::None)
                        != definition.legacy
                    || origin.as_deref() != Some(MarkerSource::Rule.xml())
            })
        };
        if let Some(definition) = applied {
            patch_scalars(
                xml,
                entry,
                marker_values(definition, MarkerSource::Rule),
                &mut patches,
            )?;
            if !definition.is_builtin() && snapshotted.insert(definition.id.as_str()) {
                definitions.push(definition);
            }
            errors += usize::from(definition.legacy == LegacyMarker::Error);
            count += 1;
        } else {
            errors += usize::from(legacy.as_deref() == Some("ERROR"));
        }
    }
    if count == 0 {
        return Ok((0, None));
    }
    let meta = child(root, "meta").ok_or_else(|| invalid("Missing session journal metadata"))?;
    let errors = errors.to_string();
    patch_scalars(
        xml,
        meta,
        [("errorCount", Some(errors.as_str()))],
        &mut patches,
    )?;
    patch_snapshots(xml, document, &definitions, &mut patches)?;
    let updated = apply_patches(xml, patches)?;
    parse_document(&updated)?;
    Ok((count, Some(updated)))
}

fn match_parts<'a>(entry: Node<'a, '_>) -> io::Result<Vec<Cow<'a, str>>> {
    require_java_fields(
        entry,
        &["title", "text", "userNote", "inputExcerpt", "outputExcerpt"],
    )?;
    let mut parts = vec![
        bounded_field(entry, "title")?,
        bounded_field(entry, "text")?,
        bounded_field(entry, "userNote")?,
    ];
    for name in ["inputExcerpt", "outputExcerpt"] {
        let mut wrappers = entry.children().filter(|node| node.has_tag_name(name));
        let Some(wrapper) = wrappers.next() else {
            continue;
        };
        if wrappers.next().is_some() {
            return Err(invalid("Duplicate session journal excerpt"));
        }
        require_java_fields(wrapper, &["line"])?;
        for line in wrapper.children().filter(|node| node.has_tag_name("line")) {
            parts.push(scalar_text(line)?);
        }
    }
    Ok(parts)
}

/// Patches only the used definitions and preserves portable snapshots for unrelated markers.
pub(super) fn patch_snapshots(
    xml: &str,
    document: &Document<'_>,
    definitions: &[&Definition],
    patches: &mut Vec<(Range<usize>, String)>,
) -> io::Result<()> {
    if definitions.is_empty() {
        return Ok(());
    }
    let root = document.root_element();
    require_java_fields(root, &["markerDefinitions"])?;
    let wrapper = child(root, "markerDefinitions");
    let requested: HashSet<_> = definitions
        .iter()
        .map(|definition| definition.id.as_str())
        .collect();
    let mut current = HashMap::new();
    for node in wrapper
        .into_iter()
        .flat_map(|wrapper| wrapper.children())
        .filter(|node| node.has_tag_name("marker") && node.tag_name().namespace().is_none())
    {
        require_java_fields(node, &["id"])?;
        if let Some(id) = normalize_marker(&bounded_field(node, "id")?)
            && requested.contains(id.as_str())
            && current.insert(id, node).is_some()
        {
            return Err(stale_marker());
        }
    }

    let mut additions = String::new();
    for definition in definitions {
        let color = definition
            .color
            .map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}"));
        if let Some(node) = current.get(&definition.id).copied() {
            require_java_fields(node, &["name", "color", "legacyMarker"])?;
            if read_snapshot(node, definition.id.clone())? != **definition {
                patch_scalars(
                    xml,
                    node,
                    [
                        ("name", Some(definition.name.as_str())),
                        ("color", color.as_deref()),
                        ("legacyMarker", Some(definition.legacy.xml())),
                    ],
                    patches,
                )?;
            }
        } else {
            additions.push_str("<marker>");
            let mut element = |name: &str, value: &str| {
                additions.push('<');
                additions.push_str(name);
                additions.push('>');
                additions.push_str(&escape(value));
                additions.push_str("</");
                additions.push_str(name);
                additions.push('>');
            };
            element("id", &definition.id);
            element("name", &definition.name);
            if let Some(color) = color.as_deref() {
                element("color", color);
            }
            element("builtIn", "false");
            element("legacyMarker", definition.legacy.xml());
            additions.push_str("</marker>");
        }
    }
    if !additions.is_empty() {
        let parent = match wrapper {
            Some(wrapper) => wrapper,
            None => {
                additions.insert_str(0, "<markerDefinitions>");
                additions.push_str("</markerDefinitions>");
                root
            }
        };
        append_children(xml, parent, additions, patches)?;
    }
    Ok(())
}
