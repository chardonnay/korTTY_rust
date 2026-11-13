//! Native About dialog reached from the in-window and system Help menus.

use std::path::PathBuf;
use std::sync::OnceLock;

use iced::widget::{
    button, column, container, image, progress_bar, row, rule, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Size, Task, window};

use crate::application_update::{self, Check, Offer, Pending, Progress, Saved};

use super::super::dialog_placement::Kind;
use super::{Message, Workspace, WorkspaceMessage, wrap};

pub(super) const TITLE: &str = "About kortty";
const PROJECT_URL: &str = "https://github.com/chardonnay/kortty";
// Original artwork by Daniel Mengel, from the MIT-licensed Java reference at 1ebbe724ee84.
const LOGO: &[u8] = include_bytes!("about/kortty_logo.png");

#[derive(Clone, Debug)]
pub enum Event {
    Focus,
    OpenProject,
    ProjectOpened(bool),
    CheckUpdates,
    UpdatesChecked(u64, Result<Check, String>),
    UpdateFolder(String),
    DownloadUpdate,
    UpdateProgress(u64, Progress),
    UpdateDownloaded(u64, Result<Saved, String>),
    CancelUpdate,
    RevealUpdate,
    UpdateRevealed(bool),
}

pub(super) struct Dialog {
    pub(super) id: window::Id,
    pub(super) owner: window::Id,
    project: Project,
    updates: Updates,
}

struct Updates {
    ticket: u64,
    folder: String,
    offer: Option<Offer>,
    saved: Option<Saved>,
    progress: Option<Progress>,
    pending: Option<Operation>,
    status: String,
    background: bool,
}

struct Operation {
    cancel: Option<application_update::Cancel>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            ticket: 0,
            folder: std::env::home_dir()
                .map(|home| home.join("Downloads").to_string_lossy().into_owned())
                .unwrap_or_default(),
            offer: None,
            saved: None,
            progress: None,
            pending: None,
            status: String::new(),
            background: false,
        }
    }
}

impl Updates {
    fn begin<T: Send + 'static>(
        &mut self,
        id: window::Id,
        pending: Pending<T>,
        complete: fn(u64, Result<T, String>) -> Event,
    ) -> (async_channel::Receiver<()>, Task<WorkspaceMessage>) {
        let Pending {
            reply,
            done,
            cancel,
        } = pending;
        self.ticket = self.ticket.wrapping_add(1);
        let ticket = self.ticket;
        self.pending = Some(Operation {
            cancel: Some(cancel),
        });
        let task = Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The update worker stopped.".into()))
            },
            move |result| wrap(Message::About(id, complete(ticket, result))),
        );
        (done, task)
    }

    fn finish(&mut self, ticket: u64, committed: bool) -> bool {
        if self.ticket != ticket {
            return false;
        }
        let Some(pending) = self.pending.take() else {
            return false;
        };
        self.progress = None;
        if pending.cancel.is_none() && !committed {
            self.status = "Application update canceled.".into();
            false
        } else {
            true
        }
    }

    fn checked(&mut self, result: Result<Check, String>) {
        self.status = match result {
            Ok(Check::NoStableRelease) => {
                "No stable release is available from the Rust release channel.".into()
            }
            Ok(Check::Current) => "This version is current on the Rust release channel.".into(),
            Ok(Check::NoCompatible(version)) => {
                format!("Version {version} has no package for this build's exact native target.")
            }
            Ok(Check::MissingDigest(version)) => {
                format!("Version {version} has no valid SHA-256 digest for its native package.")
            }
            Ok(Check::Available(offer)) => {
                let status = format!(
                    "Version {} is available. Review the archive and destination before downloading.",
                    offer.version()
                );
                self.offer = Some(offer);
                status
            }
            Err(error) => error,
        };
    }

    fn present_background(&mut self, check: Option<&Check>) {
        if self.pending.is_some() || (!self.background && !self.status.is_empty()) {
            return;
        }
        self.offer = None;
        self.status.clear();
        self.background = check.is_some();
        if let Some(check) = check {
            self.checked(Ok(check.clone()));
        }
    }

    fn view(
        &self,
        id: window::Id,
        allowed: bool,
        channel: String,
    ) -> Element<'_, WorkspaceMessage> {
        let event = move |event| wrap(Message::About(id, event));
        let idle = self.pending.is_none();
        let review = self.offer.as_ref().map(|offer| {
            column![
                text(offer.name())
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .size(14),
                text(format!(
                    "{} bytes · SHA-256 {}",
                    offer.size(),
                    offer.checksum()
                ))
                .wrapping(text::Wrapping::WordOrGlyph)
                .size(13),
                text(offer.source())
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .size(13),
                text_input("Existing absolute download directory", &self.folder).on_input_maybe(
                    (allowed && idle).then_some(move |path| event(Event::UpdateFolder(path)))
                ),
                button("Download verified archive").on_press_maybe(
                    (allowed && idle && !self.folder.trim().is_empty())
                        .then_some(event(Event::DownloadUpdate))
                ),
            ]
            .spacing(6)
        });
        column![
            rule::horizontal(1),
            text("Application updates").size(17),
            text(channel).size(13),
            (!allowed)
                .then(|| text("Application updates are disabled by managed policy.").size(13)),
            row![
                button("Check for updates")
                    .on_press_maybe((allowed && idle).then_some(event(Event::CheckUpdates))),
                self.pending
                    .as_ref()
                    .and_then(|pending| pending.cancel.as_ref())
                    .map(|_| button("Cancel update").on_press(event(Event::CancelUpdate))),
            ]
            .spacing(8),
            self.progress.map(|progress| column![
                progress_bar(
                    0.0..=1.0,
                    (progress.received as f64 / progress.total as f64) as f32
                ),
                text(format!("{} of {} bytes", progress.received, progress.total)).size(13),
            ]
            .spacing(4)),
            (!self.status.is_empty()).then(|| text(&self.status)
                .wrapping(text::Wrapping::WordOrGlyph)
                .size(13)),
            review,
            self.saved.as_ref().map(|saved| column![
                text(saved.path.display().to_string())
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .size(13),
                button("Show download directory").on_press(event(Event::RevealUpdate)),
            ]
            .spacing(6)),
        ]
        .spacing(8)
        .into()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Project {
    Ready,
    Opening,
    Failed,
}

impl Workspace {
    pub(super) fn show_about(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some((_, focus)) = self.owner_dialog(owner) {
            return self.update(focus);
        }
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            Kind::About,
            window::Settings {
                size: Size::new(680.0, 580.0),
                min_size: Some(Size::new(360.0, 245.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        let mut updates = Updates::default();
        updates.present_background(self.background_application_update());
        self.chrome.dialog = Some(Dialog {
            id,
            owner,
            project: Project::Ready,
            updates,
        });
        opened
    }

    pub(in crate::workspace) fn present_background_application_update(
        &mut self,
        check: Option<&Check>,
    ) {
        if let Some(dialog) = &mut self.chrome.dialog {
            dialog.updates.present_background(check);
        }
    }

    pub(in crate::workspace) fn about_modal(
        &self,
        surface: window::Id,
    ) -> Option<(window::Id, WorkspaceMessage)> {
        let dialog = self
            .chrome
            .dialog
            .as_ref()
            .filter(|dialog| dialog.id != surface)?;
        Some((dialog.id, wrap(Message::About(dialog.id, Event::Focus))))
    }

    pub(in crate::workspace) fn close_about(
        &mut self,
        id: window::Id,
    ) -> Option<Task<WorkspaceMessage>> {
        let owner = self.chrome.dialog.take_if(|dialog| dialog.id == id)?.owner;
        Some(if self.windows.contains_key(&owner) && !self.exiting {
            self.close_placed_dialog_and_focus(id, owner)
        } else {
            self.close_placed_dialog(id)
        })
    }

    pub(super) fn update_about(&mut self, id: window::Id, event: Event) -> Task<WorkspaceMessage> {
        let updates_allowed =
            !self.exiting && !self.scheduler_draining && self.application_updates_allowed();
        let Some(dialog) = self.chrome.dialog.as_mut().filter(|dialog| dialog.id == id) else {
            return Task::none();
        };
        if !updates_allowed && let Some(pending) = &mut dialog.updates.pending {
            pending.cancel = None;
        }
        match event {
            Event::Focus => window::gain_focus(id),
            Event::OpenProject if dialog.project != Project::Opening => {
                dialog.project = Project::Opening;
                #[cfg(target_os = "macos")]
                {
                    let opened = kortty_windowing::open_desktop_uri(PROJECT_URL);
                    self.update_about(id, Event::ProjectOpened(opened))
                }
                #[cfg(not(target_os = "macos"))]
                Task::perform(
                    async { kortty_windowing::open_desktop_uri(PROJECT_URL) },
                    move |opened| wrap(Message::About(id, Event::ProjectOpened(opened))),
                )
            }
            Event::ProjectOpened(opened) if dialog.project == Project::Opening => {
                dialog.project = if opened {
                    Project::Ready
                } else {
                    Project::Failed
                };
                Task::none()
            }
            Event::CheckUpdates if updates_allowed && dialog.updates.pending.is_none() => {
                dialog.updates.background = false;
                dialog.updates.offer = None;
                dialog.updates.saved = None;
                match application_update::check() {
                    Ok(pending) => {
                        dialog.updates.status = "Checking the Rust release channel…".into();
                        let (done, task) = dialog.updates.begin(id, pending, Event::UpdatesChecked);
                        self.track_session_shutdown(done);
                        task
                    }
                    Err(error) => {
                        dialog.updates.status = error;
                        Task::none()
                    }
                }
            }
            Event::UpdatesChecked(ticket, result) => {
                if dialog.updates.finish(ticket, false) && updates_allowed {
                    dialog.updates.checked(result);
                }
                Task::none()
            }
            Event::UpdateFolder(folder) if updates_allowed && dialog.updates.pending.is_none() => {
                dialog.updates.folder =
                    folder[..folder.floor_char_boundary(folder.len().min(4096))].into();
                Task::none()
            }
            Event::DownloadUpdate if updates_allowed && dialog.updates.pending.is_none() => {
                let Some(offer) = dialog.updates.offer.clone() else {
                    return Task::none();
                };
                let folder = PathBuf::from(dialog.updates.folder.trim());
                match application_update::download(offer, folder) {
                    Ok(download) => {
                        dialog.updates.background = false;
                        dialog.updates.status =
                            "Downloading and verifying the update archive…".into();
                        let (done, task) =
                            dialog
                                .updates
                                .begin(id, download.pending, Event::UpdateDownloaded);
                        let ticket = dialog.updates.ticket;
                        self.track_session_shutdown(done);
                        Task::batch([
                            task,
                            Task::run(download.progress, move |progress| {
                                wrap(Message::About(id, Event::UpdateProgress(ticket, progress)))
                            }),
                        ])
                    }
                    Err(error) => {
                        dialog.updates.status = error;
                        Task::none()
                    }
                }
            }
            Event::UpdateProgress(ticket, progress)
                if progress.total > 0
                    && progress.received <= progress.total
                    && ticket == dialog.updates.ticket
                    && dialog
                        .updates
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.cancel.is_some()) =>
            {
                dialog.updates.progress = Some(progress);
                Task::none()
            }
            Event::UpdateDownloaded(ticket, result) => {
                if dialog.updates.finish(ticket, result.is_ok()) {
                    match result {
                        Ok(saved) => {
                            dialog.updates.status = if saved.sync_failed {
                                "Archive verified and saved. The filesystem could not confirm directory synchronization."
                            } else {
                                "Archive verified and saved. Install the portable package when ready."
                            }.into();
                            dialog.updates.offer = None;
                            dialog.updates.saved = Some(saved);
                        }
                        Err(error) => dialog.updates.status = error,
                    }
                }
                Task::none()
            }
            Event::CancelUpdate => {
                if let Some(pending) = &mut dialog.updates.pending {
                    pending.cancel = None;
                    dialog.updates.status = "Canceling application update…".into();
                }
                Task::none()
            }
            Event::RevealUpdate => {
                let Some(folder) = dialog
                    .updates
                    .saved
                    .as_ref()
                    .and_then(|saved| saved.path.parent())
                    .map(PathBuf::from)
                else {
                    return Task::none();
                };
                #[cfg(target_os = "macos")]
                {
                    self.update_about(
                        id,
                        Event::UpdateRevealed(kortty_windowing::open_desktop_directory(&folder)),
                    )
                }
                #[cfg(not(target_os = "macos"))]
                Task::perform(
                    async move { kortty_windowing::open_desktop_directory(&folder) },
                    move |opened| wrap(Message::About(id, Event::UpdateRevealed(opened))),
                )
            }
            Event::UpdateRevealed(false) => {
                dialog.updates.status = "Could not open the download directory.".into();
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub(in crate::workspace) fn chrome_dialog_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        let dialog = self
            .chrome
            .dialog
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        let link = button(text(PROJECT_URL).wrapping(text::Wrapping::Glyph))
            .style(button::text)
            .padding([3, 0])
            .on_press_maybe(
                (dialog.project != Project::Opening)
                    .then_some(wrap(Message::About(id, Event::OpenProject))),
            );
        let mut details = column![
            image(logo()).width(Length::Fill).height(Length::Shrink),
            rule::horizontal(1),
            text("Native SSH client with split terminals, SFTP and command snippets.").size(15),
            text("Developed by Daniel Mengel").size(15),
            column![text("Project page").size(14), link].spacing(2),
        ]
        .spacing(10);
        if dialog.project == Project::Failed {
            details =
                details.push(text("Could not open the project page in your browser.").size(14));
        }
        details = details.push(dialog.updates.view(
            id,
            self.clipboard.policy.problem.is_none() && self.clipboard.policy.updates_allowed,
            self.application_update_channel(),
        ));
        let body = container(container(details).width(Length::Fill).max_width(560))
            .center_x(Length::Fill)
            .padding([12, 18]);
        let close = WorkspaceMessage::Window(id, window::Event::CloseRequested);
        Some(
            column![
                container(text(concat!("kortty v", env!("CARGO_PKG_VERSION"))).size(27))
                    .width(Length::Fill)
                    .padding([12, 18]),
                rule::horizontal(1),
                scrollable(body).width(Length::Fill).height(Length::Fill),
                rule::horizontal(1),
                row![space::horizontal(), button("Close").on_press(close)]
                    .align_y(Alignment::Center)
                    .padding([8, 18]),
            ]
            .height(Length::Fill)
            .into(),
        )
    }
}

fn logo() -> image::Handle {
    static HANDLE: OnceLock<image::Handle> = OnceLock::new();
    HANDLE
        .get_or_init(|| image::Handle::from_bytes(LOGO))
        .clone()
}

#[cfg(test)]
mod tests {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::futures::{StreamExt, executor::block_on, future::ready};
    use iced::keyboard::{Key, Location, Modifiers};
    use iced::{Font, Renderer};
    use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

    use super::super::Action;
    use super::*;

    #[test]
    fn about_dialog_blocks_peer_actions_and_closes_before_its_owner() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (peer, _) = workspace.open_window(false);
        let _ = workspace.update(wrap(Message::Invoke(owner, Action::About)));
        let about = workspace.chrome.dialog.as_ref().unwrap().id;
        assert_eq!(workspace.placed_dialog_owner(about), Some(owner));
        assert!(workspace.owner_dialog(about).is_none());
        assert_eq!(workspace.owner_dialog(owner).unwrap().0, about);
        let (dialog, focus) = workspace.owner_dialog(peer).unwrap();
        assert_eq!(dialog, about);

        let command = if cfg!(target_os = "macos") {
            Modifiers::COMMAND
        } else {
            Modifiers::COMMAND | Modifiers::SHIFT
        };
        for message in [
            focus,
            WorkspaceMessage::Window(peer, window::Event::CloseRequested),
            WorkspaceMessage::Key(
                peer,
                Key::Character("q".into()),
                Location::Standard,
                command,
                None,
            ),
        ] {
            let task = workspace.update(message);
            assert!(matches!(
                block_on(task::into_stream(task).unwrap().next()),
                Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == about
            ));
        }
        let _ = workspace.update(wrap(Message::Invoke(peer, Action::NewWindow)));
        assert_eq!(workspace.windows.len(), 2);
        assert!(!workspace.exiting);

        let _ = workspace.finish_owned_dialog_open(about, owner, None);
        let close = workspace.update(WorkspaceMessage::Window(
            about,
            window::Event::CloseRequested,
        ));
        let mut actions = task::into_stream(close).unwrap();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Run(id, _))) if id == about
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == about
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == owner
        ));
        let _ = workspace.update(WorkspaceMessage::Window(about, window::Event::Closed));
        assert!(workspace.owner_dialog(peer).is_none());

        let _ = workspace.update(wrap(Message::Invoke(peer, Action::About)));
        let peer_about = workspace.chrome.dialog.as_ref().unwrap().id;
        assert_eq!(workspace.placed_dialog_owner(peer_about), Some(peer));
        let close = task::into_stream(workspace.close_window(peer)).unwrap();
        let closed: Vec<_> = block_on(
            close
                .filter_map(|action| {
                    ready(match action {
                        RuntimeAction::Window(WindowAction::Close(id)) => Some(id),
                        _ => None,
                    })
                })
                .collect(),
        );
        assert_eq!(closed, [peer_about, peer]);
        assert!(workspace.chrome.dialog.is_none());
        assert!(workspace.windows.contains_key(&owner));
        assert!(workspace.owner_dialog(owner).is_none());
    }

    #[test]
    fn about_dialog_keeps_close_inside_the_minimum_viewport() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_about(owner);
        let id = workspace.chrome.dialog.as_ref().unwrap().id;
        let mut view = workspace.chrome_dialog_view(id).unwrap();
        let mut tree = Tree::new(view.as_widget());
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let viewport = Size::new(360.0, 245.0);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, viewport),
        );
        let footer = Layout::new(&node).children().last().unwrap();
        let close = footer.children().last().unwrap().bounds();
        assert!(close.width > 0.0 && close.height > 0.0);
        assert!(close.x + close.width <= viewport.width);
        assert!(close.y + close.height <= viewport.height);
    }

    #[test]
    fn about_update_policy_refuses_a_manual_check_before_starting_a_worker() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_about(owner);
        let id = workspace.chrome.dialog.as_ref().unwrap().id;
        workspace.clipboard.policy.updates_allowed = false;
        let request = workspace.update_about(id, Event::CheckUpdates);
        assert!(task::into_stream(request).is_none());
        assert!(
            workspace
                .chrome
                .dialog
                .as_ref()
                .unwrap()
                .updates
                .pending
                .is_none()
        );
    }

    #[test]
    fn about_background_status_yields_to_an_explicit_update_operation() {
        let mut updates = Updates::default();
        updates.present_background(Some(&Check::Current));
        assert!(updates.background);
        assert!(updates.status.contains("current"));
        updates.present_background(None);
        assert!(updates.status.is_empty());
        updates.checked(Ok(Check::NoStableRelease));
        updates.present_background(Some(&Check::Current));
        assert!(!updates.background);
        assert!(updates.status.contains("No stable release"));
    }
}
