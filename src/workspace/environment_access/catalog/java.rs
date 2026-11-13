//! Java credential environments and their optional tab colors.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::io;

use roxmltree::{Document, ParsingOptions};

use super::{
    Catalog, Definition, MAX_BYTES, MAX_CUSTOM, Rgb, checked_name, invalid, valid_custom_id,
};

pub(crate) const PATH: &str = "environments.json";
pub(crate) const MAX_BACKUP_BYTES: usize = MAX_BYTES as usize;
const MAX_XML_NODES: u32 = 2048;

#[derive(Default)]
pub(crate) struct Report {
    pub(crate) definitions: usize,
    pub(crate) colors: usize,
    pub(crate) omitted: usize,
}

pub(crate) fn portable(bytes: &[u8]) -> io::Result<(Vec<u8>, Report)> {
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err(invalid("Java environments.xml exceeds its size limit"));
    }
    let xml = std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
        .map_err(|_| invalid("Java environments.xml must use UTF-8"))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Invalid Java environments.xml"))?;
    let root = document.root_element();
    if !root.has_tag_name("environments") || root.tag_name().namespace().is_some() {
        return Err(invalid("Unexpected Java environments.xml root"));
    }
    let mut definitions = Vec::new();
    let mut colors = BTreeMap::new();
    let mut report = Report::default();
    let mut seen_definitions = BTreeSet::new();
    let mut seen_colors = BTreeSet::new();
    let mut duplicate_definitions = BTreeSet::new();
    let mut duplicate_colors = BTreeSet::new();
    let mut colors_wrapper = false;
    for node in root.children().filter(roxmltree::Node::is_element) {
        match (node.tag_name().namespace(), node.tag_name().name()) {
            (None, "environment") => {
                let Some(id) = node.attribute("id").filter(|id| valid_custom_id(id, 8)) else {
                    report.omitted += 1;
                    continue;
                };
                if !seen_definitions.insert(id) {
                    duplicate_definitions.insert(id);
                    report.omitted += 1;
                    continue;
                }
                let Some(label) = node
                    .attribute("displayName")
                    .and_then(|label| checked_name(label).ok())
                else {
                    report.omitted += 1;
                    continue;
                };
                definitions.push((id, label));
            }
            (None, "colors") if !colors_wrapper => {
                colors_wrapper = true;
                for color in node.children().filter(roxmltree::Node::is_element) {
                    let Some(id) = color.attribute("id").filter(|_| {
                        color.has_tag_name("color") && color.tag_name().namespace().is_none()
                    }) else {
                        report.omitted += 1;
                        continue;
                    };
                    if !seen_colors.insert(id) {
                        duplicate_colors.insert(id);
                        report.omitted += 1;
                        continue;
                    }
                    if let Some(rgb) = color.attribute("color").and_then(Rgb::parse) {
                        colors.insert(id, rgb);
                    } else {
                        report.omitted += 1;
                    }
                }
            }
            (None, "colors") => return Err(invalid("Duplicate Java environment color list")),
            _ => report.omitted += 1,
        }
    }
    let mut catalog = Catalog::default();
    for (id, label) in definitions {
        if duplicate_definitions.contains(id) || catalog.custom.len() == MAX_CUSTOM {
            report.omitted += 1;
            continue;
        }
        catalog.custom.push(Definition {
            id: id.into(),
            label: label.into(),
        });
    }
    for (id, color) in colors {
        if duplicate_colors.contains(id) || catalog.get(id).is_none_or(|entry| entry.id != id) {
            report.omitted += 1;
        } else {
            catalog.colors.insert(id.into(), color);
        }
    }
    report.definitions = catalog.custom.len();
    report.colors = catalog.colors.len();
    let encoded = serde_json::to_vec(&catalog)?;
    Catalog::validate_backup(&encoded)?;
    Ok((encoded, report))
}

pub(crate) fn merge(local: Option<&[u8]>, incoming: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let mut local = local.map(Catalog::decode).transpose()?.unwrap_or_default();
    let mut incoming = Catalog::decode(incoming)?;
    if incoming
        .custom
        .iter()
        .any(|item| !valid_custom_id(&item.id, 8))
    {
        return Err(invalid("Invalid Java environment identity"));
    }
    let mut changed = false;
    for item in incoming.custom {
        if let Some(existing) = local.get(&item.id) {
            if existing.label != item.label {
                incoming.colors.remove(&item.id);
            }
        } else if local.custom.len() < MAX_CUSTOM {
            local.custom.push(item);
            changed = true;
        }
    }
    for (id, color) in incoming.colors {
        if local.get(&id).is_some()
            && let Entry::Vacant(slot) = local.colors.entry(id)
        {
            slot.insert(color);
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    let encoded = serde_json::to_vec(&local)?;
    Catalog::validate_backup(&encoded)?;
    Ok(Some(encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_exact_java_ids_labels_and_both_color_scopes() {
        let (portable, report) = portable(
            r##"
            <environments>
              <environment id="custom-a1b2c3d4" displayName="Café &amp; Ops"/>
              <colors>
                <color id="custom-a1b2c3d4" color="#abc"/>
                <color id="PRODUCTION" color="#a123f0"/>
                <color id="custom-deadbeef" color="#000"/>
              </colors>
            </environments>"##
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            (report.definitions, report.colors, report.omitted),
            (1, 2, 1)
        );
        let catalog = Catalog::decode(&portable).unwrap();
        assert_eq!(catalog.label("custom-a1b2c3d4"), Some("Café & Ops"));
        assert_eq!(
            catalog.colors["custom-a1b2c3d4"],
            Rgb::parse("#AABBCC").unwrap()
        );
        assert_eq!(catalog.colors["PRODUCTION"], Rgb::parse("#A123F0").unwrap());
    }

    #[test]
    fn preserves_local_colors_and_labels_and_replays_without_changes() {
        let (incoming, _) = portable(
            br##"
            <environments>
              <environment id="custom-a1b2c3d4" displayName="Java label"/>
              <environment id="custom-deadbeef" displayName="New environment"/>
              <colors>
                <color id="custom-a1b2c3d4" color="#abc"/>
                <color id="PRODUCTION" color="#a123f0"/>
                <color id="TEST" color="#abc"/>
              </colors>
            </environments>"##,
        )
        .unwrap();
        let (local, _) = portable(
            br##"
            <environments>
              <environment id="custom-a1b2c3d4" displayName="Local label"/>
              <colors><color id="PRODUCTION" color="#fff"/></colors>
            </environments>"##,
        )
        .unwrap();
        let merged = merge(Some(&local), &incoming).unwrap().unwrap();
        let catalog = Catalog::decode(&merged).unwrap();
        assert_eq!(catalog.label("custom-a1b2c3d4"), Some("Local label"));
        assert_eq!(catalog.label("custom-deadbeef"), Some("New environment"));
        assert!(!catalog.colors.contains_key("custom-a1b2c3d4"));
        assert_eq!(catalog.colors["PRODUCTION"], Rgb::parse("#fff").unwrap());
        assert_eq!(catalog.colors["TEST"], Rgb::parse("#abc").unwrap());
        assert!(merge(Some(&merged), &incoming).unwrap().is_none());
    }
}
