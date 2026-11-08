//! Reloads a visible journal from its original source without blocking the UI.

use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::Duration;

use iced::futures::{StreamExt, stream::BoxStream};
use iced::widget::{button, row, svg, text};
use iced::{Alignment, Element, Subscription, Task, Theme, window};

use super::{Desktop, Journal, Message, Mode, Viewer, Workspace, read_task};

const INTERVAL: Duration = Duration::from_millis(1200);

#[derive(Clone, Debug)]
pub enum Event {
    Request,
    Loaded(u64, Result<Option<Arc<Journal>>, String>),
}

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    active: Option<Active>,
    manual_pending: bool,
    problem: Option<String>,
}

struct Active {
    revision: u64,
    manual: bool,
    obsolete: bool,
}

impl State {
    pub(super) fn manual_running(&self) -> bool {
        self.manual_pending || self.active.as_ref().is_some_and(|request| request.manual)
    }

    pub(super) fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub(super) fn invalidate(&mut self) {
        if let Some(active) = &mut self.active {
            active.obsolete = true;
        }
    }
}

impl Workspace {
    pub(in crate::workspace) fn journal_viewer_subscription(&self) -> Subscription<Desktop> {
        if self.journal_allowed()
            && !self.exiting
            && !self.scheduler_draining
            && self.journals.iter().any(|(&id, surface)| {
                matches!(&surface.mode, Mode::Viewer(viewer) if viewer.document.is_some())
                    && self.visible_journal_surface_owner(id).is_some()
            })
        {
            Subscription::run(clock)
        } else {
            Subscription::none()
        }
    }

    pub(in crate::workspace) fn journal_viewer_tick(&mut self) -> Task<Desktop> {
        let visible: Vec<_> = self
            .journals
            .keys()
            .copied()
            .filter(|&id| self.journal_viewer_should_poll(id))
            .collect();
        Task::batch(visible.into_iter().map(|id| {
            match self.journals.get_mut(&id).map(|surface| &mut surface.mode) {
                Some(Mode::Viewer(viewer)) => {
                    let refresh = viewer.start_refresh(id, false);
                    Task::batch([refresh, viewer.request_screenshot_thumbnails(id)])
                }
                _ => Task::none(),
            }
        }))
    }

    fn journal_viewer_should_poll(&self, id: window::Id) -> bool {
        matches!(self.journals.get(&id).map(|surface| &surface.mode),
            Some(Mode::Viewer(viewer)) if viewer.document.is_some() && viewer.can_refresh())
            && self.viewer_navigation_interactive(id)
    }

    pub(super) fn update_viewer_refresh(&mut self, id: window::Id, event: Event) -> Task<Desktop> {
        let interactive = self.viewer_navigation_interactive(id);
        let Some(Mode::Viewer(viewer)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        let can_refresh = interactive && viewer.can_refresh();
        let refreshed = match event {
            Event::Request if can_refresh => viewer.start_refresh(id, true),
            Event::Request => Task::none(),
            Event::Loaded(revision, result) => {
                viewer.finish_refresh(id, revision, result, can_refresh)
            }
        };
        if interactive {
            Task::batch([refreshed, viewer.request_screenshot_thumbnails(id)])
        } else {
            refreshed
        }
    }
}

impl Viewer {
    pub(super) fn can_refresh(&self) -> bool {
        !self.loading
            && !self.entry_flow.applying_markers()
            && !self.entry_flow.screenshot_saving()
            && self.screenshot.is_none()
            && self.export_range.is_none()
            && !self.title_menu
    }

    pub(super) fn refresh_button(&self) -> Element<'static, Message> {
        // The Refresh glyph from the pinned Java ButtonIcons.
        static ICON: LazyLock<svg::Handle> = LazyLock::new(|| {
            svg::Handle::from_memory(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
              <path d="M17.65 6.35 C16.2 4.9 14.21 4 12 4 C7.58 4 4 7.58 4 12 C4 16.42 7.58 20 12 20
                       C15.73 20 18.84 17.45 19.73 14 H17.65 C16.83 16.33 14.61 18 12 18 C8.69 18 6 15.31 6 12
                       C6 8.69 8.69 6 12 6 C13.66 6 15.14 6.69 16.22 7.78 L13 11 H20 V4 Z"/>
            </svg>"#.as_slice())
        });
        let refreshing = self.refresh.manual_running();
        let enabled = self.can_refresh() && !refreshing;
        let icon =
            svg::Svg::new(ICON.clone())
                .width(15)
                .height(15)
                .style(move |theme: &Theme, _| {
                    let mut color = theme.extended_palette().primary.base.text;
                    if !enabled {
                        color.a *= 0.5;
                    }
                    svg::Style { color: Some(color) }
                });
        button(
            row![
                icon,
                text(if refreshing {
                    "Refreshing…"
                } else {
                    "Refresh"
                })
            ]
            .align_y(Alignment::Center)
            .spacing(6),
        )
        .on_press_maybe(enabled.then_some(Message::ViewerRefresh(Event::Request)))
        .into()
    }

    fn start_refresh(&mut self, id: window::Id, manual: bool) -> Task<Desktop> {
        let refresh = &mut self.refresh;
        if let Some(active) = &refresh.active {
            refresh.manual_pending |= manual && !active.manual;
            return Task::none();
        }
        let manual = manual || refresh.manual_pending;
        refresh.manual_pending = false;
        refresh.revision = refresh.revision.wrapping_add(1);
        let revision = refresh.revision;
        refresh.active = Some(Active {
            revision,
            manual,
            obsolete: false,
        });
        let previous = if manual {
            None
        } else {
            self.document
                .as_ref()
                .and_then(|doc| doc.summary.deletion_identity.clone())
        };
        let source = self.source.clone();
        read_task(
            id,
            move || source.read(previous.as_ref()),
            move |result| {
                Message::ViewerRefresh(Event::Loaded(
                    revision,
                    result.map(|document| Arc::unwrap_or_clone(document).map(Arc::new)),
                ))
            },
        )
    }

    fn finish_refresh(
        &mut self,
        id: window::Id,
        revision: u64,
        result: Result<Option<Arc<Journal>>, String>,
        can_refresh: bool,
    ) -> Task<Desktop> {
        if self
            .refresh
            .active
            .as_ref()
            .is_none_or(|active| active.revision != revision)
        {
            return Task::none();
        }
        let request = self.refresh.active.take().expect("checked refresh ticket");
        if !can_refresh {
            return Task::none();
        }
        let mut applied = Task::none();
        if !request.obsolete {
            match result {
                Ok(Some(document)) if document.summary.id == self.source.id() => {
                    if self.document.is_none() {
                        self.error = None;
                    }
                    self.refresh.problem = None;
                    if request.manual {
                        self.screenshot_thumbnails.clear();
                    }
                    self.name.clone_from(&document.summary.title);
                    self.capture = None;
                    let timeline = self.replace_timeline(id, document);
                    let capture = if self.capture_shown {
                        self.restart_capture(id)
                    } else {
                        self.cursors.clear();
                        self.cursors.push(None);
                        Task::none()
                    };
                    applied = Task::batch([timeline, capture]);
                }
                Ok(Some(_)) => {
                    self.refresh.problem =
                        Some("The journal changed. Refresh the journal list.".into())
                }
                Ok(None) => self.refresh.problem = None,
                Err(error) => self.refresh.problem = Some(error),
            }
        }
        if self.refresh.manual_pending && self.can_refresh() {
            Task::batch([applied, self.start_refresh(id, true)])
        } else {
            applied
        }
    }
}

fn clock() -> BoxStream<'static, Desktop> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-journal-viewer-clock".into())
        .spawn(move || {
            while !sender.is_closed() {
                thread::sleep(INTERVAL);
                let _ = sender.try_send(());
            }
        });
    receiver.map(|()| Desktop::JournalViewerTick).boxed()
}

#[cfg(test)]
mod tests;
