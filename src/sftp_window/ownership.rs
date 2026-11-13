use std::sync::Arc;

use iced::widget::{button, checkbox, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

use super::worker::ownership::principals::{INPUT_LIMIT, Selection};
use super::{
    Directory, Entry, EntryKind, EntryName, Location, Message, Panel, Prompt, SftpWindow, Side,
    files, worker,
};

const REVIEW_PAGE_SIZE: usize = 16;
const PERMISSIONS: [(&str, u32, char); 3] =
    [("Read", 4, 'r'), ("Write", 2, 'w'), ("Execute", 1, 'x')];
const PERMISSION_CLASSES: [(&str, u32); 3] = [("Owner", 6), ("Group", 3), ("Others", 0)];

pub(super) struct Draft {
    side: Side,
    listing: Arc<Directory>,
    selected: Vec<Entry>,
    owner: String,
    group: String,
    mode: String,
    recursive: bool,
    review: Option<Arc<worker::OwnershipReview>>,
    error: Option<String>,
    page: usize,
}

impl Draft {
    fn new(side: Side, panel: &Panel) -> Option<Self> {
        let directory = panel.directory.as_ref()?;
        let path = match (side, &directory.path) {
            (Side::Remote, Location::Remote(path)) if path.starts_with(b"/") => path.as_slice(),
            (Side::Local, Location::Local(path)) if cfg!(unix) && path.is_absolute() => {
                path.as_os_str().as_encoded_bytes()
            }
            _ => return None,
        };
        if path.escape_ascii().count() > 4096 {
            return None;
        }
        let selected = panel
            .entries()
            .filter(|entry| panel.selected.contains(&entry.name))
            .filter(|entry| match (side, &entry.name) {
                (Side::Remote, EntryName::Remote(name)) => {
                    entry.ownership_reviewable() && files::validate_transfer_name(name).is_ok()
                }
                (Side::Local, EntryName::Local(_)) => {
                    entry.kind.transferable() && entry.owner.is_some() && entry.group.is_some()
                }
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        (selected.len() == panel.selected.len()
            && (1..=worker::MAX_OWNERSHIP_TARGETS).contains(&selected.len()))
        .then(|| Self {
            side,
            listing: Arc::clone(directory),
            selected,
            owner: String::new(),
            group: String::new(),
            mode: String::new(),
            recursive: false,
            review: None,
            error: None,
            page: 0,
        })
    }

    fn still_selected(&self, panel: &Panel) -> bool {
        panel
            .directory
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.listing))
            && panel.selected.len() == self.selected.len()
            && self
                .selected
                .iter()
                .all(|entry| panel.selected.contains(&entry.name))
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        if let Some(review) = &self.review {
            return self.review_view(review);
        }
        let selection = self.selection();
        let apply = selection
            .as_ref()
            .filter(|value| !value.is_empty())
            .map(|_| Message::ApplyOwnership);
        let needs_review = selection
            .as_ref()
            .is_some_and(|selection| self.needs_review(selection));
        let entries = self
            .selected
            .iter()
            .fold(column![].spacing(4), |rows, entry| {
                let name = match &entry.name {
                    EntryName::Remote(name) => name.as_slice(),
                    EntryName::Local(name) => name.as_encoded_bytes(),
                };
                rows.push(text(format!(
                    "{} · UID {} · GID {} · {}",
                    name.escape_ascii(),
                    entry.owner.unwrap_or_default(),
                    entry.group.unwrap_or_default(),
                    entry
                        .mode
                        .map_or_else(|| "Mode unavailable".into(), mode_label),
                )))
            });
        let path = match &self.listing.path {
            Location::Remote(path) => path.as_slice(),
            Location::Local(path) => path.as_os_str().as_encoded_bytes(),
        };
        let mut form = column![
            text(match self.side {
                Side::Local => "Local ownership and permissions",
                Side::Remote => "Server ownership and permissions",
            })
            .size(20),
            text(format!("Folder · {}", path.escape_ascii())),
            text(format!("Selected entries · {}", self.selected.len())),
            scrollable(entries).height(Length::Fixed(96.0)),
            text("Leave a field blank to keep its current value for each entry."),
            text_input("Owner name or UID", &self.owner)
                .on_input(Message::OwnerId)
                .on_submit_maybe(apply.clone()),
            text_input("Group name or GID", &self.group)
                .on_input(Message::GroupId)
                .on_submit_maybe(apply.clone()),
            text_input("Permissions (three octal digits)", &self.mode)
                .on_input(Message::OwnershipMode)
                .on_submit_maybe(apply.clone()),
            self.permissions_view(),
            text("Hard-linked files can share ownership and permissions. A batch can stop partway through."),
        ]
        .padding(16)
        .spacing(8);
        if self.side == Side::Remote {
            form = form.push(text(
                "Server name lookup requires SSH command access on this connection.",
            ));
        }
        if self.can_recurse() {
            form = form.push(
                checkbox(self.recursive)
                    .label("Include entries inside selected folders")
                    .on_toggle(Message::OwnershipRecursive),
            );
        }
        if Selection::parse_mode(&self.mode).is_none() {
            form = form.push(text("Enter exactly three digits from 0 through 7."));
        } else if selection.is_none() {
            form = form.push(text(
                "Enter a valid account name or a numeric ID from 0 through 4294967294.",
            ));
        }
        if let Some(error) = &self.error {
            form = form.push(text(error));
        }
        scrollable(
            form.push(
                row![
                    button("Cancel").on_press(Message::Dismiss),
                    button(if needs_review {
                        "Review changes"
                    } else {
                        "Apply ownership"
                    })
                    .on_press_maybe(apply),
                ]
                .spacing(8),
            ),
        )
        .into()
    }

    fn selection(&self) -> Option<Selection> {
        Selection::parse_with_mode(&self.owner, &self.group, &self.mode)
    }

    fn needs_review(&self, selection: &Selection) -> bool {
        self.recursive
            || self.side == Side::Local
            || selection.uses_names()
            || selection.mode().is_some()
    }

    fn permissions_view(&self) -> Element<'_, Message> {
        let mode = Selection::parse_mode(&self.mode).flatten();
        let displayed = mode.or_else(|| self.common_mode()).unwrap_or_default();
        let rows =
            PERMISSION_CLASSES
                .into_iter()
                .fold(column![].spacing(4), |rows, (label, shift)| {
                    let options = PERMISSIONS.into_iter().fold(
                        row![text(label).width(Length::Fixed(56.0))].spacing(8),
                        |options, (label, bit, _)| {
                            let mask = bit << shift;
                            options.push(checkbox(displayed & mask != 0).label(label).on_toggle(
                                move |checked| {
                                    let next = if checked {
                                        displayed | mask
                                    } else {
                                        displayed & !mask
                                    };
                                    Message::OwnershipMode(format!("{next:03o}"))
                                },
                            ))
                        },
                    );
                    rows.push(options.wrap())
                });
        let summary = mode.map_or_else(|| "Keep current permissions".into(), mode_label);
        rows.push(
            row![
                text(summary),
                button("Clear mode").on_press_maybe(
                    (!self.mode.is_empty()).then(|| Message::OwnershipMode(String::new())),
                ),
            ]
            .spacing(8)
            .wrap(),
        )
        .push(text(
            "An explicit mode clears set-user-ID, set-group-ID and sticky bits.",
        ))
        .into()
    }

    fn common_mode(&self) -> Option<u32> {
        let mode = self.selected.first()?.mode? & 0o777;
        self.selected
            .iter()
            .all(|entry| entry.mode.is_some_and(|value| value & 0o777 == mode))
            .then_some(mode)
    }

    fn can_recurse(&self) -> bool {
        !matches!(&self.listing.path, Location::Remote(path) if super::tree::snapshot::windows_drive(path))
            && self
                .selected
                .iter()
                .any(|entry| entry.kind == EntryKind::Directory)
    }

    fn review_view<'a>(&self, review: &'a worker::OwnershipReview) -> Element<'a, Message> {
        let total = review.rows().len();
        let pages = total.div_ceil(REVIEW_PAGE_SIZE).max(1);
        let page = self.page.min(pages - 1);
        let (uid, gid) = review.principal_labels();
        let rows = review.rows()[page * REVIEW_PAGE_SIZE..]
            .iter()
            .take(REVIEW_PAGE_SIZE)
            .fold(column![].spacing(4), |rows, entry| {
                rows.push(text(format!(
                    "{} · {} · UID {} · GID {} · {}",
                    if entry.directory { "Folder" } else { "File" },
                    entry.path,
                    entry.uid,
                    entry.gid,
                    mode_label(entry.mode),
                )))
            });
        let mut form = column![
            text("Review ownership and permissions").size(20),
            text(format!("Folder · {}", review.directory_label())),
            text(format!("New UID · {uid}    New GID · {gid}")),
            text(format!(
                "Permissions · {}",
                review
                    .mode()
                    .map_or_else(|| "Keep current".into(), mode_label)
            )),
            text(format!(
                "{} reviewed entries · {} skipped",
                total,
                review.skipped()
            )),
            scrollable(rows).height(Length::Fixed(210.0)),
        ]
        .padding(16)
        .spacing(8);
        if pages > 1 {
            form = form.push(
                row![
                    button("Previous")
                        .on_press_maybe(page.checked_sub(1).map(Message::OwnershipPage)),
                    text(format!("{} of {pages}", page + 1)),
                    button("Next").on_press_maybe(
                        (page + 1 < pages).then_some(Message::OwnershipPage(page + 1))
                    ),
                ]
                .spacing(8),
            );
        }
        if review.skipped() != 0 {
            form = form.push(text(match self.side {
                Side::Local => "Symbolic links, hard-linked files and special entries are skipped.",
                Side::Remote => {
                    "Symbolic links, special entries and unsupported names are skipped."
                }
            }));
        }
        scrollable(
            form.push(text(
                "Ownership changes run before permission changes. A batch can stop partway through.",
            ))
            .push(
                row![
                    button("Cancel").on_press(Message::Dismiss),
                    button("Apply changes").on_press(Message::ApplyOwnership),
                ]
                .spacing(8),
            ),
        )
        .into()
    }
}

fn mode_label(mode: u32) -> String {
    let symbols: String = PERMISSION_CLASSES
        .into_iter()
        .flat_map(|(_, shift)| {
            PERMISSIONS.into_iter().map(move |(_, bit, symbol)| {
                if mode & (bit << shift) == 0 {
                    '-'
                } else {
                    symbol
                }
            })
        })
        .collect();
    format!("{:03o} ({symbols})", mode & 0o7777)
}

impl SftpWindow {
    pub(super) fn can_begin_ownership(&self, side: Side) -> bool {
        self.browser_available()
            && !self.transport_busy()
            && match side {
                Side::Local => cfg!(unix),
                Side::Remote => self
                    .worker
                    .as_ref()
                    .is_some_and(worker::Worker::supports_ownership),
            }
            && (1..=worker::MAX_OWNERSHIP_TARGETS).contains(&self.panel(side).selected.len())
    }

    pub(super) fn begin_ownership(&mut self, side: Side) {
        if self.can_begin_ownership(side) {
            if let Some(draft) = Draft::new(side, self.panel(side)) {
                self.prompt = Some(Prompt::Ownership(draft));
            } else {
                self.status = Some(
                    "Select files or folders with complete ownership and file attributes.".into(),
                );
            }
        }
    }

    pub(super) fn edit_ownership(&mut self, owner: bool, value: String) {
        if let Some(Prompt::Ownership(draft)) = &mut self.prompt
            && draft.review.is_none()
        {
            let field = if owner {
                &mut draft.owner
            } else {
                &mut draft.group
            };
            *field = if value.len() <= INPUT_LIMIT {
                value
            } else {
                "Invalid account input".into()
            };
            draft.error = None;
        }
    }

    pub(super) fn ownership_recursive(&mut self, recursive: bool) {
        if let Some(Prompt::Ownership(draft)) = &mut self.prompt
            && draft.review.is_none()
            && draft.can_recurse()
        {
            draft.recursive = recursive;
        }
    }

    pub(super) fn edit_ownership_mode(&mut self, mode: String) {
        if let Some(Prompt::Ownership(draft)) = &mut self.prompt
            && draft.review.is_none()
        {
            draft.mode = if mode.len() <= INPUT_LIMIT {
                mode
            } else {
                "Invalid mode".into()
            };
            draft.error = None;
        }
    }

    pub(super) fn ownership_page(&mut self, page: usize) {
        if let Some(Prompt::Ownership(draft)) = &mut self.prompt
            && let Some(review) = &draft.review
        {
            draft.page = page.min(review.rows().len().saturating_sub(1) / REVIEW_PAGE_SIZE);
        }
    }

    pub(super) fn apply_ownership(&mut self) -> Task<Message> {
        if self.transport_busy() {
            return Task::none();
        }
        let Some(worker) = self.worker.as_ref().filter(|worker| !worker.is_closed()) else {
            return Task::none();
        };
        let Some(Prompt::Ownership(draft)) = &self.prompt else {
            return Task::none();
        };
        if !draft.still_selected(self.panel(draft.side)) {
            self.prompt = None;
            self.status = Some("The selection changed. Review ownership again.".into());
            return Task::none();
        }
        let Some(principals) = draft.selection().filter(|value| !value.is_empty()) else {
            return Task::none();
        };
        let Some(Prompt::Ownership(draft)) = self.prompt.take() else {
            return Task::none();
        };
        if let Some(review) = &draft.review {
            if let Some(operation) = review.apply(worker, &self.target) {
                return self.dispatch(operation, "Changing ownership and permissions", None);
            }
            self.status =
                Some("The original SFTP connection changed. Review ownership again.".into());
            return Task::none();
        }
        let permitted = worker::sync::Cancel::default();
        if let Location::Remote(directory) = &draft.listing.path
            && !draft.needs_review(&principals)
            && let Some((uid, gid)) = principals.numeric_ids()
        {
            return self.dispatch(
                worker::Operation::ChangeOwnership {
                    directory: directory.clone(),
                    selected: draft.selected,
                    uid,
                    gid,
                    route: self.target.clone(),
                    permitted,
                },
                "Changing server ownership",
                None,
            );
        }
        let operation = match &draft.listing.path {
            Location::Remote(directory) => match worker::ownership::recursive::Request::new(
                directory.clone(),
                draft.selected.clone(),
                principals,
                draft.recursive,
                self.target.clone(),
                worker,
            ) {
                Ok(request) => {
                    worker::Operation::ReviewRemoteOwnership(Box::new(request), permitted)
                }
                Err(error) => {
                    self.finish_ownership_review(draft, Err(error.to_string()));
                    return Task::none();
                }
            },
            #[cfg(unix)]
            Location::Local(directory) => worker::Operation::ReviewLocalOwnership {
                directory: directory.clone(),
                selected: draft.selected.clone(),
                principals,
                recursive: draft.recursive,
                permitted,
            },
            #[cfg(not(unix))]
            Location::Local(_) => return Task::none(),
        };
        let task = self.dispatch(operation, "Reviewing ownership and permissions", None);
        if let Some(busy) = &mut self.busy {
            busy.ownership_review = Some(draft);
        }
        task
    }

    pub(super) fn finish_ownership_review(
        &mut self,
        mut draft: Draft,
        result: Result<Arc<worker::OwnershipReview>, String>,
    ) {
        if !draft.still_selected(self.panel(draft.side)) {
            self.status = Some("The selection changed. Review ownership again.".into());
            return;
        }
        self.status = None;
        match result {
            Ok(review) => {
                draft.review = Some(review);
                draft.error = None;
            }
            Err(error) => draft.error = Some(error),
        }
        let prompt = Prompt::Ownership(draft);
        if let Some(Prompt::Close(previous)) = &mut self.prompt {
            *previous = Some(Box::new(prompt));
        } else {
            self.prompt = Some(prompt);
        }
    }

    pub(super) fn clear_ownership_prompt(&mut self) {
        if matches!(&self.prompt, Some(Prompt::Ownership(_))) {
            self.prompt = None;
        } else if let Some(Prompt::Close(previous)) = &mut self.prompt
            && matches!(previous.as_deref(), Some(Prompt::Ownership(_)))
        {
            *previous = None;
        }
    }

    pub(super) fn cancel_ownership(&mut self) {
        if let Some(busy) = self
            .busy
            .as_mut()
            .filter(|busy| busy.ownership && !busy.cancelled)
        {
            busy.cancelled = true;
            if let Some((worker, cancel)) = self.worker.as_ref().zip(busy.sync_cancel.as_ref()) {
                cancel.stop(worker);
            }
            self.status = Some("Stopping ownership and permission changes…".into());
        }
    }

    pub(super) fn ownership_running(&self) -> bool {
        self.busy.as_ref().is_some_and(|busy| busy.ownership)
    }
}
