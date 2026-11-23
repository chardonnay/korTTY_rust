//! Inventory tables for knowledge stores and their sources.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use iced::widget::{
    button, column, container, pick_list, row, rule, scrollable, space, text, text_input, tooltip,
};
use iced::{Alignment, Element, Font, Length, Task};

use super::knowledge::{
    SourceKey, SourceResult, group_source_results, outcomes, sync::SourceStatus,
};
use super::{
    Dialog, Embedding, Index, IndexReview, KnowledgeSourcePicker, Message, SOURCE_REVIEW_PAGE_SIZE,
    SourceKind, State, Store, SyncMode, file_review, folder, folder_review_view, knowledge_options,
};
use crate::ai_manager::file_chooser::display_path;
use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::Reveal;
use crate::workspace::dialog_design;

pub(super) fn body_heights(viewport_height: f32) -> (f32, f32) {
    // Reserve room for the introduction, table headings and action rows.
    let available = (viewport_height - 220.0).max(162.0);
    let sources = (available / 2.0).max(108.0);
    (available - sources, sources)
}

type SourceRows = BTreeMap<SourceKey, SourceResult>;

pub(super) struct SyncEdit {
    pub(super) store: usize,
    pub(super) source: SourceKey,
    pub(super) mode: SyncMode,
}

#[derive(PartialEq, Eq)]
pub(super) enum SourcePhase {
    Scanning,
    Indexing,
    Cancelling,
    Cancelled,
    Interrupted,
}

pub(super) struct SourceWork {
    store: Store,
    rows: BTreeSet<SourceKey>,
    pub(super) phase: SourcePhase,
    pub(super) failed: Option<SourceKey>,
}

impl SourceWork {
    pub(super) fn new(
        store: &Store,
        rows: impl IntoIterator<Item = SourceKey>,
        phase: SourcePhase,
    ) -> Self {
        Self {
            store: store.clone(),
            rows: rows.into_iter().collect(),
            phase,
            failed: None,
        }
    }

    pub(super) fn status(&self, key: &SourceKey) -> Option<&'static str> {
        self.rows.contains(key).then(|| {
            if self.failed.as_ref() == Some(key) {
                "Error"
            } else {
                match self.phase {
                    SourcePhase::Scanning => "Scanning…",
                    SourcePhase::Indexing => "Indexing…",
                    SourcePhase::Cancelling => "Cancelling…",
                    SourcePhase::Cancelled => "Cancelled",
                    SourcePhase::Interrupted => "Not updated",
                }
            }
        })
    }
}

#[derive(Default)]
pub(super) struct Results {
    ticket: u64,
    store: Option<(Store, Embedding)>,
    pending: Option<folder::Cancel>,
    rows: Option<Arc<SourceRows>>,
    outcomes: outcomes::Rows,
}

impl Results {
    #[cfg(test)]
    pub(super) fn restored(
        &self,
        source: &SourceKey,
    ) -> (Option<SourceStatus>, Option<SourceResult>) {
        (
            self.outcomes.get(source).copied(),
            self.rows
                .as_deref()
                .and_then(|rows| rows.get(source))
                .copied(),
        )
    }

    pub(super) fn reset(&mut self) {
        self.ticket = self.ticket.wrapping_add(1);
        self.store = None;
        self.pending = None;
        self.rows = None;
        self.outcomes.clear();
    }

    pub(super) fn loaded(&mut self, ticket: u64, rows: Arc<SourceRows>) {
        if self.ticket == ticket && self.pending.take().is_some() {
            self.rows = Some(rows);
        }
    }

    fn display_completed(&mut self, review: &IndexReview) {
        self.outcomes.extend(
            review
                .outcomes
                .iter()
                .map(|(source, &status)| (source.clone(), status)),
        );
    }

    fn bind(&mut self, root: &Path, state: &State, store: &Store) {
        let (endpoint, model, managed_model) = state.embedding_fields(store);
        self.store = Some((
            store.clone(),
            Embedding {
                endpoint: endpoint.into(),
                model: model.into(),
                managed_model: managed_model.map(str::to_owned),
            },
        ));
        self.outcomes = outcomes::load(root, state, store).unwrap_or_default();
    }

    fn matches(&self, state: &State, store: &Store) -> Option<()> {
        let (indexed, destination) = self.store.as_ref()?;
        let fields = state.embedding_fields(store);
        (*indexed == *store
            && fields
                == (
                    destination.endpoint.as_str(),
                    destination.model.as_str(),
                    destination.managed_model.as_deref(),
                ))
            .then_some(())
    }

    fn rows(&self, state: &State, store: &Store) -> Option<&SourceRows> {
        self.matches(state, store)?;
        self.rows.as_deref()
    }

    pub(super) fn replace(
        &mut self,
        root: &Path,
        state: &State,
        selected: Option<usize>,
        index: &Index,
        review: &IndexReview,
    ) {
        self.reset();
        if let Some(store) = selected.and_then(|selected| state.stores.get(selected)) {
            self.bind(root, state, store);
            self.display_completed(review);
            let coverage = store.qdrant.as_ref().map(|_| &review.coverage);
            self.rows = Some(Arc::new(group_source_results(
                store,
                index.source_results(coverage),
            )));
        }
    }
}

impl Dialog {
    pub(in crate::ai_manager) fn refresh_source_outcomes(&mut self, state: &State) {
        if let Some(store) = self.selected.and_then(|id| state.stores.get(id))
            && self.source_results.matches(state, store).is_some()
        {
            self.source_results.outcomes =
                outcomes::load(&self.profile_dir, state, store).unwrap_or_default();
        }
    }

    pub(in crate::ai_manager) fn load_source_results(&mut self, state: &State) -> Task<Message> {
        self.source_results.reset();
        let Some(store) = self
            .selected
            .and_then(|selected| state.stores.get(selected))
        else {
            return Task::none();
        };
        self.source_results.bind(&self.profile_dir, state, store);
        let Ok(scope) = state.scope(store, &self.profile_dir) else {
            if state.embedding_fields(store).1.trim().is_empty() {
                self.source_results.rows =
                    Some(Arc::new(group_source_results(store, std::iter::empty())));
            }
            return Task::none();
        };
        if let Some(draft) = self.drafts.iter().find(|draft| scope.same_as(&draft.index)) {
            if let Some(review) = &draft.review {
                self.source_results.display_completed(review);
            }
            let coverage = draft
                .review
                .as_ref()
                .filter(|_| store.qdrant.is_some())
                .map(|review| &review.coverage);
            self.source_results.rows = Some(Arc::new(group_source_results(
                store,
                draft.index.source_results(coverage),
            )));
            return Task::none();
        }
        let cache = self.cache.clone();
        let store = store.clone();
        let pending = folder::start_review(move |stopped| {
            let files = match cache.source_results(&scope, || {
                if stopped.load(Ordering::Relaxed) {
                    Err("Source inventory changed.".into())
                } else {
                    Ok(())
                }
            }) {
                Ok(files) => files,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(_) => return Ok(BTreeMap::new()),
            };
            Ok(group_source_results(&store, files.into_iter()))
        });
        let Ok((reply, cancel)) = pending else {
            return Task::none();
        };
        self.source_results.pending = Some(cancel);
        let ticket = self.source_results.ticket;
        Task::perform(
            async move {
                Arc::new(
                    reply
                        .recv()
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or_default(),
                )
            },
            move |rows| Message::SourceResults(ticket, rows),
        )
    }

    pub(super) fn knowledge_source_picker_view<'a>(
        &'a self,
        picker: &'a KnowledgeSourcePicker,
        editable: bool,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let (body, action) = match picker {
            KnowledgeSourcePicker::ChooseInitial(_) | KnowledgeSourcePicker::ReviewFiles { .. } => {
                return container(space::vertical()).into();
            }
            KnowledgeSourcePicker::Native { kind, .. } => (
                text(kind.to_string()).size(19).height(Length::Fill).into(),
                None,
            ),
            KnowledgeSourcePicker::Retry(kind) => (
                column![
                    text("Review knowledge sources").size(19),
                    dialog_design::secondary_action("Choose again…").on_press_maybe(
                        editable.then_some(Message::PickNativeKnowledgeSources(*kind))
                    ),
                ]
                .spacing(10)
                .height(Length::Fill)
                .into(),
                Some(
                    dialog_design::secondary_action("Browse in AI Manager")
                        .on_press_maybe(editable.then_some(Message::BrowseKnowledgeSources(*kind))),
                ),
            ),
            KnowledgeSourcePicker::Browse(chooser) => (
                chooser.view(!editable).map(Message::KnowledgeSourceBrowse),
                Some(
                    dialog_design::secondary_action(if chooser.chooses_folder() {
                        "Review folder"
                    } else {
                        "Review selection"
                    })
                    .on_press_maybe(
                        (editable
                            && (chooser.chosen_folder().is_some()
                                || !chooser.selection().is_empty()))
                        .then_some(Message::UseBrowsedKnowledgeSources),
                    ),
                ),
            ),
            KnowledgeSourcePicker::ScanningFiles => (
                text("Review knowledge sources")
                    .size(19)
                    .height(Length::Fill)
                    .into(),
                None,
            ),
            KnowledgeSourcePicker::InitialFolder => (
                container(self.folder_review.as_ref().map_or_else(
                    || text("Review folder sources").size(19).into(),
                    |review| folder_review_view(review, self.folder_review_page, reveal),
                ))
                .height(Length::Fill)
                .into(),
                (editable && self.folder_review.is_none()).then(|| {
                    dialog_design::secondary_action("Choose sources again")
                        .on_press(Message::DiscardFolderReview)
                }),
            ),
        };
        column![
            body,
            self.status.as_deref().map(|status| text(status).size(13)),
            row![
                dialog_design::secondary_action("Cancel")
                    .on_press(Message::CloseKnowledgeSourcePicker),
                action
            ]
            .spacing(8),
        ]
        .spacing(10)
        .padding(12)
        .height(Length::Fill)
        .into()
    }

    pub(super) fn file_review_view<'a>(
        &'a self,
        review: &'a file_review::Review,
        page: usize,
        size_limit: &'a str,
        editable: bool,
    ) -> Element<'a, Message> {
        use file_review::Status;
        let can_edit = editable && !self.busy();
        let limit_changed = knowledge_options::file_size(size_limit) != Ok(review.file_limit);
        let mut formats = BTreeMap::new();
        let mut ready = 0;
        let mut bytes = 0;
        for entry in &review.rows {
            if let Status::Ready { bytes: size, .. } = entry.status {
                ready += 1;
                bytes += size;
                *formats.entry(source_format(&entry.path)).or_insert(0) += 1;
            }
        }
        let skipped = review.rows.len() - ready;
        let formats = formats
            .into_iter()
            .map(|(format, count)| format!("{format} {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let widths = [9, 2, 2, 3, 8];
        let pages = review.rows.len().div_ceil(SOURCE_REVIEW_PAGE_SIZE);
        let rows = review
            .rows
            .iter()
            .skip(page * SOURCE_REVIEW_PAGE_SIZE)
            .take(SOURCE_REVIEW_PAGE_SIZE)
            .map(|entry| {
                let (format, size, status, reason) = match entry.status {
                    Status::Ready { bytes, .. } => (
                        source_format(&entry.path),
                        format!("{bytes} B"),
                        "Ready",
                        "",
                    ),
                    Status::Skipped(reason) => ("—".into(), "—".into(), "Skipped", reason),
                };
                container(cells(
                    [
                        display_path(&entry.path).into_owned(),
                        format,
                        size,
                        status.into(),
                        reason.into(),
                    ],
                    widths,
                    false,
                ))
                .padding([5, 8])
                .into()
            });
        column![
            container(text("Only verified files can be added to the knowledge store.").size(15))
                .padding([12, 14])
                .width(Length::Fill)
                .style(dialog_design::header),
            container(
                column![
                    column![
                        text(format!("Usable files: {ready}")).size(13),
                        text(format!("Total size: {bytes} B")).size(13),
                        (!formats.is_empty()).then(|| text(format!("Formats: {formats}")).size(13)),
                        text(format!("Skipped: {skipped}")).size(13),
                    ]
                    .spacing(3),
                    inventory_with_height(
                        ["Path", "Format", "Size", "Status", "Reason"],
                        widths,
                        rows,
                        "No files",
                        Length::Fill
                    ),
                    (pages > 1).then(|| row![
                        dialog_design::secondary_action("Previous page")
                            .on_press_maybe((page > 0).then(|| Message::FileReviewPage(page - 1))),
                        text(format!("{} of {pages}", page + 1)).size(13),
                        dialog_design::secondary_action("Next page").on_press_maybe(
                            (page + 1 < pages).then_some(Message::FileReviewPage(page + 1))
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)),
                    (review.has_oversized_files() || review.file_limit.is_some()).then(|| column![
                        text("File size limit for this selection in bytes. A blank field uses 50 MiB per file.").size(13),
                        row![
                            text_input("Default (52428800 bytes)", size_limit)
                                .size(13)
                                .on_input_maybe(can_edit.then_some(Message::FileReviewLimit)),
                            dialog_design::secondary_action("Review again")
                                .on_press_maybe((can_edit && limit_changed).then_some(Message::ReviewWithFileLimit)),
                        ].spacing(8).align_y(Alignment::Center),
                    ].spacing(4)),
                    text("Indexing requires separate consent.").size(13),
                    self.status.as_deref().map(|status| text(status).size(13)),
                    row![
                        dialog_design::secondary_action("Choose again…").on_press_maybe(
                            can_edit
                                .then_some(Message::PickNativeKnowledgeSources(SourceKind::File))
                        ),
                        space::horizontal(),
                        button(text("Add ready files").size(13))
                            .padding([7, 18])
                            .on_press_maybe(
                                (can_edit && ready > 0 && !limit_changed)
                                    .then_some(Message::ApproveBrowsedKnowledgeFiles)
                            ),
                        dialog_design::secondary_action("Cancel")
                            .on_press(Message::CloseKnowledgeSourcePicker),
                    ]
                    .spacing(8),
                ]
                .spacing(8)
                .height(Length::Fill)
            )
            .padding(14)
            .height(Length::Fill),
        ]
        .height(Length::Fill)
        .into()
    }

    pub(super) fn store_inventory<'a>(
        &self,
        state: &'a State,
        height: f32,
    ) -> Element<'a, Message> {
        let rows = state.stores.iter().enumerate().map(|(index, store)| {
            let (_, model, managed) = state.embedding_fields(store);
            let embedding = managed
                .or_else(|| (!model.trim().is_empty()).then_some(model))
                .unwrap_or("—");
            let roles = [
                store.text_profile.is_some().then_some("Text"),
                store.coding_profile.is_some().then_some("Coding"),
                store.autonomous_enabled.then_some("Agent"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            inventory_row(
                [
                    store.name.clone(),
                    backend_label(store).into(),
                    embedding.into(),
                    roles,
                    (store.sources.len() + store.folders.len()).to_string(),
                ],
                [3, 2, 3, 2, 1],
                self.selected == Some(index),
                index % 2 != 0,
                Some(Message::SelectStore(index)),
            )
        });
        inventory_with_height(
            ["Name", "Type", "Embedding model", "Used for", "Sources"],
            [3, 2, 3, 2, 1],
            rows,
            "No knowledge stores",
            height,
        )
    }

    pub(super) fn source_inventory<'a>(
        &'a self,
        state: &'a State,
        selected: Option<(usize, &'a Store)>,
        height: f32,
    ) -> Element<'a, Message> {
        let widths = [30, 6, 12, 11, 11, 12, 13];
        let rows = selected.into_iter().flat_map(|(store_index, store)| {
            let results = self.source_results.rows(state, store);
            let automatic = self
                .active_automatic_sources
                .as_ref()
                .filter(|(name, _)| *name == store.name)
                .and_then(|(_, report)| report.rows(state, store));
            let saved_outcomes = self
                .source_results
                .matches(state, store)
                .map(|()| &self.source_results.outcomes);
            let last_automatic = self
                .automatic_status
                .get(&store.name)
                .and_then(|status| status.sources.as_ref())
                .and_then(|report| report.rows(state, store));
            let work = self
                .source_work
                .as_ref()
                .filter(|work| work.store == *store);
            let review = self
                .approved_update
                .as_ref()
                .filter(|approved| approved.definition == *store)
                .map(|approved| approved.review.as_ref());
            store
                .sources
                .iter()
                .map(|path| (SourceKind::File, path, store.file_sync(path), 1, None))
                .chain(store.folders.iter().map(|folder| {
                    (
                        SourceKind::Folder,
                        &folder.root,
                        folder.options.sync,
                        folder.len(),
                        folder.review_problems,
                    )
                }))
                .enumerate()
                .map(
                    move |(row, (kind, path, mode, expected_files, folder_problems))| {
                        let key = (kind, path.clone());
                        let manual = work.and_then(|work| work.status(&key));
                        let automatic = if manual.is_none() && mode == SyncMode::Automatic {
                            automatic.and_then(|rows| rows.get(&key)).copied()
                        } else {
                            None
                        };
                        let result = results.and_then(|results| results.get(&key));
                        let saved = saved_outcomes
                            .and_then(|rows| rows.get(&key))
                            .copied()
                            .or_else(|| last_automatic.and_then(|rows| rows.get(&key)).copied());
                        let updated = result
                            .filter(|result| result.indexed_at != 0)
                            .and_then(|result| jiff::Timestamp::from_second(result.indexed_at).ok())
                            .map(|timestamp| {
                                timestamp
                                    .to_zoned(jiff::tz::TimeZone::system())
                                    .strftime("%Y-%m-%d %H:%M")
                                    .to_string()
                            });
                        let reviewed_problems = if review.is_some_and(|review| {
                            review.issues.iter().any(|issue| issue.source == key)
                        }) {
                            Some(1)
                        } else {
                            folder_problems.or_else(|| {
                                review
                                    .and_then(|review| {
                                        review.files.iter().find(|row| row.path == *path)
                                    })
                                    .map(|row| {
                                        usize::from(!matches!(
                                            row.status,
                                            file_review::Status::Ready { .. }
                                        ))
                                    })
                                    .or_else(|| {
                                        (kind == SourceKind::File
                                            && result.is_some_and(|result| result.files > 0))
                                        .then_some(0)
                                    })
                            })
                        };
                        let problems = automatic
                            .and_then(SourceStatus::problems)
                            .or_else(|| saved.and_then(SourceStatus::problems))
                            .or(reviewed_problems);
                        let status = if !store.source_enabled(kind, path) {
                            "Disabled"
                        } else if let Some(status) = manual
                            .or_else(|| automatic.and_then(SourceStatus::label))
                            .or_else(|| saved.and_then(SourceStatus::label))
                        {
                            status
                        } else if problems.is_some_and(|count| count != 0) {
                            "Warning"
                        } else {
                            match result {
                                Some(result)
                                    if result.files > 0 && result.files >= expected_files =>
                                {
                                    "Ready"
                                }
                                Some(result) if result.files > 0 => "Partially indexed",
                                Some(_) => "Pending",
                                None => "—",
                            }
                        };
                        inventory_row(
                            [
                                display_path(path).into_owned(),
                                match kind {
                                    SourceKind::File if crate::knowledge_pdf::is_pdf(path) => "PDF",
                                    SourceKind::File => "File",
                                    SourceKind::Folder => "Folder",
                                }
                                .into(),
                                mode.to_string(),
                                status.into(),
                                result.map_or_else(
                                    || "—".into(),
                                    |result| format!("{} / {}", result.files, result.chunks),
                                ),
                                if work.is_some_and(|work| work.failed.as_ref() == Some(&key)) {
                                    "1".into()
                                } else {
                                    problems.map_or_else(|| "—".into(), |count| count.to_string())
                                },
                                updated.unwrap_or_else(|| "—".into()),
                            ],
                            widths,
                            self.source_selected(kind, path),
                            row % 2 != 0,
                            Some(Message::SelectKnowledgeSource(
                                store_index,
                                kind,
                                path.clone(),
                            )),
                        )
                    },
                )
        });
        let placeholder = if selected.is_none() {
            "Select a knowledge store"
        } else {
            "No sources in this store"
        };
        inventory_with_height(
            [
                "Path",
                "Type",
                "Synchronization",
                "Status",
                "Files / chunks",
                "Skipped / failed",
                "Last indexed",
            ],
            widths,
            rows,
            placeholder,
            height,
        )
    }

    pub(super) fn source_actions<'a>(
        &'a self,
        selected: Option<(usize, &'a Store)>,
        editable: bool,
    ) -> [Element<'a, Message>; 4] {
        let properties = selected.filter(|_| editable && self.selected_sources.len() == 1);
        let actions = properties.and_then(|(index, store)| {
            let (kind, path) = self.selected_sources.first()?;
            let advanced = match kind {
                SourceKind::File if store.sources.contains(path) => {
                    Message::ConfigureFileSize(index, path.clone())
                }
                SourceKind::Folder => {
                    let position = store
                        .folders
                        .iter()
                        .position(|folder| folder.root == *path)?;
                    Message::ConfigureFolder(position)
                }
                _ => return None,
            };
            let toggle = Message::SourceEnabled {
                store: index,
                kind: *kind,
                path: path.clone(),
                enabled: !store.source_enabled(*kind, path),
            };
            Some((Message::OpenSourceSync(index), advanced, toggle))
        });
        let (mode, advanced, toggle) = actions
            .map_or((None, None, None), |(mode, advanced, toggle)| {
                (Some(mode), Some(advanced), Some(toggle))
            });
        let remove = selected
            .filter(|_| {
                editable && !self.selected_sources.is_empty() && self.source_removal.is_none()
            })
            .map(|(index, _)| Message::ReviewSourceRemoval(index));
        [
            dialog_design::secondary_action("Sync mode…")
                .on_press_maybe(mode)
                .into(),
            dialog_design::secondary_action("Advanced…")
                .on_press_maybe(advanced)
                .into(),
            dialog_design::secondary_action("Enable / disable")
                .on_press_maybe(toggle)
                .into(),
            dialog_design::secondary_action("Remove…")
                .on_press_maybe(remove)
                .into(),
        ]
    }

    pub(super) fn source_sync_edit(&self, state: &State, index: usize) -> Option<SyncEdit> {
        let store = state.stores.get(index)?;
        if self.selected_sources.len() != 1 {
            return None;
        }
        let source = self.selected_sources.first()?;
        let (kind, path) = source;
        let mode = match kind {
            SourceKind::File if store.sources.contains(path) => store.file_sync(path),
            SourceKind::Folder => {
                store
                    .folders
                    .iter()
                    .find(|folder| folder.root == *path)?
                    .options
                    .sync
            }
            _ => return None,
        };
        Some(SyncEdit {
            store: index,
            source: source.clone(),
            mode,
        })
    }

    pub(super) fn source_sync_view(&self, editable: bool) -> Option<Element<'_, Message>> {
        let edit = self.source_sync_editor.as_ref()?;
        Some(
            column![
                container(
                    text("Choose automatic monitoring or manual updates for this source.").size(15)
                )
                .width(Length::Fill)
                .padding([14, 18])
                .style(dialog_design::header),
                container(
                    pick_list(SyncMode::ALL, Some(edit.mode), Message::SourceSyncMode)
                        .width(Length::Fill)
                        .text_size(13)
                        .dismiss_on_escape()
                )
                .padding([8, 18]),
                container(
                    row![
                        button(text("OK").size(13))
                            .padding([6, 24])
                            .on_press_maybe(editable.then_some(Message::ApplySourceSync)),
                        dialog_design::secondary_action("Cancel")
                            .on_press(Message::CancelSourceSync),
                    ]
                    .spacing(8)
                )
                .padding([8, 18])
                .align_right(Length::Fill),
            ]
            .into(),
        )
    }

    pub(super) fn source_update_pages(&self) -> usize {
        self.source_updates.as_ref().map_or(0, |updates| {
            (updates.files.len()
                + updates.issues.len()
                + updates
                    .folders
                    .iter()
                    .map(|review| review.rows.len().max(1))
                    .sum::<usize>())
            .div_ceil(SOURCE_REVIEW_PAGE_SIZE)
        })
    }

    pub(super) fn source_updates_view(&self, editable: bool) -> Option<Element<'_, Message>> {
        use folder::Status;

        let updates = self.source_updates.as_ref()?;
        let mut counts = [0, updates.issues.len(), 0];
        for entry in &updates.files {
            match entry.status {
                file_review::Status::Ready { .. } => counts[0] += 1,
                file_review::Status::Skipped(_) => counts[1] += 1,
            }
        }
        for entry in updates.folders.iter().flat_map(|review| &review.rows) {
            match entry.status {
                Status::Ready { .. } => counts[0] += 1,
                Status::Skipped(_) => counts[1] += 1,
                Status::Removed => counts[2] += 1,
            }
        }
        let [ready, skipped, removed] = counts;
        let file_count = updates.files.len();
        let folder_count = updates.folders.len() + updates.issues.len();
        let file_label = if file_count == 1 { "file" } else { "files" };
        let folder_label = if folder_count == 1 {
            "folder"
        } else {
            "folders"
        };
        let widths = [8, 2, 2, 3, 4];
        let page = self.folder_review_page;
        let pages = self.source_update_pages();
        let files = updates.files.iter().map(|row| {
            let (size, status, reason) = match row.status {
                file_review::Status::Ready { bytes, .. } => (format!("{bytes} B"), "Ready", ""),
                file_review::Status::Skipped(reason) => ("—".into(), "Skipped", reason),
            };
            container(cells(
                [
                    display_path(&row.path).into_owned(),
                    source_format(&row.path),
                    size,
                    status.into(),
                    reason.into(),
                ],
                widths,
                false,
            ))
            .padding([5, 8])
            .into()
        });
        let folders = updates.folders.iter().flat_map(|review| {
            let root = &review.folder.root;
            review
                .rows
                .iter()
                .map(Some)
                .chain(review.rows.is_empty().then_some(None))
                .map(move |entry| {
                    let values = if let Some(entry) = entry {
                        let (size, status, reason) = match entry.status {
                            Status::Ready {
                                bytes,
                                change: "Unchanged",
                            } => (format!("{bytes} B"), "Unchanged", ""),
                            Status::Ready { bytes, change } => {
                                (format!("{bytes} B"), "Will be indexed", change)
                            }
                            Status::Skipped(reason) => ("—".into(), "Skipped", reason),
                            Status::Removed => ("—".into(), "Removed", "Missing or excluded"),
                        };
                        [
                            display_path(&root.join(&entry.path)).into_owned(),
                            source_format(Path::new(&entry.path)),
                            size,
                            status.into(),
                            reason.into(),
                        ]
                    } else {
                        [
                            display_path(root).into_owned(),
                            "Folder".into(),
                            "—".into(),
                            if review.missing {
                                "Missing".into()
                            } else {
                                "Empty".into()
                            },
                            "No eligible files".into(),
                        ]
                    };
                    container(cells(values, widths, false))
                        .padding([5, 8])
                        .into()
                })
        });
        let issues = updates.issues.iter().map(|issue| {
            container(cells(
                [
                    display_path(&issue.source.1).into_owned(),
                    "Folder".into(),
                    "—".into(),
                    "Skipped".into(),
                    issue.message.clone(),
                ],
                widths,
                false,
            ))
            .padding([5, 8])
            .into()
        });
        let rows = files
            .chain(folders)
            .chain(issues)
            .skip(page * SOURCE_REVIEW_PAGE_SIZE)
            .take(SOURCE_REVIEW_PAGE_SIZE);
        Some(column![
            container(text("Review the files before updating the knowledge store.").size(15))
                .padding([10, 14]).width(Length::Fill).style(dialog_design::header),
            container(column![
                text(format!("{file_count} individual {file_label} · {folder_count} {folder_label} · {ready} ready · {skipped} skipped · {removed} removed")).size(13),
                inventory_with_height(["Path", "Format", "Size", "Status", "Reason"], widths, rows,
                    "No reviewed files", Length::Fill),
                (pages > 1).then(|| row![
                    dialog_design::secondary_action("Previous page").on_press_maybe(
                        (page > 0).then(|| Message::FolderReviewPage(page - 1))),
                    text(format!("{} of {pages}", page + 1)).size(13),
                    dialog_design::secondary_action("Next page").on_press_maybe(
                        (page + 1 < pages).then_some(Message::FolderReviewPage(page + 1))),
                ].spacing(8).align_y(Alignment::Center)),
                text("Approval stages the reviewed sources. Indexing requires separate consent.").size(13),
                row![space::horizontal(),
                    button(text("Approve sources").size(13)).padding([7,18])
                        .on_press_maybe((editable && !self.busy() && updates.can_approve()).then_some(Message::ApproveSourceUpdates)),
                    dialog_design::secondary_action("Cancel").on_press(Message::CancelSourceUpdates),
                ].spacing(8),
            ].spacing(8).height(Length::Fill)).padding(14).height(Length::Fill),
        ].height(Length::Fill).into())
    }

    pub(in crate::ai_manager) fn source_removal_height(&self) -> Option<f32> {
        self.source_removal
            .as_ref()
            .map(|review| 156.0 + review.sources.len().min(4) as f32 * 40.0)
    }

    pub(super) fn source_removal_review(&self, editable: bool) -> Option<Element<'_, Message>> {
        let review = self.source_removal.as_ref()?;
        let paths = column(review.sources.iter().map(|(_, path)| {
            text(display_path(path).into_owned())
                .size(13)
                .width(Length::Fill)
                .wrapping(text::Wrapping::WordOrGlyph)
                .into()
        }))
        .spacing(5);
        let count = review.sources.len();
        let noun = if count == 1 { "source" } else { "sources" };
        Some(
            column![
                text(format!("Remove {count} {noun} from the store?")).size(15),
                container(scrollable(paths).width(Length::Fill).height(Length::Fill))
                    .padding([6, 8])
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(dialog_design::sidebar),
                text("Save AI Manager to apply the change. The files stay on disk.").size(13),
                row![
                    space::horizontal(),
                    button(text("Remove").size(13))
                        .width(76)
                        .padding([6, 10])
                        .on_press_maybe(
                            (editable && !self.busy()).then_some(Message::RemoveSelectedSources)
                        ),
                    dialog_design::secondary_action("Cancel")
                        .width(76)
                        .on_press(Message::CancelSourceRemoval),
                ]
                .spacing(8),
            ]
            .spacing(10)
            .padding(14)
            .height(Length::Fill)
            .into(),
        )
    }

    fn source_selected(&self, kind: SourceKind, path: &Path) -> bool {
        self.selected_sources.contains(&(kind, path.to_owned()))
    }
}

fn backend_label(store: &Store) -> &'static str {
    if store.qdrant.is_some() {
        "Qdrant"
    } else {
        "Local HNSW"
    }
}

fn source_format(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map_or_else(
            || {
                path.file_name()
                    .map_or_else(|| "Text".into(), |name| name.to_string_lossy().into_owned())
            },
            |extension| format!(".{}", extension.to_ascii_lowercase()),
        )
}

pub(super) fn cells<'a, const N: usize>(
    values: [String; N],
    widths: [u16; N],
    header: bool,
) -> Element<'a, Message> {
    inventory_cells(values, widths, header, std::array::from_fn(|_| None))
}

fn inventory_cells<'a, const N: usize>(
    values: [String; N],
    widths: [u16; N],
    header: bool,
    actions: [Option<Message>; N],
) -> Element<'a, Message> {
    let font = Font {
        weight: if header {
            iced::font::Weight::Bold
        } else {
            iced::font::Weight::Normal
        },
        ..Font::DEFAULT
    };
    let alignment = if header {
        Alignment::Center
    } else {
        Alignment::Start
    };
    let mut cells = row![].align_y(Alignment::Center);
    for (column, ((value, width), action)) in
        values.into_iter().zip(widths).zip(actions).enumerate()
    {
        if column != 0 {
            cells = cells.push(container(rule::vertical(1)).height(18));
        }
        let label = container(
            text(value.clone())
                .font(font)
                .size(13)
                .wrapping(text::Wrapping::None),
        )
        .width(Length::Fill)
        .align_x(alignment)
        .padding([0, 4])
        .clip(true);
        let label: Element<'_, Message> = if let Some(action) = action {
            button(label)
                .style(button::text)
                .padding(0)
                .width(Length::Fill)
                .on_press(action)
                .into()
        } else {
            label.into()
        };
        cells = cells.push(
            container(
                tooltip(
                    label,
                    container(text(value).size(13).wrapping(text::Wrapping::WordOrGlyph))
                        .max_width(620),
                    tooltip::Position::Bottom,
                )
                .gap(4)
                .delay(Duration::from_millis(500))
                .style(dialog_design::header),
            )
            .width(Length::FillPortion(width)),
        );
    }
    cells.into()
}

pub(super) fn inventory_row<'a, const N: usize>(
    values: [String; N],
    widths: [u16; N],
    selected: bool,
    alternate: bool,
    action: Option<Message>,
) -> Element<'a, Message> {
    button(cells(values, widths, false))
        .padding([5, 8])
        .width(Length::Fill)
        .style(move |theme, state| {
            let mut style = button::text(theme, state);
            let row = dialog_design::table_row(theme, selected, alternate);
            style.background = style.background.or(row.background);
            style.border.radius = 0.0.into();
            style
        })
        .on_press_maybe(action)
        .into()
}

pub(super) fn inventory_with_height<'a, const N: usize>(
    headers: [&str; N],
    widths: [u16; N],
    rows: impl Iterator<Item = Element<'a, Message>>,
    empty_label: &'static str,
    height: impl Into<Length>,
) -> Element<'a, Message> {
    inventory_with_header(
        cells(headers.map(str::to_owned), widths, true),
        rows,
        empty_label,
        height,
        None,
    )
}

pub(super) fn sortable_inventory<'a, const N: usize>(
    headers: [String; N],
    widths: [u16; N],
    actions: [Option<Message>; N],
    rows: impl Iterator<Item = Element<'a, Message>>,
    empty_label: &'static str,
    height: impl Into<Length>,
    body_id: iced::widget::Id,
) -> Element<'a, Message> {
    inventory_with_header(
        inventory_cells(headers, widths, true, actions),
        rows,
        empty_label,
        height,
        Some(body_id),
    )
}

fn inventory_with_header<'a>(
    header: Element<'a, Message>,
    rows: impl Iterator<Item = Element<'a, Message>>,
    empty_label: &'static str,
    height: impl Into<Length>,
    body_id: Option<iced::widget::Id>,
) -> Element<'a, Message> {
    let mut rows = rows.peekable();
    let body: Element<'_, Message> = if rows.peek().is_some() {
        let body = scrollable(column(rows)).height(Length::Fill);
        if let Some(id) = body_id {
            body.id(id).into()
        } else {
            body.into()
        }
    } else {
        container(text(empty_label).size(13))
            .center(Length::Fill)
            .into()
    };
    column![
        container(header)
            .padding([5, 8])
            .style(dialog_design::header),
        rule::horizontal(1),
        container(body)
            .width(Length::Fill)
            .height(height)
            .style(dialog_design::sidebar),
    ]
    .width(Length::Fill)
    .into()
}
