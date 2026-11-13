use std::sync::Arc;

use iced::widget::{button, column, container, row, space, text};
use iced::{Alignment, Element, Length, Size, Task, window};

use crate::workspace::dialog_placement::Kind as DialogKind;
use crate::{session::SessionId, terminal_links::Hit};

use super::{Message as Desktop, Workspace};

const TITLE: &str = "Open Terminal Link?";

#[derive(Clone, Debug)]
pub enum Message {
    Open(SessionId, Arc<Hit>),
    Focus,
    Decide(bool),
    Approved(SessionId, Arc<Hit>),
    Opened(bool),
}

pub(super) struct Confirmation {
    owner: window::Id,
    session: SessionId,
    link: Arc<Hit>,
}

impl Workspace {
    pub(super) fn link_confirmation_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.link_confirmations
            .iter()
            .find_map(|(id, dialog)| (dialog.owner == owner).then_some(*id))
    }

    pub(super) fn link_confirmation_title(&self, id: window::Id) -> Option<&'static str> {
        self.link_confirmations.contains_key(&id).then_some(TITLE)
    }

    pub(super) fn remove_link_confirmation_for_owner(&mut self, owner: window::Id) {
        if let Some(id) = self.link_confirmation_for_owner(owner) {
            self.link_confirmations.remove(&id);
        }
    }

    pub(super) fn update_terminal_link(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Open(session, link) => self.open_terminal_link(id, session, link),
            Message::Focus if self.link_confirmations.contains_key(&id) => window::gain_focus(id),
            Message::Decide(approved) => {
                let Some(dialog) = self.link_confirmations.remove(&id) else {
                    return Task::none();
                };
                let close = self.close_placed_dialog_and_focus(id, dialog.owner);
                if approved {
                    close.chain(Task::done(Desktop::TerminalLink(
                        dialog.owner,
                        Message::Approved(dialog.session, dialog.link),
                    )))
                } else {
                    close
                }
            }
            Message::Approved(session, link)
                if self.palette_can_open(id) && self.window_contains_visible_pane(id, session) =>
            {
                self.dispatch_terminal_uri(id, &link)
            }
            Message::Opened(false) => {
                self.notice(id, "The desktop could not open the link.".into());
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn open_terminal_link(
        &mut self,
        owner: window::Id,
        session: SessionId,
        link: Arc<Hit>,
    ) -> Task<Desktop> {
        if !self.palette_can_open(owner)
            || self.palette.is_open(owner)
            || !self.window_contains_visible_pane(owner, session)
        {
            return Task::none();
        }
        if let Some(pane) = self.visible_pane(owner, session)
            && let Some(tab) = self.active_tab_mut(owner)
        {
            tab.focused = pane;
        }
        self.dismiss_terminal_context(owner);
        if link.mismatch.is_none() {
            return self.dispatch_terminal_uri(owner, &link);
        }

        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            DialogKind::TerminalLink,
            window::Settings {
                size: Size::new(490.0, 250.0),
                min_size: Some(Size::new(385.0, 225.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        self.link_confirmations.insert(
            id,
            Confirmation {
                owner,
                session,
                link,
            },
        );
        opened
    }

    fn dispatch_terminal_uri(&mut self, owner: window::Id, link: &Hit) -> Task<Desktop> {
        let uri = link.uri();
        #[cfg(target_os = "macos")]
        {
            let opened = kortty_windowing::open_desktop_uri(&uri);
            self.update_terminal_link(owner, Message::Opened(opened))
        }
        #[cfg(not(target_os = "macos"))]
        Task::perform(
            async move { kortty_windowing::open_desktop_uri(&uri) },
            move |opened| Desktop::TerminalLink(owner, Message::Opened(opened)),
        )
    }

    pub(super) fn link_confirmation_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self.link_confirmations.get(&id)?;
        let shown = dialog.link.mismatch.as_ref()?;
        let send = move |approved| Desktop::TerminalLink(id, Message::Decide(approved));
        Some(
            container(
                column![
                    text("The link opens a different address").size(21),
                    text(format!("The terminal text shows {shown}.")),
                    text(dialog.link.display_target())
                        .font(iced::Font::MONOSPACE)
                        .wrapping(text::Wrapping::Glyph),
                    space::vertical(),
                    row![
                        space::horizontal(),
                        button("Cancel").on_press(send(false)),
                        button("Open Link").on_press(send(true)),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                ]
                .spacing(12),
            )
            .padding(18)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{GridSize, Session};
    use kortty_terminal::{Terminal, TerminalProfile};

    #[test]
    fn mismatch_confirmation_only_blocks_its_owner_and_cancel_releases_it() {
        use iced::futures::{StreamExt, executor::block_on, future::ready};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let grid = GridSize::default();
        let session = Session::ssh(None, grid, TerminalProfile::Xterm, true, None).unwrap();
        let session = workspace.store_session(session, grid, 14);
        workspace.add_tab(owner, session);

        let mut terminal = Terminal::new(80, 24);
        terminal
            .advance(b"\x1b]8;;https://target.example/path\x07https://shown.example\x1b]8;;\x07");
        let link = Arc::new(crate::terminal_links::at(&terminal.snapshot(), 0, 0, false).unwrap());
        let _ = workspace.update_terminal_link(other, Message::Open(session, link.clone()));
        assert!(workspace.link_confirmations.is_empty());
        let _ = workspace.update_terminal_link(owner, Message::Open(session, link.clone()));
        let dialog = workspace.link_confirmation_for_owner(owner).unwrap();

        assert_eq!(workspace.owner_dialog(owner).unwrap().0, dialog);
        assert!(workspace.owner_dialog(other).is_none());
        let _ = workspace.update_terminal_link(dialog, Message::Decide(false));
        assert!(workspace.owner_dialog(owner).is_none());
        assert!(workspace.link_confirmation_title(dialog).is_none());

        let _ = workspace.update_terminal_link(owner, Message::Open(session, link));
        let dialog = workspace.link_confirmation_for_owner(owner).unwrap();
        let shutdown = workspace.close_window(owner);
        let closed = task::into_stream(shutdown).unwrap().filter_map(|action| {
            ready(match action {
                RuntimeAction::Window(WindowAction::Close(id)) => Some(id),
                _ => None,
            })
        });
        assert_eq!(block_on(closed.collect::<Vec<_>>()), [dialog, owner]);
        assert!(workspace.link_confirmation_title(dialog).is_none());
        assert!(workspace.windows.contains_key(&other));
    }
}
