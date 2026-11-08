//! Options are applied only to an authenticated, immutable import preview.

use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, pick_list, radio, row, rule, scrollable, text, text_input,
};
use iced::{Element, Length};

use crate::connection_store::MAX_GROUP_PATH_BYTES;
use crate::connection_store::import::{
    CredentialReplacement, Destination, Options, Prepared, SourceGroup,
};
use crate::connection_window::CredentialChoice;
use crate::ssh_dialog::KeyChoice;
use crate::vault_window::{Kind as CredentialKind, VaultWindow};

use super::LoadedPreview;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DestinationMode {
    #[default]
    Preserve,
    Existing,
    New,
}

#[derive(Clone, Copy, Debug)]
pub enum Field {
    Username,
    Password,
    Tunnels,
    Jump,
}

#[derive(Clone, Debug)]
pub enum Edit {
    Filter(bool),
    Group(SourceGroup, bool),
    AllGroups(bool),
    Field(Field, bool),
    Destination(DestinationMode),
    ExistingFolder(String),
    NewFolder(String),
    ReplaceCredential(bool),
    Credential(String),
    ReplaceKey(bool),
    Key(u64),
}

#[derive(Default)]
pub(super) struct Draft {
    fields: Options,
    destination: DestinationMode,
    existing_folder: Option<String>,
    new_folder: String,
    replace_credential: bool,
    credential: Option<String>,
    replace_key: bool,
    key: Option<u64>,
    revision: u64,
    prepared: Option<Arc<Prepared>>,
    verified_credential: Option<CredentialChoice>,
    problem: Option<String>,
}

pub(super) struct Choices<'a> {
    pub(super) passwords: &'a [CredentialChoice],
    pub(super) keys: &'a [KeyChoice],
}

impl Draft {
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn prepared(&self) -> Option<&Arc<Prepared>> {
        self.prepared.as_ref()
    }

    pub(super) fn can_import(&self, choices: &Choices<'_>) -> bool {
        self.references_available(choices)
            && self.prepared().is_some_and(|plan| {
                plan.outcome().imported != 0 && plan.unimported_passwords() == 0
            })
    }

    fn references_available(&self, choices: &Choices<'_>) -> bool {
        (!self.replace_credential
            || self
                .verified_credential
                .as_ref()
                .is_some_and(|current| choices.passwords.contains(current)))
            && (!self.replace_key
                || self
                    .key
                    .is_some_and(|id| choices.keys.iter().any(|choice| choice.id == id)))
    }

    pub(super) fn edit(
        &mut self,
        edit: Edit,
        loaded: &LoadedPreview,
        choices: &Choices<'_>,
        vault: Option<&VaultWindow>,
    ) {
        match edit {
            Edit::Filter(value) => {
                self.fields.source_groups = value.then(|| loaded.preview.source_groups());
            }
            Edit::Group(group, selected) => {
                if let Some(groups) = &mut self.fields.source_groups {
                    if selected && loaded.preview.source_groups().contains(&group) {
                        groups.insert(group);
                    } else if !selected {
                        groups.remove(&group);
                    }
                }
            }
            Edit::AllGroups(selected) => {
                if let Some(groups) = &mut self.fields.source_groups {
                    if selected {
                        *groups = loaded.preview.source_groups();
                    } else {
                        groups.clear();
                    }
                }
            }
            Edit::Field(field, value) => match field {
                Field::Username => self.fields.include_username = value,
                Field::Password => self.fields.include_password = value,
                Field::Tunnels => self.fields.include_tunnels = value,
                Field::Jump => self.fields.include_jump = value,
            },
            Edit::Destination(mode) => self.destination = mode,
            Edit::ExistingFolder(folder)
                if self.destination == DestinationMode::Existing
                    && loaded.store.has_group(&folder) =>
            {
                self.existing_folder = Some(folder);
            }
            Edit::NewFolder(folder)
                if self.destination == DestinationMode::New
                    && folder.len() <= MAX_GROUP_PATH_BYTES
                    && !crate::ssh_dialog::unsafe_text(&folder) =>
            {
                self.new_folder = folder;
            }
            Edit::ReplaceCredential(value) if !value || !choices.passwords.is_empty() => {
                self.replace_credential = value;
                self.credential = None;
            }
            Edit::Credential(id)
                if self.replace_credential
                    && choices.passwords.iter().any(|choice| choice.id == id) =>
            {
                self.credential = Some(id);
            }
            Edit::ReplaceKey(value) if !value || !choices.keys.is_empty() => {
                self.replace_key = value;
                self.key = None;
            }
            Edit::Key(id)
                if self.replace_key && choices.keys.iter().any(|choice| choice.id == id) =>
            {
                self.key = Some(id);
            }
            _ => return,
        }
        self.revision = self.revision.wrapping_add(1);
        self.refresh(loaded, choices, vault);
    }

    pub(super) fn refresh(
        &mut self,
        loaded: &LoadedPreview,
        choices: &Choices<'_>,
        vault: Option<&VaultWindow>,
    ) {
        let result = self.prepare(loaded, choices, vault);
        self.prepared = None;
        self.verified_credential = None;
        self.problem = None;
        match result {
            Ok((prepared, credential)) => {
                self.prepared = Some(Arc::new(prepared));
                self.verified_credential = credential;
            }
            Err(problem) => self.problem = Some(problem),
        }
    }

    fn prepare(
        &self,
        loaded: &LoadedPreview,
        choices: &Choices<'_>,
        vault: Option<&VaultWindow>,
    ) -> Result<(Prepared, Option<CredentialChoice>), String> {
        let mut options = self.fields.clone();
        options.destination = match self.destination {
            DestinationMode::Preserve => Destination::Preserve,
            DestinationMode::Existing => Destination::Existing(
                self.existing_folder
                    .clone()
                    .ok_or("Select a destination folder.")?,
            ),
            DestinationMode::New => {
                if self.new_folder.trim().is_empty() {
                    return Err("Enter a name for the destination folder.".into());
                }
                Destination::New(self.new_folder.trim().to_owned())
            }
        };
        let credential = if self.replace_credential {
            let choice = choices
                .passwords
                .iter()
                .find(|choice| self.credential.as_deref() == Some(choice.id.as_str()))
                .ok_or("Select an available stored credential or turn off replacement.")?;
            options.credential = Some(CredentialReplacement {
                id: choice.id.clone(),
                username: Some(choice.username.clone()),
            });
            Some(choice.clone())
        } else {
            None
        };
        if self.replace_key {
            options.key = Some(
                self.key
                    .filter(|id| choices.keys.iter().any(|choice| choice.id == *id))
                    .ok_or("Select an available registered SSH key or turn off replacement.")?,
            );
        }
        let prepared = loaded
            .preview
            .prepare(&options, &loaded.store)
            .map_err(|error| error.to_string())?;
        validate_scope(&prepared, credential.as_ref(), vault)?;
        Ok((prepared, credential))
    }

    pub(super) fn commit_plan(
        &self,
        choices: &Choices<'_>,
        vault: Option<&VaultWindow>,
    ) -> Result<Arc<Prepared>, String> {
        if !self.references_available(choices) {
            return Err(
                "The selected credential or key changed. Review the import options again.".into(),
            );
        }
        let prepared = self
            .prepared()
            .filter(|plan| plan.outcome().imported != 0)
            .ok_or("No new connections are ready to import.")?;
        if prepared.unimported_passwords() != 0 {
            return Err("The selected Java passwords need the source master password. Choose a local credential or clear Passwords before importing.".into());
        }
        validate_scope(prepared, self.verified_credential.as_ref(), vault)?;
        Ok(Arc::clone(prepared))
    }

    pub(super) fn view<'a>(
        &'a self,
        loaded: &'a LoadedPreview,
        choices: &Choices<'_>,
        ready: bool,
    ) -> Element<'a, Edit> {
        let groups = loaded.preview.source_groups();
        let mut body = column![
            text("Source folders").size(16),
            checkbox(self.fields.source_groups.is_some())
                .label("Import only selected folders")
                .on_toggle_maybe((ready && !groups.is_empty()).then_some(Edit::Filter)),
        ]
        .spacing(6);
        if let Some(selected) = &self.fields.source_groups {
            let items = column(groups.into_iter().map(|group| {
                let label = match &group {
                    SourceGroup::Ungrouped => "Ungrouped connections".to_owned(),
                    SourceGroup::Named(name) => format!("Folder {name}"),
                };
                checkbox(selected.contains(&group))
                    .label(label)
                    .on_toggle_maybe(
                        ready.then_some(move |value| Edit::Group(group.clone(), value)),
                    )
                    .into()
            }))
            .spacing(4);
            body = body
                .push(
                    row![
                        button("Select all folders")
                            .on_press_maybe(ready.then_some(Edit::AllGroups(true))),
                        button("Deselect all")
                            .on_press_maybe(ready.then_some(Edit::AllGroups(false))),
                    ]
                    .spacing(8),
                )
                .push(scrollable(items).height(100));
        }

        body = body
            .push(rule::horizontal(1))
            .push(text("Fields to import").size(16));
        for (field, enabled, label) in [
            (Field::Username, self.fields.include_username, "Usernames"),
            (Field::Password, self.fields.include_password, "Passwords"),
            (
                Field::Tunnels,
                self.fields.include_tunnels,
                "SSH forwarding rules",
            ),
            (Field::Jump, self.fields.include_jump, "Jump servers"),
        ] {
            body = body.push(
                checkbox(enabled)
                    .label(label)
                    .on_toggle_maybe(ready.then_some(move |value| Edit::Field(field, value))),
            );
        }

        body = body
            .push(rule::horizontal(1))
            .push(text("Local authentication replacements").size(16))
            .push(text("The profile keeps its imported authentication method.").size(13))
            .push(
                checkbox(self.replace_credential)
                    .label("Use a local stored credential")
                    .on_toggle_maybe(
                        (ready && (self.replace_credential || !choices.passwords.is_empty()))
                            .then_some(Edit::ReplaceCredential),
                    ),
            );
        if self.replace_credential && !choices.passwords.is_empty() && ready {
            let selected = choices
                .passwords
                .iter()
                .find(|choice| self.credential.as_deref() == Some(choice.id.as_str()))
                .cloned();
            body = body.push(
                pick_list(choices.passwords.to_vec(), selected, |choice| {
                    Edit::Credential(choice.id)
                })
                .placeholder("Select a stored credential")
                .width(Length::Fill),
            );
        }
        body = body.push(
            checkbox(self.replace_key)
                .label("Use a registered SSH key")
                .on_toggle_maybe(
                    (ready && (self.replace_key || !choices.keys.is_empty()))
                        .then_some(Edit::ReplaceKey),
                ),
        );
        if self.replace_key && !choices.keys.is_empty() && ready {
            let selected = choices
                .keys
                .iter()
                .find(|choice| Some(choice.id) == self.key)
                .cloned();
            body = body.push(
                pick_list(choices.keys.to_vec(), selected, |choice| {
                    Edit::Key(choice.id)
                })
                .placeholder("Select a registered SSH key")
                .width(Length::Fill),
            );
        }

        body = body
            .push(rule::horizontal(1))
            .push(text("Destination folder").size(16));
        if ready {
            for (mode, label) in [
                (DestinationMode::Preserve, "Keep source folders"),
                (DestinationMode::Existing, "Use an existing folder"),
                (
                    DestinationMode::New,
                    "Create a folder and use it for this import",
                ),
            ] {
                body = body.push(radio(
                    label,
                    mode,
                    Some(self.destination),
                    Edit::Destination,
                ));
            }
        }
        match self.destination {
            DestinationMode::Preserve => {}
            DestinationMode::Existing if ready => {
                body = body.push(
                    pick_list(
                        loaded.store.groups().map(str::to_owned).collect::<Vec<_>>(),
                        self.existing_folder.clone(),
                        Edit::ExistingFolder,
                    )
                    .placeholder("Select an existing folder")
                    .width(Length::Fill),
                );
            }
            DestinationMode::New => {
                body = body.push(
                    text_input("New folder or nested path", &self.new_folder)
                        .on_input_maybe(ready.then_some(Edit::NewFolder))
                        .width(Length::Fill),
                );
            }
            _ => {}
        }
        if let Some(problem) = &self.problem {
            body = body.push(text(problem).size(13));
        } else if !self.references_available(choices) {
            body =
                body.push(text("The selected credential or key is no longer available.").size(13));
        }
        if self
            .prepared
            .as_ref()
            .is_some_and(|plan| plan.unimported_passwords() != 0)
        {
            body = body.push(text("The selected Java passwords need the source master password. Choose a local credential or clear Passwords before importing.").size(13));
        }
        body.into()
    }
}

fn validate_scope(
    prepared: &Prepared,
    credential: Option<&CredentialChoice>,
    vault: Option<&VaultWindow>,
) -> Result<(), String> {
    let Some(credential) = credential else {
        return Ok(());
    };
    let vault = vault.ok_or("Unlock the vault to use the selected credential.")?;
    let allowed = prepared.entries().all(|(entry, _)| {
        vault
            .choices_for(CredentialKind::Password, Some(&entry.login.endpoint))
            .iter()
            .any(|choice| choice.id == credential.id)
    });
    if !allowed {
        return Err("The stored credential does not cover every selected SSH login. Choose a credential with the right scope or narrow the source folders.".into());
    }
    Ok(())
}
