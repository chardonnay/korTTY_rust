//! Layout shared by the terminal and the settings preview.

use iced::widget::{column, container, opaque, row, space, stack};
use iced::{Element, Length, Size};

use crate::terminal_agent::design::{History, Status};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum Slot {
    Top,
    TopDrawer,
    Bottom,
    Right,
    OverlayTop,
    OverlayBottom,
}

pub(in crate::workspace) fn history_slot(history: History) -> Option<Slot> {
    match history {
        History::Transcript | History::StepCards | History::InlineBlocks | History::Focus => None,
        History::Timeline | History::Tmux | History::Conversation => Some(Slot::Right),
        History::PlanExecution | History::CardStack | History::LogTable => Some(Slot::Bottom),
    }
}

pub(in crate::workspace) fn status_slot(status: Status) -> Slot {
    match status {
        Status::Hidden | Status::Steps | Status::Pill => Slot::Top,
        Status::TopBar => Slot::TopDrawer,
        Status::Metro | Status::Powerline | Status::Neon => Slot::Bottom,
        Status::Minimap => Slot::Right,
        Status::Spotlight => Slot::OverlayTop,
        Status::Hud | Status::Toasts | Status::Orbit => Slot::OverlayBottom,
    }
}

pub(in crate::workspace) fn compose<'a, M: 'a>(
    terminal: Element<'a, M>,
    history: Option<Element<'a, M>>,
    status: Option<Element<'a, M>>,
    history_slot: Option<Slot>,
    status_slot: Slot,
    bounds: Size,
    side_width: f32,
) -> Element<'a, M> {
    let mut center = column![container(terminal).width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);
    let mut right = column![]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(3);
    let mut has_right = false;
    if let Some(history) = history {
        if history_slot == Some(Slot::Right) {
            right = right.push(history);
            has_right = true;
        } else {
            center = center.push(
                container(history)
                    .width(Length::Fill)
                    .height((bounds.height * 0.4).min(260.0)),
            );
        }
    }
    let mut overlay = None;
    if let Some(status) = status {
        match status_slot {
            Slot::Top => {
                center = column![status, center]
                    .width(Length::Fill)
                    .height(Length::Fill)
            }
            Slot::Bottom => center = center.push(status),
            Slot::Right => {
                right = right.push(status);
                has_right = true;
            }
            Slot::TopDrawer => {
                center = column![space::vertical().height(38), center]
                    .width(Length::Fill)
                    .height(Length::Fill);
                overlay = Some(status);
            }
            Slot::OverlayTop | Slot::OverlayBottom => overlay = Some(status),
        }
    }
    let center: Element<'a, M> = if has_right {
        row![
            center,
            container(right)
                .width(side_width.min(bounds.width * 0.45))
                .height(Length::Fill)
        ]
        .spacing(3)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    } else {
        center.into()
    };
    if let Some(overlay) = overlay {
        if status_slot == Slot::TopDrawer {
            return stack![
                center,
                container(opaque(overlay))
                    .width(Length::Fill)
                    .max_height(bounds.height * 0.7)
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }
        let overlay = container(opaque(
            container(overlay)
                .max_width(420)
                .max_height(bounds.height * 0.7),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_right(Length::Fill)
        .padding(8);
        let overlay = if status_slot == Slot::OverlayBottom {
            overlay.align_bottom(Length::Fill)
        } else {
            overlay
        };
        stack![center, overlay]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        center
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_modules_use_java_dock_slots_without_moving_legacy_designs() {
        for history in [History::Timeline, History::Tmux, History::Conversation] {
            assert_eq!(history_slot(history), Some(Slot::Right));
        }
        for history in [
            History::PlanExecution,
            History::CardStack,
            History::LogTable,
        ] {
            assert_eq!(history_slot(history), Some(Slot::Bottom));
        }
        assert_eq!(history_slot(History::Transcript), None);
        assert_eq!(history_slot(History::StepCards), None);
        assert_eq!(status_slot(Status::Steps), Slot::Top);
        assert_eq!(status_slot(Status::Metro), Slot::Bottom);
        assert_eq!(status_slot(Status::Minimap), Slot::Right);
        assert_eq!(status_slot(Status::TopBar), Slot::TopDrawer);
        assert_eq!(status_slot(Status::Spotlight), Slot::OverlayTop);
        assert_eq!(status_slot(Status::Hud), Slot::OverlayBottom);
        assert_eq!(status_slot(Status::Toasts), Slot::OverlayBottom);
        assert_eq!(status_slot(Status::Orbit), Slot::OverlayBottom);
    }
}
