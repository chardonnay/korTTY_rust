//! Owner-scoped publication of a reviewed journal screenshot.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use iced::widget::{button, column, container, image, row, scrollable, space, text};
use iced::{Element, Length, Size, Task, window};

use crate::session_journal::export_output::{PngDestination, PublishedScreenshot};
use crate::session_journal::viewer::Screenshot;
use crate::workspace::dialog_placement::Kind as DialogKind;
use crate::workspace::journal_thumbnail::image_handle;

use super::export_dialog::browser::{self, Browser};
use super::screenshot_access::Origin;
use super::{Desktop, Message, ViewerSource, Workspace, display, read_task};

pub(super) const TITLE: &str = "Export screenshot";

#[derive(Default)]
pub(in crate::workspace) struct Exports {
    pub(super) dialogs: HashMap<window::Id, Dialog>,
}

pub(crate) struct Displayed {
    pub(in crate::workspace) image: image::Handle,
    asset: Arc<Screenshot>,
}

impl Displayed {
    pub(in crate::workspace) fn entry_id(&self) -> &str {
        self.asset.id()
    }

    pub(in crate::workspace) fn load(
        source: &ViewerSource,
        id: &str,
        reference: &str,
    ) -> io::Result<Self> {
        let asset = Arc::new(Screenshot::review(source, id, reference)?);
        let pixels = decode(&asset)?;
        Ok(Self {
            image: image_handle(pixels),
            asset,
        })
    }
}

impl std::fmt::Debug for Displayed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DisplayedJournalScreenshot")
            .finish_non_exhaustive()
    }
}

fn decode(screenshot: &Screenshot) -> io::Result<tiny_skia::Pixmap> {
    tiny_skia::Pixmap::decode_png(screenshot.bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "The journal screenshot is not a valid PNG image.",
        )
    })
}

pub(super) enum Target {
    Row {
        source: ViewerSource,
        id: String,
        reference: String,
    },
    Displayed(Arc<Screenshot>),
}

impl Target {
    pub(super) fn row(source: ViewerSource, id: String, reference: String) -> Self {
        Self::Row {
            source,
            id,
            reference,
        }
    }

    pub(super) fn displayed(displayed: &Displayed) -> Self {
        Self::Displayed(Arc::clone(&displayed.asset))
    }

    pub(super) fn source(&self) -> &ViewerSource {
        match self {
            Self::Row { source, .. } => source,
            Self::Displayed(image) => image.source(),
        }
    }

    pub(super) fn reference(&self) -> &str {
        match self {
            Self::Row { reference, .. } => reference,
            Self::Displayed(image) => image.reference(),
        }
    }

    pub(super) fn entry_id(&self) -> &str {
        match self {
            Self::Row { id, .. } => id,
            Self::Displayed(image) => image.id(),
        }
    }

    pub(super) fn review(self) -> io::Result<Arc<Screenshot>> {
        match self {
            Self::Row {
                source,
                id,
                reference,
            } => {
                let asset = Screenshot::review(&source, &id, &reference)?;
                decode(&asset)?;
                Ok(Arc::new(asset))
            }
            Self::Displayed(asset) => {
                asset.require_current()?;
                Ok(asset)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Focus,
    Loaded(Result<Arc<Screenshot>, String>),
    Browser(browser::Event),
    Reviewed(u64, Result<Arc<PngDestination>, String>),
    Back,
    Replace,
    Saved(u64, Result<Arc<PublishedScreenshot>, String>),
    Close,
}

pub(super) struct Dialog {
    pub(super) origin: Origin,
    pub(super) owner: window::Id,
    bound: ViewerSource,
    asset: Option<Arc<Screenshot>>,
    browser: Option<Browser>,
    phase: Phase,
    ticket: u64,
}

enum Phase {
    Loading,
    Choose,
    Reviewing(u64),
    Confirm(Arc<PngDestination>),
    Saving(u64),
    Finished(Arc<PublishedScreenshot>),
    Failed(String),
}

enum Action {
    None,
    Task(Task<Desktop>),
    Save(Arc<PngDestination>),
}

impl Workspace {
    pub(super) fn open_journal_screenshot_export(
        &mut self,
        source: window::Id,
        target: Target,
    ) -> Task<Desktop> {
        self.open_screenshot_export(Origin::Viewer(source), target)
    }

    pub(in crate::workspace) fn open_dock_journal_screenshot_export(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        displayed: &Displayed,
    ) -> Task<Desktop> {
        self.open_screenshot_export(
            Origin::Dock { owner, tab, run },
            Target::displayed(displayed),
        )
    }

    fn open_screenshot_export(&mut self, origin: Origin, target: Target) -> Task<Desktop> {
        let Some(owner) = origin.owner(self) else {
            return Task::none();
        };
        if !origin.is_current(self, owner, target.source()) {
            return Task::none();
        }
        if let Some((&id, _)) = self
            .journal_screenshot_exports
            .dialogs
            .iter()
            .find(|(_, dialog)| dialog.origin == origin && dialog.owner == owner)
        {
            return window::gain_focus(id);
        }
        if !self.palette_can_open(owner)
            || self.palette.is_open(owner)
            || self.journal_edit_for_owner(origin.window()).is_some()
            || self.journal_edit_for_owner(owner).is_some()
        {
            return Task::none();
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::JournalScreenshotExport,
            window::Settings {
                size: Size::new(640.0, 500.0),
                min_size: Some(Size::new(390.0, 280.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.journal_screenshot_exports.dialogs.insert(
            id,
            Dialog {
                origin,
                owner,
                bound: target.source().clone(),
                asset: None,
                browser: None,
                phase: Phase::Loading,
                ticket: 0,
            },
        );
        let loaded = read_task(
            id,
            move || target.review(),
            |result| {
                Message::ScreenshotExport(Event::Loaded(
                    result.map(|asset| Arc::clone(asset.as_ref())),
                ))
            },
        );
        Task::batch([opened.map(|_| Desktop::Noop), loaded])
    }

    pub(super) fn update_journal_screenshot_export(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        if matches!(event, Event::Close | Event::Browser(browser::Event::Cancel)) {
            return self
                .close_journal_screenshot_export(id)
                .unwrap_or_else(Task::none);
        }
        let Some(dialog) = self.journal_screenshot_exports.dialogs.get(&id) else {
            return Task::none();
        };
        if matches!(event, Event::Focus) {
            return window::gain_focus(id);
        }
        let scope = dialog.origin.is_current(self, dialog.owner, &dialog.bound)
            && self
                .journal_edit_for_owner(dialog.origin.window())
                .is_none_or(|modal| modal == id)
            && self
                .journal_edit_for_owner(dialog.owner)
                .is_none_or(|modal| modal == id);
        if !scope {
            if let Phase::Saving(current) = dialog.phase {
                if let Event::Saved(ticket, result) = event
                    && current == ticket
                    && let Some(dialog) = self.journal_screenshot_exports.dialogs.get_mut(&id)
                {
                    dialog.phase = match result {
                        Ok(saved) => Phase::Finished(saved),
                        Err(error) => Phase::Failed(error),
                    };
                }
                return Task::none();
            }
            if matches!(dialog.phase, Phase::Finished(_) | Phase::Failed(_)) {
                return Task::none();
            }
            return self
                .close_journal_screenshot_export(id)
                .unwrap_or_else(Task::none);
        }
        let Some(dialog) = self.journal_screenshot_exports.dialogs.get_mut(&id) else {
            return Task::none();
        };
        match dialog.update(id, event) {
            Action::None => Task::none(),
            Action::Task(task) => task,
            Action::Save(destination) => self.save_journal_screenshot(id, destination),
        }
    }

    fn save_journal_screenshot(
        &mut self,
        id: window::Id,
        destination: Arc<PngDestination>,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_screenshot_exports.dialogs.get_mut(&id) else {
            return Task::none();
        };
        let asset = Arc::clone(dialog.asset.as_ref().expect("reviewed PNG"));
        dialog.ticket = dialog.ticket.wrapping_add(1);
        let ticket = dialog.ticket;
        dialog.phase = Phase::Saving(ticket);
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("kortty-journal-screenshot-export".into())
            .spawn(move || {
                let result = destination
                    .publish(&asset, &|| false)
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let _ = send.try_send(result);
                drop(finished);
            });
        if let Err(error) = worker {
            dialog.back_with_error(error.to_string());
            return Task::none();
        }
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Screenshot export worker stopped.".into()));
            Desktop::SessionJournal(id, Message::ScreenshotExport(Event::Saved(ticket, result)))
        })
    }

    pub(super) fn journal_screenshot_export_view(&self, id: window::Id) -> Element<'_, Desktop> {
        self.journal_screenshot_exports
            .dialogs
            .get(&id)
            .map_or_else(
                || text(TITLE).into(),
                |dialog| {
                    dialog.view().map(move |event| {
                        Desktop::SessionJournal(id, Message::ScreenshotExport(event))
                    })
                },
            )
    }

    pub(super) fn close_journal_screenshot_export(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        let dialog = self.journal_screenshot_exports.dialogs.get(&id)?;
        if matches!(dialog.phase, Phase::Saving(_)) {
            return Some(Task::none());
        }
        Some(self.finish_journal_screenshot_close(id))
    }

    fn finish_journal_screenshot_close(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_screenshot_exports.dialogs.remove(&id) else {
            return Task::none();
        };
        let source = dialog.origin.window();
        let focus = self.hosted_tool_owner(source).unwrap_or(source);
        self.close_placed_dialog(id)
            .chain(window::gain_focus(focus))
    }

    pub(super) fn remove_journal_screenshot_exports(
        &mut self,
        owner_or_source: window::Id,
    ) -> Task<Desktop> {
        let mut closed = Vec::new();
        self.journal_screenshot_exports
            .dialogs
            .retain(|&id, dialog| {
                let keep = matches!(dialog.phase, Phase::Saving(_))
                    || (dialog.owner != owner_or_source
                        && dialog.origin.window() != owner_or_source);
                if !keep {
                    closed.push(id);
                }
                keep
            });
        Task::batch(closed.into_iter().map(|id| self.close_placed_dialog(id)))
    }
}

impl Dialog {
    fn update(&mut self, id: window::Id, event: Event) -> Action {
        match event {
            Event::Loaded(result) if matches!(self.phase, Phase::Loading) => match result {
                Ok(asset) if asset.source().same_journal(&self.bound) => {
                    let mut browser = Browser::new(asset.filename().to_owned(), ".png")
                        .with_route(|event| Message::ScreenshotExport(Event::Browser(event)));
                    let list = browser.update(id, browser::Event::List);
                    self.browser = Some(browser);
                    self.asset = Some(asset);
                    self.phase = Phase::Choose;
                    return Action::Task(list);
                }
                Ok(_) => self.phase = Phase::Failed("The selected journal changed.".into()),
                Err(error) => self.phase = Phase::Failed(error),
            },
            Event::Browser(browser::Event::Save) if matches!(self.phase, Phase::Choose) => {
                let browser = self.browser.as_mut().expect("initialized PNG browser");
                let destination = match browser.destination() {
                    Ok(destination) => destination,
                    Err(error) => {
                        browser.set_problem(error.to_string());
                        return Action::None;
                    }
                };
                let asset = Arc::clone(self.asset.as_ref().expect("reviewed PNG"));
                self.ticket = self.ticket.wrapping_add(1);
                let ticket = self.ticket;
                self.phase = Phase::Reviewing(ticket);
                return Action::Task(read_task(
                    id,
                    move || PngDestination::review(destination, &asset),
                    move |result| Message::ScreenshotExport(Event::Reviewed(ticket, result)),
                ));
            }
            Event::Browser(event) if matches!(self.phase, Phase::Choose) => {
                return Action::Task(
                    self.browser
                        .as_mut()
                        .expect("initialized PNG browser")
                        .update(id, event),
                );
            }
            Event::Reviewed(ticket, result) if matches!(self.phase, Phase::Reviewing(current) if ticket == current) => {
                match result {
                    Ok(destination) if destination.exists() => {
                        self.phase = Phase::Confirm(destination)
                    }
                    Ok(destination) => return Action::Save(destination),
                    Err(error) => self.back_with_error(error),
                }
            }
            Event::Replace => {
                if let Phase::Confirm(destination) = &self.phase {
                    return Action::Save(Arc::clone(destination));
                }
            }
            Event::Back if matches!(self.phase, Phase::Confirm(_)) => self.phase = Phase::Choose,
            Event::Saved(ticket, result) if matches!(self.phase, Phase::Saving(current) if current == ticket) => {
                match result {
                    Ok(saved) => self.phase = Phase::Finished(saved),
                    Err(error) => self.back_with_error(error),
                }
            }
            _ => {}
        }
        Action::None
    }

    fn back_with_error(&mut self, error: String) {
        self.browser
            .as_mut()
            .expect("initialized PNG browser")
            .set_problem(error);
        self.phase = Phase::Choose;
    }

    fn view(&self) -> Element<'_, Event> {
        let close = |label| button(label).on_press(Event::Close);
        let body = match &self.phase {
            Phase::Choose => {
                return self
                    .browser
                    .as_ref()
                    .expect("initialized PNG browser")
                    .view_with_title(TITLE)
                    .map(Event::Browser);
            }
            Phase::Loading => column![text("Reading screenshot…"), close("Cancel")],
            Phase::Reviewing(_) => {
                column![text("Checking the export destination…"), close("Cancel")]
            }
            Phase::Confirm(destination) => column![
                text("Replace the existing PNG image?").size(19),
                text(display(&destination.path().to_string_lossy(), 4096)),
                row![
                    space::horizontal(),
                    button("Back").on_press(Event::Back),
                    button("Replace")
                        .style(button::danger)
                        .on_press(Event::Replace),
                ]
                .spacing(8)
            ],
            Phase::Saving(_) => column![text("Saving PNG image…")],
            Phase::Finished(saved) => column![
                text("Screenshot exported").size(19),
                text(display(&saved.path.to_string_lossy(), 4096)),
                saved
                    .warning
                    .as_deref()
                    .map(|warning| text(display(warning, 4096))),
                close("Close")
            ],
            Phase::Failed(error) => column![text(display(error, 4096)), close("Close")],
        };
        container(scrollable(body.spacing(12)))
            .padding(16)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests;
