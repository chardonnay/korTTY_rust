use std::fmt;
use std::sync::Arc;

use iced::{Task, window};
use kortty_windowing::ClipboardPng;

use super::{Desktop, Displayed, Event, Message, Mode, Origin, Target, ViewerSource, Workspace};
use crate::workspace::journal_image_menu::Choice;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(in crate::workspace) struct Copies {
    pending: Option<Pending>,
}

struct Pending {
    ticket: u64,
    origin: Origin,
    owner: window::Id,
    source: ViewerSource,
}

#[derive(Clone)]
pub(crate) enum Prepared {
    Internal(Arc<[u8]>),
    Native(ClipboardPng),
}

impl fmt::Debug for Prepared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedScreenshotCopy")
            .finish_non_exhaustive()
    }
}

impl Workspace {
    pub(in crate::workspace) fn copy_dock_journal_screenshot_row(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        source: ViewerSource,
        key: &crate::workspace::journal_thumbnail::Key,
    ) -> Task<Desktop> {
        self.copy_journal_screenshot(
            Origin::Dock { owner, tab, run },
            Target::row(source, key.id().into(), key.reference().into()),
            Choice::CopyImage,
        )
    }

    pub(in crate::workspace) fn copy_dock_journal_screenshot(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        displayed: &Displayed,
    ) -> Task<Desktop> {
        self.copy_journal_screenshot(
            Origin::Dock { owner, tab, run },
            Target::displayed(displayed),
            Choice::CopyImage,
        )
    }

    pub(in crate::workspace) fn copy_dock_journal_screenshot_path(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        source: &ViewerSource,
        reference: &str,
    ) -> Task<Desktop> {
        self.copy_journal_screenshot_path(Origin::Dock { owner, tab, run }, source, reference)
    }

    pub(super) fn copy_journal_screenshot(
        &mut self,
        origin: Origin,
        target: Target,
        choice: Choice,
    ) -> Task<Desktop> {
        if choice == Choice::CopyPath {
            return self.copy_journal_screenshot_path(origin, target.source(), target.reference());
        }
        let Some(owner) = origin.owner(self) else {
            return Task::none();
        };
        if !origin.is_current(self, owner, target.source())
            || self.screenshot_copy_busy(origin, owner)
        {
            return Task::none();
        }
        let ticket = self.allocate();
        let internal = self.clipboard.policy.internal;
        self.journal_screenshot_copies.pending = Some(Pending {
            ticket,
            origin,
            owner,
            source: target.source().clone(),
        });
        super::super::read_task(
            origin.window(),
            move || prepare(target, internal),
            move |result| Message::ScreenshotAction(Event::CopyPrepared(ticket, result)),
        )
    }

    fn copy_journal_screenshot_path(
        &mut self,
        origin: Origin,
        source: &ViewerSource,
        reference: &str,
    ) -> Task<Desktop> {
        let Some(owner) = origin.owner(self) else {
            return Task::none();
        };
        if !origin.is_current(self, owner, source)
            || !crate::session_journal::viewer::Screenshot::is_visible_reference(reference)
            || self.screenshot_copy_busy(origin, owner)
        {
            return Task::none();
        }
        self.write_clipboard(reference.to_owned())
    }

    fn screenshot_copy_busy(&mut self, origin: Origin, owner: window::Id) -> bool {
        if self.journal_screenshot_copies.pending.is_none() {
            return false;
        }
        self.screenshot_copy_notice(origin, owner, "A screenshot copy is still running.".into());
        true
    }

    pub(super) fn screenshot_copy_prepared(
        &mut self,
        ticket: u64,
        result: Result<Arc<Prepared>, String>,
    ) -> Task<Desktop> {
        let Some(pending) = self
            .journal_screenshot_copies
            .pending
            .as_ref()
            .filter(|pending| pending.ticket == ticket)
        else {
            return Task::none();
        };
        if !pending
            .origin
            .is_current(self, pending.owner, &pending.source)
        {
            self.journal_screenshot_copies.pending = None;
            return Task::none();
        }
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => return self.screenshot_copy_finished(ticket, Err(error)),
        };
        match prepared.as_ref() {
            Prepared::Internal(png) if self.clipboard.policy.internal => {
                self.clipboard.buffer.set_png(Arc::clone(png));
                self.screenshot_copy_finished(ticket, Ok(()))
            }
            Prepared::Native(png) if !self.clipboard.policy.internal => {
                let source = pending.origin.window();
                let native = self.hosted_tool_owner(source).unwrap_or(source);
                crate::workspace::clipboard_access::publish_png(
                    native,
                    png.clone(),
                    || Ok(()),
                    move |result| {
                        Desktop::SessionJournal(
                            source,
                            Message::ScreenshotAction(Event::CopyFinished(ticket, result)),
                        )
                    },
                )
            }
            _ => self.screenshot_copy_finished(ticket, Err("The clipboard mode changed.".into())),
        }
    }

    pub(super) fn screenshot_copy_finished(
        &mut self,
        ticket: u64,
        result: Result<(), String>,
    ) -> Task<Desktop> {
        if self
            .journal_screenshot_copies
            .pending
            .as_ref()
            .is_some_and(|pending| pending.ticket == ticket)
            && let Some(pending) = self.journal_screenshot_copies.pending.take()
            && let Err(error) = result
        {
            self.screenshot_copy_notice(
                pending.origin,
                pending.owner,
                format!("Could not copy the screenshot ({error})."),
            );
        }
        Task::none()
    }

    fn screenshot_copy_notice(&mut self, origin: Origin, owner: window::Id, text: String) {
        if let Origin::Viewer(id) = origin
            && let Some(Mode::Viewer(viewer)) =
                self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        {
            viewer.error = Some(text);
        } else {
            self.notice(owner, text);
        }
    }
}

fn prepare(target: Target, internal: bool) -> std::io::Result<Prepared> {
    let asset = target.review()?;
    let png = Arc::from(asset.bytes());
    if internal {
        Ok(Prepared::Internal(png))
    } else {
        ClipboardPng::prepare(png).map(Prepared::Native)
    }
}
