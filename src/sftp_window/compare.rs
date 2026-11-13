//! Checked, bounded file snapshots for the SFTP comparison view.

use std::io::{self, Read, Write};
use std::sync::Arc;

use iced::widget::{button, checkbox, column, row, scrollable, text, text_editor};
use iced::{Element, Length, Task};
use kortty_ssh::sftp::{Attributes, FileKind, Sftp};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::snippet_window::compare as lines;

use super::{
    Directory, Entry, EntryKind, LocalSource, Location, Message as WindowMessage, SftpWindow, Side,
    files, search, worker,
};

pub(crate) mod ai;
#[cfg(all(test, target_os = "linux"))]
mod draft_replace_native;
pub(super) mod folders;

use worker::sync::replacement::draft::Origin as DraftOrigin;

const MAX_BYTES: u64 = 4 * 1024 * 1024;
const REPLACEMENTS_PER_PAGE: usize = 12;

#[derive(Clone, Debug)]
pub struct Review {
    diff: Arc<lines::Comparison>,
    pub(super) same_bytes: bool,
    remote: bool,
    files: [Location; 2],
    local_origins: Arc<[Option<DraftOrigin>; 2]>,
    remote_origins: Arc<[Option<RemoteDraftOrigin>; 2]>,
}

#[derive(Clone, Debug)]
pub(super) struct RemoteDraftOrigin {
    path: Vec<u8>,
    original: kortty_server_archive::draft_replace::Original,
}

impl RemoteDraftOrigin {
    pub(super) fn path(&self) -> &[u8] {
        &self.path
    }

    pub(super) fn original(&self) -> &kortty_server_archive::draft_replace::Original {
        &self.original
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Files(lines::Message),
    Ai(u64, ai::Event),
    FolderExcludes(u64, text_editor::Action),
    StartFolders(u64),
    FolderPage(usize),
    FolderDifferencesOnly(bool),
    ReviewCopy(Side),
    ConfirmCopy(u64),
    IncludeRemovals(u64, bool),
    ReviewRemovals(u64),
    ConfirmRemovals(u64),
    ReplacementPage(u64, usize),
    ReviewReplacement(u64, usize),
    ReviewReplacementBatch(u64),
    ReviewCombined(u64),
    ReviewCombinedRemovals(u64),
    ConfirmReplacement(u64),
    ConfirmCombinedRemovals(u64),
    BackToCopy(u64),
    Search(search::Message),
    Close,
}

pub(super) enum View {
    Files(Box<FileView>),
    FolderSetup(folders::Setup),
    Folders(folders::View),
    CopySetup(Box<CopySetup>),
    RemovalReview(Box<RemovalReview>),
    ReplacementReview(Box<ReplacementReview>),
    SearchSetup(search::Setup),
    SearchResults(search::View),
}

impl View {
    pub(super) fn files(ticket: u64, review: &Review) -> Self {
        Self::Files(Box::new(FileView::new(ticket, review)))
    }

    pub(super) fn folders(review: Arc<folders::Review>) -> Self {
        Self::Folders(folders::View::new(review))
    }

    pub(super) fn view(&self) -> Element<'_, WindowMessage> {
        match self {
            Self::Files(view) => view.view(),
            Self::FolderSetup(setup) => setup.view(),
            Self::Folders(view) => view.view(),
            Self::CopySetup(setup) => setup.view(),
            Self::RemovalReview(review) => review.view(),
            Self::ReplacementReview(review) => review.view(),
            Self::SearchSetup(setup) => setup.view(),
            Self::SearchResults(view) => view.view(),
        }
        .map(WindowMessage::ComparisonView)
    }
}

pub(super) struct FileView {
    diff: lines::View,
    same_bytes: bool,
    ticket: u64,
    source: Arc<lines::Comparison>,
    remote: bool,
    files: [Location; 2],
    local_origins: Arc<[Option<DraftOrigin>; 2]>,
    remote_origins: Arc<[Option<RemoteDraftOrigin>; 2]>,
    refresh: Option<Refresh>,
    notice: Option<String>,
    stale: bool,
    ai: Option<Box<ai::Panel>>,
}

struct Refresh {
    ticket: u64,
    note: String,
    connection: worker::ComparisonConnection,
    path: Location,
    digest: [u8; 32],
}

pub(super) struct CopySetup {
    ticket: u64,
    plan: worker::sync::Plan,
    review: Arc<folders::Review>,
    to: Side,
    removal_candidates: usize,
    include_removals: bool,
    replacement_candidates: usize,
    replacement_page: usize,
    error: Option<String>,
}

impl CopySetup {
    fn has_copies(&self) -> bool {
        self.plan.counts() != (0, 0)
    }

    fn replacement_pages(&self) -> usize {
        self.replacement_candidates.div_ceil(REPLACEMENTS_PER_PAGE)
    }

    fn has_combined_changes(&self) -> bool {
        self.has_copies() && self.replacement_candidates != 0
    }

    fn view(&self) -> Element<'_, Message> {
        let [source, target] = self.plan.roots();
        let (files, folders) = self.plan.counts();
        let mut rows = self
            .plan
            .paths()
            .fold(column![].spacing(8), |rows, (folder, path)| {
                let operation = if folder { "Create folder" } else { "Copy file" };
                rows.push(sync_detail(format!("{operation} · {path}")))
            });
        let mut view = column![
            text("Review directional synchronization").size(19),
            sync_detail(format!("From · {source}")),
            sync_detail(format!("To · {target}")),
            text(format!("{files} new files · {folders} new folders · {} bytes", self.plan.bytes())),
            text(if self.has_copies() {
                "Each copy requires an unused destination. Completed copies remain if synchronization stops."
            } else {
                "There are no missing entries to copy."
            }),
        ].spacing(8);
        let show_replacements = self.replacement_candidates != 0 && !self.include_removals;
        if show_replacements {
            rows = rows
                .push(text("Changed files eligible for replacement").size(16))
                .push(
                    button(text(format!(
                        "Review all {} changed files…",
                        self.replacement_candidates
                    )))
                    .on_press(Message::ReviewReplacementBatch(self.ticket)),
                );
            let first = self.replacement_page * REPLACEMENTS_PER_PAGE;
            for (index, path) in self
                .review
                .replacement_candidates(self.to)
                .enumerate()
                .skip(first)
                .take(REPLACEMENTS_PER_PAGE)
            {
                rows = rows.push(
                    row![
                        sync_detail(path.review_label()),
                        button("Review replacement…")
                            .on_press(Message::ReviewReplacement(self.ticket, index)),
                    ]
                    .spacing(8)
                    .wrap(),
                );
            }
        }
        if self.has_copies() || show_replacements {
            view = view.push(scrollable(rows).height(Length::Fill));
        }
        let pages = self.replacement_pages();
        if show_replacements && pages > 1 {
            let page = self.replacement_page;
            view = view.push(
                row![
                    button("Previous").on_press_maybe(
                        page.checked_sub(1)
                            .map(|page| Message::ReplacementPage(self.ticket, page))
                    ),
                    text(format!("Replacement page {} of {pages}", page + 1)),
                    button("Next").on_press_maybe(
                        (page + 1 < pages)
                            .then_some(Message::ReplacementPage(self.ticket, page + 1))
                    ),
                ]
                .spacing(8)
                .wrap(),
            );
        }
        if self.removal_candidates != 0 {
            view = view.push(
                checkbox(self.include_removals)
                    .label(format!(
                        "Review removal of destination-only entries ({})",
                        self.removal_candidates
                    ))
                    .on_toggle(move |remove| Message::IncludeRemovals(self.ticket, remove)),
            );
        }
        if let Some(error) = &self.error {
            view = view.push(text(error));
        }
        let start = if self.include_removals {
            button("Review removals…").on_press(Message::ReviewRemovals(self.ticket))
        } else {
            button("Copy reviewed files").on_press_maybe(
                self.has_copies()
                    .then_some(Message::ConfirmCopy(self.ticket)),
            )
        };
        let mut actions = row![].spacing(8);
        if self.has_combined_changes() {
            actions = actions.push(
                button("Review synchronization…").on_press(Message::ReviewCombined(self.ticket)),
            );
        }
        view.push(
            actions
                .push(start)
                .push(button("Cancel").on_press(Message::Close))
                .wrap(),
        )
        .height(Length::Fill)
        .padding(14)
        .into()
    }
}

pub(super) struct ReplacementReview {
    ticket: u64,
    plan: Arc<worker::sync::replacement::Plan>,
    previous: Box<CopySetup>,
    reviewing_removals: bool,
}

enum ReplacementSelection {
    Single(usize),
    Batch,
    Combined,
}

impl ReplacementReview {
    fn view(&self) -> Element<'_, Message> {
        if self.reviewing_removals {
            return self.removals_view();
        }
        let local = self.plan.direction() == Side::Local;
        let direction = if local {
            "Download from"
        } else {
            "Upload from"
        };
        let mut details = column![].spacing(8);
        if let Some(combined) = self.plan.combined() {
            let [source, target] = self.previous.plan.roots();
            let (files, folders) = combined.copy_counts();
            details = details
                .push(sync_detail(format!("From · {source}")))
                .push(sync_detail(format!("To · {target}")))
                .push(text(format!("{files} new files · {folders} new folders")).size(16));
            for (folder, path) in combined.copy_paths() {
                let operation = if folder { "Create folder" } else { "Copy file" };
                details = details.push(sync_detail(format!("{operation} · {path}")));
            }
            if !self.plan.entries().is_empty() {
                details = details.push(text("Changed files to replace").size(16));
            }
        }
        let mut staged_replacement = false;
        for entry in self.plan.entries() {
            let [source, target] = entry.labels();
            let [source_size, target_size] = entry.sizes();
            let [source_digest, target_digest] = entry.digests();
            details = details
                .push(sync_detail(format!("{direction} · {source}")))
                .push(sync_detail(format!(
                    "New file · {source_size} bytes · SHA-256 {source_digest}"
                )))
                .push(sync_detail(format!("Replace · {target}")))
                .push(sync_detail(format!(
                    "Original file · {target_size} bytes · SHA-256 {target_digest}"
                )));
            if entry.is_in_place() {
                details = details.push(sync_detail(
                    "Write in place to retain the server file's owner and group. Readers may see partial contents during the write. The server may decline the source modification time.".into(),
                ));
            } else {
                staged_replacement = true;
            }
        }
        if !self.plan.entries().is_empty() {
            details = details.push(sync_detail(
                "Original files are kept in private backups beside the destinations.".into(),
            ));
        }
        if staged_replacement {
            details = details.push(sync_detail(if local {
                "Replacements keep the original POSIX permissions or Windows access rules and use the server modification time.".into()
            } else {
                "Staged replacements keep the original POSIX mode and use the local modification time. The original group is copied when the server allows it.".into()
            }));
        }
        if !self.plan.omitted().is_empty() {
            details = details.push(text("Unavailable for replacement").size(16));
            for issue in self.plan.omitted() {
                details = details.push(sync_detail(issue.clone()));
            }
        }
        let removals = self.removal_count();
        if removals != 0 {
            details = details.push(text(format!("{removals} optional removals")).size(16));
            if let Some(combined) = self.plan.combined() {
                for (folder, path) in combined.removal_paths() {
                    details = details.push(removal_detail(folder, path));
                }
            }
        }
        let title = if self.plan.combined().is_some() {
            "Confirm directional synchronization".into()
        } else if self.plan.is_batch() {
            format!(
                "Confirm {} changed-file replacements",
                self.plan.entries().len()
            )
        } else if local {
            "Confirm local file replacement".into()
        } else {
            "Confirm server file replacement".into()
        };
        let action = if removals != 0 {
            "Synchronize without removals"
        } else if self.plan.combined().is_some() {
            "Synchronize reviewed entries"
        } else if self.plan.is_batch() {
            "Replace reviewed files"
        } else if local {
            "Download and replace file"
        } else {
            "Upload and replace file"
        };
        let mut actions =
            row![button("Back").on_press(Message::BackToCopy(self.ticket))].spacing(8);
        if removals != 0 {
            actions = actions.push(
                button("Review permanent removals…")
                    .on_press(Message::ReviewCombinedRemovals(self.ticket)),
            );
        }
        let actions = actions
            .push(button(action).on_press(Message::ConfirmReplacement(self.ticket)))
            .push(button("Cancel").on_press(Message::Close));
        column![
            text(title).size(19),
            scrollable(details).height(Length::Fill),
            actions.wrap()
        ]
        .height(Length::Fill)
        .padding(14)
        .spacing(8)
        .into()
    }

    fn removal_count(&self) -> usize {
        self.plan
            .combined()
            .map_or(0, |combined| combined.removal_count())
    }

    fn removals_view(&self) -> Element<'_, Message> {
        let [source, target] = self.previous.plan.roots();
        let mut rows = column![].spacing(4);
        if let Some(combined) = self.plan.combined() {
            for (folder, path) in combined.removal_paths() {
                rows = rows.push(removal_detail(folder, path));
            }
        }
        column![
            text("Confirm permanent removals").size(19),
            sync_detail(format!("Source · {source}")),
            sync_detail(format!("Destination · {target}")),
            text("Removals start after the reviewed copies and replacements succeed. Folders are removed only if empty."),
            scrollable(rows).height(Length::Fill),
            row![
                button("Back").on_press(Message::BackToCopy(self.ticket)),
                button("Synchronize and remove reviewed entries")
                    .on_press(Message::ConfirmCombinedRemovals(self.ticket)),
                button("Synchronize without removals")
                    .on_press(Message::ConfirmReplacement(self.ticket)),
                button("Cancel").on_press(Message::Close),
            ].spacing(8).wrap(),
        ].height(Length::Fill).padding(14).spacing(8).into()
    }
}

pub(super) struct RemovalReview {
    ticket: u64,
    plan: worker::sync::Plan,
    previous: Box<CopySetup>,
}

impl RemovalReview {
    fn view(&self) -> Element<'_, Message> {
        let [source, target] = self.plan.roots();
        let (files, folders) = self.plan.counts();
        let rows = self
            .plan
            .removal_paths()
            .fold(column![].spacing(4), |rows, (folder, path)| {
                rows.push(removal_detail(folder, path))
            });
        column![
            text("Confirm permanent removals").size(19),
            sync_detail(format!("Source · {source}")),
            sync_detail(format!("Destination · {target}")),
            text(format!("{} reviewed removals · {files} files and {folders} folders to copy", self.plan.removal_count())),
            text("Only the listed destination entries are eligible. A folder is removed after its reviewed children and only if it is empty."),
            text("Completed changes remain if synchronization stops."),
            scrollable(rows).height(Length::Fill),
            row![
                button("Back").on_press(Message::BackToCopy(self.ticket)),
                button(if (files, folders) == (0, 0) {
                    "Remove reviewed entries"
                } else {
                    "Synchronize and remove reviewed entries"
                }).on_press(Message::ConfirmRemovals(self.ticket)),
                button("Cancel").on_press(Message::Close),
            ].spacing(8).wrap(),
        ].height(Length::Fill).padding(14).spacing(8).into()
    }
}

fn removal_detail(folder: bool, path: &str) -> iced::widget::Text<'static> {
    let operation = if folder {
        "Remove folder if empty"
    } else {
        "Remove file"
    };
    sync_detail(format!("{operation} · {path}"))
}

fn sync_detail(label: String) -> iced::widget::Text<'static> {
    text(label)
        .width(Length::Fill)
        .wrapping(text::Wrapping::WordOrGlyph)
}

impl FileView {
    fn new(ticket: u64, review: &Review) -> Self {
        let mut diff = lines::View::default();
        let _ = diff.update(lines::Message::Loaded(Ok(Arc::clone(&review.diff))));
        Self {
            diff,
            same_bytes: review.same_bytes,
            ticket,
            source: Arc::clone(&review.diff),
            remote: review.remote,
            files: review.files.clone(),
            local_origins: Arc::clone(&review.local_origins),
            remote_origins: Arc::clone(&review.remote_origins),
            refresh: None,
            notice: None,
            stale: false,
            ai: None,
        }
    }

    fn update(&mut self, message: lines::Message) -> Task<Message> {
        self.diff.update(message).map(Message::Files)
    }

    fn start_refresh(
        &mut self,
        path: Location,
        note: String,
        digest: [u8; 32],
        worker: Option<&worker::Worker>,
        next: &mut u64,
    ) -> ai::Update {
        self.stale = true;
        let pending = worker
            .ok_or_else(|| io::Error::other("The original SFTP connection is unavailable"))
            .and_then(|worker| {
                let reply = worker.submit(worker::Operation::CompareFiles(self.files.clone()))?;
                Ok((reply, worker.comparison_connection()))
            });
        let (reply, connection) = match pending {
            Ok(pending) => pending,
            Err(error) => {
                self.notice = Some(refresh_failed(&note, &error.to_string()));
                return ai::Update::None;
            }
        };
        *next = next.wrapping_add(1);
        let ticket = *next;
        self.notice = Some(format!("{note} · Refreshing comparison…"));
        self.refresh = Some(Refresh {
            ticket,
            note,
            connection,
            path,
            digest,
        });
        ai::Update::Task(Task::future(async move {
            let result = match reply.recv().await {
                Ok(Ok(reply)) => match reply.as_ref() {
                    worker::Reply::Compared(review) => Ok(Arc::new(review.clone())),
                    _ => Err("Unexpected comparison response".into()),
                },
                Ok(Err(error)) => Err(error.message),
                Err(_) => Err("The original SFTP connection closed".into()),
            };
            ai::Event::Refreshed(ticket, result)
        }))
    }

    fn finish_refresh(
        &mut self,
        ticket: u64,
        result: Result<Arc<Review>, String>,
        worker: Option<&worker::Worker>,
        target: &crate::workspace::SshTarget,
        policy: &crate::clipboard_policy::ClipboardPolicy,
    ) {
        if self
            .refresh
            .as_ref()
            .is_none_or(|refresh| refresh.ticket != ticket)
        {
            return;
        }
        let refresh = self.refresh.take().expect("current comparison refresh");
        let result = result.and_then(|review| {
            if !worker.is_some_and(|worker| {
                refresh.connection.matches(worker)
                    && (!self.remote
                        || worker
                            .editor()
                            .is_some_and(|editor| editor.allows_source_tool(policy, false)))
            }) || !target.workflow_route_allowed(policy)
            {
                return Err("The original SFTP connection or file access policy changed".into());
            }
            let exact_bytes = match &refresh.path {
                Location::Local(path) => review
                    .local_origins
                    .iter()
                    .flatten()
                    .any(|origin| origin.path() == path && origin.has_digest(&refresh.digest)),
                Location::Remote(path) => {
                    review.remote_origins.iter().flatten().any(|origin| {
                        &origin.path == path && origin.original.digest == refresh.digest
                    })
                }
            };
            if review.files != self.files || !exact_bytes {
                return Err("The saved file changed before the comparison could refresh".into());
            }
            Ok(review)
        });
        match result {
            Ok(review) => {
                self.source = Arc::clone(&review.diff);
                self.same_bytes = review.same_bytes;
                self.local_origins = Arc::clone(&review.local_origins);
                self.remote_origins = Arc::clone(&review.remote_origins);
                let _ = self
                    .diff
                    .update(lines::Message::Loaded(Ok(Arc::clone(&review.diff))));
                if let Some(panel) = &mut self.ai {
                    panel.refresh_baseline();
                }
                self.stale = false;
                self.notice = Some(format!("{} · Comparison refreshed", refresh.note));
            }
            Err(error) => self.notice = Some(refresh_failed(&refresh.note, &error)),
        }
    }

    #[cfg(test)]
    pub(in crate::sftp_window) fn refresh_test_snapshot(
        &self,
    ) -> ([(&str, &str); 2], Option<&str>, bool) {
        (self.source.sources(), self.notice.as_deref(), self.stale)
    }

    fn view(&self) -> Element<'_, Message> {
        if let Some(panel) = &self.ai {
            return panel
                .view(
                    self.local_origins[1].as_ref(),
                    self.remote_origins[1].as_ref(),
                    self.refresh.is_some(),
                    self.stale,
                    self.notice.as_deref(),
                )
                .map(|event| Message::Ai(self.ticket, event));
        }
        column![
            text("Compare files").size(18),
            self.notice.as_ref().map(text),
            text(if self.same_bytes {
                "Files are byte-identical"
            } else {
                "File bytes differ. The line view decodes text and normalizes line endings."
            }),
            row![
                button("Explain differences with AI…").on_press_maybe(
                    (self.refresh.is_none() && !self.stale)
                        .then_some(Message::Ai(self.ticket, ai::Event::Open(ai::Mode::Explain)))
                ),
                button("Draft new file with AI…").on_press_maybe(
                    (self.refresh.is_none() && !self.stale)
                        .then_some(Message::Ai(self.ticket, ai::Event::Open(ai::Mode::Draft)))
                ),
            ]
            .spacing(8),
            self.diff.view().map(Message::Files),
        ]
        .padding(12)
        .spacing(6)
        .height(Length::Fill)
        .into()
    }
}

fn refresh_failed(note: &str, error: &str) -> String {
    format!("{note} · Could not refresh the comparison ({error}). Reopen it to see current files.")
}

impl SftpWindow {
    pub(super) fn comparison_publication_active(&self) -> bool {
        matches!(&self.comparison, Some(View::Files(view)) if view.ai.as_ref().is_some_and(|panel| panel.publication_active()))
    }

    pub(super) fn revoke_comparison_publication(&mut self) {
        if let Some(View::Files(view)) = &mut self.comparison {
            view.stale = true;
            if let Some(panel) = &view.ai {
                panel.revoke_publication();
            }
        }
    }

    #[cfg(test)]
    pub(in crate::sftp_window) fn start_test_comparison_refresh(
        &mut self,
        path: Location,
        digest: [u8; 32],
    ) -> Option<(u64, u64)> {
        let Some(View::Files(view)) = &mut self.comparison else {
            return None;
        };
        let _ = view.start_refresh(
            path,
            "Saved fixture".into(),
            digest,
            self.worker.as_ref(),
            &mut self.counter,
        );
        Some((view.ticket, view.refresh.as_ref()?.ticket))
    }

    pub(crate) fn update_comparison_ai(
        &mut self,
        ticket: u64,
        event: ai::Event,
        scope: ai::Scope<'_>,
    ) -> ai::Update {
        let can_refresh = self.queue_available();
        let Some(View::Files(view)) = &mut self.comparison else {
            return ai::Update::None;
        };
        if view.ticket != ticket {
            return ai::Update::None;
        }
        if let ai::Event::Refreshed(refresh, result) = event {
            view.finish_refresh(
                refresh,
                result,
                self.worker.as_ref(),
                &self.target,
                scope.policy,
            );
            return ai::Update::None;
        }
        if matches!(event, ai::Event::BackToComparison | ai::Event::Open(_))
            && view
                .ai
                .as_ref()
                .is_some_and(|panel| panel.publication_active())
        {
            return ai::Update::None;
        }
        if matches!(event, ai::Event::BackToComparison) {
            view.ai = None;
            return ai::Update::None;
        }
        if let ai::Event::Open(mode) = event {
            if view.refresh.is_some() || view.stale {
                return ai::Update::None;
            }
            view.ai = Some(Box::new(ai::Panel::new(
                mode,
                scope.config.as_deref(),
                self.incognito,
            )));
            return ai::Update::None;
        }
        let source = ai::Source {
            diff: &view.source,
            target: &self.target,
            worker: self.worker.as_ref(),
            remote: view.remote,
            transfer_allowed: self.transfer_allowed,
            private: self.incognito,
            right_local: view.local_origins[1].as_ref(),
            right_remote: view.remote_origins[1].as_ref(),
            remote_directory: self.remote.directory.as_ref().and_then(|directory| {
                if let Location::Remote(path) = &directory.path {
                    Some(path.as_slice())
                } else {
                    None
                }
            }),
        };
        let Some(panel) = &mut view.ai else {
            return ai::Update::None;
        };
        if (view.refresh.is_some() || view.stale)
            && !event.available_during_refresh()
            && !panel.is_publication_receipt(&event)
        {
            return ai::Update::None;
        }
        let update = panel.update(event, source, scope, &mut self.counter);
        if let Some(saved) = panel.take_local_save() {
            let saved_side = view
                .files
                .iter()
                .any(|file| matches!(file, Location::Local(compared) if compared == &saved.path));
            if saved_side {
                return view.start_refresh(
                    Location::Local(saved.path),
                    saved.note,
                    saved.digest,
                    self.worker.as_ref().filter(|_| can_refresh),
                    &mut self.counter,
                );
            }
        }
        if let Some(saved) = panel.take_remote_save() {
            let path = Location::Remote(saved.path);
            if view.files[1] == path {
                return view.start_refresh(
                    path,
                    saved.note,
                    saved.digest,
                    self.worker.as_ref().filter(|_| can_refresh),
                    &mut self.counter,
                );
            }
        }
        update
    }

    pub(super) fn comparison_pair(&self, preferred: Side) -> Option<[Location; 2]> {
        let pair = self.selected_pair(preferred, EntryKind::File)?;
        if self.editor_policy == RemoteEditorPolicy::Deny
            && pair.iter().any(|file| matches!(file, Location::Remote(_)))
        {
            return None;
        }
        Some(pair)
    }

    pub(super) fn folder_pair(&self, preferred: Option<Side>) -> Option<[Location; 2]> {
        if let Some(side) = preferred {
            self.selected_pair(side, EntryKind::Directory)
        } else {
            Some([
                self.local.directory.as_ref()?.path.clone(),
                self.remote.directory.as_ref()?.path.clone(),
            ])
        }
    }

    fn selected_pair(&self, preferred: Side, kind: EntryKind) -> Option<[Location; 2]> {
        let location = |directory: &Directory, entry: &Entry| {
            (entry.kind == kind).then(|| directory.path.with(&entry.name))?
        };
        let panel = self.panel(preferred);
        if panel.selected.len() == 2 {
            let directory = panel.directory.as_deref()?;
            let mut selected = panel
                .entries()
                .filter(|entry| panel.selected.contains(&entry.name));
            Some([
                location(directory, selected.next()?)?,
                location(directory, selected.next()?)?,
            ])
        } else {
            let (left_root, left) = self.local.selected()?;
            let (right_root, right) = self.remote.selected()?;
            Some([location(left_root, left)?, location(right_root, right)?])
        }
    }

    pub(super) fn open_folder_comparison(&mut self, preferred: Option<Side>) {
        if let Some(roots) = self.folder_pair(preferred) {
            self.counter = self.counter.wrapping_add(1);
            self.comparison = Some(View::FolderSetup(folders::Setup::new(self.counter, roots)));
        }
    }

    pub(super) fn open_server_search(&mut self) {
        let Some(Location::Remote(root)) = self
            .remote
            .directory
            .as_ref()
            .map(|directory| &directory.path)
        else {
            return;
        };
        if root.len() > 4096 || root.escape_ascii().to_string().len() > 4096 {
            self.status = Some("The server folder path exceeds the search limit".into());
            return;
        }
        self.counter = self.counter.wrapping_add(1);
        self.comparison = Some(View::SearchSetup(search::Setup::new(
            self.counter,
            root.clone(),
            self.remote.filter.input(),
            self.remote.hide_hidden,
        )));
    }

    pub(super) fn update_comparison(&mut self, message: Message) -> Task<WindowMessage> {
        let idle = !self.transport_busy();
        if matches!(
            message,
            Message::Close | Message::Files(lines::Message::Close)
        ) && self.comparison_publication_active()
        {
            return Task::none();
        }
        match (&mut self.comparison, message) {
            (Some(_), Message::Close | Message::Files(lines::Message::Close)) => {
                self.comparison = None
            }
            (Some(View::FolderSetup(setup)), Message::FolderExcludes(ticket, action)) => {
                setup.edit(ticket, action);
            }
            (Some(View::FolderSetup(setup)), Message::StartFolders(ticket)) => {
                if let Some(request) = setup.start(ticket) {
                    self.comparison = None;
                    return self.dispatch(
                        worker::Operation::CompareFolders(Box::new(request)),
                        "Comparing folders",
                        None,
                    );
                }
            }
            (Some(View::Files(view)), Message::Files(message)) => {
                return view.update(message).map(WindowMessage::ComparisonView);
            }
            (
                Some(View::SearchSetup(setup)),
                Message::Search(search::Message::Input(ticket, value)),
            ) => {
                setup.edit(ticket, value);
            }
            (Some(View::SearchSetup(setup)), Message::Search(search::Message::Start(ticket))) => {
                if let Some(request) = setup.start(ticket) {
                    self.comparison = None;
                    return self.dispatch(
                        worker::Operation::SearchTree(Box::new(request)),
                        "Searching server folders",
                        None,
                    );
                }
            }
            (
                Some(View::SearchResults(view)),
                Message::Search(search::Message::Page(ticket, page)),
            ) => {
                return view.update(ticket, page).map(WindowMessage::ComparisonView);
            }
            (
                Some(View::SearchResults(view)),
                Message::Search(search::Message::Open(ticket, index)),
            ) => {
                if let Some(hit) = view.hit(ticket, index) {
                    self.comparison = None;
                    return self.dispatch(
                        worker::Operation::RevealSearchHit(Box::new(hit)),
                        "Opening search result",
                        None,
                    );
                }
            }
            (Some(View::Folders(view)), Message::ReviewCopy(to))
                if self.transfer_allowed && idle =>
            {
                let review = Arc::clone(view.review());
                let removal_candidates = review.destination_only(to).count();
                let replacement_candidates = review.replacement_candidates(to).count();
                let result = self
                    .worker
                    .as_ref()
                    .ok_or_else(|| io::Error::other("The reviewed SFTP connection closed"))
                    .and_then(|worker| {
                        worker::sync::Plan::new_sync(
                            Arc::clone(&review),
                            self.target.clone(),
                            worker,
                            to,
                            false,
                        )
                    });
                match result {
                    Ok(plan) => {
                        self.counter = self.counter.wrapping_add(1);
                        self.comparison = Some(View::CopySetup(Box::new(CopySetup {
                            ticket: self.counter,
                            plan,
                            review,
                            to,
                            removal_candidates,
                            include_removals: false,
                            replacement_candidates,
                            replacement_page: 0,
                            error: None,
                        })));
                    }
                    Err(error) => view.copy_notice(error.to_string()),
                }
            }
            (Some(View::CopySetup(setup)), Message::IncludeRemovals(ticket, include))
                if setup.ticket == ticket
                    && setup.removal_candidates != 0
                    && self.transfer_allowed =>
            {
                setup.include_removals = include;
                setup.error = None;
            }
            (Some(View::CopySetup(setup)), Message::ReviewRemovals(ticket))
                if setup.ticket == ticket
                    && setup.include_removals
                    && self.transfer_allowed
                    && idle =>
            {
                let result = self.worker.as_ref()
                    .filter(|worker| setup.plan.applies_to(worker, &self.target))
                    .ok_or_else(|| io::Error::other("The reviewed SSH connection changed. Compare again before synchronizing."))
                    .and_then(|worker| worker::sync::Plan::new_sync(
                        Arc::clone(&setup.review), self.target.clone(), worker, setup.to, true,
                    ));
                match result {
                    Ok(plan) if plan.removal_count() != 0 => {
                        let Some(View::CopySetup(previous)) = self.comparison.take() else {
                            return Task::none();
                        };
                        self.counter = self.counter.wrapping_add(1);
                        self.comparison = Some(View::RemovalReview(Box::new(RemovalReview {
                            ticket: self.counter,
                            plan,
                            previous,
                        })));
                    }
                    Ok(_) => {
                        setup.error = Some("No reviewed destination entries can be removed.".into())
                    }
                    Err(error) => setup.error = Some(error.to_string()),
                }
            }
            (Some(View::CopySetup(setup)), Message::ReplacementPage(ticket, page))
                if setup.ticket == ticket
                    && !setup.include_removals
                    && self.transfer_allowed
                    && page < setup.replacement_pages() =>
            {
                setup.replacement_page = page;
            }
            (Some(View::CopySetup(setup)), Message::ReviewReplacement(ticket, index))
                if setup.ticket == ticket
                    && !setup.include_removals
                    && self.transfer_allowed
                    && idle
                    && index / REPLACEMENTS_PER_PAGE == setup.replacement_page
                    && index < setup.replacement_candidates =>
            {
                return self.review_sync_replacements(ReplacementSelection::Single(index));
            }
            (Some(View::CopySetup(setup)), Message::ReviewReplacementBatch(ticket))
                if setup.ticket == ticket
                    && !setup.include_removals
                    && self.transfer_allowed
                    && idle
                    && setup.replacement_candidates != 0 =>
            {
                return self.review_sync_replacements(ReplacementSelection::Batch);
            }
            (Some(View::CopySetup(setup)), Message::ReviewCombined(ticket))
                if setup.ticket == ticket
                    && self.transfer_allowed
                    && idle
                    && setup.has_combined_changes() =>
            {
                return self.review_sync_replacements(ReplacementSelection::Combined);
            }
            (Some(View::RemovalReview(review)), Message::BackToCopy(ticket))
                if review.ticket == ticket =>
            {
                let Some(View::RemovalReview(review)) = self.comparison.take() else {
                    return Task::none();
                };
                self.restore_copy_setup(review.previous);
            }
            (Some(View::ReplacementReview(review)), Message::BackToCopy(ticket))
                if review.ticket == ticket =>
            {
                if review.reviewing_removals {
                    self.counter = self.counter.wrapping_add(1);
                    review.ticket = self.counter;
                    review.reviewing_removals = false;
                    return Task::none();
                }
                let Some(View::ReplacementReview(review)) = self.comparison.take() else {
                    return Task::none();
                };
                self.restore_copy_setup(review.previous);
            }
            (Some(View::ReplacementReview(review)), Message::ReviewCombinedRemovals(ticket))
                if review.ticket == ticket
                    && !review.reviewing_removals
                    && review.removal_count() != 0
                    && self.transfer_allowed
                    && idle =>
            {
                self.counter = self.counter.wrapping_add(1);
                review.ticket = self.counter;
                review.reviewing_removals = true;
            }
            (Some(View::CopySetup(setup)), Message::ConfirmCopy(ticket))
                if setup.ticket == ticket && !setup.include_removals && setup.has_copies() =>
            {
                if !self.transfer_allowed
                    || !idle
                    || !self
                        .worker
                        .as_ref()
                        .is_some_and(|worker| setup.plan.applies_to(worker, &self.target))
                {
                    self.comparison = None;
                    self.status = Some(
                        "The reviewed SSH connection changed. Compare again before copying.".into(),
                    );
                    return Task::none();
                }
                let Some(View::CopySetup(setup)) = self.comparison.take() else {
                    return Task::none();
                };
                return self.start_reviewed_sync(setup.plan);
            }
            (Some(View::RemovalReview(review)), Message::ConfirmRemovals(ticket))
                if review.ticket == ticket && review.plan.removal_count() != 0 =>
            {
                if !self.transfer_allowed
                    || !idle
                    || !self
                        .worker
                        .as_ref()
                        .is_some_and(|worker| review.plan.applies_to(worker, &self.target))
                {
                    self.comparison = None;
                    self.status = Some(
                        "The reviewed SSH connection changed. Compare again before synchronizing."
                            .into(),
                    );
                    return Task::none();
                }
                let Some(View::RemovalReview(review)) = self.comparison.take() else {
                    return Task::none();
                };
                return self.start_reviewed_sync(review.plan);
            }
            (Some(View::ReplacementReview(review)), Message::ConfirmReplacement(ticket))
                if review.ticket == ticket =>
            {
                return self.start_reviewed_replacement(idle, false);
            }
            (Some(View::ReplacementReview(review)), Message::ConfirmCombinedRemovals(ticket))
                if review.ticket == ticket
                    && review.reviewing_removals
                    && review.removal_count() != 0 =>
            {
                return self.start_reviewed_replacement(idle, true);
            }
            (Some(View::Folders(view)), message) => {
                return view.update(message).map(WindowMessage::ComparisonView);
            }
            _ => {}
        }
        Task::none()
    }

    fn review_sync_replacements(&mut self, selection: ReplacementSelection) -> Task<WindowMessage> {
        let Some(View::CopySetup(setup)) = &mut self.comparison else {
            return Task::none();
        };
        let request = self
            .worker
            .as_ref()
            .filter(|worker| setup.plan.applies_to(worker, &self.target))
            .ok_or_else(|| {
                io::Error::other(
                    "The reviewed SSH connection changed. Compare again before replacing files.",
                )
            })
            .and_then(|worker| {
                let review = Arc::clone(&setup.review);
                let route = self.target.clone();
                use worker::sync::replacement::Request;
                match selection {
                    ReplacementSelection::Single(index) => {
                        let path = setup
                            .review
                            .replacement_candidates(setup.to)
                            .nth(index)
                            .ok_or_else(|| {
                                io::Error::other("The selected replacement is unavailable")
                            })?;
                        Request::new(review, route, worker, setup.to, path)
                    }
                    ReplacementSelection::Batch => Request::batch(review, route, worker, setup.to),
                    ReplacementSelection::Combined => {
                        Request::combined(review, route, worker, setup.to, setup.include_removals)
                    }
                }
            });
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                setup.error = Some(error.to_string());
                return Task::none();
            }
        };
        let Some(View::CopySetup(setup)) = self.comparison.take() else {
            return Task::none();
        };
        let task = self.dispatch(
            worker::Operation::ReviewSyncReplacement(
                Box::new(request),
                worker::sync::Cancel::default(),
            ),
            "Preparing synchronization review",
            None,
        );
        if let Some(busy) = &mut self.busy {
            busy.replacement_setup = Some(setup);
        } else if self.worker.is_some() {
            let error = self
                .status
                .clone()
                .unwrap_or_else(|| "Could not start the replacement review.".into());
            self.finish_sync_replacement_review(setup, Err(error));
        }
        task
    }

    fn start_reviewed_replacement(&mut self, idle: bool, remove: bool) -> Task<WindowMessage> {
        let Some(View::ReplacementReview(review)) = self.comparison.take() else {
            return Task::none();
        };
        if !self.transfer_allowed
            || !idle
            || !self
                .worker
                .as_ref()
                .is_some_and(|worker| review.plan.applies_to(worker, &self.target))
        {
            self.status = Some(
                "The reviewed SSH connection changed. Compare again before synchronizing.".into(),
            );
            return Task::none();
        }
        let progress = worker::Progress::new(Some(review.plan.bytes()));
        let title = if review.plan.combined().is_some() {
            "Synchronizing reviewed entries"
        } else if review.plan.is_batch() {
            "Replacing reviewed files"
        } else if review.plan.direction() == Side::Local {
            "Replacing reviewed local file"
        } else {
            "Replacing reviewed server file"
        };
        self.dispatch(
            worker::Operation::ApplySyncReplacement(
                review.plan,
                progress.clone(),
                worker::sync::Cancel::default(),
                remove,
            ),
            title,
            Some(progress),
        )
    }

    pub(super) fn finish_sync_replacement_review(
        &mut self,
        mut setup: Box<CopySetup>,
        result: Result<Arc<worker::sync::replacement::Plan>, String>,
    ) {
        if !self.transfer_allowed || self.prompt.is_some() {
            return;
        }
        let plan = match result {
            Ok(plan)
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|worker| plan.applies_to(worker, &self.target)) =>
            {
                plan
            }
            Ok(_) => {
                self.status = Some(
                    "The reviewed SSH connection changed. Compare again before replacing a file."
                        .into(),
                );
                return;
            }
            Err(error) => {
                setup.error = Some(error);
                self.status = None;
                self.restore_copy_setup(setup);
                return;
            }
        };
        self.status = None;
        self.counter = self.counter.wrapping_add(1);
        self.comparison = Some(View::ReplacementReview(Box::new(ReplacementReview {
            ticket: self.counter,
            plan,
            previous: setup,
            reviewing_removals: false,
        })));
    }

    fn restore_copy_setup(&mut self, mut setup: Box<CopySetup>) {
        self.counter = self.counter.wrapping_add(1);
        setup.ticket = self.counter;
        self.comparison = Some(View::CopySetup(setup));
    }

    fn start_reviewed_sync(&mut self, plan: worker::sync::Plan) -> Task<WindowMessage> {
        let copying = plan.counts() != (0, 0);
        let title = if plan.removal_count() == 0 {
            "Copying reviewed files"
        } else if copying {
            "Synchronizing reviewed folders"
        } else {
            "Removing reviewed destination entries"
        };
        let progress = worker::Progress::new(copying.then_some(plan.bytes()));
        self.dispatch(
            worker::Operation::ApplySync(
                Box::new(plan),
                progress.clone(),
                worker::sync::Cancel::default(),
            ),
            title,
            copying.then_some(progress),
        )
    }
}

pub(super) fn read(
    client: &mut Sftp<impl Read, impl Write>,
    files: [Location; 2],
    policy: RemoteEditorPolicy,
    cancelled: impl Fn() -> bool,
) -> Result<Review, worker::Failure> {
    if policy == RemoteEditorPolicy::Deny
        && files.iter().any(|file| matches!(file, Location::Remote(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Application policy forbids remote file comparison",
        )
        .into());
    }
    let remote = files.iter().any(|file| matches!(file, Location::Remote(_)));
    let [left, right] = files.clone();
    let left = Snapshot::read(client, left, &cancelled)?;
    let right = Snapshot::read(client, right, &cancelled)?;
    left.verify(client)?;
    right.verify(client)?;
    check_cancelled(&cancelled)?;
    let same_bytes = left.bytes == right.bytes;
    let local_origins = Arc::new([
        left.local_origin(&files[0])?,
        right.local_origin(&files[1])?,
    ]);
    let remote_origins = Arc::new([left.remote_origin()?, right.remote_origin()?]);
    let diff = lines::Request::texts(left.text()?, right.text()?)
        .compare()
        .map_err(io::Error::other)?;
    check_cancelled(&cancelled)?;
    Ok(Review {
        diff: Arc::new(diff),
        same_bytes,
        remote,
        files,
        local_origins,
        remote_origins,
    })
}

struct Snapshot {
    source: Source,
    label: String,
    bytes: Vec<u8>,
}

enum Source {
    Local(LocalSource),
    Remote { path: Vec<u8>, stamp: Attributes },
}

impl Snapshot {
    fn remote_origin(&self) -> io::Result<Option<RemoteDraftOrigin>> {
        let Source::Remote { path, stamp } = &self.source else {
            return Ok(None);
        };
        let (Some(uid), Some(gid), Some(mode)) = (stamp.uid, stamp.gid, stamp.permissions) else {
            return Ok(None);
        };
        if std::str::from_utf8(path).is_err() || mode & 0o7022 != 0 {
            return Ok(None);
        }
        Ok(Some(RemoteDraftOrigin {
            path: path.clone(),
            original: kortty_server_archive::draft_replace::Original {
                bytes: self.bytes.len() as u64,
                digest: worker::draft_file::digest(&self.bytes)?,
                uid,
                gid,
                mode: mode & 0o777,
            },
        }))
    }

    fn local_origin(&self, location: &Location) -> io::Result<Option<DraftOrigin>> {
        match (location, &self.source) {
            (Location::Local(path), Source::Local(source)) => {
                DraftOrigin::capture(path.clone(), source, &self.bytes).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn read(
        client: &mut Sftp<impl Read, impl Write>,
        location: Location,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, worker::Failure> {
        check_cancelled(cancelled)?;
        let place = match &location {
            Location::Local(_) => "Local",
            Location::Remote(_) => "Server",
        };
        let label = format!("{place} · {}", location.display());
        let (source, bytes) = match location {
            Location::Local(path) => {
                let source = LocalSource::open(&path)?;
                source.verify()?;
                let size = source.size();
                let mut capture = Capture::new(size, cancelled)?;
                let mut input = source.file();
                let mut block = [0; 32 * 1024];
                loop {
                    check_cancelled(cancelled)?;
                    let count = input.read(&mut block)?;
                    if count == 0 {
                        break;
                    }
                    capture.write_all(&block[..count])?;
                }
                (Source::Local(source), capture.finish()?)
            }
            Location::Remote(path) => {
                verify_parent(client, &path)?;
                let stamp = client.lstat(&path)?;
                if stamp.kind() != FileKind::File {
                    return Err(
                        io::Error::other("Only regular remote files can be compared").into(),
                    );
                }
                let size = stamp
                    .size
                    .ok_or_else(|| io::Error::other("The server did not report the file size"))?;
                let mut capture = Capture::new(size, cancelled)?;
                client.download_regular(&path, stamp, &mut capture)?;
                (Source::Remote { path, stamp }, capture.finish()?)
            }
        };
        let snapshot = Self {
            source,
            label,
            bytes,
        };
        snapshot.verify(client)?;
        Ok(snapshot)
    }

    fn verify(&self, client: &mut Sftp<impl Read, impl Write>) -> Result<(), worker::Failure> {
        match &self.source {
            Source::Local(source) => source.verify()?,
            Source::Remote { path, stamp } => {
                verify_parent(client, path)?;
                if !worker::unchanged(*stamp, client.lstat(path)?) {
                    return Err(
                        io::Error::other("The remote file changed during comparison").into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn text(self) -> io::Result<(String, String)> {
        let bytes = self
            .bytes
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(&self.bytes);
        if bytes.contains(&0) {
            return Err(io::Error::other("Binary files cannot be compared as text"));
        }
        let text = match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => bytes.iter().copied().map(char::from).collect(),
        };
        Ok((self.label, text))
    }
}

fn verify_parent(
    client: &mut Sftp<impl Read, impl Write>,
    path: &[u8],
) -> Result<(), worker::Failure> {
    let split = path
        .iter()
        .rposition(|&byte| byte == b'/')
        .filter(|_| path.starts_with(b"/") && !path.contains(&0))
        .ok_or_else(|| io::Error::other("The remote comparison needs an absolute file path"))?;
    files::validate_transfer_name(&path[split + 1..])?;
    let parent = &path[..split.max(1)];
    if client.canonicalize(parent)? != parent {
        return Err(io::Error::other("The remote file's folder changed during comparison").into());
    }
    Ok(())
}

struct Capture<'a, C> {
    bytes: Vec<u8>,
    size: usize,
    cancelled: &'a C,
}

impl<'a, C: Fn() -> bool> Capture<'a, C> {
    fn new(size: u64, cancelled: &'a C) -> io::Result<Self> {
        if size > MAX_BYTES {
            return Err(io::Error::other(
                "A file exceeds the 4 MiB comparison limit",
            ));
        }
        let size = size as usize;
        Ok(Self {
            bytes: Vec::with_capacity(size),
            size,
            cancelled,
        })
    }

    fn finish(self) -> io::Result<Vec<u8>> {
        self.flush_check()?;
        if self.bytes.len() != self.size {
            return Err(io::Error::other("The file changed during comparison"));
        }
        Ok(self.bytes)
    }

    fn flush_check(&self) -> io::Result<()> {
        check_cancelled(self.cancelled)
    }
}

impl<C: Fn() -> bool> Write for Capture<'_, C> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.flush_check()?;
        if bytes.len() > self.size - self.bytes.len() {
            return Err(io::Error::other("The file grew during comparison"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_check()
    }
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "File comparison cancelled",
        ))
    } else {
        Ok(())
    }
}
