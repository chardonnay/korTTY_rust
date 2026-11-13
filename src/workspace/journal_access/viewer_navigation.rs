//! Search and marker navigation for the native journal timeline.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::time::{Duration, Instant};

use iced::widget::{self, button, checkbox, column, pick_list, row, text, text_input};
use iced::{Alignment, Element, Font, Task, window};
use zeroize::Zeroize;

use super::timeline_find::{Hit, Results, Terms, scan, step};
use super::{Desktop, Journal, MAX_QUERY, Message, PAGE_ROWS, Viewer, read_task};

pub(super) mod projection;
pub(super) use projection::Projection;
mod geometry;
mod refresh;

use refresh::{RefreshSearch, ScrollEvent};

const DEBOUNCE: Duration = Duration::from_millis(180);
const BASE_ROW_HEIGHT: f32 = 28.0;
const BASE_ROW_SPACING: f32 = 3.0;

#[derive(Clone, Debug)]
pub enum Event {
    OpenFind,
    CloseFind,
    Query(String),
    OnlyMatches(bool),
    MoveMatch(bool),
    Scanned(u64, Result<Arc<Results>, String>),
    Scrolled(ScrollEvent),
    OpenSelectedScreenshot,
    ToggleMarkers,
    CloseMarkers,
    MoveMarker(bool),
    Marker(Marker),
    Chat(usize, String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Marker {
    key: Option<String>,
    name: String,
}

impl fmt::Display for Marker {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.name)
    }
}

struct Scan {
    revision: u64,
    cancelled: Arc<AtomicBool>,
    wake: SyncSender<()>,
}

impl Scan {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        let _ = self.wake.try_send(());
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(super) struct State {
    find_open: bool,
    find_input: widget::Id,
    scroll_id: widget::Id,
    query: String,
    only_matches: bool,
    revision: u64,
    earliest: Instant,
    scanning: Option<Scan>,
    results: Option<Arc<Results>>,
    hit_index: Option<usize>,
    pending_reveal: bool,
    refresh_search: Option<RefreshSearch>,
    scale: u16,
    geometry_revision: u64,
    scroll: Option<ScrollEvent>,
    viewport_height: f32,
    restore_scroll_pending: bool,
    problem: Option<String>,
    markers_open: bool,
    markers: Vec<Marker>,
    marker_index: usize,
    marker_rows: Vec<usize>,
    marker_position: Option<usize>,
    chat_override: Option<(usize, bool)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            find_open: false,
            find_input: widget::Id::unique(),
            scroll_id: widget::Id::unique(),
            query: String::new(),
            only_matches: false,
            revision: 0,
            earliest: Instant::now(),
            scanning: None,
            results: None,
            hit_index: None,
            pending_reveal: false,
            refresh_search: None,
            scale: 100,
            geometry_revision: 0,
            scroll: None,
            viewport_height: 420.0,
            restore_scroll_pending: false,
            problem: None,
            markers_open: false,
            markers: Vec::new(),
            marker_index: 0,
            marker_rows: Vec::new(),
            marker_position: None,
            chat_override: None,
        }
    }
}

impl State {
    pub(super) fn row_height(&self) -> f32 {
        BASE_ROW_HEIGHT * f32::from(self.scale) / 100.0
    }

    pub(super) fn row_spacing(&self) -> f32 {
        BASE_ROW_SPACING * f32::from(self.scale) / 100.0
    }

    pub(super) fn find_open(&self) -> bool {
        self.find_open
    }

    pub(super) fn markers_open(&self) -> bool {
        self.markers_open
    }

    pub(super) fn input_id(&self) -> widget::Id {
        self.find_input.clone()
    }

    pub(super) fn scroll_id(&self) -> widget::Id {
        self.scroll_id.clone()
    }

    pub(super) fn has_markers(&self) -> bool {
        self.markers.len() > 1
    }

    pub(super) fn load_markers(&mut self, document: &Journal) {
        let mut used = HashSet::new();
        self.markers = std::iter::once(Marker {
            key: None,
            name: "All markers".into(),
        })
        .chain(document.timeline.iter().filter_map(|entry| {
            let key = entry.marker_key.as_ref()?;
            used.insert(key).then(|| Marker {
                key: Some(key.clone()),
                name: entry.marker_name.clone(),
            })
        }))
        .collect();
    }

    pub(super) fn filtered(&self) -> Option<&[usize]> {
        self.results
            .as_ref()
            .filter(|_| self.only_matches)
            .map(|result| result.matches.as_slice())
    }

    pub(super) fn hits(&self, index: usize) -> &[Hit] {
        self.results
            .as_ref()
            .map_or(&[], |result| result.hits_for_entry(index))
    }

    pub(super) fn active_hit(&self) -> Option<&Hit> {
        self.results.as_ref()?.hits.get(self.hit_index?)
    }

    pub(super) fn is_marker_target(&self, index: usize) -> bool {
        self.marker_position
            .and_then(|position| self.marker_rows.get(position))
            .copied()
            == Some(index)
    }

    pub(super) fn chat_open(&self, index: usize, first_field: usize) -> bool {
        self.chat_override
            .filter(|(entry, _)| *entry == index)
            .map_or_else(
                || self.hits(index).iter().any(|hit| hit.field >= first_field),
                |(_, open)| open,
            )
    }

    fn clear_scan(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        if let Some(scan) = &self.scanning {
            scan.cancel();
        }
        self.results = None;
        self.hit_index = None;
        self.pending_reveal = false;
        self.refresh_search = None;
        self.problem = None;
        self.chat_override = None;
    }

    fn start_scan(&mut self, id: window::Id, document: Arc<Journal>) -> Task<Desktop> {
        let terms = Terms::parse(&self.query);
        if !self.find_open || terms.is_empty() || self.scanning.is_some() || self.results.is_some()
        {
            return Task::none();
        }
        let revision = self.revision;
        let earliest = self.earliest;
        let cancelled = Arc::new(AtomicBool::new(false));
        let (wake, wait) = mpsc::sync_channel(1);
        self.scanning = Some(Scan {
            revision,
            cancelled: Arc::clone(&cancelled),
            wake,
        });
        read_task(
            id,
            move || {
                let _ = wait.recv_timeout(earliest.saturating_duration_since(Instant::now()));
                let rows = document
                    .timeline
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| (index, Projection::new(entry).into_fields()));
                scan(&terms, rows, &|| cancelled.load(Ordering::Relaxed))
            },
            move |result| Message::ViewerNavigation(Event::Scanned(revision, result)),
        )
    }

    fn rebuild_marker_rows(&mut self, document: &Journal) {
        let previous = self
            .marker_position
            .and_then(|position| self.marker_rows.get(position))
            .copied();
        self.marker_rows.clear();
        if self.markers_open {
            let key = self
                .markers
                .get(self.marker_index)
                .and_then(|marker| marker.key.as_deref());
            let filtered = self.results.as_ref().filter(|_| self.only_matches);
            self.marker_rows
                .extend(
                    document
                        .timeline
                        .iter()
                        .enumerate()
                        .filter_map(|(index, entry)| {
                            let marker = entry.marker_key.as_deref()?;
                            (key.is_none_or(|key| key == marker)
                                && filtered.is_none_or(|result| {
                                    result.matches.binary_search(&index).is_ok()
                                }))
                            .then_some(index)
                        }),
                );
        }
        self.marker_position = previous
            .and_then(|index| self.marker_rows.binary_search(&index).ok())
            .or_else(|| (!self.marker_rows.is_empty()).then_some(0));
    }

    pub(super) fn replace_query(&self) -> Option<&str> {
        (self.find_open && !self.query.is_empty()).then_some(&self.query)
    }

    pub(super) fn reset_after_replace(&mut self) {
        self.query.zeroize();
        self.clear_scan();
        self.marker_rows.clear();
        self.marker_position = None;
    }

    pub(super) fn find_view(&self, font: Font, scale: u16, enabled: bool) -> Element<'_, Message> {
        let scale = f32::from(scale) / 100.0;
        let control_size = 16.0 * scale;
        let caption_size = 13.0 * scale;
        let control = |label| text(label).font(font).size(control_size);
        let found = self.results.as_ref().map(|results| {
            format!(
                "{}/{} · {} of {} entries",
                self.hit_index.map_or(0, |index| index + 1),
                results.hits.len(),
                results.matches.len(),
                results.total_entries
            )
        });
        let status = self
            .problem
            .as_deref()
            .or(found.as_deref())
            .unwrap_or_else(|| {
                if self.scanning.is_some() && !self.query.is_empty() {
                    "Searching…"
                } else {
                    ""
                }
            });
        let has_hits = self.hit_index.is_some();
        column![
            row![
                text_input("Search journal", &self.query)
                    .id(self.input_id())
                    .font(font)
                    .size(control_size)
                    .on_input(|value| message(Event::Query(value))),
                button(control("Previous match"))
                    .on_press_maybe(has_hits.then(|| message(Event::MoveMatch(false)))),
                button(control("Next match"))
                    .on_press_maybe(has_hits.then(|| message(Event::MoveMatch(true)))),
                button(control("Replace…")).on_press_maybe(
                    (enabled && self.replace_query().is_some())
                        .then_some(Message::Replace(super::replace_dialog::Event::FromSearch))
                ),
                button(control("Close search")).on_press(message(Event::CloseFind)),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .wrap(),
            row![
                checkbox(self.only_matches)
                    .label("Only matches")
                    .font(font)
                    .size(control_size)
                    .text_size(control_size)
                    .on_toggle(|value| message(Event::OnlyMatches(value))),
                text(status.to_owned()).font(font).size(caption_size),
                self.results
                    .as_ref()
                    .filter(|results| results.hits_limited)
                    .map(|_| text("Highlight limit reached")
                        .font(font)
                        .size(caption_size)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
        ]
        .spacing(4)
        .into()
    }

    pub(super) fn marker_view(&self, font: Font, scale: u16) -> Element<'_, Message> {
        let scale = f32::from(scale) / 100.0;
        let control_size = 16.0 * scale;
        let control = |label| text(label).font(font).size(control_size);
        let has_rows = !self.marker_rows.is_empty();
        row![
            super::viewer_keys::marker_picker(
                pick_list(
                    self.markers.as_slice(),
                    self.markers.get(self.marker_index),
                    |marker| message(Event::Marker(marker))
                )
                .font(font)
                .text_size(control_size)
                .into()
            ),
            button(control("Previous marked entry"))
                .on_press_maybe(has_rows.then(|| message(Event::MoveMarker(false)))),
            button(control("Next marked entry"))
                .on_press_maybe(has_rows.then(|| message(Event::MoveMarker(true)))),
            text(format!(
                "{}/{}",
                self.marker_position.map_or(0, |index| index + 1),
                self.marker_rows.len()
            ))
            .font(font)
            .size(13.0 * scale),
            button(control("Close markers")).on_press(message(Event::CloseMarkers)),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .wrap()
        .into()
    }
}

impl Viewer {
    pub(super) fn can_navigate_timeline(&self) -> bool {
        self.document.is_some()
            && !self.loading
            && !self.capture_shown
            && self.screenshot.is_none()
            && self.export_range.is_none()
    }

    pub(super) fn update_navigation(
        &mut self,
        id: window::Id,
        event: Event,
        interactive: bool,
    ) -> Task<Desktop> {
        let Some(document) = self.document.clone() else {
            return Task::none();
        };
        let enabled = interactive && self.can_navigate_timeline() && !self.title_menu;
        self.thumbnail_menu = None;
        let navigation = &mut self.navigation;
        if let Event::Scanned(revision, result) = event {
            if !navigation
                .scanning
                .as_ref()
                .is_some_and(|scan| scan.revision == revision)
            {
                return Task::none();
            }
            navigation.scanning = None;
            if revision != navigation.revision {
                return navigation.start_scan(id, document);
            }
            let refresh = navigation.refresh_search.take();
            match result {
                Ok(result) => {
                    let viewport = refresh
                        .as_ref()
                        .map(|_| navigation.viewport(self.timeline_page, &document));
                    navigation.hit_index = refresh
                        .and_then(|refresh| refresh.hit_index(&result))
                        .or_else(|| (!result.hits.is_empty()).then_some(0));
                    navigation.results = Some(result);
                    navigation.rebuild_marker_rows(&document);
                    if let Some(viewport) = viewport {
                        return self.restore_viewport(viewport, enabled);
                    }
                    navigation.pending_reveal = true;
                    return self.resume_timeline_navigation(enabled);
                }
                Err(error) => navigation.problem = Some(error),
            }
            return Task::none();
        }
        if !enabled {
            return Task::none();
        }
        let target = match event {
            Event::OpenSelectedScreenshot if !self.entry_flow.saving() => {
                let Some(index) = self.selected_entry else {
                    return Task::none();
                };
                let Some(entry) = document.timeline.get(index) else {
                    return Task::none();
                };
                return self.show_screenshot(id, index, entry.id.clone());
            }
            Event::Scrolled(event) => {
                navigation.record_scroll(self.timeline_page, event);
                return Task::none();
            }
            Event::OpenFind => {
                navigation.find_open = true;
                let id = navigation.input_id();
                return widget::operation::focus(id.clone())
                    .chain(widget::operation::select_all(id));
            }
            Event::CloseFind if navigation.find_open => {
                navigation.find_open = false;
                navigation.query.clear();
                navigation.clear_scan();
                navigation.rebuild_marker_rows(&document);
                self.selected_entry
            }
            Event::Query(mut query) if navigation.find_open => {
                query.truncate(query.floor_char_boundary(MAX_QUERY));
                if query == navigation.query {
                    return Task::none();
                }
                navigation.query = query;
                navigation.clear_scan();
                navigation.earliest = Instant::now() + DEBOUNCE;
                navigation.rebuild_marker_rows(&document);
                let scan = navigation.start_scan(id, document);
                return Task::batch([scan, self.reveal_timeline(self.selected_entry, true)]);
            }
            Event::OnlyMatches(value) if navigation.find_open => {
                navigation.only_matches = value;
                navigation.hit_index = navigation
                    .results
                    .as_ref()
                    .and_then(|results| (!results.hits.is_empty()).then_some(0));
                navigation.chat_override = None;
                navigation.rebuild_marker_rows(&document);
                navigation
                    .active_hit()
                    .map(|hit| hit.entry)
                    .or(self.selected_entry)
            }
            Event::MoveMatch(forward) if navigation.find_open => {
                if navigation.results.is_none() {
                    navigation.earliest = Instant::now();
                    if let Some(scan) = &navigation.scanning {
                        let _ = scan.wake.try_send(());
                    }
                    return navigation.start_scan(id, document);
                }
                if let Some((index, results)) =
                    navigation.hit_index.zip(navigation.results.as_ref())
                {
                    navigation.hit_index = step(index, results.hits.len(), forward);
                    navigation.chat_override = None;
                }
                navigation.active_hit().map(|hit| hit.entry)
            }
            Event::ToggleMarkers if navigation.has_markers() => {
                navigation.markers_open = !navigation.markers_open;
                navigation.rebuild_marker_rows(&document);
                navigation.marker_rows.first().copied()
            }
            Event::CloseMarkers if navigation.markers_open => {
                navigation.markers_open = false;
                navigation.rebuild_marker_rows(&document);
                return Task::none();
            }
            Event::MoveMarker(forward) if navigation.has_markers() => {
                if navigation.markers_open {
                    navigation.marker_position = navigation
                        .marker_position
                        .and_then(|position| step(position, navigation.marker_rows.len(), forward));
                } else {
                    navigation.markers_open = true;
                    navigation.rebuild_marker_rows(&document);
                }
                navigation
                    .marker_position
                    .and_then(|position| navigation.marker_rows.get(position))
                    .copied()
            }
            Event::Marker(marker) if navigation.markers_open => {
                let Some(index) = navigation
                    .markers
                    .iter()
                    .position(|candidate| *candidate == marker)
                else {
                    return Task::none();
                };
                navigation.marker_index = index;
                navigation.marker_position = None;
                navigation.rebuild_marker_rows(&document);
                navigation.marker_rows.first().copied()
            }
            Event::Chat(index, id) if self.selected_entry == Some(index) => {
                let Some(entry) = document.timeline.get(index).filter(|entry| entry.id == id)
                else {
                    return Task::none();
                };
                let first = Projection::new(entry).first_chat_field();
                navigation.chat_override = Some((index, !navigation.chat_open(index, first)));
                return Task::none();
            }
            _ => return Task::none(),
        };
        self.reveal_timeline(target, true)
    }

    pub(super) fn resume_timeline_navigation(&mut self, scroll: bool) -> Task<Desktop> {
        if !self.can_navigate_timeline() {
            return Task::none();
        }
        if std::mem::take(&mut self.navigation.pending_reveal) {
            return self.reveal_timeline(self.navigation.active_hit().map(|hit| hit.entry), scroll);
        }
        if scroll && self.navigation.restore_scroll_pending {
            return self.navigation.restore_scroll(self.timeline_page);
        }
        Task::none()
    }

    pub(super) fn reveal_timeline(&mut self, target: Option<usize>, scroll: bool) -> Task<Desktop> {
        let target = if let Some(rows) = self.navigation.filtered() {
            target
                .and_then(|entry| rows.binary_search(&entry).ok().map(|row| (entry, row)))
                .or_else(|| rows.first().map(|&entry| (entry, 0)))
        } else {
            target.map(|entry| (entry, entry))
        };
        if let Some((entry, row)) = target {
            self.selected_entry = Some(entry);
            self.timeline_page = row / PAGE_ROWS;
            if scroll {
                let offset = self.document.as_ref().map_or(0.0, |document| {
                    let first = self.timeline_page * PAGE_ROWS;
                    self.navigation
                        .page_layout(self.timeline_page, document)
                        .row_top(row.saturating_sub(1).max(first))
                });
                return self.navigation.scroll_to(self.timeline_page, offset);
            }
        } else if self.navigation.filtered().is_some() {
            self.selected_entry = None;
            self.timeline_page = 0;
        }
        Task::none()
    }

    pub(super) fn reveal_question_entry(&mut self, entry: &str) -> Task<Desktop> {
        let Some(document) = &self.document else {
            return Task::none();
        };
        let mut matches = document
            .timeline
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.id == entry);
        let Some(index) = matches
            .next()
            .filter(|_| matches.next().is_none())
            .map(|(index, _)| index)
        else {
            self.error = Some("The cited journal entry is no longer available.".into());
            return Task::none();
        };
        self.navigation.find_open = false;
        self.navigation.query.clear();
        self.navigation.clear_scan();
        self.navigation.rebuild_marker_rows(document);
        self.capture_shown = false;
        self.reveal_timeline(Some(index), true)
    }
}

pub(super) fn message(event: Event) -> Message {
    Message::ViewerNavigation(event)
}

#[cfg(test)]
mod tests;
