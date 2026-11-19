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
    ("workflowScriptDialogGeometry", Kind::AgentWorkflow),
];

const NAMED: &[(&str, Kind)] = &[
    ("de.kortty.ui.SettingsDialog", Kind::GlobalSettings),
    ("de.kortty.ui.QuickConnectDialog", Kind::QuickConnect),
    (
        "de.kortty.ui.ConnectionManagerDialog",
        Kind::ConnectionManager,
    ),
    ("de.kortty.ui.ConnectionEditDialog", Kind::ConnectionEditor),
    (
        "de.kortty.ui.ConnectionImportDialog",
        Kind::ConnectionImport,
    ),
    (
        "de.kortty.ui.ConnectionExportDialog",
        Kind::ConnectionExport,
    ),
    (
        "de.kortty.ui.CredentialManagementDialog",
        Kind::CredentialVault,
    ),
    (
        "de.kortty.ui.TeamworkSettingsDialog",
        Kind::TeamworkSettings,
    ),
    ("de.kortty.ui.RestoreTeamworkDialog", Kind::TeamworkRestore),
    ("de.kortty.ui.OsBadgeManagerDialog", Kind::OsBadgeManager),
    ("de.kortty.ui.KnownHostsDialog", Kind::KnownHosts),
    ("de.kortty.ui.SSHKeyManagementDialog", Kind::SshKeyManager),
    ("de.kortty.ui.LogfileManagerDialog", Kind::LogfileManager),
    ("de.kortty.ui.LogfileViewerDialog", Kind::LogfileViewer),
    (
        "de.kortty.ui.TerminalRecordingManagerDialog",
        Kind::VideoManager,
    ),
    (
        "de.kortty.ui.TerminalRecordingReplayDialog",
        Kind::VideoReplay,
    ),
    (
        "de.kortty.ui.SessionProcessesDialog",
        Kind::SessionProcesses,
    ),
    (
        "de.kortty.ui.ShellIntegrationSetupDialog",
        Kind::ShellIntegration,
    ),
    (
        "de.kortty.ui.EnvironmentManagementDialog",
        Kind::Environments,
    ),
    (
        "de.kortty.ui.TerminalEffectPluginManagerDialog",
        Kind::TerminalEffects,
    ),
    ("de.kortty.ui.SnippetDiffDialog", Kind::SnippetComparison),
    ("highlight.rules", Kind::HighlightRules),
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
        assert_eq!((incoming.count, incoming.omitted), (EXPLICIT.len() + 2, 2));
        let incoming = incoming.contents.unwrap();
        let native = br#"{"java_dialog_geometry":{"ai-manager":{"width":950,"height":700,"position":null}}}"#;
        let restored = merge_java_window_settings(Some(native), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let placements = WindowSettings::decode(&restored)
            .unwrap()
            .java_dialog_geometry;
        assert_eq!(placements.len(), EXPLICIT.len() + 2);
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

    #[test]
    fn java_class_keys_restore_independent_dialogs_and_reject_aliases() {
        let named = NAMED.iter().enumerate().map(|(index, (name, _))| {
            let width = 900 + index;
            format!(
                "<window key=\"{name}\"><geometry><width>{width}</width><height>700</height></geometry></window>"
            )
        }).collect::<String>();
        let xml = format!(
            "<globalSettings><windowGeometries>{named}
             <window key=\"QuickConnectDialog\"><geometry><width>1000</width><height>700</height></geometry></window>
             <window key=\"de.kortty.ui.SshKeyManagementDialog\"><geometry><width>1000</width><height>700</height></geometry></window>
             </windowGeometries></globalSettings>"
        );
        let incoming = portable_java_preferences(xml.as_bytes(), None).unwrap();
        assert_eq!((incoming.count, incoming.omitted), (NAMED.len(), 2));
        let payload = incoming.contents.unwrap();
        let native = br#"{"java_dialog_geometry":{"connection-manager":{"width":1220,"height":770,"position":null}}}"#;
        let merged = merge_java_window_settings(Some(native), &payload, &Default::default())
            .unwrap()
            .unwrap();
        let settings = WindowSettings::decode(&merged).unwrap();
        assert_eq!(settings.java_dialog_geometry.len(), NAMED.len());
        for (index, (_, kind)) in NAMED.iter().enumerate() {
            let bounds = settings.java_dialog_geometry[kind.key()].placement(iced::Size::ZERO);
            let expected = if *kind == Kind::ConnectionManager {
                1220
            } else {
                900 + index
            };
            assert_eq!(bounds.width, expected as f32, "{}", kind.key());
        }
        assert!(
            merge_java_window_settings(Some(&merged), &payload, &Default::default())
                .unwrap()
                .is_none()
        );
    }
}
