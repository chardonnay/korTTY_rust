use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length, window};

use super::super::journal_access::link_text::journal_keyed_action;
use super::{Desktop, Message, ProbeChoice};

pub(super) struct JournalProbeView<'a> {
    pub(super) ticket: u64,
    pub(super) automatic: bool,
    pub(super) can_cancel: bool,
    pub(super) status: Status<'a>,
}

pub(super) enum Status<'a> {
    Checking { profile: &'a str },
    Failed { profile: &'a str, reason: &'a str },
}

impl<'a> JournalProbeView<'a> {
    pub(super) fn view(self, owner: window::Id, tab: u64) -> Element<'a, Desktop> {
        let choice = |action| {
            Desktop::JournalCapture(Message::AiProbeChoice(owner, tab, self.ticket, action))
        };
        let checking = matches!(self.status, Status::Checking { .. });
        let content = match self.status {
            Status::Checking { profile } => {
                let status = text(format!("Testing the journal AI connection… · {profile}"))
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph);
                let mut content = row![status].spacing(6).align_y(Alignment::Center);
                if !self.automatic {
                    content = content.push(
                        button("Cancel")
                            .style(button::secondary)
                            .on_press(choice(ProbeChoice::Cancel)),
                    );
                }
                content.wrap().into()
            }
            Status::Failed { profile, reason } => {
                let stop = if self.automatic {
                    "End journal"
                } else {
                    "Cancel"
                };
                let style = if self.automatic {
                    container::danger
                } else {
                    container::warning
                };
                let heading = if self.automatic {
                    "Journal AI is unreachable. Output is being recorded."
                } else {
                    "Journal AI is unreachable"
                };
                let actions = row![
                    button("Test again").on_press(choice(ProbeChoice::Retry)),
                    button("Record without AI")
                        .style(button::secondary)
                        .on_press(choice(ProbeChoice::WithoutAi)),
                    self.can_cancel.then(|| button(stop)
                        .style(button::secondary)
                        .on_press(choice(ProbeChoice::Cancel))),
                ]
                .spacing(6)
                .wrap();
                container(
                    column![
                        text(heading).size(15).wrapping(text::Wrapping::WordOrGlyph),
                        text(format!("{profile} · {reason}"))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                        actions,
                    ]
                    .spacing(5),
                )
                .width(Length::Fill)
                .padding([5, 8])
                .style(style)
                .into()
            }
        };
        journal_keyed_action((owner, tab, self.ticket, checking), content)
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
    use iced::{Event, Font, Renderer, Size, mouse};

    use super::*;

    #[test]
    fn journal_preflight_actions_wrap_and_keep_their_capture_ticket() {
        let owner = window::Id::unique();
        for automatic in [false, true] {
            let mut view = JournalProbeView {
                ticket: 14,
                automatic,
                can_cancel: true,
                status: Status::Failed {
                    profile: "Example model",
                    reason: "The model could not answer the test request.",
                },
            }
            .view(owner, 23);
            let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(200.0, 500.0)),
            );
            let layout = Layout::new(&node);
            let content = layout.children().next().unwrap();
            let actions: Vec<_> = content.children().nth(2).unwrap().children().collect();
            assert_eq!(actions.len(), 3);
            assert!(actions[0].bounds().y < actions[2].bounds().y);

            let mut messages = Vec::new();
            for action in actions {
                for event in [
                    mouse::Event::ButtonPressed(mouse::Button::Left),
                    mouse::Event::ButtonReleased(mouse::Button::Left),
                ] {
                    view.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(event),
                        layout,
                        mouse::Cursor::Available(action.bounds().center()),
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &layout.bounds(),
                    );
                }
            }
            assert!(matches!(
                messages.as_slice(),
                [
                    Desktop::JournalCapture(Message::AiProbeChoice(retry, 23, 14, ProbeChoice::Retry)),
                    Desktop::JournalCapture(Message::AiProbeChoice(raw, 23, 14, ProbeChoice::WithoutAi)),
                    Desktop::JournalCapture(Message::AiProbeChoice(cancel, 23, 14, ProbeChoice::Cancel)),
                ] if [*retry, *raw, *cancel] == [owner; 3]
            ));
        }
    }
}
