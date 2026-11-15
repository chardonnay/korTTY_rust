use std::collections::{BTreeMap, BTreeSet};
use std::io;

use roxmltree::{Document, ParsingOptions};

use super::{MAX_PROFILES, invalid, kortty, library, valid_teamwork_id, xml};
use crate::connection_store::TabColor;

const MAX_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
const MAX_SETTINGS_NODES: u32 = 65_536;

/// Presentation settings from Java's global settings, tied to the same source catalogue.
#[derive(Clone, Default)]
pub(crate) struct JavaLibrarySettings {
    pub(super) favorites: BTreeSet<String>,
    pub(super) group_colors: BTreeMap<String, TabColor>,
}

impl JavaLibrarySettings {
    pub(crate) fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(invalid(
                "Java connection library settings exceed their size limit",
            ));
        }
        let text = std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .map_err(invalid)?;
        let document = Document::parse_with_options(
            text,
            ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_SETTINGS_NODES,
                entity_resolver: None,
            },
        )
        .map_err(invalid)?;
        let root = document.root_element();
        if !xml::has_unqualified_tag(root, "globalSettings") {
            return Err(invalid("Invalid Java connection library settings"));
        }

        let mut result = Self::default();
        if let Ok(Some(entries)) = xml::optional_child(root, "favoriteConnections") {
            for entry in entries
                .children()
                .filter(|node| xml::has_unqualified_tag(*node, "connection"))
            {
                let mut content = entry.children();
                if let Some(id) = content
                    .next()
                    .filter(|text| text.is_text())
                    .and_then(|text| text.text())
                    && content.next().is_none()
                    && entry.attributes().next().is_none()
                    && valid_teamwork_id(id)
                {
                    if result.favorites.len() == MAX_PROFILES && !result.favorites.contains(id) {
                        break;
                    }
                    result.favorites.insert(id.to_owned());
                }
            }
        }
        if let Ok(Some(entries)) = xml::optional_child(root, "connectionGroupColors") {
            let mut colors = BTreeMap::<String, Option<TabColor>>::new();
            for entry in entries
                .children()
                .filter(|node| xml::has_unqualified_tag(*node, "group"))
            {
                let Some(path) = entry
                    .attribute("path")
                    .map(kortty::normalize_java_group_path)
                else {
                    continue;
                };
                if !library::valid_path(&path) {
                    continue;
                }
                if colors.len() == MAX_PROFILES && !colors.contains_key(&path) {
                    continue;
                }
                let color = if entry.attributes().len() == 2
                    && !entry.children().any(|child| child.is_element())
                {
                    entry.attribute("color").and_then(TabColor::parse)
                } else {
                    None
                };
                colors
                    .entry(path)
                    .and_modify(|stored| {
                        if *stored != color {
                            *stored = None;
                        }
                    })
                    .or_insert(color);
            }
            result.group_colors.extend(
                colors
                    .into_iter()
                    .filter_map(|(path, color)| Some((path, color?))),
            );
        }
        Ok(result)
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.favorites.len(), self.group_colors.len())
    }
}
