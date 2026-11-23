use std::collections::{BTreeMap, BTreeSet};
use std::io;

use roxmltree::{Document, ParsingOptions};

use super::{MAX_PROFILES, invalid, kortty, library, valid_teamwork_id, xml};
use crate::connection_store::custom_os_badges::JavaBadgeHint;
use crate::connection_store::{JavaGroupColor, JavaGroupIsolation, TabColor};
use crate::session_security::IsolationLevel;

const MAX_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
const MAX_SETTINGS_NODES: u32 = 65_536;

/// Connection library settings tied to the source catalogue in a Java backup.
#[derive(Clone, Default)]
pub(crate) struct JavaLibrarySettings {
    pub(super) favorites: BTreeSet<String>,
    pub(super) group_colors: BTreeMap<String, TabColor>,
    pub(super) group_color_hints: BTreeSet<JavaGroupColor>,
    pub(super) group_isolation: BTreeMap<String, IsolationLevel>,
    pub(super) group_isolation_hints: BTreeSet<JavaGroupIsolation>,
    pub(super) highlight_sets: Option<BTreeMap<String, String>>,
    pub(super) os_badges: BTreeMap<String, JavaBadgeHint>,
}

impl JavaLibrarySettings {
    pub(crate) fn with_highlight_sets(mut self, ids: BTreeMap<String, String>) -> Self {
        self.highlight_sets = Some(ids);
        self
    }

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
                if let Some(color) = color
                    && result.group_color_hints.len() < MAX_PROFILES
                {
                    result.group_color_hints.insert(JavaGroupColor {
                        source: path.clone(),
                        color,
                    });
                }
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
        if let Ok(Some(entries)) = xml::optional_child(root, "connectionGroupIsolation") {
            let mut levels = BTreeMap::<String, Option<IsolationLevel>>::new();
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
                if !library::valid_path(&path)
                    || levels.len() == MAX_PROFILES && !levels.contains_key(&path)
                {
                    continue;
                }
                let level = if entry.attributes().len() == 2
                    && !entry.children().any(|child| child.is_element())
                {
                    entry.attribute("level").and_then(|value| {
                        IsolationLevel::from_id(&value.trim().to_ascii_lowercase())
                    })
                } else {
                    None
                };
                if let Some(level) = level
                    && result.group_isolation_hints.len() < MAX_PROFILES
                {
                    result.group_isolation_hints.insert(JavaGroupIsolation {
                        source: path.clone(),
                        level,
                    });
                }
                levels
                    .entry(path)
                    .and_modify(|saved| {
                        if *saved != level {
                            *saved = None;
                        }
                    })
                    .or_insert(level);
            }
            result.group_isolation.extend(
                levels
                    .into_iter()
                    .filter_map(|(path, level)| Some((path, level?))),
            );
        }
        if let Ok(Some(entries)) = xml::optional_child(root, "customOsBadges") {
            let mut unique = BTreeMap::<String, Option<JavaBadgeHint>>::new();
            for entry in entries
                .children()
                .filter(|node| xml::has_unqualified_tag(*node, "badge"))
            {
                let Some(id) = entry.attribute("id") else {
                    continue;
                };
                let reference = format!("custom:{id}");
                let hint = JavaBadgeHint {
                    reference: reference.clone(),
                    name: entry.attribute("name").map(str::to_owned),
                    file: entry.attribute("file").map(str::to_owned),
                };
                if !hint.valid() || unique.len() == MAX_PROFILES && !unique.contains_key(&reference)
                {
                    continue;
                }
                unique
                    .entry(reference)
                    .and_modify(|stored| {
                        if stored.as_ref() != Some(&hint) {
                            *stored = None;
                        }
                    })
                    .or_insert(Some(hint));
            }
            result.os_badges.extend(
                unique
                    .into_iter()
                    .filter_map(|(reference, hint)| Some((reference, hint?))),
            );
        }
        Ok(result)
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.favorites.len(), self.group_color_hints.len())
    }

    pub(crate) fn group_isolation_count(&self) -> usize {
        self.group_isolation_hints.len()
    }

    pub(crate) fn os_badges(&self) -> impl ExactSizeIterator<Item = &JavaBadgeHint> {
        self.os_badges.values()
    }
}
