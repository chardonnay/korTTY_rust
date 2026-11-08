//! Native About dialog reached from the in-window and system Help menus.

use std::sync::OnceLock;

use iced::widget::{button, column, container, image, row, rule, scrollable, space, text};
use iced::{Alignment, Element, Length, Size, Task, window};

use super::super::dialog_placement::Kind;
use super::{Message, Workspace, WorkspaceMessage, wrap};

pub(super) const TITLE: &str = "About kortty";
const PROJECT_URL: &str = "https://github.com/chardonnay/kortty";
// Original artwork by Daniel Mengel, from the MIT-licensed Java reference at 1ebbe724ee84.
const LOGO: &[u8] = include_bytes!("about/kortty_logo.png");

#[derive(Clone, Copy, Debug)]
pub enum Event {
    OpenProject,
    ProjectOpened(bool),
}

pub(super) struct Dialog {
    pub(super) id: window::Id,
    pub(super) owner: window::Id,
    project: Project,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Project {
    Ready,
    Opening,
    Failed,
}

impl Workspace {
    pub(super) fn show_about(&mut self, owner: window::Id) -> Task<WorkspaceMessage> {
        if let Some(dialog) = &self.chrome.dialog {
            return window::gain_focus(dialog.id);
        }
        let (id, opened) = self.open_placed_dialog(
            Kind::About,
            window::Settings {
                size: Size::new(680.0, 580.0),
                min_size: Some(Size::new(360.0, 245.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.chrome.dialog = Some(Dialog {
            id,
            owner,
            project: Project::Ready,
        });
        opened.discard()
    }

    pub(super) fn update_about(&mut self, id: window::Id, event: Event) -> Task<WorkspaceMessage> {
        let Some(dialog) = self.chrome.dialog.as_mut().filter(|dialog| dialog.id == id) else {
            return Task::none();
        };
        match event {
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
    use iced::{Font, Renderer};

    use super::*;

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
}
