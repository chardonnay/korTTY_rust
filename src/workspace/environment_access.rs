use std::io;
use std::path::PathBuf;

use iced::{Color, Element, Size, Task, window};

use crate::connection_store::TabColor;
use crate::profile;
use crate::session::SessionId;
use crate::vault_window;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace};

mod catalog;
mod dialog;

pub(crate) use catalog::java as java_backup;
pub(crate) use catalog::{Catalog, DEFAULT_ENVIRONMENT};
use dialog::Dialog;
pub use dialog::Message;

#[derive(Default)]
pub(super) struct Environments {
    catalog: Catalog,
    open: Option<OpenDialog>,
}

struct OpenDialog {
    id: window::Id,
    owner: window::Id,
    path: Option<PathBuf>,
    editor: Dialog,
}

pub(super) struct Mark<'a> {
    source: MarkSource<'a>,
    color: TabColor,
}

enum MarkSource<'a> {
    Connection,
    Group(&'a str),
    Environment(&'a str),
}

impl Mark<'_> {
    pub(super) fn color(&self) -> Color {
        self.color.color()
    }

    pub(super) fn description(&self) -> String {
        match self.source {
            MarkSource::Connection => format!("Connection color · {}", self.color),
            MarkSource::Group(path) => format!("Folder color · {path} · {}", self.color),
            MarkSource::Environment(label) => format!("Environment · {label} · {}", self.color),
        }
    }
}

impl Environments {
    pub(super) fn load() -> Self {
        Self {
            catalog: environment_path()
                .and_then(|path| Catalog::read(&path))
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    pub(super) fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

impl Workspace {
    pub(super) fn show_environment(&mut self, owner: window::Id) -> Task<Desktop> {
        self.show_environment_at(owner, environment_path())
    }

    fn show_environment_at(
        &mut self,
        owner: window::Id,
        path: io::Result<PathBuf>,
    ) -> Task<Desktop> {
        if self.vault_id != Some(owner)
            || !self
                .vault
                .as_ref()
                .is_some_and(vault_window::VaultWindow::can_manage_environments)
        {
            return Task::none();
        }
        if let Some(dialog) = &self.environments.open {
            return window::gain_focus(dialog.id);
        }
        // The unlocked vault keeps the profile's exclusive vault lock while
        // this window is open. Read the catalog again after acquiring that lock.
        let (path, error) = match path {
            Ok(path) => match Catalog::read(&path) {
                Ok(catalog) => {
                    self.environments.catalog = catalog;
                    (Some(path), None)
                }
                Err(error) => (
                    None,
                    Some(format!("Could not read the environment catalog ({error}).")),
                ),
            },
            Err(error) => (
                None,
                Some(format!(
                    "Could not locate the environment catalog ({error})."
                )),
            ),
        };
        let (id, opened) = self.open_placed_dialog(
            DialogKind::Environments,
            window::Settings {
                size: Size::new(770.0, 450.0),
                min_size: Some(Size::new(600.0, 365.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.environments.open = Some(OpenDialog {
            id,
            owner,
            path,
            editor: Dialog::new(self.environments.catalog.clone(), error),
        });
        opened.map(|_| Desktop::Noop)
    }

    pub(super) fn environment_title(&self, id: window::Id) -> Option<&'static str> {
        self.environments
            .open
            .as_ref()
            .filter(|dialog| dialog.id == id)
            .map(|_| "Manage Environments")
    }

    pub(super) fn environment_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self
            .environments
            .open
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        let usage = dialog
            .editor
            .selected()
            .and_then(|entry| self.vault.as_ref()?.environment_usage(entry.id));
        Some(dialog.editor.view(usage).map(move |message| {
            Desktop::Vault(
                dialog.owner,
                vault_window::Message::Environment(id, message),
            )
        }))
    }

    pub(super) fn environment_for_vault(&self, owner: window::Id) -> Option<window::Id> {
        self.environments
            .open
            .as_ref()
            .filter(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(super) fn close_environment(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self
            .environments
            .open
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        let owner = dialog.owner;
        self.environments.open = None;
        Some(Task::batch([
            self.close_placed_dialog(id),
            window::gain_focus(owner),
        ]))
    }

    pub(super) fn close_environment_owner(&mut self, owner: window::Id) -> Task<Desktop> {
        let Some(id) = self.environment_for_vault(owner) else {
            return Task::none();
        };
        self.environments.open = None;
        self.close_placed_dialog(id)
    }

    pub(super) fn update_environment(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        if matches!(message, Message::Cancel) {
            return self.close_environment(id).unwrap_or_else(Task::none);
        }
        let Some(dialog) = self
            .environments
            .open
            .as_mut()
            .filter(|dialog| dialog.id == id && self.vault_id == Some(dialog.owner))
        else {
            return Task::none();
        };
        let Some(vault) = self
            .vault
            .as_ref()
            .filter(|vault| vault.can_manage_environments())
        else {
            return Task::none();
        };
        if matches!(message, Message::Save) {
            if !dialog.editor.ready() {
                return Task::none();
            }
            let current = &self.environments.catalog;
            let draft = &dialog.editor.draft;
            if current.entries().any(|entry| {
                !entry.built_in
                    && draft.get(entry.id).is_none()
                    && vault.environment_usage(entry.id) != Some(0)
            }) {
                dialog.editor.error =
                    Some("A credential uses an environment marked for deletion.".into());
                return Task::none();
            }
            if draft != current {
                let Some(path) = &dialog.path else {
                    return Task::none();
                };
                if let Err(error) = draft.save(path, current) {
                    dialog.editor.error =
                        Some(format!("Could not save the environment catalog ({error})."));
                    return Task::none();
                }
                self.environments.catalog = draft.clone();
            }
            return self.close_environment(id).unwrap_or_else(Task::none);
        }
        let usage = dialog
            .editor
            .selected()
            .and_then(|entry| vault.environment_usage(entry.id));
        dialog.editor.update(message, usage);
        Task::none()
    }

    pub(super) fn connection_color_mark(&self, session: SessionId) -> Option<Mark<'_>> {
        let pane = self.sessions.get(&session)?;
        if let Some(target) = &pane.ssh
            && let Some((color, group)) = self.saved_terminal_preferences.color(target)
        {
            return Some(Mark {
                source: group.map_or(MarkSource::Connection, MarkSource::Group),
                color,
            });
        }
        let credential = pane.credential_environment_source.as_deref()?;
        let id = self.vault.as_ref()?.credential_environment(credential)?;
        let entry = self.environments.catalog.get(id)?;
        Some(Mark {
            source: MarkSource::Environment(entry.label),
            color: entry.color?,
        })
    }
}

fn environment_path() -> io::Result<PathBuf> {
    Ok(profile::root()?.join("environments.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_store::{Connection, Store};
    use crate::session::{GridSize, Session};
    use crate::session_security::IsolationLevel;
    use crate::snippets::tests::Profile;
    use crate::ssh_dialog::LoginTarget;
    use crate::workspace::SshTarget;
    use catalog::Rgb;
    use kortty_terminal::TerminalProfile;
    use kortty_vault::{CredentialInput, Secret, Vault};

    #[test]
    fn environment_catalog_preserves_stable_references_and_refuses_stale_or_damaged_saves() {
        let profile = Profile::new();
        let path = profile.0.join("environments.json");
        let empty = Catalog::read(&path).unwrap();
        assert_eq!(empty.entries().count(), 4);
        assert!(empty.entries().all(|entry| entry.color.is_none()));
        let mut catalog = empty.clone();
        assert!(catalog.rename("PRODUCTION", "Other").is_err());
        assert!(!catalog.remove("STAGING"));
        assert!(catalog.add("left\u{202e}right").is_err());
        let id = catalog.add("  Canary  ").unwrap();
        let color = Rgb::parse("#f3a").unwrap();
        assert_eq!(color.to_string(), "#FF33AA");
        assert!(Rgb::parse("#12345678").is_none());
        catalog.set_color(&id, Some(color));
        catalog.set_color("PRODUCTION", Some(color));
        catalog.save(&path, &empty).unwrap();
        let mut updated = Catalog::read(&path).unwrap();
        assert_eq!(updated, catalog);
        updated.rename(&id, "Canary EU").unwrap();
        updated.save(&path, &catalog).unwrap();
        assert!(catalog.save(&path, &empty).is_err());
        let reopened = Catalog::read(&path).unwrap();
        assert_eq!(reopened.label(&id), Some("Canary EU"));
        assert_eq!(reopened.get(&id).unwrap().color, Some(color));
        assert_eq!(reopened.label("Production"), Some("Production"));
        let choices = reopened.choices("Older free-text category");
        assert_eq!(
            reopened
                .selection(&choices, "Older free-text category")
                .unwrap()
                .id,
            "Older free-text category"
        );
        std::fs::write(&path, b"damaged fixture").unwrap();
        assert!(catalog.save(&path, &reopened).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged fixture");
    }

    #[test]
    fn environment_dialog_guards_live_vault_references_and_applies_tab_color_only_on_save() {
        let profile = Profile::new();
        let vault_path = profile.0.join("vault.bin");
        let path = profile.0.join("environments.json");
        let mut catalog = Catalog::default();
        let id = catalog.add("Canary").unwrap();
        let old_color = Rgb::parse("#345").unwrap();
        let new_color = Rgb::parse("#abc").unwrap();
        catalog.set_color(&id, Some(old_color));
        catalog.save(&path, &Catalog::default()).unwrap();

        let mut vault = Vault::create(&vault_path, b"environment-test-master").unwrap();
        let credential = vault
            .save(
                CredentialInput {
                    label: "Canary login".into(),
                    environment: id.clone(),
                    ..CredentialInput::default()
                },
                Secret::new("environment-test-secret".into()),
            )
            .unwrap();
        let mut workspace = Workspace::default();
        let owner = window::Id::unique();
        workspace.vault_id = Some(owner);
        workspace.vault = Some(vault_window::VaultWindow::with_unlocked_fixture(
            vault_path, vault,
        ));
        let _ = workspace.show_environment_at(owner, Ok(path.clone()));
        let dialog = workspace.environment_for_vault(owner).unwrap();
        assert!(workspace.environment_view(dialog).is_some());
        assert_eq!(
            workspace.vault.as_ref().unwrap().environment_usage(&id),
            Some(1)
        );
        let _ = workspace.update_vault(owner, vault_window::Message::Lock);
        assert!(workspace.vault.as_ref().unwrap().can_manage_environments());

        // Exercise the tab badge with the metadata a successful SSH pane retains.
        // No transport or native window runs in this test.
        let (main, _) = workspace.open_window(false);
        let grid = GridSize::default();
        let ssh = Session::ssh(None, grid, TerminalProfile::Xterm, true, None).unwrap();
        let session = workspace.store_session(ssh, grid, 14);
        let pane = workspace.sessions.get_mut(&session).unwrap();
        let login = LoginTarget {
            host: "environment.example.test".into(),
            port: 22,
            username: "operator".into(),
        };
        pane.ssh = Some(
            SshTarget::from_imported_route(login.clone(), None, IsolationLevel::None).unwrap(),
        );
        pane.credential_environment_source = Some(credential.clone());
        workspace.add_tab(main, session);
        let layout = serde_json::to_string(&workspace.capture_layout().unwrap()).unwrap();
        assert!(layout.contains("environment.example.test"));
        assert!(!layout.contains(&credential));
        assert!(!layout.contains(&id));
        let update = |workspace: &mut Workspace, message| {
            let _ =
                workspace.update_vault(owner, vault_window::Message::Environment(dialog, message));
        };
        update(&mut workspace, Message::Add);
        update(&mut workspace, Message::Name("Temporary".into()));
        update(&mut workspace, Message::ApplyName);
        let added = workspace
            .environments
            .open
            .as_ref()
            .unwrap()
            .editor
            .selected()
            .unwrap()
            .id
            .to_owned();
        update(&mut workspace, Message::Rename);
        update(&mut workspace, Message::Name("Temporary EU".into()));
        update(&mut workspace, Message::ApplyName);
        assert_eq!(
            workspace
                .environments
                .open
                .as_ref()
                .unwrap()
                .editor
                .draft
                .label(&added),
            Some("Temporary EU")
        );
        update(&mut workspace, Message::Delete);
        update(&mut workspace, Message::ConfirmDelete(id.clone()));
        update(&mut workspace, Message::ConfirmDelete(added.clone()));
        assert!(
            workspace
                .environments
                .open
                .as_ref()
                .unwrap()
                .editor
                .draft
                .get(&added)
                .is_none()
        );

        update(&mut workspace, Message::Select(id.clone()));
        update(&mut workspace, Message::Delete);
        update(&mut workspace, Message::ConfirmDelete(id.clone()));
        update(&mut workspace, Message::Color("#abc".into()));
        assert_eq!(
            workspace.connection_color_mark(session).unwrap().color(),
            old_color.color()
        );
        update(&mut workspace, Message::Save);
        assert!(workspace.environment_title(dialog).is_none());
        assert_eq!(
            Catalog::read(&path).unwrap().get(&id).unwrap().color,
            Some(new_color)
        );
        let mark = workspace.connection_color_mark(session).unwrap();
        assert_eq!(mark.description(), "Environment · Canary · #AABBCC");
        assert_eq!(mark.color(), new_color.color());

        let mut store = Store::open(profile.0.join("connections.json")).unwrap();
        let folder = store.create_group(None, "Production").unwrap();
        let nested = store.create_group(Some(&folder), "Database").unwrap();
        let group_color = Rgb::parse("#b50").unwrap();
        store.set_group_color(&folder, Some(group_color)).unwrap();
        let mut connection = Connection {
            id: 0,
            name: "Canary login".into(),
            tag: String::new(),
            login: crate::connection_store::SavedLogin {
                endpoint: login.clone(),
                auth: crate::connection_store::Auth::Prompt,
                keyboard_interactive: false,
            },
            jump: None,
            emulation: TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: IsolationLevel::None,
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: Rgb::parse("#123"),
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        };
        connection.id = store
            .save_in_group(connection.clone(), Some(&nested))
            .unwrap();
        let mut target = serde_json::to_value(&login).unwrap();
        target["saved"] = connection.id.into();
        workspace.sessions.get_mut(&session).unwrap().ssh =
            Some(serde_json::from_value(target).unwrap());
        workspace.update_connection_store(store.clone());
        let mark = workspace.connection_color_mark(session).unwrap();
        assert_eq!(mark.description(), "Connection color · #112233");
        assert_eq!(
            workspace
                .terminal_tab_color(workspace.windows[&main].tab().unwrap())
                .unwrap()
                .color(),
            mark.color()
        );

        connection.login.endpoint.port = 2222;
        store.save(connection.clone()).unwrap();
        workspace.update_connection_store(store.clone());
        assert_eq!(
            workspace.connection_color_mark(session).unwrap().color(),
            new_color.color()
        );

        connection.login.endpoint = login;
        connection.tab_color = None;
        store.save(connection).unwrap();
        workspace.update_connection_store(store.clone());
        assert_eq!(
            workspace
                .connection_color_mark(session)
                .unwrap()
                .description(),
            "Folder color · Production · #BB5500"
        );

        let nested_color = Rgb::parse("#abc").unwrap();
        store.set_group_color(&nested, Some(nested_color)).unwrap();
        workspace.update_connection_store(store.clone());
        assert_eq!(
            workspace
                .connection_color_mark(session)
                .unwrap()
                .description(),
            "Folder color · Production/Database · #AABBCC"
        );

        store.set_group_color(&nested, None).unwrap();
        store.set_group_color(&folder, None).unwrap();
        workspace.update_connection_store(store);
        assert_eq!(
            workspace.connection_color_mark(session).unwrap().color(),
            new_color.color()
        );

        let _ = workspace.show_environment_at(owner, Ok(path.clone()));
        let next = workspace.environment_for_vault(owner).unwrap();
        update(&mut workspace, Message::Save);
        assert_eq!(workspace.environment_for_vault(owner), Some(next));
        let _ = workspace.update_environment(next, Message::Select(id.clone()));
        let _ = workspace.update_environment(next, Message::Color("#000".into()));
        let _ = workspace.update_environment(next, Message::Cancel);
        assert_eq!(
            workspace.connection_color_mark(session).unwrap().color(),
            new_color.color()
        );
        let _ = workspace.update_vault(owner, vault_window::Message::Lock);
        assert!(workspace.connection_color_mark(session).is_none());
    }
}
