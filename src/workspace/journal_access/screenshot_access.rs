//! Native context actions for the displayed PNG and screenshot entries.

use iced::widget::image;
use iced::{Element, Length, Point, Task, window};

use crate::workspace::journal_image_menu::{self, Choice};

use super::screenshot_export::{Displayed, Target};
use super::{Desktop, Message, Mode, TimelineEntry, ViewerSource, Workspace};

mod copy;
pub(in crate::workspace) use copy::Copies;
pub(crate) use copy::Prepared;

#[cfg(test)]
pub(super) mod tests;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Origin {
    Viewer(window::Id),
    Dock {
        owner: window::Id,
        tab: u64,
        run: u64,
    },
}

impl Origin {
    pub(super) fn window(self) -> window::Id {
        match self {
            Self::Viewer(id) | Self::Dock { owner: id, .. } => id,
        }
    }

    pub(super) fn owner(self, workspace: &Workspace) -> Option<window::Id> {
        match self {
            Self::Viewer(id) => workspace.visible_journal_surface_owner(id),
            Self::Dock { owner, .. } => workspace.windows.contains_key(&owner).then_some(owner),
        }
    }

    pub(super) fn is_current(
        self,
        workspace: &Workspace,
        owner: window::Id,
        bound: &ViewerSource,
    ) -> bool {
        if !workspace.journal_allowed() || workspace.exiting || workspace.scheduler_draining {
            return false;
        }
        match self {
            Self::Viewer(id) => {
                !workspace.hosted_close_locks(id)
                    && self.owner(workspace) == Some(owner)
                    && workspace.journals.get(&id).is_some_and(|surface| {
                        matches!(&surface.mode, Mode::Viewer(viewer) if viewer.source.same_journal(bound))
                    })
            }
            Self::Dock { owner, tab, run } => {
                workspace.dock_screenshot_source_current(owner, tab, run, bound)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    ImageMenu(u64, Point),
    ImageAction(u64, Choice),
    StepImageAction(u64, bool),
    DismissImage,
    ThumbnailMenu(Row, Point),
    ThumbnailAction(Row, Choice),
    StepThumbnailAction(Row, bool),
    DismissThumbnail,
    EntryMenu(Row, Point),
    EntryAction(Row, Choice),
    StepEntryAction(Row, bool),
    DoubleClick(Row),
    DismissEntry,
    CopyPrepared(u64, Result<std::sync::Arc<Prepared>, String>),
    CopyFinished(u64, Result<(), String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Row {
    index: usize,
    id: String,
    reference: String,
}

pub(super) struct ThumbnailMenu {
    pub(super) row: Row,
    pub(super) point: Point,
    pub(super) choice: Choice,
}

impl Row {
    pub(super) fn new(index: usize, entry: &TimelineEntry) -> Option<Self> {
        let reference = reference(entry)?.to_owned();
        Some(Self {
            index,
            id: entry.id.clone(),
            reference,
        })
    }

    pub(super) fn is_current(&self, document: &super::Journal) -> bool {
        document.timeline.get(self.index).is_some_and(|entry| {
            entry.id == self.id && reference(entry) == Some(self.reference.as_str())
        })
    }
}

pub(super) fn reference(entry: &TimelineEntry) -> Option<&str> {
    if entry.id.is_empty() || entry.kind != "SCREENSHOT" {
        return None;
    }
    entry.screenshot.as_deref().filter(|reference| {
        crate::session_journal::viewer::Screenshot::is_visible_reference(reference)
    })
}

pub(super) fn image_view(
    screenshot: &Displayed,
    revision: u64,
    active: Option<(Point, Choice)>,
    interactive: bool,
    analysis: bool,
) -> Element<'_, Message> {
    let image = image(screenshot.image.clone())
        .width(Length::Fill)
        .height(Length::Fill);
    if !interactive {
        return image.into();
    }
    journal_image_menu::menu(
        image.into(),
        active,
        analysis,
        move |point| Message::ScreenshotAction(Event::ImageMenu(revision, point)),
        |choice| Message::ScreenshotAction(Event::ImageAction(revision, choice)),
        |forward| Message::ScreenshotAction(Event::StepImageAction(revision, forward)),
        Message::ScreenshotAction(Event::DismissImage),
    )
}

impl Workspace {
    pub(super) fn update_journal_screenshot_access(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        let event = match event {
            Event::CopyPrepared(ticket, result) => {
                return self.screenshot_copy_prepared(ticket, result);
            }
            Event::CopyFinished(ticket, result) => {
                return self.screenshot_copy_finished(ticket, result);
            }
            event => event,
        };
        let interactive = self.viewer_navigation_interactive(id) && !self.hosted_close_locks(id);
        let analysis = self.journal_screenshot_analysis_available();
        let editor = matches!(
            &event,
            Event::EntryMenu(..)
                | Event::EntryAction(..)
                | Event::StepEntryAction(..)
                | Event::DoubleClick(..)
        );
        let editor_interactive = editor
            && self.entry_interactive(id)
            && !self
                .journal_appearance_page(super::AppearanceTarget::Viewer(id))
                .open;
        let thumbnail_interactive = interactive
            && !self
                .journal_appearance_page(super::AppearanceTarget::Viewer(id))
                .open;
        let Some(Mode::Viewer(viewer)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        let target = match event {
            Event::DismissImage => {
                viewer.screenshot_menu = None;
                None
            }
            Event::DismissEntry => {
                viewer.entry_editor.screenshot_menu = None;
                None
            }
            Event::DismissThumbnail => {
                viewer.thumbnail_menu = None;
                None
            }
            Event::ThumbnailMenu(row, point)
                if thumbnail_interactive
                    && viewer.can_navigate_timeline()
                    && !viewer.entry_flow.saving()
                    && viewer.thumbnail_is_current(row.index, &row.id, &row.reference) =>
            {
                viewer.entry_editor.screenshot_menu = None;
                viewer.thumbnail_menu = Some(ThumbnailMenu {
                    row,
                    point,
                    choice: Choice::default(),
                });
                None
            }
            Event::StepThumbnailAction(row, forward)
                if thumbnail_interactive
                    && viewer.can_navigate_timeline()
                    && !viewer.entry_flow.saving()
                    && viewer.thumbnail_is_current(row.index, &row.id, &row.reference) =>
            {
                if let Some(menu) = viewer
                    .thumbnail_menu
                    .as_mut()
                    .filter(|menu| menu.row == row)
                {
                    menu.choice = menu.choice.step(forward, analysis);
                }
                None
            }
            Event::ThumbnailAction(ref row, choice)
                if thumbnail_interactive
                    && viewer.can_navigate_timeline()
                    && !viewer.entry_flow.saving()
                    && viewer.thumbnail_is_current(row.index, &row.id, &row.reference)
                    && viewer
                        .thumbnail_menu
                        .as_ref()
                        .is_some_and(|menu| menu.row == *row) =>
            {
                viewer.thumbnail_menu = None;
                let target =
                    Target::row(viewer.source.clone(), row.id.clone(), row.reference.clone());
                (choice != Choice::Edit || !viewer.entry_editor.is_dirty())
                    .then_some((choice, target))
            }
            Event::ImageMenu(revision, point)
                if interactive
                    && revision == viewer.page_revision
                    && viewer.screenshot.is_some()
                    && !viewer.entry_flow.saving() =>
            {
                viewer.screenshot_menu = Some(point);
                viewer.screenshot_menu_choice = Choice::default();
                None
            }
            Event::StepImageAction(revision, forward)
                if interactive
                    && revision == viewer.page_revision
                    && viewer.screenshot_menu.is_some() =>
            {
                viewer.screenshot_menu_choice =
                    viewer.screenshot_menu_choice.step(forward, analysis);
                None
            }
            Event::ImageAction(revision, choice)
                if interactive
                    && revision == viewer.page_revision
                    && viewer.screenshot_menu.is_some()
                    && !viewer.entry_flow.saving() =>
            {
                viewer.screenshot_menu = None;
                viewer
                    .screenshot
                    .as_deref()
                    .filter(|_| choice != Choice::Edit || !viewer.entry_editor.is_dirty())
                    .map(|displayed| (choice, Target::displayed(displayed)))
            }
            Event::EntryMenu(row, point)
                if editor_interactive
                    && viewer.entry_editor.is_open()
                    && viewer.entry_flow.input_enabled() =>
            {
                if viewer
                    .document
                    .as_ref()
                    .is_some_and(|document| row.is_current(document))
                {
                    viewer.thumbnail_menu = None;
                    viewer.entry_editor.screenshot_menu = Some((row, point));
                    viewer.entry_editor.screenshot_menu_choice = Choice::default();
                }
                None
            }
            Event::StepEntryAction(row, forward)
                if editor_interactive
                    && viewer
                        .entry_editor
                        .screenshot_menu
                        .as_ref()
                        .is_some_and(|(current, _)| current == &row) =>
            {
                viewer.entry_editor.screenshot_menu_choice = viewer
                    .entry_editor
                    .screenshot_menu_choice
                    .step(forward, false);
                None
            }
            Event::DoubleClick(row)
                if editor_interactive
                    && viewer.entry_editor.is_open()
                    && !viewer.entry_flow.saving()
                    && !viewer.entry_editor.is_dirty()
                    && viewer
                        .document
                        .as_ref()
                        .is_some_and(|document| row.is_current(document)) =>
            {
                viewer.entry_editor.screenshot_menu = None;
                Some((
                    Choice::Edit,
                    Target::row(viewer.source.clone(), row.id, row.reference),
                ))
            }
            Event::EntryAction(ref row, choice)
                if editor_interactive
                    && choice != Choice::Analyze
                    && viewer.entry_editor.is_open()
                    && viewer.entry_flow.input_enabled()
                    && viewer
                        .entry_editor
                        .screenshot_menu
                        .as_ref()
                        .is_some_and(|(current, _)| current == row) =>
            {
                viewer.entry_editor.screenshot_menu = None;
                viewer
                    .document
                    .as_ref()
                    .filter(|document| row.is_current(document))
                    .filter(|_| choice != Choice::Edit || !viewer.entry_editor.is_dirty())
                    .map(|_| {
                        (
                            choice,
                            Target::row(
                                viewer.source.clone(),
                                row.id.clone(),
                                row.reference.clone(),
                            ),
                        )
                    })
            }
            _ => None,
        };
        match target {
            Some((Choice::Edit, target)) => self.open_journal_screenshot_editor(id, target),
            Some((Choice::Export, target)) => self.open_journal_screenshot_export(id, target),
            Some((Choice::Analyze, target)) if analysis => {
                self.analyze_journal_screenshot(Origin::Viewer(id), target)
            }
            Some((Choice::Analyze, _)) => Task::none(),
            Some((choice, target)) => {
                self.copy_journal_screenshot(Origin::Viewer(id), target, choice)
            }
            None => Task::none(),
        }
    }
}
