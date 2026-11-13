//! Native editor for custom OS symbols assigned to saved connections.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::{Element, Size, Task, window};

use crate::connection_store::custom_os_badges::{Catalog, CustomBadge};
use crate::profile;
use crate::sftp_window::{Directory, EntryKind, Location, list_local};
use crate::ssh_dialog::unsafe_text;

use super::{Message, Workspace, chrome::focus_dialog_field, dialog_placement::Kind};

mod view;

const TITLE: &str = "OS symbols";
const FAMILY: &str = "Custom";
const NAME_INPUT: &str = "os-badge-manager-name";
const PAGE_SIZE: usize = 48;
const MAX_PATH_BYTES: usize = 8192;
const MAX_NAME_BYTES: usize = 512;

#[derive(Clone, Debug)]
pub enum Event {
    Focus,
    Close,
    Dismiss,
    Loaded(Result<Arc<Catalog>, String>),
    Select(String),
    Add,
    Rename,
    Delete,
    BrowsePath(String),
    OpenPath,
    Parent,
    Page(bool),
    Pick(u64, usize),
    Listed(u64, Result<Arc<Directory>, String>),
    Name(String),
    SaveName,
    ConfirmDelete,
    Saved(u64, Result<Saved, String>),
}

#[derive(Clone, Debug)]
pub struct Saved {
    catalog: Arc<Catalog>,
    selected: Option<String>,
}

#[derive(Default)]
pub(super) struct State {
    dialogs: BTreeMap<window::Id, Dialog>,
}

struct Dialog {
    id: window::Id,
    owner: window::Id,
    focus_scope: iced::widget::Id,
    catalog: Arc<Catalog>,
    catalog_state: CatalogState,
    selected: Option<String>,
    mode: Mode,
    generation: u64,
    writing: bool,
    problem: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CatalogState {
    Loading,
    Ready,
    Unavailable,
}

enum Mode {
    List,
    Browse(Browser),
    Name { target: Target, value: String },
    ConfirmDelete { reference: String, name: String },
}

#[derive(Clone)]
enum Target {
    Add(PathBuf),
    Rename { reference: String, family: String },
}

enum Change {
    Name(Target, String),
    Delete(String),
}

struct Browser {
    folder: PathBuf,
    directory: Option<Arc<Directory>>,
    page: usize,
    path: String,
    loading: bool,
}

impl Dialog {
    fn new(id: window::Id, owner: window::Id, catalog: Arc<Catalog>) -> Self {
        Self {
            id,
            owner,
            focus_scope: iced::widget::Id::unique(),
            catalog,
            catalog_state: CatalogState::Loading,
            selected: None,
            mode: Mode::List,
            generation: 0,
            writing: false,
            problem: None,
        }
    }

    fn selected(&self) -> Option<&CustomBadge> {
        let reference = self.selected.as_deref()?;
        self.catalog
            .entries()
            .iter()
            .find(|badge| badge.reference() == reference)
    }

    fn advance(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.problem = None;
        self.generation
    }

    fn browse(&mut self, folder: PathBuf, path: String) -> Task<Message> {
        let generation = self.advance();
        self.mode = Mode::Browse(Browser {
            folder: folder.clone(),
            directory: None,
            page: 0,
            path,
            loading: true,
        });
        let id = self.id;
        Task::perform(
            async move { list_images(&folder).map_err(|error| error.to_string()) },
            move |result| message(id, Event::Listed(generation, result)),
        )
    }

    fn browse_path(&mut self, path: PathBuf) -> Task<Message> {
        let Mode::Browse(browser) = &self.mode else {
            return Task::none();
        };
        self.browse(path, browser.path.clone())
    }

    fn choose_image(&mut self, path: PathBuf) -> Task<Message> {
        if !supported_image(&path) {
            self.problem = Some("Choose a PNG, JPG, GIF or SVG image.".into());
            return Task::none();
        }
        let value = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|name| name.len() <= MAX_NAME_BYTES && !unsafe_text(name))
            .unwrap_or_default()
            .to_owned();
        self.advance();
        self.mode = Mode::Name {
            target: Target::Add(path),
            value,
        };
        focus_dialog_field(self.focus_scope.clone(), NAME_INPUT)
    }

    fn save(&mut self, change: Change) -> Task<Message> {
        let generation = self.advance();
        let id = self.id;
        let mut catalog = Arc::clone(&self.catalog);
        self.writing = true;
        Task::perform(
            async move {
                let edit = Arc::make_mut(&mut catalog);
                let selected = match change {
                    Change::Name(Target::Add(source), name) => {
                        Some(edit.add(&name, FAMILY, &source).map_err(problem)?)
                    }
                    Change::Name(Target::Rename { reference, family }, name) => {
                        edit.rename(&reference, &name, &family).map_err(problem)?;
                        Some(reference)
                    }
                    Change::Delete(reference) => {
                        edit.remove(&reference).map_err(problem)?;
                        None
                    }
                };
                Ok(Saved { catalog, selected })
            },
            move |result| message(id, Event::Saved(generation, result)),
        )
    }

    fn finish_save(&mut self, result: Result<Saved, String>) -> Option<Arc<Catalog>> {
        self.writing = false;
        match result {
            Ok(Saved { catalog, selected }) => {
                self.catalog = Arc::clone(&catalog);
                self.selected = selected;
                self.mode = Mode::List;
                Some(catalog)
            }
            Err(error) => {
                self.problem = Some(error);
                None
            }
        }
    }

    fn finish_load(&mut self, result: Result<Arc<Catalog>, String>) -> Option<Arc<Catalog>> {
        match result {
            Ok(catalog) => {
                self.catalog_state = CatalogState::Ready;
                if Arc::ptr_eq(&self.catalog, &catalog) {
                    return None;
                }
                self.catalog = Arc::clone(&catalog);
                Some(catalog)
            }
            Err(error) => {
                self.catalog_state = CatalogState::Unavailable;
                self.problem = Some(format!("Could not load the OS symbols ({error})."));
                None
            }
        }
    }

    fn handle(&mut self, action: Event) -> Task<Message> {
        if self.writing || self.catalog_state != CatalogState::Ready {
            return Task::none();
        }
        match action {
            Event::Dismiss => {
                self.advance();
                self.mode = Mode::List;
            }
            Event::Select(reference) if matches!(self.mode, Mode::List) => {
                if self
                    .catalog
                    .entries()
                    .iter()
                    .any(|entry| entry.reference() == reference)
                {
                    self.selected = Some(reference);
                }
            }
            Event::Add if matches!(self.mode, Mode::List) => {
                let folder = std::env::home_dir().or_else(|| std::env::current_dir().ok());
                if let Some(folder) = folder {
                    return self.browse(folder, String::new());
                }
                self.problem = Some("Could not find a starting folder.".into());
            }
            Event::Rename if matches!(self.mode, Mode::List) => {
                if let Some(badge) = self.selected() {
                    self.mode = Mode::Name {
                        target: Target::Rename {
                            reference: badge.reference().into(),
                            family: badge.family().into(),
                        },
                        value: badge.name().into(),
                    };
                    self.problem = None;
                    return focus_dialog_field(self.focus_scope.clone(), NAME_INPUT);
                }
            }
            Event::Delete if matches!(self.mode, Mode::List) => {
                if let Some(badge) = self.selected() {
                    self.mode = Mode::ConfirmDelete {
                        reference: badge.reference().into(),
                        name: badge.name().into(),
                    };
                    self.problem = None;
                }
            }
            Event::Listed(generation, result) if generation == self.generation => {
                if let Mode::Browse(browser) = &mut self.mode {
                    browser.loading = false;
                    match result {
                        Ok(directory) => {
                            if let Location::Local(folder) = &directory.path {
                                browser.folder.clone_from(folder);
                            }
                            browser.directory = Some(directory);
                        }
                        Err(error) => {
                            self.problem = Some(format!("Could not open the folder ({error})."));
                        }
                    }
                }
            }
            Event::BrowsePath(path) if path.len() <= MAX_PATH_BYTES && !unsafe_text(&path) => {
                if let Mode::Browse(browser) = &mut self.mode {
                    browser.path = path;
                    self.problem = None;
                }
            }
            Event::OpenPath => {
                if let Mode::Browse(browser) = &self.mode
                    && let Some(path) = browser.absolute_path()
                {
                    return self.choose_image(path);
                }
            }
            Event::Parent => {
                if let Mode::Browse(browser) = &self.mode
                    && !browser.loading
                    && let Some(parent) = browser.folder.parent()
                {
                    return self.browse_path(parent.to_owned());
                }
            }
            Event::Page(forward) => {
                if let Mode::Browse(browser) = &mut self.mode {
                    browser.page = if forward {
                        (browser.page + 1).min(browser.max_page())
                    } else {
                        browser.page.saturating_sub(1)
                    };
                }
            }
            Event::Pick(generation, index) if generation == self.generation => {
                if let Mode::Browse(browser) = &self.mode
                    && let Some((path, kind)) = browser.pick(index)
                {
                    return if kind == EntryKind::Directory {
                        self.browse_path(path)
                    } else {
                        self.choose_image(path)
                    };
                }
            }
            Event::Name(name) if name.len() <= MAX_NAME_BYTES && !unsafe_text(&name) => {
                if let Mode::Name { value, .. } = &mut self.mode {
                    *value = name;
                    self.problem = None;
                }
            }
            Event::SaveName => {
                if let Mode::Name { target, value } = &self.mode
                    && !value.trim().is_empty()
                {
                    return self.save(Change::Name(target.clone(), value.trim().to_owned()));
                }
            }
            Event::ConfirmDelete => {
                if let Mode::ConfirmDelete { reference, .. } = &self.mode {
                    return self.save(Change::Delete(reference.clone()));
                }
            }
            _ => {}
        }
        Task::none()
    }
}

impl Browser {
    fn absolute_path(&self) -> Option<PathBuf> {
        let path = Path::new(&self.path);
        path.is_absolute().then(|| path.to_owned())
    }

    fn max_page(&self) -> usize {
        self.directory.as_ref().map_or(0, |directory| {
            directory.entries.len().saturating_sub(1) / PAGE_SIZE
        })
    }

    fn pick(&self, index: usize) -> Option<(PathBuf, EntryKind)> {
        if self.loading || index / PAGE_SIZE != self.page {
            return None;
        }
        let directory = self.directory.as_ref()?;
        let entry = directory.entries.get(index)?;
        let Location::Local(path) = directory.path.with(&entry.name)? else {
            return None;
        };
        Some((path, entry.kind))
    }
}

fn supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "gif", "svg"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn list_images(path: &Path) -> std::io::Result<Arc<Directory>> {
    let mut directory = list_local(path)?;
    directory.entries.retain(|entry| {
        entry.kind == EntryKind::Directory
            || (entry.kind == EntryKind::File
                && directory.path.with(&entry.name).is_some_and(
                    |location| matches!(location, Location::Local(path) if supported_image(&path)),
                ))
    });
    Ok(Arc::new(directory))
}

fn problem(error: std::io::Error) -> String {
    format!("The symbol could not be added or changed ({error}).")
}

fn read_catalog(cached: Arc<Catalog>) -> Result<Arc<Catalog>, String> {
    let root = profile::root().map_err(|error| error.to_string())?;
    let loaded = Catalog::load(&root).map_err(|error| error.to_string())?;
    let same = loaded
        .entries()
        .iter()
        .map(badge_data)
        .eq(cached.entries().iter().map(badge_data));
    Ok(if same { cached } else { Arc::new(loaded) })
}

fn badge_data(badge: &CustomBadge) -> (&str, &str, &str, &[u8]) {
    (badge.reference(), badge.name(), badge.family(), badge.png())
}

pub(super) fn message(id: window::Id, event: Event) -> Message {
    Message::OsBadgeManager(id, event)
}

impl Workspace {
    pub(super) fn show_os_badge_manager(&mut self, owner: window::Id) -> Task<Message> {
        if let Some((id, focus)) = self.owner_dialog(owner) {
            return window::gain_focus(id).chain(Task::done(focus));
        }
        let Some(manager) = self.connection_managers.get(&owner) else {
            return Task::none();
        };
        let catalog = manager.window.custom_badges();
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            Kind::OsBadgeManager,
            window::Settings {
                size: Size::new(540.0, 470.0),
                min_size: Some(Size::new(360.0, 300.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(|id| message(id, Event::Focus)),
        );
        self.os_badge_manager
            .dialogs
            .insert(id, Dialog::new(id, owner, Arc::clone(&catalog)));
        Task::batch([
            opened,
            Task::perform(async move { read_catalog(catalog) }, move |result| {
                message(id, Event::Loaded(result))
            }),
        ])
    }

    pub(super) fn os_badge_manager_title(&self, id: window::Id) -> Option<&'static str> {
        self.os_badge_manager
            .dialogs
            .contains_key(&id)
            .then_some(TITLE)
    }

    pub(super) fn os_badge_manager_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        let dialog = self.os_badge_manager.dialogs.get(&id)?;
        Some(dialog.view().map(move |event| message(id, event)))
    }

    pub(super) fn os_badge_manager_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.os_badge_manager
            .dialogs
            .values()
            .find(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(super) fn os_badge_manager_saving(&self) -> Option<window::Id> {
        self.os_badge_manager
            .dialogs
            .values()
            .find(|dialog| dialog.writing)
            .map(|dialog| dialog.id)
    }

    pub(super) fn close_os_badge_manager(&mut self, id: window::Id) -> Option<Task<Message>> {
        if self.os_badge_manager.dialogs.get(&id)?.writing {
            return Some(Task::none());
        }
        let dialog = self.os_badge_manager.dialogs.remove(&id)?;
        Some(self.close_placed_dialog_and_focus(id, dialog.owner))
    }

    pub(super) fn update_os_badge_manager(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        let another_save = matches!(event, Event::SaveName | Event::ConfirmDelete)
            && self
                .os_badge_manager_saving()
                .is_some_and(|saving| saving != id);
        let Some(dialog) = self.os_badge_manager.dialogs.get_mut(&id) else {
            return Task::none();
        };
        match event {
            Event::Focus => return window::gain_focus(id),
            Event::Close => return self.close_os_badge_manager(id).unwrap_or_else(Task::none),
            Event::Dismiss if matches!(dialog.mode, Mode::List) => {
                return self.close_os_badge_manager(id).unwrap_or_else(Task::none);
            }
            Event::Loaded(result) if dialog.catalog_state == CatalogState::Loading => {
                if let Some(manager) = self.connection_managers.get_mut(&dialog.owner) {
                    let latest = manager.window.custom_badges();
                    let result = if Arc::ptr_eq(&dialog.catalog, &latest) {
                        result
                    } else {
                        // A peer saved after this read started.
                        Ok(latest)
                    };
                    if let Some(catalog) = dialog.finish_load(result) {
                        manager.window.replace_custom_badges(catalog);
                    }
                } else {
                    dialog.finish_load(result);
                }
            }
            Event::Saved(generation, result)
                if dialog.writing && generation == dialog.generation =>
            {
                if let Some(catalog) = dialog.finish_save(result) {
                    for manager in self.connection_managers.values_mut() {
                        manager.window.replace_custom_badges(Arc::clone(&catalog));
                    }
                    for (&peer, dialog) in &mut self.os_badge_manager.dialogs {
                        if peer != id
                            && dialog.catalog_state == CatalogState::Ready
                            && matches!(dialog.mode, Mode::List)
                        {
                            dialog.catalog = Arc::clone(&catalog);
                            if dialog.selected().is_none() {
                                dialog.selected = None;
                            }
                        }
                    }
                }
            }
            Event::SaveName | Event::ConfirmDelete if another_save => {
                dialog.problem =
                    Some("Another OS symbols window is saving. Try again shortly.".into());
            }
            event => return dialog.handle(event),
        }
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_close_respects_its_symbols_dialog_and_the_native_menu_targets_the_dialog() {
        use crate::connection_window::ConnectionWindow;
        use crate::workspace::profile_tools::ConnectionManager;

        let mut workspace = Workspace::default();
        let (main, _) = workspace.open_window(false);
        let owner = window::Id::unique();
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir()
            .join(format!(
                "kortty-os-symbols-modal-{:016x}",
                u64::from_ne_bytes(nonce)
            ))
            .join("connections.json");
        workspace.connection_managers.insert(
            owner,
            ConnectionManager::new(main, ConnectionWindow::open(path).unwrap()),
        );
        let _ = workspace.show_os_badge_manager(owner);
        let id = workspace.os_badge_manager_for_owner(owner).unwrap();
        assert_eq!(workspace.owner_dialog(owner).map(|(id, _)| id), Some(id));
        assert_eq!(workspace.placed_dialog_owner(id), Some(owner));
        assert_eq!(workspace.owner_dialog(main).map(|(id, _)| id), Some(id));

        let _ = workspace.close_window(owner);
        assert!(workspace.connection_managers.contains_key(&owner));
        assert_eq!(workspace.os_badge_manager_for_owner(owner), Some(id));

        #[cfg(target_os = "macos")]
        {
            let _ = workspace.update(Message::Window(id, window::Event::Focused));
            let _ = workspace.native_menu(crate::workspace::chrome::Action::CloseWindow as u16);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = workspace.close_window(id);

        assert!(workspace.os_badge_manager_for_owner(owner).is_none());
        assert!(workspace.connection_managers.contains_key(&owner));
        assert!(workspace.windows.contains_key(&main));
    }

    #[test]
    fn cancelled_file_picker_ignores_its_late_listing() {
        let id = window::Id::unique();
        let catalog = Arc::new(Catalog::empty(Path::new("unused-in-memory-catalog")));
        let mut dialog = Dialog::new(id, window::Id::unique(), catalog);
        dialog.catalog_state = CatalogState::Ready;
        // Test double: the task is never run and its completion is injected below.
        let _ = dialog.browse(PathBuf::new(), String::new());
        let old_generation = dialog.generation;
        let _ = dialog.handle(Event::Dismiss);
        assert!(matches!(dialog.mode, Mode::List));

        let _ = dialog.browse(PathBuf::new(), String::new());
        let _ = dialog.handle(Event::Listed(old_generation, Err("old response".into())));
        assert!(dialog.problem.is_none());
        assert!(matches!(
            dialog.mode,
            Mode::Browse(Browser { loading: true, .. })
        ));
    }
}
