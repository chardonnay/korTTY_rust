//! Native Connections export, with a reviewed destination and optional protection.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, container, pick_list, radio, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Element, Length, Size, Task, window};
use kortty_vault::Secret;

use crate::connection_export::{
    Entry, Format, Options, encode_with_options,
    protection::{self, Protection},
};
use crate::connection_store::Store;
use crate::profile;
use crate::snippets::recipients::Choice;
use crate::workspace::dialog_placement::Kind;
use crate::workspace::journal_access::export_dialog::browser::{
    Browser, Event as FileEvent, ReviewedSelection,
};

use super::{Message, Workspace, WorkspaceMessage as Desktop, wrap};

const TITLE: &str = "Export Connections";
const MAX_PASSWORD_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encryption {
    #[default]
    None,
    Password,
    OpenPgp,
}

#[derive(Clone)]
pub struct PasswordInput(Secret);

impl std::fmt::Debug for PasswordInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PasswordInput([redacted])")
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Focus,
    Close,
    Format(Format),
    Select(u64, bool),
    Folder(String, bool),
    All(bool),
    ByTags(bool),
    Tag(String, bool),
    IncludeUsername(bool),
    IncludeTunnels(bool),
    IncludeJumpServer(bool),
    Encryption(Encryption),
    Password(PasswordInput),
    ConfirmPassword(PasswordInput),
    Recipient(Choice),
    Choose,
    File(u64, FileEvent),
    Reviewed(u64, Result<Arc<Review>, String>),
    Replace(u64, bool),
    Written(u64, Result<Arc<Receipt>, String>),
}

#[derive(Debug)]
pub struct Review(ReviewedSelection);

#[derive(Debug)]
pub struct Receipt {
    path: PathBuf,
    count: usize,
    warning: Option<String>,
}

pub(super) struct Dialog {
    id: window::Id,
    owner: window::Id,
    store: Arc<Store>,
    selected: BTreeSet<u64>,
    manager: Option<ManagerSelection>,
    options: Options,
    format: Format,
    encryption: Encryption,
    password: Secret,
    confirm: Secret,
    recipients: Vec<Choice>,
    recipient: Option<Choice>,
    destination: Option<Destination>,
    receipt: Option<Arc<Receipt>>,
    attempt: u64,
}

struct ManagerSelection {
    tags: Vec<String>,
    by_tags: bool,
    selected_tags: BTreeSet<String>,
}

impl ManagerSelection {
    fn new(store: &Store) -> Self {
        let mut tags = store
            .connections()
            .iter()
            .map(|entry| entry.tag.as_str())
            .filter(|tag| !tag.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        tags.sort_by_cached_key(|tag| tag.to_lowercase());
        Self {
            tags,
            by_tags: false,
            selected_tags: BTreeSet::new(),
        }
    }
}

struct Destination {
    browser: Browser,
    phase: Phase,
}

enum Phase {
    Choose,
    Review,
    Confirm(Arc<Review>),
    Write,
}

impl Dialog {
    fn selection(&self) -> Cow<'_, BTreeSet<u64>> {
        match &self.manager {
            Some(manager) if manager.by_tags => Cow::Owned(
                self.store
                    .connections()
                    .iter()
                    .filter(|entry| manager.selected_tags.contains(&entry.tag))
                    .map(|entry| entry.id)
                    .collect(),
            ),
            _ => Cow::Borrowed(&self.selected),
        }
    }

    fn can_choose(&self) -> bool {
        !self.selection().is_empty()
            && match self.encryption {
                Encryption::None => true,
                Encryption::Password => !self.password.is_empty() && self.password == self.confirm,
                Encryption::OpenPgp => self.recipient.is_some(),
            }
    }

    fn busy(&self) -> bool {
        self.destination
            .as_ref()
            .is_some_and(|destination| matches!(destination.phase, Phase::Review | Phase::Write))
    }

    fn clear_password(&mut self) {
        self.password = Secret::default();
        self.confirm = Secret::default();
    }

    fn view(&self) -> Element<'_, Event> {
        if let Some(receipt) = &self.receipt {
            let noun = if receipt.count == 1 {
                "connection"
            } else {
                "connections"
            };
            return column![
                text(TITLE).size(20),
                text(format!(
                    "{} {noun} exported to {}",
                    receipt.count,
                    receipt.path.display()
                )),
                receipt
                    .warning
                    .as_deref()
                    .map(|warning| text(warning).size(13)),
                space::vertical(),
                row![space::horizontal(), button("Close").on_press(Event::Close)],
            ]
            .padding(16)
            .spacing(12)
            .height(Length::Fill)
            .into();
        }
        if let Some(destination) = &self.destination {
            let attempt = self.attempt;
            return match &destination.phase {
                Phase::Choose => destination
                    .browser
                    .view_with_title(TITLE)
                    .map(move |action| Event::File(attempt, action)),
                Phase::Review => container(text("Checking destination…")).padding(16).into(),
                Phase::Write => container(text("Writing connections…")).padding(16).into(),
                Phase::Confirm(review) => column![
                    text("Replace existing file?").size(20),
                    text(review.0.path().display().to_string()),
                    space::vertical(),
                    row![
                        space::horizontal(),
                        button("Cancel").on_press(Event::Replace(attempt, false)),
                        button("Replace")
                            .style(button::danger)
                            .on_press(Event::Replace(attempt, true)),
                    ]
                    .spacing(8),
                ]
                .padding(16)
                .spacing(12)
                .height(Length::Fill)
                .into(),
            };
        }
        self.options()
    }

    fn tree(&self) -> Element<'_, Event> {
        let mut groups = BTreeMap::<Option<&str>, Vec<_>>::new();
        for connection in self.store.connections() {
            groups
                .entry(self.store.group_of(connection.id))
                .or_default()
                .push(connection);
        }
        let mut tree = column![].spacing(5);
        for (folder, connections) in groups {
            let label = folder.unwrap_or("No folder");
            if let Some(folder) = folder {
                let all = connections
                    .iter()
                    .all(|entry| self.selected.contains(&entry.id));
                tree = tree.push(
                    checkbox(all)
                        .label(label)
                        .on_toggle(move |selected| Event::Folder(folder.to_owned(), selected)),
                );
            } else {
                tree = tree.push(text(label).size(14).style(text::secondary));
            }
            for entry in connections {
                tree = tree.push(
                    container(
                        checkbox(self.selected.contains(&entry.id))
                            .label(format!("{} ({})", entry.name, entry.login.endpoint.label()))
                            .on_toggle(move |selected| Event::Select(entry.id, selected)),
                    )
                    .padding([0, 20]),
                );
            }
        }
        column![
            text(format!(
                "{} of {} connections selected",
                self.selected.len(),
                self.store.connections().len()
            )),
            container(scrollable(tree).height(180))
                .padding(8)
                .width(Length::Fill)
                .style(container::bordered_box),
            row![
                button("Select all").on_press(Event::All(true)),
                button("Deselect all").on_press(Event::All(false))
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into()
    }

    fn manager_options<'a>(&'a self, manager: &'a ManagerSelection) -> Element<'a, Event> {
        let mut view = column![text(format!(
            "{} connections to export",
            self.selection().len()
        ))]
        .spacing(8);
        if !manager.tags.is_empty() {
            view = view.push(radio(
                format!("Selected connections ({})", self.selected.len()),
                false,
                Some(manager.by_tags),
                Event::ByTags,
            ));
            view = view.push(radio(
                "Connections with selected tags",
                true,
                Some(manager.by_tags),
                Event::ByTags,
            ));
            if manager.by_tags {
                let tags = column(manager.tags.iter().map(|tag| {
                    checkbox(manager.selected_tags.contains(tag))
                        .label(tag.as_str())
                        .on_toggle(move |selected| Event::Tag(tag.to_owned(), selected))
                        .into()
                }))
                .spacing(5);
                view = view.push(scrollable(tags).height(115));
            }
        }
        view = view.push(rule::horizontal(1));
        view = view.push(text("Profile fields").size(16));
        view = view.push(
            checkbox(self.options.include_username)
                .label("Include usernames")
                .on_toggle(Event::IncludeUsername),
        );
        if !self.options.include_username {
            view = view.push(text("A username must be added to each profile before it can be imported into kortty.").size(13));
        }
        view = view.push(
            checkbox(self.options.include_tunnels)
                .label("Include tunnels")
                .on_toggle(Event::IncludeTunnels),
        );
        view.push(
            checkbox(self.options.include_jump_server)
                .label("Include jump servers")
                .on_toggle(Event::IncludeJumpServer),
        )
        .into()
    }

    fn encryption_options(&self) -> Element<'_, Event> {
        let selected = self.encryption;
        let mut encryption = column![
            text("Encryption").size(16),
            radio(
                "No encryption",
                Encryption::None,
                Some(selected),
                Event::Encryption
            ),
            radio(
                "Password-protected ZIP",
                Encryption::Password,
                Some(selected),
                Event::Encryption
            ),
        ]
        .spacing(8);
        if self.recipients.is_empty() {
            encryption = encryption.push(
                text("OpenPGP requires a verified recipient.")
                    .size(13)
                    .style(text::secondary),
            );
        } else {
            encryption = encryption.push(radio(
                "OpenPGP",
                Encryption::OpenPgp,
                Some(selected),
                Event::Encryption,
            ));
        }
        match selected {
            Encryption::None => {}
            Encryption::Password => {
                encryption = encryption.push(
                    text_input("Archive password", &self.password)
                        .secure(true)
                        .on_input(|input| Event::Password(PasswordInput(Secret::new(input)))),
                );
                encryption = encryption.push(
                    text_input("Confirm password", &self.confirm)
                        .secure(true)
                        .on_input(|input| {
                            Event::ConfirmPassword(PasswordInput(Secret::new(input)))
                        }),
                );
                if !self.confirm.is_empty() && self.confirm != self.password {
                    encryption = encryption.push(text("The passwords do not match.").size(13));
                }
            }
            Encryption::OpenPgp => {
                encryption = encryption.push(
                    pick_list(
                        self.recipients.as_slice(),
                        self.recipient.as_ref(),
                        Event::Recipient,
                    )
                    .placeholder("Select a verified recipient"),
                );
            }
        }
        encryption.into()
    }

    fn options(&self) -> Element<'_, Event> {
        let body = column![
            text(TITLE).size(20),
            row![
                text("Format"),
                pick_list(Format::ALL, Some(self.format), Event::Format)
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            rule::horizontal(1),
            self.manager
                .as_ref()
                .map_or_else(|| self.tree(), |manager| self.manager_options(manager)),
            rule::horizontal(1),
            self.manager.is_none().then(|| self.encryption_options()),
            text("Passwords, stored credentials and private keys are excluded from the export.")
                .size(13)
                .style(text::secondary),
        ]
        .padding(16)
        .spacing(10);
        column![
            scrollable(body).height(Length::Fill),
            rule::horizontal(1),
            row![
                space::horizontal(),
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Event::Close),
                button("Export…").on_press_maybe(self.can_choose().then_some(Event::Choose)),
            ]
            .spacing(8)
            .padding([8, 16]),
        ]
        .height(Length::Fill)
        .into()
    }

    fn choose(&mut self) -> Task<Desktop> {
        let extension = self.format.extension();
        let (filename, suffix) = match self.encryption {
            Encryption::None => (
                format!("connections.{extension}"),
                match self.format {
                    Format::Kortty | Format::Mtputty => ".xml",
                    Format::Mobaxterm => ".mxtsessions",
                    Format::PuttyCm => ".csv",
                },
            ),
            Encryption::Password => ("connections.zip".into(), ".zip"),
            Encryption::OpenPgp => (format!("connections.{extension}.gpg"), ".gpg"),
        };
        self.attempt = self.attempt.wrapping_add(1);
        let (id, attempt) = (self.id, self.attempt);
        let mut browser = Browser::new(filename, suffix);
        let task = browser.update_event(FileEvent::List);
        self.destination = Some(Destination {
            browser,
            phase: Phase::Choose,
        });
        task.map(move |action| event(id, Event::File(attempt, action)))
    }

    fn publish(&mut self, review: Arc<Review>) -> Task<Desktop> {
        let selected = self.selection().into_owned();
        let Some(destination) = &mut self.destination else {
            return Task::none();
        };
        destination.phase = Phase::Write;
        let store = Arc::clone(&self.store);
        let format = self.format;
        let options = self.options;
        let encryption = self.encryption;
        let password = self.password.clone();
        let recipient = self.recipient.clone();
        let attempt = self.attempt;
        work(
            self.id,
            move || {
                let entries = store
                    .connections()
                    .iter()
                    .filter(|entry| selected.contains(&entry.id))
                    .map(|connection| Entry {
                        connection,
                        group: store.group_of(connection.id).unwrap_or_default(),
                        os_badge: store.os_badge_id(connection.id),
                    })
                    .collect::<Vec<_>>();
                let payload = encode_with_options(format, &entries, options)?;
                let output = match encryption {
                    Encryption::None => {
                        protection::protect(payload, format.extension(), Protection::None)?
                    }
                    Encryption::Password => protection::protect(
                        payload,
                        format.extension(),
                        Protection::Password(&password),
                    )?,
                    Encryption::OpenPgp => {
                        let root = profile::root()?;
                        let recipient = recipient.as_ref().ok_or_else(|| {
                            std::io::Error::other("Choose a verified OpenPGP recipient.")
                        })?;
                        protection::protect(
                            payload,
                            format.extension(),
                            Protection::OpenPgp {
                                profile: &root,
                                recipient,
                            },
                        )?
                    }
                };
                let warning = review.0.publish_bytes(&output)?;
                Ok(Arc::new(Receipt {
                    path: review.0.path().to_owned(),
                    count: entries.len(),
                    warning,
                }))
            },
            move |result| Event::Written(attempt, result),
        )
    }

    fn update(&mut self, action: Event) -> Task<Desktop> {
        let (id, attempt) = (self.id, self.attempt);
        if let Some(destination) = &mut self.destination {
            match action {
                Event::File(current, action)
                    if current == attempt && matches!(destination.phase, Phase::Choose) =>
                {
                    match action {
                        FileEvent::Cancel => self.destination = None,
                        FileEvent::Save => match destination.browser.selection() {
                            Ok(selection) => {
                                destination.phase = Phase::Review;
                                return work(
                                    id,
                                    move || {
                                        selection.review().map(|review| Arc::new(Review(review)))
                                    },
                                    move |result| Event::Reviewed(attempt, result),
                                );
                            }
                            Err(problem) => destination.browser.set_problem(problem.to_string()),
                        },
                        action => {
                            return destination
                                .browser
                                .update_event(action)
                                .map(move |action| event(id, Event::File(attempt, action)));
                        }
                    }
                }
                Event::Reviewed(current, result)
                    if current == attempt && matches!(destination.phase, Phase::Review) =>
                {
                    match result {
                        Ok(review) if review.0.exists() => {
                            destination.phase = Phase::Confirm(review)
                        }
                        Ok(review) => return self.publish(review),
                        Err(problem) => {
                            destination.browser.set_problem(problem);
                            destination.phase = Phase::Choose;
                        }
                    }
                }
                Event::Replace(current, replace) if current == attempt => {
                    if let Phase::Confirm(review) = &destination.phase {
                        if replace {
                            let review = Arc::clone(review);
                            return self.publish(review);
                        }
                        destination.phase = Phase::Choose;
                    }
                }
                Event::Written(current, result)
                    if current == attempt && matches!(destination.phase, Phase::Write) =>
                {
                    match result {
                        Ok(receipt) => {
                            self.clear_password();
                            self.destination = None;
                            self.receipt = Some(receipt);
                        }
                        Err(problem) => {
                            destination.browser.set_problem(problem);
                            destination.phase = Phase::Choose;
                        }
                    }
                }
                _ => {}
            }
            return Task::none();
        }
        if self.receipt.is_some() {
            return Task::none();
        }
        match action {
            Event::Format(format) => self.format = format,
            Event::Select(id, selected)
                if self.manager.is_none() && self.store.get(id).is_some() =>
            {
                if selected {
                    self.selected.insert(id);
                } else {
                    self.selected.remove(&id);
                }
            }
            Event::All(selected) if self.manager.is_none() => {
                self.selected.clear();
                if selected {
                    self.selected
                        .extend(self.store.connections().iter().map(|entry| entry.id));
                }
            }
            Event::Folder(folder, selected) if self.manager.is_none() => {
                let members = self
                    .store
                    .connections()
                    .iter()
                    .filter(|entry| self.store.group_of(entry.id) == Some(folder.as_str()))
                    .map(|entry| entry.id);
                for id in members {
                    if selected {
                        self.selected.insert(id);
                    } else {
                        self.selected.remove(&id);
                    }
                }
            }
            Event::ByTags(by_tags) => {
                if let Some(manager) = self
                    .manager
                    .as_mut()
                    .filter(|manager| !manager.tags.is_empty())
                {
                    manager.by_tags = by_tags;
                }
            }
            Event::Tag(tag, selected) => {
                if let Some(manager) = self
                    .manager
                    .as_mut()
                    .filter(|manager| manager.by_tags && manager.tags.contains(&tag))
                {
                    if selected {
                        manager.selected_tags.insert(tag);
                    } else {
                        manager.selected_tags.remove(&tag);
                    }
                }
            }
            Event::IncludeUsername(value) if self.manager.is_some() => {
                self.options.include_username = value
            }
            Event::IncludeTunnels(value) if self.manager.is_some() => {
                self.options.include_tunnels = value
            }
            Event::IncludeJumpServer(value) if self.manager.is_some() => {
                self.options.include_jump_server = value
            }
            Event::Encryption(encryption)
                if self.manager.is_none()
                    && (encryption != Encryption::OpenPgp || !self.recipients.is_empty()) =>
            {
                if encryption != Encryption::Password {
                    self.clear_password();
                }
                self.encryption = encryption;
            }
            Event::Password(input)
                if input.0.len() <= MAX_PASSWORD_BYTES
                    && self.encryption == Encryption::Password =>
            {
                self.password = input.0
            }
            Event::ConfirmPassword(input)
                if input.0.len() <= MAX_PASSWORD_BYTES
                    && self.encryption == Encryption::Password =>
            {
                self.confirm = input.0
            }
            Event::Recipient(choice) if self.recipients.contains(&choice) => {
                self.recipient = Some(choice)
            }
            Event::Choose if self.can_choose() => return self.choose(),
            _ => {}
        }
        Task::none()
    }
}

impl Workspace {
    pub(super) fn show_connection_export(&mut self, owner: window::Id) -> Task<Desktop> {
        if !self.windows.contains_key(&owner) || !self.connection_export_allowed() {
            return Task::none();
        }
        if let Some(dialog) = &self.chrome.connection_export {
            return window::gain_focus(dialog.id);
        }
        match profile::connections_path().and_then(Store::open) {
            Ok(store) if store.connections().is_empty() => {
                self.notice(owner, "No saved connections to export.".into());
                Task::none()
            }
            Ok(store) => {
                let selected = store.connections().iter().map(|entry| entry.id).collect();
                self.open_connection_export(owner, store, selected, None)
            }
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not read saved connections ({error})."),
                );
                Task::none()
            }
        }
    }

    pub(in crate::workspace) fn show_connection_manager_export(
        &mut self,
        owner: window::Id,
    ) -> Task<Desktop> {
        if self.connections_id != Some(owner) || !self.connection_export_allowed() {
            return Task::none();
        }
        if let Some(dialog) = &self.chrome.connection_export {
            return window::gain_focus(dialog.id);
        }
        let Some((store, selected)) = self
            .connections
            .as_ref()
            .and_then(|manager| manager.export_connections_sources())
        else {
            return Task::none();
        };
        let manager = ManagerSelection::new(store);
        self.open_connection_export(
            owner,
            store.clone(),
            selected.into_iter().collect(),
            Some(manager),
        )
    }

    fn open_connection_export(
        &mut self,
        owner: window::Id,
        store: Store,
        selected: BTreeSet<u64>,
        manager: Option<ManagerSelection>,
    ) -> Task<Desktop> {
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_paste();
        }
        let recipients = self.backup_recipient_choices();
        let recipient = recipients.first().cloned();
        let (id, opened) = self.open_placed_dialog(
            Kind::ConnectionExport,
            window::Settings {
                size: Size::new(660.0, 710.0),
                min_size: Some(Size::new(460.0, 365.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.chrome.connection_export = Some(Dialog {
            id,
            owner,
            store: Arc::new(store),
            selected,
            manager,
            options: Options::default(),
            format: Format::Kortty,
            encryption: Encryption::None,
            password: Secret::default(),
            confirm: Secret::default(),
            recipients,
            recipient,
            destination: None,
            receipt: None,
            attempt: 0,
        });
        opened.discard()
    }

    fn connection_export_allowed(&self) -> bool {
        !self.exiting && !self.scheduler_draining && self.clipboard.policy.problem.is_none()
    }

    pub(in crate::workspace) fn connection_export_open(&self) -> bool {
        self.chrome.connection_export.is_some()
    }

    pub(in crate::workspace) fn connection_export_for_owner(
        &self,
        owner: window::Id,
    ) -> Option<window::Id> {
        self.chrome
            .connection_export
            .as_ref()
            .filter(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(in crate::workspace) fn connection_export_title(
        &self,
        id: window::Id,
    ) -> Option<&'static str> {
        self.chrome
            .connection_export
            .as_ref()
            .filter(|dialog| dialog.id == id)
            .map(|_| TITLE)
    }

    pub(in crate::workspace) fn connection_export_saving(&self) -> Option<window::Id> {
        self.chrome
            .connection_export
            .as_ref()
            .filter(|dialog| dialog.busy())
            .map(|dialog| dialog.id)
    }

    pub(in crate::workspace) fn connection_export_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let dialog = self
            .chrome
            .connection_export
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        Some(dialog.view().map(move |action| event(id, action)))
    }

    pub(in crate::workspace) fn close_connection_export(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        if let Some(dialog) = self.connection_export_for_owner(id) {
            return Some(window::gain_focus(dialog));
        }
        self.connection_export_title(id)?;
        if self.connection_export_saving() == Some(id) {
            return Some(Task::none());
        }
        let dialog = self.chrome.connection_export.take()?;
        Some(
            self.close_placed_dialog(id)
                .chain(window::gain_focus(dialog.owner)),
        )
    }

    pub(super) fn update_connection_export(
        &mut self,
        id: window::Id,
        action: Event,
    ) -> Task<Desktop> {
        if self.connection_export_title(id).is_none() {
            return Task::none();
        }
        match action {
            Event::Focus => window::gain_focus(id),
            Event::Close => self.close_connection_export(id).unwrap_or_else(Task::none),
            action => self
                .chrome
                .connection_export
                .as_mut()
                .map_or_else(Task::none, |dialog| dialog.update(action)),
        }
    }
}

pub(in crate::workspace) fn focus(id: window::Id) -> Desktop {
    event(id, Event::Focus)
}

fn event(id: window::Id, action: Event) -> Desktop {
    wrap(Message::ConnectionExport(id, action))
}

fn work<T: Send + 'static>(
    id: window::Id,
    run: impl FnOnce() -> std::io::Result<T> + Send + 'static,
    done: impl FnOnce(Result<T, String>) -> Event + Send + 'static,
) -> Task<Desktop> {
    Task::future(async move {
        let (send, receive) = async_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("kortty-connection-export".into())
            .spawn(move || {
                let _ = send.try_send(run().map_err(|problem| problem.to_string()));
            });
        let result = match worker {
            Ok(_) => receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The export worker stopped.".into())),
            Err(problem) => Err(problem.to_string()),
        };
        event(id, done(result))
    })
}
