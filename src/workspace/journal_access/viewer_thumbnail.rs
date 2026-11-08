use iced::widget::{button, container, image, tooltip};
use iced::{ContentFit, Element, Length, Task, window};

use super::super::journal_thumbnail::{Key, Loaded};
use super::{
    Desktop, Message, Mode, PAGE_ROWS, TimelineEntry, Viewer, Workspace, screenshot_access,
};

impl Workspace {
    pub(super) fn integrate_viewer_thumbnails(
        &mut self,
        id: window::Id,
        loaded: &Loaded,
    ) -> Task<Desktop> {
        let interactive = self.viewer_navigation_interactive(id);
        let visible = self.journal_allowed() && self.visible_journal_surface_owner(id).is_some();
        let Some(Mode::Viewer(viewer)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        if !visible {
            viewer.screenshot_thumbnails.clear();
        }
        if viewer.screenshot_thumbnails.integrate(loaded) && interactive {
            return viewer.request_screenshot_thumbnails(id);
        }
        Task::none()
    }
}

impl Viewer {
    pub(super) fn invalidate_screenshot_thumbnail(&mut self, entry_id: &str) {
        self.screenshot_thumbnails.invalidate(entry_id);
    }

    pub(super) fn request_screenshot_thumbnails(&mut self, id: window::Id) -> Task<Desktop> {
        if !self.can_navigate_timeline() || self.entry_flow.saving() {
            return Task::none();
        }
        let Some(document) = self.document.as_ref() else {
            return Task::none();
        };
        let page = self.timeline_page;
        let range = self.navigation.page_layout(page, document).visible(
            self.navigation.offset_at(page),
            self.navigation.thumbnail_viewport_height(),
        );
        let filtered = self.navigation.filtered();
        let keys = range.filter_map(|row| {
            let index = filtered.map_or(row, |matches| matches[row]);
            Key::for_entry(&document.timeline[index])
        });
        self.screenshot_thumbnails
            .request(&self.source, keys, move |loaded| {
                Desktop::SessionJournal(id, Message::ScreenshotThumbnails(loaded))
            })
    }

    pub(super) fn thumbnail<'a>(
        &'a self,
        index: usize,
        entry: &'a TimelineEntry,
        enabled: bool,
        analysis: bool,
    ) -> Option<Element<'a, Message>> {
        let key = Key::for_entry(entry)?;
        let height = self.navigation.thumbnail_height();
        let preview: Element<'a, Message> = match self.screenshot_thumbnails.get(&key) {
            Some(handle) => image(handle.clone())
                .content_fit(ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => iced::widget::text("View screenshot").size(13).into(),
        };
        let action = button(preview)
            .style(button::secondary)
            .width(Length::Fill)
            .height(height)
            .on_press_maybe(enabled.then(|| {
                Message::OpenThumbnail(index, key.id().to_owned(), key.reference().to_owned())
            }));
        let trigger: Element<'a, Message> = tooltip(
            container(action).max_width(560),
            iced::widget::text("View screenshot"),
            tooltip::Position::Top,
        )
        .into();
        if !enabled {
            return Some(trigger);
        }
        let row = screenshot_access::Row::new(index, entry)?;
        let active = self.thumbnail_menu.as_ref().filter(|menu| menu.row == row);
        let request = row.clone();
        Some(crate::workspace::journal_image_menu::menu(
            trigger,
            active.map(|menu| (menu.point, menu.choice)),
            analysis,
            move |point| {
                Message::ScreenshotAction(screenshot_access::Event::ThumbnailMenu(
                    request.clone(),
                    point,
                ))
            },
            |choice| {
                Message::ScreenshotAction(screenshot_access::Event::ThumbnailAction(
                    row.clone(),
                    choice,
                ))
            },
            |forward| {
                Message::ScreenshotAction(screenshot_access::Event::StepThumbnailAction(
                    row.clone(),
                    forward,
                ))
            },
            Message::ScreenshotAction(screenshot_access::Event::DismissThumbnail),
        ))
    }

    pub(super) fn thumbnail_is_current(&self, index: usize, id: &str, reference: &str) -> bool {
        let Some(document) = &self.document else {
            return false;
        };
        let first = self.timeline_page * PAGE_ROWS;
        let Some(ordinal) = self
            .timeline_filter()
            .map_or(Some(index), |matches| matches.binary_search(&index).ok())
        else {
            return false;
        };
        (first..first + PAGE_ROWS).contains(&ordinal)
            && document.timeline.get(index).is_some_and(|entry| {
                entry.id == id && screenshot_access::reference(entry) == Some(reference)
            })
    }

    pub(super) fn has_selected_screenshot(&self) -> bool {
        self.selected_entry
            .and_then(|index| self.document.as_ref()?.timeline.get(index))
            .and_then(screenshot_access::reference)
            .is_some()
    }

    pub(super) fn show_screenshot(
        &mut self,
        id: window::Id,
        index: usize,
        entry_id: String,
    ) -> Task<Desktop> {
        let Some(reference) = self
            .document
            .as_ref()
            .and_then(|document| document.timeline.get(index))
            .filter(|entry| entry.id == entry_id)
            .and_then(screenshot_access::reference)
            .map(str::to_owned)
        else {
            return Task::none();
        };
        self.selected_entry = Some(index);
        self.thumbnail_menu = None;
        self.loading = true;
        self.error = None;
        self.page_revision = self.page_revision.wrapping_add(1);
        self.screenshot_request = Some(entry_id.clone());
        let revision = self.page_revision;
        let source = self.source.clone();
        super::read_task(
            id,
            move || super::screenshot_export::Displayed::load(&source, &entry_id, &reference),
            move |result| Message::ScreenshotLoaded(revision, result),
        )
    }
}
