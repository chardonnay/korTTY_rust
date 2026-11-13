use std::io;
use std::sync::Arc;

use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};
use iced::{Element, Length, Task};
use kortty_vault::{MAX_ARCHIVE_PASSWORD_BYTES, valid_archive_password};
use serde::{Deserialize, Serialize};

use crate::scheduler_window::{
    ArchiveFormat, MAX_ARCHIVE_EXCLUDE_TEXT, MAX_ARCHIVE_SOURCES, RemotePath,
};
use crate::ssh_dialog::Password;

use super::worker::archive::{Options, Plan, Request, local};
use super::worker::folder_usage::{Estimate, Route};
use super::worker::ownership::principals::{INPUT_LIMIT, Kind, Selection};
use super::{
    Directory, Entry, EntryName, Location, Message as WindowMessage, Panel, Prompt, SftpWindow,
    Side, worker,
};

mod estimate;

const SERVER_FORMATS: &[ArchiveFormat] = &[
    ArchiveFormat::Zip,
    ArchiveFormat::TarBz2,
    ArchiveFormat::SevenZip,
];
const SERVER_ONLY_FORMATS: &[ArchiveFormat] = &[
    ArchiveFormat::Zip,
    ArchiveFormat::Tar,
    ArchiveFormat::TarBz2,
    ArchiveFormat::SevenZip,
];
const LOCAL_FORMATS: &[ArchiveFormat] = &[
    ArchiveFormat::Zip,
    ArchiveFormat::ZipPassword,
    ArchiveFormat::TarBz2,
    ArchiveFormat::SevenZip,
    ArchiveFormat::SevenZipPassword,
];
const PAGE_SIZE: usize = 16;
const ARCHIVE_NAME: &str = "archive.zip";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) remote_directory: String,
    pub(crate) level: u8,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            remote_directory: String::new(),
            level: 6,
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn valid(&self) -> bool {
        ArchiveFormat::Zip.valid_level(self.level)
            && (self.remote_directory.is_empty()
                || crate::profile::RemoteProjectTool::valid_remote_path(
                    &self.remote_directory,
                    true,
                ) && RemotePath::try_from(self.remote_name("20000101_000000")).is_ok())
    }

    fn name(&self, side: Side, now: &jiff::Zoned) -> String {
        if side == Side::Remote && !self.remote_directory.is_empty() {
            self.remote_name(&now.strftime("%Y%m%d_%H%M%S").to_string())
        } else {
            ARCHIVE_NAME.into()
        }
    }

    fn remote_name(&self, timestamp: &str) -> String {
        let directory = self.remote_directory.trim_end_matches('/');
        format!("{directory}/archive_{timestamp}.zip")
    }
}

#[cfg(test)]
#[test]
fn archive_preferences_bind_remote_path_and_compression() {
    let mut preferences = Preferences {
        remote_directory: "/C:/Release Archives/".into(),
        level: 0,
    };
    assert!(preferences.valid());
    let now: jiff::Zoned = "2026-11-13T06:00:00+00:00[UTC]".parse().unwrap();
    assert_eq!(
        preferences.name(Side::Remote, &now),
        "/C:/Release Archives/archive_20261113_060000.zip"
    );
    assert_eq!(preferences.name(Side::Local, &now), ARCHIVE_NAME);
    for path in ["", "/"] {
        preferences.remote_directory = path.into();
        assert!(preferences.valid());
    }
    for path in ["relative", "/tmp/../root", "/tmp//new", "/tmp/\nnew"] {
        preferences.remote_directory = path.into();
        assert!(!preferences.valid());
    }
    preferences.remote_directory.clear();
    preferences.level = 10;
    assert!(!preferences.valid());
}

#[derive(Clone, Debug)]
pub enum Message {
    ServerOnly(bool),
    Name(String),
    Format(ArchiveFormat),
    Level(String),
    Excludes(String),
    Owner(String),
    Group(String),
    Permissions(String),
    Password(Password),
    ConfirmPassword(Password),
    Estimated(u64, Option<Estimate>),
    Review,
    Edit,
    Page(usize),
    Apply,
}

pub(super) struct Draft {
    side: Side,
    server_available: bool,
    client_available: bool,
    server_only: bool,
    listing: Arc<Directory>,
    selected: Vec<Entry>,
    name: String,
    format: ArchiveFormat,
    level: String,
    excludes: String,
    owner: String,
    group: String,
    permissions: String,
    password: Password,
    confirmation: Password,
    estimate: Option<estimate::Selection>,
    review: Option<Review>,
    page: usize,
    error: Option<String>,
}

pub(super) enum Review {
    Remote(Arc<Plan>),
    Local(Arc<local::Plan>),
}

struct ReviewSummary<'a> {
    destination: &'a str,
    format: ArchiveFormat,
    entries: &'a [(String, bool)],
    input_bytes: u64,
    excluded_count: usize,
}

impl Review {
    fn summary(&self) -> ReviewSummary<'_> {
        match self {
            Self::Remote(plan) => ReviewSummary {
                destination: plan.destination(),
                format: plan.format(),
                entries: plan.entries(),
                input_bytes: plan.input_bytes(),
                excluded_count: plan.excluded_count(),
            },
            Self::Local(plan) => ReviewSummary {
                destination: plan.destination(),
                format: plan.format(),
                entries: plan.entries(),
                input_bytes: plan.input_bytes(),
                excluded_count: plan.excluded_count(),
            },
        }
    }
}

impl Draft {
    fn still_selected(&self, panel: &Panel) -> bool {
        panel
            .directory
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.listing))
            && self.selected.len() == panel.selected.len()
            && self
                .selected
                .iter()
                .all(|entry| panel.selected.contains(&entry.name))
    }

    fn formats(&self) -> &'static [ArchiveFormat] {
        match self.side {
            Side::Local => LOCAL_FORMATS,
            Side::Remote if self.server_only => SERVER_ONLY_FORMATS,
            Side::Remote => SERVER_FORMATS,
        }
    }

    fn password_ready(&self) -> bool {
        !self.format.is_password_protected()
            || (valid_archive_password(self.password.as_str())
                && self.password.as_str() == self.confirmation.as_str())
    }

    fn level(&self) -> Option<u8> {
        let [digit] = self.level.as_bytes() else {
            return None;
        };
        let level = digit.checked_sub(b'0')?;
        self.format.valid_level(level).then_some(level)
    }

    fn metadata(&self) -> Option<Selection> {
        Selection::parse_with_mode(&self.owner, &self.group, &self.permissions)
    }

    fn ready(&self) -> bool {
        !self.name.is_empty()
            && self.level().is_some()
            && !self.excludes.contains('\0')
            && (self.side != Side::Remote || self.server_only || self.metadata().is_some())
            && self.password_ready()
    }

    fn options(&self) -> Option<Options> {
        if !self.ready() {
            return None;
        }
        Some(Options {
            name: self.name.clone(),
            format: self.format,
            level: self.level()?,
            excludes: self
                .excludes
                .split([';', '\n'])
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
                .map(str::to_owned)
                .collect(),
        })
    }

    pub(super) fn view(&self) -> Element<'_, WindowMessage> {
        if let Some(review) = &self.review {
            return self.review_view(review);
        }
        let roots = self
            .selected
            .iter()
            .fold(column![].spacing(4), |list, entry| {
                list.push(text(entry.display.escape_debug().to_string()))
            });
        let (title, directory, destination) = match &self.listing.path {
            Location::Remote(path) => (
                "Create server archive",
                path.as_slice(),
                "New archive name or absolute server path",
            ),
            Location::Local(path) => (
                "Create local archive",
                path.as_os_str().as_encoded_bytes(),
                "New archive name or absolute local path",
            ),
        };
        let level_hint = self.format.level_hint();
        let mut form = column![
            text(title).size(20),
            text(format!("Source folder · {}", directory.escape_ascii())),
            text(format!("Selected entries · {}", self.selected.len())),
            scrollable(roots).height(Length::Fixed(88.0)),
            text_input(destination, &self.name)
                .on_input(|name| WindowMessage::ServerArchive(Message::Name(name))),
            row![
                pick_list(self.formats(), Some(self.format), |format| {
                    WindowMessage::ServerArchive(Message::Format(format))
                }),
                text_input(level_hint, &self.level)
                    .on_input(|level| WindowMessage::ServerArchive(Message::Level(level))),
            ]
            .spacing(8)
            .wrap(),
            text("Separate optional exclusion patterns with semicolons."),
            text_input("Exclude patterns", &self.excludes)
                .on_input(|patterns| WindowMessage::ServerArchive(Message::Excludes(patterns))),
        ]
        .padding(16)
        .spacing(8);
        if self.level().is_none() {
            form = form.push(text(level_hint));
        }
        if self.side == Side::Remote {
            if self.server_available {
                form = form.push(
                    checkbox(self.server_only)
                        .label("Create entirely on the server")
                        .on_toggle_maybe(self.client_available.then_some(|enabled| {
                            WindowMessage::ServerArchive(Message::ServerOnly(enabled))
                        })),
                );
            }
            if let Some(estimate) = &self.estimate {
                form = form.push(text(estimate.label()));
            }
        }
        if self.side == Side::Remote && self.server_only {
            form = form.push(text("Requires the kortty server archive helper. The archive is private to the SSH account."));
        }
        if self.side == Side::Remote && !self.server_only {
            form = form
                .push(text(
                    "Owner and group are optional. Blank permissions use 600.",
                ))
                .push(
                    text_input("Owner name or UID", &self.owner)
                        .on_input(|owner| WindowMessage::ServerArchive(Message::Owner(owner))),
                )
                .push(
                    text_input("Group name or GID", &self.group)
                        .on_input(|group| WindowMessage::ServerArchive(Message::Group(group))),
                )
                .push(
                    text_input("Permissions (three octal digits)", &self.permissions)
                        .on_input(|mode| WindowMessage::ServerArchive(Message::Permissions(mode))),
                );
            if Selection::parse_mode(&self.permissions).is_none() {
                form = form.push(text("Enter exactly three digits from 0 through 7."));
            } else if self.metadata().is_none() {
                form = form.push(text(
                    "Enter a valid account name or a numeric ID from 0 through 4294967294.",
                ));
            }
        }
        if self.format.is_password_protected() {
            form = form
                .push(
                    text_input("Archive password", self.password.as_str())
                        .secure(true)
                        .on_input(|value| {
                            WindowMessage::ServerArchive(Message::Password(Password::new(value)))
                        }),
                )
                .push(
                    text_input("Confirm archive password", self.confirmation.as_str())
                        .secure(true)
                        .on_input(|value| {
                            WindowMessage::ServerArchive(Message::ConfirmPassword(Password::new(
                                value,
                            )))
                        }),
                );
            if !self.confirmation.as_str().is_empty() && !self.password_ready() {
                form = form.push(text("Enter matching nonblank archive passwords."));
            }
        }
        if let Some(error) = &self.error {
            form = form.push(text(error));
        }
        scrollable(
            form.push(
                row![
                    button("Cancel").on_press(WindowMessage::Dismiss),
                    button("Review archive").on_press_maybe(
                        self.ready()
                            .then_some(WindowMessage::ServerArchive(Message::Review)),
                    ),
                ]
                .spacing(8),
            ),
        )
        .into()
    }

    fn review_view(&self, review: &Review) -> Element<'_, WindowMessage> {
        let ReviewSummary {
            destination,
            format,
            entries,
            input_bytes,
            excluded_count,
        } = review.summary();
        let pages = entries.len().div_ceil(PAGE_SIZE).max(1);
        let page = self.page.min(pages - 1);
        let paths = entries.iter().skip(page * PAGE_SIZE).take(PAGE_SIZE).fold(
            column![].spacing(4),
            |list, (name, folder)| {
                list.push(text(format!(
                    "{} · {}",
                    if *folder { "Folder" } else { "File" },
                    name.as_bytes().escape_ascii()
                )))
            },
        );
        let mut form = column![
            text(match self.side { Side::Local => "Review local archive", Side::Remote => "Review server archive" }).size(20),
            text(format!("Destination · {destination}")),
            text(format!("{format} · Compression {} · {input_bytes} source bytes", self.level)),
            text(format!("{} entries to archive · {excluded_count} excluded paths", entries.len())),
            text(match self.side {
                Side::Local => "korTTY reads the reviewed local files and creates a new private archive.",
                Side::Remote if self.server_only => "The server helper reads the reviewed files and creates a private archive on the server.",
                Side::Remote => "korTTY reads the selected files over SFTP, builds the archive in memory and publishes it with the reviewed attributes.",
            }),
        ].padding(16).spacing(8);
        if let Review::Remote(plan) = review
            && !plan.server_only()
        {
            let (owner, group, mode) = reviewed_metadata(plan);
            form = form
                .push(text(format!(
                    "Owner (UID) · {owner}    Group (GID) · {group}"
                )))
                .push(text(format!("Permissions · {mode:03o}")));
        }
        match format {
            ArchiveFormat::ZipPassword => {
                form = form.push(text("ZIP keeps member names visible."));
            }
            ArchiveFormat::SevenZipPassword => {
                form = form.push(text("7z encrypts file contents and member names."));
            }
            _ => {}
        }
        form = form.push(scrollable(paths).height(Length::Fixed(220.0)));
        if pages > 1 {
            let page_message = |page| WindowMessage::ServerArchive(Message::Page(page));
            form = form.push(
                row![
                    button("Previous").on_press_maybe(page.checked_sub(1).map(page_message)),
                    text(format!("{} of {pages}", page + 1)),
                    button("Next")
                        .on_press_maybe((page + 1 < pages).then(|| page_message(page + 1))),
                ]
                .spacing(8),
            );
        }
        scrollable(
            form.push(
                row![
                    button("Cancel").on_press(WindowMessage::Dismiss),
                    button("Edit options").on_press(WindowMessage::ServerArchive(Message::Edit)),
                    button("Create archive").on_press(WindowMessage::ServerArchive(Message::Apply)),
                ]
                .spacing(8)
                .wrap(),
            ),
        )
        .into()
    }

    fn update(&mut self, message: Message) {
        if let Some(review) = &self.review {
            match message {
                Message::Page(page) => {
                    self.page =
                        page.min(review.summary().entries.len().saturating_sub(1) / PAGE_SIZE);
                }
                Message::Edit => self.review = None,
                _ => {}
            }
            return;
        }
        self.error = None;
        match message {
            Message::ServerOnly(enabled) if self.server_available && self.client_available => {
                self.server_only = enabled;
                if !self.formats().contains(&self.format) {
                    self.update(Message::Format(ArchiveFormat::Zip));
                }
            }
            Message::Name(name) => self.name = super::bounded(name),
            Message::Format(format) if self.formats().contains(&format) => {
                if let Some(stem) = self.name.strip_suffix(self.format.extension()) {
                    self.name = format!("{stem}{}", format.extension());
                }
                if self.format != format {
                    self.password = Password::default();
                    self.confirmation = Password::default();
                }
                if self.level == "0" && !format.valid_level(0) {
                    self.level = "1".into();
                }
                self.format = format;
            }
            Message::Password(value) if self.format.is_password_protected() => {
                self.password = self.bounded_password(value);
            }
            Message::ConfirmPassword(value) if self.format.is_password_protected() => {
                self.confirmation = self.bounded_password(value);
            }
            Message::Level(level) => {
                self.level = if level.len() <= 1 {
                    level
                } else {
                    String::new()
                }
            }
            Message::Excludes(excludes) => {
                if excludes.len() <= MAX_ARCHIVE_EXCLUDE_TEXT {
                    self.excludes = excludes;
                } else {
                    self.excludes = "\0".into();
                    self.error = Some("The exclusion patterns exceed the input limit.".into());
                }
            }
            Message::Owner(owner) if self.side == Side::Remote => {
                self.owner = bounded_account(owner);
            }
            Message::Group(group) if self.side == Side::Remote => {
                self.group = bounded_account(group);
            }
            Message::Permissions(mode) if self.side == Side::Remote => {
                self.permissions = if mode.len() <= INPUT_LIMIT {
                    mode
                } else {
                    "Invalid mode".into()
                };
            }
            _ => {}
        }
    }

    fn bounded_password(&mut self, value: Password) -> Password {
        if value.as_str().len() <= MAX_ARCHIVE_PASSWORD_BYTES {
            value
        } else {
            self.error = Some("The archive password exceeds the input limit.".into());
            Password::default()
        }
    }
}

fn bounded_account(input: String) -> String {
    if input.len() <= INPUT_LIMIT {
        input
    } else {
        "Invalid account input".into()
    }
}

fn reviewed_metadata(plan: &Plan) -> (String, String, u32) {
    let default_account = || "Server default".to_owned();
    let Some((selection, (uid, gid))) = plan.metadata() else {
        return (default_account(), default_account(), 0o600);
    };
    let label = |kind, id: Option<u32>| {
        id.map_or_else(default_account, |id| selection.label(kind, Some(id)))
    };
    (
        label(Kind::User, uid),
        label(Kind::Group, gid),
        selection.mode().unwrap_or(0o600),
    )
}

impl SftpWindow {
    pub(crate) fn configure_archive_preferences(&mut self, preferences: &Preferences) {
        self.archive_preferences = if preferences.valid() {
            preferences.clone()
        } else {
            Preferences::default()
        };
    }

    fn archive_modes(&self) -> (bool, bool) {
        self.worker.as_ref().map_or((false, false), |worker| {
            (
                worker.exec_opener().is_some(),
                self.transfer_allowed && worker.supports_archive(),
            )
        })
    }

    #[cfg(test)]
    pub(super) fn archive_estimate_label(&self) -> Option<String> {
        match &self.prompt {
            Some(Prompt::Archive(draft)) => draft.estimate.as_ref().map(estimate::Selection::label),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn archive_review_bytes(&self) -> Option<u64> {
        let Some(Prompt::Archive(draft)) = &self.prompt else {
            return None;
        };
        draft
            .review
            .as_ref()
            .map(|review| review.summary().input_bytes)
    }

    pub(super) fn can_begin_archive(&self, side: Side) -> bool {
        let panel = self.panel(side);
        let (server, client) = self.archive_modes();
        (side == Side::Local || server || client)
            && self.prompt.is_none()
            && !self.transport_busy()
            && panel.directory.is_some()
            && (1..=MAX_ARCHIVE_SOURCES).contains(&panel.selected.len())
            && panel
                .entries()
                .filter(|entry| panel.selected.contains(&entry.name))
                .all(|entry| {
                    entry.kind.transferable()
                        && match (side, &entry.name) {
                            (Side::Local, EntryName::Local(name)) => name.to_str().is_some(),
                            (Side::Remote, EntryName::Remote(name)) => {
                                std::str::from_utf8(name).is_ok()
                            }
                            _ => false,
                        }
                })
    }

    pub(super) fn begin_archive(&mut self, side: Side) -> Task<WindowMessage> {
        if !self.can_begin_archive(side) {
            return Task::none();
        }
        self.counter = self.counter.wrapping_add(1);
        let (server_available, client_available) = self.archive_modes();
        let panel = self.panel(side);
        let Some(listing) = &panel.directory else {
            return Task::none();
        };
        let selected = panel
            .entries()
            .filter(|entry| panel.selected.contains(&entry.name))
            .cloned()
            .collect::<Vec<_>>();
        let mut estimate = (side == Side::Remote).then(|| estimate::Selection::new(&selected));
        let task = estimate.as_mut().map_or_else(Task::none, |estimate| {
            estimate.measure(
                self.counter,
                listing,
                panel.allocation_batch.as_ref(),
                self.worker.as_ref().and_then(Route::new),
            )
        });
        self.prompt = Some(Prompt::Archive(Draft {
            side,
            server_available,
            client_available,
            server_only: side == Side::Remote && !client_available,
            listing: Arc::clone(listing),
            selected,
            name: self.archive_preferences.name(side, &jiff::Zoned::now()),
            format: ArchiveFormat::Zip,
            level: self.archive_preferences.level.to_string(),
            excludes: String::new(),
            owner: String::new(),
            group: String::new(),
            permissions: String::new(),
            password: Password::default(),
            confirmation: Password::default(),
            estimate,
            review: None,
            page: 0,
            error: None,
        }));
        task
    }

    pub(super) fn archive_message(&mut self, message: Message) -> Task<WindowMessage> {
        if let Message::Estimated(id, measured) = message {
            let prompt = match &mut self.prompt {
                Some(Prompt::Close(previous)) => previous.as_deref_mut(),
                prompt => prompt.as_mut(),
            };
            if let Some(Prompt::Archive(draft)) = prompt
                && draft.side == Side::Remote
                && draft.still_selected(&self.remote)
                && let Some(estimate) = &mut draft.estimate
            {
                estimate.finish(id, measured);
            }
            return Task::none();
        }
        if matches!(message, Message::Review | Message::Apply) {
            return self.submit_archive(matches!(message, Message::Apply));
        }
        if let Some(Prompt::Archive(draft)) = &mut self.prompt {
            draft.update(message);
        }
        Task::none()
    }

    fn submit_archive(&mut self, apply: bool) -> Task<WindowMessage> {
        if self.transport_busy() {
            return Task::none();
        }
        let Some(Prompt::Archive(draft)) = &self.prompt else {
            return Task::none();
        };
        if draft.side == Side::Remote && !draft.server_only && !self.transfer_allowed {
            return Task::none();
        }
        if !draft.still_selected(self.panel(draft.side)) {
            self.prompt = None;
            self.status = Some("The archive selection changed. Select the entries again.".into());
            return Task::none();
        }
        if apply {
            return if draft.review.is_some() {
                self.apply_archive()
            } else {
                Task::none()
            };
        }
        if draft.review.is_some() {
            return Task::none();
        }
        let Some(options) = draft.options() else {
            return Task::none();
        };
        let operation = self.archive_review_operation(draft, options);
        let Some(Prompt::Archive(mut draft)) = self.prompt.take() else {
            return Task::none();
        };
        let operation = match operation {
            Ok(operation) => operation,
            Err(error) => {
                self.finish_archive_review(draft, Err(error.to_string()));
                return Task::none();
            }
        };
        if let Some(estimate) = &mut draft.estimate {
            estimate.cancel();
        }
        let title = match draft.side {
            Side::Local => "Reviewing local archive",
            Side::Remote => "Reviewing server archive",
        };
        let task = self.dispatch(operation, title, None);
        if let Some(busy) = &mut self.busy {
            busy.archive_review = Some(draft);
        }
        task
    }

    fn archive_review_operation(
        &self,
        draft: &Draft,
        options: Options,
    ) -> io::Result<worker::Operation> {
        let cancel = worker::sync::Cancel::default();
        let selected = draft.selected.clone();
        match &draft.listing.path {
            Location::Local(directory) => local::Request::new(directory.clone(), selected, options)
                .map(|request| worker::Operation::ReviewLocalArchive(Box::new(request), cancel)),
            Location::Remote(directory) => {
                let worker = self
                    .worker
                    .as_ref()
                    .filter(|worker| draft.server_only || worker.supports_archive())
                    .ok_or_else(|| {
                        io::Error::other(
                            "A live SFTP connection with hard-link support is required.",
                        )
                    })?;
                let request = Request::new(
                    directory.clone(),
                    selected,
                    options,
                    self.target.clone(),
                    worker,
                )?;
                if draft.server_only {
                    return Ok(worker::Operation::ReviewServerArchive(
                        Box::new(request.on_server(worker)?),
                        cancel,
                    ));
                }
                let metadata = draft.metadata().ok_or_else(|| {
                    io::Error::other("Enter valid server archive ownership and permissions.")
                })?;
                let request = if metadata.is_empty() {
                    request
                } else {
                    request.with_metadata(
                        metadata,
                        worker,
                        draft
                            .estimate
                            .as_ref()
                            .is_some_and(estimate::Selection::is_pending),
                    )?
                };
                Ok(worker::Operation::ReviewServerArchive(
                    Box::new(request),
                    cancel,
                ))
            }
        }
    }

    fn apply_archive(&mut self) -> Task<WindowMessage> {
        let Some(Prompt::Archive(mut draft)) = self.prompt.take() else {
            return Task::none();
        };
        let Some(review) = draft.review.take() else {
            return Task::none();
        };
        let cancel = worker::sync::Cancel::default();
        let (operation, title) = match review {
            Review::Local(plan) => {
                let password = plan
                    .format()
                    .is_password_protected()
                    .then_some(draft.password);
                (
                    worker::Operation::ApplyLocalArchive(plan, password, cancel),
                    "Creating local archive",
                )
            }
            Review::Remote(plan) => {
                if !self
                    .worker
                    .as_ref()
                    .is_some_and(|worker| plan.applies_to(worker, &self.target))
                {
                    self.status = Some(
                        "The reviewed SSH connection changed. Review the archive again.".into(),
                    );
                    return Task::none();
                }
                (
                    worker::Operation::ApplyServerArchive(plan, cancel),
                    "Creating server archive",
                )
            }
        };
        self.dispatch(operation, title, None)
    }

    pub(super) fn finish_archive_review(
        &mut self,
        mut draft: Draft,
        result: Result<Review, String>,
    ) {
        if !draft.still_selected(self.panel(draft.side)) {
            self.status = Some("The archive selection changed. Select the entries again.".into());
            return;
        }
        self.status = None;
        match result {
            Ok(review) => {
                draft.review = Some(review);
                draft.error = None;
                draft.page = 0;
            }
            Err(error) => draft.error = Some(error),
        }
        let prompt = Prompt::Archive(draft);
        if let Some(Prompt::Close(previous)) = &mut self.prompt {
            *previous = Some(Box::new(prompt));
        } else {
            self.prompt = Some(prompt);
        }
    }

    pub(super) fn clear_archive_prompt(&mut self, side: Option<Side>) {
        let matches_side = |prompt: &Prompt| matches!(prompt, Prompt::Archive(draft) if side.is_none_or(|side| draft.side == side));
        if self.prompt.as_ref().is_some_and(matches_side) {
            self.prompt = None;
        } else if let Some(Prompt::Close(previous)) = &mut self.prompt
            && previous.as_deref().is_some_and(matches_side)
        {
            *previous = None;
        }
    }

    pub(super) fn revalidate_archive_transfer(&mut self) {
        fn denied(prompt: &mut Prompt) -> bool {
            let Prompt::Archive(draft) = prompt else {
                return false;
            };
            if draft.side != Side::Remote {
                return false;
            }
            draft.client_available = false;
            !draft.server_only
        }
        if self.prompt.as_mut().is_some_and(denied) {
            self.prompt = None;
        } else if let Some(Prompt::Close(previous)) = &mut self.prompt
            && previous.as_deref_mut().is_some_and(denied)
        {
            *previous = None;
        }
    }
}
