//! Reviewed exports from the native Connection Manager.

use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, container, radio, row, rule, scrollable, space, text, text_input,
};
use iced::{Element, Font, Length, Size, Task, window};

use crate::connection_window::{
    network_map::{layout::Topology, pdf},
    overview::{Format, Part, Snapshot},
};
use crate::session_journal::export_branding::PdfBranding;
use crate::vault_window::Kind;

use super::super::dialog_placement;
use super::super::journal_access::export_dialog::browser::{
    Browser, Event as FileEvent, ReviewedSelection,
};
use super::{Message, Workspace};

#[cfg(test)]
mod tests;

const OVERVIEW_TITLE: &str = "Export server overview";
const MAP_TITLE: &str = "Export connection map PDF";
const MAX_MAP_TITLE_CHARS: usize = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Selected,
    Shown,
    All,
}

impl Scope {
    const ALL: [Self; 3] = [Self::Selected, Self::Shown, Self::All];

    fn label(self) -> &'static str {
        match self {
            Self::Selected => "Selected connections",
            Self::Shown => "What the manager shows now",
            Self::All => "All connections",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Focus,
    Scope(Scope),
    Format(Format),
    Part(Part, bool),
    AllParts(bool),
    MapTitle(String),
    Landscape(bool),
    ChooseDestination,
    File(u64, FileEvent),
    Reviewed(u64, Result<Arc<Review>, String>),
    Replace(u64, bool),
    Published(u64, Result<Arc<Receipt>, String>),
    Dismiss,
    Close,
}

#[derive(Debug)]
pub struct Review(ReviewedSelection);

#[derive(Debug)]
pub struct Receipt {
    path: PathBuf,
    count: usize,
    warning: Option<String>,
}

pub(in crate::workspace) struct Dialog {
    id: window::Id,
    owner: window::Id,
    content: Content,
    destination: Option<Destination>,
    receipt: Option<Arc<Receipt>>,
    attempt: u64,
}

enum Content {
    Servers(Servers),
    Map(MapPdf),
}

struct Servers {
    snapshot: Arc<Snapshot>,
    sources: [Vec<u64>; 3],
    scope: Scope,
    format: Format,
    parts: std::collections::BTreeSet<Part>,
    preview: String,
}

struct MapPdf {
    snapshot: Arc<Topology>,
    title: String,
    landscape: bool,
    branding: PdfBranding,
}

struct Destination {
    browser: Browser,
    state: SaveState,
}

#[derive(Default)]
enum SaveState {
    #[default]
    Choosing,
    Reviewing,
    Confirm(Arc<Review>),
    Saving,
}

impl Servers {
    fn selected(&self) -> &[u64] {
        &self.sources[self.scope as usize]
    }

    fn refresh_preview(&mut self) {
        let ids = self.selected();
        self.preview = self
            .snapshot
            .render(&ids[..ids.len().min(1)], &self.parts, self.format)
            .unwrap_or_else(|error| format!("Could not render the preview ({error})."));
    }
}

impl Dialog {
    fn title(&self) -> &'static str {
        match self.content {
            Content::Servers(_) => OVERVIEW_TITLE,
            Content::Map(_) => MAP_TITLE,
        }
    }

    fn count(&self) -> usize {
        match &self.content {
            Content::Servers(servers) => servers.selected().len(),
            Content::Map(map) => map.snapshot.connection_count,
        }
    }

    fn busy(&self) -> bool {
        self.destination
            .as_ref()
            .is_some_and(|target| matches!(target.state, SaveState::Reviewing | SaveState::Saving))
    }

    fn view(&self) -> Element<'_, Event> {
        if let Some(receipt) = &self.receipt {
            return column![
                text(match self.content {
                    Content::Servers(_) => "Server overview exported",
                    Content::Map(_) => "Connection map PDF exported",
                })
                .size(20),
                text(format!(
                    "{} {} exported to {}",
                    receipt.count,
                    if receipt.count == 1 {
                        "connection"
                    } else {
                        "connections"
                    },
                    receipt.path.display()
                )),
                receipt
                    .warning
                    .as_deref()
                    .map(|warning| text(warning).size(13)),
                space::vertical(),
                row![space::horizontal(), button("Close").on_press(Event::Close)],
            ]
            .spacing(12)
            .padding(16)
            .height(Length::Fill)
            .into();
        }
        if let Some(destination) = &self.destination {
            let attempt = self.attempt;
            return match &destination.state {
                SaveState::Choosing => destination
                    .browser
                    .view_with_title(self.title())
                    .map(move |event| Event::File(attempt, event)),
                SaveState::Reviewing => container(text("Checking destination…")).padding(16).into(),
                SaveState::Saving => container(text("Writing export…")).padding(16).into(),
                SaveState::Confirm(review) => column![
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
                .spacing(12)
                .padding(16)
                .height(Length::Fill)
                .into(),
            };
        }
        self.options()
    }

    fn options(&self) -> Element<'_, Event> {
        let content = match &self.content {
            Content::Servers(servers) => Self::server_options(servers),
            Content::Map(map) => column![
                text(MAP_TITLE).size(20),
                text(format!(
                    "{} · {} in this view",
                    map.snapshot.scope, map.snapshot.connection_count
                ))
                .size(14),
                rule::horizontal(1),
                text("Page orientation").size(14),
                row![
                    radio("A4 landscape", true, Some(map.landscape), Event::Landscape),
                    radio("A4 portrait", false, Some(map.landscape), Event::Landscape),
                ]
                .spacing(16),
                text("Title").size(14),
                text_input(pdf::DEFAULT_TITLE, &map.title).on_input(Event::MapTitle),
            ]
            .spacing(10)
            .padding(16)
            .into(),
        };
        column![
            scrollable(content).height(Length::Fill),
            rule::horizontal(1),
            row![
                space::horizontal(),
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Event::Close),
                button("Export…")
                    .on_press_maybe((self.count() != 0).then_some(Event::ChooseDestination)),
            ]
            .spacing(8)
            .padding([8, 16]),
        ]
        .height(Length::Fill)
        .into()
    }

    fn server_options(servers: &Servers) -> Element<'_, Event> {
        let mut scopes = column![].spacing(5);
        for scope in Scope::ALL {
            let count = servers.sources[scope as usize].len();
            let caption = format!("{} ({count})", scope.label());
            if count == 0 {
                scopes = scopes.push(text(caption).size(14).style(text::secondary));
            } else {
                scopes =
                    scopes.push(radio(caption, scope, Some(servers.scope), Event::Scope).size(15));
            }
        }
        let formats = row(Format::ALL.into_iter().map(|format| {
            button(text(format.to_string()))
                .style(if servers.format == format {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Event::Format(format))
                .into()
        }))
        .spacing(5);
        let mut parts =
            row![checkbox(true).label("Name, host, port, user, protocol (always)")].spacing(10);
        for part in Part::ALL {
            parts = parts.push(
                checkbox(servers.parts.contains(&part))
                    .label(part.label())
                    .on_toggle(move |enabled| Event::Part(part, enabled)),
            );
        }
        column![
            text(OVERVIEW_TITLE).size(20),
            text("An inventory of your servers as JSON, YAML, TOML or XML. For documentation and scripts.").size(14),
            rule::horizontal(1),
            text("Connections").size(14),
            scopes,
            text("Format").size(14),
            formats,
            text("Parts").size(14),
            parts.wrap(),
            row![button("All").on_press(Event::AllParts(true)), button("None").on_press(Event::AllParts(false))].spacing(8),
            text("Passwords, passphrases and keys are never exported.").size(13).style(text::secondary),
            text("Preview (first connection)").size(14),
            container(text(&servers.preview).font(Font::MONOSPACE).size(13)).padding(8).width(Length::Fill).style(container::bordered_box),
        ].spacing(8).padding(16).into()
    }

    fn update(&mut self, event: Event) -> Task<Message> {
        let id = self.id;
        let attempt = self.attempt;
        if let Some(destination) = &mut self.destination {
            match event {
                Event::Dismiss if matches!(destination.state, SaveState::Confirm(_)) => {
                    destination.state = SaveState::Choosing;
                }
                Event::Dismiss if matches!(destination.state, SaveState::Choosing) => {
                    self.destination = None;
                }
                Event::File(current, FileEvent::Cancel)
                    if current == attempt && matches!(destination.state, SaveState::Choosing) =>
                {
                    self.destination = None;
                }
                Event::File(current, FileEvent::Save)
                    if current == attempt && matches!(destination.state, SaveState::Choosing) =>
                {
                    match destination.browser.selection() {
                        Ok(selection) => {
                            destination.state = SaveState::Reviewing;
                            return work(
                                id,
                                move || {
                                    selection
                                        .review()
                                        .map(|review| Arc::new(Review(review)))
                                        .map_err(|error| error.to_string())
                                },
                                move |result| Event::Reviewed(attempt, result),
                            );
                        }
                        Err(error) => destination.browser.set_problem(error.to_string()),
                    }
                }
                Event::File(current, event)
                    if current == attempt && matches!(destination.state, SaveState::Choosing) =>
                {
                    return destination
                        .browser
                        .update_event(event)
                        .map(move |event| wrap(id, Event::File(attempt, event)));
                }
                Event::Reviewed(current, result)
                    if current == attempt && matches!(destination.state, SaveState::Reviewing) =>
                {
                    match result {
                        Ok(review) if review.0.exists() => {
                            destination.state = SaveState::Confirm(review)
                        }
                        Ok(review) => return self.publish(review),
                        Err(problem) => {
                            destination.browser.set_problem(problem);
                            destination.state = SaveState::Choosing;
                        }
                    }
                }
                Event::Replace(current, replace) if current == attempt => {
                    if let SaveState::Confirm(review) = &destination.state {
                        if replace {
                            let review = Arc::clone(review);
                            return self.publish(review);
                        }
                        destination.state = SaveState::Choosing;
                    }
                }
                Event::Published(current, result)
                    if current == attempt && matches!(destination.state, SaveState::Saving) =>
                {
                    match result {
                        Ok(receipt) => {
                            self.destination = None;
                            self.receipt = Some(receipt);
                        }
                        Err(problem) => {
                            destination.browser.set_problem(problem);
                            destination.state = SaveState::Choosing;
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
        if matches!(event, Event::ChooseDestination) && self.count() != 0 {
            self.attempt = self.attempt.wrapping_add(1);
            let attempt = self.attempt;
            let (filename, suffix) = match &self.content {
                Content::Servers(servers) => {
                    let suffix = servers.format.suffix();
                    (format!("kortty-servers{suffix}"), suffix)
                }
                Content::Map(_) => ("kortty-map.pdf".to_owned(), ".pdf"),
            };
            let mut browser = Browser::new(filename, suffix);
            let list = browser.update_event(FileEvent::List);
            self.destination = Some(Destination {
                browser,
                state: SaveState::Choosing,
            });
            return list.map(move |event| wrap(id, Event::File(attempt, event)));
        }
        match (&mut self.content, event) {
            (Content::Servers(servers), event) => {
                match event {
                    Event::Scope(scope) if !servers.sources[scope as usize].is_empty() => {
                        servers.scope = scope
                    }
                    Event::Format(format) => servers.format = format,
                    Event::Part(part, true) => {
                        servers.parts.insert(part);
                    }
                    Event::Part(part, false) => {
                        servers.parts.remove(&part);
                    }
                    Event::AllParts(enabled) => {
                        servers.parts.clear();
                        if enabled {
                            servers.parts.extend(Part::ALL);
                        }
                    }
                    _ => return Task::none(),
                }
                servers.refresh_preview();
            }
            (Content::Map(map), Event::MapTitle(value)) => {
                map.title = value
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(MAX_MAP_TITLE_CHARS)
                    .collect();
            }
            (Content::Map(map), Event::Landscape(value)) => map.landscape = value,
            _ => {}
        }
        Task::none()
    }

    fn publish(&mut self, review: Arc<Review>) -> Task<Message> {
        let count = self.count();
        let Some(destination) = &mut self.destination else {
            return Task::none();
        };
        destination.state = SaveState::Saving;
        let render: Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send> = match &self.content {
            Content::Servers(servers) => {
                let snapshot = Arc::clone(&servers.snapshot);
                let ids = servers.selected().to_vec();
                let parts = servers.parts.clone();
                let format = servers.format;
                Box::new(move || {
                    snapshot
                        .render(&ids, &parts, format)
                        .map(String::into_bytes)
                        .map_err(|error| error.to_string())
                })
            }
            Content::Map(map) => {
                let snapshot = Arc::clone(&map.snapshot);
                let title = map.title.trim().to_owned();
                let landscape = map.landscape;
                let branding = map.branding.clone();
                Box::new(move || {
                    pdf::render(&snapshot, &title, landscape, &branding)
                        .map_err(|error| error.to_string())
                })
            }
        };
        let attempt = self.attempt;
        work(
            self.id,
            move || {
                let output = render()?;
                let warning = review
                    .0
                    .publish_bytes(&output)
                    .map_err(|error| error.to_string())?;
                Ok(Arc::new(Receipt {
                    path: review.0.path().to_owned(),
                    count,
                    warning,
                }))
            },
            move |result| Event::Published(attempt, result),
        )
    }
}

impl Workspace {
    pub(super) fn open_connection_overview(&mut self, owner: window::Id) -> Task<Message> {
        if let Some((id, focus)) = self.owner_dialog(owner) {
            return window::gain_focus(id).chain(Task::done(focus));
        }
        let Some(sources) = self
            .connection_managers
            .get(&owner)
            .and_then(|manager| manager.window.overview_sources())
        else {
            return Task::none();
        };
        let credentials =
            [Kind::Password, Kind::KeyPassphrase].map(|kind| self.credential_choices(kind));
        let snapshot = Snapshot::capture(
            sources.store,
            sources.custom_badges,
            |id| {
                credentials
                    .iter()
                    .flatten()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.label.clone())
            },
            |id| {
                self.ssh_key_choices
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.name.clone())
            },
        );
        let sources = [sources.selected, sources.shown, sources.all];
        let Some(scope) = Scope::ALL
            .into_iter()
            .find(|scope| !sources[*scope as usize].is_empty())
        else {
            return Task::none();
        };
        let mut servers = Servers {
            snapshot: Arc::new(snapshot),
            sources,
            scope,
            format: Format::default(),
            parts: Part::defaults(),
            preview: String::new(),
        };
        servers.refresh_preview();
        self.open_connection_document(owner, Content::Servers(servers))
    }

    pub(super) fn open_connection_map_pdf(&mut self, owner: window::Id) -> Task<Message> {
        if !self.connection_export_allowed() {
            if self.clipboard.policy.problem.is_some()
                && let Some(manager) = self.connection_managers.get_mut(&owner)
            {
                manager.window.set_notice(
                    "Connection exports are unavailable under the current policy.".into(),
                );
            }
            return Task::none();
        }
        if let Some((id, focus)) = self.owner_dialog(owner) {
            return window::gain_focus(id).chain(Task::done(focus));
        }
        let Some(snapshot) = self
            .connection_managers
            .get(&owner)
            .and_then(|manager| manager.window.map_pdf_snapshot())
        else {
            return Task::none();
        };
        let map = MapPdf {
            snapshot: Arc::new(snapshot),
            title: pdf::DEFAULT_TITLE.into(),
            landscape: true,
            branding: self.window_settings.export_branding.resolve(),
        };
        self.open_connection_document(owner, Content::Map(map))
    }

    fn open_connection_document(&mut self, owner: window::Id, content: Content) -> Task<Message> {
        let (kind, size, min_size) = match &content {
            Content::Servers(_) => (
                dialog_placement::Kind::ConnectionOverview,
                Size::new(780.0, 700.0),
                Size::new(460.0, 360.0),
            ),
            Content::Map(_) => (
                dialog_placement::Kind::ConnectionMapPdf,
                Size::new(630.0, 520.0),
                Size::new(460.0, 360.0),
            ),
        };
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            kind,
            window::Settings {
                size,
                min_size: Some(min_size),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(focus),
        );
        let dialog = Dialog {
            id,
            owner,
            content,
            destination: None,
            receipt: None,
            attempt: 0,
        };
        self.connection_overview.insert(id, dialog);
        opened
    }

    pub(in crate::workspace) fn connection_overview_for_owner(
        &self,
        owner: window::Id,
    ) -> Option<window::Id> {
        self.connection_overview
            .values()
            .find(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(super) fn connection_overview_busy(&self) -> Option<window::Id> {
        self.connection_overview
            .values()
            .find(|dialog| dialog.busy())
            .map(|dialog| dialog.id)
    }

    pub(in crate::workspace) fn connection_overview_title(
        &self,
        id: window::Id,
    ) -> Option<&'static str> {
        self.connection_overview.get(&id).map(Dialog::title)
    }

    pub(in crate::workspace) fn connection_overview_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Message>> {
        let dialog = self.connection_overview.get(&id)?;
        Some(dialog.view().map(move |event| wrap(id, event)))
    }

    pub(in crate::workspace) fn update_connection_overview(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        let Some(dialog) = self.connection_overview.get_mut(&id) else {
            return Task::none();
        };
        match event {
            Event::Focus => window::gain_focus(id),
            Event::Close => self.close_connection_overview(id),
            Event::Dismiss if dialog.destination.is_none() => self.close_connection_overview(id),
            event => dialog.update(event),
        }
    }

    pub(in crate::workspace) fn close_connection_overview(
        &mut self,
        id: window::Id,
    ) -> Task<Message> {
        if self.connection_overview.get(&id).is_some_and(Dialog::busy) {
            return Task::none();
        }
        let Some(dialog) = self.connection_overview.remove(&id) else {
            return Task::none();
        };
        self.close_placed_dialog_and_focus(id, dialog.owner)
    }
}

pub(in crate::workspace) fn focus(id: window::Id) -> Message {
    wrap(id, Event::Focus)
}

fn wrap(id: window::Id, event: Event) -> Message {
    Message::ConnectionOverview(id, event)
}

fn work<T: Send + 'static>(
    id: window::Id,
    run: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(Result<T, String>) -> Event + Send + 'static,
) -> Task<Message> {
    Task::future(async move {
        let (send, receive) = async_channel::bounded(1);
        let result = std::thread::Builder::new()
            .name("kortty-connection-export".into())
            .spawn(move || {
                let _ = send.try_send(run());
            });
        let result = match result {
            Ok(_) => receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The export worker stopped.".into())),
            Err(error) => Err(error.to_string()),
        };
        wrap(id, done(result))
    })
}
