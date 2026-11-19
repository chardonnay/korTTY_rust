use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::path::Path;

use iced::{Task, window};
use serde::{Deserialize, Serialize};

use crate::connection_store::{Store as Connections, use_timestamp};
use crate::profile::{self, projects};

use super::{Message as Desktop, Workspace};

const CONNECTION_LIMIT: usize = 10;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Preferences {
    cleared_at: u64,
    projects: Vec<RememberedProject>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum RememberedProject {
    Named(String),
    File(projects::ProjectFile),
    Imported {
        imported_file: projects::ProjectFile,
    },
}

impl RememberedProject {
    fn same_location(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Named(left), Self::Named(right)) => left == right,
            _ => self
                .file()
                .zip(other.file())
                .is_some_and(|(left, right)| left.path() == right.path()),
        }
    }

    fn file(&self) -> Option<&projects::ProjectFile> {
        match self {
            Self::Named(_) => None,
            Self::File(file)
            | Self::Imported {
                imported_file: file,
            } => Some(file),
        }
    }
}

impl Preferences {
    pub(super) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(super) fn normalize(&mut self) {
        let mut names = BTreeSet::new();
        let mut files = BTreeSet::new();
        let mut clean = Vec::new();
        for entry in self.projects.drain(..) {
            let valid = match &entry {
                RememberedProject::Named(entry) => projects::Name::parse(entry)
                    .is_ok_and(|name| name.as_str() == entry && names.insert(name)),
                RememberedProject::File(file)
                | RememberedProject::Imported {
                    imported_file: file,
                } => file.valid() && files.insert(file.path().to_owned()),
            };
            if valid {
                clean.push(entry);
            }
            if clean.len() == projects::RECENT_LIMIT {
                break;
            }
        }
        self.projects = clean;
    }

    fn remember(&mut self, project: RememberedProject, original: Option<&projects::ProjectFile>) {
        self.projects.retain(|entry| {
            !entry.same_location(&project)
                && !entry
                    .file()
                    .zip(original)
                    .is_some_and(|(entry, original)| entry.path() == original.path())
        });
        self.projects.insert(0, project);
        self.projects.truncate(projects::RECENT_LIMIT);
    }

    pub(super) fn project_count(&self) -> usize {
        self.projects.len()
    }

    pub(super) fn from_java(
        cleared_at: Option<u64>,
        paths: Option<roxmltree::Node<'_, '_>>,
        omitted: &mut usize,
    ) -> Option<Self> {
        let mut history = Self {
            cleared_at: cleared_at.unwrap_or_default(),
            ..Self::default()
        };
        if let Some(paths) = paths {
            for node in paths.children().filter(roxmltree::Node::is_element) {
                let project = (history.projects.len() < projects::RECENT_LIMIT
                    && node.has_tag_name("path")
                    && node.tag_name().namespace().is_none()
                    && node.attributes().next().is_none()
                    && node.children().all(|child| child.is_text()))
                .then(|| node.text())
                .flatten()
                .and_then(|path| projects::ProjectFile::new(path.into(), None).ok())
                .map(|file| RememberedProject::Imported {
                    imported_file: file,
                });
                match project {
                    Some(project)
                        if !history
                            .projects
                            .iter()
                            .any(|entry| entry.same_location(&project)) =>
                    {
                        history.projects.push(project);
                    }
                    _ => *omitted += 1,
                }
            }
        }
        (!history.is_default()).then_some(history)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Entry {
    Connection {
        id: u64,
        label: String,
        used_at: u64,
    },
    Project {
        name: projects::Name,
        label: String,
        cleared_at: u64,
    },
    ProjectFile {
        file: projects::ProjectFile,
        label: String,
        cleared_at: u64,
    },
}

impl Entry {
    pub(super) fn label(&self) -> &str {
        match self {
            Self::Connection { label, .. }
            | Self::Project { label, .. }
            | Self::ProjectFile { label, .. } => label,
        }
    }

    pub(super) fn is_connection(&self) -> bool {
        matches!(self, Self::Connection { .. })
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Loaded(u64, Vec<Entry>),
}

#[derive(Default)]
pub(super) struct History {
    entries: Vec<Entry>,
    reading: Option<u64>,
    refresh_again: bool,
}

impl Workspace {
    pub(super) fn open_recent_entries(&self) -> Vec<Entry> {
        self.open_recent.entries.clone()
    }

    pub(super) fn open_recent_cutoff(&self) -> u64 {
        self.window_settings.open_recent.cleared_at
    }

    pub(super) fn set_open_recent_connections(&mut self, store: &Connections) {
        let mut connections =
            connection_entries(store, self.window_settings.open_recent.cleared_at);
        connections.extend(
            self.open_recent
                .entries
                .drain(..)
                .filter(|entry| !entry.is_connection()),
        );
        self.open_recent.entries = connections;
        self.open_recent.refresh_again |= self.open_recent.reading.is_some();
    }

    #[cfg(test)]
    pub(super) fn seed_open_recent_entries(&mut self, entries: Vec<Entry>) {
        self.open_recent.entries = entries;
    }

    pub(super) fn refresh_open_recent(&mut self) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        if self.open_recent.reading.is_some() {
            self.open_recent.refresh_again = true;
            return Task::none();
        }
        let ticket = self.allocate();
        self.open_recent.reading = Some(ticket);
        let settings = self.window_settings.open_recent.clone();
        Task::perform(async move { scan(&settings) }, move |entries| {
            Desktop::OpenRecent(Message::Loaded(ticket, entries))
        })
    }

    pub(super) fn update_open_recent(
        &mut self,
        Message::Loaded(ticket, entries): Message,
    ) -> Task<Desktop> {
        if self.open_recent.reading != Some(ticket) {
            return Task::none();
        }
        self.open_recent.reading = None;
        if std::mem::take(&mut self.open_recent.refresh_again) {
            return self.refresh_open_recent();
        }
        self.open_recent.entries = entries;
        Task::none()
    }

    pub(super) fn open_recent_item(&mut self, owner: window::Id, entry: Entry) -> Task<Desktop> {
        if self.exiting
            || self.scheduler_draining
            || !self.palette_can_open(owner)
            || self.clipboard.policy.problem.is_some()
            || !self.open_recent.entries.contains(&entry)
        {
            return Task::none();
        }
        match entry {
            Entry::Connection { id, .. } => self.open_recent_connection(owner, id),
            Entry::Project { name, .. } => self.open_recent_project(owner, name),
            Entry::ProjectFile { file, .. } => self.open_recent_project_file(owner, file),
        }
    }

    pub(super) fn clear_open_recent(&mut self, owner: Option<window::Id>) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || self.open_recent.entries.is_empty() {
            return Task::none();
        }
        let cutoff = self.open_recent.entries.iter().fold(
            self.open_recent_cutoff().max(use_timestamp()),
            |latest, entry| match entry {
                Entry::Connection { used_at, .. } => latest.max(*used_at),
                Entry::Project { .. } | Entry::ProjectFile { .. } => latest,
            },
        );
        if self.save_open_recent(
            owner,
            Preferences {
                cleared_at: cutoff,
                projects: Vec::new(),
            },
        ) {
            self.open_recent.entries.clear();
            return self.refresh_open_recent();
        }
        Task::none()
    }

    pub(super) fn remember_open_recent_project(
        &mut self,
        owner: window::Id,
        name: &projects::Name,
    ) -> Task<Desktop> {
        self.remember_recent_project(owner, RememberedProject::Named(name.as_str().into()), None)
    }

    pub(super) fn remember_open_recent_project_file(
        &mut self,
        owner: window::Id,
        file: projects::ProjectFile,
        original: Option<&projects::ProjectFile>,
    ) -> Task<Desktop> {
        self.remember_recent_project(owner, RememberedProject::File(file), original)
    }

    fn remember_recent_project(
        &mut self,
        owner: window::Id,
        project: RememberedProject,
        original: Option<&projects::ProjectFile>,
    ) -> Task<Desktop> {
        let mut next = self.window_settings.open_recent.clone();
        next.remember(project, original);
        if next != self.window_settings.open_recent {
            self.save_open_recent(Some(owner), next);
        }
        self.refresh_open_recent()
    }

    fn save_open_recent(&mut self, owner: Option<window::Id>, preferences: Preferences) -> bool {
        let mut next = self.window_settings.clone();
        next.open_recent = preferences;
        match profile::window_settings_path().and_then(|path| next.save(&path)) {
            Ok(()) => {
                self.window_settings.open_recent = next.open_recent;
                true
            }
            Err(error) => {
                let message = format!("Could not save Open Recent ({error}).");
                if let Some(owner) = owner {
                    self.notice(owner, message);
                } else {
                    eprintln!("{message}");
                }
                false
            }
        }
    }
}

fn scan(settings: &Preferences) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Ok(store) = profile::connections_path().and_then(Connections::open) {
        entries.extend(connection_entries(&store, settings.cleared_at));
    }
    entries.extend(project_entries(
        settings,
        projects::Store::open().ok().as_ref(),
    ));
    entries
}

fn project_entries(settings: &Preferences, store: Option<&projects::Store>) -> Vec<Entry> {
    let names: Vec<_> = settings
        .projects
        .iter()
        .filter_map(|entry| match entry {
            RememberedProject::Named(name) => projects::Name::parse(name).ok(),
            RememberedProject::File(_) | RememberedProject::Imported { .. } => None,
        })
        .collect();
    let home = std::env::home_dir();
    let mut named = store
        .and_then(|store| store.recent(&names, settings.cleared_at).ok())
        .unwrap_or_default();
    let folder = store
        .map(|store| folder_label(store.directory(), home.as_deref()))
        .unwrap_or_default();
    let named_entry = |name: projects::Name| Entry::Project {
        label: menu_label(&format!("{} — {folder}", name.as_str())),
        name,
        cleared_at: settings.cleared_at,
    };
    let mut recent = Vec::new();
    for entry in &settings.projects {
        match entry {
            RememberedProject::Named(name) => {
                if let Some(index) = named.iter().position(|entry| entry.as_str() == name) {
                    recent.push(named_entry(named.remove(index)));
                }
            }
            RememberedProject::File(file)
            | RememberedProject::Imported {
                imported_file: file,
            } => {
                let imported = matches!(entry, RememberedProject::Imported { .. });
                if !imported && !file.is_present() {
                    continue;
                }
                let folder = file
                    .path()
                    .parent()
                    .map(|path| folder_label(path, home.as_deref()))
                    .unwrap_or_default();
                let prefix = if imported { "Imported: " } else { "" };
                recent.push(Entry::ProjectFile {
                    label: menu_label(&format!("{prefix}{} — {folder}", file.display_name())),
                    file: file.clone(),
                    cleared_at: settings.cleared_at,
                });
            }
        }
    }
    let remaining = projects::RECENT_LIMIT.saturating_sub(recent.len());
    recent.extend(named.into_iter().take(remaining).map(named_entry));
    recent
}

fn connection_entries(store: &Connections, cutoff: u64) -> Vec<Entry> {
    let mut recent: Vec<_> = store
        .recent_since(cutoff)
        .map(|(connection, used_at)| {
            let login = &connection.login.endpoint;
            let target = format!("{}@{}", login.username, login.host);
            let label = if connection.name == target {
                menu_label(&target)
            } else {
                menu_label(&format!("{} — {target}", connection.name))
            };
            let order = if used_at == 0 {
                0
            } else {
                store
                    .connections()
                    .iter()
                    .position(|saved| saved.id == connection.id)
                    .unwrap_or(usize::MAX)
            };
            (used_at, order, connection.id, label)
        })
        .collect();
    recent.extend(store.recent_serial_since(cutoff).map(|(profile, used_at)| {
        (
            used_at,
            store.connections().len()
                + store
                    .serial_profiles()
                    .iter()
                    .position(|saved| saved.id == profile.id)
                    .unwrap_or_default(),
            profile.id,
            menu_label(&profile.display_name()),
        )
    }));
    recent.extend(store.recent_local_since(cutoff).enumerate().map(
        |(index, (profile, used_at))| {
            (
                used_at,
                store.connections().len() + store.serial_profiles().len() + index,
                profile.id,
                menu_label(&profile.display_name()),
            )
        },
    ));
    recent.sort_by_key(|(used_at, order, ..)| (Reverse(*used_at), *order));
    recent
        .into_iter()
        .take(CONNECTION_LIMIT)
        .map(|(used_at, _, id, label)| Entry::Connection { id, label, used_at })
        .collect()
}

fn folder_label(folder: &Path, home: Option<&Path>) -> String {
    let display = home
        .and_then(|home| folder.strip_prefix(home).ok())
        .map_or_else(
            || folder.to_path_buf(),
            |relative| Path::new("~").join(relative),
        );
    let display = display.to_string_lossy();
    let count = display.chars().count();
    if count > 60 {
        format!("…{}", display.chars().skip(count - 59).collect::<String>())
    } else {
        display.into_owned()
    }
}

fn menu_label(text: &str) -> String {
    text.chars().filter(|character| {
        !character.is_control()
            && !matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }).take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_open_recent_restores_order_and_keeps_imported_paths_unprobed() {
        use crate::workspace::backup_data::{
            merge_java_window_settings, portable_java_preferences,
        };
        use std::collections::BTreeMap;

        #[cfg(windows)]
        let root = r"C:\kortty-recent-backup-fixture";
        #[cfg(not(windows))]
        let root = "/kortty-recent-backup-fixture";
        let xml = format!(
            "<globalSettings><openRecentClearedAt>1234000</openRecentClearedAt>
                <recentProjectPaths>
                    <path>{root}/second/Release.kortty</path>
                    <path>{root}/first/Release.kortty</path>
                    <path>{root}/second/Release.kortty</path>
                    <path>relative.kortty</path>
                    <path>{root}/unrelated.txt</path>
                </recentProjectPaths></globalSettings>"
        );
        let java = portable_java_preferences(xml.as_bytes(), None).unwrap();
        assert_eq!(java.recent_projects, 2);
        assert_eq!(java.omitted, 3);
        assert!(java.recent_list_cutoff);
        let incoming = java.contents.unwrap();
        let installed = merge_java_window_settings(None, &incoming, &BTreeMap::new())
            .unwrap()
            .unwrap();
        let mut history: Preferences = serde_json::from_value(
            serde_json::from_slice::<serde_json::Value>(&installed).unwrap()["open_recent"].clone(),
        )
        .unwrap();
        history.normalize();
        assert_eq!(history.cleared_at, 1234000);
        let recent = project_entries(&history, None);
        assert_eq!(recent.len(), 2);
        assert!(recent[0].label().starts_with("Imported: Release"));
        assert!(recent[0].label().contains("second"));
        assert!(recent[1].label().contains("first"));
        let Entry::ProjectFile { file, .. } = &recent[0] else {
            panic!("expected an imported project file");
        };
        let resolved =
            projects::ProjectFile::new(file.path().with_file_name("Resolved.kortty"), None)
                .unwrap();
        history.remember(RememberedProject::File(resolved.clone()), Some(file));
        assert_eq!(history.projects.len(), 2);
        assert!(matches!(&history.projects[0], RememberedProject::File(file) if file == &resolved));

        let local = br#"{"open_recent":{"cleared_at":9876,"projects":["Local"]}}"#;
        assert!(
            merge_java_window_settings(Some(local), &incoming, &BTreeMap::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn open_recent_project_files_keep_equal_filenames_distinct_and_drop_missing_paths()
    -> std::io::Result<()> {
        use std::fs;

        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let root = Scratch(
            std::env::temp_dir().join(format!("kortty-recent-{}", crate::private_files::nonce()?)),
        );
        let path = |folder: &str| root.0.join(folder).join("release.kortty");
        let first = projects::ProjectFile::new(path("team-one"), None)?;
        let second = projects::ProjectFile::new(path("team-two"), None)?;
        for file in [&first, &second] {
            fs::create_dir_all(file.path().parent().unwrap())?;
            // Recent scans check file presence only. The native and Java codecs have separate tests.
            fs::write(file.path(), b"fixture")?;
        }
        let mut prefs: Preferences = serde_json::from_str(r#"{"projects":["Legacy"]}"#)?;
        prefs.remember(RememberedProject::File(first.clone()), None);
        prefs.remember(RememberedProject::File(second.clone()), None);
        prefs.remember(RememberedProject::File(first.clone()), None);
        let mut reloaded: Preferences = serde_json::from_slice(&serde_json::to_vec(&prefs)?)?;
        reloaded.normalize();
        assert_eq!(reloaded, prefs);
        assert!(
            matches!(&reloaded.projects[2], RememberedProject::Named(name) if name == "Legacy")
        );

        let rows = project_entries(&reloaded, None);
        assert!(matches!(&rows[..],
            [Entry::ProjectFile { file: left, .. }, Entry::ProjectFile { file: right, .. }]
            if left == &first && right == &second
        ));
        assert!(rows[0].label().contains("team-one"));
        assert!(rows[1].label().contains("team-two"));
        fs::remove_file(first.path())?;
        assert!(matches!(&project_entries(&reloaded, None)[..],
            [Entry::ProjectFile { file, .. }] if file == &second
        ));
        Ok(())
    }

    #[test]
    fn open_recent_discards_scan_started_before_the_list_was_cleared() {
        let mut workspace = Workspace::default();
        let _ = workspace.refresh_open_recent();
        let before = workspace.open_recent.reading.unwrap();
        workspace.window_settings.open_recent.cleared_at = 100;
        let _ = workspace.refresh_open_recent();

        let _ = workspace.update_open_recent(Message::Loaded(
            before,
            vec![Entry::Connection {
                id: 42,
                label: "obsolete".into(),
                used_at: 90,
            }],
        ));
        assert!(workspace.open_recent_entries().is_empty());
        let current = workspace.open_recent.reading.unwrap();
        assert_ne!(before, current);
        let fresh = Entry::Connection {
            id: 42,
            label: "new use".into(),
            used_at: 101,
        };
        let _ = workspace.update_open_recent(Message::Loaded(current, vec![fresh.clone()]));
        assert_eq!(workspace.open_recent_entries(), [fresh]);
    }
}
