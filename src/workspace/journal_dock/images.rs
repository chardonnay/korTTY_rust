//! Viewport thumbnails and a run-bound screenshot review inside the live dock.

use std::sync::Arc;
use std::thread;

use iced::{Point, Task, window};

use crate::session_journal::{TimelineEntry, viewer::Source};

use super::super::journal_access::screenshot_export::Displayed;
use super::super::journal_image_menu::Choice;
use super::super::journal_thumbnail::{Cache, Key, Loaded};
use super::super::{Message as Desktop, Workspace};
use super::{Action, Message, Placement, view};

pub(super) mod widget;

#[derive(Default)]
pub(super) struct Images {
    pub(super) thumbnails: Cache,
    // A rebind or hide invalidates the request but leaves its worker slot occupied.
    pending: Option<u64>,
}

#[derive(Default)]
pub(super) struct State {
    visible: Vec<Key>,
    pub(super) full: Option<Full>,
    pub(super) menu: Option<Menu>,
}

impl State {
    pub(super) fn retain(&mut self, entries: &[TimelineEntry]) {
        self.visible.retain(|key| contains(entries, key));
        if self
            .full
            .as_ref()
            .is_some_and(|full| !contains(entries, &full.key))
        {
            self.full = None;
        }
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| !contains(entries, &menu.key))
        {
            self.menu = None;
        }
    }

    fn invalidate(&mut self, entry: &str) {
        if self
            .full
            .as_ref()
            .is_some_and(|full| full.key.id() == entry)
        {
            self.full = None;
        }
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.key.id() == entry)
        {
            self.menu = None;
        }
    }

    fn visible(&mut self, key: Key, visible: bool) {
        if visible {
            if !self.visible.contains(&key) {
                self.visible.push(key);
            }
        } else {
            self.visible.retain(|current| current != &key);
            if self
                .menu
                .as_ref()
                .is_some_and(|menu| !menu.full && menu.key == key)
            {
                self.menu = None;
            }
        }
    }

    fn dismiss(&mut self) -> bool {
        self.menu.take().is_some() || self.full.take().is_some()
    }
}

pub(super) struct Menu {
    pub(super) key: Key,
    pub(super) full: bool,
    pub(super) point: Point,
    pub(super) choice: Choice,
}

pub(super) struct Full {
    pub(super) key: Key,
    ticket: u64,
    choice: Option<Choice>,
    pub(super) result: Option<Result<Arc<Displayed>, String>>,
}

impl Full {
    pub(super) fn is_lightbox(&self) -> bool {
        self.choice.is_none()
    }
}

pub(super) fn contains(entries: &[TimelineEntry], key: &Key) -> bool {
    let mut entries = view::visible_entries(entries)
        .iter()
        .filter(|entry| entry.id == key.id());
    entries.next().and_then(Key::for_entry).as_ref() == Some(key) && entries.next().is_none()
}

impl Workspace {
    pub(in crate::workspace) fn dock_screenshot_source_current(
        &self,
        owner: window::Id,
        tab: u64,
        run: u64,
        source: &Source,
    ) -> bool {
        self.journal_allowed()
            && !self.exiting
            && !self.scheduler_draining
            && !self.terminal_only_fullscreen_active(owner)
            && self.windows.get(&owner).is_some_and(|window| {
                let dock = &window.journal_dock;
                dock.placement() != Placement::Hidden
                    && dock.bound.as_ref().is_some_and(|bound| {
                        bound.stamp() == (tab, run)
                            && Source::capture(&bound.source.capture).same_journal(source)
                    })
            })
    }

    pub(in crate::workspace) fn dismiss_journal_dock_screenshot(
        &mut self,
        owner: window::Id,
    ) -> bool {
        if !self.journal_allowed()
            || self.terminal_only_fullscreen_active(owner)
            || self.owner_dialog(owner).is_some()
            || self.palette.is_open(owner)
        {
            return false;
        }
        self.windows
            .get_mut(&owner)
            .and_then(|window| window.journal_dock.bound.as_mut())
            .is_some_and(|bound| bound.images.dismiss())
    }

    pub(in crate::workspace) fn invalidate_journal_dock_screenshot(
        &mut self,
        source: &Source,
        entry: &str,
    ) -> Task<Desktop> {
        let owners = self
            .windows
            .iter_mut()
            .filter_map(|(&owner, window)| {
                let dock = &mut window.journal_dock;
                let bound = dock.bound.as_mut()?;
                if !Source::capture(&bound.source.capture).same_journal(source) {
                    return None;
                }
                dock.images.thumbnails.invalidate(entry);
                bound.images.invalidate(entry);
                Some(owner)
            })
            .collect::<Vec<_>>();
        Task::batch(
            owners
                .into_iter()
                .map(|owner| self.request_dock_thumbnails(owner)),
        )
    }

    pub(super) fn dock_screenshot_actions_allowed(
        &self,
        owner: window::Id,
        tab: u64,
        run: u64,
    ) -> bool {
        self.journal_dock_appearance_current(owner, tab, run) && self.owner_dialog(owner).is_none()
    }

    pub(super) fn set_dock_thumbnail_visibility(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        key: Key,
        visible: bool,
    ) -> Task<Desktop> {
        if let Some(bound) = self
            .windows
            .get_mut(&owner)
            .and_then(|window| window.journal_dock.bound.as_mut())
            .filter(|bound| bound.stamp() == (tab, run) && contains(&bound.timeline.entries, &key))
        {
            bound.images.visible(key, visible);
        }
        self.request_dock_thumbnails(owner)
    }

    pub(super) fn dock_thumbnails_loaded(
        &mut self,
        owner: window::Id,
        loaded: &Loaded,
    ) -> Task<Desktop> {
        if self
            .windows
            .get_mut(&owner)
            .is_some_and(|window| window.journal_dock.images.thumbnails.integrate(loaded))
        {
            self.request_dock_thumbnails(owner)
        } else {
            Task::none()
        }
    }

    pub(super) fn request_dock_thumbnails(&mut self, owner: window::Id) -> Task<Desktop> {
        if !self.journal_allowed()
            || self.exiting
            || self.scheduler_draining
            || self.terminal_only_fullscreen_active(owner)
        {
            return Task::none();
        }
        let Some(dock) = self
            .windows
            .get_mut(&owner)
            .map(|window| &mut window.journal_dock)
        else {
            return Task::none();
        };
        let Some(bound) = &dock.bound else {
            return Task::none();
        };
        let source = Source::capture(&bound.source.capture);
        dock.images.thumbnails.request(
            &source,
            bound.images.visible.iter().rev().cloned(),
            move |loaded| Desktop::JournalDock(owner, Message::ThumbnailsLoaded(loaded)),
        )
    }

    pub(super) fn update_dock_screenshot(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        action: Action,
    ) -> Task<Desktop> {
        if !self.dock_screenshot_actions_allowed(owner, tab, run) {
            return Task::none();
        }
        let analysis = self.journal_screenshot_analysis_available();
        let ticket = matches!(
            &action,
            Action::ShowScreenshot(_) | Action::ChooseScreenshot { full: false, .. }
        )
        .then(|| self.allocate());
        let Some(bound) = self
            .windows
            .get_mut(&owner)
            .and_then(|window| window.journal_dock.bound.as_mut())
            .filter(|bound| bound.stamp() == (tab, run))
        else {
            return Task::none();
        };
        let current = |key| contains(&bound.timeline.entries, key);
        match action {
            Action::ShowScreenshot(key) if current(&key) => {
                bound.images.menu = None;
                bound.images.full = ticket.map(|ticket| Full {
                    key,
                    ticket,
                    choice: None,
                    result: None,
                });
                return self.request_dock_screenshot(owner);
            }
            Action::HideScreenshot(key)
                if bound
                    .images
                    .full
                    .as_ref()
                    .is_some_and(|full| full.key == key) =>
            {
                bound.images.full = None;
                bound.images.menu = None;
            }
            Action::ScreenshotMenu { key, full, point }
                if current(&key) && (!full || reviewed(&bound.images, &key).is_some()) =>
            {
                bound.images.menu = Some(Menu {
                    key,
                    full,
                    point,
                    choice: Choice::default(),
                });
            }
            Action::DismissScreenshotMenu { key, full } => {
                if bound
                    .images
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.key == key && menu.full == full)
                {
                    bound.images.menu = None;
                }
            }
            Action::StepScreenshotChoice { key, full, forward } => {
                if let Some(menu) = bound
                    .images
                    .menu
                    .as_mut()
                    .filter(|menu| menu.key == key && menu.full == full)
                {
                    menu.choice = menu.choice.step(forward, analysis);
                }
            }
            Action::ChooseScreenshot { key, full, choice }
                if current(&key)
                    && bound
                        .images
                        .menu
                        .as_ref()
                        .is_some_and(|menu| menu.key == key && menu.full == full) =>
            {
                bound.images.menu = None;
                if choice == Choice::Analyze && !analysis {
                    return Task::none();
                }
                if choice == Choice::CopyPath {
                    let source = Source::capture(&bound.source.capture);
                    return self.copy_dock_journal_screenshot_path(
                        owner,
                        tab,
                        run,
                        &source,
                        key.reference(),
                    );
                }
                if !full && choice == Choice::CopyImage {
                    let source = Source::capture(&bound.source.capture);
                    return self.copy_dock_journal_screenshot_row(owner, tab, run, source, &key);
                }
                if !full && choice == Choice::Analyze {
                    let source = Source::capture(&bound.source.capture);
                    return self.analyze_dock_journal_screenshot_row(owner, tab, run, source, &key);
                }
                if full {
                    if let Some(displayed) = reviewed(&bound.images, &key).cloned() {
                        return self
                            .open_dock_screenshot_choice(owner, tab, run, &displayed, choice);
                    }
                } else {
                    bound.images.full = ticket.map(|ticket| Full {
                        key,
                        ticket,
                        choice: Some(choice),
                        result: None,
                    });
                    return self.request_dock_screenshot(owner);
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn request_dock_screenshot(&mut self, owner: window::Id) -> Task<Desktop> {
        if !self.journal_allowed()
            || self.exiting
            || self.scheduler_draining
            || self.terminal_only_fullscreen_active(owner)
        {
            return Task::none();
        }
        let Some(dock) = self
            .windows
            .get_mut(&owner)
            .map(|window| &mut window.journal_dock)
        else {
            return Task::none();
        };
        let Some(bound) = dock
            .bound
            .as_mut()
            .filter(|_| dock.images.pending.is_none())
        else {
            return Task::none();
        };
        let Some(full) = bound
            .images
            .full
            .as_mut()
            .filter(|full| full.result.is_none())
        else {
            return Task::none();
        };
        let ticket = full.ticket;
        let key = full.key.clone();
        let source = Source::capture(&bound.source.capture);
        let (send, receive) = async_channel::bounded(1);
        let (signal, done) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-dock-screenshot".into())
            .spawn(move || {
                let result = Displayed::load(&source, key.id(), key.reference())
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let _ = send.try_send(result);
                drop(signal);
            });
        if let Err(error) = worker {
            full.result = Some(Err(format!(
                "Could not start the screenshot reader ({error})."
            )));
            if full.choice.is_some() {
                bound.notice = Some(format!("Could not start the screenshot reader ({error})."));
                bound.images.full = None;
            }
            return Task::none();
        }
        dock.images.pending = Some(ticket);
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The screenshot reader stopped.".into()));
            Desktop::JournalDock(owner, Message::ScreenshotLoaded(ticket, result))
        })
    }

    pub(super) fn dock_screenshot_loaded(
        &mut self,
        owner: window::Id,
        ticket: u64,
        result: Result<Arc<Displayed>, String>,
    ) -> Task<Desktop> {
        let allowed = self.journal_allowed()
            && !self.exiting
            && !self.scheduler_draining
            && !self.terminal_only_fullscreen_active(owner);
        let Some(dock) = self
            .windows
            .get_mut(&owner)
            .map(|window| &mut window.journal_dock)
        else {
            return Task::none();
        };
        if dock.images.pending != Some(ticket) {
            return Task::none();
        }
        dock.images.pending = None;
        let mut activate = None;
        if allowed
            && let Some(bound) = &mut dock.bound
            && let Some(full) = bound
                .images
                .full
                .as_mut()
                .filter(|full| full.ticket == ticket)
            && contains(&bound.timeline.entries, &full.key)
        {
            if let Some(choice) = full.choice {
                bound.images.full = None;
                match result {
                    Ok(displayed) => activate = Some((bound.stamp(), displayed, choice)),
                    Err(error) => bound.notice = Some(error),
                }
            } else {
                full.result = Some(result);
            }
        }
        if let Some(((tab, run), displayed, choice)) = activate {
            return self.open_dock_screenshot_choice(owner, tab, run, &displayed, choice);
        }
        self.request_dock_screenshot(owner)
    }

    fn open_dock_screenshot_choice(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        displayed: &Displayed,
        choice: Choice,
    ) -> Task<Desktop> {
        if !self.dock_screenshot_actions_allowed(owner, tab, run) {
            return Task::none();
        }
        match choice {
            Choice::Edit => self.open_dock_journal_screenshot_editor(owner, tab, run, displayed),
            Choice::Export => self.open_dock_journal_screenshot_export(owner, tab, run, displayed),
            Choice::CopyImage => self.copy_dock_journal_screenshot(owner, tab, run, displayed),
            Choice::Analyze => self.analyze_dock_journal_screenshot(owner, tab, run, displayed),
            Choice::CopyPath => Task::none(),
        }
    }
}

fn reviewed<'a>(images: &'a State, key: &Key) -> Option<&'a Arc<Displayed>> {
    images
        .full
        .as_ref()
        .filter(|full| full.is_lightbox() && full.key == *key)?
        .result
        .as_ref()?
        .as_ref()
        .ok()
}

#[cfg(test)]
mod tests;
