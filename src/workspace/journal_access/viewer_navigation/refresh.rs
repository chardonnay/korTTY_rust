//! Rebinds native viewer navigation after a verified refresh of the same journal.

use std::sync::Arc;
use std::time::Instant;

use iced::widget::{operation, scrollable};
use iced::{Task, window};

use super::{
    Desktop, Event, Hit, Journal, Message, PAGE_ROWS, Projection, Results, State, Viewer, message,
};

#[derive(Clone, Copy, Debug)]
pub struct ScrollEvent {
    revision: u64,
    geometry_revision: u64,
    page: usize,
    filtered: bool,
    offset: f32,
    height: f32,
}

pub(super) struct ViewportAnchor {
    entry: Option<usize>,
    page: usize,
    offset: f32,
    fraction: f32,
}

impl State {
    pub(in crate::workspace::journal_access) fn scroll_event(
        &self,
        page: usize,
        viewport: scrollable::Viewport,
    ) -> Message {
        let mut event = self.scroll_stamp(page, viewport.absolute_offset().y);
        event.height = viewport.bounds().height;
        message(Event::Scrolled(event))
    }

    pub(super) fn scroll_stamp(&self, page: usize, offset: f32) -> ScrollEvent {
        ScrollEvent {
            revision: self.revision,
            geometry_revision: self.geometry_revision,
            page,
            filtered: self.filtered().is_some(),
            offset,
            height: self.viewport_height,
        }
    }

    pub(super) fn record_scroll(&mut self, page: usize, event: ScrollEvent) {
        if self.current_scroll(page, event) && event.offset.is_finite() && event.offset >= 0.0 {
            if event.height.is_finite() && event.height > 0.0 {
                self.viewport_height = event.height;
            }
            self.scroll = Some(event);
        }
    }

    fn current_scroll(&self, page: usize, event: ScrollEvent) -> bool {
        event.revision == self.revision
            && event.geometry_revision == self.geometry_revision
            && event.page == page
            && event.filtered == self.filtered().is_some()
    }

    pub(in crate::workspace::journal_access) fn offset_at(&self, page: usize) -> f32 {
        self.scroll
            .filter(|event| self.current_scroll(page, *event))
            .map_or(0.0, |event| event.offset)
    }

    pub(in crate::workspace::journal_access) fn scroll_to(
        &mut self,
        page: usize,
        offset: f32,
    ) -> Task<Desktop> {
        self.scroll = Some(self.scroll_stamp(page, offset));
        self.restore_scroll_pending = false;
        operation::scroll_to(
            self.scroll_id(),
            scrollable::AbsoluteOffset { x: 0.0, y: offset },
        )
    }

    pub(super) fn restore_scroll(&mut self, page: usize) -> Task<Desktop> {
        self.scroll_to(page, self.offset_at(page))
    }

    fn remember_scroll(&mut self, page: usize, offset: f32) {
        self.scroll = Some(self.scroll_stamp(page, offset));
        self.restore_scroll_pending = true;
    }

    pub(super) fn viewport(&self, page: usize, document: &Journal) -> ViewportAnchor {
        let offset = self.offset_at(page);
        let anchor = self.page_layout(page, document).anchor(offset);
        let entry = anchor.map(|(row, _)| self.filtered().map_or(row, |rows| rows[row]));
        ViewportAnchor {
            entry,
            page,
            offset,
            fraction: anchor.map_or(0.0, |(_, fraction)| fraction),
        }
    }
}

pub(super) struct RefreshSearch {
    entry: Option<usize>,
    exact: Option<Hit>,
}

impl RefreshSearch {
    fn from_hit(hit: Option<&Hit>) -> Self {
        Self {
            entry: hit.map(|hit| hit.entry),
            exact: hit.cloned(),
        }
    }

    fn rebind(self, previous: &Journal, document: &Journal) -> Self {
        let entry = self
            .entry
            .and_then(|index| rebind_entry(previous, document, index));
        let exact = self.exact.zip(entry).and_then(|(mut hit, index)| {
            let old = Projection::new(previous.timeline.get(hit.entry)?);
            let new = Projection::new(document.timeline.get(index)?);
            let old_field = old.field_value(hit.field)?;
            if new.field_value(hit.field) != Some(old_field) {
                return None;
            }
            hit.entry = index;
            Some(hit)
        });
        Self { entry, exact }
    }

    pub(super) fn hit_index(self, results: &Results) -> Option<usize> {
        self.exact
            .as_ref()
            .and_then(|old| results.hits.iter().position(|hit| hit == old))
            .or_else(|| {
                let entry = self.entry?;
                results.hits.iter().position(|hit| hit.entry == entry)
            })
    }
}

impl Viewer {
    pub(in crate::workspace::journal_access) fn resize_timeline(
        &mut self,
        scale: u16,
    ) -> Task<Desktop> {
        let scale = scale.clamp(70, 250);
        let navigation = &mut self.navigation;
        if navigation.scale == scale {
            return Task::none();
        }
        let page = self.timeline_page;
        let anchor = self.document.as_ref().and_then(|document| {
            navigation
                .page_layout(page, document)
                .anchor(navigation.offset_at(page))
        });
        navigation.scale = scale;
        navigation.geometry_revision = navigation.geometry_revision.wrapping_add(1);
        let Some(document) = &self.document else {
            return Task::none();
        };
        // The projection is unchanged. Translate the row and its fractional inset without
        // invalidating the query, selection or marker cursor.
        let offset = anchor.map_or(0.0, |(row, fraction)| {
            navigation.page_layout(page, document).offset(row, fraction)
        });
        navigation.remember_scroll(page, offset);
        if self.can_navigate_timeline() {
            self.navigation.restore_scroll(page)
        } else {
            Task::none()
        }
    }

    /// Installs a fresh, source-verified timeline. Search answers from the old document are
    /// discarded, and only uniquely identified entries can carry selection across a reload.
    pub(in crate::workspace::journal_access) fn replace_timeline(
        &mut self,
        id: window::Id,
        document: Arc<Journal>,
    ) -> Task<Desktop> {
        self.thumbnail_menu = None;
        self.entry_editor.reanchor(&document);
        let Some(previous) = self.document.as_ref() else {
            self.navigation.load_markers(&document);
            self.document = Some(document);
            return Task::none();
        };
        let mut viewport = self.navigation.viewport(self.timeline_page, previous);
        let navigation = &mut self.navigation;
        let rebind = |index| rebind_entry(previous, &document, index);
        let marker_entry = navigation
            .marker_position
            .and_then(|position| navigation.marker_rows.get(position).copied())
            .and_then(rebind);
        let marker_key = navigation
            .markers
            .get(navigation.marker_index)
            .and_then(|marker| marker.key.clone());
        let chat = navigation.chat_override.or_else(|| {
            let index = self.selected_entry?;
            let entry = previous.timeline.get(index)?;
            if entry.chat_details.is_empty() {
                return None;
            }
            let first_chat = Projection::new(entry).first_chat_field();
            navigation
                .chat_open(index, first_chat)
                .then_some((index, true))
        });
        let refresh = navigation.refresh_search.take().or_else(|| {
            navigation
                .results
                .as_ref()
                .map(|_| RefreshSearch::from_hit(navigation.active_hit()))
        });
        navigation.clear_scan();
        navigation.refresh_search = refresh.map(|refresh| refresh.rebind(previous, &document));
        navigation.earliest = Instant::now();
        navigation.chat_override = chat.and_then(|(index, open)| {
            let index = rebind(index)?;
            (!document.timeline[index].chat_details.is_empty()).then_some((index, open))
        });
        navigation.load_markers(&document);
        navigation.marker_index = navigation
            .markers
            .iter()
            .position(|marker| marker.key == marker_key)
            .unwrap_or(0);
        navigation.markers_open &= navigation.has_markers();
        navigation.marker_position = None;
        navigation.rebuild_marker_rows(&document);
        if let Some(position) =
            marker_entry.and_then(|index| navigation.marker_rows.binary_search(&index).ok())
        {
            navigation.marker_position = Some(position);
        }
        self.selected_entry = self.selected_entry.and_then(rebind);
        viewport.entry = viewport.entry.and_then(rebind);
        self.document = Some(Arc::clone(&document));
        let scan = self.navigation.start_scan(id, document);
        Task::batch([scan, self.restore_viewport(viewport, true)])
    }

    pub(super) fn restore_viewport(
        &mut self,
        viewport: ViewportAnchor,
        scroll: bool,
    ) -> Task<Desktop> {
        let Some(document) = &self.document else {
            return Task::none();
        };
        let filtered = self.navigation.filtered();
        let row = |entry| {
            filtered.map_or_else(
                || (entry < document.timeline.len()).then_some(entry),
                |rows| rows.binary_search(&entry).ok(),
            )
        };
        let selected = self.selected_entry.and_then(row);
        if selected.is_none() {
            self.selected_entry = None;
        }
        let last_page = self.timeline_len(document).saturating_sub(1) / PAGE_ROWS;
        let anchor = viewport.entry.and_then(row);
        let page = anchor
            .or(selected)
            .map_or(viewport.page.min(last_page), |row| row / PAGE_ROWS);
        let layout = self.navigation.page_layout(page, document);
        let offset = if let Some(row) = anchor {
            layout.offset(row, viewport.fraction)
        } else if let Some(row) = selected {
            layout.row_top(row.saturating_sub(1).max(page * PAGE_ROWS))
        } else if page == viewport.page {
            viewport.offset
        } else {
            0.0
        };
        self.timeline_page = page;
        self.navigation.remember_scroll(page, offset);
        self.resume_timeline_navigation(scroll)
    }
}

fn rebind_entry(previous: &Journal, document: &Journal, index: usize) -> Option<usize> {
    let id = &previous.timeline.get(index)?.id;
    if id.trim().is_empty()
        || previous
            .timeline
            .iter()
            .enumerate()
            .any(|(row, entry)| row != index && entry.id == *id)
    {
        return None;
    }
    let mut matching = document
        .timeline
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.id == *id);
    let (index, _) = matching.next()?;
    matching.next().is_none().then_some(index)
}
