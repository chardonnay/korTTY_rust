//! Migration of named and explicit Java window bounds.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use roxmltree::Node;

use crate::profile::SavedPosition;
use crate::workspace::dialog_placement::Kind;
use crate::workspace::journal_access::dialog_geometry::Geometry;

use super::{preference_node, preference_text};

pub(in crate::workspace) type Placements = BTreeMap<String, Geometry>;

const EXPLICIT: &[(&str, Kind)] = &[
    ("aiManagerDialogGeometry", Kind::AiManager),
    ("savedChatsDialogGeometry", Kind::SavedChats),
    ("asciiArtDialogGeometry", Kind::AsciiArt),
    ("snippetManagerGeometry", Kind::SnippetManager),
    ("jobSchedulerDialogGeometry", Kind::JobScheduler),
    ("guideViewerGeometry", Kind::Manual),
];

const NAMED: &[(&str, Kind)] = &[
    ("sessionJournal.markers", Kind::JournalMarkerManager),
    ("sessionJournal.exportOptions", Kind::JournalExport),
    ("terminal.journalNote", Kind::JournalNote),
];

fn supported_key(key: &str) -> bool {
    EXPLICIT
        .iter()
        .chain(NAMED)
        .any(|(_, kind)| kind.key() == key)
}

pub(in crate::workspace) fn validate(placements: &Placements) -> io::Result<()> {
    if !placements
        .iter()
        .all(|(key, geometry)| supported_key(key) && geometry.valid_imported_bounds())
    {
        return Err(io::Error::other("Invalid Java dialog geometry"));
    }
    Ok(())
}

pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Placements {
    let mut entries = Placements::new();
    for &(name, kind) in EXPLICIT {
        if let Some(geometry) = read_geometry(root, name, omitted) {
            entries.insert(kind.key().into(), geometry);
        }
    }
    let list = match preference_node(root, "windowGeometries") {
        Ok(Some(list)) => list,
        Ok(None) => return entries,
        Err(()) => {
            *omitted += 1;
            return entries;
        }
    };
    let mut seen = BTreeSet::new();
    for window in list.children().filter(|node| node.is_element()) {
        let kind = window.attribute("key").and_then(|key| {
            NAMED
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, kind)| *kind)
        });
        let Some(kind) = kind.filter(|_| {
            window.has_tag_name("window")
                && window.tag_name().namespace().is_none()
                && window.attributes().len() == 1
        }) else {
            *omitted += 1;
            continue;
        };
        if !seen.insert(kind.key()) {
            entries.remove(kind.key());
            *omitted += 1;
            continue;
        }
        match preference_node(window, "geometry")
            .and_then(|node| node.ok_or(()))
            .and_then(parse_geometry)
        {
            Ok(geometry) => {
                entries.insert(kind.key().into(), geometry);
            }
            Err(()) => *omitted += 1,
        }
    }
    entries
}

pub(super) fn read_geometry(
    root: Node<'_, '_>,
    name: &str,
    omitted: &mut usize,
) -> Option<Geometry> {
    match preference_node(root, name).and_then(|node| node.map(parse_geometry).transpose()) {
        Ok(geometry) => geometry,
        Err(()) => {
            *omitted += 1;
            None
        }
    }
}

fn parse_geometry(node: Node<'_, '_>) -> Result<Geometry, ()> {
    let number = |name| -> Result<Option<f64>, ()> {
        preference_text(node, name)?
            .map(|value| value.parse().map_err(|_| ()))
            .transpose()
    };
    let limit = f64::from(SavedPosition::MAX_COORDINATE);
    let position = match (number("x")?, number("y")?) {
        (None, None) => None,
        (Some(x), Some(y)) if (-limit..=limit).contains(&x) && (-limit..=limit).contains(&y) => {
            Some([x.round() as i32, y.round() as i32])
        }
        _ => return Err(()),
    };
    Geometry::from_java(
        number("width")?.ok_or(())?,
        number("height")?.ok_or(())?,
        position,
    )
    .ok_or(())
}

#[cfg(test)]
mod tests {
    use super::super::{WindowSettings, merge_java_window_settings, portable_java_preferences};
    use super::*;

    #[test]
    fn java_dialog_geometry_resolves_exact_keys_and_preserves_individual_native_values() {
        let geometry =
            "<geometry><width>1110</width><height>735</height><x>-280</x><y>40</y></geometry>";
        let explicit = EXPLICIT
            .iter()
            .map(|(field, _)| format!("<{field}><width>1000</width><height>700</height></{field}>"))
            .collect::<String>();
        let xml = format!(
            "<globalSettings>{explicit}<windowGeometries>
            <window key=\"sessionJournal.markers\">{geometry}</window>
            <window key=\"sessionJournal.exportOptions\">{geometry}</window>
            <window key=\"terminal.journalNote\">{geometry}</window>
            <window key=\"terminal.journalNote\">{geometry}</window>
            <window key=\"sessionJournal.ExportOptions\">{geometry}</window>
            </windowGeometries></globalSettings>"
        );
        let incoming = portable_java_preferences(xml.as_bytes(), None).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (8, 2));
        let incoming = incoming.contents.unwrap();
        let native = br#"{"java_dialog_geometry":{"ai-manager":{"width":950,"height":700,"position":null}}}"#;
        let restored = merge_java_window_settings(Some(native), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let placements = WindowSettings::decode(&restored)
            .unwrap()
            .java_dialog_geometry;
        assert_eq!(placements.len(), 8);
        for &(_, kind) in EXPLICIT {
            let placement = placements[kind.key()].placement(iced::Size::new(0.0, 0.0));
            assert_eq!(placement.height, 700.0);
            assert_eq!(
                placement.width,
                if kind == Kind::AiManager {
                    950.0
                } else {
                    1000.0
                }
            );
        }
        assert_eq!(
            placements[Kind::JournalExport.key()]
                .placement(iced::Size::new(0.0, 0.0))
                .position,
            SavedPosition::new(-280.0, 40.0),
        );
        assert!(placements.contains_key(Kind::JournalMarkerManager.key()));
        assert!(!placements.contains_key(Kind::JournalNote.key()));
        assert!(
            merge_java_window_settings(Some(&restored), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
    }
}
